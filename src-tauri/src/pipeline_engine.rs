use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::fs;
use std::path::PathBuf;
use chrono::Utc;
use tokio::sync::Mutex as TokioMutex;
use crate::pipelines::{ConnectorType, load_pipelines, validate_pipeline, PipelineState, PipelineStatus, StepStatus, PipelineProgressPayload};
use crate::storage::get_data_dir;
use crate::transcription::{TranscriptJson, render_transcript_from_json};
use crate::connectors;

lazy_static::lazy_static! {
    /// Per-(recording, pipeline) execution lock.
    /// Ensures runs of the same pipeline on the same recording execute sequentially.
    static ref PIPELINE_EXEC_LOCKS: std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>> =
        std::sync::Mutex::new(HashMap::new());
}

/// Known topic heading labels (case-insensitive match for Latin)
const TOPIC_LABELS: &[&str] = &["topic", "тема", "tema"];

/// Extract a short title from pipeline step output.
/// Looks for topic section heading, inline topic label, or first content heading.
fn extract_title_from_output(content: &str) -> Option<String> {
    // Skip frontmatter
    let body = if content.starts_with("---") {
        content.splitn(3, "---").nth(2).unwrap_or(content)
    } else {
        content
    };

    let lines: Vec<&str> = body.lines().collect();

    // Pass 1: Look for topic heading — take the next non-empty line as title
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Match: "## Topic", "**Topic**", "**Тема**", "# Topic", etc.
        let heading = trimmed.trim_start_matches('#').trim();
        // Also strip bold markers: **Topic** → Topic
        let clean = heading.trim_matches('*').trim();
        let clean_lower = clean.to_lowercase();

        let is_topic_heading = TOPIC_LABELS.iter().any(|&label| clean_lower == label);

        if is_topic_heading {
            // Check for inline value: "**Topic**: value" or "**Topic:** value"
            // Find if there's text after a colon on the same line
            if let Some(colon_pos) = trimmed.find(':') {
                let after_colon = trimmed[colon_pos + 1..].trim().trim_matches('*').trim();
                if !after_colon.is_empty() {
                    return Some(after_colon.chars().take(100).collect());
                }
            }
            // Otherwise take the next non-empty line
            for next_line in lines.iter().skip(i + 1) {
                let next = next_line.trim();
                if next.is_empty() || next == "---" { continue; }
                let title = next.trim_start_matches(|c: char| c == '-' || c == '*' || c == ' ')
                    .trim_matches(|c: char| c == '*')
                    .trim();
                if !title.is_empty() {
                    return Some(title.chars().take(100).collect());
                }
            }
        }
    }

    // Pass 2: First non-generic heading (skip things like "Summary", "Резюме", etc.)
    const SKIP_HEADINGS: &[&str] = &[
        "summary", "резюме", "краткое резюме", "краткое содержание",
        "краткое саммари разговора", "краткое резюме разговора",
        "краткое содержание разговора", "краткое содержание транскрипта",
        "резюме созвона", "резюме разговора",
    ];
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() && !SKIP_HEADINGS.iter().any(|&s| heading.to_lowercase() == s) {
                return Some(heading.chars().take(100).collect());
            }
        }
    }

    None
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Get the pipeline output directory for a recording
fn get_pipeline_output_dir(recording_id: &str, pipeline_name: &str, run_index: usize) -> PathBuf {
    let dir_name = if run_index == 0 {
        pipeline_name.to_string()
    } else {
        format!("{}.run-{}", pipeline_name, run_index)
    };
    get_data_dir()
        .join(recording_id)
        .join("pipelines")
        .join(dir_name)
}

/// Resolve the input path for a step.
/// For transcript input: renders JSON to a cached .txt file (with .md fallback).
/// For step outputs: returns the step's .md file path.
fn resolve_input_path(
    recording_id: &str,
    pipeline_name: &str,
    input: &str,
    run_index: usize,
) -> PathBuf {
    if input == "transcript" {
        let recording_dir = get_data_dir().join(recording_id);
        let json_path = recording_dir.join("transcript.json");

        if json_path.exists() {
            // DESIGN NOTE: Save connector behavior change (v0.3 -> v0.4)
            //
            // Connectors now receive transcript_rendered.txt (plain text) instead of
            // transcript.md (markdown with YAML frontmatter).
            //
            // RATIONALE:
            // - transcript.json is the source of truth (metadata + text)
            // - transcript_rendered.txt is an ephemeral cache for connector consumption
            // - Connectors process content, not metadata
            // - Metadata stays in transcript.json where it belongs
            // - Separation of concerns: storage (JSON) vs processing (plain text)
            //
            // This is INTENTIONAL design, not a bug. The rendered text file:
            // 1. Contains only the transcript body (plain text)
            // 2. Has no YAML frontmatter (metadata lives in .json)
            // 3. Is generated on-demand for each pipeline execution
            // 4. Is cleaned up after pipeline completes (see execute_pipeline_internal)
            //
            // Previous behavior (v0.3): transcript.md with frontmatter passed directly.
            // Current behavior (v0.4): transcript.json rendered to .txt, frontmatter-free.
            let rendered_path = recording_dir.join("transcript_rendered.txt");
            if let Ok(content) = fs::read_to_string(&json_path) {
                if let Ok(tj) = serde_json::from_str::<TranscriptJson>(&content) {
                    let text = render_transcript_from_json(&tj);
                    let _ = fs::write(&rendered_path, &text);
                    return rendered_path;
                }
            }
        }

        // Fallback: legacy transcript.md
        recording_dir.join("transcript.md")
    } else {
        get_pipeline_output_dir(recording_id, pipeline_name, run_index).join(format!("{}.md", input))
    }
}

/// Writable property types that the LLM should populate.
/// Read-only/computed types are excluded from the format spec.
const WRITABLE_TYPES: &[&str] = &[
    "title", "rich_text", "select", "multi_select", "people",
    "date", "number", "checkbox", "url", "email", "phone_number", "status",
];

/// Maximum number of select/multi_select options to include per field.
/// Prevents token budget overflow for databases with many options.
const MAX_OPTIONS_IN_SPEC: usize = 12;

/// Build an augmented prompt for an LLM step that feeds into a Notion delivery step.
/// Loads the prompt template, substitutes the transcript, then appends a format spec
/// derived from the Notion integration profile.
///
/// Returns `Err` if the profile is missing, corrupt, or has no synced properties —
/// this is a HARD FAIL that prevents the expensive LLM API call.
fn build_augmented_prompt(
    step_config: &serde_json::Value,
    input_path: &std::path::Path,
    notion_integration_id: &str,
) -> Result<String, String> {
    // Load base prompt from template name or inline prompt
    let base_prompt = if let Some(template_name) = step_config
        .get("prompt_template")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let template = crate::prompt_templates::get_prompt_template_internal(template_name)?;
        let raw_input = std::fs::read_to_string(input_path)
            .map_err(|e| format!("Failed to read input file for augmentation: {}", e))?;
        let input_content = crate::connectors::strip_frontmatter(&raw_input);
        crate::prompt_templates::substitute_variables(&template.prompt, input_content)
    } else if let Some(inline_prompt) = step_config
        .get("prompt_inline")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let raw_input = std::fs::read_to_string(input_path)
            .map_err(|e| format!("Failed to read input file for augmentation: {}", e))?;
        let input_content = crate::connectors::strip_frontmatter(&raw_input);
        crate::prompt_templates::substitute_variables(inline_prompt, input_content)
    } else {
        return Err("LLM step missing prompt_template or prompt_inline in config".to_string());
    };

    // Load Notion integration profile — hard fail if missing
    let profile = crate::integrations::notion::load_notion_profile(notion_integration_id)
        .map_err(|_| format!(
            "Cannot augment prompt: Notion integration '{}' profile not found. \
             Sync schema in Settings > Integrations before running this pipeline.",
            notion_integration_id
        ))?;

    // Verify profile has synced properties (not just an empty initial profile)
    if profile.properties.is_empty() || profile.database_id.is_empty() {
        return Err(format!(
            "Cannot augment prompt: Notion integration '{}' has no schema synced. \
             Open Settings > Integrations > {} > Sync Schema before running this pipeline.",
            notion_integration_id,
            profile.name
        ));
    }

    // Build format spec from profile
    let format_spec = build_notion_format_spec(&profile);

    // Append format spec to base prompt
    Ok(format!("{}\n\n{}", base_prompt, format_spec))
}

/// Build a compact JSON format specification from a Notion integration profile.
/// The spec instructs the LLM to output a JSON array matching the database schema.
/// Token budget: typically 100-300 tokens for 5-15 property databases.
fn build_notion_format_spec(
    profile: &crate::integrations::notion::NotionIntegrationProfile,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push("Output a JSON array where each element is an object with these fields:".to_string());
    lines.push(format!("Database: {}", profile.database_name));
    lines.push(String::new());

    for prop in &profile.properties {
        if !WRITABLE_TYPES.contains(&prop.property_type.as_str()) {
            continue;
        }

        let field_spec = match prop.property_type.as_str() {
            "title" => format!("- \"{}\" (string, REQUIRED): page title", prop.name),
            "rich_text" => format!("- \"{}\" (string): long text", prop.name),
            "number" => format!("- \"{}\" (number): numeric value", prop.name),
            "checkbox" => format!("- \"{}\" (boolean): true or false", prop.name),
            "url" => format!("- \"{}\" (string): URL", prop.name),
            "email" => format!("- \"{}\" (string): email address", prop.name),
            "phone_number" => format!("- \"{}\" (string): phone number", prop.name),
            "date" => format!("- \"{}\" (string): ISO 8601 date, e.g. \"2026-02-18\"", prop.name),
            "people" => {
                let aliases: Vec<&str> = profile.people_mappings.iter()
                    .map(|m| m.alias.as_str())
                    .collect();
                if aliases.is_empty() {
                    format!("- \"{}\" (array of strings): person aliases", prop.name)
                } else {
                    format!(
                        "- \"{}\" (array of strings): known aliases are: {}",
                        prop.name,
                        aliases.join(", ")
                    )
                }
            }
            "select" | "status" => {
                if prop.select_options.is_empty() {
                    format!("- \"{}\" (string): one of the defined options", prop.name)
                } else {
                    let (shown, overflow) = if prop.select_options.len() > MAX_OPTIONS_IN_SPEC {
                        (&prop.select_options[..MAX_OPTIONS_IN_SPEC],
                         prop.select_options.len() - MAX_OPTIONS_IN_SPEC)
                    } else {
                        (&prop.select_options[..], 0)
                    };
                    let options_str: Vec<&str> = shown.iter().map(|s| s.as_str()).collect();
                    if overflow > 0 {
                        format!(
                            "- \"{}\" (string): one of: {} (+ {} more)",
                            prop.name,
                            options_str.join(", "),
                            overflow
                        )
                    } else {
                        format!(
                            "- \"{}\" (string): one of: {}",
                            prop.name,
                            options_str.join(", ")
                        )
                    }
                }
            }
            "multi_select" => {
                if prop.select_options.is_empty() {
                    format!("- \"{}\" (array of strings): multiple values from defined options", prop.name)
                } else {
                    let (shown, overflow) = if prop.select_options.len() > MAX_OPTIONS_IN_SPEC {
                        (&prop.select_options[..MAX_OPTIONS_IN_SPEC],
                         prop.select_options.len() - MAX_OPTIONS_IN_SPEC)
                    } else {
                        (&prop.select_options[..], 0)
                    };
                    let options_str: Vec<&str> = shown.iter().map(|s| s.as_str()).collect();
                    if overflow > 0 {
                        format!(
                            "- \"{}\" (array of strings): values from: {} (+ {} more)",
                            prop.name,
                            options_str.join(", "),
                            overflow
                        )
                    } else {
                        format!(
                            "- \"{}\" (array of strings): values from: {}",
                            prop.name,
                            options_str.join(", ")
                        )
                    }
                }
            }
            _ => continue,
        };
        lines.push(field_spec);
    }

    lines.push(String::new());
    lines.push("Omit any field you cannot determine from the transcript. Use null for unknown optional fields.".to_string());
    lines.push("People fields: use the person's name or alias as a string.".to_string());
    lines.push("Return ONLY the JSON array. No prose, no markdown, no code fences.".to_string());

    lines.join("\n")
}

/// Build a compact JSON format specification from a Linear integration profile.
/// The spec instructs the LLM to output a single JSON object (not an array —
/// Linear creates one issue per delivery step, unlike Notion which creates multiple pages).
/// Token budget: typically 50-150 tokens for a typical team schema.
fn build_linear_format_spec(
    profile: &crate::integrations::linear::LinearIntegrationProfile,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push("Output a JSON object with these fields:".to_string());
    lines.push(format!("Team: {}", profile.team_name));
    lines.push(String::new());

    // title (always required)
    lines.push("- \"title\" (string, REQUIRED): issue title".to_string());

    // description
    lines.push("- \"description\" (string): issue description in markdown".to_string());

    // priority — from profile.priorities
    {
        let priority_labels: Vec<&str> = profile.priorities.iter()
            .map(|p| p.label.as_str())
            .collect();
        if priority_labels.is_empty() {
            lines.push("- \"priority\" (string): priority level".to_string());
        } else {
            lines.push(format!(
                "- \"priority\" (string): one of: {}",
                priority_labels.join(", ")
            ));
        }
    }

    // status — from profile.workflow_states
    {
        let all_states: Vec<&str> = profile.workflow_states.iter()
            .map(|s| s.name.as_str())
            .collect();
        if all_states.is_empty() {
            lines.push("- \"status\" (string): workflow state name".to_string());
        } else {
            let (shown, overflow) = if all_states.len() > MAX_OPTIONS_IN_SPEC {
                (&all_states[..MAX_OPTIONS_IN_SPEC], all_states.len() - MAX_OPTIONS_IN_SPEC)
            } else {
                (&all_states[..], 0)
            };
            if overflow > 0 {
                lines.push(format!(
                    "- \"status\" (string): one of: {} (+ {} more)",
                    shown.join(", "),
                    overflow
                ));
            } else {
                lines.push(format!(
                    "- \"status\" (string): one of: {}",
                    shown.join(", ")
                ));
            }
        }
    }

    // labels — from profile.labels
    {
        let all_labels: Vec<&str> = profile.labels.iter()
            .map(|l| l.name.as_str())
            .collect();
        if all_labels.is_empty() {
            lines.push("- \"labels\" (array of strings): label names".to_string());
        } else {
            let (shown, overflow) = if all_labels.len() > MAX_OPTIONS_IN_SPEC {
                (&all_labels[..MAX_OPTIONS_IN_SPEC], all_labels.len() - MAX_OPTIONS_IN_SPEC)
            } else {
                (&all_labels[..], 0)
            };
            if overflow > 0 {
                lines.push(format!(
                    "- \"labels\" (array of strings): values from: {} (+ {} more)",
                    shown.join(", "),
                    overflow
                ));
            } else {
                lines.push(format!(
                    "- \"labels\" (array of strings): values from: {}",
                    shown.join(", ")
                ));
            }
        }
    }

    // assignee — from profile.members (prefer display_name, fallback to name)
    {
        let all_members: Vec<String> = profile.members.iter()
            .map(|m| {
                if !m.display_name.is_empty() {
                    m.display_name.clone()
                } else {
                    m.name.clone()
                }
            })
            .collect();
        let member_refs: Vec<&str> = all_members.iter().map(|s| s.as_str()).collect();
        if member_refs.is_empty() {
            lines.push("- \"assignee\" (string): team member name".to_string());
        } else {
            let (shown, overflow) = if member_refs.len() > MAX_OPTIONS_IN_SPEC {
                (&member_refs[..MAX_OPTIONS_IN_SPEC], member_refs.len() - MAX_OPTIONS_IN_SPEC)
            } else {
                (&member_refs[..], 0)
            };
            if overflow > 0 {
                lines.push(format!(
                    "- \"assignee\" (string): one of: {} (+ {} more)",
                    shown.join(", "),
                    overflow
                ));
            } else {
                lines.push(format!(
                    "- \"assignee\" (string): one of: {}",
                    shown.join(", ")
                ));
            }
        }
    }

    lines.push(String::new());
    lines.push("Omit any field you cannot determine from the transcript. Use null for unknown optional fields.".to_string());
    lines.push("Return ONLY the JSON object. No prose, no markdown, no code fences.".to_string());

    lines.join("\n")
}

/// Build an augmented prompt for an LLM step that feeds into a Linear delivery step.
/// Loads the prompt template, substitutes the transcript, then appends a format spec
/// derived from the Linear integration profile.
///
/// Returns `Err` if the profile is missing, corrupt, or has no synced schema —
/// this is a HARD FAIL that prevents the expensive LLM API call.
fn build_linear_augmented_prompt(
    step_config: &serde_json::Value,
    input_path: &std::path::Path,
    linear_integration_id: &str,
) -> Result<String, String> {
    // Load base prompt from template name or inline prompt
    let base_prompt = if let Some(template_name) = step_config
        .get("prompt_template")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let template = crate::prompt_templates::get_prompt_template_internal(template_name)?;
        let raw_input = std::fs::read_to_string(input_path)
            .map_err(|e| format!("Failed to read input file for augmentation: {}", e))?;
        let input_content = crate::connectors::strip_frontmatter(&raw_input);
        crate::prompt_templates::substitute_variables(&template.prompt, input_content)
    } else if let Some(inline_prompt) = step_config
        .get("prompt_inline")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let raw_input = std::fs::read_to_string(input_path)
            .map_err(|e| format!("Failed to read input file for augmentation: {}", e))?;
        let input_content = crate::connectors::strip_frontmatter(&raw_input);
        crate::prompt_templates::substitute_variables(inline_prompt, input_content)
    } else {
        return Err("LLM step missing prompt_template or prompt_inline in config".to_string());
    };

    // Load Linear integration profile — hard fail if missing
    let profile = crate::integrations::linear::load_linear_profile(linear_integration_id)
        .map_err(|_| format!(
            "Cannot augment prompt: Linear integration '{}' profile not found. \
             Sync schema in Settings > Integrations before running this pipeline.",
            linear_integration_id
        ))?;

    // Verify profile has synced schema (not just an empty initial profile)
    if profile.workflow_states.is_empty() && profile.team_id.is_empty() {
        return Err(format!(
            "Cannot augment prompt: Linear integration '{}' has no schema synced. \
             Open Settings > Integrations > {} > Sync Schema before running this pipeline.",
            linear_integration_id,
            profile.name
        ));
    }

    // Build format spec from profile
    let format_spec = build_linear_format_spec(&profile);

    // Append format spec to base prompt
    Ok(format!("{}\n\n{}", base_prompt, format_spec))
}

/// Validate that an augmented prompt fits within the model's context window.
///
/// Called after `build_augmented_prompt` and before the LLM API call to prevent
/// wasted API costs when the Notion schema has produced an oversized prompt.
///
/// Returns `Err` with an actionable message if the prompt exceeds the provider's
/// context limit. Returns `Ok(())` if the prompt fits within budget.
fn validate_augmented_prompt_budget(
    augmented_prompt: &str,
    step_config: &serde_json::Value,
) -> Result<(), String> {
    let provider = step_config
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai");
    let estimated = connectors::llm::estimate_tokens(augmented_prompt);
    let limit = connectors::llm::context_limit_for_provider(provider);
    if estimated > limit {
        return Err(format!(
            "Augmented prompt ({estimated} est. tokens) exceeds {provider} context limit \
             ({limit} tokens). Your Notion schema may be too large for this model. \
             Try a provider with a larger context window or reduce the number of database properties."
        ));
    }
    Ok(())
}

/// Execute a pipeline for a recording
pub async fn execute_pipeline_internal(
    recording_id: &str,
    pipeline_name: &str,
    app_handle: Option<&tauri::AppHandle>,
) -> Result<PipelineStatus, String> {
    // Serialize execution: if the same pipeline is already running on this recording, wait.
    let lock = {
        let key = format!("{}:{}", recording_id, pipeline_name);
        let mut locks = PIPELINE_EXEC_LOCKS.lock().unwrap();
        locks.entry(key).or_insert_with(|| Arc::new(TokioMutex::new(()))).clone()
    };
    let _exec_guard = lock.lock().await;

    // Load pipeline definition — if deleted since assignment, mark as Partial and bail
    let pipelines = load_pipelines()?;
    let pipeline = match pipelines.get(pipeline_name) {
        Some(p) => p.clone(),
        None => {
            let error_msg = format!("Pipeline '{}' was deleted before execution", pipeline_name);
            eprintln!("[pipeline] {}", error_msg);
            update_pipeline_state(
                recording_id,
                pipeline_name,
                PipelineStatus::Partial,
                None,
                Some(&error_msg),
            )?;
            return Ok(PipelineStatus::Partial);
        }
    };

    validate_pipeline(&pipeline)?;

    // PIPE-01: Zero-step pipeline = label only; skip execution, return Done immediately
    if pipeline.steps.is_empty() {
        update_pipeline_state(recording_id, pipeline_name, PipelineStatus::Done, None, None)?;
        return Ok(PipelineStatus::Done);
    }

    // Verify transcript exists (.json primary, .md fallback)
    let recording_dir = get_data_dir().join(recording_id);
    let has_transcript = recording_dir.join("transcript.json").exists()
        || recording_dir.join("transcript.md").exists();
    if !has_transcript {
        return Err("No transcript found. Transcribe the recording first.".to_string());
    }

    // Resolve run_index from the active (waiting/running) pipeline state.
    // Use .last() to get the most recently assigned entry — if a previous run is still
    // Running when a new run starts, .next() would incorrectly pick the old one.
    let run_index = {
        let states = read_pipeline_states(recording_id);
        states.iter()
            .filter(|s| s.name == pipeline_name && s.status != PipelineStatus::Done && s.status != PipelineStatus::Partial)
            .map(|s| s.run_index)
            .last()
            .unwrap_or(0)
    };

    // Update pipeline state to running
    update_pipeline_state(recording_id, pipeline_name, PipelineStatus::Running, None, None)?;

    let output_dir = get_pipeline_output_dir(recording_id, pipeline_name, run_index);
    let total_steps = pipeline.steps.len();

    // Track which steps have failed or been skipped (by step name).
    // Used to determine whether subsequent steps should be skipped based on their input.
    let mut failed_or_skipped: HashSet<String> = HashSet::new();
    // Track whether any step failed so we can set Partial status at the end.
    let mut has_failure = false;
    // Track the first error message for backward-compatible pipeline state error field.
    let mut first_error: Option<String> = None;

    // Execute steps sequentially
    for (i, step) in pipeline.steps.iter().enumerate() {
        // Check if this step should be skipped because its input came from a failed/skipped step.
        // "transcript" input is always available — never skip based on it.
        let should_skip = step.input != "transcript" && failed_or_skipped.contains(&step.input);

        if should_skip {
            // Write a skipped step .md file so get_step_outputs returns correct status
            let _ = fs::create_dir_all(&output_dir);
            let output_path = output_dir.join(format!("{}.md", step.name));
            let now = chrono::Utc::now().to_rfc3339();
            let skip_note = format!("Skipped because step '{}' failed", step.input);
            let skip_note_escaped = skip_note.replace('"', "\\\"");
            let connector_name = format!("{:?}", step.connector).to_lowercase();
            let file_content = format!(
                "---\nname: {}\ndescription: \"{}\"\nconnector: {}\ninput: {}\nstatus: skipped\ncreated_at: {}\ncompleted_at: {}\nerror: \"{}\"\n---\n\n## Skipped\n{}\n",
                step.name,
                step.description.as_deref().unwrap_or(""),
                connector_name,
                step.input,
                now, now,
                skip_note_escaped,
                skip_note,
            );
            let _ = fs::write(&output_path, &file_content);

            // Emit skipped progress event
            if let Some(app) = app_handle {
                use tauri::Emitter;
                let _ = app.emit(
                    "pipeline-progress",
                    PipelineProgressPayload {
                        recording_id: recording_id.to_string(),
                        pipeline_name: pipeline_name.to_string(),
                        step_name: step.name.clone(),
                        step_index: i,
                        total_steps,
                        status: "skipped".to_string(),
                    },
                );
            }

            // This step is skipped, so its output is also unavailable for downstream steps
            failed_or_skipped.insert(step.name.clone());
            continue;
        }

        // Update current step
        update_pipeline_state(
            recording_id,
            pipeline_name,
            PipelineStatus::Running,
            Some(i),
            None,
        )?;

        // Emit progress event
        if let Some(app) = app_handle {
            use tauri::Emitter;
            let _ = app.emit(
                "pipeline-progress",
                PipelineProgressPayload {
                    recording_id: recording_id.to_string(),
                    pipeline_name: pipeline_name.to_string(),
                    step_name: step.name.clone(),
                    step_index: i,
                    total_steps,
                    status: "running".to_string(),
                },
            );
        }

        let input_path = resolve_input_path(recording_id, pipeline_name, &step.input, run_index);

        if !input_path.exists() {
            let error = format!(
                "Input file not found for step '{}': {}",
                step.name,
                input_path.display()
            );
            // Missing input is treated as a step failure
            if first_error.is_none() {
                first_error = Some(error.clone());
            }
            has_failure = true;
            failed_or_skipped.insert(step.name.clone());

            // Emit failed event
            if let Some(app) = app_handle {
                use tauri::Emitter;
                let _ = app.emit(
                    "pipeline-progress",
                    PipelineProgressPayload {
                        recording_id: recording_id.to_string(),
                        pipeline_name: pipeline_name.to_string(),
                        step_name: step.name.clone(),
                        step_index: i,
                        total_steps,
                        status: "failed".to_string(),
                    },
                );
            }

            // Processing step failure (or input-missing) — skip all remaining steps
            if !step.connector.is_delivery() {
                // Mark all remaining steps as skipped (they depend transitively on this step)
                for remaining in pipeline.steps.iter().skip(i + 1) {
                    failed_or_skipped.insert(remaining.name.clone());
                }
                break;
            }
            // Delivery step with missing input — also treat as blocking since we can't
            // determine independence. Mark remaining and break.
            for remaining in pipeline.steps.iter().skip(i + 1) {
                failed_or_skipped.insert(remaining.name.clone());
            }
            break;
        }

        let step_result = match step.connector {
            ConnectorType::Llm => {
                // N+1 look-ahead: augment prompt if next step is Notion or Linear.
                // Only augments the LLM step directly before a delivery step (v1 scope).
                // Non-contiguous chains like [LLM, Save, Notion] are not augmented.
                let augmented = if let Some(next_step) = pipeline.steps.get(i + 1) {
                    if next_step.connector == ConnectorType::Notion {
                        let integration_id = next_step.config
                            .get("integration_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| format!(
                                "Step '{}': Notion step missing integration_id in config \
                                 (required for prompt augmentation)",
                                next_step.name
                            ))?;
                        Some(build_augmented_prompt(&step.config, &input_path, integration_id)?)
                    } else if next_step.connector == ConnectorType::Linear {
                        let integration_id = next_step.config
                            .get("integration_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| format!(
                                "Step '{}': Linear step missing integration_id in config \
                                 (required for prompt augmentation)",
                                next_step.name
                            ))?;
                        Some(build_linear_augmented_prompt(&step.config, &input_path, integration_id)?)
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Save augmented prompt as sidecar file so it can be shown in the UI
                if let Some(ref aug_text) = augmented {
                    let aug_path = output_dir.join(format!("{}.augmented-prompt.txt", step.name));
                    let _ = fs::create_dir_all(&output_dir);
                    let _ = fs::write(&aug_path, aug_text);
                }

                // Budget check: reject over-limit augmented prompts before making the API call
                if let Some(ref aug_text) = augmented {
                    validate_augmented_prompt_budget(aug_text, &step.config)?;
                }

                connectors::llm::execute(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                    augmented.as_deref(),
                )
                .await
            }
            ConnectorType::Save => {
                connectors::save::execute(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                    pipeline_name,
                    recording_id,
                )
                .await
            }
            ConnectorType::Webhook => {
                connectors::webhook::execute(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                )
                .await
            }
            ConnectorType::Slack => {
                connectors::slack::execute(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                )
                .await
            }
            ConnectorType::Notion => {
                // Use execute_structured to detect JSON parse failures for retry.
                // JSON parse failures trigger exactly one corrective-prompt retry
                // using the same LLM provider/model. Other errors skip retry.
                use connectors::notion::NotionErrorKind;

                let notion_result = connectors::notion::execute_structured(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                )
                .await;

                match notion_result {
                    Ok(path) => Ok(path),
                    Err(ref notion_err) if matches!(notion_err.kind, NotionErrorKind::JsonParse { .. }) => {
                        // Extract raw_output from the JSON parse error
                        let raw_output = match &notion_err.kind {
                            NotionErrorKind::JsonParse { raw_output, .. } => raw_output.clone(),
                            _ => unreachable!(),
                        };

                        eprintln!(
                            "[pipeline] Notion JSON parse failure on step '{}', retrying with corrective prompt",
                            step.name
                        );

                        // Find the LLM step that fed into this Notion step so we can
                        // read its config for provider/model and its output for context.
                        //
                        // The N+1 look-ahead guarantees the immediately-preceding step
                        // is the LLM step that produced the JSON (when the pipeline
                        // is structured as LLM -> Notion).
                        // We look backward from the current Notion step index.
                        let llm_step_for_retry = if i > 0 {
                            pipeline.steps.get(i - 1).filter(|s| s.connector == ConnectorType::Llm)
                        } else {
                            None
                        };

                        let retry_result = if let Some(llm_step) = llm_step_for_retry {
                            // Get the augmented prompt for context (rebuild it for the retry call)
                            let notion_integration_id = step.config
                                .get("integration_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let original_prompt = build_augmented_prompt(
                                &llm_step.config,
                                &resolve_input_path(recording_id, pipeline_name, &llm_step.input, run_index),
                                notion_integration_id,
                            ).unwrap_or_default();

                            connectors::llm::execute_retry(
                                &raw_output,
                                &original_prompt,
                                &llm_step.config,
                                &output_dir,
                                &llm_step.name,
                                &llm_step.input,
                                llm_step.description.as_deref(),
                            )
                            .await
                        } else {
                            // No preceding LLM step found — cannot retry
                            Err(format!(
                                "Notion JSON parse failure on step '{}': no preceding LLM step found for retry",
                                step.name
                            ))
                        };

                        match retry_result {
                            Ok(retry_output_path) => {
                                // Retry LLM call succeeded — attempt Notion step again with retry output
                                connectors::notion::execute_with_raw_preservation(
                                    &retry_output_path,
                                    &step.config,
                                    &output_dir,
                                    &step.name,
                                    &step.input,
                                    step.description.as_deref(),
                                    Some(&raw_output),
                                )
                                .await
                            }
                            Err(retry_llm_error) => {
                                // Retry LLM call failed — write failure output with raw output preserved
                                let error_message = format!(
                                    "Notion step '{}' failed with JSON parse error, and retry LLM call also failed: {}",
                                    step.name, retry_llm_error
                                );

                                let _ = fs::create_dir_all(&output_dir);
                                let output_path = output_dir.join(format!("{}.md", step.name));
                                let integration_id = step.config
                                    .get("integration_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let now = chrono::Utc::now().to_rfc3339();
                                let error_escaped = error_message.replace('"', "\\\"").replace('\n', " ");
                                let file_content = format!(
                                    "---\nname: {}\ndescription: \"{}\"\nconnector: notion\ninput: {}\nstatus: failed\ncreated_at: {}\ncompleted_at: {}\nintegration_id: {}\npages_created: 0\nerror: \"{}\"\n---\n\n## Error\n{}\n\n## Raw AI Output\n{}\n",
                                    step.name,
                                    step.description.as_deref().unwrap_or("Create Notion pages"),
                                    step.input,
                                    now, now,
                                    integration_id,
                                    error_escaped,
                                    error_message,
                                    raw_output,
                                );
                                let _ = fs::write(&output_path, &file_content);

                                Err(error_message)
                            }
                        }
                    }
                    Err(other_err) => {
                        // Non-JSON error (API failure, config error, etc.) — no retry
                        Err(other_err.to_string())
                    }
                }
            }
            ConnectorType::Linear => {
                // Use execute_structured to detect JSON parse failures for retry.
                // JSON parse failures trigger exactly one corrective-prompt retry
                // using the same LLM provider/model. Other errors skip retry.
                use connectors::linear::LinearErrorKind;

                let linear_result = connectors::linear::execute_structured(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                )
                .await;

                match linear_result {
                    Ok(path) => Ok(path),
                    Err(ref linear_err) if matches!(linear_err.kind, LinearErrorKind::JsonParse { .. }) => {
                        let raw_output = match &linear_err.kind {
                            LinearErrorKind::JsonParse { raw_output, .. } => raw_output.clone(),
                            _ => unreachable!(),
                        };

                        eprintln!(
                            "[pipeline] Linear JSON parse failure on step '{}', retrying with corrective prompt",
                            step.name
                        );

                        let llm_step_for_retry = if i > 0 {
                            pipeline.steps.get(i - 1).filter(|s| s.connector == ConnectorType::Llm)
                        } else {
                            None
                        };

                        let retry_result = if let Some(llm_step) = llm_step_for_retry {
                            // Rebuild augmented prompt for retry — uses same schema guidance as original call
                            let linear_integration_id = step.config
                                .get("integration_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let original_prompt = build_linear_augmented_prompt(
                                &llm_step.config,
                                &resolve_input_path(recording_id, pipeline_name, &llm_step.input, run_index),
                                linear_integration_id,
                            ).unwrap_or_default();

                            connectors::llm::execute_retry(
                                &raw_output,
                                &original_prompt,
                                &llm_step.config,
                                &output_dir,
                                &llm_step.name,
                                &llm_step.input,
                                llm_step.description.as_deref(),
                            )
                            .await
                        } else {
                            Err(format!(
                                "Linear JSON parse failure on step '{}': no preceding LLM step found for retry",
                                step.name
                            ))
                        };

                        match retry_result {
                            Ok(retry_output_path) => {
                                connectors::linear::execute_with_raw_preservation(
                                    &retry_output_path,
                                    &step.config,
                                    &output_dir,
                                    &step.name,
                                    &step.input,
                                    step.description.as_deref(),
                                    Some(&raw_output),
                                )
                                .await
                            }
                            Err(retry_llm_error) => {
                                let error_message = format!(
                                    "Linear step '{}' failed with JSON parse error, and retry LLM call also failed: {}",
                                    step.name, retry_llm_error
                                );

                                let _ = fs::create_dir_all(&output_dir);
                                let output_path = output_dir.join(format!("{}.md", step.name));
                                let integration_id = step.config
                                    .get("integration_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let now = chrono::Utc::now().to_rfc3339();
                                let error_escaped = error_message.replace('"', "\\\"").replace('\n', " ");
                                let file_content = format!(
                                    "---\nname: {}\ndescription: \"{}\"\nconnector: linear\ninput: {}\nstatus: failed\ncreated_at: {}\ncompleted_at: {}\nintegration_id: {}\nissues_created: 0\nerror: \"{}\"\n---\n\n## Error\n{}\n\n## Raw AI Output\n{}\n",
                                    step.name,
                                    step.description.as_deref().unwrap_or("Create Linear issue"),
                                    step.input,
                                    now, now,
                                    integration_id,
                                    error_escaped,
                                    error_message,
                                    raw_output,
                                );
                                let _ = fs::write(&output_path, &file_content);

                                Err(error_message)
                            }
                        }
                    }
                    Err(other_err) => {
                        Err(other_err.to_string())
                    }
                }
            }
            ConnectorType::Mcp => {
                connectors::mcp::execute(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                )
                .await
            }
            ConnectorType::CliAgent => {
                connectors::cli_agent::execute(
                    &input_path,
                    &step.config,
                    &output_dir,
                    &step.name,
                    &step.input,
                    step.description.as_deref(),
                )
                .await
            }
        };

        match step_result {
            Ok(_) => {
                // Emit step done
                if let Some(app) = app_handle {
                    use tauri::Emitter;
                    let _ = app.emit(
                        "pipeline-progress",
                        PipelineProgressPayload {
                            recording_id: recording_id.to_string(),
                            pipeline_name: pipeline_name.to_string(),
                            step_name: step.name.clone(),
                            step_index: i,
                            total_steps,
                            status: "done".to_string(),
                        },
                    );
                }
            }
            Err(ref error) => {
                // Record the first error for backward-compatible pipeline state
                if first_error.is_none() {
                    first_error = Some(error.clone());
                }
                has_failure = true;
                failed_or_skipped.insert(step.name.clone());

                // Emit failed event
                if let Some(app) = app_handle {
                    use tauri::Emitter;
                    let _ = app.emit(
                        "pipeline-progress",
                        PipelineProgressPayload {
                            recording_id: recording_id.to_string(),
                            pipeline_name: pipeline_name.to_string(),
                            step_name: step.name.clone(),
                            step_index: i,
                            total_steps,
                            status: "failed".to_string(),
                        },
                    );
                }

                if !step.connector.is_delivery() {
                    // Processing step failure — all downstream steps depend on this output.
                    // Mark all remaining steps as skipped and stop executing.
                    for remaining in pipeline.steps.iter().skip(i + 1) {
                        failed_or_skipped.insert(remaining.name.clone());
                    }
                    break;
                }
                // Delivery step failure — continue to the next step.
                // Downstream steps whose input is NOT this step will execute normally.
                // Downstream steps whose input IS this step will be skipped (checked at loop top).
            }
        }
    }

    // Write skipped step files for any steps in failed_or_skipped that don't yet have .md files.
    // (Processing-halt scenario: remaining steps are added to the set but never visited in the loop.)
    let _ = fs::create_dir_all(&output_dir);
    for step in &pipeline.steps {
        if failed_or_skipped.contains(&step.name) {
            let output_path = output_dir.join(format!("{}.md", step.name));
            if !output_path.exists() {
                let now = chrono::Utc::now().to_rfc3339();
                let skip_reason = if step.input != "transcript" && failed_or_skipped.contains(&step.input) {
                    format!("Skipped because step '{}' failed", step.input)
                } else {
                    "Skipped because a preceding processing step failed".to_string()
                };
                let skip_reason_escaped = skip_reason.replace('"', "\\\"");
                let connector_name = format!("{:?}", step.connector).to_lowercase();
                let file_content = format!(
                    "---\nname: {}\ndescription: \"{}\"\nconnector: {}\ninput: {}\nstatus: skipped\ncreated_at: {}\ncompleted_at: {}\nerror: \"{}\"\n---\n\n## Skipped\n{}\n",
                    step.name,
                    step.description.as_deref().unwrap_or(""),
                    connector_name,
                    step.input,
                    now, now,
                    skip_reason_escaped,
                    skip_reason,
                );
                let _ = fs::write(&output_path, &file_content);
            }
        }
    }

    // Determine final pipeline status
    let final_status = if has_failure {
        PipelineStatus::Partial
    } else {
        PipelineStatus::Done
    };

    update_pipeline_state(
        recording_id,
        pipeline_name,
        final_status.clone(),
        None,
        first_error.as_deref(),
    )?;

    // Auto-title: if recording still has default title, extract one from the first processing step output
    if final_status == PipelineStatus::Done || final_status == PipelineStatus::Partial {
        if let Ok(meta) = crate::storage::read_metadata(recording_id) {
            let needs_title = meta.title.is_empty()
                || meta.title == "Untitled Recording"
                || meta.title.starts_with("Recording ");
            if needs_title {
                // Find first non-delivery step that succeeded
                if let Some(step) = pipeline.steps.iter().find(|s| !s.connector.is_delivery() && !failed_or_skipped.contains(&s.name)) {
                    let step_output = output_dir.join(format!("{}.md", step.name));
                    if let Ok(content) = fs::read_to_string(&step_output) {
                        if let Some(title) = extract_title_from_output(&content) {
                            let _ = crate::storage::update_title(recording_id, title);
                        }
                    }
                }
            }
        }
    }

    // Clean up temporary rendered transcript file
    let rendered_path = get_data_dir().join(recording_id).join("transcript_rendered.txt");
    let _ = fs::remove_file(&rendered_path);

    Ok(final_status)
}

/// Acquire an exclusive file lock using platform-appropriate mechanism.
/// On macOS/Unix, uses flock(2). Returns a guard that releases the lock on drop.
struct FileLockGuard {
    _lock_file: fs::File,
    lock_path: PathBuf,
}

impl FileLockGuard {
    fn acquire(lock_path: &PathBuf) -> Result<Self, String> {
        let lock_file = fs::File::create(lock_path)
            .map_err(|e| format!("Failed to create lock file: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = lock_file.as_raw_fd();
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
            if ret != 0 {
                return Err("Failed to acquire metadata lock".to_string());
            }
        }

        // On non-Unix platforms, the file create itself provides basic exclusion
        // since we do atomic read-modify-write within the lock scope.

        Ok(FileLockGuard {
            _lock_file: lock_file,
            lock_path: lock_path.clone(),
        })
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Lock is released when _lock_file is dropped (fd closed).
        // Clean up lock file (best effort).
        let _ = fs::remove_file(&self.lock_path);
    }
}

/// Update pipeline state in recording metadata.
/// Uses file-level locking to prevent concurrent modifications from corrupting state.
fn update_pipeline_state(
    recording_id: &str,
    pipeline_name: &str,
    status: PipelineStatus,
    current_step: Option<usize>,
    error: Option<&str>,
) -> Result<(), String> {
    let recording_dir = get_data_dir().join(recording_id);
    let metadata_path = recording_dir.join("metadata.json");
    let lock_path = recording_dir.join(".metadata.lock");

    // Acquire file lock for atomic read-modify-write
    let _lock = FileLockGuard::acquire(&lock_path)?;

    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // Read current metadata JSON (preserves all fields including unknown ones)
    let content = fs::read_to_string(&metadata_path)
        .map_err(|e| format!("Failed to read metadata: {}", e))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse metadata: {}", e))?;

    // Extract existing pipeline states
    let mut pipeline_states: Vec<PipelineState> = json
        .get("pipelines")
        .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
        .unwrap_or_default();

    // Find the active (non-completed) entry for this pipeline, or create a new one.
    // Search from the end so we target the most recently assigned run when multiple
    // active entries exist (e.g. a previous run still Running when a new run starts).
    if let Some(state) = pipeline_states.iter_mut().rev().find(|s| s.name == pipeline_name && s.status != PipelineStatus::Done && s.status != PipelineStatus::Partial) {
        state.status = status.clone();
        state.current_step = current_step;
        if status == PipelineStatus::Running && state.started_at.is_none() {
            state.started_at = Some(now.clone());
        }
        if status == PipelineStatus::Done || status == PipelineStatus::Partial {
            state.completed_at = Some(now);
        }
        state.error = error.map(|e| e.to_string());
    } else {
        pipeline_states.push(PipelineState {
            id: uuid::Uuid::new_v4().to_string(),
            name: pipeline_name.to_string(),
            status,
            run_index: 0,
            started_at: Some(now.clone()),
            completed_at: None,
            current_step,
            error: error.map(|e| e.to_string()),
        });
    }

    // Write pipeline states back to JSON
    json["pipelines"] = serde_json::to_value(&pipeline_states)
        .map_err(|e| format!("Failed to serialize pipeline states: {}", e))?;

    // Atomic write via temp file + rename
    let temp_path = metadata_path.with_extension("json.tmp");
    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
    fs::write(&temp_path, &updated)
        .map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;

    Ok(())
}

/// Read pipeline states directly from metadata JSON file on disk.
/// This avoids the RecordingMetadata struct (which doesn't include a pipelines field)
/// by reading the raw JSON and extracting the pipeline states.
///
/// Migrates old entries that lack an `id` field: assigns a stable UUID and persists it,
/// so subsequent reads (e.g. delete) can match by the same ID.
fn read_pipeline_states(recording_id: &str) -> Vec<PipelineState> {
    let recording_dir = get_data_dir().join(recording_id);
    let metadata_path = recording_dir.join("metadata.json");

    let Ok(content) = fs::read_to_string(&metadata_path) else { return Vec::new() };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else { return Vec::new() };
    let Some(pipelines) = json.get("pipelines") else { return Vec::new() };

    // Backfill missing `id` fields so serde default doesn't generate a new UUID each read
    if let Some(arr) = pipelines.as_array() {
        let needs_migration = arr.iter().any(|e| {
            e.get("id").and_then(|v| v.as_str()).map_or(true, |s| s.is_empty())
        });
        if needs_migration {
            // Acquire lock BEFORE re-reading to avoid overwriting concurrent changes
            let lock_path = recording_dir.join(".metadata.lock");
            if let Ok(_lock) = FileLockGuard::acquire(&lock_path) {
                // Re-read file under lock to get the latest state
                if let Ok(locked_content) = fs::read_to_string(&metadata_path) {
                    if let Ok(mut locked_json) = serde_json::from_str::<serde_json::Value>(&locked_content) {
                        if let Some(locked_arr) = locked_json.get("pipelines").and_then(|v| v.as_array()) {
                            let mut migrated = locked_arr.clone();
                            for entry in &mut migrated {
                                let missing = entry.get("id").and_then(|v| v.as_str()).map_or(true, |s| s.is_empty());
                                if missing {
                                    if let Some(obj) = entry.as_object_mut() {
                                        obj.insert("id".to_string(), serde_json::Value::String(uuid::Uuid::new_v4().to_string()));
                                    }
                                }
                            }
                            locked_json["pipelines"] = serde_json::Value::Array(migrated);
                            if let Ok(updated) = serde_json::to_string_pretty(&locked_json) {
                                let temp_path = metadata_path.with_extension("json.tmp");
                                if fs::write(&temp_path, &updated).is_ok() {
                                    let _ = fs::rename(&temp_path, &metadata_path);
                                }
                            }
                            // Return states from the locked read (authoritative)
                            return locked_json.get("pipelines")
                                .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
                                .unwrap_or_default();
                        }
                    }
                }
            }
        }
    }

    json.get("pipelines")
        .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
        .unwrap_or_default()
}

/// Execute a pipeline (Tauri command)
#[tauri::command]
pub async fn execute_pipeline(
    app_handle: tauri::AppHandle,
    recording_id: String,
    pipeline_name: String,
) -> Result<String, String> {
    let status =
        execute_pipeline_internal(&recording_id, &pipeline_name, Some(&app_handle)).await?;

    match status {
        PipelineStatus::Done => Ok("done".to_string()),
        PipelineStatus::Partial => Ok("partial".to_string()),
        _ => Ok(format!("{:?}", status)),
    }
}

/// Get pipeline execution status for a recording
#[tauri::command]
pub fn get_pipeline_status(
    recording_id: String,
    pipeline_name: String,
) -> Result<Option<PipelineState>, String> {
    let states = read_pipeline_states(&recording_id);
    Ok(states.into_iter().find(|s| s.name == pipeline_name))
}

/// Get all pipeline states for a recording
#[tauri::command]
pub fn get_all_pipeline_states(recording_id: String) -> Result<Vec<PipelineState>, String> {
    Ok(read_pipeline_states(&recording_id))
}

/// Get step outputs for a pipeline execution
#[tauri::command]
pub fn get_step_outputs(
    recording_id: String,
    pipeline_name: String,
    run_index: Option<usize>,
) -> Result<Vec<StepStatus>, String> {
    let output_dir = get_pipeline_output_dir(&recording_id, &pipeline_name, run_index.unwrap_or(0));

    if !output_dir.exists() {
        return Ok(Vec::new());
    }

    // Load pipeline definition to get step order
    // If pipeline was deleted, return empty — the pipeline state error field tells the story
    let pipelines = load_pipelines()?;
    let Some(pipeline) = pipelines.get(&pipeline_name) else {
        return Ok(Vec::new());
    };

    let mut statuses = Vec::new();

    for step in &pipeline.steps {
        let step_file = output_dir.join(format!("{}.md", step.name));
        if step_file.exists() {
            let content = fs::read_to_string(&step_file).unwrap_or_default();
            // Parse status, error, and duration from frontmatter
            let (status, error, duration_secs, output) = parse_step_status(&content);
            // Load augmented prompt from sidecar file if present
            let aug_path = output_dir.join(format!("{}.augmented-prompt.txt", step.name));
            let augmented_prompt = if aug_path.exists() {
                fs::read_to_string(&aug_path).ok()
            } else {
                None
            };
            statuses.push(StepStatus {
                name: step.name.clone(),
                status,
                error,
                duration_secs,
                augmented_prompt,
                output,
            });
        } else {
            statuses.push(StepStatus {
                name: step.name.clone(),
                status: "pending".to_string(),
                error: None,
                duration_secs: None,
                augmented_prompt: None,
                output: None,
            });
        }
    }

    Ok(statuses)
}

/// Parse step status from frontmatter
/// Returns (status, error, duration_secs, output)
fn parse_step_status(content: &str) -> (String, Option<String>, Option<f64>, Option<String>) {
    if let Some(stripped) = content.strip_prefix("---")
        && let Some(end_idx) = stripped.find("---")
    {
        let frontmatter = &stripped[..end_idx];
        let mut status = "pending".to_string();
        let mut error = None;
        let mut created_at: Option<String> = None;
        let mut completed_at: Option<String> = None;

        for line in frontmatter.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("status:") {
                status = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("error:") {
                let err_val = val.trim();
                if err_val != "null" {
                    error = Some(err_val.trim_matches('"').to_string());
                }
            } else if let Some(val) = line.strip_prefix("created_at:") {
                let ts = val.trim().trim_matches('"').to_string();
                if !ts.is_empty() && ts != "null" {
                    created_at = Some(ts);
                }
            } else if let Some(val) = line.strip_prefix("completed_at:") {
                let ts = val.trim().trim_matches('"').to_string();
                if !ts.is_empty() && ts != "null" {
                    completed_at = Some(ts);
                }
            }
        }

        let duration_secs = match (&created_at, &completed_at) {
            (Some(c), Some(d)) => {
                let start = chrono::DateTime::parse_from_rfc3339(c).ok();
                let end = chrono::DateTime::parse_from_rfc3339(d).ok();
                match (start, end) {
                    (Some(s), Some(e)) => {
                        let dur = e.signed_duration_since(s);
                        Some(dur.num_milliseconds() as f64 / 1000.0)
                    }
                    _ => None,
                }
            }
            _ => None,
        };

        // Extract body content after the closing frontmatter delimiter
        let body_start = end_idx + 3; // skip past "---"
        let body = stripped[body_start..].trim();
        let output = if body.is_empty() { None } else { Some(body.to_string()) };

        return (status, error, duration_secs, output);
    }

    ("pending".to_string(), None, None, None)
}

/// Remove a pipeline run from a recording's metadata and delete its output directory.
#[tauri::command]
pub fn remove_pipeline_run(recording_id: String, run_id: String) -> Result<(), String> {
    let recording_dir = get_data_dir().join(&recording_id);
    let metadata_path = recording_dir.join("metadata.json");
    let lock_path = recording_dir.join(".metadata.lock");

    let _lock = FileLockGuard::acquire(&lock_path)?;

    let content = fs::read_to_string(&metadata_path)
        .map_err(|e| format!("Failed to read metadata: {}", e))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse metadata: {}", e))?;

    // Work with raw JSON array to avoid serde default generating new random UUIDs
    // for legacy entries without persisted `id` fields (which would never match run_id)
    let pipelines_arr = json.get("pipelines")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "No pipelines array in metadata".to_string())?;

    let pos = pipelines_arr.iter().position(|entry| {
        entry.get("id").and_then(|v| v.as_str()) == Some(run_id.as_str())
    });

    let idx = match pos {
        Some(i) => i,
        None => return Err(format!("Pipeline run '{}' not found", run_id)),
    };

    // Extract name and run_index for output dir cleanup before removing
    let removed = &pipelines_arr[idx];
    let removed_name = removed.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let removed_run_index = removed.get("run_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let mut arr = pipelines_arr.clone();
    arr.remove(idx);
    json["pipelines"] = serde_json::Value::Array(arr);

    let temp_path = metadata_path.with_extension("json.tmp");
    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
    fs::write(&temp_path, &updated)
        .map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;

    // Delete the output directory (best effort)
    let output_dir = get_pipeline_output_dir(&recording_id, &removed_name, removed_run_index);
    let _ = fs::remove_dir_all(&output_dir);

    Ok(())
}

/// Assign a pipeline to a recording (sets status to "waiting")
#[tauri::command]
pub fn assign_pipeline(recording_id: String, pipeline_name: String) -> Result<(), String> {
    // Verify pipeline exists
    let pipelines = load_pipelines()?;
    if !pipelines.contains_key(&pipeline_name) {
        return Err(format!("Pipeline '{}' not found", pipeline_name));
    }

    let recording_dir = get_data_dir().join(&recording_id);
    let metadata_path = recording_dir.join("metadata.json");
    let lock_path = recording_dir.join(".metadata.lock");

    let _lock = FileLockGuard::acquire(&lock_path)?;

    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let content = fs::read_to_string(&metadata_path)
        .map_err(|e| format!("Failed to read metadata: {}", e))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse metadata: {}", e))?;

    let mut pipeline_states: Vec<PipelineState> = json
        .get("pipelines")
        .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
        .unwrap_or_default();

    // Count existing runs of this pipeline to determine run_index
    let run_index = pipeline_states.iter().filter(|s| s.name == pipeline_name).count();

    pipeline_states.push(PipelineState {
        id: uuid::Uuid::new_v4().to_string(),
        name: pipeline_name,
        status: PipelineStatus::Waiting,
        run_index,
        started_at: Some(now),
        completed_at: None,
        current_step: None,
        error: None,
    });

    json["pipelines"] = serde_json::to_value(&pipeline_states)
        .map_err(|e| format!("Failed to serialize pipeline states: {}", e))?;

    let temp_path = metadata_path.with_extension("json.tmp");
    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
    fs::write(&temp_path, &updated)
        .map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_input_path_transcript() {
        let path = resolve_input_path("abc-123", "my-pipeline", "transcript", 0);
        assert!(path.to_string_lossy().contains("abc-123"));
        // Falls back to transcript.md when no .json exists
        assert!(
            path.to_string_lossy().ends_with("transcript.md")
            || path.to_string_lossy().ends_with("transcript_rendered.txt")
        );
    }

    #[test]
    fn test_resolve_input_path_step() {
        let path = resolve_input_path("abc-123", "my-pipeline", "summarize", 0);
        assert!(path.to_string_lossy().contains("pipelines/my-pipeline"));
        assert!(path.to_string_lossy().ends_with("summarize.md"));
    }

    #[test]
    fn test_parse_step_status_done() {
        let content = "---\nname: test\nstatus: done\nerror: null\n---\n\nContent";
        let (status, error, _duration, output) = parse_step_status(content);
        assert_eq!(status, "done");
        assert!(error.is_none());
        assert_eq!(output.unwrap(), "Content");
    }

    #[test]
    fn test_parse_step_status_failed() {
        let content =
            "---\nname: test\nstatus: failed\nerror: \"API error: 401\"\n---\n\nFailed";
        let (status, error, _duration, output) = parse_step_status(content);
        assert_eq!(status, "failed");
        assert_eq!(error.unwrap(), "API error: 401");
        assert_eq!(output.unwrap(), "Failed");
    }

    #[test]
    fn test_parse_step_status_no_frontmatter() {
        let content = "Plain content without frontmatter";
        let (status, error, _duration, output) = parse_step_status(content);
        assert_eq!(status, "pending");
        assert!(error.is_none());
        assert!(output.is_none());
    }

    #[test]
    fn test_pipeline_status_serialization() {
        let state = PipelineState {
            id: "test-id".to_string(),
            name: "meeting-notes".to_string(),
            status: PipelineStatus::Done,
            run_index: 0,
            started_at: Some("2026-02-03T12:00:00Z".to_string()),
            completed_at: Some("2026-02-03T12:00:10Z".to_string()),
            current_step: None,
            error: None,
        };

        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"done\""));
        assert!(json.contains("meeting-notes"));

        let deserialized: PipelineState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.status, PipelineStatus::Done);
    }

    #[test]
    fn test_pipeline_output_dir() {
        let dir = get_pipeline_output_dir("abc-123", "my-pipeline", 0);
        assert!(dir.to_string_lossy().contains("abc-123"));
        assert!(dir.to_string_lossy().contains("pipelines/my-pipeline"));
    }

    #[test]
    fn test_pipeline_output_dir_isolation() {
        // PIPE-05: Each pipeline gets its own output directory
        let dir_a = get_pipeline_output_dir("rec-1", "pipeline-a", 0);
        let dir_b = get_pipeline_output_dir("rec-1", "pipeline-b", 0);
        assert_ne!(dir_a, dir_b);
        assert!(dir_a.to_string_lossy().contains("pipelines/pipeline-a"));
        assert!(dir_b.to_string_lossy().contains("pipelines/pipeline-b"));
    }
}
