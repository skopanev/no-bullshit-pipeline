use crate::config::{TranscriptionProvider, load_settings};
use crate::storage::{get_data_dir, read_metadata};
use crate::transcript_migration::{TranscriptMetadata, TranscriptSource};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Mutex;
use tauri::Emitter;
use tauri_plugin_shell::ShellExt;

struct SidecarGuard(Option<tauri_plugin_shell::process::CommandChild>);

impl Drop for SidecarGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            let _ = child.kill();
        }
    }
}

pub struct TranscriptionState {
    pub active_ids: Mutex<HashSet<String>>,
}

impl Default for TranscriptionState {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptionState {
    pub fn new() -> Self {
        Self {
            active_ids: Mutex::new(HashSet::new()),
        }
    }
}

#[tauri::command]
pub fn is_transcribing(recording_id: String, state: tauri::State<'_, TranscriptionState>) -> bool {
    state
        .active_ids
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&recording_id)
}

#[derive(Deserialize, Debug)]
struct FluidAudioOutput {
    model: String,
    text: String,
    /// v2 diarization computed in the same sidecar run (single ASR pass) —
    /// persisted as diarization.json alongside the transcript.
    #[serde(rename = "diarV2", default)]
    diar_v2: Option<DiarV2Payload>,
}

#[derive(Deserialize, Debug)]
struct DiarV2Payload {
    #[serde(rename = "speakerCount")]
    speaker_count: usize,
    segments: Vec<crate::diarization::DiarSegment>,
    centroids: Vec<Vec<f32>>,
}

/// JSON transcript stored as transcript.json (source of truth)
#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct TranscriptJson {
    source: TranscriptSource,
    model: String,
    created_at: String,
    duration_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Clone, Serialize)]
struct TranscriptionProgress {
    recording_id: String,
    stage: String,
    percent: u32,
}

/// FluidAudio sidecar args selecting the active engine + code-switch recovery,
/// shared by the recording and dictation paths so the two can't drift. Excludes
/// the wav path and `--no-diarize` (those differ per caller).
pub fn fluidaudio_engine_args(settings: &crate::config::AppSettings) -> Vec<String> {
    let t = &settings.transcription;
    if matches!(t.provider, TranscriptionProvider::Qwen3) {
        return vec![
            "--engine".into(),
            "qwen3".into(),
            "--variant".into(),
            t.qwen3_variant.clone(),
        ];
    }
    // Parakeet (FluidAudio): code-switch recovery if enabled + a word list exists.
    if t.translit_lang != "off" {
        let vp = crate::vocab::vocab_path();
        if vp.exists() {
            return vec![
                "--translit-all".into(),
                "--translit-threshold".into(),
                format!("{}", t.translit_threshold),
                "--translit-min-len".into(),
                format!("{}", t.translit_min_len),
                "--vocab".into(),
                vp.to_string_lossy().to_string(),
            ];
        }
    }
    Vec::new()
}

#[tauri::command]
pub async fn transcribe_recording(
    app_handle: tauri::AppHandle,
    recording_id: String,
    transcription_state: tauri::State<'_, TranscriptionState>,
) -> Result<String, String> {
    let _update_work = crate::update_gate::work()?;
    {
        let mut active = transcription_state
            .active_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if active.contains(&recording_id) {
            return Ok("__already_running__".to_string());
        }
        active.insert(recording_id.clone());
    }

    let result = transcribe_recording_inner(&app_handle, &recording_id).await;

    {
        let mut active = transcription_state
            .active_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        active.remove(&recording_id);
    }

    result
}

async fn transcribe_recording_inner(
    app_handle: &tauri::AppHandle,
    recording_id: &str,
) -> Result<String, String> {
    let recording_id = recording_id.to_string();
    let settings = load_settings();
    if !settings.transcription.enabled {
        return Err("Transcription is disabled in settings".to_string());
    }

    let recording_dir = get_data_dir().join(&recording_id);
    let mut audio_path = recording_dir.join("audio_mix.ogg");
    if !audio_path.exists() {
        audio_path = recording_dir.join("raw_mic.ogg");
    }
    // Set inside the FluidAudio arm when clean stems exist: skip mix diarization
    // there and run the source-split diarize pass after the transcript is saved.
    let mut split_diar_after = false;

    if !audio_path.exists() {
        return Err("Audio file not found".to_string());
    }

    // Clear existing transcript before re-transcription so stale data is never visible
    let _ = std::fs::remove_file(recording_dir.join("transcript.json"));
    let _ = std::fs::remove_file(recording_dir.join("transcript.md"));

    let provider = settings.transcription.provider.clone();

    // Shared metadata fields. Cloud STT providers were killed in the
    // asr-bakeoff branch — only on-device engines remain. `Unknown` is a
    // serde catch-all for stale config strings (see config.rs); we map it
    // to FluidAudio so old settings.json files still work.
    let source = match provider {
        TranscriptionProvider::FluidAudio
        | TranscriptionProvider::Qwen3
        | TranscriptionProvider::Unknown => TranscriptSource::Fluidaudio,
        TranscriptionProvider::AppleSpeech => TranscriptSource::Apple,
    };

    let _ = app_handle.emit(
        "transcription_progress",
        TranscriptionProgress {
            recording_id: recording_id.clone(),
            stage: "Starting".to_string(),
            percent: 0,
        },
    );

    let transcript_json = match provider {
        TranscriptionProvider::AppleSpeech => {
            let wav_path = recording_dir.join("temp_transcription.wav");
            convert_ogg_to_wav(&audio_path, &wav_path)?;

            let (mut rx, child) = app_handle
                .shell()
                .sidecar("apple-speech-sidecar")
                .map_err(|e| format!("Failed to create sidecar command: {}", e))?
                .arg(wav_path.to_str().ok_or("Invalid WAV path")?)
                .arg("--lang")
                .arg(&settings.transcription.apple_locale)
                .spawn()
                .map_err(|e| format!("Failed to spawn Apple Speech sidecar: {}", e))?;
            let _guard = SidecarGuard(Some(child));

            let mut stdout_buf = Vec::new();
            let mut stderr_buf = String::new();
            let mut exit_code: Option<i32> = None;

            let timeout_duration = std::time::Duration::from_secs(600);
            let start = std::time::Instant::now();

            while let Some(event) = rx.recv().await {
                if start.elapsed() > timeout_duration {
                    return Err("Apple Speech sidecar timed out after 10 minutes".to_string());
                }
                use tauri_plugin_shell::process::CommandEvent;
                match event {
                    CommandEvent::Stdout(data) => stdout_buf.extend_from_slice(&data),
                    CommandEvent::Stderr(data) => {
                        let line = String::from_utf8_lossy(&data);
                        stderr_buf.push_str(&line);
                        for l in line.lines() {
                            if let Some(rest) = l.strip_prefix("PROGRESS:") {
                                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                                if parts.len() == 2
                                    && let Ok(pct) = parts[1].parse::<u32>()
                                {
                                    let _ = app_handle.emit(
                                        "transcription_progress",
                                        TranscriptionProgress {
                                            recording_id: recording_id.clone(),
                                            stage: parts[0].to_string(),
                                            percent: pct,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    CommandEvent::Terminated(payload) => {
                        exit_code = payload.code;
                        break;
                    }
                    _ => {}
                }
            }

            let _ = std::fs::remove_file(&wav_path);

            for line in stderr_buf.lines() {
                if !line.starts_with("PROGRESS:") && !line.is_empty() {
                    eprintln!("{}", line);
                }
            }

            if exit_code != Some(0) {
                return Err(format!("Apple Speech sidecar failed: {}", stderr_buf));
            }

            let stdout = String::from_utf8_lossy(&stdout_buf);
            let out: FluidAudioOutput = serde_json::from_str(&stdout)
                .map_err(|e| format!("Failed to parse Apple Speech output: {}", e))?;

            let duration_sec = read_metadata(&recording_id)
                .ok()
                .and_then(|m| m.audio.mix.as_ref().map(|a| a.duration_sec))
                .unwrap_or(0.0);

            TranscriptJson {
                source,
                model: out.model,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                duration_sec,
                language: Some("auto".to_string()),
                text: Some(out.text),
            }
        }
        TranscriptionProvider::FluidAudio
        | TranscriptionProvider::Qwen3
        | TranscriptionProvider::Unknown => {
            let wav_path = recording_dir.join("temp_transcription.wav");
            convert_ogg_to_wav(&audio_path, &wav_path)?;

            let mut fa_cmd = app_handle
                .shell()
                .sidecar("fluidaudio-sidecar")
                .map_err(|e| format!("Failed to create sidecar command: {}", e))?
                .arg(wav_path.to_str().ok_or("Invalid WAV path")?);
            // Speaker labels (diarization) are opt-out via settings. Recordings
            // default to on; disabling skips the diarizer for a faster,
            // single-speaker transcript. (Quick Dictate always passes --no-diarize.)
            // When BOTH stems exist, in-transcribe (mix) diarization is skipped
            // too — the cleaner source-split pass runs right after the
            // transcript is saved (see the tail of this function).
            let has_stems = recording_dir.join("raw_system.ogg").exists()
                && recording_dir.join("raw_mic.ogg").exists();
            if settings.transcription.diarize && has_stems {
                split_diar_after = true;
            }
            if !settings.transcription.diarize || split_diar_after {
                fa_cmd = fa_cmd.arg("--no-diarize");
            }
            // Engine selection + code-switch recovery — shared with the dictation
            // path via fluidaudio_engine_args so the two never drift.
            for a in fluidaudio_engine_args(&settings) {
                fa_cmd = fa_cmd.arg(a);
            }
            let (mut rx, child) = fa_cmd
                .spawn()
                .map_err(|e| format!("Failed to spawn FluidAudio sidecar: {}", e))?;
            let _guard = SidecarGuard(Some(child));

            let mut stdout_buf = Vec::new();
            let mut stderr_buf = String::new();
            let mut exit_code: Option<i32> = None;

            let timeout_duration = std::time::Duration::from_secs(600);
            let start = std::time::Instant::now();

            while let Some(event) = rx.recv().await {
                if start.elapsed() > timeout_duration {
                    return Err("FluidAudio sidecar timed out after 10 minutes".to_string());
                }

                use tauri_plugin_shell::process::CommandEvent;
                match event {
                    CommandEvent::Stdout(data) => {
                        stdout_buf.extend_from_slice(&data);
                    }
                    CommandEvent::Stderr(data) => {
                        let line = String::from_utf8_lossy(&data);
                        stderr_buf.push_str(&line);

                        for l in line.lines() {
                            if let Some(rest) = l.strip_prefix("PROGRESS:") {
                                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                                if parts.len() == 2
                                    && let Ok(pct) = parts[1].parse::<u32>()
                                {
                                    let _ = app_handle.emit(
                                        "transcription_progress",
                                        TranscriptionProgress {
                                            recording_id: recording_id.clone(),
                                            stage: parts[0].to_string(),
                                            percent: pct,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    CommandEvent::Terminated(payload) => {
                        exit_code = payload.code;
                        break;
                    }
                    _ => {}
                }
            }

            for line in stderr_buf.lines() {
                if !line.starts_with("PROGRESS:") && !line.is_empty() {
                    eprintln!("{}", line);
                }
            }

            if exit_code != Some(0) {
                let _ = std::fs::remove_file(&wav_path);
                return Err(format!("FluidAudio sidecar failed: {}", stderr_buf));
            }

            let stdout = String::from_utf8_lossy(&stdout_buf);
            let fa_output: FluidAudioOutput = serde_json::from_str(&stdout)
                .inspect_err(|_| {
                    let _ = std::fs::remove_file(&wav_path);
                })
                .map_err(|e| format!("Failed to parse FluidAudio output: {}", e))?;

            let duration_sec = read_metadata(&recording_id)
                .ok()
                .and_then(|m| m.audio.mix.as_ref().map(|a| a.duration_sec))
                .unwrap_or(0.0);

            // Same-run diarization (Speaker labels on): persist diarization.json
            // from the single ASR pass — no separate Diarize needed. The temp
            // wav is removed only AFTER this, so voice samples can be cut.
            if let Some(diar) = fa_output.diar_v2 {
                crate::diarization::store_from_transcribe(
                    app_handle,
                    &recording_id,
                    diar.speaker_count,
                    diar.segments,
                    &diar.centroids,
                    Some(&wav_path),
                );
            }
            let _ = std::fs::remove_file(&wav_path);

            TranscriptJson {
                source,
                model: fa_output.model,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                duration_sec,
                language: Some("auto".to_string()),
                text: Some(fa_output.text),
            }
        }
    };

    // Save raw JSON as source of truth
    let json_str = serde_json::to_string_pretty(&transcript_json)
        .map_err(|e| format!("Failed to serialize transcript JSON: {}", e))?;
    let json_path = recording_dir.join("transcript.json");
    let temp_path = json_path.with_extension("json.tmp");
    std::fs::write(&temp_path, &json_str)
        .map_err(|e| format!("Failed to write transcript: {}", e))?;
    std::fs::rename(&temp_path, &json_path)
        .map_err(|e| format!("Failed to finalize transcript: {}", e))?;

    // Populate the metadata `transcript_preview` so the recordings list can
    // render a hint of what the call was about without re-reading the whole
    // transcript on every refresh. ~200 chars, broken at word boundary.
    let preview_text = render_transcript_from_json(&transcript_json);
    let preview = truncate_preview(&preview_text, 200);
    if let Ok(mut meta) = crate::storage::read_metadata(&recording_id)
        && meta.transcript_preview.as_deref() != Some(preview.as_str())
    {
        meta.transcript_preview = if preview.is_empty() {
            None
        } else {
            Some(preview)
        };
        if let Err(e) = crate::storage::write_metadata(&meta) {
            log::warn!(
                "transcription: failed to write preview for {}: {}",
                recording_id,
                e
            );
        }
    }

    let _ = app_handle.emit(
        "transcription_progress",
        TranscriptionProgress {
            recording_id: recording_id.clone(),
            stage: "Done".to_string(),
            percent: 100,
        },
    );

    // Clean stems present → kick off the source-split diarization now that the
    // transcript is saved. Fire-and-forget: the diarization job has its own
    // status file + progress events, and must not delay the transcript.
    if split_diar_after {
        let app = app_handle.clone();
        let id = recording_id.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::diarization::diarize_recording(app, id).await {
                eprintln!("post-transcribe diarization failed: {e}");
            }
        });
    }

    // Return rendered text for immediate UI display
    Ok(preview_text)
}

/// Compress a transcript to a short preview snippet. Collapses whitespace,
/// truncates to `max` chars at a word boundary, appends an ellipsis if cut.
fn truncate_preview(text: &str, max: usize) -> String {
    let cleaned: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max {
        return cleaned;
    }
    // Char-safe slice to ~max chars.
    let mut end = 0;
    for (i, _) in cleaned.char_indices().take(max) {
        end = i;
    }
    // Walk back to the last space so we don't cut a word in half.
    let cut = &cleaned[..end];
    let word_end = cut.rfind(' ').unwrap_or(end);
    format!("{}…", cleaned[..word_end].trim_end())
}

/// Render text from a TranscriptJson struct
pub(crate) fn render_transcript_from_json(tj: &TranscriptJson) -> String {
    tj.text.clone().unwrap_or_default()
}

/// Render transcript text on the fly from transcript.json (with .md fallback)
fn render_transcript_text(recording_id: &str) -> Option<String> {
    let recording_dir = get_data_dir().join(recording_id);
    let json_path = recording_dir.join("transcript.json");

    if json_path.exists()
        && let Ok(content) = std::fs::read_to_string(&json_path)
        && let Ok(tj) = serde_json::from_str::<TranscriptJson>(&content)
    {
        return Some(render_transcript_from_json(&tj));
    }

    // Fallback: legacy transcript.md
    let md_path = recording_dir.join("transcript.md");
    if md_path.exists()
        && let Ok(content) = std::fs::read_to_string(&md_path)
    {
        if content.starts_with("---") {
            let parts: Vec<&str> = content.splitn(3, "---").collect();
            if parts.len() >= 3 {
                return Some(parts[2].trim().to_string());
            }
        }
        return Some(content);
    }

    None
}

#[tauri::command]
pub async fn get_transcript(recording_id: String) -> Result<Option<String>, String> {
    Ok(render_transcript_text(&recording_id))
}

/// Export transcript as markdown with frontmatter (for Save button)
#[tauri::command]
pub async fn export_transcript_md(
    app_handle: tauri::AppHandle,
    recording_id: String,
) -> Result<(), String> {
    let recording_dir = get_data_dir().join(&recording_id);
    let json_path = recording_dir.join("transcript.json");

    // Build markdown content
    let md_content = if json_path.exists() {
        let content = std::fs::read_to_string(&json_path)
            .map_err(|e| format!("Failed to read transcript: {}", e))?;
        let tj: TranscriptJson = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse transcript: {}", e))?;

        let body = render_transcript_from_json(&tj);
        let metadata = TranscriptMetadata {
            source: tj.source,
            model: tj.model,
            created_at: tj.created_at,
            duration_sec: tj.duration_sec,
            language: tj.language,
        };
        let frontmatter = serde_yaml::to_string(&metadata)
            .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
        format!("---\n{}---\n\n{}", frontmatter, body)
    } else {
        // Fallback: legacy .md
        let md_path = recording_dir.join("transcript.md");
        if md_path.exists() {
            std::fs::read_to_string(&md_path)
                .map_err(|e| format!("Failed to read transcript: {}", e))?
        } else {
            return Err("No transcript found".to_string());
        }
    };

    // Use Tauri save dialog (async via oneshot channel to avoid blocking tokio worker)
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app_handle
        .dialog()
        .file()
        .add_filter("Markdown", &["md"])
        .set_file_name("transcript.md")
        .save_file(move |file_path| {
            let _ = tx.send(file_path);
        });

    let file_path = rx.await.map_err(|_| "Dialog channel closed unexpectedly")?;

    if let Some(path) = file_path {
        let dest = path.as_path().ok_or("Invalid file path selected")?;
        std::fs::write(dest, &md_content).map_err(|e| format!("Failed to save file: {}", e))?;
    }

    Ok(())
}

pub fn convert_ogg_to_wav(
    ogg_path: &std::path::Path,
    wav_path: &std::path::Path,
) -> Result<(), String> {
    convert_ogg_to_wav_progress(ogg_path, wav_path, 0.0, &mut |_| {})
}

/// Decode OGG → mono WAV at the SOURCE sample rate — no resampling. For
/// consumers that resample anyway (the FluidAudio sidecar runs everything
/// through AVFoundation/Accelerate), the expensive sinc resample here was
/// pure waste; skipping it makes conversion several times faster.
pub fn convert_ogg_to_wav_native_progress(
    ogg_path: &std::path::Path,
    wav_path: &std::path::Path,
    total_secs: f64,
    on_pct: &mut dyn FnMut(u32),
) -> Result<(), String> {
    use hound::{WavSpec, WavWriter};
    use lewton::inside_ogg::OggStreamReader;
    use std::fs::File;

    let ogg_file = File::open(ogg_path).map_err(|e| e.to_string())?;
    let mut ogg_reader = OggStreamReader::new(ogg_file).map_err(|e| e.to_string())?;
    let src_rate = ogg_reader.ident_hdr.audio_sample_rate;
    let src_channels = ogg_reader.ident_hdr.audio_channels as u16;

    let spec = WavSpec {
        channels: 1,
        sample_rate: src_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav_writer = WavWriter::create(wav_path, spec).map_err(|e| e.to_string())?;

    let mut written: u64 = 0;
    let mut last_pct: u32 = 0;
    while let Some(packet) = ogg_reader
        .read_dec_packet_generic::<Vec<Vec<i16>>>()
        .map_err(|e| e.to_string())?
    {
        if packet.is_empty() {
            continue;
        }
        let frames = packet[0].len();
        for i in 0..frames {
            let mut sum: i32 = 0;
            for ch in 0..src_channels as usize {
                if ch < packet.len() && i < packet[ch].len() {
                    sum += packet[ch][i] as i32;
                }
            }
            wav_writer
                .write_sample((sum / src_channels as i32) as i16)
                .map_err(|e| e.to_string())?;
        }
        written += frames as u64;
        if total_secs > 0.0 {
            let decoded = written as f64 / src_rate as f64;
            let pct = ((decoded / total_secs) * 100.0).min(100.0) as u32;
            if pct != last_pct {
                last_pct = pct;
                on_pct(pct);
            }
        }
    }
    wav_writer.finalize().map_err(|e| e.to_string())?;
    Ok(())
}

/// Same conversion with a progress callback (0-100). `total_secs` (e.g. from
/// recording metadata) drives the estimate; pass 0.0 for no progress.
pub fn convert_ogg_to_wav_progress(
    ogg_path: &std::path::Path,
    wav_path: &std::path::Path,
    total_secs: f64,
    on_pct: &mut dyn FnMut(u32),
) -> Result<(), String> {
    use crate::resampler_compat::{
        SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };
    use hound::{WavSpec, WavWriter};
    use lewton::inside_ogg::OggStreamReader;
    use std::fs::File;

    const TARGET_RATE: u32 = 16000;

    let ogg_file = File::open(ogg_path).map_err(|e| e.to_string())?;
    let mut ogg_reader = OggStreamReader::new(ogg_file).map_err(|e| e.to_string())?;

    let src_rate = ogg_reader.ident_hdr.audio_sample_rate;
    let src_channels = ogg_reader.ident_hdr.audio_channels as u16;

    let spec = WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut wav_writer = WavWriter::create(wav_path, spec).map_err(|e| e.to_string())?;

    // Collect all decoded mono samples as f32. Decoding dominates the wall
    // time, so it owns 0-85% of the progress; resampling takes 85-100%.
    let mut all_mono: Vec<f32> = Vec::new();
    let mut last_pct: u32 = 0;
    while let Some(packet) = ogg_reader
        .read_dec_packet_generic::<Vec<Vec<i16>>>()
        .map_err(|e| e.to_string())?
    {
        if packet.is_empty() {
            continue;
        }
        let frames = packet[0].len();
        for i in 0..frames {
            let mut sum: i32 = 0;
            for ch in 0..src_channels as usize {
                if ch < packet.len() && i < packet[ch].len() {
                    sum += packet[ch][i] as i32;
                }
            }
            all_mono.push((sum / src_channels as i32) as f32 / 32768.0);
        }
        if total_secs > 0.0 {
            let decoded = all_mono.len() as f64 / src_rate as f64;
            let pct = ((decoded / total_secs) * 85.0).min(85.0) as u32;
            if pct != last_pct {
                last_pct = pct;
                on_pct(pct);
            }
        }
    }

    if src_rate == TARGET_RATE {
        // No resampling needed
        for s in &all_mono {
            wav_writer
                .write_sample((*s * 32768.0).clamp(-32768.0, 32767.0) as i16)
                .map_err(|e| e.to_string())?;
        }
    } else {
        // High-quality sinc interpolation resampling (matches mic pipeline approach)
        let chunk_size = 1024usize;
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: None,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let resample_ratio = TARGET_RATE as f64 / src_rate as f64;
        let mut resampler = SincFixedIn::<f32>::new(
            resample_ratio,
            2.0,
            params,
            chunk_size,
            1, // mono
        )
        .map_err(|e| format!("Failed to create resampler: {}", e))?;

        let mut pos = 0;
        while pos + chunk_size <= all_mono.len() {
            let chunk = vec![&all_mono[pos..pos + chunk_size]];
            let output = resampler
                .process(&chunk, None)
                .map_err(|e| format!("Resampling error: {}", e))?;
            for s in &output[0] {
                wav_writer
                    .write_sample((*s * 32768.0).clamp(-32768.0, 32767.0) as i16)
                    .map_err(|e| e.to_string())?;
            }
            pos += chunk_size;
            if total_secs > 0.0 && !all_mono.is_empty() {
                let pct = 85 + ((pos as f64 / all_mono.len() as f64) * 15.0) as u32;
                if pct != last_pct {
                    last_pct = pct;
                    on_pct(pct.min(100));
                }
            }
        }

        // Process remaining frames
        if pos < all_mono.len() {
            let chunk = vec![&all_mono[pos..]];
            let output = resampler
                .process_partial(Some(&chunk), None)
                .map_err(|e| format!("Resampling error: {}", e))?;
            for s in &output[0] {
                wav_writer
                    .write_sample((*s * 32768.0).clamp(-32768.0, 32767.0) as i16)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(())
}
