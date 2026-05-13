use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Mutex;
use crate::config::{TranscriptionProvider, load_settings};
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

#[derive(Clone, Serialize)]
struct TranscriptionProgress {
    recording_id: String,
    stage: String,
    percent: u32,
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

    // Shared metadata fields
    let source = match provider {
        TranscriptionProvider::Unknown => TranscriptSource::Local,
        TranscriptionProvider::FluidAudio => TranscriptSource::Fluidaudio,
        TranscriptionProvider::OpenAI => TranscriptSource::Openai,
        TranscriptionProvider::Google => TranscriptSource::Google,
        TranscriptionProvider::Anthropic => TranscriptSource::Anthropic,
        TranscriptionProvider::AppleSpeech => TranscriptSource::Apple,
    };

    let _ = app_handle.emit("transcription_progress", TranscriptionProgress {
        recording_id: recording_id.clone(),
        stage: "Starting".to_string(),
        percent: 0,
    });

    let transcript_json = match provider {
        TranscriptionProvider::AppleSpeech => {
            let wav_path = recording_dir.join("temp_transcription.wav");
            convert_ogg_to_wav(&audio_path, &wav_path)?;

            let (mut rx, _child) = app_handle.shell().sidecar("apple-speech-sidecar")
                .map_err(|e| format!("Failed to create sidecar command: {}", e))?
                .arg(wav_path.to_str().ok_or("Invalid WAV path")?)
                .spawn()
                .map_err(|e| format!("Failed to spawn Apple Speech sidecar: {}", e))?;

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
        },
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
            let model_id = settings.local_llm.model_id.clone().ok_or(
                "Local LLM summary path requires `local_llm.model_id` in settings.json — set explicitly or remove the auto-summary trigger.",
            )?;
            tokio::task::spawn_blocking(move || {
                crate::local_llm::summarize_with_local(&model_id, &transcript)
            })
            .await
            .map_err(|e| format!("Local LLM task failed: {}", e))??
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
            let model_id = settings.local_llm.model_id.clone().ok_or(
                "Local LLM template path requires `local_llm.model_id` in settings.json — set explicitly or move the template into a pipeline step.",
            )?;
            let prompt = template.prompt.clone();
            let transcript_clone = transcript.clone();
            tokio::task::spawn_blocking(move || {
                crate::local_llm::process_with_local(&model_id, &prompt, &transcript_clone)
            })
            .await
            .map_err(|e| format!("Local LLM task failed: {}", e))??
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

