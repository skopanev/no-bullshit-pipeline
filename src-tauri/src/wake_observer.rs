//! macOS sleep/wake observer — re-warms the ASR specialization cache after
//! the laptop wakes up.
//!
//! **Why this exists.** Dictation transcription is mostly fast, but `iногда`
//! tormoz'it 1–3 sec when the user opens the lid after sleep. Hypothesis from
//! research + ensemble panel: CoreML caches its per-(mlmodelc path × compute
//! unit × MLModelConfiguration) ANE/Metal specialization in *purgeable* VM
//! memory, and macOS evicts that cache during sleep / under memory pressure.
//! Without resident mode, each Quick Dictate spawns a fresh one-shot sidecar
//! (`fluidaudio-sidecar`), so the next dictation after wake can pay the full
//! re-specialization cost on first inference.
//!
//! **What this does.** Subscribes to `NSWorkspaceDidWakeNotification` on
//! `[NSWorkspace.sharedWorkspace.notificationCenter]`. On fire, it verifies
//! the resident Parakeet supervisor when enabled. Other engines keep the
//! existing throwaway silence prewarm.
//!
//! **Debounce.** macOS occasionally fires `didWakeNotification` 2–3 times
//! within seconds on a single lid open (especially with external monitors
//! cycling). Without the throttle, every fire would spawn a redundant
//! prewarm sidecar.

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
use block2::RcBlock;
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyClass, AnyObject};
#[cfg(target_os = "macos")]
use objc2_foundation::NSString;

#[cfg(target_os = "macos")]
static LAST_WAKE_PREWARM_AT: AtomicU64 = AtomicU64::new(0);

/// Skip wake-prewarm if one already fired in this many seconds. 30s safely
/// covers macOS's multi-fire window without leaving the cache stale longer
/// than the user could realistically grab the hotkey.
#[cfg(target_os = "macos")]
const WAKE_DEBOUNCE_SECS: u64 = 30;

/// Install the wake observer. Idempotent in practice — called once from
/// `lib.rs::setup`. On non-macOS targets this is a no-op (the dictation
/// pipeline is macOS-only anyway).
#[cfg(target_os = "macos")]
pub fn install(app_handle: tauri::AppHandle) {
    use objc2::msg_send;

    // Capture an AppHandle for the wake callback — Tauri's AppHandle is
    // Clone + Send + Sync, safe to move into the long-lived block.
    let handle_for_block = app_handle.clone();

    // Block-based observer keeps the API surface tiny — no need to
    // `define_class!` an Objective-C delegate for a one-method protocol.
    let block = RcBlock::new(move |_note: *mut AnyObject| {
        // Reconcile capture state on EVERY wake (not debounced): the audio
        // device died during sleep, so cancel a stranded dictation, finalize a
        // stranded recording, and kill the orphaned tray pulse. Runs off the
        // notification thread — the stop paths join threads and reacquire locks.
        {
            let handle = handle_for_block.clone();
            std::thread::spawn(move || {
                crate::reconcile_after_wake(&handle);
            });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last = LAST_WAKE_PREWARM_AT.load(Ordering::Acquire);
        if now.saturating_sub(last) < WAKE_DEBOUNCE_SECS {
            log::debug!(
                "wake_observer: didWake — debounced ({}s since last prewarm)",
                now.saturating_sub(last)
            );
            return;
        }
        LAST_WAKE_PREWARM_AT.store(now, Ordering::Release);
        let settings = crate::config::load_settings();
        if crate::parakeet_worker::configured(&settings) {
            log::info!("wake_observer: didWake — verifying and warming resident Parakeet");
            let app = handle_for_block.clone();
            tauri::async_runtime::spawn(async move {
                crate::parakeet_worker::verify_after_wake(&app).await;
            });
        } else {
            log::info!("wake_observer: didWake — re-prewarming ASR (post-sleep cache refill)");
            // `prewarm_models` spawns its own async task internally and returns
            // immediately — block stays cheap to call from the notification queue.
            crate::dictation::prewarm_models(&handle_for_block);
        }
    });

    unsafe {
        let workspace_cls = match AnyClass::get(c"NSWorkspace") {
            Some(c) => c,
            None => {
                log::warn!("wake_observer: NSWorkspace class not found — wake prewarm disabled");
                return;
            }
        };
        let workspace: *mut AnyObject = msg_send![workspace_cls, sharedWorkspace];
        if workspace.is_null() {
            log::warn!("wake_observer: sharedWorkspace returned nil — wake prewarm disabled");
            return;
        }

        // IMPORTANT: NSWorkspace notifications live on its OWN notification
        // center, not the default one. Apple sends sleep/wake events only
        // through `[[NSWorkspace sharedWorkspace] notificationCenter]`.
        let center: *mut AnyObject = msg_send![workspace, notificationCenter];
        if center.is_null() {
            log::warn!("wake_observer: workspace notificationCenter is nil");
            return;
        }

        // NSWorkspaceDidWakeNotification is an exported NSString constant.
        // Building an equivalent NSString side-steps the dance around its
        // linkage and works because NSNotificationCenter matches names by
        // `isEqualToString:`.
        let name = NSString::from_str("NSWorkspaceDidWakeNotification");

        let _observer: *mut AnyObject = msg_send![
            center,
            addObserverForName: &*name,
            object: std::ptr::null_mut::<AnyObject>(),
            queue: std::ptr::null_mut::<AnyObject>(),
            usingBlock: &*block
        ];
        log::info!("wake_observer: NSWorkspaceDidWake observer installed");
    }

    // Leak the block: AppKit retains it internally via the addObserver call,
    // but holding a Rust-side reference keeps the closure captures
    // (`handle_for_block`) alive for the lifetime of the process and means
    // we don't need to plumb the observer token through anywhere to manage
    // unregistration — the observer fires for as long as the app lives,
    // which is exactly what we want.
    std::mem::forget(block);
}

#[cfg(not(target_os = "macos"))]
pub fn install(_app_handle: tauri::AppHandle) {}
