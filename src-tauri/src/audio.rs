use crate::storage::{self, RecordingMetadata};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::SystemTime;
use tauri::{Emitter, State};

pub struct AudioState {
    pub is_recording: Mutex<bool>,
    pub mic_recorder: Mutex<Option<crate::mic_audio::MicAudioRecorder>>,
    pub system_recorder: Mutex<Option<crate::system_audio::SystemAudioRecorder>>,
    pub realtime_mixer: Mutex<Option<crate::audio_processing::RealtimeMixer>>,
    pub current_session: Mutex<Option<RecordingMetadata>>,
    pub start_timestamp: Mutex<Option<SystemTime>>,
    pub save_mix_only: Mutex<bool>,
    pub finalization_handle: Mutex<Option<JoinHandle<()>>>,
    pub realtime_transcriber:
        Mutex<Option<Box<dyn crate::realtime_transcription::RealtimeTranscriberHandle>>>,
}

impl AudioState {
    pub fn new() -> Self {
        Self {
            is_recording: Mutex::new(false),
            mic_recorder: Mutex::new(None),
            system_recorder: Mutex::new(None),
            realtime_mixer: Mutex::new(None),
            current_session: Mutex::new(None),
            start_timestamp: Mutex::new(None),
            save_mix_only: Mutex::new(true),
            finalization_handle: Mutex::new(None),
            realtime_transcriber: Mutex::new(None),
        }
    }

    /// Wait for any in-progress finalization to complete (called on shutdown / before new recording)
    pub fn wait_for_finalization(&self) {
        if let Ok(mut handle_guard) = self.finalization_handle.lock()
            && let Some(handle) = handle_guard.take()
        {
            let _ = handle.join();
        }
    }
}

#[tauri::command]
pub fn start_recording(
    app_handle: tauri::AppHandle,
    state: State<'_, AudioState>,
    save_mix_only: bool,
) -> Result<storage::RecordingMetadata, String> {
    let mut is_recording = state.is_recording.lock().map_err(|e| e.to_string())?;
    if *is_recording {
        return Err("Already recording".to_string());
    }
    // One process-wide claim prevents regular recording and Quick Dictate from
    // opening the microphone concurrently. It rolls back automatically if any
    // setup step below fails.
    let mut capture_claim = crate::capture_coordinator::claim_recording(&app_handle)?;

    // Leaked-session probe: at the start of a fresh recording no mic stream
    // should be alive. >0 here means a prior session never disposed its cpal
    // stream (the "still recording after restart" symptom).
    let leaked = crate::mic_audio::active_mic_streams();
    if leaked > 0 {
        log::warn!(
            "[mic-debug] start_recording: {leaked} mic stream(s) already active before start — previous session not disposed"
        );
    } else {
        log::info!("[mic-debug] start_recording: clean (active_mic_streams=0)");
    }

    // Wait for any previous finalization to complete before starting new recording
    state.wait_for_finalization();

    // --- Disk space check ---
    let data_dir = storage::get_data_dir();
    check_disk_space(&data_dir)?;

    // Default title for manual recordings: "NBP · HH:MM". Call recordings get
    // this overwritten with "{App} · HH:MM" by call_session::run_start.
    let title = format!("NBP · {}", chrono::Local::now().format("%H:%M"));
    let metadata = storage::create_recording(title)?;
    *state.save_mix_only.lock().map_err(|e| e.to_string())? = save_mix_only;

    // --- Real-time Mixer FIRST (so it's ready before capture threads push data) ---
    let mix_path = storage::get_recording_dir(&metadata.id).join("audio_mix.ogg");
    match crate::audio_processing::RealtimeMixer::new(mix_path) {
        Ok(mixer) => {
            *state.realtime_mixer.lock().map_err(|e| e.to_string())? = Some(mixer);
        }
        Err(e) => {
            eprintln!("WARNING: Real-time mixer failed: {}", e);
        }
    }

    // --- Microphone Capture ---
    let mic_path = storage::get_recording_dir(&metadata.id).join("raw_mic.ogg");
    match crate::mic_audio::start_mic_capture(mic_path, None, save_mix_only) {
        Ok(recorder) => {
            *state.mic_recorder.lock().map_err(|e| e.to_string())? = Some(recorder);
        }
        Err(e) => {
            return Err(format!("Microphone capture failed: {}", e));
        }
    }

    // --- System Audio ---
    let system_path = storage::get_recording_dir(&metadata.id).join("raw_system.ogg");
    match crate::system_audio::start_system_capture(system_path, save_mix_only) {
        Ok(recorder) => {
            *state.system_recorder.lock().map_err(|e| e.to_string())? = Some(recorder);
        }
        Err(e) => {
            eprintln!("ERROR: System audio capture failed: {:?}", e);
            let _ = app_handle.emit(
                "recording_warning",
                format!("System audio unavailable: {}", e),
            );
        }
    }

    *state.start_timestamp.lock().map_err(|e| e.to_string())? = Some(std::time::SystemTime::now());

    let metadata_clone = metadata.clone();
    *state.current_session.lock().map_err(|e| e.to_string())? = Some(metadata);
    *is_recording = true;

    capture_claim.activate_recording(metadata_clone.id.clone(), metadata_clone.title.clone());
    // Blink the tray icon while recording.
    crate::start_tray_pulse(&app_handle);
    capture_claim.commit();

    Ok(metadata_clone)
}

/// Check that at least 100MB of disk space is available
fn check_disk_space(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|e| format!("Invalid path: {}", e))?;
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
        if ret == 0 {
            let available_bytes = stat.f_bavail as u64 * stat.f_bsize as u64;
            let min_bytes = 100 * 1024 * 1024; // 100 MB
            if available_bytes < min_bytes {
                return Err(format!(
                    "Insufficient disk space: {:.0} MB available, need at least 100 MB",
                    available_bytes as f64 / (1024.0 * 1024.0)
                ));
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn pause_recording(_state: State<'_, AudioState>) -> Result<(), String> {
    Err("Pause is not yet implemented".to_string())
}

#[tauri::command]
pub fn resume_recording(_state: State<'_, AudioState>) -> Result<(), String> {
    Err("Resume is not yet implemented".to_string())
}

#[tauri::command]
pub fn stop_recording(
    app_handle: tauri::AppHandle,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    log::info!("[ignore-trace] stop_recording entered");
    let mut is_recording = state.is_recording.lock().map_err(|e| e.to_string())?;
    if !*is_recording {
        log::info!("[ignore-trace] stop_recording: is_recording=false, returning Ok early");
        return Ok(());
    }
    log::info!("[ignore-trace] stop_recording: is_recording=true, proceeding to stop captures");

    crate::capture_coordinator::begin_finishing_recording(&app_handle);
    // Stop the tray-icon blink — recording is ending.
    crate::stop_tray_pulse();

    // Dismiss the call-detection popup immediately — recording is ending, so its
    // Ignore window is moot. Covers every stop route (in-app button, Ignore
    // click, call end). Harmless no-op if the popup isn't visible.
    let _ = app_handle.emit("call-popup", serde_json::json!({ "kind": "hide" }));

    // CORRECT ORDER: Stop capture sources FIRST, then let mixer drain

    // 1. Stop Microphone capture (stops cpal stream, joins encoder thread)
    {
        let mut mic_guard = state.mic_recorder.lock().map_err(|e| e.to_string())?;
        if let Some(mut recorder) = mic_guard.take() {
            recorder.stop();
        }
    }

    // 2. Stop System Audio capture (stops Core Audio device, joins encoder thread)
    {
        let mut system_guard = state.system_recorder.lock().map_err(|e| e.to_string())?;
        if let Some(mut recorder) = system_guard.take() {
            recorder.stop();
        }
    }

    // 3. Stop Real-time Transcriber (before mixer, so no more samples are needed)
    {
        let mut rt_guard = state
            .realtime_transcriber
            .lock()
            .map_err(|e| e.to_string())?;
        if let Some(mut handle) = rt_guard.take() {
            handle.stop();
        }
    }

    // 4. Stop Real-time Mixer LAST — it drains remaining buffer samples before finishing
    {
        let mut mixer_guard = state.realtime_mixer.lock().map_err(|e| e.to_string())?;
        if let Some(mut mixer) = mixer_guard.take() {
            mixer.stop();
        }
    }

    // --- Duration & Threshold Check ---
    let start_time = *state.start_timestamp.lock().map_err(|e| e.to_string())?;
    let duration_sec = if let Some(start) = start_time {
        start.elapsed().unwrap_or_default().as_secs_f64()
    } else {
        0.0
    };

    let threshold = crate::config::load_settings().auto_discard_seconds as f64;

    // Too short — discard immediately, never enter "processing".
    // Emit `recording_discarded` so the frontend can drop the row added by
    // the optimistic `recording_started` (call-popup flow) or `loadRecordings`
    // (manual flow). Without this, the row lingers until something else
    // triggers a list refresh.
    log::info!(
        "[ignore-trace] stop_recording: duration={:.2}s, threshold={:.2}s, path={}",
        duration_sec,
        threshold,
        if duration_sec < threshold {
            "DISCARD"
        } else {
            "FINALIZE"
        }
    );
    if duration_sec < threshold {
        let mut session_guard = state.current_session.lock().map_err(|e| e.to_string())?;
        let discarded_id = session_guard.take().map(|meta| {
            let id = meta.id.clone();
            eprintln!(
                "Discarding recording {} (duration {:.2}s < threshold {:.2}s)",
                id, duration_sec, threshold
            );
            let dir = storage::get_recording_dir(&id);
            let dir_existed = dir.exists();
            log::info!(
                "[ignore-trace] stop_recording DISCARD: id={} dir={} existed={}",
                id,
                dir.display(),
                dir_existed
            );
            if dir_existed {
                match std::fs::remove_dir_all(&dir) {
                    Ok(_) => log::info!("[ignore-trace] stop_recording DISCARD: removed dir"),
                    Err(e) => log::warn!(
                        "[ignore-trace] stop_recording DISCARD: remove failed: {}",
                        e
                    ),
                }
            }
            id
        });
        drop(session_guard);
        *is_recording = false;
        crate::mic_audio::reset_audio_level();
        crate::system_audio::reset_system_audio_level();
        if let Some(id) = discarded_id {
            // remove_dir_all bypasses write_metadata, so the list cache
            // still holds the row with status="recording". Without this the
            // frontend's loadRecordings (triggered by recording_discarded
            // below) serves the stale row and the list keeps showing the
            // discarded recording.
            storage::invalidate_list_cache();
            log::info!(
                "[ignore-trace] stop_recording DISCARD: invalidated cache, emitting recording_discarded({})",
                id
            );
            let _ = app_handle.emit("recording_discarded", &id);
            crate::capture_coordinator::finish_recording(&app_handle, Some(&id));
        } else {
            log::warn!(
                "[ignore-trace] stop_recording DISCARD: session_guard.take() returned None — no emit"
            );
            crate::capture_coordinator::finish_recording(&app_handle, None);
        }
        return Ok(());
    }

    let save_mix_only = *state.save_mix_only.lock().map_err(|e| e.to_string())?;

    let finalization_handle = {
        let mut session_guard = state.current_session.lock().map_err(|e| e.to_string())?;
        if let Some(in_memory_metadata) = session_guard.take() {
            let id = in_memory_metadata.id.clone();

            let mut metadata = match storage::read_metadata(&id) {
                Ok(m) => m,
                Err(_) => in_memory_metadata,
            };

            metadata.status = "processing".to_string();
            if !save_mix_only {
                metadata.audio.mic = Some(storage::AudioInfo {
                    file: "raw_mic.ogg".to_string(),
                    duration_sec,
                    sample_rate: 48000,
                    channels: 2,
                });
                metadata.audio.system = Some(storage::AudioInfo {
                    file: "raw_system.ogg".to_string(),
                    duration_sec,
                    sample_rate: 48000,
                    channels: 2,
                });
            }

            if let Err(e) = storage::write_metadata(&metadata) {
                eprintln!("Failed to write initial metadata: {}", e);
            }

            let id = metadata.id.clone();

            // Finalization in background thread — handle is STORED, not fire-and-forget
            let finalization_id = id.clone();
            let finalization_app_handle = app_handle.clone();
            Some(std::thread::spawn(move || {
                log::info!(
                    "[ignore-trace] finalize thread started for {}",
                    finalization_id
                );
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    finalize_recording(&finalization_id, duration_sec, save_mix_only);
                }));
                if result.is_err() {
                    eprintln!("Finalization thread panicked for {}", finalization_id);
                    log::warn!("[ignore-trace] finalize PANICKED for {}", finalization_id);
                    if let Ok(mut m) = storage::read_metadata(&finalization_id) {
                        m.status = "error".to_string();
                        let _ = storage::write_metadata(&m);
                    }
                }
                let dir_after = storage::get_recording_dir(&finalization_id).exists();
                log::info!(
                    "[ignore-trace] finalize thread DONE for {}, dir_exists={}, emitting recording_complete",
                    finalization_id,
                    dir_after
                );
                crate::capture_coordinator::finish_recording(
                    &finalization_app_handle,
                    Some(&finalization_id),
                );
                // Notify frontend that recording is finalized and ready
                let _ = finalization_app_handle.emit("recording_complete", &finalization_id);
            }))
        } else {
            None
        }
    };

    // Store finalization handle so it can be joined on shutdown or next recording
    if let Some(handle) = finalization_handle {
        if let Ok(mut h) = state.finalization_handle.lock() {
            // Join any previous handle first
            if let Some(prev) = h.take() {
                let _ = prev.join();
            }
            *h = Some(handle);
        }
    } else {
        crate::capture_coordinator::finish_recording(&app_handle, None);
    }

    *is_recording = false;

    crate::mic_audio::reset_audio_level();
    crate::system_audio::reset_system_audio_level();

    Ok(())
}

/// Background finalization: cleanup files and update metadata to "ready"
fn finalize_recording(id: &str, duration_sec: f64, save_mix_only: bool) {
    let dir = storage::get_recording_dir(id);
    let mix_path = dir.join("audio_mix.ogg");
    let mic_path = dir.join("raw_mic.ogg");
    let system_path = dir.join("raw_system.ogg");

    let mix_exists = mix_path.exists();

    // Delete separate files if save_mix_only
    if save_mix_only {
        let _ = std::fs::remove_file(&mic_path);
        let _ = std::fs::remove_file(&system_path);
    }

    // Final metadata update
    match storage::read_metadata(id) {
        Ok(mut latest_metadata) => {
            latest_metadata.status = "ready".to_string();

            if !save_mix_only {
                latest_metadata.audio.mic = Some(storage::AudioInfo {
                    file: "raw_mic.ogg".to_string(),
                    duration_sec,
                    sample_rate: 48000,
                    channels: 2,
                });
                latest_metadata.audio.system = Some(storage::AudioInfo {
                    file: "raw_system.ogg".to_string(),
                    duration_sec,
                    sample_rate: 48000,
                    channels: 2,
                });
            } else {
                latest_metadata.audio.mic = None;
                latest_metadata.audio.system = None;
            }

            if mix_exists {
                latest_metadata.audio.mix = Some(storage::AudioInfo {
                    file: "audio_mix.ogg".to_string(),
                    duration_sec,
                    sample_rate: 48000,
                    channels: 2,
                });
            }

            // Match this recording to a calendar event by span overlap (silent;
            // no-op without calendar access or an overlapping event). Sets the
            // title + attendees in place before we persist.
            crate::calendar::try_match_on_finalize(&mut latest_metadata, duration_sec);

            if let Err(e) = storage::write_metadata(&latest_metadata) {
                eprintln!("Failed to save final metadata for {}: {}", id, e);
            }
        }
        Err(e) => eprintln!("Failed to reload metadata for {}: {}", id, e),
    }
}

/// Start real-time transcription for the given recording.
///
/// Validates that `recording_id` matches the currently active session to prevent
/// stale IDs from hijacking a running transcription. Dispatches to the streaming
/// backend matching the configured transcription provider (currently AppleSpeech;
/// FluidAudio streaming arrives in a follow-up). No-ops if the selected provider
/// has no streaming sidecar.
#[tauri::command]
pub async fn start_realtime_transcription(
    recording_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    {
        let is_recording = state.is_recording.lock().map_err(|e| e.to_string())?;
        if !*is_recording {
            return Err("No active recording".to_string());
        }
    }

    {
        let session = state.current_session.lock().map_err(|e| e.to_string())?;
        let current_id = session.as_ref().map(|s| s.id.as_str());
        if current_id != Some(recording_id.as_str()) {
            return Err(format!(
                "recording_id '{}' does not match current recording '{}'",
                recording_id,
                current_id.unwrap_or("<none>")
            ));
        }
    }

    let settings = crate::config::load_settings();
    let rt_config = &settings.transcription;
    if !rt_config.realtime_supported() {
        return Ok(());
    }

    {
        let mut guard = state
            .realtime_transcriber
            .lock()
            .map_err(|e| e.to_string())?;
        if let Some(mut existing) = guard.take() {
            existing.stop();
        }
    }

    use crate::config::TranscriptionProvider;
    use crate::realtime_transcription::{
        AppleSpeechRealtimeTranscriber, RealtimeTranscriberHandle,
    };

    let new_transcriber: Option<Box<dyn RealtimeTranscriberHandle>> = match rt_config.provider {
        TranscriptionProvider::AppleSpeech => {
            let t = AppleSpeechRealtimeTranscriber::start(app_handle, recording_id)?;
            Some(Box::new(t))
        }
        // FluidAudio streaming arrives in a follow-up ticket; accept the call
        // silently so the recording flow keeps working when its variant is
        // selected without a sidecar wired up yet.
        _ => None,
    };

    if let Some(t) = new_transcriber {
        let mut guard = state
            .realtime_transcriber
            .lock()
            .map_err(|e| e.to_string())?;
        *guard = Some(t);
    }
    Ok(())
}

/// Stop the active real-time transcription session (if any).
#[tauri::command]
pub fn stop_realtime_transcription(state: State<'_, AudioState>) -> Result<(), String> {
    let mut guard = state
        .realtime_transcriber
        .lock()
        .map_err(|e| e.to_string())?;
    if let Some(mut transcriber) = guard.take() {
        transcriber.stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time test: AudioState must be Send + Sync
    /// This verifies Story 7.1 - compiler enforces thread safety after removing unsafe impls
    #[test]
    fn test_audio_state_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<AudioState>();
        assert_sync::<AudioState>();
    }
}
