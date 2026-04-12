use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::Path;
use lewton::inside_ogg::OggStreamReader;

const WAVEFORM_SAMPLES: usize = 1000;

/// Audio file metadata from actual OGG file
#[derive(Clone, Debug)]
pub struct OggFileInfo {
    pub duration_sec: f64,
}

/// Get actual audio info from OGG file (duration calculated from sample count)
pub fn get_ogg_file_info(path: &Path) -> Result<OggFileInfo, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open audio: {}", e))?;
    let mut ogg_reader = OggStreamReader::new(file).map_err(|e| format!("Failed to parse OGG: {}", e))?;

    let sample_rate = ogg_reader.ident_hdr.audio_sample_rate;

    // Count total samples by decoding
    let mut total_frames: u64 = 0;

    while let Some(packet) = ogg_reader.read_dec_packet_generic::<Vec<Vec<i16>>>()
        .map_err(|e| format!("Decode error: {}", e))?
    {
        if !packet.is_empty() {
            total_frames += packet[0].len() as u64;
        }
    }

    let duration_sec = total_frames as f64 / sample_rate as f64;

    Ok(OggFileInfo {
        duration_sec,
    })
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WaveformData {
    pub duration_ms: u64,
    pub samples: Vec<f32>,  // Normalized 0-1
    pub sample_count: usize,
}

/// Generate waveform data from an audio file
#[tauri::command]
pub fn get_waveform_data(recording_id: String) -> Result<WaveformData, String> {
    let recording_dir = crate::storage::get_data_dir().join(&recording_id);
    let mut audio_path = recording_dir.join("audio_mix.ogg");

    if !audio_path.exists() {
        audio_path = recording_dir.join("raw_mic.ogg");
    }

    if !audio_path.exists() {
        return Err("Audio file not found".to_string());
    }

    // Check for cached waveform
    let cache_path = recording_dir.join("waveform.json");
    if cache_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&cache_path) {
            if let Ok(data) = serde_json::from_str::<WaveformData>(&content) {
                return Ok(data);
            }
        }
    }

    // Generate waveform
    let data = generate_waveform(&audio_path)?;

    // Cache it
    if let Ok(json) = serde_json::to_string(&data) {
        let _ = std::fs::write(&cache_path, json);
    }

    Ok(data)
}

fn generate_waveform(audio_path: &std::path::Path) -> Result<WaveformData, String> {
    let file = File::open(audio_path).map_err(|e| format!("Failed to open audio: {}", e))?;
    let mut ogg_reader = OggStreamReader::new(file).map_err(|e| format!("Failed to parse OGG: {}", e))?;

    let sample_rate = ogg_reader.ident_hdr.audio_sample_rate;
    let channels = ogg_reader.ident_hdr.audio_channels as usize;

    // Collect all samples and find peaks
    let mut all_samples: Vec<f32> = Vec::new();

    while let Some(packet) = ogg_reader.read_dec_packet_generic::<Vec<Vec<i16>>>()
        .map_err(|e| format!("Decode error: {}", e))?
    {
        if packet.is_empty() { continue; }

        let frames = packet[0].len();
        for i in 0..frames {
            // Mix channels to mono and convert to float
            let mut sum: f32 = 0.0;
            for ch in 0..channels {
                if ch < packet.len() && i < packet[ch].len() {
                    sum += packet[ch][i] as f32 / 32768.0;
                }
            }
            all_samples.push((sum / channels as f32).abs());
        }
    }

    if all_samples.is_empty() {
        return Ok(WaveformData {
            duration_ms: 0,
            samples: vec![0.0; WAVEFORM_SAMPLES],
            sample_count: WAVEFORM_SAMPLES,
        });
    }

    let total_samples = all_samples.len();
    let duration_ms = (total_samples as f64 / sample_rate as f64 * 1000.0) as u64;

    // Downsample to target resolution
    let samples_per_bin = total_samples / WAVEFORM_SAMPLES;
    let mut waveform: Vec<f32> = Vec::with_capacity(WAVEFORM_SAMPLES);

    for i in 0..WAVEFORM_SAMPLES {
        let start = i * samples_per_bin;
        let end = ((i + 1) * samples_per_bin).min(total_samples);

        if start < end {
            // Use peak value in this bin
            let peak = all_samples[start..end]
                .iter()
                .cloned()
                .fold(0.0f32, |a, b| a.max(b));
            waveform.push(peak);
        } else {
            waveform.push(0.0);
        }
    }

    // Normalize to 0-1 range
    let max_val = waveform.iter().cloned().fold(0.0f32, |a, b| a.max(b));
    if max_val > 0.0 {
        for sample in &mut waveform {
            *sample /= max_val;
        }
    }

    Ok(WaveformData {
        duration_ms,
        samples: waveform,
        sample_count: WAVEFORM_SAMPLES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_waveform_data_structure() {
        let data = WaveformData {
            duration_ms: 60000,
            samples: vec![0.5; 1000],
            sample_count: 1000,
        };
        assert_eq!(data.duration_ms, 60000);
        assert_eq!(data.samples.len(), 1000);
        assert_eq!(data.sample_count, 1000);
    }
}
