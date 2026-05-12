//! Realtime transcription — placeholder.
//!
//! The whisper-rs-backed implementation was removed; FluidAudio streaming
//! (Parakeet EOU 120M via SlidingWindowAsrManager) and Apple SpeechAnalyzer
//! realtime mode (macOS 26+) will plug back in here once their sidecars ship.
//! Until then, this struct is an inert handle so the recording state machine
//! still compiles.

use std::path::PathBuf;

pub struct LocalTranscriber;

impl LocalTranscriber {
    pub fn start(
        _app_handle: tauri::AppHandle,
        _model_path: PathBuf,
        _recording_id: String,
    ) -> Result<Self, String> {
        Err("Realtime transcription is currently disabled (whisper-rs removed; FluidAudio/Apple streaming pending)".into())
    }

    pub fn stop(&mut self) {}
}

impl Drop for LocalTranscriber {
    fn drop(&mut self) {}
}
