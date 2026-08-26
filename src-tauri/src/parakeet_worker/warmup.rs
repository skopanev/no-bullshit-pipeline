use crate::config::load_settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const WARMUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const WARMUP_TIMEOUT: Duration = Duration::from_secs(30);
const WARMUP_SAMPLES: usize = 16_000 * 3 / 10;

static WARMUP_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

struct WarmupGuard;

impl Drop for WarmupGuard {
    fn drop(&mut self) {
        WARMUP_IN_FLIGHT.store(false, Ordering::Release);
    }
}

pub(crate) async fn run(app: &AppHandle, reason: &'static str) -> Result<(), String> {
    let mut settings = load_settings();
    if !super::configured(&settings) {
        return Ok(());
    }
    if WARMUP_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        log::debug!("resident Parakeet warmup skipped ({reason}): already running");
        return Ok(());
    }
    let _guard = WarmupGuard;
    loop {
        let capture_active = app
            .state::<crate::dictation::DictationState>()
            .is_active
            .load(Ordering::Acquire);
        if !capture_active
            && !crate::dictation::asr_reserved(app)
            && !super::request::user_request_in_flight()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        settings = load_settings();
        if !super::configured(&settings) {
            return Ok(());
        }
    }
    settings = load_settings();
    if !super::configured(&settings) {
        return Ok(());
    }
    let started = Instant::now();
    let silence = vec![0.0; WARMUP_SAMPLES];
    let result = super::request::transcribe_warmup(app, &silence, &settings, WARMUP_TIMEOUT)
        .await
        .map(|_| ());
    match &result {
        Ok(()) => log::info!(
            "resident Parakeet warmup complete reason={reason} elapsed={:?}",
            started.elapsed()
        ),
        Err(error) => log::warn!(
            "resident Parakeet warmup failed reason={reason} elapsed={:?}: {error}",
            started.elapsed()
        ),
    }
    result
}

pub(crate) fn request(app: &AppHandle, reason: &'static str) {
    if !super::configured(&load_settings()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = run(&app, reason).await;
    });
}

pub(crate) fn install_timer(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(WARMUP_INTERVAL).await;
            let _ = run(&app, "periodic").await;
        }
    });
}
