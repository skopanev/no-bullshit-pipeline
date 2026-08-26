use super::cancellation::CancelOnDrop;
use super::protocol::{OwnedTempPath, ResidentRequest, WorkerCommand, write_mono_wav};
use crate::config::AppSettings;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tokio::sync::oneshot;
use uuid::Uuid;

static USER_REQUESTS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static REQUEST_SERIALIZER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct UserRequestGuard;

impl UserRequestGuard {
    fn new() -> Self {
        USER_REQUESTS_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for UserRequestGuard {
    fn drop(&mut self) {
        USER_REQUESTS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn user_request_in_flight() -> bool {
    USER_REQUESTS_IN_FLIGHT.load(Ordering::Acquire) > 0
}

pub(super) async fn transcribe_user(
    app: &AppHandle,
    samples_16k: &[f32],
    settings: &AppSettings,
) -> Result<String, String> {
    let _guard = UserRequestGuard::new();
    let _serial = REQUEST_SERIALIZER.lock().await;
    transcribe_with_timeout(
        app,
        samples_16k,
        settings,
        crate::dictation::QUICK_DICTATE_TRANSCRIPTION_TIMEOUT,
    )
    .await
}

pub(super) async fn transcribe_warmup(
    app: &AppHandle,
    samples_16k: &[f32],
    settings: &AppSettings,
    timeout: Duration,
) -> Result<String, String> {
    loop {
        let serial = REQUEST_SERIALIZER.lock().await;
        if !crate::dictation::capture_active(app)
            && !crate::dictation::asr_reserved(app)
            && !user_request_in_flight()
        {
            return transcribe_with_timeout(app, samples_16k, settings, timeout).await;
        }
        drop(serial);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn transcribe_with_timeout(
    app: &AppHandle,
    samples_16k: &[f32],
    settings: &AppSettings,
    timeout: Duration,
) -> Result<String, String> {
    let started = Instant::now();
    let audio_secs = samples_16k.len() as f64 / 16_000.0;
    let path = std::env::temp_dir().join(format!("nbp-dict-{}.wav", Uuid::new_v4().simple()));
    let temp_path = OwnedTempPath::new(path);
    write_mono_wav(temp_path.path(), samples_16k)?;
    let vocab_path = if settings.transcription.translit_lang != "off" {
        let path = crate::vocab::vocab_path();
        path.exists().then(|| path.to_string_lossy().into_owned())
    } else {
        None
    };
    let request_id = Uuid::new_v4().to_string();
    let request = ResidentRequest::transcribe(
        request_id.clone(),
        temp_path.path().to_string_lossy().into_owned(),
        vocab_path,
        settings.transcription.translit_threshold,
        settings.transcription.translit_min_len,
    );
    let tx = super::running_tx(app).ok_or("resident Parakeet is not running")?;
    let (dispatched, dispatch_receiver) = oneshot::channel();
    let (reply, receiver) = oneshot::channel();
    tx.send(WorkerCommand::Transcribe {
        request,
        temp_path,
        dispatched,
        reply,
    })
    .map_err(|_| "resident Parakeet stopped".to_string())?;
    let cancellation = CancelOnDrop::new(tx, request_id);
    // Loading/download time is deliberately outside the inference deadline.
    // The supervisor acknowledges only after the request is written to a Ready
    // child, so a healthy loader cannot be killed by the 30s warmup timeout.
    match tokio::time::timeout(
        crate::dictation::QUICK_DICTATE_TRANSCRIPTION_TIMEOUT,
        dispatch_receiver,
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            cancellation.disarm();
            return receiver
                .await
                .map_err(|_| "resident Parakeet stopped".to_string())?;
        }
        Err(_) => {
            cancellation.cancel_timed_out();
            return Err(super::TRANSCRIPTION_TIMEOUT_ERROR.into());
        }
    }
    let result = match tokio::time::timeout(timeout, receiver).await {
        Ok(Ok(result)) => {
            cancellation.disarm();
            result
        }
        Ok(Err(_)) => {
            cancellation.disarm();
            Err("resident Parakeet stopped".into())
        }
        Err(_) => {
            cancellation.cancel_timed_out();
            Err(super::TRANSCRIPTION_TIMEOUT_ERROR.into())
        }
    };
    match &result {
        Ok(_) => log::info!(
            "resident Parakeet request complete audio_s={audio_secs:.2} elapsed={:?}",
            started.elapsed()
        ),
        Err(error) => log::warn!(
            "resident Parakeet request failed audio_s={audio_secs:.2} elapsed={:?}: {error}",
            started.elapsed()
        ),
    }
    result
}
