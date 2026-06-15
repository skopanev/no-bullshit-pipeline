//! Streaming bridge between the cpal mic input and `fluidaudio-sidecar --stream`.
//!
//! Currently dormant — `dictation::start_inner` keeps streaming off because
//! the Parakeet EOU model is English-biased. Batch Parakeet TDT v3 handles
//! multilingual speech far better. Kept here so we can flip streaming back
//! on once a comparable multilingual model lands.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::CommandEvent;

const TARGET_RATE: u32 = 16_000;

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
    /// `Option` so `finish`/`cancel` can take it out to close the channel while
    /// still satisfying the `Drop` impl (a type with `Drop` can't be
    /// partially moved). Always `Some` for a live session.
    pub samples_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<f32>>>,
    /// Source sample rate the internal resampler was built for. Used by the
    /// warm-pool consumer to verify compatibility with the chosen cpal device.
    pub source_rate: u32,
    /// Source channel count (raw cpal interleaved before downmix). Same role
    /// as `source_rate` for the warm-pool match check.
    pub source_channels: u16,
    final_text: Arc<Mutex<Option<String>>>,
    error_text: Arc<Mutex<Option<String>>>,
    /// `Option` for the same take-on-consume reason as `samples_tx`. `Drop`
    /// aborts whatever's left so a dropped (not explicitly finished/cancelled)
    /// session kills its sidecar instead of letting it transcribe to the end.
    reader_task: Option<tauri::async_runtime::JoinHandle<()>>,
    writer_task: Option<tauri::async_runtime::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
}

/// Kills the streaming sidecar if the writer task is dropped while still armed
/// (i.e. the task was aborted mid-stream). On a normal loop exit the writer
/// disarms this first and drops the child gracefully, so the sidecar gets EOF
/// on stdin and flushes its final transcript. Dropping a `CommandChild` alone
/// does NOT terminate the process — only `kill()` does.
struct StreamSidecarKiller(Option<tauri_plugin_shell::process::CommandChild>);

impl Drop for StreamSidecarKiller {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            let _ = child.kill();
        }
    }
}

impl Drop for StreamingSession {
    fn drop(&mut self) {
        // If we still own the task handles, the session was dropped WITHOUT a
        // graceful `finish()`/`cancel()` (e.g. the parent pipeline task was
        // aborted by Esc). A dropped `JoinHandle` only detaches — the tasks
        // would keep running and the sidecar would transcribe to completion,
        // model resident, burning CPU. Abort them so the writer's
        // `StreamSidecarKiller` fires and kills the child promptly.
        if let Some(h) = self.writer_task.take() {
            h.abort();
        }
        if let Some(h) = self.reader_task.take() {
            h.abort();
        }
    }
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
                                    // Tolerate lock poisoning: degrade gracefully
                                    // instead of cascading another thread's panic.
                                    *final_text_r.lock().unwrap_or_else(|e| e.into_inner()) =
                                        Some(text);
                                }
                                Ok(StreamEvent::Error { message }) => {
                                    log::warn!("dictation streaming sidecar error: {}", message);
                                    *error_text_r.lock().unwrap_or_else(|e| e.into_inner()) =
                                        Some(message);
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

        // Don't await the sidecar's `ready` event before returning — we want
        // start_inner to feel instant from the user's perspective. Samples
        // pile up in the OS stdin pipe buffer (~64KB on macOS = ~1s of 16k
        // f32 mono) while the model loads. Once the sidecar starts reading
        // stdin, the backlog drains and live transcription catches up. We
        // deliberately drop the ready receiver here: nothing gates on it.
        drop(ready_rx);

        // Writer task — owns the child handle. When the sample channel closes
        // (StreamingSession dropped or finish() called), the loop exits, the
        // child is dropped, stdin closes, sidecar EOFs and flushes its final.
        let (samples_tx, mut samples_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let writer_task = tauri::async_runtime::spawn(async move {
            // Armed killer: if this task is aborted (session dropped on Esc),
            // the killer drops and SIGKILLs the sidecar. On a normal loop exit
            // we disarm it below so the child drops gracefully (stdin EOF →
            // sidecar flushes its final transcript).
            let mut killer = StreamSidecarKiller(Some(child));
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
                let child = killer.0.as_mut().expect("sidecar child present");
                if !needs_resample {
                    if let Err(e) = write_f32_samples(child, &mono) {
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
                    if let Err(e) = write_f32_samples(child, &out[0]) {
                        log::warn!("dictation streaming: stdin write failed: {}", e);
                        break;
                    }
                }
            }
            // Normal exit: disarm the killer and drop the child gracefully so
            // stdin closes → the sidecar sees EOF and emits the consolidated
            // `final` event before terminating (vs. an abort, which SIGKILLs).
            drop(killer.0.take());
        });

        Ok(Self {
            samples_tx: Some(samples_tx),
            source_rate,
            source_channels,
            final_text,
            error_text,
            reader_task: Some(reader_task),
            writer_task: Some(writer_task),
            cancelled,
        })
    }

    /// Close the sample channel and wait for the sidecar to flush + emit
    /// final. Returns the consolidated transcript text.
    ///
    /// Awaits the task handles *in place* (`as_mut`, not `take`) so that if THIS
    /// future is dropped mid-await — e.g. the parent pipeline task is aborted by
    /// Esc while we're waiting on the writer — `self` drops and
    /// `Drop for StreamingSession` aborts the still-`Some` handles, firing the
    /// writer's `StreamSidecarKiller` to SIGKILL the sidecar. On the normal path
    /// we disarm (set the handles to `None`) before returning so `Drop` is a
    /// no-op and the graceful EOF→final flush is preserved.
    ///
    /// FIXME(streaming-reenable): residual gap in the SECOND await below. Once
    /// the writer has finished gracefully it has already disarmed its
    /// `StreamSidecarKiller` and dropped the child (stdin EOF) — so if THIS
    /// future is aborted while awaiting the *reader*, `Drop` aborts the reader
    /// but nobody holds the child to `kill()` it; the sidecar runs its final
    /// pass (~1–30s) before exiting on its own. Bounded, not a forever-leak, and
    /// no stale paste (the caller's generation check guards that) — but wasteful.
    /// Proper fix before re-enabling streaming: keep the child in an
    /// `Arc<Mutex<Option<CommandChild>>>` shared with `StreamingSession` so
    /// `Drop` can `kill()` it unconditionally while the reader is still `Some`.
    pub async fn finish(mut self) -> Result<String, String> {
        // Close the sender so the writer drains, then everything cascades.
        drop(self.samples_tx.take());
        if let Some(h) = self.writer_task.as_mut() {
            join_in_place(h).await;
        }
        if let Some(h) = self.reader_task.as_mut() {
            join_in_place(h).await;
        }
        // Both finished gracefully — disarm so the trailing `Drop` doesn't abort
        // already-completed tasks.
        self.writer_task = None;
        self.reader_task = None;

        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(String::new());
        }
        // Tolerate lock poisoning here too (see the reader task above).
        if let Some(err) = self
            .error_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Err(err);
        }
        Ok(self
            .final_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default())
    }

    /// Abort streaming — used when the user hits Escape to cancel. Aborts both
    /// tasks (the writer's `StreamSidecarKiller` then SIGKILLs the sidecar)
    /// rather than `block_on`-waiting for a graceful drain: `cancel_inner` runs
    /// inside the async Esc handler, and `block_on` from within a Tokio runtime
    /// panics. Abort is non-blocking, kills immediately, and needs no await.
    pub fn cancel(mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        drop(self.samples_tx.take());
        if let Some(h) = self.writer_task.take() {
            h.abort();
        }
        if let Some(h) = self.reader_task.take() {
            h.abort();
        }
    }
}

/// Await a `JoinHandle` through a mutable borrow so ownership stays in the
/// caller's struct. If the caller's future is dropped mid-await, the handle is
/// NOT moved here — it stays in the struct and its owner's `Drop` decides
/// whether to abort it. (Awaiting a `JoinHandle` by value would detach, not
/// abort, on drop — the bug this avoids.)
async fn join_in_place(handle: &mut tauri::async_runtime::JoinHandle<()>) {
    use std::future::Future;
    let _ = std::future::poll_fn(|cx| std::pin::Pin::new(&mut *handle).poll(cx)).await;
}

fn build_resampler(
    source_rate: u32,
) -> Result<(usize, crate::resampler_compat::FftFixedInOut<f32>), String> {
    use crate::resampler_compat::FftFixedInOut;
    let resampler = FftFixedInOut::<f32>::new(source_rate as usize, TARGET_RATE as usize, 1024, 1)
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
