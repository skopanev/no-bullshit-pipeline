//! Quick Dictate: hotkey-driven ephemeral mic capture → transcribe → optional
//! LLM pipeline → paste. Bypasses the main recording pipeline (no storage, no
//! system audio, no EBU). Supports N independent shortcuts — each with its own
//! hotkey, input source, and optional LLM-only pipeline — all transcribed with
//! the single global ASR engine (there is no per-shortcut engine; see
//! `transcribe`).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Split};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::{DictationShortcut, StepType, TranscriptionProvider, load_settings};
use crate::pipelines::load_pipelines;

const TARGET_RATE: u32 = 16_000;

/// Hard ceiling on the in-memory mic capture buffer. A Quick Dictate
/// session is meant to last seconds. If a stop path ever fails to fire
/// (or the user walks away with a session live), the cpal input callback
/// would otherwise `extend_from_slice` into `samples` forever — a single
/// orphaned session was caught by malloc_history holding 5.8 GB. Past this
/// many f32 samples we simply stop appending: the transcript gets
/// truncated instead of the process eating gigabytes. 48 kHz × 2ch × 300s
/// ≈ 115 MB absolute worst case — far beyond any real dictation.
const MAX_CAPTURE_SAMPLES: usize = 48_000 * 2 * 300;

/// Append `data` into `buf` but never past `MAX_CAPTURE_SAMPLES`. Structural
/// safety net: bounds memory regardless of which stop path may have leaked.
fn append_capped(buf: &mut Vec<f32>, data: &[f32]) {
    let room = MAX_CAPTURE_SAMPLES.saturating_sub(buf.len());
    if room == 0 {
        return;
    }
    let take = data.len().min(room);
    buf.extend_from_slice(&data[..take]);
}

pub struct DictationState {
    pub is_active: Arc<AtomicBool>,
    inner: Mutex<Option<Session>>,
    pub last_registration: Mutex<Vec<crate::ShortcutRegistration>>,
    /// Handle to the in-flight post-capture pipeline (transcribe → LLM →
    /// paste). The pipeline runs detached from `stop_inner` so the user can
    /// abort it with Esc at any point: aborting drops the task's future, which
    /// drops the sidecar guard (killing the transcription process) AND
    /// guarantees no stale, out-of-context text gets pasted afterwards.
    /// `None` whenever no pipeline is running.
    pipeline: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Monotonic delivery counter — the authoritative cancellation primitive.
    ///
    /// A plain "cancelled" bool can't serve two overlapping deliveries: if Esc
    /// sets it for delivery N and a new delivery N+1 starts before N observes
    /// it, N+1 must either reset the flag (and N then pastes stale text — the
    /// race the panel found) or leave it set (and N+1 false-cancels itself).
    ///
    /// Instead each delivery claims a unique generation at start
    /// (`claim_generation`) and carries it through the pipeline. Cancel
    /// (`supersede`) and every new delivery bump the counter, so any older
    /// generation is now stale. The pipeline checks `is_current(my_gen)` at
    /// every boundary (entry, before transcription, before the paste keystroke)
    /// and bails the instant its generation is no longer the live one. Because
    /// the counter only ever increments, there is no reset race.
    generation: Arc<AtomicU64>,
}

struct Session {
    shortcut_id: String,
    /// The delivery generation this session claimed at start. Carried into the
    /// detached pipeline so it can detect supersession (Esc, or a newer
    /// delivery) and bail before pasting.
    generation: u64,
    /// In-memory capture accumulator. Filled by the drain thread (NOT the cpal
    /// callback) — the audio thread only pushes into a lock-free ring buffer, so
    /// nothing locks or allocates on it. Read once at stop.
    samples: Arc<Mutex<Vec<f32>>>,
    /// Signals the drain thread to flush the ring's tail and exit. Set at stop.
    drain_stop: Arc<AtomicBool>,
    /// Handle to the drain thread; joined at stop so `samples` is complete and
    /// the ring consumer is released before we read.
    drain_thread: Option<std::thread::JoinHandle<()>>,
    sample_rate: u32,
    channels: u16,
    stream: Option<cpal::Stream>,
    /// Pre-duck system output volume snapshot. `Some` only when we manually
    /// ducked — i.e., when the chosen input device differs from the system
    /// output device (so macOS doesn't auto-route and we can't hear ourselves
    /// over the music otherwise).
    pre_duck_volume: Option<u32>,
    /// Live streaming bridge. `Some` only when the shortcut's engine supports
    /// streaming (currently FluidAudio). When set, samples are forwarded to
    /// the sidecar in the audio callback and the final transcript comes from
    /// the sidecar's last NDJSON line on stop. When `None` we fall back to
    /// the legacy batch path: accumulate into `samples`, write a tmp WAV at
    /// stop, transcribe that.
    streaming: Option<crate::dictation_streaming::StreamingSession>,
    /// Concurrent system-audio (loopback) capture, set up off the hot path
    /// because `start_system_capture` (AggregateDevice + ProcessTap creation)
    /// is synchronous and on a cold cache can take 2-3 seconds — which would
    /// add that latency to every hotkey press. Spawned via `spawn_blocking`;
    /// the inner Option is the produced recorder (or None if Core Audio
    /// errored). On stop we await the handle with a short timeout, on cancel
    /// we just abort. Set only when `capture_system_audio = true`.
    system_setup:
        Option<tauri::async_runtime::JoinHandle<Option<crate::system_audio::SystemAudioRecorder>>>,
}

/// Coefficient applied to the current system output volume during a session.
/// 0.4 = duck to 40% of current.
const VOLUME_DUCK_RATIO: f32 = 0.4;

/// Adaptive level-meter state. Stores the running peak of (rms+peak) as
/// f32 bits so push_level can normalize current frames against it without
/// any device-specific gain constant. Reset at session start.
static PEAK_TRACKER: AtomicU32 = AtomicU32::new(0);

// SAFETY: cpal::Stream is !Send on macOS due to PhantomData<*mut ()>. The stream
// is created, kept alive in the Mutex, and dropped on stop — never moved across
// threads. All access serialised by the outer Mutex.
unsafe impl Send for Session {}

impl DictationState {
    pub fn new() -> Self {
        Self {
            is_active: Arc::new(AtomicBool::new(false)),
            inner: Mutex::new(None),
            last_registration: Mutex::new(Vec::new()),
            pipeline: Mutex::new(None),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Claim a fresh delivery generation. Bumping the counter also supersedes any
/// older in-flight delivery (its `is_current` checks will now fail), so this
/// doubles as "start mine, cancel anyone older". Returns the claimed value.
fn claim_generation(app: &AppHandle) -> u64 {
    app.state::<DictationState>()
        .generation
        .fetch_add(1, Ordering::AcqRel)
        + 1
}

/// Supersede the current delivery without starting a new one — i.e. cancel.
/// Bumps the counter so the live pipeline's generation is no longer current.
fn supersede(app: &AppHandle) {
    app.state::<DictationState>()
        .generation
        .fetch_add(1, Ordering::AcqRel);
}

/// True while `gen` is still the live delivery generation. Goes false the
/// instant a cancel (`supersede`) or a newer delivery (`claim_generation`)
/// bumps the counter — checked at every pipeline boundary so a stale delivery
/// stops before it pastes, regardless of where Esc landed.
fn is_current(app: &AppHandle, generation: u64) -> bool {
    app.state::<DictationState>()
        .generation
        .load(Ordering::Acquire)
        == generation
}

/// Abort the in-flight stop pipeline, if any. Aborting drops the task's future
/// at its current `.await` point, which drops the sidecar guard (killing the
/// transcription process) and stops the flow before it can paste. Returns true
/// if a handle was present. Safe to call when none is running (no-op).
fn abort_pipeline(app: &AppHandle) -> bool {
    let handle = app
        .state::<DictationState>()
        .pipeline
        .lock()
        .ok()
        .and_then(|mut g| g.take());
    if let Some(h) = handle {
        h.abort();
        true
    } else {
        false
    }
}

/// Kills the wrapped transcription sidecar on drop. The pipeline task may be
/// aborted mid-transcription (user pressed Esc); without this the orphaned
/// sidecar keeps the model resident and burns CPU until it finishes on its own.
struct SidecarKiller(Option<tauri_plugin_shell::process::CommandChild>);

impl Drop for SidecarKiller {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            let _ = child.kill();
        }
    }
}

/// Prewarm on-device transcription models at app startup.
///
/// The sidecars are one-shot processes: each dictation spawns one, which
/// loads Parakeet / SpeechAnalyzer into RAM from scratch. The big one-time
/// cost is the CoreML `.mlpackage → .mlmodelc` compile (seconds); the
/// recurring cost is reading the weights off disk into the OS page cache.
/// Running one throwaway pass over a sliver of silence at startup pays both
/// up front, so the user's first real dictation doesn't eat the cold-start
/// penalty.
///
/// Dictation always transcribes with the single GLOBAL ASR engine (the ASR
/// settings tab), the same one recordings use — see `transcribe`. So there is
/// exactly one model to warm; per-shortcut engine selection does not exist.
/// Cloud / unsupported engines have no local model to compile — skipped.
pub fn prewarm_models(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let settings = load_settings();
        if !settings.dictation.enabled {
            return;
        }

        // Dictation uses the GLOBAL ASR engine (same as recordings) — warm that
        // ONE model. Cloud / unsupported engines have no local model to compile.
        let provider = settings.transcription.provider.clone();
        if !matches!(
            provider,
            TranscriptionProvider::FluidAudio
                | TranscriptionProvider::Qwen3
                | TranscriptionProvider::AppleSpeech
        ) {
            return;
        }

        // ~0.3s of silence at 16 kHz. Enough for the sidecar to load + run the
        // model (FluidAudio reports "No speech detected" → Ok(""), SpeechAnalyzer
        // yields empty) — we only want the cold-start compile paid up front, once.
        let silence = vec![0.0f32; (TARGET_RATE as usize) * 3 / 10];
        let t0 = std::time::Instant::now();
        match transcribe(&app, &silence).await {
            Ok(_) => log::info!("dictation: prewarmed {:?} in {:?}", provider, t0.elapsed()),
            Err(e) => log::warn!("dictation: prewarm failed (non-fatal): {}", e),
        }
    });
}

/// Read-only snapshot of the most recent shortcut-registration result, used
/// by the frontend to render status badges without triggering a re-register.
#[tauri::command]
pub fn dictation_get_registration_status(
    state: tauri::State<'_, DictationState>,
) -> Vec<crate::ShortcutRegistration> {
    state
        .last_registration
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

#[derive(Clone, Serialize)]
pub struct DictationStatus {
    pub state: String,
    pub shortcut_id: Option<String>,
    pub message: Option<String>,
}

fn emit_status(app: &AppHandle, state: &str, shortcut_id: Option<&str>, message: Option<String>) {
    let _ = app.emit(
        "dictation_status",
        DictationStatus {
            state: state.to_string(),
            shortcut_id: shortcut_id.map(|s| s.to_string()),
            message,
        },
    );
}

fn find_shortcut(shortcut_id: &str) -> Option<DictationShortcut> {
    let settings = load_settings();
    settings
        .dictation
        .shortcuts
        .into_iter()
        .find(|s| s.id == shortcut_id)
}

/// Update the live mic level meter from a callback buffer. AGC + dB mapping so
/// every syllable moves the bar regardless of mic sensitivity. Module-level so
/// the cpal callbacks built in `open_mic_stream` (off the async worker) can call
/// it. Reads/writes the `PEAK_TRACKER` static; reset per session at start.
fn push_level(samples: &[f32]) {
    if samples.is_empty() {
        return;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    let peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
    let signal = rms * 0.6 + peak * 0.4;

    // AGC: slowly-decaying running peak compensates for mic sensitivity.
    // Decay is intentionally slow (~10s window) so it learns the user's loud
    // syllables and treats them as 0 dB — quieter parts of speech then show as
    // -10..-30 dB below, giving natural dynamics instead of pegging at 100%.
    const FLOOR: f32 = 0.005;
    const DECAY: f32 = 0.9995;
    let prev = f32::from_bits(PEAK_TRACKER.load(Ordering::Relaxed));
    let agc_max = (prev * DECAY).max(signal).max(FLOOR);
    PEAK_TRACKER.store(agc_max.to_bits(), Ordering::Relaxed);

    let gained = (signal / agc_max).max(1e-5);
    let db = 20.0 * gained.log10();
    let level = ((db + 40.0) / 40.0).clamp(0.0, 1.0);
    crate::mic_audio::set_audio_level(level);
}

/// Everything `start_inner` needs back from opening the mic: the live stream,
/// the drain thread that empties the capture ring into `samples`, plus metadata
/// (rate/channels for the Session, device names for the duck decision) —
/// gathered on the blocking thread so the async side never touches Core Audio.
struct MicSetup {
    stream: crate::audio_capture::SendStream,
    drain_thread: std::thread::JoinHandle<()>,
    sample_rate: u32,
    channels: u16,
    input_name: Option<String>,
    output_name: Option<String>,
}

/// Open + start the mic input stream and start metering.
///
/// BLOCKING: `build_input_stream` and `play()` are synchronous Core Audio
/// calls. On a Bluetooth input they trigger the ~1-2s A2DP→HFP profile switch.
/// ALWAYS call this from `spawn_blocking`, never the async runtime, or the
/// hotkey press freezes the whole UI for that switch.
fn open_mic_stream(
    device_name: Option<String>,
    samples: Arc<Mutex<Vec<f32>>>,
    drain_stop: Arc<AtomicBool>,
) -> Result<MicSetup, String> {
    let host = cpal::default_host();
    let device = if let Some(name) = device_name.as_deref() {
        crate::devices::get_device_by_name(name).or_else(|| host.default_input_device())
    } else {
        host.default_input_device()
    }
    .ok_or("No input device available")?;

    let config = pick_input_config(&device)?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    log::info!(
        "dictation: chose input config — rate={}, channels={}, format={:?}",
        sample_rate,
        channels,
        config.sample_format()
    );

    // Capture is lock-free: the cpal callback (shared `audio_capture` primitive)
    // only pushes into this ring buffer. The drain thread below pops it into
    // `samples`, so the real-time audio thread never locks, allocates, or
    // reallocates — that work all happens off it. The drain is a plain memcpy
    // with no I/O, so it can't fall behind on a slow disk the way the on-disk
    // recorder's encoder could; ~2 s of headroom makes a full ring effectively
    // impossible here. (If one ever did fill, `push_slice` drops the *newest*
    // overflowing samples — never the audio thread.)
    let ring_size = (sample_rate as usize) * (channels as usize) * 2;
    let rb = HeapRb::<f32>::new(ring_size.max(8192));
    let (producer, mut consumer) = rb.split();

    let stream = crate::audio_capture::build_input_stream(&device, config, producer)?;

    // Drain thread: empty the ring into the in-memory accumulator and update the
    // level meter — both off the audio thread. Spins while data flows, sleeps
    // briefly when the ring is empty, and exits once `drain_stop` is set AND the
    // ring is drained (so the tail captured right after stop isn't lost).
    let drain_thread = {
        let samples = samples.clone();
        let drain_stop = drain_stop.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0.0f32; 4096];
            loop {
                let n = consumer.pop_slice(&mut buf);
                if n > 0 {
                    push_level(&buf[..n]);
                    if let Ok(mut acc) = samples.lock() {
                        append_capped(&mut acc, &buf[..n]);
                    }
                    continue;
                }
                if drain_stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        })
    };

    // cpal `DeviceTrait::name` is deprecated in 0.17 but kept here: we only
    // compare input vs output device labels for the duck decision, and name()
    // is the stable shape. Grabbed here so the async side never touches cpal.
    #[allow(deprecated)]
    let input_name = device.name().ok();
    #[allow(deprecated)]
    let output_name = host.default_output_device().and_then(|d| d.name().ok());

    Ok(MicSetup {
        stream,
        drain_thread,
        sample_rate,
        channels,
        input_name,
        output_name,
    })
}

/// Tear down the mic capture cleanly: pause + drop the cpal stream so its
/// callback stops feeding the ring, then signal the drain thread and join it so
/// `samples` is final and no thread is left spinning. Idempotent (both the
/// stream and the handle are `take`n). Shared by the stop, cancel, and
/// start-abort paths so the teardown order lives in exactly one place.
fn teardown_capture(session: &mut Session) {
    // Pause BEFORE drop: on macOS `cpal::Stream::Drop` doesn't block on the HAL
    // thread, so the callback can keep pushing for an unbounded time after drop;
    // pause() stops it synchronously so the drain thread can actually reach an
    // empty ring and exit.
    if let Some(ref s) = session.stream {
        let _ = s.pause();
    }
    drop(session.stream.take());
    session.drain_stop.store(true, Ordering::Relaxed);
    if let Some(handle) = session.drain_thread.take() {
        let _ = handle.join();
    }
}

/// Decide whether to duck system output for this session, snapshot the pre-duck
/// volume, and kick off the fade. Returns the volume to restore on stop (`None`
/// when we didn't duck — e.g. input == output device).
///
/// BLOCKING: `get_system_output_volume` shells out to `osascript` (~80-150ms).
/// Call from `spawn_blocking`, never the async runtime. The mic is already live
/// by the time this runs, so the cost never delays capture.
fn duck_system_output(input_name: Option<String>, output_name: Option<String>) -> Option<u32> {
    // Duck ONLY when the input device differs from the output device. When
    // they're the same physical thing (typical BT case, AirPods → AirPods),
    // macOS forces an HFP profile switch and the music already drops on its
    // own — touching the slider then is unreliable (HFP decouples the slider
    // from real playback level, ends up at zero).
    match (&input_name, &output_name) {
        (Some(i), Some(o)) if i == o => {
            log::info!(
                "dictation: input=output ({}) — letting macOS handle audio routing, skipping fade",
                i
            );
            None
        }
        _ => {
            let snap = get_system_output_volume();
            if let Some(now) = snap {
                let target = ((now as f32) * VOLUME_DUCK_RATIO).round().max(1.0) as u32;
                log::info!(
                    "dictation: input={:?}, output={:?} — ducking {} → {}",
                    input_name,
                    output_name,
                    now,
                    target
                );
                fade_system_volume(now, target, 750);
            }
            snap
        }
    }
}

pub async fn start_inner(app: &AppHandle, shortcut_id: &str) -> Result<(), String> {
    let state = app.state::<DictationState>();

    // Atomically claim the active slot. The old load-then-late-store pattern
    // had a wide race window (~1-3s while the streaming sidecar loaded its
    // model) during which a second hotkey press would also pass the check
    // and start a second concurrent setup — spawning duplicate sidecars and
    // overwriting the in-flight Session.
    let active_flag = state.is_active.clone();
    if active_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        log::warn!("dictation: start_inner rejected — already active");
        return Err("Dictation already active".into());
    }
    // Reset the flag if we bail out before successfully installing the
    // Session in state. RAII so we don't have to remember on every `?`.
    struct ActiveGuard {
        flag: Arc<AtomicBool>,
        commit: bool,
    }
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            if !self.commit {
                self.flag.store(false, Ordering::Release);
            }
        }
    }
    let mut active_guard = ActiveGuard {
        flag: active_flag,
        commit: false,
    };

    // Claim a fresh delivery generation. This atomically supersedes any
    // still-running pipeline from the previous dictation (its `is_active` was
    // already cleared at stop, so it could otherwise paste stale text into this
    // new context): the bump makes the old generation non-current, so the old
    // pipeline bails at its next boundary check. We also abort it to kill its
    // sidecar promptly — but correctness rests on the generation, not the
    // (cooperative, await-only) abort. No reset race: the counter only ever
    // increments, so the old delivery can never observe itself as "current".
    let generation = claim_generation(app);
    abort_pipeline(app);

    let shortcut = find_shortcut(shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' not found", shortcut_id))?;

    // #2 — Instant start. Opening the mic below can block ~1-2s on Bluetooth
    // (the A2DP→HFP profile switch), so fire the live "recording" HUD NOW,
    // before the stream is even open. No intermediate "starting" screen — the
    // user sees the listening UI the instant they press the key. Land it on the
    // active monitor first.
    crate::hud::reposition(app);
    emit_status(app, "recording", Some(&shortcut.id), None);

    // Fresh meter state per session — last shortcut's mic/level shouldn't bias
    // the adaptive gain of the next one.
    PEAK_TRACKER.store(0, Ordering::Relaxed);

    // The drain thread appends into this off the audio thread; pre-allocating
    // the full cap up front keeps even that growth realloc-free. It's one large
    // mmap, lazily backed by physical pages only as samples actually arrive —
    // cheap to reserve, no eager 100+ MB commit.
    let samples: Arc<Mutex<Vec<f32>>> =
        Arc::new(Mutex::new(Vec::with_capacity(MAX_CAPTURE_SAMPLES)));
    let drain_stop = Arc::new(AtomicBool::new(false));

    // Bring up the system-audio tap in the background. Creating the
    // AggregateDevice + ProcessTap pokes Core Audio's engine and on a cold
    // cache can block 2-3 seconds — running it inline made every hotkey
    // press feel laggy. cpal mic stream starts in parallel below; the tap
    // produces samples whenever it becomes ready and they land in
    // SYSTEM_BUFFER for the stop-time mix. Quick-stop sessions may miss
    // the first ~second of loopback, but that's the trade-off for an
    // instant-feeling HUD.
    let system_setup = if shortcut.capture_system_audio {
        crate::audio_processing::SYSTEM_BUFFER.clear();
        let tmp_path = std::env::temp_dir().join(format!("nbp-dictation-sys-{}.ogg", shortcut.id));
        let name = shortcut.name.clone();
        Some(tauri::async_runtime::spawn_blocking(move || {
            match crate::system_audio::start_system_capture(tmp_path, true) {
                Ok(rec) => {
                    log::info!("dictation: system audio capture started for '{}'", name);
                    Some(rec)
                }
                Err(e) => {
                    log::warn!(
                        "dictation: system audio capture failed for '{}': {} — mic-only",
                        name,
                        e
                    );
                    None
                }
            }
        }))
    } else {
        None
    };

    // #1/#3 — Open the mic OFF the async worker. build_input_stream + play()
    // are synchronous Core Audio calls; on Bluetooth they trigger the ~1-2s
    // A2DP→HFP switch. Running them on a blocking thread keeps the async
    // runtime (and the HUD shown above) responsive throughout. On failure we
    // surface an error to the HUD so it doesn't sit on "starting" forever.
    let device_name = shortcut.device_name.clone();
    let samples_for_cb = samples.clone();
    let drain_stop_for_cb = drain_stop.clone();
    let MicSetup {
        stream,
        drain_thread,
        sample_rate,
        channels,
        input_name,
        output_name,
    } = match tauri::async_runtime::spawn_blocking(move || {
        open_mic_stream(device_name, samples_for_cb, drain_stop_for_cb)
    })
    .await
    {
        Ok(Ok(setup)) => setup,
        Ok(Err(e)) => {
            emit_status(app, "error", Some(&shortcut.id), Some(e.clone()));
            return Err(e);
        }
        Err(e) => {
            let msg = format!("mic setup task: {}", e);
            emit_status(app, "error", Some(&shortcut.id), Some(msg.clone()));
            return Err(msg);
        }
    };
    let stream = stream.0;

    // Streaming is DISABLED — Parakeet EOU 120M (the streaming model) is
    // English-biased and butchers non-English speech. Always the batch path
    // (Parakeet TDT v3, multilingual) until we get an equally good streaming
    // model.
    let streaming: Option<crate::dictation_streaming::StreamingSession> = None;

    // #4 — Duck system output OFF the async worker too. get_system_output_volume
    // shells out to osascript (~80-150ms); inline it blocked the runtime. The
    // mic is already live here, so this never delays capture — only the cosmetic
    // volume dip. Device names came back from open_mic_stream, so this thread
    // never re-touches Core Audio. We still fold back the pre-duck snapshot so
    // stop_inner can restore the exact original level.
    let pre_duck_volume = if shortcut.capture_system_audio {
        // Don't duck — we're recording whatever is playing, so killing the
        // output volume would also wipe the very signal the user wants
        // captured (call, video, music).
        log::info!("dictation: capture_system_audio=true — skipping output duck");
        None
    } else {
        tauri::async_runtime::spawn_blocking(move || duck_system_output(input_name, output_name))
            .await
            .unwrap_or(None)
    };

    // While dictation is live, hijack Escape to cancel the session — the user
    // can wave it off without their text leaking out as a paste. The shortcut
    // is unregistered as soon as the session ends (stop or cancel).
    register_escape_cancel(app);

    let mut session = Session {
        shortcut_id: shortcut.id.clone(),
        generation,
        samples,
        drain_stop,
        drain_thread: Some(drain_thread),
        sample_rate,
        channels,
        stream: Some(stream),
        pre_duck_volume,
        streaming,
        system_setup,
    };

    {
        let mut guard = state
            .inner
            .lock()
            .map_err(|e| format!("state lock: {}", e))?;
        // A sleep/wake reconcile or a cancel can fire during the mic-open await
        // above (up to ~1-2s on Bluetooth), flipping is_active back to false and
        // taking the still-empty session slot. If that happened, DON'T install an
        // orphaned session and start a tray pulse that no stop path will ever
        // clear — that's the "blinking red after wake" bug. Tear down instead.
        if !state.is_active.load(Ordering::Relaxed) {
            drop(guard);
            log::warn!(
                "dictation: '{}' startup aborted — cancelled while opening mic",
                shortcut.name
            );
            if let Some(v) = pre_duck_volume {
                restore_system_volume_to(v);
            }
            unregister_escape_cancel(app);
            // Stop the stream AND join the drain thread before the session drops
            // — otherwise the drain thread spins forever (it only exits once
            // drain_stop is set and the ring is empty).
            teardown_capture(&mut session);
            // Leave is_active as-is (cancel owns it now). Commit so the guard's
            // Drop doesn't touch the flag.
            active_guard.commit = true;
            return Ok(());
        }
        *guard = Some(session);
        // Start the pulse INSIDE the critical section — while is_active is still
        // true and the session is installed under this same lock. cancel_inner /
        // stop_inner take this lock to clear is_active and take the session, so
        // they are serialized strictly after this point and their stop_tray_pulse
        // always runs after ours. No install→pulse gap to race, no re-check.
        crate::start_tray_pulse(app);
    }
    // is_active was claimed at the top via compare_exchange — commit the guard
    // so it doesn't clear it on drop. Stays true until stop/cancel.
    active_guard.commit = true;

    // "recording" HUD was already emitted up-front (instant start).
    log::info!(
        "dictation: '{}' recording started (rate={}, channels={})",
        shortcut.name,
        sample_rate,
        channels
    );
    Ok(())
}

pub async fn stop_inner(app: &AppHandle) -> Result<String, String> {
    let session = {
        let state = app.state::<DictationState>();
        let mut guard = state
            .inner
            .lock()
            .map_err(|e| format!("state lock: {}", e))?;
        let s = guard.take();
        state.is_active.store(false, Ordering::Relaxed);
        s
    };

    // Capture is done — stop the tray-icon blink (transcribe/paste runs after).
    crate::stop_tray_pulse();

    let mut session = match session {
        Some(s) => s,
        None => {
            emit_status(app, "idle", None, None);
            return Ok(String::new());
        }
    };

    let shortcut_id = session.shortcut_id.clone();
    // ESC stays registered through the pipeline so the user can dismiss the
    // HUD at any time (transcribing/processing/pasting/error display). It's
    // released at the natural end of stop_inner or by force_hide_hud.

    // Grace period: let the last ~250ms of audio reach the cpal callback (and
    // the drain thread that's still emptying the ring into `samples`) before we
    // tear down. Otherwise the tail of the last word is truncated because the OS
    // audio queue hasn't flushed yet.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    // Pause + drop the stream, then stop and join the drain thread. Joining is
    // what guarantees the ring's final tail is in `samples` and the drain's Arc
    // clone is released — no more capture buffers stuck alive across sessions.
    teardown_capture(&mut session);

    // Stop the parallel system-audio tap (if any). Its `stop()` joins the
    // capture thread, so by the time it returns SYSTEM_BUFFER holds the full
    // loopback PCM at 48 kHz stereo for us to drain below. The setup ran in
    // a background spawn_blocking task — wait briefly for it to finish so we
    // can stop cleanly; if it's still mid-init we just discard, leaving
    // SYSTEM_BUFFER with whatever samples the half-started tap managed to
    // push (likely none).
    if let Some(handle) = session.system_setup.take() {
        match tokio::time::timeout(std::time::Duration::from_secs(2), handle).await {
            Ok(Ok(Some(mut rec))) => rec.stop(),
            Ok(Ok(None)) => {}
            Ok(Err(e)) => log::warn!("dictation: system tap setup task failed: {}", e),
            Err(_) => log::warn!("dictation: system tap still initializing after 2s — discarding"),
        }
    }

    if let Some(orig) = session.pre_duck_volume {
        restore_system_volume_to(orig);
    }

    let shortcut = find_shortcut(&shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' vanished mid-session", shortcut_id))?;

    // Everything past this point — finalize the transcript and deliver it — can
    // take seconds (sidecar transcription, optional LLM pipeline, paste). We run
    // it as a DETACHED, abortable task instead of awaiting inline so that Esc
    // can cancel it cleanly: aborting the task drops its future, which kills the
    // transcription sidecar (via SidecarKiller) and stops the flow before any
    // stale text reaches the focused field. The capture itself is fully torn
    // down above, so nothing audio-related is left running.
    let streaming = session.streaming.take();
    let channels = session.channels;
    let sample_rate = session.sample_rate;
    let generation = session.generation;
    // Batch path owns `samples` exclusively (drain thread already joined) — move
    // it out instead of cloning up to ~100 MB. Streaming path has no buffer.
    let raw_samples = if streaming.is_some() {
        Vec::new()
    } else {
        let mut s = session
            .samples
            .lock()
            .map_err(|e| format!("samples lock: {}", e))?;
        std::mem::take(&mut *s)
    };

    let app_task = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let _ = run_stop_pipeline(
            &app_task,
            shortcut,
            shortcut_id,
            streaming,
            raw_samples,
            channels,
            sample_rate,
            generation,
        )
        .await;
    });
    if let Ok(mut g) = app.state::<DictationState>().pipeline.lock() {
        *g = Some(handle);
    }
    Ok(String::new())
}

/// Detached tail of `stop_inner`: finalize the transcript (streaming finish or
/// batch convert+transcribe) and deliver it via `process_and_deliver`. Runs in
/// its own task so Esc can abort the whole thing mid-flight — see the spawn site
/// in `stop_inner` and `abort_pipeline`.
#[allow(clippy::too_many_arguments)]
async fn run_stop_pipeline(
    app: &AppHandle,
    shortcut: DictationShortcut,
    shortcut_id: String,
    streaming: Option<crate::dictation_streaming::StreamingSession>,
    raw_samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
    generation: u64,
) -> Result<String, String> {
    // Entry guard: Esc may have fired during stop_inner's teardown limbo
    // (is_active already false, no handle to abort yet), or a newer delivery may
    // have started. Either bumps the generation. Honour it before doing any work
    // — otherwise we'd transcribe + paste despite the cancel/supersession.
    if !is_current(app, generation) {
        emit_status(app, "idle", Some(&shortcut_id), Some("Cancelled".into()));
        return Ok(String::new());
    }

    // Streaming path: close the sample channel, wait for the sidecar to
    // flush + emit its final transcript. Bypasses the entire batch
    // normalize/resample/transcribe pipeline.
    if let Some(streaming) = streaming {
        emit_status(
            app,
            "transcribing",
            Some(&shortcut_id),
            Some("Finalizing live transcript".into()),
        );
        let t0 = std::time::Instant::now();
        let final_text = match streaming.finish().await {
            Ok(t) => t,
            Err(e) => return finish_with_error(app, &shortcut_id, "Transcription failed", e),
        };
        log::info!(
            "dictation: streaming finish {:?} (chars={})",
            t0.elapsed(),
            final_text.len()
        );
        let trimmed = final_text.trim().to_string();
        if trimmed.is_empty() {
            emit_status(
                app,
                "idle",
                Some(&shortcut_id),
                Some("No speech detected".into()),
            );
            return Ok(String::new());
        }
        // Streaming path has no buffered samples to save — pass None.
        return process_and_deliver(app, &shortcut, trimmed, None, generation).await;
    }

    if raw_samples.is_empty() {
        emit_status(
            app,
            "idle",
            Some(&shortcut_id),
            Some("No audio captured".into()),
        );
        return Ok(String::new());
    }

    let duration_secs = raw_samples.len() as f64 / (sample_rate as f64 * channels as f64);
    log::info!(
        "dictation: '{}' captured {:.2}s, transcribing...",
        shortcut.name,
        duration_secs
    );
    emit_status(app, "transcribing", Some(&shortcut_id), None);

    // From here on, any error MUST end with an emit_status that takes the
    // HUD out of "transcribing" — otherwise the HUD hangs and the user has
    // to restart nbp. Use `finish_with_error` for every fallible step.
    log::info!(
        "dictation: pipeline start — raw_samples={}, channels={}, rate={}",
        raw_samples.len(),
        channels,
        sample_rate
    );

    let t0 = std::time::Instant::now();
    let normalized = normalize_loudness(&raw_samples, channels, sample_rate);
    log::info!("dictation: normalize_loudness {:?}", t0.elapsed());

    let t1 = std::time::Instant::now();
    let mono = downmix_to_mono(&normalized, channels);
    log::info!("dictation: downmix_to_mono {:?}", t1.elapsed());

    let t2 = std::time::Instant::now();
    let mono_16k = if sample_rate == TARGET_RATE {
        mono
    } else {
        match resample_mono(&mono, sample_rate, TARGET_RATE) {
            Ok(s) => s,
            Err(e) => return finish_with_error(app, &shortcut_id, "Resample failed", e),
        }
    };
    log::info!(
        "dictation: resample_mono {:?} (skipped={}, out_samples={})",
        t2.elapsed(),
        sample_rate == TARGET_RATE,
        mono_16k.len()
    );

    // Optional system-audio mixdown. Drain SYSTEM_BUFFER (48 kHz stereo planar),
    // average to mono, resample to 16 kHz, sum with mic with safe headroom so
    // the loudest combined sample fits in [-1, 1] without clipping.
    let mono_16k = if shortcut.capture_system_audio {
        mix_in_system_audio(mono_16k)?
    } else {
        mono_16k
    };

    // The conversions above (normalize/downmix/resample/mix) are synchronous
    // and CPU-bound — a task abort can't interrupt them. Re-check the generation
    // before spawning the sidecar so an Esc during that window doesn't start a
    // transcription that nobody wants.
    if !is_current(app, generation) {
        emit_status(app, "idle", Some(&shortcut_id), Some("Cancelled".into()));
        return Ok(String::new());
    }

    let t3 = std::time::Instant::now();
    let transcript = match transcribe(app, &mono_16k).await {
        Ok(t) => t,
        Err(e) => return finish_with_error(app, &shortcut_id, "Transcription failed", e),
    };
    log::info!("dictation: transcribe {:?}", t3.elapsed());
    let trimmed = transcript.trim().to_string();
    if trimmed.is_empty() {
        emit_status(
            app,
            "idle",
            Some(&shortcut_id),
            Some("No speech detected".into()),
        );
        return Ok(String::new());
    }

    // Audio path: hand the (16 kHz mono) samples to process_and_deliver so it
    // can optionally save them as a full recording (Save dictations setting).
    process_and_deliver(app, &shortcut, trimmed, Some(mono_16k), generation).await
}

/// Surface a fatal error to the HUD and return an Err. The HUD listens for
/// non-"transcribing"/"recording" statuses to reset itself to idle, so we
/// MUST emit one before bubbling — otherwise the HUD hangs on the previous
/// state and the next hotkey press appears to do nothing.
fn finish_with_error(
    app: &AppHandle,
    shortcut_id: &str,
    summary: &str,
    err: String,
) -> Result<String, String> {
    log::warn!("dictation: {summary} — {err}");
    emit_status(
        app,
        "error",
        Some(shortcut_id),
        Some(format!("{summary}: {err}")),
    );
    Err(err)
}

/// Trigger flow for Clipboard-input shortcuts: snapshot the pasteboard, run
/// the pipeline if configured, and paste the result back. No mic capture.
pub async fn run_clipboard_inner(app: &AppHandle, shortcut_id: &str) -> Result<String, String> {
    let shortcut = find_shortcut(shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' not found", shortcut_id))?;

    // Fresh delivery — claim a generation (also supersedes any older in-flight
    // delivery) and carry it through so the pre-paste guard can detect an Esc /
    // newer delivery without false-tripping on a prior run's cancel.
    let generation = claim_generation(app);

    crate::hud::reposition(app);
    emit_status(app, "reading_clipboard", Some(&shortcut.id), None);
    let text = read_clipboard().map_err(|e| {
        emit_status(
            app,
            "error",
            Some(&shortcut.id),
            Some(format!("Clipboard read failed: {}", e)),
        );
        e
    })?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        emit_status(
            app,
            "idle",
            Some(&shortcut.id),
            Some("Clipboard is empty".into()),
        );
        return Ok(String::new());
    }

    // Clipboard input has no audio to save.
    process_and_deliver(app, &shortcut, trimmed, None, generation).await
}

/// Shared tail of both Audio and Clipboard flows: run the LLM pipeline (if any)
/// against `text`, then paste / copy the result. Emits status events along the
/// way. Returns the final text that was delivered.
async fn process_and_deliver(
    app: &AppHandle,
    shortcut: &DictationShortcut,
    input_text: String,
    audio_samples_16k: Option<Vec<f32>>,
    generation: u64,
) -> Result<String, String> {
    let shortcut_id = shortcut.id.clone();

    // Best-effort save as a full recording (transcript + audio) when the user
    // opted in via Settings → Dictation → Storage → Save dictations, and we
    // actually have audio (Audio-input shortcuts only). Failure here is
    // logged but does NOT block the pipeline/paste flow — the dictation
    // itself still works.
    if let Some(ref samples) = audio_samples_16k
        && !samples.is_empty()
        && matches!(
            shortcut.input_source,
            crate::config::DictationInputSource::Audio
        )
    {
        let settings_check = load_settings();
        if settings_check.dictation.save_dictations
            && let Err(e) = save_dictation_as_recording(app, samples, &input_text).await
        {
            log::warn!("dictation: save_dictations failed (non-fatal): {}", e);
        }
    }

    let final_text = if let Some(ref pipeline_name) = shortcut.pipeline {
        emit_status(app, "processing", Some(&shortcut_id), None);
        match run_text_pipeline(&input_text, pipeline_name).await {
            Ok(processed) => processed,
            Err(e) => {
                log::warn!(
                    "dictation: pipeline '{}' failed: {} — falling back to raw input",
                    pipeline_name,
                    e
                );
                emit_status(
                    app,
                    "pipeline_error",
                    Some(&shortcut_id),
                    Some(format!("Pipeline failed, pasted raw: {}", e)),
                );
                input_text.clone()
            }
        }
    } else {
        input_text.clone()
    };

    let final_trimmed = final_text.trim().to_string();
    if final_trimmed.is_empty() {
        emit_status(app, "idle", Some(&shortcut_id), Some("Empty output".into()));
        return Ok(String::new());
    }

    // Gate before the paste. transcribe→paste can have no intervening await (no
    // LLM step, no save), so a task abort might not fire in time; the generation
    // check makes the cancel authoritative. paste_text re-checks once more after
    // its synchronous settle, right before the keystroke.
    if !is_current(app, generation) {
        emit_status(app, "idle", Some(&shortcut_id), Some("Cancelled".into()));
        return Ok(String::new());
    }

    if shortcut.auto_paste {
        emit_status(app, "pasting", Some(&shortcut_id), None);
        match paste_text(&final_trimmed, app, generation) {
            Ok(PasteOutcome::Pasted) => {}
            Ok(PasteOutcome::Cancelled) => {
                emit_status(app, "idle", Some(&shortcut_id), Some("Cancelled".into()));
                return Ok(String::new());
            }
            Err(PasteError::AccessibilityDenied) => {
                log::warn!("dictation: Accessibility permission missing — text in clipboard only");
                emit_status(app, "accessibility_needed", Some(&shortcut_id), None);
                return Ok(final_trimmed);
            }
            Err(PasteError::Other(e)) => {
                log::warn!("dictation paste failed: {}", e);
                emit_status(
                    app,
                    "error",
                    Some(&shortcut_id),
                    Some(format!("Paste failed: {}", e)),
                );
                return Err(format!("Paste failed: {}", e));
            }
        }
    } else if let Err(e) = copy_to_clipboard(&final_trimmed) {
        log::warn!("dictation clipboard failed: {}", e);
    }

    emit_status(app, "idle", Some(&shortcut_id), None);
    log::info!(
        "dictation: '{}' done ({} chars)",
        shortcut.name,
        final_trimmed.len()
    );
    Ok(final_trimmed)
}

/// Optionally persist a dictation as a full recording (transcript + audio) so
/// it shows up in the main recordings list. Only invoked when the user opted
/// in via Settings → Dictation → Save dictations AND we have actual audio
/// samples (Audio-input shortcuts). Best-effort: any failure is logged and
/// returned, but the caller treats it as non-fatal (paste still happens).
async fn save_dictation_as_recording(
    app: &AppHandle,
    samples_16k: &[f32],
    transcript: &str,
) -> Result<(), String> {
    // Title mirrors the existing convention ("{App} · HH:MM" for calls,
    // "NBP · HH:MM" for manual) so the kind is readable at a glance in the
    // list. The explicit `source` field on the metadata is the canonical
    // signal for any code-level filtering.
    let title = format!("Dictation · {}", chrono::Local::now().format("%H:%M"));
    let metadata = crate::storage::create_recording(title, vec![])?;
    let recording_dir = crate::storage::get_recording_dir(&metadata.id);

    // Audio: encode the 16 kHz mono buffer as audio_mix.ogg — same filename
    // convention the player + finalize path use for meeting recordings.
    let audio_path = recording_dir.join("audio_mix.ogg");
    encode_mono_16k_to_ogg(&audio_path, samples_16k)
        .map_err(|e| format!("encode dictation audio: {}", e))?;
    let duration_sec = samples_16k.len() as f64 / TARGET_RATE as f64;

    // Transcript: same frontmatter format as meeting recordings, written via
    // the shared helper so reads (incl. legacy migration) keep working.
    let settings = load_settings();
    let tx_source = match settings.transcription.provider {
        TranscriptionProvider::AppleSpeech => crate::transcript_migration::TranscriptSource::Apple,
        _ => crate::transcript_migration::TranscriptSource::Fluidaudio,
    };
    let tx_meta = crate::transcript_migration::TranscriptMetadata {
        source: tx_source,
        model: format!("{:?}", settings.transcription.provider),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        duration_sec,
        language: None,
    };
    let transcript_path = recording_dir.join("transcript.md");
    crate::transcript_migration::write_transcript_with_metadata(
        &transcript_path,
        transcript,
        &tx_meta,
    )?;

    // Patch metadata.json: register the audio file, set status to "ready",
    // populate the list-preview, and stamp source="dictation" so the recording
    // list can tell it apart from manual / call recordings without parsing
    // the title format.
    let mut latest = crate::storage::read_metadata(&metadata.id)?;
    latest.audio.mix = Some(crate::storage::AudioInfo {
        file: "audio_mix.ogg".to_string(),
        duration_sec,
        sample_rate: TARGET_RATE,
        channels: 1,
    });
    latest.status = "ready".to_string();
    latest.source = "dictation".to_string();
    let preview: String = transcript.chars().take(200).collect();
    latest.transcript_preview = if preview.is_empty() {
        None
    } else {
        Some(preview)
    };
    crate::storage::write_metadata(&latest)?;

    // Nudge the frontend to refresh the recordings list immediately — reuse
    // the existing event so we don't add a second listener path.
    let _ = app.emit("recording_complete", &metadata.id);
    log::info!(
        "dictation: saved as recording id={} ({:.2}s, {} chars)",
        metadata.id,
        duration_sec,
        transcript.len()
    );
    Ok(())
}

/// Encode a 16 kHz mono f32 buffer as Vorbis OGG at `path`. Matches the
/// quality/format profile of the meeting-recording encoder (QualityVbr 0.4),
/// just mono+16k instead of stereo+48k.
fn encode_mono_16k_to_ogg(path: &std::path::Path, samples: &[f32]) -> Result<(), String> {
    use std::num::{NonZeroU8, NonZeroU32};
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};
    let file = std::fs::File::create(path).map_err(|e| format!("create: {}", e))?;
    let mut enc = VorbisEncoderBuilder::new_with_serial(
        NonZeroU32::new(TARGET_RATE).ok_or("bad rate")?,
        NonZeroU8::new(1).ok_or("bad channels")?,
        file,
        0,
    )
    .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: 0.4,
    })
    .build()
    .map_err(|e| format!("build: {}", e))?;
    // vorbis_rs takes planar slices — single channel = one slice.
    enc.encode_audio_block([samples])
        .map_err(|e| format!("encode: {}", e))?;
    enc.finish().map_err(|e| format!("finish: {}", e))?;
    Ok(())
}

async fn transcribe(app: &AppHandle, mono_16k: &[f32]) -> Result<String, String> {
    // Dictation uses the SAME engine + settings as recordings (the global ASR
    // config from the ASR settings tab), not a per-shortcut engine — one path,
    // so code-switch recovery (translit/vocab/min-len) applies here too. Apple
    // locale also comes from the global setting.
    let settings = load_settings();
    match settings.transcription.provider {
        TranscriptionProvider::AppleSpeech => {
            run_apple_speech(app, mono_16k, Some(&settings.transcription.apple_locale)).await
        }
        // FluidAudio, Qwen3, and `Unknown` (old settings.json referencing dead
        // cloud providers) all run FluidAudio — matches the config.rs contract
        // and the recording path, instead of erroring out.
        _ => run_fluidaudio(app, mono_16k, &settings).await,
    }
}

/// Runs `pipeline_name` against the dictation `text` and returns the text to
/// paste. Mirrors the recording pipeline — every step type runs (the user owns
/// what they put in the pipeline):
/// - CliAgent / Shell : transforms; their stdout becomes the running text.
/// - SaveLocal        : side-effect; writes to the configured folder and
///   leaves the running text unchanged, so the transform result is still what
///   gets pasted.
///
/// Placeholders match the engine: `{transcript}` = the original transcript,
/// `{processing_result}` = the running text (empty-effectively on step 1),
/// `{app}` = "Dictation" (no recording context here).
async fn run_text_pipeline(text: &str, pipeline_name: &str) -> Result<String, String> {
    let pipelines = load_pipelines().map_err(|e| format!("load pipelines: {}", e))?;
    let pipeline = pipelines
        .get(pipeline_name)
        .ok_or_else(|| format!("Pipeline '{}' not found", pipeline_name))?
        .clone();

    // Ephemeral context: dictation has no recording, so connector artifacts go
    // to a throwaway temp dir (the recordings UI never reads them) and the app
    // name is just "Dictation". Everything else runs through the *same*
    // `run_one_step` the recording engine uses — no duplicate dispatch.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let tmp_dir = std::env::temp_dir().join("nbp-dictation-pipeline");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let ctx = crate::pipeline_engine::StepContext {
        transcript: text,
        app: "Dictation",
        started_at: &now,
        output_dir: &tmp_dir,
        // Dictation has no calendar match; only {date} is meaningful.
        calendar_title: "",
        calendar_attendees: "",
        date: &date,
    };

    let mut chained = String::new(); // {processing_result} for the next step (engine semantics)
    let mut paste = text.to_string(); // what we actually paste — the last transform output

    for step in &pipeline.steps {
        match crate::pipeline_engine::run_one_step(step, &chained, &ctx).await {
            Ok(body) => {
                // Transforms (CLI/Shell) update what gets pasted; SaveLocal is a
                // side-effect and leaves the paste text alone. A transform that
                // emits nothing (e.g. a delivery `curl`) also leaves it alone.
                if matches!(step.step_type, StepType::CliAgent | StepType::Shell)
                    && !body.trim().is_empty()
                {
                    paste = body.clone();
                }
                chained = body;
                log::info!(
                    "dictation pipeline '{}': step '{}' ({:?}) ok",
                    pipeline_name,
                    step.name,
                    step.step_type
                );
            }
            Err(e) => {
                // Save failures are non-fatal — a side-effect shouldn't cost the
                // user their paste. Transform failures propagate so the caller
                // falls back to pasting the raw transcript.
                if matches!(step.step_type, StepType::SaveLocal) {
                    log::warn!(
                        "dictation pipeline '{}': save step '{}' failed (non-fatal): {}",
                        pipeline_name,
                        step.name,
                        e
                    );
                } else {
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                    return Err(format!("step '{}': {}", step.name, e));
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(paste)
}

// --- Tauri commands -------------------------------------------------------

/// Cancel an in-flight dictation session: stop the mic, drop the samples, hide
/// the HUD, restore volume. No transcription, no paste, no clipboard write.
pub fn cancel_inner(app: &AppHandle) {
    // Defensive: if a stop pipeline somehow raced in, supersede + abort it too
    // so cancel never leaves an orphaned sidecar or a pending paste behind.
    supersede(app);
    abort_pipeline(app);
    let session = {
        let state = app.state::<DictationState>();
        let mut guard = match state.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let s = guard.take();
        state.is_active.store(false, Ordering::Relaxed);
        s
    };
    crate::stop_tray_pulse();
    let mut session = match session {
        Some(s) => s,
        None => return,
    };
    // Pause + drop the stream and join the drain thread (same teardown the stop
    // path uses) so the callback stops, the drain thread exits, and no capture
    // buffer is left alive across shortcut cycles. Cancel discards the samples.
    teardown_capture(&mut session);
    if let Some(handle) = session.system_setup.take() {
        // Cancel path is sync — can't await. Abort the background setup.
        // If the spawn_blocking already produced a SystemAudioRecorder,
        // aborting drops that result; SystemAudioRecorder's Drop impl then
        // calls stop(), joining and ending the capture thread (otherwise it
        // would loop forever — this was the system-audio memory leak).
        handle.abort();
    }
    unregister_escape_cancel(app);
    if let Some(orig) = session.pre_duck_volume {
        restore_system_volume_to(orig);
    }
    if let Some(streaming) = session.streaming.take() {
        streaming.cancel();
    }

    log::info!("dictation: '{}' cancelled via Escape", session.shortcut_id);
    emit_status(
        app,
        "idle",
        Some(&session.shortcut_id),
        Some("Cancelled".into()),
    );
}

fn register_escape_cancel(app: &AppHandle) {
    #[cfg(desktop)]
    {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
        let app_clone = app.clone();
        let result = app
            .global_shortcut()
            .on_shortcut("Escape", move |_app, _shortcut, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                let app = app_clone.clone();
                tauri::async_runtime::spawn(async move {
                    // If a session is in-flight: cancel mic + emit idle.
                    // If only the post-pipeline HUD is lingering (error/done
                    // state): just force-hide the window via emit_status so
                    // the user can dismiss it instantly with Esc.
                    let has_session = app
                        .state::<DictationState>()
                        .is_active
                        .load(Ordering::Relaxed);
                    if has_session {
                        cancel_inner(&app);
                    } else {
                        force_hide_hud(&app);
                    }
                });
            });
        if let Err(e) = result {
            log::warn!("dictation: failed to register Escape cancel: {}", e);
        }
    }
}

/// Force-hide the dictation HUD immediately — used by Esc when the user is
/// staring at a stale error/done message and wants it gone. Sends an
/// `idle` event with `Cancelled` so the JS hide path runs with 0ms delay.
pub fn force_hide_hud(app: &AppHandle) {
    // Esc during the post-capture pipeline (transcribing/processing/pasting):
    // bump the generation (authoritative supersede) AND kill the in-flight task.
    // The generation covers the limbo window where the pipeline task isn't
    // stored yet (so abort_pipeline is a no-op) — run_stop_pipeline re-checks it
    // and bails before pasting. The abort is the fast-kill for an already-running
    // sidecar. Both are no-ops when only a lingering error/done HUD is dismissed.
    supersede(app);
    abort_pipeline(app);
    emit_status(app, "idle", None, Some("Cancelled".into()));
    unregister_escape_cancel(app);
}

fn unregister_escape_cancel(app: &AppHandle) {
    #[cfg(desktop)]
    {
        use tauri_plugin_global_shortcut::GlobalShortcutExt;
        let _ = app.global_shortcut().unregister("Escape");
    }
}

/// Called from JS when the HUD's CSS hide animation actually finishes — at
/// that point Esc no longer has any HUD to dismiss, so we release the global
/// hook (returning Esc to whatever app the user is in).
#[tauri::command]
pub fn dictation_release_esc(app: AppHandle) {
    unregister_escape_cancel(&app);
}

#[tauri::command]
pub async fn dictation_start(app: AppHandle, shortcut_id: String) -> Result<(), String> {
    start_inner(&app, &shortcut_id).await
}

#[tauri::command]
pub fn dictation_cancel(app: AppHandle) {
    cancel_inner(&app);
}

#[tauri::command]
pub async fn dictation_stop(app: AppHandle) -> Result<String, String> {
    stop_inner(&app).await
}

#[tauri::command]
pub async fn dictation_toggle(
    app: AppHandle,
    shortcut_id: String,
) -> Result<Option<String>, String> {
    let active = app
        .state::<DictationState>()
        .is_active
        .load(Ordering::Relaxed);
    if active {
        Ok(Some(stop_inner(&app).await?))
    } else {
        start_inner(&app, &shortcut_id).await?;
        Ok(None)
    }
}

#[tauri::command]
pub fn dictation_is_active(state: tauri::State<'_, DictationState>) -> bool {
    state.is_active.load(Ordering::Relaxed)
}

/// Re-register all dictation hotkeys based on the current settings. Called by
/// the frontend after saving Settings → Shortcuts. Returns per-shortcut status
/// so the UI can show conflicts inline.
#[tauri::command]
pub fn dictation_reload_shortcuts(
    app: AppHandle,
) -> Result<Vec<crate::ShortcutRegistration>, String> {
    crate::reload_dictation_shortcuts(&app)
}

// --- audio helpers --------------------------------------------------------

/// Quick peak normalize: find the loudest sample, apply uniform gain to lift
/// it toward `target_peak` (with a hard `max_gain` cap so we don't blow up
/// silence into noise). Single pass, ~40× faster than EBU R128 on big buffers.
/// Whisper is robust to absolute level — what matters is that quiet speech
/// reaches a reasonable amplitude before transcription.
fn normalize_loudness(interleaved: &[f32], _channels: u16, _sample_rate: u32) -> Vec<f32> {
    const TARGET_PEAK: f32 = 0.7;
    const MAX_GAIN: f32 = 20.0;
    if interleaved.is_empty() {
        return Vec::new();
    }
    let max_abs = interleaved.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
    if max_abs < 1e-4 {
        return interleaved.to_vec();
    }
    let gain = (TARGET_PEAK / max_abs).min(MAX_GAIN);
    if gain <= 1.0 {
        return interleaved.to_vec();
    }
    interleaved
        .iter()
        .map(|&s| (s * gain).clamp(-1.0, 1.0))
        .collect()
}

fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let ch = channels as usize;
    interleaved
        .chunks_exact(ch)
        .map(|chunk| chunk.iter().sum::<f32>() / ch as f32)
        .collect()
}

fn resample_mono(input: &[f32], src_rate: u32, dst_rate: u32) -> Result<Vec<f32>, String> {
    use crate::resampler_compat::FftFixedInOut;

    // FFT-based resampler — orders of magnitude faster than the polyphase
    // Sinc path for integer ratios like 48k→16k (gcd=16k, ratio 3:1). Quality
    // is more than adequate for speech-to-text.
    let mut resampler = FftFixedInOut::<f32>::new(src_rate as usize, dst_rate as usize, 1024, 1)
        .map_err(|e| format!("resampler init: {}", e))?;

    let chunk_in = resampler.input_frames_next();
    let mut output =
        Vec::with_capacity(((input.len() * dst_rate as usize) / src_rate as usize) + 256);
    let mut pos = 0;
    while pos + chunk_in <= input.len() {
        let chunk = vec![&input[pos..pos + chunk_in]];
        let res = resampler
            .process(&chunk, None)
            .map_err(|e| format!("resample: {}", e))?;
        output.extend(&res[0]);
        pos += chunk_in;
    }
    if pos < input.len() {
        let chunk = vec![&input[pos..]];
        if let Ok(res) = resampler.process_partial(Some(&chunk), None) {
            output.extend(&res[0]);
        }
    }
    Ok(output)
}

/// Drain SYSTEM_BUFFER (48 kHz stereo planar), downmix to mono, resample to
/// 16 kHz, and mix with `mic` sample-by-sample with safe headroom. Returns
/// `mic` unchanged on failure or when no system audio was captured.
fn mix_in_system_audio(mic: Vec<f32>) -> Result<Vec<f32>, String> {
    const SYS_RATE: u32 = 48_000;
    let buf = &crate::audio_processing::SYSTEM_BUFFER;
    let frames = buf.available();
    if frames == 0 {
        log::info!("dictation: capture_system_audio=true but no loopback samples captured");
        return Ok(mic);
    }
    let (left, right) = buf.pop(frames);
    if left.is_empty() {
        return Ok(mic);
    }
    let mono_48k: Vec<f32> = left
        .iter()
        .zip(right.iter())
        .map(|(l, r)| 0.5 * (l + r))
        .collect();
    let sys_16k = resample_mono(&mono_48k, SYS_RATE, TARGET_RATE)?;
    let len = mic.len().max(sys_16k.len());
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let m = mic.get(i).copied().unwrap_or(0.0);
        let s = sys_16k.get(i).copied().unwrap_or(0.0);
        out.push((0.7 * m + 0.5 * s).clamp(-1.0, 1.0));
    }
    log::info!(
        "dictation: mixed in system audio — mic_len={}, sys_len={}, out_len={}",
        mic.len(),
        sys_16k.len(),
        out.len()
    );
    Ok(out)
}

fn write_mono_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> Result<(), String> {
    use hound::{SampleFormat, WavSpec, WavWriter};
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut w = WavWriter::create(path, spec).map_err(|e| format!("wav create: {}", e))?;
    for &s in samples {
        w.write_sample((s * 32768.0).clamp(-32768.0, 32767.0) as i16)
            .map_err(|e| format!("wav write: {}", e))?;
    }
    w.finalize().map_err(|e| format!("wav finalize: {}", e))?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct FluidOut {
    text: String,
    #[allow(dead_code)]
    model: String,
}

async fn run_fluidaudio(
    app: &AppHandle,
    samples_16k: &[f32],
    settings: &crate::config::AppSettings,
) -> Result<String, String> {
    use tauri_plugin_shell::ShellExt;

    let tmp = std::env::temp_dir().join(format!("nbp-dict-{}.wav", uuid::Uuid::new_v4().simple()));
    write_mono_wav(&tmp, samples_16k, TARGET_RATE)?;

    // Quick Dictate is single-speaker — always skip diarization (saves the
    // diarizer load/process + dodges the "No speech detected" VAD failure on
    // short clips). Engine + code-switch recovery flags are the SAME as the
    // recording path (fluidaudio_engine_args), so dictation and recordings
    // can't drift.
    let mut cmd = app
        .shell()
        .sidecar("fluidaudio-sidecar")
        .map_err(|e| format!("sidecar create: {}", e))?
        .arg(tmp.to_str().ok_or("invalid tmp path")?)
        .arg("--no-diarize");
    for a in crate::transcription::fluidaudio_engine_args(settings) {
        cmd = cmd.arg(a);
    }
    let (mut rx, child) = cmd.spawn().map_err(|e| format!("sidecar spawn: {}", e))?;
    // Kill the sidecar if this future is dropped (e.g. the pipeline task is
    // aborted by Esc) so the model doesn't stay resident burning CPU.
    let mut killer = SidecarKiller(Some(child));

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf = String::new();
    let mut exit_code: Option<i32> = None;

    while let Some(event) = rx.recv().await {
        use tauri_plugin_shell::process::CommandEvent;
        match event {
            CommandEvent::Stdout(data) => stdout_buf.extend_from_slice(&data),
            CommandEvent::Stderr(data) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&data));
            }
            CommandEvent::Terminated(p) => {
                exit_code = p.code;
                // Process already exited — disarm so Drop doesn't kill() a
                // reaped PID (which could, in theory, have been reused).
                killer.0 = None;
                break;
            }
            _ => {}
        }
    }

    let _ = std::fs::remove_file(&tmp);

    // Surface sidecar diagnostics (incl. TRANSLIT:/VOCAB: replacement lines) to
    // the dev terminal, same as the recording path — so code-switch fixes are
    // observable for dictation too.
    for line in stderr_buf.lines() {
        if !line.starts_with("PROGRESS:") && !line.is_empty() {
            eprintln!("{}", line);
        }
    }

    if exit_code != Some(0) {
        // "No speech detected" isn't a failure — it's the sidecar's polite
        // way of saying the audio was silent. Surface it as an empty
        // transcript so stop_inner takes the normal idle-with-message path
        // ("No speech detected" in the HUD) instead of error-with-stacktrace.
        if stderr_buf.contains("No speech detected") {
            return Ok(String::new());
        }
        return Err(format!("FluidAudio sidecar failed: {}", stderr_buf));
    }

    let out: FluidOut = serde_json::from_slice(&stdout_buf)
        .map_err(|e| format!("parse FluidAudio output: {}", e))?;
    Ok(out.text)
}

/// Apple SpeechAnalyzer dictation path — mirrors run_fluidaudio but spawns the
/// apple-speech-sidecar binary. Output JSON shape is identical (text + model)
/// so the parser is shared.
async fn run_apple_speech(
    app: &AppHandle,
    samples_16k: &[f32],
    language: Option<&str>,
) -> Result<String, String> {
    use tauri_plugin_shell::ShellExt;

    let tmp = std::env::temp_dir().join(format!("nbp-dict-{}.wav", uuid::Uuid::new_v4().simple()));
    write_mono_wav(&tmp, samples_16k, TARGET_RATE)?;

    let mut cmd = app
        .shell()
        .sidecar("apple-speech-sidecar")
        .map_err(|e| format!("sidecar create: {}", e))?
        .arg(tmp.to_str().ok_or("invalid tmp path")?);
    // Apple SpeechAnalyzer requires a Locale at init — no auto-detection.
    // The sidecar falls back to `Locale.current` (= system locale) if no
    // --lang is passed, which on an English-set Mac mangles Russian audio.
    if let Some(lang) = language.filter(|s| !s.is_empty()) {
        cmd = cmd.arg("--lang").arg(lang);
    }
    let (mut rx, child) = cmd.spawn().map_err(|e| format!("sidecar spawn: {}", e))?;
    // Kill the sidecar if this future is dropped (Esc-aborted pipeline task).
    let mut killer = SidecarKiller(Some(child));

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf = String::new();
    let mut exit_code: Option<i32> = None;
    while let Some(event) = rx.recv().await {
        use tauri_plugin_shell::process::CommandEvent;
        match event {
            CommandEvent::Stdout(data) => stdout_buf.extend_from_slice(&data),
            CommandEvent::Stderr(data) => stderr_buf.push_str(&String::from_utf8_lossy(&data)),
            CommandEvent::Terminated(p) => {
                exit_code = p.code;
                // Process already exited — disarm so Drop doesn't kill() a
                // reaped PID.
                killer.0 = None;
                break;
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_file(&tmp);
    if exit_code != Some(0) {
        return Err(format!("Apple Speech sidecar failed: {}", stderr_buf));
    }
    let out: FluidOut = serde_json::from_slice(&stdout_buf)
        .map_err(|e| format!("parse Apple Speech output: {}", e))?;
    Ok(out.text)
}

/// Pick the smallest-overhead input config the device supports. Preference
/// order: 16 kHz mono (Whisper's native rate — no resample/downmix at all),
/// then 16 kHz multichannel (only downmix), then whatever the device defaults
/// to. Built-in MacBook mics typically advertise 16 kHz mono.
fn pick_input_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, String> {
    const PREFERRED_RATE: u32 = TARGET_RATE;
    let configs: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_input_configs()
        .map(|it| it.collect())
        .unwrap_or_default();

    for c in &configs {
        log::debug!(
            "dictation: cpal advertises — channels={}, rate_range={}..={}, format={:?}",
            c.channels(),
            c.min_sample_rate(),
            c.max_sample_rate(),
            c.sample_format()
        );
    }

    let rate_ok = |cfg: &cpal::SupportedStreamConfigRange| {
        cfg.min_sample_rate() <= PREFERRED_RATE && cfg.max_sample_rate() >= PREFERRED_RATE
    };

    if let Some(cfg) = configs.iter().find(|c| c.channels() == 1 && rate_ok(c)) {
        return Ok((*cfg).with_sample_rate(PREFERRED_RATE));
    }
    if let Some(cfg) = configs.iter().find(|c| rate_ok(c)) {
        return Ok((*cfg).with_sample_rate(PREFERRED_RATE));
    }
    device
        .default_input_config()
        .map_err(|e| format!("default_input_config: {}", e))
}

fn restore_system_volume_to(target: u32) {
    let now = get_system_output_volume();
    log::info!(
        "dictation: restoring volume — current={:?}, target={}",
        now,
        target
    );
    if let Some(now) = now
        && target != now
    {
        fade_system_volume(now, target, 750);
    }
}

fn get_system_output_volume() -> Option<u32> {
    // Native CoreAudio read (replaces the old `osascript` fork). Master output
    // volume comes back as 0.0..=1.0; expose 0..=100 to keep the duck/restore
    // contract used by the rest of this module unchanged.
    coreaudio_volume::get_scalar().map(|v| (v * 100.0).round() as u32)
}

// Generation counter so a freshly-started fade supersedes an in-flight one.
// Replaces the old "kill the previous osascript pid" trick: each fade claims a
// generation and the worker thread bails the instant a newer one starts — so a
// down-fade can't keep clobbering a later up-fade.
static FADE_GEN: AtomicU64 = AtomicU64::new(0);

fn fade_system_volume(from: u32, to: u32, duration_ms: u64) {
    let generation = FADE_GEN.fetch_add(1, Ordering::SeqCst) + 1;

    // Native set is cheap (no process fork), so we can afford smooth steps.
    const STEPS: u64 = 12;
    let step_delay = std::time::Duration::from_millis((duration_ms / STEPS).max(1));
    let from = from as f32;
    let to = to as f32;

    std::thread::spawn(move || {
        for i in 1..=STEPS {
            if FADE_GEN.load(Ordering::SeqCst) != generation {
                return; // a newer fade started — abandon this one mid-flight
            }
            let v = from + (to - from) * (i as f32 / STEPS as f32);
            coreaudio_volume::set_scalar(v / 100.0);
            std::thread::sleep(step_delay);
        }
    });
}

/// Native master output-volume control via CoreAudio's
/// `kAudioHardwareServiceDeviceProperty_VirtualMainVolume` (Float32 0.0..=1.0).
/// Replaces shelling out to `osascript` for the dictation duck: no process
/// fork, instant, and it links against the CoreAudio framework already pulled
/// in by the system-audio capture path.
#[allow(non_upper_case_globals, non_snake_case)]
mod coreaudio_volume {
    use std::os::raw::c_void;

    type AudioObjectID = u32;
    type OSStatus = i32;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const kAudioObjectSystemObject: AudioObjectID = 1;
    const kAudioHardwarePropertyDefaultOutputDevice: u32 = 0x644F_7574; // 'dOut'
    const kAudioHardwareServiceDeviceProperty_VirtualMainVolume: u32 = 0x766D_7663; // 'vmvc'
    const kAudioObjectPropertyScopeGlobal: u32 = 0x676C_6F62; // 'glob'
    const kAudioObjectPropertyScopeOutput: u32 = 0x6F75_7470; // 'outp'
    const kAudioObjectPropertyElementMain: u32 = 0;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            in_object_id: AudioObjectID,
            in_address: *const AudioObjectPropertyAddress,
            in_qualifier_data_size: u32,
            in_qualifier_data: *const c_void,
            io_data_size: *mut u32,
            out_data: *mut c_void,
        ) -> OSStatus;

        fn AudioObjectSetPropertyData(
            in_object_id: AudioObjectID,
            in_address: *const AudioObjectPropertyAddress,
            in_qualifier_data_size: u32,
            in_qualifier_data: *const c_void,
            in_data_size: u32,
            in_data: *const c_void,
        ) -> OSStatus;
    }

    fn default_output_device() -> Option<AudioObjectID> {
        let addr = AudioObjectPropertyAddress {
            selector: kAudioHardwarePropertyDefaultOutputDevice,
            scope: kAudioObjectPropertyScopeGlobal,
            element: kAudioObjectPropertyElementMain,
        };
        let mut dev: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let st = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut dev as *mut _ as *mut c_void,
            )
        };
        if st == 0 && dev != 0 { Some(dev) } else { None }
    }

    fn volume_address() -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            selector: kAudioHardwareServiceDeviceProperty_VirtualMainVolume,
            scope: kAudioObjectPropertyScopeOutput,
            element: kAudioObjectPropertyElementMain,
        }
    }

    /// Master output volume as 0.0..=1.0, or None if the current default output
    /// device exposes no settable main volume (some aggregate/virtual devices).
    pub fn get_scalar() -> Option<f32> {
        let dev = default_output_device()?;
        let addr = volume_address();
        let mut vol: f32 = 0.0;
        let mut size = std::mem::size_of::<f32>() as u32;
        let st = unsafe {
            AudioObjectGetPropertyData(
                dev,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut vol as *mut _ as *mut c_void,
            )
        };
        if st == 0 {
            Some(vol.clamp(0.0, 1.0))
        } else {
            None
        }
    }

    /// Set master output volume from a 0.0..=1.0 scalar. Returns false if the
    /// device rejected it (e.g. no main volume control).
    pub fn set_scalar(v: f32) -> bool {
        let Some(dev) = default_output_device() else {
            return false;
        };
        let addr = volume_address();
        let vol = v.clamp(0.0, 1.0);
        let st = unsafe {
            AudioObjectSetPropertyData(
                dev,
                &addr,
                0,
                std::ptr::null(),
                std::mem::size_of::<f32>() as u32,
                &vol as *const _ as *const c_void,
            )
        };
        st == 0
    }
}

// NSPasteboard directly, NOT pbcopy/pbpaste. A .app launched from Finder/
// Dock inherits no UTF-8 LANG, so the CLI tools fall back to Mac Roman and
// mangle any non-ASCII transcript into mojibake (works in `tauri dev` only
// because the terminal exports LANG). The native API is locale-independent.
const NS_PASTEBOARD_TYPE_STRING: &str = "public.utf8-plain-text";

fn read_clipboard() -> Result<String, String> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            return Err("NSPasteboard unavailable".into());
        }
        let ns_type = NSString::from_str(NS_PASTEBOARD_TYPE_STRING);
        let s: Option<Retained<NSString>> = msg_send![pb, stringForType: &*ns_type];
        Ok(s.map(|v| v.to_string()).unwrap_or_default())
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            return Err("NSPasteboard unavailable".into());
        }
        let _: isize = msg_send![pb, clearContents];
        let ns_text = NSString::from_str(text);
        let ns_type = NSString::from_str(NS_PASTEBOARD_TYPE_STRING);
        let ok: bool = msg_send![pb, setString: &*ns_text, forType: &*ns_type];
        if !ok {
            return Err("NSPasteboard setString failed".into());
        }
    }
    Ok(())
}

enum PasteError {
    /// Accessibility permission is not granted to NBP — CGEventPost would
    /// silently no-op. Text stays in the clipboard for the user to paste
    /// manually with ⌘V.
    AccessibilityDenied,
    Other(String),
}

// --- macOS Accessibility + key-posting FFI -------------------------------
//
// Posting ⌘V directly via CGEventPost in this process is critical: macOS
// associates the Accessibility prompt with the bundle that calls
// AXIsProcessTrustedWithOptions / CGEvent. Spawning `osascript` instead
// would attribute the request to /usr/bin/osascript and the user would
// never see a prompt asking to trust NBP itself.
//
// AXIsProcessTrustedWithOptions lives in ApplicationServices.framework.
// CGEvent* APIs live in the same framework.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    fn CGEventCreateKeyboardEvent(
        source: *const std::ffi::c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *const std::ffi::c_void;
    fn CGEventSetFlags(event: *const std::ffi::c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *const std::ffi::c_void);
    fn CFRelease(cf: *const std::ffi::c_void);
}

const KEY_V: u16 = 9; // ANSI 'v' virtual key
const CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x100000;
const CG_HID_EVENT_TAP: u32 = 0;

pub fn is_ax_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Trigger the macOS Accessibility permission prompt for NBP. Returns the
/// current trusted state. macOS displays the dialog only the first time;
/// subsequent calls just return the current state.
pub fn request_ax_prompt() -> bool {
    use objc2::ClassType;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    let key = NSString::from_str("AXTrustedCheckOptionPrompt");
    let value = NSNumber::new_bool(true);

    // NSDictionary::from_slices needs an unresolvable CopiedKey generic for our
    // call site; use the canonical Cocoa class method instead.
    let dict: Retained<NSDictionary<NSString, NSNumber>> = unsafe {
        let cls = NSDictionary::<NSString, NSNumber>::class();
        msg_send![cls, dictionaryWithObject: &*value, forKey: &*key]
    };
    let ptr = Retained::as_ptr(&dict) as *const AnyObject as *const std::ffi::c_void;
    unsafe { AXIsProcessTrustedWithOptions(ptr) }
}

fn post_cmd_v() {
    unsafe {
        let down = CGEventCreateKeyboardEvent(std::ptr::null(), KEY_V, true);
        if !down.is_null() {
            CGEventSetFlags(down, CG_EVENT_FLAG_MASK_COMMAND);
            CGEventPost(CG_HID_EVENT_TAP, down);
            CFRelease(down);
        }
        let up = CGEventCreateKeyboardEvent(std::ptr::null(), KEY_V, false);
        if !up.is_null() {
            CGEventSetFlags(up, CG_EVENT_FLAG_MASK_COMMAND);
            CGEventPost(CG_HID_EVENT_TAP, up);
            CFRelease(up);
        }
    }
}

enum PasteOutcome {
    Pasted,
    /// The delivery was superseded (Esc / newer delivery) during the
    /// synchronous pre-keystroke settle — clipboard was written but Cmd+V was
    /// NOT sent, so no stale text landed in the focused field.
    Cancelled,
}

fn paste_text(text: &str, app: &AppHandle, generation: u64) -> Result<PasteOutcome, PasteError> {
    copy_to_clipboard(text).map_err(PasteError::Other)?;
    if !is_ax_trusted() {
        return Err(PasteError::AccessibilityDenied);
    }
    // Tiny delay to let the pasteboard settle before the keystroke.
    std::thread::sleep(std::time::Duration::from_millis(40));
    // Final cancel gate. This settle is synchronous, so a task abort can't
    // interrupt it — re-check the generation right before the keystroke so an
    // Esc landing in this ~40ms window doesn't fire Cmd+V into the focused
    // field. The only window left after this is the CGEventPost itself
    // (microseconds), which is genuinely past the point of no return.
    if !is_current(app, generation) {
        return Ok(PasteOutcome::Cancelled);
    }
    post_cmd_v();
    Ok(PasteOutcome::Pasted)
}

// Caret/Accessibility positioning was removed — the HUD now anchors to the
// mouse cursor instead, which is always available in every app.
