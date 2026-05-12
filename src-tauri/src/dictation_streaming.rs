//! Streaming bridge between the cpal mic input and `fluidaudio-sidecar --stream`.
//!
//! Spawns the sidecar on `start`, pipes downmixed/resampled 16 kHz mono f32
//! PCM into its stdin while the user is talking, reads NDJSON events back
//! (`ready`/`partial`/`final`/`error`) from stdout. On `finish` the sample
//! channel is closed which drops the child → EOF on stdin → sidecar flushes
//! and exits with the consolidated transcript.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use rubato::Resampler;

const TARGET_RATE: u32 = 16_000;
const READY_TIMEOUT_SECS: u64 = 60;

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum StreamEvent {
    Ready,
    Partial { text: String },
    Final { text: String },
    Error { message: String },
}

#[derive(Clone, serde::Serialize)]
struct DictationPartial {
    shortcut_id: String,
    text: String,
}

pub struct StreamingSession {
    /// Public so the cpal callback can clone the sender. cpal::Stream is `!Send`
    /// on macOS so the streaming object itself can't be moved into the callback.
    pub samples_tx: tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
    final_text: Arc<Mutex<Option<String>>>,
    error_text: Arc<Mutex<Option<String>>>,
    reader_task: tauri::async_runtime::JoinHandle<()>,
    writer_task: tauri::async_runtime::JoinHandle<()>,
    cancelled: Arc<AtomicBool>,
}

impl StreamingSession {
    pub async fn start(
        app: &AppHandle,
        shortcut_id: String,
        source_rate: u32,
        source_channels: u16,
    ) -> Result<Self, String> {
        let (mut rx, child) = app
            .shell()
            .sidecar("fluidaudio-sidecar")
            .map_err(|e| format!("streaming sidecar create: {}", e))?
            .arg("--stream")
            .spawn()
            .map_err(|e| format!("streaming sidecar spawn: {}", e))?;

        let final_text: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_text: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        // Stdout reader — splits the NDJSON stream into lines, dispatches each
        // event. Lives until the sidecar terminates.
        let final_text_r = final_text.clone();
        let error_text_r = error_text.clone();
        let app_r = app.clone();
        let shortcut_id_r = shortcut_id.clone();
        let reader_task = tauri::async_runtime::spawn(async move {
            let mut ready_signal: Option<tokio::sync::oneshot::Sender<()>> = Some(ready_tx);
            let mut line_buf = String::new();
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(data) => {
                        line_buf.push_str(&String::from_utf8_lossy(&data));
                        while let Some(nl) = line_buf.find('\n') {
                            let line: String = line_buf.drain(..=nl).collect();
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<StreamEvent>(line) {
                                Ok(StreamEvent::Ready) => {
                                    if let Some(tx) = ready_signal.take() {
                                        let _ = tx.send(());
                                    }
                                }
                                Ok(StreamEvent::Partial { text }) => {
                                    let _ = app_r.emit(
                                        "dictation_partial",
                                        DictationPartial {
                                            shortcut_id: shortcut_id_r.clone(),
                                            text,
                                        },
                                    );
                                }
                                Ok(StreamEvent::Final { text }) => {
                                    *final_text_r.lock().unwrap() = Some(text);
                                }
                                Ok(StreamEvent::Error { message }) => {
                                    log::warn!("dictation streaming sidecar error: {}", message);
                                    *error_text_r.lock().unwrap() = Some(message);
                                }
                                Err(e) => {
                                    log::warn!(
                                        "dictation streaming: failed to parse event line ({}): {}",
                                        e,
                                        line
                                    );
                                }
                            }
                        }
                    }
                    CommandEvent::Stderr(data) => {
                        // sidecar uses stderr only for PROGRESS:* lines — log at debug
                        log::debug!(
                            "dictation streaming sidecar stderr: {}",
                            String::from_utf8_lossy(&data).trim()
                        );
                    }
                    CommandEvent::Terminated(_) => break,
                    _ => {}
                }
            }
        });

        // Block start_inner until the sidecar finishes loading models, but
        // don't hang forever if it dies first.
        match tokio::time::timeout(Duration::from_secs(READY_TIMEOUT_SECS), ready_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                // ready_tx dropped before signaling → sidecar terminated early
                let err = error_text.lock().unwrap().clone();
                return Err(err.unwrap_or_else(|| "streaming sidecar exited before ready".into()));
            }
            Err(_) => {
                return Err(format!(
                    "streaming sidecar didn't become ready within {}s",
                    READY_TIMEOUT_SECS
                ));
            }
        }

        // Writer task — owns the child handle. When the sample channel closes
        // (StreamingSession dropped or finish() called), the loop exits, the
        // child is dropped, stdin closes, sidecar EOFs and flushes its final.
        let (samples_tx, mut samples_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let writer_task = tauri::async_runtime::spawn(async move {
            let mut child = child;
            let mut accumulator: Vec<f32> = Vec::with_capacity(4096);
            let mut resampler = match build_resampler(source_rate) {
                Ok(r) => r,
                Err(e) => {
                    log::error!("dictation streaming: resampler init failed: {}", e);
                    return;
                }
            };
            let chunk_in = resampler.0;
            let needs_resample = source_rate != TARGET_RATE;

            while let Some(samples) = samples_rx.recv().await {
                let mono = downmix_to_mono(&samples, source_channels);
                if !needs_resample {
                    if let Err(e) = write_f32_samples(&mut child, &mono) {
                        log::warn!("dictation streaming: stdin write failed: {}", e);
                        break;
                    }
                    continue;
                }
                accumulator.extend_from_slice(&mono);
                while accumulator.len() >= chunk_in {
                    let chunk: Vec<f32> = accumulator.drain(..chunk_in).collect();
                    let out = match resampler.1.process(&[&chunk[..]], None) {
                        Ok(o) => o,
                        Err(e) => {
                            log::warn!("dictation streaming: resample failed: {}", e);
                            break;
                        }
                    };
                    if let Err(e) = write_f32_samples(&mut child, &out[0]) {
                        log::warn!("dictation streaming: stdin write failed: {}", e);
                        break;
                    }
                }
            }
            // Drop the child handle to close stdin → sidecar sees EOF and emits
            // the consolidated `final` event before terminating.
            drop(child);
        });

        Ok(Self {
            samples_tx,
            final_text,
            error_text,
            reader_task,
            writer_task,
            cancelled,
        })
    }

    /// Close the sample channel and wait for the sidecar to flush + emit
    /// final. Returns the consolidated transcript text.
    pub async fn finish(self) -> Result<String, String> {
        // Close the sender so the writer drains, then everything cascades.
        drop(self.samples_tx);
        let _ = self.writer_task.await;
        let _ = self.reader_task.await;

        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(String::new());
        }
        if let Some(err) = self.error_text.lock().unwrap().clone() {
            return Err(err);
        }
        Ok(self
            .final_text
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default())
    }

    /// Abort streaming — used when the user hits Escape to cancel.
    pub fn cancel(self) {
        self.cancelled.store(true, Ordering::Relaxed);
        drop(self.samples_tx);
        let _ = tauri::async_runtime::block_on(self.writer_task);
        let _ = tauri::async_runtime::block_on(self.reader_task);
    }
}

fn build_resampler(
    source_rate: u32,
) -> Result<(usize, rubato::FftFixedInOut<f32>), String> {
    use rubato::{FftFixedInOut, Resampler};
    let resampler = FftFixedInOut::<f32>::new(
        source_rate as usize,
        TARGET_RATE as usize,
        1024,
        1,
    )
    .map_err(|e| format!("resampler init: {}", e))?;
    let chunk_in = resampler.input_frames_next();
    Ok((chunk_in, resampler))
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

fn write_f32_samples(
    child: &mut tauri_plugin_shell::process::CommandChild,
    samples: &[f32],
) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }
    let mut buf = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    child.write(&buf).map_err(|e| e.to_string())
}

