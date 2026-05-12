//! Quick Dictate: hotkey-driven ephemeral mic capture → transcribe → optional
//! LLM pipeline → paste. Bypasses the main recording pipeline (no storage, no
//! system audio, no EBU). Supports N independent shortcuts, each with its own
//! engine + optional LLM-only pipeline.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::{
    get_api_key_for_provider, load_settings, DictationShortcut, TranscriptionProvider,
};
use crate::pipelines::{load_pipelines, ConnectorType};

const TARGET_RATE: u32 = 16_000;

pub struct DictationState {
    pub is_active: Arc<AtomicBool>,
    inner: Mutex<Option<Session>>,
    pub last_registration: Mutex<Vec<crate::ShortcutRegistration>>,
    /// Pre-warmed streaming sidecar — kept idle in the background so the
    /// first hotkey press never pays the model-load cost. Filled by a task
    /// spawned during app setup (see `prewarm_streaming` below).
    pub warm_streaming: Arc<Mutex<Option<crate::dictation_streaming::StreamingSession>>>,
}

struct Session {
    shortcut_id: String,
    samples: Arc<Mutex<Vec<f32>>>,
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
            warm_streaming: Arc::new(Mutex::new(None)),
        }
    }
}

/// Default capture rate/channels we pre-warm for. Built-in MacBook mic and
/// most USB mics natively expose 48 kHz mono — if the user later pins a
/// device with a different rate, we fall back to spawning a fresh inline
/// session in start_inner.
const PREWARM_RATE: u32 = 48_000;
const PREWARM_CHANNELS: u16 = 1;

/// Spawn a background task that fills the warm-streaming slot. Called at app
/// setup AND after every shortcut press that consumed the warm session, so
/// the next press also feels instant.
#[allow(dead_code)]
pub fn schedule_warm_streaming(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<DictationState>();
        // Skip if there's already a warm session in the slot.
        {
            let guard = match state.warm_streaming.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if guard.is_some() {
                return;
            }
        }
        log::info!("dictation: warming streaming sidecar in background");
        match crate::dictation_streaming::StreamingSession::start(
            &app,
            String::new(),
            PREWARM_RATE,
            PREWARM_CHANNELS,
        )
        .await
        {
            Ok(session) => {
                let state = app.state::<DictationState>();
                if let Ok(mut guard) = state.warm_streaming.lock() {
                    *guard = Some(session);
                    log::info!("dictation: warm streaming sidecar ready");
                }
            }
            Err(e) => {
                log::warn!("dictation: failed to warm streaming sidecar: {}", e);
            }
        }
    });
}

/// Read-only snapshot of the most recent shortcut-registration result, used
/// by the frontend to render status badges without triggering a re-register.
#[tauri::command]
pub fn dictation_get_registration_status(
    state: tauri::State<'_, DictationState>,
) -> Vec<crate::ShortcutRegistration> {
    state.last_registration.lock().map(|g| g.clone()).unwrap_or_default()
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
    let mut active_guard = ActiveGuard { flag: active_flag, commit: false };

    let shortcut = find_shortcut(shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' not found", shortcut_id))?;

    let host = cpal::default_host();
    let device = if let Some(name) = shortcut.device_name.as_deref() {
        crate::devices::get_device_by_name(name).or_else(|| host.default_input_device())
    } else {
        host.default_input_device()
    }
    .ok_or("No input device available")?;

    let config = pick_input_config(&device)?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    log::info!(
        "dictation: chose input config — rate={}, channels={}, format={:?}",
        sample_rate,
        channels,
        config.sample_format()
    );

    // Streaming setup. Three tiers:
    //   1) Warm pool — sidecar pre-spawned at app startup, model already
    //      loaded. Instant: just claim it.
    //   2) Fresh inline spawn — fallback when the warm slot is empty (first
    //      press before warm-up finished, or shortcut uses an exotic rate).
    //   3) Batch path — for non-streaming engines.
    // Streaming is DISABLED — Parakeet EOU 120M (the streaming model) is
    // English-biased and butchers non-English speech. Always taking the
    // batch path (Parakeet TDT v3, multilingual) until we get an equally
    // good streaming model. Code path kept above as dormant scaffolding.
    let _ = sample_rate; // silence unused warnings for streaming-only refs
    let _ = channels;
    let streaming: Option<crate::dictation_streaming::StreamingSession> = None;
    let streaming_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<f32>>> =
        streaming.as_ref().map(|s| s.samples_tx.clone());

    // Fresh meter state per session — last shortcut's mic/level shouldn't
    // bias the adaptive gain of the next one.
    PEAK_TRACKER.store(0, Ordering::Relaxed);

    let samples: Arc<Mutex<Vec<f32>>> =
        Arc::new(Mutex::new(Vec::with_capacity((sample_rate as usize) * 30)));
    let samples_cb = samples.clone();

    let err_fn = |err| log::error!("dictation cpal stream error: {}", err);

    fn push_level(samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        let rms = (sum_sq / samples.len() as f32).sqrt();
        let peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
        let signal = rms * 0.6 + peak * 0.4;

        // AGC: slowly-decaying running peak compensates for mic sensitivity.
        // Decay is intentionally slow (~10s window) so it learns the user's
        // loud syllables and treats them as 0 dB — quieter parts of speech
        // then show as -10..-30 dB below, giving the meter natural dynamics
        // between syllables instead of pegging at 100% during speech.
        const FLOOR: f32 = 0.005;
        const DECAY: f32 = 0.9995;
        let prev = f32::from_bits(PEAK_TRACKER.load(Ordering::Relaxed));
        let agc_max = (prev * DECAY).max(signal).max(FLOOR);
        PEAK_TRACKER.store(agc_max.to_bits(), Ordering::Relaxed);

        // Convert to dB relative to AGC max (0 dB = current loudness ceiling)
        // and map a 40 dB range to the 0..1 meter. Speech phonemes naturally
        // span 20-30 dB → every syllable visibly moves the bar.
        let gained = (signal / agc_max).max(1e-5);
        let db = 20.0 * gained.log10();
        let level = ((db + 40.0) / 40.0).clamp(0.0, 1.0);
        crate::mic_audio::set_audio_level(level);
    }

    // When streaming is active, samples go straight to the sidecar — no need
    // to also hoard them in `samples`. When streaming is None we still need
    // the in-memory buffer for the batch fallback.
    let streaming_tx_f32 = streaming_tx.clone();
    let streaming_tx_i16 = streaming_tx.clone();
    let streaming_tx_u16 = streaming_tx.clone();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &_| {
                push_level(data);
                if let Some(tx) = &streaming_tx_f32 {
                    let _ = tx.send(data.to_vec());
                } else if let Ok(mut buf) = samples_cb.lock() {
                    buf.extend_from_slice(data);
                }
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &_| {
                let f: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                push_level(&f);
                if let Some(tx) = &streaming_tx_i16 {
                    let _ = tx.send(f);
                } else if let Ok(mut buf) = samples_cb.lock() {
                    buf.extend_from_slice(&f);
                }
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _: &_| {
                let f: Vec<f32> = data
                    .iter()
                    .map(|&s| (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0))
                    .collect();
                push_level(&f);
                if let Some(tx) = &streaming_tx_u16 {
                    let _ = tx.send(f);
                } else if let Ok(mut buf) = samples_cb.lock() {
                    buf.extend_from_slice(&f);
                }
            },
            err_fn,
            None,
        ),
        _ => return Err("Unsupported sample format".into()),
    }
    .map_err(|e| format!("build_input_stream: {}", e))?;

    stream
        .play()
        .map_err(|e| format!("stream.play: {}", e))?;

    // Duck system output volume. Snapshot the pre-duck level so stop_inner
    // can restore to the exact original — bypasses rounding drift from the
    // old "divide by ratio" math.
    // Duck system output volume ONLY when the input device differs from the
    // output device. When they're the same physical thing (typical BT case,
    // AirPods → AirPods), macOS forces an HFP profile switch and the music
    // already drops on its own — touching the slider then is unreliable
    // (HFP decouples the slider from real playback level, ends up at zero).
    let input_name = device.name().ok();
    let output_name = host.default_output_device().and_then(|d| d.name().ok());
    let pre_duck_volume = match (&input_name, &output_name) {
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
    };

    // While dictation is live, hijack Escape to cancel the session — the user
    // can wave it off without their text leaking out as a paste. The shortcut
    // is unregistered as soon as the session ends (stop or cancel).
    register_escape_cancel(app);

    let session = Session {
        shortcut_id: shortcut.id.clone(),
        samples,
        sample_rate,
        channels,
        stream: Some(stream),
        pre_duck_volume,
        streaming,
    };

    *state
        .inner
        .lock()
        .map_err(|e| format!("state lock: {}", e))? = Some(session);
    // is_active was claimed at the top via compare_exchange — commit the
    // guard so it doesn't clear it on drop. Stays true until stop/cancel.
    active_guard.commit = true;

    // Land the HUD on the monitor the user is actually looking at.
    crate::reposition_dictation_hud(app);

    emit_status(app, "recording", Some(&shortcut.id), None);
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

    let mut session = match session {
        Some(s) => s,
        None => {
            emit_status(app, "idle", None, None);
            return Ok(String::new());
        }
    };

    let shortcut_id = session.shortcut_id.clone();
    unregister_escape_cancel(app);

    // Grace period: let the last ~250ms of audio reach the cpal callback
    // before we drop the stream. Otherwise the tail of the last word is
    // truncated because the OS audio queue hasn't flushed yet.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    drop(session.stream.take());

    if let Some(orig) = session.pre_duck_volume {
        restore_system_volume_to(orig);
    }

    let shortcut = find_shortcut(&shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' vanished mid-session", shortcut_id))?;

    // Streaming path: close the sample channel, wait for the sidecar to
    // flush + emit its final transcript. Bypasses the entire batch
    // normalize/resample/transcribe pipeline.
    if let Some(streaming) = session.streaming.take() {
        emit_status(app, "transcribing", Some(&shortcut_id), Some("Finalizing live transcript".into()));
        let t0 = std::time::Instant::now();
        let final_text = streaming.finish().await?;
        log::info!(
            "dictation: streaming finish {:?} (chars={})",
            t0.elapsed(),
            final_text.len()
        );
        let trimmed = final_text.trim().to_string();
        if trimmed.is_empty() {
            emit_status(app, "idle", Some(&shortcut_id), Some("No speech detected".into()));
            return Ok(String::new());
        }
        return process_and_deliver(app, &shortcut, trimmed).await;
    }

    // Batch fallback: legacy path that buffers all PCM in memory, then
    // converts + transcribes after stop.
    let raw_samples = {
        let s = session
            .samples
            .lock()
            .map_err(|e| format!("samples lock: {}", e))?;
        s.clone()
    };

    if raw_samples.is_empty() {
        emit_status(app, "idle", Some(&shortcut_id), Some("No audio captured".into()));
        return Ok(String::new());
    }

    let duration_secs =
        raw_samples.len() as f64 / (session.sample_rate as f64 * session.channels as f64);
    log::info!(
        "dictation: '{}' captured {:.2}s, transcribing...",
        shortcut.name,
        duration_secs
    );
    emit_status(app, "transcribing", Some(&shortcut_id), None);

    log::info!(
        "dictation: pipeline start — raw_samples={}, channels={}, rate={}",
        raw_samples.len(),
        session.channels,
        session.sample_rate
    );

    let t0 = std::time::Instant::now();
    let normalized = normalize_loudness(&raw_samples, session.channels, session.sample_rate);
    log::info!("dictation: normalize_loudness {:?}", t0.elapsed());

    let t1 = std::time::Instant::now();
    let mono = downmix_to_mono(&normalized, session.channels);
    log::info!("dictation: downmix_to_mono {:?}", t1.elapsed());

    let t2 = std::time::Instant::now();
    let mono_16k = if session.sample_rate == TARGET_RATE {
        mono
    } else {
        resample_mono(&mono, session.sample_rate, TARGET_RATE)?
    };
    log::info!(
        "dictation: resample_mono {:?} (skipped={}, out_samples={})",
        t2.elapsed(),
        session.sample_rate == TARGET_RATE,
        mono_16k.len()
    );

    let t3 = std::time::Instant::now();
    let transcript = transcribe(app, &shortcut, mono_16k).await?;
    log::info!(
        "dictation: transcribe {:?} (engine={:?})",
        t3.elapsed(),
        shortcut.engine
    );
    let trimmed = transcript.trim().to_string();
    if trimmed.is_empty() {
        emit_status(app, "idle", Some(&shortcut_id), Some("No speech detected".into()));
        return Ok(String::new());
    }

    process_and_deliver(app, &shortcut, trimmed).await
}

/// Trigger flow for Clipboard-input shortcuts: snapshot the pasteboard, run
/// the pipeline if configured, and paste the result back. No mic capture.
pub async fn run_clipboard_inner(app: &AppHandle, shortcut_id: &str) -> Result<String, String> {
    let shortcut = find_shortcut(shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' not found", shortcut_id))?;

    crate::reposition_dictation_hud(app);
    emit_status(app, "reading_clipboard", Some(&shortcut.id), None);
    let text = read_clipboard().map_err(|e| {
        emit_status(app, "error", Some(&shortcut.id), Some(format!("Clipboard read failed: {}", e)));
        e
    })?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        emit_status(app, "idle", Some(&shortcut.id), Some("Clipboard is empty".into()));
        return Ok(String::new());
    }

    process_and_deliver(app, &shortcut, trimmed).await
}

/// Shared tail of both Audio and Clipboard flows: run the LLM pipeline (if any)
/// against `text`, then paste / copy the result. Emits status events along the
/// way. Returns the final text that was delivered.
async fn process_and_deliver(
    app: &AppHandle,
    shortcut: &DictationShortcut,
    input_text: String,
) -> Result<String, String> {
    let shortcut_id = shortcut.id.clone();

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

    if shortcut.auto_paste {
        emit_status(app, "pasting", Some(&shortcut_id), None);
        match paste_text(&final_trimmed) {
            Ok(()) => {}
            Err(PasteError::AccessibilityDenied) => {
                log::warn!(
                    "dictation: Accessibility permission missing — text in clipboard only"
                );
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
    log::info!("dictation: '{}' done ({} chars)", shortcut.name, final_trimmed.len());
    Ok(final_trimmed)
}

async fn transcribe(
    app: &AppHandle,
    shortcut: &DictationShortcut,
    mono_16k: Vec<f32>,
) -> Result<String, String> {
    match &shortcut.engine {
        TranscriptionProvider::FluidAudio => run_fluidaudio(app, &mono_16k).await,
        TranscriptionProvider::OpenAI => {
            let settings = load_settings();
            let api_key = get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured")?;
            let tmp = std::env::temp_dir().join(format!(
                "nbp-dict-{}.wav",
                uuid::Uuid::new_v4().simple()
            ));
            write_mono_wav(&tmp, &mono_16k, TARGET_RATE)?;
            let result = crate::cloud_ai::transcribe_with_whisper(&api_key, &tmp).await;
            let _ = std::fs::remove_file(&tmp);
            result
        }
        other => Err(format!("Unsupported dictation engine: {:?}", other)),
    }
}

/// Runs LLM-only steps of `pipeline_name` against `text` in memory.
/// Non-LLM steps (Save/Webhook/Slack/Notion) are skipped silently. The output
/// of the last successful LLM step is returned; if no LLM step ran, the
/// original text is returned unchanged.
async fn run_text_pipeline(text: &str, pipeline_name: &str) -> Result<String, String> {
    let pipelines = load_pipelines().map_err(|e| format!("load pipelines: {}", e))?;
    let pipeline = pipelines
        .get(pipeline_name)
        .ok_or_else(|| format!("Pipeline '{}' not found", pipeline_name))?
        .clone();

    let mut current = text.to_string();
    let mut ran_any_llm = false;

    for step in &pipeline.steps {
        if step.connector != ConnectorType::Llm {
            log::debug!(
                "dictation pipeline '{}': skipping non-LLM step '{}'",
                pipeline_name,
                step.name
            );
            continue;
        }
        let out = crate::connectors::llm::execute_inline(&step.config, &current)
            .await
            .map_err(|e| format!("step '{}': {}", step.name, e))?;
        current = out;
        ran_any_llm = true;
    }

    if !ran_any_llm {
        log::warn!(
            "dictation pipeline '{}' has no LLM steps; returning raw transcript",
            pipeline_name
        );
    }
    Ok(current)
}

// --- Tauri commands -------------------------------------------------------

/// Cancel an in-flight dictation session: stop the mic, drop the samples, hide
/// the HUD, restore volume. No transcription, no paste, no clipboard write.
pub fn cancel_inner(app: &AppHandle) {
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
    let mut session = match session {
        Some(s) => s,
        None => return,
    };
    drop(session.stream.take());
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
                    cancel_inner(&app);
                });
            });
        if let Err(e) = result {
            log::warn!("dictation: failed to register Escape cancel: {}", e);
        }
    }
}

fn unregister_escape_cancel(app: &AppHandle) {
    #[cfg(desktop)]
    {
        use tauri_plugin_global_shortcut::GlobalShortcutExt;
        let _ = app.global_shortcut().unregister("Escape");
    }
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
    interleaved.iter().map(|&s| (s * gain).clamp(-1.0, 1.0)).collect()
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
    use rubato::{FftFixedInOut, Resampler};

    // FFT-based resampler — orders of magnitude faster than the polyphase
    // Sinc path for integer ratios like 48k→16k (gcd=16k, ratio 3:1). Quality
    // is more than adequate for speech-to-text.
    let mut resampler = FftFixedInOut::<f32>::new(
        src_rate as usize,
        dst_rate as usize,
        1024,
        1,
    )
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

fn write_mono_wav(
    path: &std::path::Path,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), String> {
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

async fn run_fluidaudio(app: &AppHandle, samples_16k: &[f32]) -> Result<String, String> {
    use tauri_plugin_shell::ShellExt;

    let tmp = std::env::temp_dir().join(format!(
        "nbp-dict-{}.wav",
        uuid::Uuid::new_v4().simple()
    ));
    write_mono_wav(&tmp, samples_16k, TARGET_RATE)?;

    let (mut rx, _child) = app
        .shell()
        .sidecar("fluidaudio-sidecar")
        .map_err(|e| format!("sidecar create: {}", e))?
        .arg(tmp.to_str().ok_or("invalid tmp path")?)
        .spawn()
        .map_err(|e| format!("sidecar spawn: {}", e))?;

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
                break;
            }
            _ => {}
        }
    }

    let _ = std::fs::remove_file(&tmp);

    if exit_code != Some(0) {
        return Err(format!("FluidAudio sidecar failed: {}", stderr_buf));
    }

    let out: FluidOut = serde_json::from_slice(&stdout_buf)
        .map_err(|e| format!("parse FluidAudio output: {}", e))?;
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
        log::info!(
            "dictation: cpal advertises — channels={}, rate_range={}..={}, format={:?}",
            c.channels(),
            c.min_sample_rate().0,
            c.max_sample_rate().0,
            c.sample_format()
        );
    }

    let rate_ok = |cfg: &cpal::SupportedStreamConfigRange| {
        cfg.min_sample_rate().0 <= PREFERRED_RATE && cfg.max_sample_rate().0 >= PREFERRED_RATE
    };

    if let Some(cfg) = configs.iter().find(|c| c.channels() == 1 && rate_ok(c)) {
        return Ok(cfg.clone().with_sample_rate(cpal::SampleRate(PREFERRED_RATE)));
    }
    if let Some(cfg) = configs.iter().find(|c| rate_ok(c)) {
        return Ok(cfg.clone().with_sample_rate(cpal::SampleRate(PREFERRED_RATE)));
    }
    device
        .default_input_config()
        .map_err(|e| format!("default_input_config: {}", e))
}

fn restore_system_volume_to(target: u32) {
    let now = get_system_output_volume();
    log::info!("dictation: restoring volume — current={:?}, target={}", now, target);
    if let Some(now) = now {
        if target != now {
            fade_system_volume(now, target, 750);
        }
    }
}

fn get_system_output_volume() -> Option<u32> {
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg("output volume of (get volume settings)")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()
}

// Tracks the previously-spawned fade osascript so we can kill it when a new
// fade starts — otherwise an in-flight down-fade keeps clobbering the up-fade.
static ACTIVE_FADE: std::sync::OnceLock<std::sync::Mutex<Option<std::process::Child>>> =
    std::sync::OnceLock::new();

fn active_fade_slot() -> &'static std::sync::Mutex<Option<std::process::Child>> {
    ACTIVE_FADE.get_or_init(|| std::sync::Mutex::new(None))
}

fn fade_system_volume(from: u32, to: u32, duration_ms: u64) {
    if let Ok(mut guard) = active_fade_slot().lock() {
        if let Some(mut prev) = guard.take() {
            let _ = prev.kill();
            let _ = prev.wait();
        }
    }

    // Each AppleScript iteration carries ~20-30ms unavoidable overhead
    // (set volume + delay + Core Audio round-trip). With many steps the
    // wall-clock drifts way past the requested duration. Use a small fixed
    // step count so the perceived fade matches `duration_ms`.
    const STEPS: u64 = 6;
    let step_delay = (duration_ms as f64) / (STEPS as f64) / 1000.0;
    let steps = STEPS;
    let script = format!(
        "set startV to {}\nset endV to {}\nset steps to {}\nrepeat with i from 1 to steps\n  set v to startV + ((endV - startV) * i / steps)\n  set volume output volume v\n  delay {:.4}\nend repeat",
        from, to, steps, step_delay
    );
    if let Ok(child) = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn()
    {
        if let Ok(mut guard) = active_fade_slot().lock() {
            *guard = Some(child);
        }
    }
}

fn read_clipboard() -> Result<String, String> {
    let output = std::process::Command::new("pbpaste")
        .output()
        .map_err(|e| format!("pbpaste spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!("pbpaste exit {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("pbcopy spawn: {}", e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("pbcopy write: {}", e))?;
    }
    child
        .wait()
        .map_err(|e| format!("pbcopy wait: {}", e))?;
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
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
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

fn paste_text(text: &str) -> Result<(), PasteError> {
    copy_to_clipboard(text).map_err(PasteError::Other)?;
    if !is_ax_trusted() {
        return Err(PasteError::AccessibilityDenied);
    }
    // Tiny delay to let the pasteboard settle before the keystroke
    std::thread::sleep(std::time::Duration::from_millis(40));
    post_cmd_v();
    Ok(())
}
