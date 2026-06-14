use rodio::{Decoder, Player};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Playback state
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum PlaybackStatus {
    Stopped,
    Playing,
    Paused,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlaybackState {
    pub status: PlaybackStatus,
    pub current_position_ms: u64,
    pub duration_ms: u64,
    pub recording_id: Option<String>,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            current_position_ms: 0,
            duration_ms: 0,
            recording_id: None,
        }
    }
}

// Thread-safe playback control signals
static STOP_SIGNAL: AtomicBool = AtomicBool::new(false);
static PAUSE_SIGNAL: AtomicBool = AtomicBool::new(false);
static IS_PLAYING: AtomicBool = AtomicBool::new(false);
static CURRENT_POSITION_MS: AtomicU64 = AtomicU64::new(0);
static DURATION_MS: AtomicU64 = AtomicU64::new(0);

lazy_static::lazy_static! {
    static ref CURRENT_RECORDING_ID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    static ref PLAYBACK_THREAD: std::sync::Mutex<Option<thread::JoinHandle<()>>> = std::sync::Mutex::new(None);
}

/// Start audio playback on a dedicated thread
#[tauri::command]
pub fn play_audio(recording_id: String) -> Result<(), String> {
    // Stop any existing playback
    stop_audio()?;

    let recording_dir = crate::storage::get_data_dir().join(&recording_id);
    let mut audio_path = recording_dir.join("audio_mix.ogg");

    if !audio_path.exists() {
        audio_path = recording_dir.join("raw_mic.ogg");
    }

    if !audio_path.exists() {
        return Err("Audio file not found".to_string());
    }

    // Get duration
    let duration_ms = match crate::waveform::get_ogg_file_info(&audio_path) {
        Ok(info) => (info.duration_sec * 1000.0) as u64,
        Err(_) => 0,
    };

    DURATION_MS.store(duration_ms, Ordering::SeqCst);
    CURRENT_POSITION_MS.store(0, Ordering::SeqCst);
    STOP_SIGNAL.store(false, Ordering::SeqCst);
    PAUSE_SIGNAL.store(false, Ordering::SeqCst);
    IS_PLAYING.store(true, Ordering::SeqCst);

    if let Ok(mut id) = CURRENT_RECORDING_ID.lock() {
        *id = Some(recording_id.clone());
    }

    // Spawn high-priority playback thread
    let path = audio_path.clone();
    let handle = thread::Builder::new()
        .name("audio-playback".to_string())
        .spawn(move || {
            if let Err(e) = run_playback(path) {
                eprintln!("Playback error: {}", e);
            }
            IS_PLAYING.store(false, Ordering::SeqCst);
        })
        .map_err(|e| format!("Failed to spawn playback thread: {}", e))?;

    if let Ok(mut thread_guard) = PLAYBACK_THREAD.lock() {
        *thread_guard = Some(handle);
    }

    Ok(())
}

fn run_playback(audio_path: std::path::PathBuf) -> Result<(), String> {
    // Create output stream (must be on this thread)
    let stream_handle = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|e| format!("Audio output error: {}", e))?;

    let sink = Player::connect_new(stream_handle.mixer());

    // Open with larger buffer for smooth playback
    let file = File::open(&audio_path).map_err(|e| format!("File error: {}", e))?;
    let reader = BufReader::with_capacity(256 * 1024, file); // 256KB buffer

    let source = Decoder::new(reader).map_err(|e| format!("Decode error: {}", e))?;

    sink.append(source);
    sink.play();

    let mut last_tick = std::time::Instant::now();
    let mut accumulated_ms = 0u64;
    let duration_ms = DURATION_MS.load(Ordering::SeqCst);

    // Minimal monitoring loop - let rodio do the work
    while !sink.empty() && !STOP_SIGNAL.load(Ordering::Relaxed) {
        let now = std::time::Instant::now();
        let delta = now.duration_since(last_tick).as_millis() as u64;
        last_tick = now;

        // Handle pause
        if PAUSE_SIGNAL.load(Ordering::Relaxed) {
            sink.pause();
            while PAUSE_SIGNAL.load(Ordering::Relaxed) && !STOP_SIGNAL.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
            last_tick = std::time::Instant::now(); // Reset tick timer after pause
            if !STOP_SIGNAL.load(Ordering::Relaxed) {
                sink.play();
            }
        } else {
            accumulated_ms += delta;
        }

        let position = accumulated_ms.min(duration_ms);
        CURRENT_POSITION_MS.store(position, Ordering::Relaxed);

        // Sleep shorter but in a loop to respond quickly to STOP_SIGNAL
        for _ in 0..20 {
            if STOP_SIGNAL.load(Ordering::Relaxed) || PAUSE_SIGNAL.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    sink.stop();
    CURRENT_POSITION_MS.store(0, Ordering::Relaxed);

    Ok(())
}

/// Play a single time range of a recording (a speaker "voice sample" preview).
/// Fire-and-forget: stops any current playback/snippet first, but deliberately
/// does NOT touch the main playback position/duration state, so the recording
/// player UI isn't hijacked by a short preview clip.
#[tauri::command]
pub fn play_audio_segment(recording_id: String, start_ms: u64, end_ms: u64) -> Result<(), String> {
    if end_ms <= start_ms {
        return Err("Invalid segment range".to_string());
    }
    stop_audio()?;

    let recording_dir = crate::storage::get_data_dir().join(&recording_id);
    let mut audio_path = recording_dir.join("audio_mix.ogg");
    if !audio_path.exists() {
        audio_path = recording_dir.join("raw_mic.ogg");
    }
    if !audio_path.exists() {
        return Err("Audio file not found".to_string());
    }

    STOP_SIGNAL.store(false, Ordering::SeqCst);
    let len_ms = end_ms - start_ms;
    let handle = thread::Builder::new()
        .name("audio-snippet".to_string())
        .spawn(move || {
            if let Err(e) = run_segment_playback(audio_path, start_ms, len_ms) {
                eprintln!("Snippet playback error: {}", e);
            }
        })
        .map_err(|e| format!("Failed to spawn snippet thread: {}", e))?;
    if let Ok(mut guard) = PLAYBACK_THREAD.lock() {
        *guard = Some(handle);
    }
    Ok(())
}

/// Decode just the `[start_ms, end_ms)` range of an Ogg/Vorbis file to
/// interleaved f32, stopping as soon as `end_ms` is reached. rodio's vorbis
/// (lewton) decoder has no native seek and `skip_duration` decode-discards the
/// whole prefix (unusable for deep offsets), so we decode the range ourselves
/// and play the exact samples — reliable regardless of where in the file it is.
fn decode_ogg_range(
    audio_path: &std::path::Path,
    start_ms: u64,
    end_ms: u64,
) -> Result<(u16, u32, Vec<f32>), String> {
    use lewton::inside_ogg::OggStreamReader;
    let file = File::open(audio_path).map_err(|e| format!("File error: {}", e))?;
    let mut reader = OggStreamReader::new(file).map_err(|e| format!("Ogg error: {}", e))?;
    let rate = reader.ident_hdr.audio_sample_rate;
    let channels = reader.ident_hdr.audio_channels as usize;
    if channels == 0 {
        return Err("Audio has no channels".to_string());
    }
    let start_frame = (start_ms as f64 / 1000.0 * rate as f64) as usize;
    let end_frame = (end_ms as f64 / 1000.0 * rate as f64) as usize;

    let mut out: Vec<f32> = Vec::with_capacity(end_frame.saturating_sub(start_frame) * channels);
    let mut frame_pos = 0usize;
    while let Ok(Some(pck)) = reader.read_dec_packet_itl() {
        let frames = pck.len() / channels;
        let pck_end = frame_pos + frames;
        if pck_end > start_frame {
            for f in 0..frames {
                let abs = frame_pos + f;
                if abs < start_frame {
                    continue;
                }
                if abs >= end_frame {
                    break;
                }
                for c in 0..channels {
                    out.push(pck[f * channels + c] as f32 / 32768.0);
                }
            }
        }
        frame_pos = pck_end;
        if frame_pos >= end_frame {
            break;
        }
    }
    Ok((channels as u16, rate, out))
}

fn run_segment_playback(
    audio_path: std::path::PathBuf,
    start_ms: u64,
    len_ms: u64,
) -> Result<(), String> {
    let (channels, rate, samples) = decode_ogg_range(&audio_path, start_ms, start_ms + len_ms)?;
    if samples.is_empty() {
        return Ok(());
    }

    let ch = std::num::NonZeroU16::new(channels).ok_or("Audio has no channels")?;
    let sr = std::num::NonZeroU32::new(rate).ok_or("Audio has no sample rate")?;
    let stream_handle = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|e| format!("Audio output error: {}", e))?;
    let sink = Player::connect_new(stream_handle.mixer());
    sink.append(rodio::buffer::SamplesBuffer::new(ch, sr, samples));
    sink.play();
    while !sink.empty() && !STOP_SIGNAL.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(20));
    }
    sink.stop();
    Ok(())
}

/// Pause audio playback
#[tauri::command]
pub fn pause_audio() -> Result<(), String> {
    PAUSE_SIGNAL.store(true, Ordering::SeqCst);
    Ok(())
}

/// Resume audio playback
#[tauri::command]
pub fn resume_audio() -> Result<(), String> {
    PAUSE_SIGNAL.store(false, Ordering::SeqCst);
    Ok(())
}

/// Stop audio playback
#[tauri::command]
pub fn stop_audio() -> Result<(), String> {
    STOP_SIGNAL.store(true, Ordering::SeqCst);
    PAUSE_SIGNAL.store(false, Ordering::SeqCst);

    // Join the thread to make sure it is completely stopped
    let handle = {
        if let Ok(mut thread_guard) = PLAYBACK_THREAD.lock() {
            thread_guard.take()
        } else {
            None
        }
    };
    if let Some(h) = handle {
        let _ = h.join();
    }

    IS_PLAYING.store(false, Ordering::SeqCst);
    CURRENT_POSITION_MS.store(0, Ordering::SeqCst);
    Ok(())
}

/// Seek to position (not fully supported - restarts from beginning)
#[tauri::command]
pub fn seek_audio(_position_ms: u64) -> Result<(), String> {
    // Seeking not supported with basic rodio Decoder
    Ok(())
}

/// Get current playback state
#[tauri::command]
pub fn get_playback_state() -> PlaybackState {
    let is_playing = IS_PLAYING.load(Ordering::Relaxed);
    let is_paused = PAUSE_SIGNAL.load(Ordering::Relaxed);

    let status = if !is_playing {
        PlaybackStatus::Stopped
    } else if is_paused {
        PlaybackStatus::Paused
    } else {
        PlaybackStatus::Playing
    };

    let recording_id = CURRENT_RECORDING_ID
        .lock()
        .map(|id| id.clone())
        .unwrap_or(None);

    PlaybackState {
        status,
        current_position_ms: CURRENT_POSITION_MS.load(Ordering::Relaxed),
        duration_ms: DURATION_MS.load(Ordering::Relaxed),
        recording_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoding a fixed time range must return ~the right number of samples
    /// (rate × channels × seconds) — proving the slice lands at the requested
    /// offset, not the file start. Skips when no real recording is present.
    #[test]
    fn decode_ogg_range_lands_at_offset() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let data = std::path::Path::new(&home).join("nbp-data");
        if !data.is_dir() {
            return;
        }
        let ogg = std::fs::read_dir(&data).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path().join("audio_mix.ogg"))
                .find(|p| p.exists())
        });
        let Some(ogg) = ogg else {
            eprintln!("decode_ogg_range: no audio_mix.ogg to test");
            return;
        };

        // 2-second window starting 1 s in.
        let (ch, rate, samples) = decode_ogg_range(&ogg, 1000, 3000).expect("decode");
        assert!(!samples.is_empty(), "decoded no samples");
        let expected = rate as f64 * ch as f64 * 2.0;
        let got = samples.len() as f64;
        let drift = (got - expected).abs() / expected;
        assert!(
            drift < 0.15,
            "got {got} samples, expected ~{expected} (drift {drift:.2})"
        );
        eprintln!(
            "decode_ogg_range: {} samples, {ch}ch {rate}Hz for a 2s window",
            samples.len()
        );
    }
}
