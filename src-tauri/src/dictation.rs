//! Quick Dictate: hotkey-driven ephemeral mic capture → transcribe → optional
//! LLM pipeline → paste. Bypasses the main recording pipeline (no storage, no
//! system audio, no EBU). Supports N independent shortcuts, each with its own
//! engine + optional LLM-only pipeline.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::{
    get_api_key_for_provider, get_models_dir, load_settings, DictationShortcut,
    TranscriptionProvider, WhisperModelSize,
};
use crate::pipelines::{load_pipelines, ConnectorType};

const TARGET_RATE: u32 = 16_000;

pub struct DictationState {
    pub is_active: Arc<AtomicBool>,
    inner: Mutex<Option<Session>>,
}

struct Session {
    shortcut_id: String,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
    stream: Option<cpal::Stream>,
}

// SAFETY: cpal::Stream is !Send on macOS due to PhantomData<*mut ()>. The stream
// is created, kept alive in the Mutex, and dropped on stop — never moved across
// threads. All access serialised by the outer Mutex.
unsafe impl Send for Session {}

impl DictationState {
    pub fn new() -> Self {
        Self {
            is_active: Arc::new(AtomicBool::new(false)),
            inner: Mutex::new(None),
        }
    }
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

pub fn start_inner(app: &AppHandle, shortcut_id: &str) -> Result<(), String> {
    let state = app.state::<DictationState>();
    if state.is_active.load(Ordering::Relaxed) {
        return Err("Dictation already active".into());
    }

    let shortcut = find_shortcut(shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' not found", shortcut_id))?;

    let host = cpal::default_host();
    let device = if let Some(name) = shortcut.device_name.as_deref() {
        crate::devices::get_device_by_name(name).or_else(|| host.default_input_device())
    } else {
        host.default_input_device()
    }
    .ok_or("No input device available")?;

    let config = device
        .default_input_config()
        .map_err(|e| format!("default_input_config: {}", e))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();

    let samples: Arc<Mutex<Vec<f32>>> =
        Arc::new(Mutex::new(Vec::with_capacity((sample_rate as usize) * 30)));
    let samples_cb = samples.clone();

    let err_fn = |err| log::error!("dictation cpal stream error: {}", err);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &_| {
                if let Ok(mut buf) = samples_cb.lock() {
                    buf.extend_from_slice(data);
                }
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &_| {
                if let Ok(mut buf) = samples_cb.lock() {
                    buf.extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                }
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _: &_| {
                if let Ok(mut buf) = samples_cb.lock() {
                    buf.extend(
                        data.iter()
                            .map(|&s| (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0)),
                    );
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

    let session = Session {
        shortcut_id: shortcut.id.clone(),
        samples,
        sample_rate,
        channels,
        stream: Some(stream),
    };

    *state
        .inner
        .lock()
        .map_err(|e| format!("state lock: {}", e))? = Some(session);
    state.is_active.store(true, Ordering::Relaxed);

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

    drop(session.stream.take());
    let shortcut_id = session.shortcut_id.clone();

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

    let shortcut = find_shortcut(&shortcut_id)
        .ok_or_else(|| format!("Shortcut '{}' vanished mid-session", shortcut_id))?;

    let duration_secs =
        raw_samples.len() as f64 / (session.sample_rate as f64 * session.channels as f64);
    log::info!(
        "dictation: '{}' captured {:.2}s, transcribing...",
        shortcut.name,
        duration_secs
    );
    emit_status(app, "transcribing", Some(&shortcut_id), None);

    let mono = downmix_to_mono(&raw_samples, session.channels);
    let mono_16k = if session.sample_rate == TARGET_RATE {
        mono
    } else {
        resample_mono(&mono, session.sample_rate, TARGET_RATE)?
    };

    let transcript = transcribe(app, &shortcut, mono_16k).await?;
    let trimmed = transcript.trim().to_string();
    if trimmed.is_empty() {
        emit_status(app, "idle", Some(&shortcut_id), Some("No speech detected".into()));
        return Ok(String::new());
    }

    let final_text = if let Some(ref pipeline_name) = shortcut.pipeline {
        emit_status(app, "processing", Some(&shortcut_id), None);
        match run_text_pipeline(&trimmed, pipeline_name).await {
            Ok(processed) => processed,
            Err(e) => {
                log::warn!(
                    "dictation: pipeline '{}' failed: {} — falling back to raw transcript",
                    pipeline_name,
                    e
                );
                emit_status(
                    app,
                    "pipeline_error",
                    Some(&shortcut_id),
                    Some(format!("Pipeline failed, pasted raw: {}", e)),
                );
                trimmed.clone()
            }
        }
    } else {
        trimmed.clone()
    };

    let final_trimmed = final_text.trim().to_string();
    if final_trimmed.is_empty() {
        emit_status(app, "idle", Some(&shortcut_id), Some("Empty output".into()));
        return Ok(String::new());
    }

    if shortcut.auto_paste {
        emit_status(app, "pasting", Some(&shortcut_id), None);
        if let Err(e) = paste_text(&final_trimmed) {
            log::warn!("dictation paste failed: {}", e);
            emit_status(app, "error", Some(&shortcut_id), Some(format!("Paste failed: {}", e)));
            return Err(format!("Paste failed: {}", e));
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
        TranscriptionProvider::LocalWhisper => {
            let model_size = shortcut
                .whisper_model
                .clone()
                .unwrap_or(WhisperModelSize::Base);
            let url = crate::transcription::get_model_url(&model_size);
            let filename = url.split('/').last().unwrap_or("ggml-base.bin");
            let model_path = get_models_dir().join(filename);
            if !model_path.exists() {
                return Err(format!("Whisper model not downloaded: {:?}", model_size));
            }
            tokio::task::spawn_blocking(move || run_local_whisper(&model_path, &mono_16k))
                .await
                .map_err(|e| format!("join: {}", e))?
        }
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

#[tauri::command]
pub fn dictation_start(app: AppHandle, shortcut_id: String) -> Result<(), String> {
    start_inner(&app, &shortcut_id)
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
        start_inner(&app, &shortcut_id)?;
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
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };

    let chunk_size = 1024usize;
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = dst_rate as f64 / src_rate as f64;
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 1)
        .map_err(|e| format!("resampler init: {}", e))?;

    let mut output = Vec::with_capacity(((input.len() as f64) * ratio) as usize + chunk_size);
    let mut pos = 0;
    while pos + chunk_size <= input.len() {
        let chunk = vec![&input[pos..pos + chunk_size]];
        let res = resampler
            .process(&chunk, None)
            .map_err(|e| format!("resample: {}", e))?;
        output.extend(&res[0]);
        pos += chunk_size;
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

fn run_local_whisper(
    model_path: &std::path::Path,
    samples_16k: &[f32],
) -> Result<String, String> {
    use whisper_rs::{FullParams, SamplingStrategy};

    let ctx = crate::transcription::load_whisper_context(model_path)?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(None);
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    let mut state = ctx
        .create_state()
        .map_err(|e| format!("whisper state: {}", e))?;
    state
        .full(params, samples_16k)
        .map_err(|e| format!("whisper full: {}", e))?;
    let mut text = String::new();
    let n = state.full_n_segments();
    for i in 0..n {
        if let Some(seg) = state.get_segment(i) {
            if let Ok(s) = seg.to_str_lossy() {
                text.push_str(&s);
                text.push(' ');
            }
        }
    }
    Ok(text.trim().to_string())
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

fn paste_text(text: &str) -> Result<(), String> {
    copy_to_clipboard(text)?;
    std::thread::sleep(std::time::Duration::from_millis(80));
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "System Events" to keystroke "v" using command down"#)
        .status()
        .map_err(|e| format!("osascript spawn: {}", e))?;
    if !status.success() {
        return Err(format!("osascript exit {}", status));
    }
    Ok(())
}
