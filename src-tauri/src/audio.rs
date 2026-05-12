use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use std::thread::JoinHandle;
use tauri::{Emitter, State};
use crate::storage::{self, RecordingMetadata};

pub struct AudioState {
    pub is_recording: Mutex<bool>,
    pub mic_recorder: Mutex<Option<crate::mic_audio::MicAudioRecorder>>,
    pub system_recorder: Mutex<Option<crate::system_audio::SystemAudioRecorder>>,
    pub realtime_mixer: Mutex<Option<crate::audio_processing::RealtimeMixer>>,
    pub current_session: Mutex<Option<RecordingMetadata>>,
    pub start_timestamp: Mutex<Option<SystemTime>>,
    pub save_mix_only: Mutex<bool>,
    pub finalization_handle: Mutex<Option<JoinHandle<()>>>,
    pub realtime_transcriber: Mutex<Option<crate::realtime_transcription::LocalTranscriber>>,
    pub silence_monitor_stop: Mutex<Option<Arc<AtomicBool>>>,
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
            silence_monitor_stop: Mutex::new(None),
        }
    }

    /// Wait for any in-progress finalization to complete (called on shutdown / before new recording)
    pub fn wait_for_finalization(&self) {
        if let Ok(mut handle_guard) = self.finalization_handle.lock() {
            if let Some(handle) = handle_guard.take() {
                let _ = handle.join();
            }
        }
    }
}

#[tauri::command]
pub fn start_recording(app_handle: tauri::AppHandle, state: State<'_, AudioState>, save_mix_only: bool) -> Result<storage::RecordingMetadata, String> {
    let mut is_recording = state.is_recording.lock().map_err(|e| e.to_string())?;
    if *is_recording {
        return Err("Already recording".to_string());
    }

    // Wait for any previous finalization to complete before starting new recording
    state.wait_for_finalization();

    // --- Disk space check ---
    let data_dir = storage::get_data_dir();
    check_disk_space(&data_dir)?;

    let metadata = storage::create_recording(String::new(), vec![])?;
    *state.save_mix_only.lock().map_err(|e| e.to_string())? = save_mix_only;

    // --- Real-time Mixer FIRST (so it's ready before capture threads push data) ---
    let mix_path = storage::get_recording_dir(&metadata.id).join("audio_mix.ogg");
    match crate::audio_processing::RealtimeMixer::new(mix_path) {
        Ok(mixer) => {
            *state.realtime_mixer.lock().map_err(|e| e.to_string())? = Some(mixer);
        },
        Err(e) => {
            eprintln!("WARNING: Real-time mixer failed: {}", e);
        }
    }

    // --- Microphone Capture ---
    let mic_path = storage::get_recording_dir(&metadata.id).join("raw_mic.ogg");
    match crate::mic_audio::start_mic_capture(mic_path, None, save_mix_only) {
        Ok(recorder) => {
            *state.mic_recorder.lock().map_err(|e| e.to_string())? = Some(recorder);
        },
        Err(e) => {
            return Err(format!("Microphone capture failed: {}", e));
        }
    }

    // --- System Audio ---
    let system_path = storage::get_recording_dir(&metadata.id).join("raw_system.ogg");
    match crate::system_audio::start_system_capture(system_path, save_mix_only) {
        Ok(recorder) => {
            *state.system_recorder.lock().map_err(|e| e.to_string())? = Some(recorder);
        },
        Err(e) => {
            eprintln!("ERROR: System audio capture failed: {:?}", e);
            let _ = app_handle.emit("recording_warning", format!("System audio unavailable: {}", e));
        }
    }

    *state.start_timestamp.lock().map_err(|e| e.to_string())? = Some(std::time::SystemTime::now());

    let metadata_clone = metadata.clone();
    *state.current_session.lock().map_err(|e| e.to_string())? = Some(metadata);
    *is_recording = true;

    // Start silence monitor if configured
    let settings = crate::config::load_settings();
    let silence_secs = settings.auto_stop_silence_seconds;
    if silence_secs > 0 {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = stop_flag.clone();
        let app_clone = app_handle.clone();
        std::thread::spawn(move || {
            run_silence_monitor(flag_clone, app_clone, silence_secs);
        });
        *state.silence_monitor_stop.lock().map_err(|e| e.to_string())? = Some(stop_flag);
    }

    Ok(metadata_clone)
}

/// System audio silence threshold (only remote participants' audio matters)
const SILENCE_THRESHOLD: f32 = 0.005;
/// Minimum recording duration before auto-stop kicks in (seconds)
const MIN_DURATION_BEFORE_AUTO_STOP: u64 = 30;

fn run_silence_monitor(should_stop: Arc<AtomicBool>, app_handle: tauri::AppHandle, silence_secs: u32) {
    use std::time::{Duration, Instant};

    let mut silence_start: Option<Instant> = None;
    let silence_threshold_duration = Duration::from_secs(silence_secs as u64);
    let start_time = Instant::now();
    // Track whether we ever saw system audio (confirms this is a call, not solo recording)
    let mut ever_had_system_audio = false;

    while !should_stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(500));

        // Don't auto-stop too early
        if start_time.elapsed().as_secs() < MIN_DURATION_BEFORE_AUTO_STOP {
            let sys_level = crate::system_audio::get_system_audio_level();
            if sys_level > SILENCE_THRESHOLD {
                ever_had_system_audio = true;
            }
            continue;
        }

        let sys_level = crate::system_audio::get_system_audio_level();
        if sys_level > SILENCE_THRESHOLD {
            ever_had_system_audio = true;
            silence_start = None;
            continue;
        }

        // Only auto-stop if we confirmed this was a call (had system audio at some point)
        if !ever_had_system_audio {
            continue;
        }

        if silence_start.is_none() {
            silence_start = Some(Instant::now());
            log::info!("silence_monitor: system audio went silent, waiting {}s", silence_secs);
        }
        if let Some(start) = silence_start {
            if start.elapsed() >= silence_threshold_duration {
                log::info!("silence_monitor: {}s system audio silence — auto-stopping", silence_secs);
                let _ = app_handle.emit("auto-stop-recording", ());
                return;
            }
        }
    }
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
pub fn stop_recording(app_handle: tauri::AppHandle, state: State<'_, AudioState>) -> Result<(), String> {
    // Stop silence monitor
    if let Ok(mut guard) = state.silence_monitor_stop.lock() {
        if let Some(flag) = guard.take() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    let mut is_recording = state.is_recording.lock().map_err(|e| e.to_string())?;
    if !*is_recording {
        return Ok(());
    }

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
        let mut rt_guard = state.realtime_transcriber.lock().map_err(|e| e.to_string())?;
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

    // Too short — discard immediately, never enter "processing"
    if duration_sec < threshold {
        let mut session_guard = state.current_session.lock().map_err(|e| e.to_string())?;
        if let Some(meta) = session_guard.take() {
            eprintln!("Discarding recording {} (duration {:.2}s < threshold {:.2}s)", meta.id, duration_sec, threshold);
            let dir = storage::get_recording_dir(&meta.id);
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        *is_recording = false;
        crate::mic_audio::reset_audio_level();
        crate::system_audio::reset_system_audio_level();
        return Ok(());
    }

    let save_mix_only = *state.save_mix_only.lock().map_err(|e| e.to_string())?;

    let finalization_handle = {
        let mut session_guard = state.current_session.lock().map_err(|e| e.to_string())?;
        if let Some(in_memory_metadata) = session_guard.take() {
            let id = in_memory_metadata.id.clone();

            let mut metadata = match storage::read_metadata(&id) {
                Ok(m) => m,
                Err(_) => in_memory_metadata
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
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    finalize_recording(&finalization_id, duration_sec, save_mix_only);
                }));
                if result.is_err() {
                    eprintln!("Finalization thread panicked for {}", finalization_id);
                    if let Ok(mut m) = storage::read_metadata(&finalization_id) {
                        m.status = "error".to_string();
                        let _ = storage::write_metadata(&m);
                    }
                }
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

            if let Err(e) = storage::write_metadata(&latest_metadata) {
                eprintln!("Failed to save final metadata for {}: {}", id, e);
            }
        },
        Err(e) => eprintln!("Failed to reload metadata for {}: {}", id, e),
    }
}

/// Start real-time transcription for the given recording.
///
/// Validates that `recording_id` matches the currently active session to prevent
/// stale IDs from hijacking a running transcription. Uses local Whisper model only.
/// No-ops if real-time transcription is disabled in settings.
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
    if !rt_config.realtime_enabled {
        return Ok(());
    }

    {
        let mut guard = state.realtime_transcriber.lock().map_err(|e| e.to_string())?;
        if let Some(mut existing) = guard.take() {
            existing.stop();
        }
    }

    // Realtime transcription is temporarily disabled — whisper-rs was removed
    // and the FluidAudio/Apple streaming providers haven't landed yet. We
    // accept the start call quietly so the recording flow doesn't error;
    // the user just won't see live transcript until streaming is back.
    let _ = (rt_config, app_handle, recording_id);
    Ok(())
}

/// Stop the active real-time transcription session (if any).
#[tauri::command]
pub fn stop_realtime_transcription(state: State<'_, AudioState>) -> Result<(), String> {
    let mut guard = state.realtime_transcriber.lock().map_err(|e| e.to_string())?;
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

