use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;

use crate::audio_processing::TRANSCRIPTION_BUFFER;
use crate::storage::get_data_dir;

const EVENT_TRANSCRIPT_UPDATED: &str = "realtime_transcript_updated";
const EVENT_TRANSCRIPTION_ERROR: &str = "realtime_transcription_error";

#[derive(Clone, Serialize)]
pub struct TranscriptUpdated {
    pub recording_id: String,
    pub text: String,
}

const LOCAL_WINDOW_SAMPLES: usize = 16000 * 5;
const LOCAL_STEP_SAMPLES: usize = 16000;
const VAD_RMS_THRESHOLD: f32 = 0.005;

pub struct LocalTranscriber {
    should_stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl LocalTranscriber {
    pub fn start(
        app_handle: tauri::AppHandle,
        model_path: PathBuf,
        recording_id: String,
    ) -> Result<Self, String> {
        if !model_path.exists() {
            return Err(format!("Whisper model not found: {}", model_path.display()));
        }

        let should_stop = Arc::new(AtomicBool::new(false));
        let stop_flag = should_stop.clone();

        let handle = std::thread::spawn(move || {
            if let Err(e) =
                run_local_transcription(app_handle.clone(), &model_path, &recording_id, stop_flag)
            {
                eprintln!("Local transcription error: {}", e);
                let _ = app_handle.emit(EVENT_TRANSCRIPTION_ERROR, e);
            }
        });

        Ok(Self {
            should_stop,
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        self.should_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LocalTranscriber {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_local_transcription(
    app_handle: tauri::AppHandle,
    model_path: &std::path::Path,
    recording_id: &str,
    should_stop: Arc<AtomicBool>,
) -> Result<(), String> {
    use whisper_rs::{FullParams, SamplingStrategy};

    log::info!("[rt-whisper] loading model: {}", model_path.display());
    let ctx = crate::transcription::load_whisper_context(model_path)?;

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create Whisper state: {}", e))?;

    log::info!("[rt-whisper] model loaded, waiting for audio...");

    let mut window: Vec<f32> = Vec::with_capacity(LOCAL_WINDOW_SAMPLES);
    let mut committed_text = String::new();
    let mut last_text = String::new();
    let mut last_write_len: usize = 0;
    let mut inference_count: u32 = 0;

    let recording_dir = get_data_dir().join(recording_id);
    let step_interval = std::time::Duration::from_secs(2);

    // Wait for initial audio data
    while !should_stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let available = TRANSCRIPTION_BUFFER.available();
        if available > 0 {
            window.extend_from_slice(&TRANSCRIPTION_BUFFER.pop(available));
        }
        if window.len() >= LOCAL_STEP_SAMPLES {
            log::info!("[rt-whisper] got {} samples, starting transcription loop", window.len());
            break;
        }
    }

    while !should_stop.load(Ordering::Relaxed) {
        let step_start = std::time::Instant::now();

        // Drain buffer
        let available = TRANSCRIPTION_BUFFER.available();
        if available > 0 {
            window.extend_from_slice(&TRANSCRIPTION_BUFFER.pop(available));
        }

        // Keep sliding window bounded
        if window.len() > LOCAL_WINDOW_SAMPLES {
            let excess = window.len() - LOCAL_WINDOW_SAMPLES;
            window.drain(..excess);
        }

        // Simple VAD on the latest chunk
        let vad_start = window.len().saturating_sub(LOCAL_STEP_SAMPLES);
        let is_speaking = compute_rms(&window[vad_start..]) > VAD_RMS_THRESHOLD;

        if !is_speaking {
            // Commit any pending text when silence detected
            if !last_text.is_empty() {
                if !committed_text.is_empty() {
                    committed_text.push(' ');
                }
                committed_text.push_str(&last_text);
                last_text.clear();
            }
            sleep_remaining(step_start, step_interval, &should_stop);
            continue;
        }

        // Optimized params for real-time
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_single_segment(true);
        params.set_no_context(true);
        params.set_n_threads(4);

        let infer_start = std::time::Instant::now();
        if let Err(e) = state.full(params, &window) {
            log::error!("[rt-whisper] inference error: {}", e);
            sleep_remaining(step_start, step_interval, &should_stop);
            continue;
        }
        inference_count += 1;
        log::debug!("[rt-whisper] inference #{} took {:?}", inference_count, infer_start.elapsed());

        // Extract text from segments
        let mut text = String::new();
        let n_segments = state.full_n_segments();
        for i in 0..n_segments {
            if let Some(seg) = state.get_segment(i) {
                if let Ok(seg_text) = seg.to_str_lossy() {
                    text.push_str(&seg_text);
                }
            }
        }
        let text = text.trim().to_string();

        if !text.is_empty() && text != last_text {
            last_text = text;
            let combined = if committed_text.is_empty() {
                last_text.clone()
            } else {
                format!("{} {}", committed_text, last_text)
            };
            if combined.len() > last_write_len {
                if let Err(e) = write_transcript_json(&recording_dir, &combined, "whisper-realtime") {
                    log::error!("[rt-whisper] failed to write transcript: {}", e);
                } else {
                    last_write_len = combined.len();
                    let _ = app_handle.emit(
                        EVENT_TRANSCRIPT_UPDATED,
                        TranscriptUpdated {
                            recording_id: recording_id.to_string(),
                            text: combined,
                        },
                    );
                }
            }
        }

        sleep_remaining(step_start, step_interval, &should_stop);
    }

    // Flush remaining text
    if !last_text.is_empty() {
        if !committed_text.is_empty() {
            committed_text.push(' ');
        }
        committed_text.push_str(&last_text);
    }

    if !committed_text.is_empty() {
        if let Err(e) = write_transcript_json(&recording_dir, &committed_text, "whisper-realtime") {
            log::error!("[rt-whisper] failed to write final transcript: {}", e);
        } else {
            let _ = app_handle.emit(
                EVENT_TRANSCRIPT_UPDATED,
                TranscriptUpdated {
                    recording_id: recording_id.to_string(),
                    text: committed_text,
                },
            );
        }
    }

    log::info!("[rt-whisper] stopped after {} inferences", inference_count);
    Ok(())
}

fn write_transcript_json(
    recording_dir: &std::path::Path,
    text: &str,
    model: &str,
) -> Result<(), String> {
    use crate::transcript_migration::TranscriptSource;

    let transcript = crate::transcription::TranscriptJson::new(
        TranscriptSource::Local,
        model.to_string(),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        read_metadata_from_dir(recording_dir)
            .and_then(|m| m.audio.mix.map(|a| a.duration_sec))
            .unwrap_or(0.0),
        Some("auto".to_string()),
        Some(text.to_string()),
    );

    let json_str = serde_json::to_string_pretty(&transcript)
        .map_err(|e| format!("Failed to serialize transcript JSON: {}", e))?;
    let json_path = recording_dir.join("transcript.json");
    let temp_path = json_path.with_extension("json.tmp");
    std::fs::write(&temp_path, &json_str)
        .map_err(|e| format!("Failed to write transcript: {}", e))?;
    std::fs::rename(&temp_path, &json_path)
        .map_err(|e| format!("Failed to finalize transcript: {}", e))?;

    Ok(())
}

fn read_metadata_from_dir(dir: &std::path::Path) -> Option<crate::storage::RecordingMetadata> {
    let path = dir.join("metadata.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

fn sleep_remaining(
    start: std::time::Instant,
    interval: std::time::Duration,
    should_stop: &AtomicBool,
) {
    let elapsed = start.elapsed();
    if elapsed >= interval {
        return;
    }
    let remaining = interval - elapsed;
    let check = std::time::Duration::from_millis(100);
    let mut slept = std::time::Duration::ZERO;
    while slept < remaining && !should_stop.load(Ordering::Relaxed) {
        let chunk = check.min(remaining - slept);
        std::thread::sleep(chunk);
        slept += chunk;
    }
}

