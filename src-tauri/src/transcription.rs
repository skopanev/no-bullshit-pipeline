use crate::config::{TranscriptionProvider, load_settings};
use crate::storage::{get_data_dir, read_metadata};
use crate::transcript_migration::{TranscriptMetadata, TranscriptSource};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// One diarized speaker turn as emitted by the sidecar JSON (camelCase keys).
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SidecarSegment {
    speaker_id: String,
    start_time: f64,
    end_time: f64,
    text: String,
}

#[derive(Deserialize, Debug)]
struct FluidAudioOutput {
    model: String,
    text: String,
    /// Diarized turns. Absent on the AppleSpeech / no-diarize sidecars → empty.
    #[serde(default)]
    segments: Vec<SidecarSegment>,
}

/// One diarized speaker turn persisted in transcript.json.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct TranscriptSegment {
    pub speaker_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub text: String,
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
    /// Diarized speaker turns. Empty for legacy transcripts (serde default) and
    /// for the AppleSpeech / no-diarize paths — render then falls back to `text`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    segments: Vec<TranscriptSegment>,
    /// User renames: original speaker id ("Speaker 1") → display name. Applied
    /// only at render time — `segments`/`text` are never rewritten, so the
    /// verbatim record stays intact and a rename is fully reversible.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    speaker_names: HashMap<String, String>,
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
                segments: to_transcript_segments(out.segments),
                speaker_names: HashMap::new(),
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
            if !settings.transcription.diarize {
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

            let _ = std::fs::remove_file(&wav_path);

            for line in stderr_buf.lines() {
                if !line.starts_with("PROGRESS:") && !line.is_empty() {
                    eprintln!("{}", line);
                }
            }

            if exit_code != Some(0) {
                return Err(format!("FluidAudio sidecar failed: {}", stderr_buf));
            }

            let stdout = String::from_utf8_lossy(&stdout_buf);
            let fa_output: FluidAudioOutput = serde_json::from_str(&stdout)
                .map_err(|e| format!("Failed to parse FluidAudio output: {}", e))?;

            let duration_sec = read_metadata(&recording_id)
                .ok()
                .and_then(|m| m.audio.mix.as_ref().map(|a| a.duration_sec))
                .unwrap_or(0.0);

            TranscriptJson {
                source,
                model: fa_output.model,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                duration_sec,
                language: Some("auto".to_string()),
                text: Some(fa_output.text),
                segments: to_transcript_segments(fa_output.segments),
                speaker_names: HashMap::new(),
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
    let preview = preview_from_transcript(&transcript_json);
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

/// Map the sidecar's segments into the persisted form.
fn to_transcript_segments(segments: Vec<SidecarSegment>) -> Vec<TranscriptSegment> {
    segments
        .into_iter()
        .map(|s| TranscriptSegment {
            speaker_id: s.speaker_id,
            start_time: s.start_time,
            end_time: s.end_time,
            text: s.text,
        })
        .collect()
}

/// Render display text from a TranscriptJson.
///
/// - No segments (legacy transcripts, AppleSpeech, `--no-diarize`) → the flat
///   `text` verbatim, exactly as before.
/// - One distinct speaker → plain prose, no speaker headers (a lone "Speaker 1"
///   turn from the no-diarize path shouldn't sprout a label).
/// - Multiple speakers → `**Name:**` headers, where `Name` is the user rename
///   from `speaker_names` if present, else the original id. Renames are applied
///   here only; `segments`/`text` are never mutated.
pub(crate) fn render_transcript_from_json(tj: &TranscriptJson) -> String {
    if tj.segments.is_empty() {
        return tj.text.clone().unwrap_or_default();
    }

    let distinct: HashSet<&str> = tj.segments.iter().map(|s| s.speaker_id.as_str()).collect();
    if distinct.len() <= 1 {
        return tj
            .segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }

    let mut out = String::new();
    let mut current = "";
    for seg in &tj.segments {
        if seg.speaker_id != current {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            let name = tj
                .speaker_names
                .get(&seg.speaker_id)
                .map(String::as_str)
                .unwrap_or(seg.speaker_id.as_str());
            out.push_str("**");
            out.push_str(name);
            out.push_str(":**\n");
            current = &seg.speaker_id;
        }
        let t = seg.text.trim();
        if !t.is_empty() {
            out.push_str(t);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Recordings-list preview from a transcript: rendered text with the `**`
/// speaker-header markers stripped so the list reads as plain prose.
fn preview_from_transcript(tj: &TranscriptJson) -> String {
    truncate_preview(&render_transcript_from_json(tj).replace("**", ""), 200)
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

/// One diarized speaker for the rename UI: the original id plus the user's
/// current display name (if any).
#[derive(Serialize)]
pub struct SpeakerInfo {
    speaker_id: String,
    display_name: Option<String>,
}

/// Distinct speakers in a recording, in first-seen order, with current renames.
/// Empty when the recording has no diarized segments (legacy / no-diarize),
/// which the UI uses to hide the rename affordance.
#[tauri::command]
pub async fn get_speakers(recording_id: String) -> Result<Vec<SpeakerInfo>, String> {
    let json_path = get_data_dir().join(&recording_id).join("transcript.json");
    if !json_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("Failed to read transcript: {}", e))?;
    let tj: TranscriptJson =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse transcript: {}", e))?;

    let mut seen = HashSet::new();
    let mut speakers = Vec::new();
    for seg in &tj.segments {
        if seen.insert(seg.speaker_id.as_str()) {
            speakers.push(SpeakerInfo {
                speaker_id: seg.speaker_id.clone(),
                display_name: tj.speaker_names.get(&seg.speaker_id).cloned(),
            });
        }
    }
    Ok(speakers)
}

/// Rename one diarized speaker for a single recording. Stores the mapping in
/// transcript.json's `speaker_names` (applied only at render) — never rewrites
/// `text`/`segments`, so it's reversible and can't corrupt prose that happens
/// to contain a literal "Speaker 1". A blank name or the original id clears the
/// rename. Returns the freshly rendered transcript for immediate UI refresh.
#[tauri::command]
pub async fn rename_speaker(
    recording_id: String,
    speaker_id: String,
    display_name: String,
) -> Result<String, String> {
    let json_path = get_data_dir().join(&recording_id).join("transcript.json");
    let content = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("Failed to read transcript: {}", e))?;
    let mut tj: TranscriptJson =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse transcript: {}", e))?;

    if !tj.segments.iter().any(|s| s.speaker_id == speaker_id) {
        return Err(format!(
            "Speaker '{}' not found in this transcript",
            speaker_id
        ));
    }

    let name = display_name.trim();
    if name.is_empty() || name == speaker_id {
        tj.speaker_names.remove(&speaker_id);
    } else {
        tj.speaker_names
            .insert(speaker_id.clone(), name.to_string());
    }

    // Atomic write (temp + rename), same pattern as the transcribe path.
    let json_str = serde_json::to_string_pretty(&tj)
        .map_err(|e| format!("Failed to serialize transcript: {}", e))?;
    let temp_path = json_path.with_extension("json.tmp");
    std::fs::write(&temp_path, &json_str)
        .map_err(|e| format!("Failed to write transcript: {}", e))?;
    std::fs::rename(&temp_path, &json_path)
        .map_err(|e| format!("Failed to finalize transcript: {}", e))?;

    // Keep the recordings-list preview in sync with the new name.
    if let Ok(mut meta) = crate::storage::read_metadata(&recording_id) {
        let preview = preview_from_transcript(&tj);
        meta.transcript_preview = if preview.is_empty() {
            None
        } else {
            Some(preview)
        };
        let _ = crate::storage::write_metadata(&meta);
    }

    Ok(render_transcript_from_json(&tj))
}

pub fn convert_ogg_to_wav(
    ogg_path: &std::path::Path,
    wav_path: &std::path::Path,
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

    // Collect all decoded mono samples as f32
    let mut all_mono: Vec<f32> = Vec::new();
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
            f_cutoff: 0.95,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tj(json: &str) -> TranscriptJson {
        serde_json::from_str(json).expect("deserialize TranscriptJson")
    }

    #[test]
    fn legacy_text_only_renders_verbatim() {
        // The exact shape every pre-diarization transcript on disk has: no
        // `segments` key. serde(default) must fill it empty and render the flat
        // text verbatim — zero behavior change for existing recordings.
        let t = tj(
            r#"{"source":"local","model":"m","created_at":"t","duration_sec":1.0,"text":"hello world"}"#,
        );
        assert!(t.segments.is_empty());
        assert!(t.speaker_names.is_empty());
        assert_eq!(render_transcript_from_json(&t), "hello world");
    }

    #[test]
    fn single_speaker_has_no_headers() {
        // The --no-diarize path emits one "Speaker 1" segment; it must read as
        // plain prose, not sprout a header.
        let t = tj(
            r#"{"source":"local","model":"m","created_at":"t","duration_sec":1.0,"text":"a b",
            "segments":[
              {"speaker_id":"Speaker 1","start_time":0.0,"end_time":1.0,"text":"a"},
              {"speaker_id":"Speaker 1","start_time":1.0,"end_time":2.0,"text":"b"}
            ]}"#,
        );
        let r = render_transcript_from_json(&t);
        assert!(
            !r.contains("**"),
            "single speaker must have no headers: {r:?}"
        );
        assert_eq!(r, "a b");
    }

    #[test]
    fn multi_speaker_renders_headers() {
        let t = tj(
            r#"{"source":"local","model":"m","created_at":"t","duration_sec":1.0,"text":"flat",
            "segments":[
              {"speaker_id":"Speaker 1","start_time":0.0,"end_time":1.0,"text":"hi"},
              {"speaker_id":"Speaker 2","start_time":1.0,"end_time":2.0,"text":"yo"}
            ]}"#,
        );
        let r = render_transcript_from_json(&t);
        assert!(r.contains("**Speaker 1:**"), "{r}");
        assert!(r.contains("**Speaker 2:**"), "{r}");
        assert!(r.contains("hi") && r.contains("yo"));
    }

    #[test]
    fn rename_applied_at_render_never_mutates_record() {
        let t = tj(
            r#"{"source":"local","model":"m","created_at":"t","duration_sec":1.0,"text":"flat",
            "speaker_names":{"Speaker 1":"Alice"},
            "segments":[
              {"speaker_id":"Speaker 1","start_time":0.0,"end_time":1.0,"text":"hi"},
              {"speaker_id":"Speaker 2","start_time":1.0,"end_time":2.0,"text":"yo"}
            ]}"#,
        );
        let r = render_transcript_from_json(&t);
        assert!(r.contains("**Alice:**"), "rename applied at render: {r}");
        assert!(!r.contains("**Speaker 1:**"));
        assert!(r.contains("**Speaker 2:**"), "un-renamed speaker stays");
        // The verbatim record is never rewritten — only the render reflects names.
        assert_eq!(t.text.as_deref(), Some("flat"));
        assert_eq!(t.segments[0].speaker_id, "Speaker 1");
        assert_eq!(t.segments[0].text, "hi");
    }

    #[test]
    fn preview_strips_speaker_markers() {
        let t = tj(
            r#"{"source":"local","model":"m","created_at":"t","duration_sec":1.0,"text":"flat",
            "segments":[
              {"speaker_id":"Speaker 1","start_time":0.0,"end_time":1.0,"text":"hi"},
              {"speaker_id":"Speaker 2","start_time":1.0,"end_time":2.0,"text":"yo"}
            ]}"#,
        );
        let p = preview_from_transcript(&t);
        assert!(!p.contains("**"), "preview must read as plain prose: {p:?}");
        assert!(p.contains("Speaker 1") && p.contains("hi"));
    }

    /// Backward-compat smoke test over the developer's real recordings. Skips
    /// gracefully when `~/nbp-data` isn't present (CI / fresh checkout).
    #[test]
    fn real_recordings_still_deserialize_and_render() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let data = std::path::Path::new(&home).join("nbp-data");
        if !data.is_dir() {
            eprintln!(
                "skipping real-recording check: {} not present",
                data.display()
            );
            return;
        }
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&data).unwrap().flatten() {
            let path = entry.path().join("transcript.json");
            if !path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            let t: TranscriptJson = serde_json::from_str(&content)
                .unwrap_or_else(|e| panic!("failed to deserialize {}: {}", path.display(), e));
            let rendered = render_transcript_from_json(&t);
            // Every existing transcript has no segments → render MUST equal the
            // stored text verbatim. Any drift means we broke an old recording.
            if t.segments.is_empty() {
                assert_eq!(
                    rendered,
                    t.text.clone().unwrap_or_default(),
                    "legacy render drifted for {}",
                    path.display()
                );
            }
            checked += 1;
        }
        eprintln!("real_recordings: verified {checked} transcript.json files");
        assert!(
            checked > 0,
            "expected at least one real transcript to verify"
        );
    }

    /// End-to-end over a REAL sidecar run: point `NBP_SIDECAR_JSON` at a captured
    /// `fluidaudio-sidecar` stdout and this exercises the exact production path —
    /// parse `FluidAudioOutput` (camelCase) → `to_transcript_segments` →
    /// `TranscriptJson` → render — plus a rename. Skips when the env var is unset.
    #[test]
    fn sidecar_output_roundtrips_through_render() {
        let Ok(path) = std::env::var("NBP_SIDECAR_JSON") else {
            return;
        };
        let raw = std::fs::read_to_string(&path).expect("read sidecar json");
        let fa: FluidAudioOutput = serde_json::from_str(&raw).expect("parse sidecar output");
        assert!(!fa.segments.is_empty(), "sidecar produced no segments");

        let mut tj = TranscriptJson {
            source: TranscriptSource::Local,
            model: fa.model,
            created_at: "t".to_string(),
            duration_sec: 0.0,
            language: Some("auto".to_string()),
            text: Some(fa.text),
            segments: to_transcript_segments(fa.segments),
            speaker_names: HashMap::new(),
        };

        let rendered = render_transcript_from_json(&tj);
        let distinct: HashSet<&str> = tj.segments.iter().map(|s| s.speaker_id.as_str()).collect();
        eprintln!(
            "\n--- rendered ({} speakers) ---\n{}\n",
            distinct.len(),
            rendered
        );
        if distinct.len() > 1 {
            assert!(
                rendered.contains("**"),
                "multi-speaker render must have headers"
            );
        }

        // Rename the first speaker and confirm only the render changes.
        if let Some(first) = tj.segments.first().map(|s| s.speaker_id.clone()) {
            tj.speaker_names.insert(first.clone(), "Сергей".to_string());
            let renamed = render_transcript_from_json(&tj);
            eprintln!("--- after rename {first} → Сергей ---\n{}\n", renamed);
            assert!(renamed.contains("**Сергей:**"));
            assert_eq!(tj.segments[0].speaker_id, first, "record itself untouched");
        }
    }
}
