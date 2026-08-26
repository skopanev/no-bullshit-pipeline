mod cancellation;
mod protocol;
#[cfg(test)]
mod protocol_tests;
mod request;
mod supervisor;
pub(crate) mod warmup;

pub(crate) use warmup::{install_timer, request as request_warmup};

use crate::config::{AppSettings, TranscriptionProvider};
use protocol::{ResidentRequest, WorkerCommand};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

pub(crate) const TRANSCRIPTION_TIMEOUT_ERROR: &str = "resident Parakeet transcription timed out";

pub struct ParakeetWorkerState {
    runtime: Mutex<RuntimeSlot>,
}

enum RuntimeSlot {
    Stopped,
    Running(WorkerRuntime),
    Stopping(StopCompletion),
    ShuttingDown(Option<StopCompletion>),
}

type StopCompletion = watch::Receiver<bool>;

struct WorkerRuntime {
    tx: mpsc::UnboundedSender<WorkerCommand>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl ParakeetWorkerState {
    pub fn new() -> Self {
        Self {
            runtime: Mutex::new(RuntimeSlot::Stopped),
        }
    }
}

pub fn configured(settings: &AppSettings) -> bool {
    settings.dictation.enabled
        && settings.transcription.provider == TranscriptionProvider::FluidAudio
        && settings.transcription.keep_model_ready
}

pub fn reconcile(app: &AppHandle) {
    let state = app.state::<ParakeetWorkerState>();
    let mut slot = state
        .runtime
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let enabled = configured(&crate::config::load_settings());
    match (&*slot, enabled) {
        (RuntimeSlot::Stopped, true) => {
            *slot = RuntimeSlot::Running(start_runtime(app));
        }
        (RuntimeSlot::Running(_), false) => {
            let runtime = match std::mem::replace(&mut *slot, RuntimeSlot::Stopped) {
                RuntimeSlot::Running(runtime) => runtime,
                _ => unreachable!(),
            };
            *slot = RuntimeSlot::Stopping(spawn_stop(app.clone(), runtime));
        }
        _ => {}
    }
}

pub async fn transcribe(
    app: &AppHandle,
    samples_16k: &[f32],
    settings: &AppSettings,
) -> Result<String, String> {
    request::transcribe_user(app, samples_16k, settings).await
}

pub async fn verify_after_wake(app: &AppHandle) {
    let Some(tx) = running_tx(app) else {
        reconcile(app);
        let _ = warmup::run(app, "wake").await;
        return;
    };
    let (reply, receiver) = oneshot::channel();
    let request = ResidentRequest::ping(Uuid::new_v4().to_string());
    if tx.send(WorkerCommand::Health { request, reply }).is_err() {
        reconcile(app);
        let _ = warmup::run(app, "wake").await;
        return;
    }
    let health = tokio::time::timeout(Duration::from_secs(5), receiver).await;
    let healthy = matches!(&health, Ok(Ok(Ok(()))));
    // A child that is still loading is not dead. Queueing the real warmup lets
    // it prove inference as soon as Ready arrives without paying a second load.
    if matches!(
        &health,
        Ok(Ok(Err(error))) if error == "resident Parakeet is not ready"
    ) {
        let _ = warmup::run(app, "wake").await;
        return;
    }
    if !healthy {
        let state = app.state::<ParakeetWorkerState>();
        let completion = {
            let mut slot = state
                .runtime
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if matches!(&*slot, RuntimeSlot::Running(runtime) if runtime.tx.same_channel(&tx)) {
                match std::mem::replace(&mut *slot, RuntimeSlot::Stopped) {
                    RuntimeSlot::Running(runtime) => {
                        let completion = spawn_stop(app.clone(), runtime);
                        *slot = RuntimeSlot::Stopping(completion.clone());
                        Some(completion)
                    }
                    _ => unreachable!(),
                }
            } else {
                None
            }
        };
        if let Some(completion) = completion {
            wait_for_stop(completion).await;
        }
    }
    let _ = warmup::run(app, "wake").await;
}

pub async fn shutdown(app: &AppHandle) {
    let state = app.state::<ParakeetWorkerState>();
    let completion = {
        let mut slot = state
            .runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match std::mem::replace(&mut *slot, RuntimeSlot::ShuttingDown(None)) {
            RuntimeSlot::Running(runtime) => {
                let completion = spawn_stop(app.clone(), runtime);
                *slot = RuntimeSlot::ShuttingDown(Some(completion.clone()));
                Some(completion)
            }
            RuntimeSlot::Stopping(completion) => {
                *slot = RuntimeSlot::ShuttingDown(Some(completion.clone()));
                Some(completion)
            }
            RuntimeSlot::ShuttingDown(completion) => {
                let wait = completion.clone();
                *slot = RuntimeSlot::ShuttingDown(completion);
                wait
            }
            RuntimeSlot::Stopped => None,
        }
    };
    if let Some(completion) = completion {
        wait_for_stop(completion).await;
    }
}

fn running_tx(app: &AppHandle) -> Option<mpsc::UnboundedSender<WorkerCommand>> {
    let state = app.state::<ParakeetWorkerState>();
    let slot = state
        .runtime
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match &*slot {
        RuntimeSlot::Running(runtime) => Some(runtime.tx.clone()),
        _ => None,
    }
}

fn start_runtime(app: &AppHandle) -> WorkerRuntime {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = app.clone();
    let task = tauri::async_runtime::spawn(async move { supervisor::supervise(handle, rx).await });
    WorkerRuntime { tx, task }
}

fn spawn_stop(app: AppHandle, runtime: WorkerRuntime) -> StopCompletion {
    let (done, completion) = watch::channel(false);
    tauri::async_runtime::spawn(async move {
        stop_runtime(runtime).await;
        finish_stop(&app);
        let _ = done.send(true);
    });
    completion
}

async fn stop_runtime(runtime: WorkerRuntime) {
    let _ = runtime.tx.send(WorkerCommand::Shutdown);
    runtime.task.abort();
    let _ = runtime.task.await;
}

async fn wait_for_stop(mut completion: StopCompletion) {
    while !*completion.borrow() {
        if completion.changed().await.is_err() {
            break;
        }
    }
}

fn finish_stop(app: &AppHandle) {
    let state = app.state::<ParakeetWorkerState>();
    let mut slot = state
        .runtime
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let enabled = configured(&crate::config::load_settings());
    match &*slot {
        RuntimeSlot::Stopping(_) if enabled => {
            *slot = RuntimeSlot::Running(start_runtime(app));
        }
        RuntimeSlot::Stopping(_) => {
            *slot = RuntimeSlot::Stopped;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_only_for_enabled_parakeet_dictation() {
        let mut settings = AppSettings::default();
        settings.dictation.enabled = true;
        settings.transcription.provider = TranscriptionProvider::FluidAudio;
        settings.transcription.keep_model_ready = true;
        assert!(configured(&settings));
        settings.dictation.enabled = false;
        assert!(!configured(&settings));
        settings.dictation.enabled = true;
        settings.transcription.provider = TranscriptionProvider::Qwen3;
        assert!(!configured(&settings));
        settings.transcription.provider = TranscriptionProvider::AppleSpeech;
        assert!(!configured(&settings));
        settings.transcription.provider = TranscriptionProvider::FluidAudio;
        settings.transcription.keep_model_ready = false;
        assert!(!configured(&settings));
    }
}
