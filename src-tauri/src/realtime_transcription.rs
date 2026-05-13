//! Realtime transcription.
//!
//! whisper-rs was removed; AppleSpeech (this module) plugs the live transcript
//! pane back in by streaming the mixer's 16 kHz mono tap (`TRANSCRIPTION_BUFFER`)
//! through `apple-speech-sidecar --stream`. A FluidAudio variant is planned in
//! a follow-up ticket and will sit alongside `AppleSpeechRealtimeTranscriber`
//! behind the same `RealtimeTranscriberHandle` trait.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::audio_processing::TRANSCRIPTION_BUFFER;

/// Common stop() contract so `AudioState` can hold any realtime provider
/// behind a single field. Provider-specific structs implement this and are
/// boxed into the slot at start time.
pub trait RealtimeTranscriberHandle: Send {
    fn stop(&mut self);
}

/// Inert handle preserved so older call-sites still resolve. The whisper-rs
/// backend it used to wrap was deleted; constructing this returns an error
/// today and the variant is no longer reachable from the dispatcher.
pub struct LocalTranscriber;

impl LocalTranscriber {
    pub fn start(
        _app_handle: AppHandle,
        _model_path: PathBuf,
        _recording_id: String,
    ) -> Result<Self, String> {
        Err("Local whisper realtime is disabled — pick AppleSpeech or FluidAudio".into())
    }

    pub fn stop(&mut self) {}
}

impl Drop for LocalTranscriber {
    fn drop(&mut self) {}
}

impl RealtimeTranscriberHandle for LocalTranscriber {
    fn stop(&mut self) {
        LocalTranscriber::stop(self);
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum StreamEvent {
    Ready,
    Partial {
        text: String,
        #[serde(default)]
        #[allow(dead_code)]
        t: Option<f64>,
    },
    Final {
        text: String,
        #[serde(default)]
        #[allow(dead_code)]
        start: Option<f64>,
        #[serde(default)]
        #[allow(dead_code)]
        end: Option<f64>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, serde::Serialize)]
struct RealtimeTranscriptPayload {
    text: String,
}

/// Live transcript driven by Apple's SpeechAnalyzer. Spawns
/// `apple-speech-sidecar --stream`, pipes the mixer's 16 kHz mono tap into
/// stdin, and forwards stdout NDJSON to `realtime_transcript_updated` /
/// `realtime_transcription_error`.
pub struct AppleSpeechRealtimeTranscriber {
    stop_flag: Arc<AtomicBool>,
    reader_task: Option<tauri::async_runtime::JoinHandle<()>>,
    writer_task: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl AppleSpeechRealtimeTranscriber {
    pub fn start(app_handle: AppHandle, _recording_id: String) -> Result<Self, String> {
        let (mut rx, child) = app_handle
            .shell()
            .sidecar("apple-speech-sidecar")
            .map_err(|e| format!("apple-speech sidecar create: {}", e))?
            .arg("--stream")
            .spawn()
            .map_err(|e| format!("apple-speech sidecar spawn: {}", e))?;

        let stop_flag = Arc::new(AtomicBool::new(false));

        // Accumulates finalized phrases across the session. Each new partial
        // is concatenated with this prefix before being emitted, so the UI
        // sees a single growing transcript rather than per-phrase fragments.
        let accumulated: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

        let app_r = app_handle.clone();
        let acc_r = accumulated.clone();
        let reader_task = tauri::async_runtime::spawn(async move {
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
                                    log::info!("apple-speech realtime: ready");
                                }
                                Ok(StreamEvent::Partial { text, .. }) => {
                                    let combined = {
                                        let acc = acc_r.lock().unwrap_or_else(|e| e.into_inner());
                                        if acc.is_empty() {
                                            text
                                        } else {
                                            format!("{} {}", *acc, text)
                                        }
                                    };
                                    let _ = app_r.emit(
                                        "realtime_transcript_updated",
                                        RealtimeTranscriptPayload { text: combined },
                                    );
                                }
                                Ok(StreamEvent::Final { text, .. }) => {
                                    let combined = {
                                        let mut acc = acc_r.lock().unwrap_or_else(|e| e.into_inner());
                                        if !text.is_empty() {
                                            if !acc.is_empty() {
                                                acc.push(' ');
                                            }
                                            acc.push_str(&text);
                                        }
                                        acc.clone()
                                    };
                                    let _ = app_r.emit(
                                        "realtime_transcript_updated",
                                        RealtimeTranscriptPayload { text: combined },
                                    );
                                }
                                Ok(StreamEvent::Error { message }) => {
                                    log::warn!("apple-speech realtime sidecar error: {}", message);
                                    let _ = app_r.emit("realtime_transcription_error", message);
                                }
                                Err(e) => {
                                    log::warn!(
                                        "apple-speech realtime: failed to parse event line ({}): {}",
                                        e,
                                        line
                                    );
                                }
                            }
                        }
                    }
                    CommandEvent::Stderr(data) => {
                        log::debug!(
                            "apple-speech sidecar stderr: {}",
                            String::from_utf8_lossy(&data).trim()
                        );
                    }
                    CommandEvent::Terminated(_) => break,
                    _ => {}
                }
            }
        });

        let stop_w = stop_flag.clone();
        let writer_task = tauri::async_runtime::spawn(async move {
            // Owning the child here means dropping it on exit closes stdin →
            // the sidecar sees EOF, emits its final, and exits cleanly.
            let mut child = child;
            // Clear any samples buffered before the writer task was scheduled
            // so the live transcript starts from "now" rather than replaying
            // however many ms the mixer already pushed.
            TRANSCRIPTION_BUFFER.clear();

            loop {
                if stop_w.load(Ordering::Relaxed) {
                    break;
                }
                let avail = TRANSCRIPTION_BUFFER.available();
                if avail == 0 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                let samples = TRANSCRIPTION_BUFFER.pop(avail);
                if samples.is_empty() {
                    continue;
                }
                let mut bytes = Vec::with_capacity(samples.len() * 4);
                for s in &samples {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if let Err(e) = child.write(&bytes) {
                    log::warn!("apple-speech realtime: stdin write failed: {}", e);
                    break;
                }
            }
            drop(child);
        });

        Ok(Self {
            stop_flag,
            reader_task: Some(reader_task),
            writer_task: Some(writer_task),
        })
    }

    fn stop_inner(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.writer_task.take() {
            let _ = tauri::async_runtime::block_on(h);
        }
        if let Some(h) = self.reader_task.take() {
            let _ = tauri::async_runtime::block_on(h);
        }
    }
}

impl RealtimeTranscriberHandle for AppleSpeechRealtimeTranscriber {
    fn stop(&mut self) {
        self.stop_inner();
    }
}

impl Drop for AppleSpeechRealtimeTranscriber {
    fn drop(&mut self) {
        // Never block on tokio handles from Drop — Drop can fire inside an
        // async context (e.g. app shutdown) where block_on would panic.
        // Setting the flag is enough: the writer task picks it up, drops the
        // child to close stdin, and the reader exits on Terminated.
        self.stop_flag.store(true, Ordering::Relaxed);
        self.writer_task.take();
        self.reader_task.take();
    }
}
