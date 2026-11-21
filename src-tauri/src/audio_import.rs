//! Import external audio without modifying the original. Publish only complete
//! recordings; playback and ASR keep using the normal audio_mix.ogg path.
use crate::storage::{self, AudioFiles, AudioInfo, RecordingMetadata};
use chrono::Utc;
use rodio::{Decoder, Source};
use std::fs::{self, File};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

mod native;
#[cfg(target_os = "macos")]
pub(crate) mod promised;

const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "aac", "flac", "ogg", "qta"];

struct StagingDir(PathBuf);

impl Drop for StagingDir {
    fn drop(&mut self) {
        // Also cleans partial copies/encodes on every error path.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tauri::command]
pub async fn import_audio_file(path: String) -> Result<RecordingMetadata, String> {
    import_path(PathBuf::from(path)).await
}

// Native file promises and JS file drops share one encoder slot. A batch of
// promised exports must not launch an encoder (and its buffers) per file.
static IMPORT_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

async fn import_path(path: PathBuf) -> Result<RecordingMetadata, String> {
    let _slot = IMPORT_SLOT.acquire().await.map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || import_into(&path, &storage::get_data_dir()))
        .await
        .map_err(|e| format!("Audio import failed: {e}"))?
}

fn import_into(source: &Path, data_dir: &Path) -> Result<RecordingMetadata, String> {
    if !source.is_absolute() || !source.is_file() {
        return Err("Drop a local audio file, not a folder.".into());
    }
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return Err("Supported audio: WAV, MP3, M4A, AAC, FLAC, OGG, QTA.".into());
    }
    if fs::metadata(source).map_err(|e| e.to_string())?.len() == 0 {
        return Err("The audio file is empty.".into());
    }

    let id = Uuid::new_v4().to_string();
    // Nested staging keeps list_recordings and startup repair from observing
    // partial metadata. Rename stays on the same filesystem.
    let staging = StagingDir(data_dir.join(".imports").join(&id));
    fs::create_dir_all(&staging.0).map_err(|e| format!("Cannot create import folder: {e}"))?;
    let original = staging.0.join(format!("source.{extension}"));
    fs::copy(source, &original).map_err(|e| format!("Cannot copy audio: {e}"))?;
    let mix = staging.0.join("audio_mix.ogg");
    let audio = if extension == "qta" || extension == "m4a" {
        native::encode_qta(&original, &mix)?
    } else {
        encode_mix(&original, &mix)?
    };

    let metadata = RecordingMetadata {
        id: id.clone(),
        created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        title: source
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        status: "ready".into(),
        audio: AudioFiles {
            mic: None,
            system: None,
            mix: Some(audio),
        },
        health: None,
        pipelines: vec![],
        transcript_preview: None,
        app_bundle_id: None,
        source: "import".into(),
        app_friendly_name: None,
        attendees: vec![],
        calendar_event_id: None,
        // An import's timestamp is not the time its audio was captured.
        calendar_matched: true,
    };
    let contents = serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?;
    fs::write(staging.0.join("metadata.json"), contents).map_err(|e| e.to_string())?;
    fs::rename(&staging.0, data_dir.join(&id))
        .map_err(|e| format!("Cannot finish audio import: {e}"))?;
    storage::invalidate_list_cache();
    Ok(metadata)
}

fn encode_mix(source: &Path, output: &Path) -> Result<AudioInfo, String> {
    let file = File::open(source).map_err(|e| e.to_string())?;
    let mut decoder = Decoder::try_from(file)
        .map_err(|e| format!("Cannot decode audio (unsupported codec or damaged file): {e}"))?;
    let channels = decoder.channels().get();
    let rate = decoder.sample_rate().get();
    if channels > 2 {
        return Err("Only mono and stereo audio files are supported.".into());
    }
    let mut encoder = VorbisEncoderBuilder::new_with_serial(
        NonZeroU32::new(rate).ok_or("Invalid audio sample rate")?,
        NonZeroU8::new(channels as u8).ok_or("Invalid audio channel count")?,
        File::create(output).map_err(|e| e.to_string())?,
        0,
    )
    .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: 0.4,
    })
    .build()
    .map_err(|e| e.to_string())?;

    let mut frames = 0u64;
    // Bounded memory even for day-long recorder dumps.
    loop {
        let mut planar = vec![Vec::with_capacity(4096); channels as usize];
        for _ in 0..4096 {
            let Some(first) = decoder.next() else { break };
            planar[0].push(first);
            for channel in planar.iter_mut().skip(1) {
                channel.push(decoder.next().ok_or("Incomplete audio frame")?);
            }
        }
        let count = planar[0].len();
        if count == 0 {
            break;
        }
        frames += count as u64;
        encoder
            .encode_audio_block(&planar)
            .map_err(|e| e.to_string())?;
    }
    if frames == 0 {
        return Err("The file contains no decodable audio.".into());
    }
    encoder.finish().map_err(|e| e.to_string())?;
    Ok(AudioInfo {
        file: "audio_mix.ogg".into(),
        duration_sec: frames as f64 / rate as f64,
        sample_rate: rate,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(path: &Path, channels: u16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for n in 0..16000 * channels as usize {
            writer
                .write_sample(((n as f32 * 0.1).sin() * 4000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn imports_mono_and_stereo_preserving_originals() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        for channels in [1, 2] {
            let source = temp.path().join(format!("Тест {channels}.WAV"));
            wav(&source, channels);
            let original = fs::read(&source).unwrap();
            let meta = import_into(&source, &data).unwrap();
            assert_eq!(meta.source, "import");
            assert_eq!(meta.status, "ready");
            assert_eq!(meta.title, format!("Тест {channels}"));
            let audio = meta.audio.mix.unwrap();
            assert_eq!(audio.channels, channels);
            assert_eq!(audio.sample_rate, 16000);
            assert!((audio.duration_sec - 1.0).abs() < 0.001);
            let dir = data.join(&meta.id);
            let decoded =
                Decoder::try_from(File::open(dir.join("audio_mix.ogg")).unwrap()).unwrap();
            assert_eq!(decoded.channels().get(), channels);
            assert!(decoded.count() > 0);
            assert_eq!(fs::read(dir.join("source.wav")).unwrap(), original);
            assert_eq!(fs::read(&source).unwrap(), original);
            let saved: RecordingMetadata =
                serde_json::from_slice(&fs::read(dir.join("metadata.json")).unwrap()).unwrap();
            assert_eq!(saved.id, meta.id);
        }
    }

    #[test]
    fn rejects_invalid_files_without_publishing_recordings() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        for name in ["bad.wav", "empty.mp3", "notes.txt"] {
            let source = temp.path().join(name);
            fs::write(
                &source,
                if name == "empty.mp3" {
                    b"".as_slice()
                } else {
                    b"not audio".as_slice()
                },
            )
            .unwrap();
            assert!(import_into(&source, &data).is_err());
        }
        assert!(import_into(temp.path(), &data).is_err());
        assert!(import_into(&temp.path().join("missing.wav"), &data).is_err());
        assert_eq!(fs::read_dir(data.join(".imports")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&data).unwrap().count(), 1);
    }

    #[test]
    fn repeat_imports_have_independent_ids() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("audio.wav");
        wav(&source, 1);
        let data = temp.path().join("data");
        let a = import_into(&source, &data).unwrap();
        let b = import_into(&source, &data).unwrap();
        assert_ne!(a.id, b.id);
        assert!(data.join(a.id).join("audio_mix.ogg").is_file());
        assert!(data.join(b.id).join("audio_mix.ogg").is_file());
    }

    #[test]
    fn imports_qta_aac_and_apple_lossless_without_retaining_temporary_wav() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.wav");
        wav(&source, 1);
        let data = temp.path().join("data");
        for codec in ["aac ", "alac"] {
            // Real MPEG-4 containers carrying each Apple Voice Memos codec,
            // not WAV bytes with a renamed extension. Both use the native
            // decoder, including ALAC which rodio cannot decode here.
            let encoded = temp.path().join(format!("{codec}.m4a"));
            let result = std::process::Command::new("/usr/bin/afconvert")
                .args(["-f", "m4af", "-d", codec])
                .arg(&source)
                .arg(&encoded)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let m4a = import_into(&encoded, &data).unwrap();
            assert!(m4a.audio.mix.unwrap().duration_sec > 0.9);
            let qta = temp.path().join(format!("New Recording 65 {codec}.qta"));
            fs::rename(encoded, &qta).unwrap();
            let bytes = fs::read(&qta).unwrap();
            let metadata = import_into(&qta, &data).unwrap();
            let dir = data.join(&metadata.id);
            assert!(metadata.audio.mix.unwrap().duration_sec > 0.9);
            assert_eq!(fs::read(dir.join("source.qta")).unwrap(), bytes);
            assert_eq!(fs::read(qta).unwrap(), bytes);
            assert!(!dir.join("decoded.wav").exists());
            assert!(
                Decoder::try_from(File::open(dir.join("audio_mix.ogg")).unwrap())
                    .unwrap()
                    .count()
                    > 0
            );
        }
    }

    #[test]
    fn corrupt_qta_is_rejected_without_a_partial_recording() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("bad.qta");
        fs::write(&source, b"not an audio container").unwrap();
        let data = temp.path().join("data");
        assert!(import_into(&source, &data).is_err());
        assert_eq!(fs::read_dir(data.join(".imports")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(data).unwrap().count(), 1);
    }

    #[test]
    fn real_qta_import_preserves_full_duration() {
        // Opt-in regression against a local user-provided file. Nothing is
        // published into the user's library or transcribed by this test.
        let Ok(path) = std::env::var("NBP_TEST_QTA") else {
            return;
        };
        let expected: f64 = std::env::var("NBP_TEST_QTA_SECONDS")
            .expect("Set the source duration from afinfo")
            .parse()
            .unwrap();
        let source = Path::new(&path);
        let before = fs::metadata(source).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let metadata = import_into(source, temp.path()).unwrap();
        let actual = metadata.audio.mix.unwrap().duration_sec;
        assert!(
            (actual - expected).abs() < 0.1,
            "Expected {expected}s, imported {actual}s"
        );
        let after = fs::metadata(source).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        let mut names: Vec<_> = fs::read_dir(temp.path().join(&metadata.id))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["audio_mix.ogg", "metadata.json", "source.qta"]);
        println!(
            "QTA duration: source={expected}s, imported={actual}s, elapsed={:.3}s",
            started.elapsed().as_secs_f64()
        );
    }
}
