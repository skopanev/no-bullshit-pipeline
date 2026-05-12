use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Mutex;
use crate::config::{WhisperModelSize, TranscriptionProvider, get_models_dir, load_settings};
use crate::storage::{get_data_dir, read_metadata};
use crate::cloud_ai;
use crate::transcript_migration::{TranscriptMetadata, TranscriptSource};
use tauri::Emitter;
use tauri_plugin_shell::ShellExt;

pub struct TranscriptionState {
    pub active_ids: Mutex<HashSet<String>>,
}

impl TranscriptionState {
    pub fn new() -> Self {
        Self {
            active_ids: Mutex::new(HashSet::new()),
        }
    }
}

#[tauri::command]
pub fn is_transcribing(
    recording_id: String,
    state: tauri::State<'_, TranscriptionState>,
) -> bool {
    state.active_ids.lock().unwrap_or_else(|e| e.into_inner()).contains(&recording_id)
}

const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelInfo {
    pub size: WhisperModelSize,
    pub filename: String,
    pub url: String,
    pub size_mb: Option<u64>,
    pub exact_bytes: Option<u64>,
    pub downloaded: bool,
    pub path: String,
}

#[derive(Deserialize, Debug)]
struct FluidAudioOutput {
    model: String,
    text: String,
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

impl TranscriptJson {
    pub fn new(
        source: TranscriptSource,
        model: String,
        created_at: String,
        duration_sec: f64,
        language: Option<String>,
        text: Option<String>,
    ) -> Self {
        Self {
            source,
            model,
            created_at,
            duration_sec,
            language,
            text,
        }
    }
}

#[derive(Clone, Serialize)]
struct TranscriptionProgress {
    recording_id: String,
    stage: String,
    percent: u32,
}

pub fn get_model_url(size: &WhisperModelSize) -> String {
    format!("{}/{}", BASE_URL, size.filename())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RemoteWhisperModel {
    pub filename: String,
    pub url: String,
    pub size_bytes: u64,
    pub last_modified: Option<String>,
    pub downloaded: bool,
    pub path: String,
}

/// Return the curated short-list of Whisper models worth offering. The upstream
/// repo (ggerganov/whisper.cpp on HF) has been frozen since Oct 2024 — there
/// will be no new files. So we hardcode the 4 picks that cover the real
/// trade-off space: turbo as the recommended default, its q8 quantization for
/// users who want half the size, and base/tiny for tight CPU/memory budgets.
///
/// The `limit` parameter is accepted but ignored — kept in the signature so the
/// existing frontend `invoke('list_whisper_models_remote', { limit: ... })`
/// calls don't need to change.
#[tauri::command]
pub async fn list_whisper_models_remote(
    limit: Option<u32>,
) -> Result<Vec<RemoteWhisperModel>, String> {
    let _ = limit;
    let models_dir = get_models_dir();
    if !models_dir.exists() {
        let _ = std::fs::create_dir_all(&models_dir);
    }

    let curated: &[(&str, u64)] = &[
        ("ggml-large-v3-turbo.bin", 1_624 * 1024 * 1024),
        ("ggml-large-v3-turbo-q8_0.bin", 874 * 1024 * 1024),
        ("ggml-base.bin", 148 * 1024 * 1024),
        ("ggml-tiny.bin", 78 * 1024 * 1024),
    ];

    Ok(curated
        .iter()
        .map(|(f, size)| {
            let local = models_dir.join(f);
            let downloaded = local
                .metadata()
                .map(|m| m.len() >= *size)
                .unwrap_or(false);
            RemoteWhisperModel {
                filename: f.to_string(),
                url: format!("{}/{}", BASE_URL, f),
                size_bytes: *size,
                last_modified: None,
                downloaded,
                path: local.to_string_lossy().to_string(),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn get_whisper_models_info() -> Result<Vec<ModelInfo>, String> {
    // Legacy command kept for backward compat with existing UI paths.
    // Returns the canonical 5-size set with current download status.
    let baseline: Vec<(WhisperModelSize, u64)> = vec![
        (WhisperModelSize::new("ggml-tiny.bin"), 74),
        (WhisperModelSize::new("ggml-base.bin"), 141),
        (WhisperModelSize::new("ggml-small.bin"), 465),
        (WhisperModelSize::new("ggml-medium.bin"), 1462),
        (WhisperModelSize::new("ggml-large-v3.bin"), 2951),
    ];

    let mut results = Vec::new();
    let models_dir = get_models_dir();
    if !models_dir.exists() {
        std::fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;
    }

    for (size, size_mb) in baseline {
        let url = get_model_url(&size);
        let filename = size.filename().to_string();
        let local_path = models_dir.join(&filename);
        let expected_bytes = size_mb * 1024 * 1024;
        let downloaded = local_path.exists()
            && std::fs::metadata(&local_path)
                .map(|m| m.len() >= expected_bytes)
                .unwrap_or(false);
        results.push(ModelInfo {
            size,
            filename,
            url,
            size_mb: Some(size_mb),
            exact_bytes: None,
            downloaded,
            path: local_path.to_string_lossy().to_string(),
        });
    }
    Ok(results)
}

#[derive(Clone, Serialize)]
struct DownloadProgress {
    size: WhisperModelSize,
    downloaded: u64,
    total: u64,
    percent: f64,
}

#[tauri::command]
pub async fn download_whisper_model(
    app_handle: tauri::AppHandle,
    size: WhisperModelSize,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    use futures_util::StreamExt;

    let url = get_model_url(&size);
    let models_dir = get_models_dir();
    
    if !models_dir.exists() {
        std::fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;
    }
    
    let filename = url.split('/').last().unwrap();
    let file_path = models_dir.join(filename);
    
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;

    // Check for partial download to resume
    let existing_len = if file_path.exists() {
        std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    let mut request = client.get(&url);
    if existing_len > 0 {
        request = request.header("Range", format!("bytes={}-", existing_len));
    }

    let res = request.send().await.map_err(|e| e.to_string())?;
    let status = res.status();

    let (total_size, mut downloaded, resume) = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        // Server supports range — resume
        let content_len = res.content_length().unwrap_or(0);
        (existing_len + content_len, existing_len, true)
    } else {
        // No range support or fresh download
        let content_len = res.content_length().unwrap_or(0);
        (content_len, 0u64, false)
    };

    let mut file = if resume {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .await
            .map_err(|e| e.to_string())?
    } else {
        tokio::fs::File::create(&file_path).await.map_err(|e| e.to_string())?
    };

    let mut stream = res.bytes_stream();
    let mut last_emit = std::time::Instant::now();

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| e.to_string())?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed().as_millis() > 100 || downloaded == total_size {
            let percent = if total_size > 0 { (downloaded as f64 / total_size as f64) * 100.0 } else { 0.0 };
            let _ = app_handle.emit("download_progress", DownloadProgress {
                size: size.clone(), downloaded, total: total_size, percent,
            });
            last_emit = std::time::Instant::now();
        }
    }
    
    Ok(file_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn delete_whisper_model(size: WhisperModelSize) -> Result<(), String> {
    let url = get_model_url(&size);
    let models_dir = get_models_dir();
    let filename = url.split('/').last().unwrap();
    let file_path = models_dir.join(filename);
    if file_path.exists() {
        std::fs::remove_file(file_path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn transcribe_recording(
    app_handle: tauri::AppHandle,
    recording_id: String,
    transcription_state: tauri::State<'_, TranscriptionState>,
) -> Result<String, String> {
    {
        let mut active = transcription_state.active_ids.lock().unwrap_or_else(|e| e.into_inner());
        if active.contains(&recording_id) {
            return Ok("__already_running__".to_string());
        }
        active.insert(recording_id.clone());
    }

    let result = transcribe_recording_inner(&app_handle, &recording_id).await;

    {
        let mut active = transcription_state.active_ids.lock().unwrap_or_else(|e| e.into_inner());
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
    let whisper_model_ref = settings.transcription.whisper_model.clone();

    // Shared metadata fields
    let source = match provider {
        TranscriptionProvider::LocalWhisper | TranscriptionProvider::Unknown => TranscriptSource::Local,
        TranscriptionProvider::FluidAudio => TranscriptSource::Fluidaudio,
        TranscriptionProvider::OpenAI => TranscriptSource::Openai,
        TranscriptionProvider::Google => TranscriptSource::Google,
        TranscriptionProvider::Anthropic => TranscriptSource::Anthropic,
    };

    let _ = app_handle.emit("transcription_progress", TranscriptionProgress {
        recording_id: recording_id.clone(),
        stage: "Starting".to_string(),
        percent: 0,
    });

    let transcript_json = match provider {
        TranscriptionProvider::FluidAudio => {
            let wav_path = recording_dir.join("temp_transcription.wav");
            convert_ogg_to_wav(&audio_path, &wav_path)?;

            let (mut rx, _child) = app_handle.shell().sidecar("fluidaudio-sidecar")
                .map_err(|e| format!("Failed to create sidecar command: {}", e))?
                .arg(wav_path.to_str().ok_or("Invalid WAV path")?)
                .spawn()
                .map_err(|e| format!("Failed to spawn FluidAudio sidecar: {}", e))?;

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
                                if parts.len() == 2 {
                                    if let Ok(pct) = parts[1].parse::<u32>() {
                                        let _ = app_handle.emit("transcription_progress", TranscriptionProgress {
                                            recording_id: recording_id.clone(),
                                            stage: parts[0].to_string(),
                                            percent: pct,
                                        });
                                    }
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
            }
        },
        TranscriptionProvider::LocalWhisper => {
            let model_size = settings.transcription.whisper_model.ok_or("No whisper model selected")?;
            let url = get_model_url(&model_size);
            let filename = url.split('/').last().unwrap();
            let model_path = get_models_dir().join(filename);

            if !model_path.exists() {
                return Err(format!("Model not downloaded: {}", model_size.filename()));
            }

            let wav_path = recording_dir.join("temp_transcription.wav");
            convert_ogg_to_wav(&audio_path, &wav_path)?;

            let _ = app_handle.emit("transcription_progress", TranscriptionProgress {
                recording_id: recording_id.clone(),
                stage: "Loading model".to_string(),
                percent: 0,
            });

            let model_p = model_path.clone();
            let wav_p = wav_path.clone();
            let app_h = app_handle.clone();
            let rec_id = recording_id.clone();

            let transcript = tokio::task::spawn_blocking(move || {
                run_whisper_transcription(&model_p, &wav_p, &app_h, &rec_id)
            }).await.map_err(|e| e.to_string())??;

            let _ = std::fs::remove_file(&wav_path);

            let model_name = whisper_model_ref
                .map(|m| m.filename().trim_end_matches(".bin").trim_start_matches("ggml-").to_string())
                .map(|s| format!("whisper-{}", s))
                .unwrap_or_else(|| "whisper-unknown".to_string());
            let duration_sec = read_metadata(&recording_id)
                .ok()
                .and_then(|m| m.audio.mix.as_ref().map(|a| a.duration_sec))
                .unwrap_or(0.0);

            TranscriptJson {
                source,
                model: model_name,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                duration_sec,
                language: Some("auto".to_string()),
                text: Some(transcript),
            }
        },
        TranscriptionProvider::OpenAI => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured")?;

            let _ = app_handle.emit("transcription_progress", TranscriptionProgress {
                recording_id: recording_id.clone(),
                stage: "Transcribing".to_string(),
                percent: 0,
            });

            let transcript = cloud_ai::transcribe_with_whisper(&api_key, &audio_path).await?;
            let duration_sec = read_metadata(&recording_id)
                .ok()
                .and_then(|m| m.audio.mix.as_ref().map(|a| a.duration_sec))
                .unwrap_or(0.0);

            TranscriptJson {
                source,
                model: "whisper-1".to_string(),
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                duration_sec,
                language: Some("auto".to_string()),
                text: Some(transcript),
            }
        },
        TranscriptionProvider::Google => {
            return Err("Google provider requires a transcript first. Use Local Whisper or OpenAI for transcription, then use Google for summarization.".to_string());
        },
        TranscriptionProvider::Anthropic => {
            return Err("Anthropic provider doesn't support audio transcription. Use Local Whisper or OpenAI for transcription, then use Anthropic for structured extraction.".to_string());
        },
        TranscriptionProvider::Unknown => {
            return Err("Unknown transcription provider. Please select a valid provider in settings.".to_string());
        },
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

    let _ = app_handle.emit("transcription_progress", TranscriptionProgress {
        recording_id: recording_id.clone(),
        stage: "Done".to_string(),
        percent: 100,
    });

    // Return rendered text for immediate UI display
    Ok(render_transcript_from_json(&transcript_json))
}

/// Render text from a TranscriptJson struct
pub(crate) fn render_transcript_from_json(tj: &TranscriptJson) -> String {
    tj.text.clone().unwrap_or_default()
}

/// Render transcript text on the fly from transcript.json (with .md fallback)
fn render_transcript_text(recording_id: &str) -> Option<String> {
    let recording_dir = get_data_dir().join(recording_id);
    let json_path = recording_dir.join("transcript.json");

    if json_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&json_path) {
            if let Ok(tj) = serde_json::from_str::<TranscriptJson>(&content) {
                return Some(render_transcript_from_json(&tj));
            }
        }
    }

    // Fallback: legacy transcript.md
    let md_path = recording_dir.join("transcript.md");
    if md_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&md_path) {
            if content.starts_with("---") {
                let parts: Vec<&str> = content.splitn(3, "---").collect();
                if parts.len() >= 3 {
                    return Some(parts[2].trim().to_string());
                }
            }
            return Some(content);
        }
    }

    None
}

/// Read transcript body for internal consumers (summarize, templates, etc.)
fn read_transcript_body(recording_id: &str) -> Result<String, String> {
    render_transcript_text(recording_id)
        .ok_or_else(|| "No transcript found. Please transcribe the recording first.".to_string())
}

/// Summarize a recording's transcript using the configured AI provider
#[tauri::command]
pub async fn summarize_recording(
    recording_id: String,
    provider: Option<String>,
) -> Result<String, String> {
    let settings = load_settings();
    let recording_dir = get_data_dir().join(&recording_id);

    let transcript = read_transcript_body(&recording_id)?;

    // Determine which processing provider to use
    let use_provider = provider.unwrap_or_else(|| crate::config::detect_processing_provider(&settings));

    let summary = match use_provider.as_str() {
        "openai" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured")?;
            cloud_ai::summarize_with_gpt4o(&api_key, &transcript, None).await?
        },
        "google" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "google")
                .ok_or("Google API key not configured")?;
            cloud_ai::summarize_with_gemini(&api_key, &transcript).await?
        },
        "anthropic" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "anthropic")
                .ok_or("Anthropic API key not configured")?;
            cloud_ai::process_with_claude(&api_key,
                "Create a comprehensive summary of this transcript. Include main topics, key points, decisions, and action items.\n\nTranscript:\n{transcript}",
                &transcript,
                ""
            ).await?
        },
        "cli_agent" => {
            let cli_config = settings.cli_agent.clone();
            crate::connectors::cli_agent::process_with_cli(
                &cli_config.cli,
                "Create a comprehensive summary of this transcript. Include main topics, key points, decisions, and action items.",
                &transcript,
                cli_config.model.as_deref(),
                cli_config.timeout_secs,
            ).await?
        },
        "local" => {
            let llm_settings = settings.local_llm.clone();
            if llm_settings.enabled && llm_settings.model_id.is_some() {
                tokio::task::spawn_blocking(move || {
                    crate::local_llm::summarize_with_local(&transcript)
                })
                .await
                .map_err(|e| format!("Local LLM task failed: {}", e))??
            } else {
                return Err("Local LLM not configured. Download a model in Settings.".to_string());
            }
        },
        "ollama" => {
            cloud_ai::process_with_openai_compat(
                "http://localhost:11434",
                None,
                "llama3.2",
                &format!("Create a comprehensive summary of this transcript. Include main topics, key points, decisions, and action items.\n\n{}", &transcript),
                "",
            ).await?
        },
        other => {
            return Err(format!("Unknown processing provider: '{}'. Configure in Settings.", other));
        },
    };

    // Save summary
    std::fs::write(recording_dir.join("summary.md"), &summary)
        .map_err(|e| format!("Failed to save summary: {}", e))?;

    Ok(summary)
}

/// Process a transcript with a specific template
#[tauri::command]
pub async fn process_with_template(
    recording_id: String,
    template_name: String,
    provider: Option<String>,
) -> Result<String, String> {
    let settings = load_settings();
    let recording_dir = get_data_dir().join(&recording_id);

    let transcript = read_transcript_body(&recording_id)?;

    // Load template
    let template = crate::templates::get_template_internal(&template_name)?;

    // Determine which processing provider to use
    let use_provider = provider.unwrap_or_else(|| crate::config::detect_processing_provider(&settings));

    let result = match use_provider.as_str() {
        "openai" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured")?;
            cloud_ai::process_with_gpt4o(&api_key, &template.prompt, &transcript, "").await?
        },
        "google" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "google")
                .ok_or("Google API key not configured")?;
            cloud_ai::process_with_gemini(&api_key, &template.prompt, &transcript, "").await?
        },
        "anthropic" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "anthropic")
                .ok_or("Anthropic API key not configured")?;
            cloud_ai::process_with_claude(&api_key, &template.prompt, &transcript, "").await?
        },
        "cli_agent" => {
            let cli_config = settings.cli_agent.clone();
            crate::connectors::cli_agent::process_with_cli(
                &cli_config.cli,
                &template.prompt,
                &transcript,
                cli_config.model.as_deref(),
                cli_config.timeout_secs,
            ).await?
        },
        "local" => {
            let llm_settings = settings.local_llm.clone();
            if llm_settings.enabled && llm_settings.model_id.is_some() {
                let prompt = template.prompt.clone();
                let transcript_clone = transcript.clone();
                tokio::task::spawn_blocking(move || {
                    crate::local_llm::process_with_local(&prompt, &transcript_clone)
                })
                .await
                .map_err(|e| format!("Local LLM task failed: {}", e))??
            } else {
                return Err("Local LLM not configured. Download a model in Settings.".to_string());
            }
        },
        "ollama" => {
            cloud_ai::process_with_openai_compat(
                "http://localhost:11434",
                None,
                "llama3.2",
                &format!("{}\n\n{}", &template.prompt, &transcript),
                "",
            ).await?
        },
        other => {
            return Err(format!("Unknown processing provider: '{}'. Configure in Settings.", other));
        },
    };

    // Save result based on output format
    let filename = match template.output_format.as_str() {
        "json" => format!("{}.json", template_name),
        _ => format!("{}.md", template_name),
    };
    std::fs::write(recording_dir.join(&filename), &result)
        .map_err(|e| format!("Failed to save result: {}", e))?;

    Ok(result)
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
    app_handle.dialog()
        .file()
        .add_filter("Markdown", &["md"])
        .set_file_name("transcript.md")
        .save_file(move |file_path| {
            let _ = tx.send(file_path);
        });

    let file_path = rx.await.map_err(|_| "Dialog channel closed unexpectedly")?;

    if let Some(path) = file_path {
        let dest = path.as_path()
            .ok_or("Invalid file path selected")?;
        std::fs::write(dest, &md_content)
            .map_err(|e| format!("Failed to save file: {}", e))?;
    }

    Ok(())
}

fn convert_ogg_to_wav(ogg_path: &std::path::Path, wav_path: &std::path::Path) -> Result<(), String> {
    use lewton::inside_ogg::OggStreamReader;
    use hound::{WavWriter, WavSpec};
    use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction, Resampler};
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
    while let Some(packet) = ogg_reader.read_dec_packet_generic::<Vec<Vec<i16>>>().map_err(|e| e.to_string())? {
        if packet.is_empty() { continue; }
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
            wav_writer.write_sample((*s * 32768.0).clamp(-32768.0, 32767.0) as i16)
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
        ).map_err(|e| format!("Failed to create resampler: {}", e))?;

        let mut pos = 0;
        while pos + chunk_size <= all_mono.len() {
            let chunk = vec![&all_mono[pos..pos + chunk_size]];
            let output = resampler.process(&chunk, None)
                .map_err(|e| format!("Resampling error: {}", e))?;
            for s in &output[0] {
                wav_writer.write_sample((*s * 32768.0).clamp(-32768.0, 32767.0) as i16)
                    .map_err(|e| e.to_string())?;
            }
            pos += chunk_size;
        }

        // Process remaining frames
        if pos < all_mono.len() {
            let chunk = vec![&all_mono[pos..]];
            let output = resampler.process_partial(Some(&chunk), None)
                .map_err(|e| format!("Resampling error: {}", e))?;
            for s in &output[0] {
                wav_writer.write_sample((*s * 32768.0).clamp(-32768.0, 32767.0) as i16)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(())
}

pub(crate) fn load_whisper_context(model_path: &std::path::Path) -> Result<whisper_rs::WhisperContext, String> {
    use whisper_rs::{WhisperContext, WhisperContextParameters};
    let params = WhisperContextParameters::default();
    WhisperContext::new_with_params(
        model_path.to_str().ok_or("Invalid model path")?,
        params,
    )
    .map_err(|e| format!("Failed to load Whisper model: {}", e))
}

fn run_whisper_transcription(
    model_path: &std::path::Path,
    wav_path: &std::path::Path,
    app_handle: &tauri::AppHandle,
    recording_id: &str,
) -> Result<String, String> {
    use whisper_rs::{FullParams, SamplingStrategy};
    use hound::WavReader;

    eprintln!("[whisper] loading model...");
    let ctx = load_whisper_context(model_path)?;
    eprintln!("[whisper] model loaded OK");

    let mut wav_reader = WavReader::open(wav_path).map_err(|e| e.to_string())?;
    let samples: Vec<f32> = wav_reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    eprintln!("[whisper] WAV loaded: {} samples", samples.len());

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(None);
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);

    let ah = app_handle.clone();
    let rid = recording_id.to_string();
    let last_emit = std::sync::Mutex::new(std::time::Instant::now());
    params.set_progress_callback_safe(move |progress: i32| {
        let mut last = last_emit.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed().as_millis() > 200 || progress >= 100 {
            let _ = ah.emit("transcription_progress", TranscriptionProgress {
                recording_id: rid.clone(),
                stage: "Transcribing".to_string(),
                percent: progress.max(0) as u32,
            });
            *last = std::time::Instant::now();
        }
    });

    eprintln!("[whisper] creating state...");
    let mut state = ctx.create_state().map_err(|e| e.to_string())?;
    eprintln!("[whisper] running inference on {} samples...", samples.len());
    state.full(params, &samples).map_err(|e| e.to_string())?;
    eprintln!("[whisper] inference done");
    
    let mut transcript = String::new();
    let n_segments = state.full_n_segments();
    for i in 0..n_segments {
        if let Some(seg) = state.get_segment(i) {
            if let Ok(text) = seg.to_str_lossy() {
                transcript.push_str(&text);
                transcript.push(' ');
            }
        }
    }
    
    Ok(transcript.trim().to_string())
}
