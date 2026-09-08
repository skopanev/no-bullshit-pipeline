//! Shared capture lifecycle for recording, dictation, and the tray menu.
//!
//! Audio recording and Quick Dictate historically kept independent flags. That
//! allowed both to claim the microphone at once and left the tray guessing which
//! action was actually live. This module provides one small process-wide state
//! machine: start paths claim it before touching audio, state transitions refresh
//! the tray, and RAII guards roll failed/cancelled starts back to idle.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tauri::AppHandle;

#[derive(Clone, Debug)]
pub(crate) struct DictationContext {
    pub name: String,
    pub pipeline: Option<String>,
    pub auto_paste: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DictationPhase {
    ReadingClipboard,
    Transcribing,
    Processing,
    Pasting,
}

#[derive(Clone, Debug)]
pub(crate) enum CaptureActivity {
    Idle,
    StartingRecording {
        token: u64,
    },
    Recording {
        token: u64,
        id: String,
        title: String,
        started_at: Instant,
    },
    FinishingRecording {
        token: u64,
        id: String,
        title: String,
    },
    StartingDictation {
        token: u64,
        context: DictationContext,
    },
    Dictating {
        token: u64,
        context: DictationContext,
        started_at: Instant,
    },
    DictationProcessing {
        token: u64,
        context: DictationContext,
        phase: DictationPhase,
    },
}

static ACTIVITY: Mutex<CaptureActivity> = Mutex::new(CaptureActivity::Idle);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn next_token() -> u64 {
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

fn busy_error(activity: &CaptureActivity) -> String {
    match activity {
        CaptureActivity::StartingRecording { .. } | CaptureActivity::Recording { .. } => {
            "A recording is already in progress".into()
        }
        CaptureActivity::FinishingRecording { .. } => {
            "The previous recording is still being saved".into()
        }
        CaptureActivity::StartingDictation { .. }
        | CaptureActivity::Dictating { .. }
        | CaptureActivity::DictationProcessing { .. } => "Dictation is already in progress".into(),
        CaptureActivity::Idle => "Capture is busy".into(),
    }
}

fn refresh(app: &AppHandle) {
    crate::refresh_tray_menu(app);
}

pub(crate) fn snapshot() -> CaptureActivity {
    ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[derive(Clone, Copy)]
enum ClaimKind {
    Recording,
    Dictation,
}

/// Owns a provisional capture claim. Unless committed after the audio/session
/// is fully installed, Drop restores Idle and refreshes the tray.
pub(crate) struct CaptureClaim {
    app: AppHandle,
    token: u64,
    kind: ClaimKind,
    committed: bool,
}

impl CaptureClaim {
    pub(crate) fn token(&self) -> u64 {
        self.token
    }

    pub(crate) fn activate_recording(&self, id: String, title: String) {
        let changed = {
            let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
            match &*activity {
                CaptureActivity::StartingRecording { token } if *token == self.token => {
                    *activity = CaptureActivity::Recording {
                        token: self.token,
                        id,
                        title,
                        started_at: Instant::now(),
                    };
                    true
                }
                _ => false,
            }
        };
        if changed {
            refresh(&self.app);
        }
    }

    pub(crate) fn activate_dictation(&self) {
        let changed = {
            let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
            match &*activity {
                CaptureActivity::StartingDictation {
                    token,
                    context,
                } if *token == self.token => {
                    let context = context.clone();
                    *activity = CaptureActivity::Dictating {
                        token: self.token,
                        context,
                        started_at: Instant::now(),
                    };
                    true
                }
                _ => false,
            }
        };
        if changed {
            refresh(&self.app);
        }
    }

    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CaptureClaim {
    fn drop(&mut self) {
        if !self.committed && release_token(self.token, self.kind) {
            refresh(&self.app);
        }
    }
}

pub(crate) fn claim_recording(app: &AppHandle) -> Result<CaptureClaim, String> {
    let token = next_token();
    {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*activity, CaptureActivity::Idle) {
            return Err(busy_error(&activity));
        }
        *activity = CaptureActivity::StartingRecording { token };
    }
    refresh(app);
    Ok(CaptureClaim {
        app: app.clone(),
        token,
        kind: ClaimKind::Recording,
        committed: false,
    })
}

pub(crate) fn claim_dictation(
    app: &AppHandle,
    context: DictationContext,
    clipboard: bool,
) -> Result<CaptureClaim, String> {
    let token = next_token();
    {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*activity, CaptureActivity::Idle) {
            return Err(busy_error(&activity));
        }
        *activity = if clipboard {
            CaptureActivity::DictationProcessing {
                token,
                context,
                phase: DictationPhase::ReadingClipboard,
            }
        } else {
            CaptureActivity::StartingDictation {
                token,
                context,
            }
        };
    }
    refresh(app);
    Ok(CaptureClaim {
        app: app.clone(),
        token,
        kind: ClaimKind::Dictation,
        committed: false,
    })
}

fn release_token(token: u64, kind: ClaimKind) -> bool {
    let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
    let matches = match (&*activity, kind) {
        (
            CaptureActivity::StartingRecording { token: live, .. }
            | CaptureActivity::Recording { token: live, .. }
            | CaptureActivity::FinishingRecording { token: live, .. },
            ClaimKind::Recording,
        ) => *live == token,
        (
            CaptureActivity::StartingDictation { token: live, .. }
            | CaptureActivity::Dictating { token: live, .. }
            | CaptureActivity::DictationProcessing { token: live, .. },
            ClaimKind::Dictation,
        ) => *live == token,
        _ => false,
    };
    if matches {
        *activity = CaptureActivity::Idle;
    }
    matches
}

pub(crate) fn begin_finishing_recording(app: &AppHandle) {
    let changed = {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        match &*activity {
            CaptureActivity::Recording {
                token, id, title, ..
            } => {
                let (token, id, title) = (*token, id.clone(), title.clone());
                *activity = CaptureActivity::FinishingRecording { token, id, title };
                true
            }
            _ => false,
        }
    };
    if changed {
        refresh(app);
    }
}

pub(crate) fn finish_recording(app: &AppHandle, id: Option<&str>) {
    let changed = {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        let should_finish = match &*activity {
            CaptureActivity::StartingRecording { .. } => id.is_none(),
            CaptureActivity::Recording { id: live, .. }
            | CaptureActivity::FinishingRecording { id: live, .. } => {
                id.is_none_or(|expected| expected == live)
            }
            _ => false,
        };
        if should_finish {
            *activity = CaptureActivity::Idle;
        }
        should_finish
    };
    if changed {
        refresh(app);
    }
}

pub(crate) fn set_recording_label(app: &AppHandle, id: &str, title: String) {
    let changed = {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *activity {
            CaptureActivity::Recording {
                id: live,
                title: live_title,
                ..
            }
            | CaptureActivity::FinishingRecording {
                id: live,
                title: live_title,
                ..
            } if live == id => {
                *live_title = title;
                true
            }
            _ => false,
        }
    };
    if changed {
        refresh(app);
    }
}

pub(crate) fn set_dictation_phase(app: &AppHandle, token: u64, phase: DictationPhase) {
    let changed = {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        match &*activity {
            CaptureActivity::StartingDictation {
                token: live,
                context,
                ..
            }
            | CaptureActivity::Dictating {
                token: live,
                context,
                ..
            }
            | CaptureActivity::DictationProcessing {
                token: live,
                context,
                ..
            } if *live == token => {
                let context = context.clone();
                *activity = CaptureActivity::DictationProcessing {
                    token,
                    context,
                    phase,
                };
                true
            }
            _ => false,
        }
    };
    if changed {
        refresh(app);
    }
}

pub(crate) fn finish_dictation(app: &AppHandle, token: u64) {
    if release_token(token, ClaimKind::Dictation) {
        refresh(app);
    }
}

/// Explicit user cancellation may happen during mic startup, while recording,
/// or after capture in the detached pipeline. Clear whichever dictation phase
/// is current; token-matched completion guards from older tasks then no-op.
pub(crate) fn cancel_current_dictation(app: &AppHandle) {
    let changed = {
        let mut activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            *activity,
            CaptureActivity::StartingDictation { .. }
                | CaptureActivity::Dictating { .. }
                | CaptureActivity::DictationProcessing { .. }
        ) {
            *activity = CaptureActivity::Idle;
            true
        } else {
            false
        }
    };
    if changed {
        refresh(app);
    }
}

/// Moved into a detached dictation task. Aborting or naturally completing that
/// task both drop the guard, so the tray cannot remain stuck on Processing.
pub(crate) struct DictationCompletionGuard {
    app: AppHandle,
    token: u64,
}

impl DictationCompletionGuard {
    pub(crate) fn new(app: &AppHandle, token: u64) -> Self {
        Self {
            app: app.clone(),
            token,
        }
    }
}

impl Drop for DictationCompletionGuard {
    fn drop(&mut self) {
        finish_dictation(&self.app, self.token);
    }
}
