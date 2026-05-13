use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use crate::config::load_settings;
use crate::prompt_templates::{get_prompt_template_internal, substitute_variables};
use crate::cloud_ai;

/// LLM connector config parsed from step config JSON
#[derive(Debug)]
struct LlmConfig {
    prompt_template: Option<String>,
    prompt_inline: Option<String>,
    provider: String,
    model: String,
}

impl LlmConfig {
    fn from_value(config: &serde_json::Value) -> Result<Self, String> {
        let prompt_template = config
            .get("prompt_template")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());

        let prompt_inline = config
            .get("prompt_inline")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());

        if prompt_template.is_none() && prompt_inline.is_none() {
            return Err("LLM connector config missing 'prompt_template' or 'prompt_inline'".to_string());
        }

        let provider = config
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("openai")
            .to_string();

        let model = config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| default_model_for_provider(&provider))
            .to_string();

        Ok(LlmConfig {
            prompt_template,
            prompt_inline,
            provider,
            model,
        })
    }
}

fn default_model_for_provider(provider: &str) -> &str {
    match provider {
        "openai" => "gpt-4o",
        "google" => "gemini-1.5-flash",
        "anthropic" => "claude-sonnet-4-20250514",
        "local" => "local",
        "ollama" => "llama3.2",
        "cli_agent" => "default",
        _ => "gpt-4o",
    }
}

/// Approximate token count using word-based heuristic.
/// Average English word is ~1.3 tokens; we use chars/3.5 as a balanced estimate
/// that accounts for whitespace, punctuation, and multi-byte characters.
pub fn estimate_tokens(text: &str) -> usize {
    // Use word count * 1.3 as primary estimate, with char-based floor
    let word_count = text.split_whitespace().count();
    let word_estimate = (word_count as f64 * 1.3) as usize;
    let char_estimate = text.len() / 4;
    // Use the higher estimate for safety
    word_estimate.max(char_estimate)
}

/// Get the context window limit (in tokens) for a provider
pub fn context_limit_for_provider(provider: &str) -> usize {
    match provider {
        "openai" => 120_000,
        "google" => 900_000,
        "anthropic" => 190_000,
        "local" => {
            let settings = load_settings();
            settings.local_llm.context_size as usize
        }
        "ollama" => 128_000,
        "cli_agent" => 190_000,
        _ => 120_000,
    }
}

/// Step output frontmatter
struct StepFrontmatter {
    name: String,
    description: String,
    connector: String,
    input: String,
    status: String,
    created_at: String,
    completed_at: Option<String>,
    error: Option<String>,
    provider: String,
    model: String,
}

impl StepFrontmatter {
    fn to_yaml(&self) -> String {
        let error_str = match &self.error {
            Some(e) => format!("\"{}\"", e.replace('"', "\\\"").replace('\n', " ")),
            None => "null".to_string(),
        };
        let completed_str = match &self.completed_at {
            Some(t) => t.clone(),
            None => "null".to_string(),
        };
        format!(
            "---\nname: {}\ndescription: \"{}\"\nconnector: {}\ninput: {}\nstatus: {}\ncreated_at: {}\ncompleted_at: {}\nerror: {}\nprovider: {}\nmodel: {}\n---",
            self.name,
            self.description.replace('"', "\\\""),
            self.connector,
            self.input,
            self.status,
            self.created_at,
            completed_str,
            error_str,
            self.provider,
            self.model,
        )
    }
}

/// Execute the LLM connector for a pipeline step
///
/// - `input_path`: path to input file (transcript or previous step output)
/// - `config`: connector-specific config from pipeline definition
/// - `output_dir`: directory to write step output
/// - `step_name`: name of the current step
/// - `step_input`: input reference name (for frontmatter)
/// - `step_description`: optional description
/// - `augmented_prompt`: optional pre-built prompt (used when LLM feeds into Notion step).
///   When `Some`, skips internal template loading and uses the provided prompt directly.
///   When `None`, behavior is identical to the pre-Phase-3 code path.
/// Execute an LLM step against raw text input, returning the model's output text.
/// No file I/O, no frontmatter, no augmentation. Used by Quick Dictate to chain
/// LLM-only pipeline steps in memory.
pub async fn execute_inline(
    config: &serde_json::Value,
    input_text: &str,
) -> Result<String, String> {
    let llm_config = LlmConfig::from_value(config)?;

    let prompt_text = if let Some(ref template_name) = llm_config.prompt_template {
        let template = get_prompt_template_internal(template_name)?;
        template.prompt.clone()
    } else if let Some(ref inline) = llm_config.prompt_inline {
        inline.clone()
    } else {
        return Err("LLM step missing prompt_template or prompt_inline".to_string());
    };

    let full_prompt = substitute_variables(&prompt_text, input_text);

    let approx_tokens = estimate_tokens(&full_prompt);
    let context_limit = context_limit_for_provider(&llm_config.provider);
    let prompt_to_send = if approx_tokens > context_limit {
        let max_chars = context_limit * 4;
        if full_prompt.len() > max_chars {
            let safe = full_prompt.floor_char_boundary(max_chars);
            full_prompt[..safe].to_string()
        } else {
            full_prompt
        }
    } else {
        full_prompt
    };

    let settings = load_settings();
    match llm_config.provider.as_str() {
        "openai" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_gpt4o(&api_key, &prompt_to_send, "", &llm_config.model).await
        }
        "google" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "google")
                .ok_or("Google API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_gemini(&api_key, &prompt_to_send, "", &llm_config.model).await
        }
        "anthropic" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "anthropic")
                .ok_or("Anthropic API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_claude(&api_key, &prompt_to_send, "", &llm_config.model).await
        }
        "local" => {
            // model_id MUST come from step config — no global default fallback.
            let model_id = llm_config.model.clone();
            if model_id.is_empty() {
                return Err(
                    "Local LLM step needs 'model' in pipeline config (e.g. \"qwen3-4b\")"
                        .to_string(),
                );
            }
            let prompt_clone = prompt_to_send.clone();
            tokio::task::spawn_blocking(move || {
                crate::local_llm::process_with_local(&model_id, &prompt_clone, "")
            })
            .await
            .map_err(|e| format!("Local LLM task failed: {}", e))?
        }
        "ollama" => {
            cloud_ai::process_with_openai_compat(
                "http://localhost:11434",
                None,
                &llm_config.model,
                &prompt_to_send,
                "",
            )
            .await
        }
        "cli_agent" => {
            let cli_config = settings.cli_agent.clone();
            let model_override = if llm_config.model.is_empty() || llm_config.model == "default" {
                cli_config.model.as_deref().map(|s| s.to_string())
            } else {
                Some(llm_config.model.clone())
            };
            super::cli_agent::process_with_cli(
                &cli_config.cli,
                &prompt_to_send,
                "",
                model_override.as_deref(),
                cli_config.timeout_secs,
            )
            .await
        }
        other => Err(format!("Unknown LLM provider: '{}'", other)),
    }
}

pub async fn execute(
    input_path: &Path,
    config: &serde_json::Value,
    output_dir: &Path,
    step_name: &str,
    step_input: &str,
    step_description: Option<&str>,
    augmented_prompt: Option<&str>,
) -> Result<PathBuf, String> {
    let llm_config = LlmConfig::from_value(config)?;
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let (prompt_to_send, truncated) = if let Some(augmented) = augmented_prompt {
        // Engine has pre-built the complete prompt — skip template loading and file reading.
        // Still apply token estimation and context limit truncation.
        let approx_tokens = estimate_tokens(augmented);
        let context_limit = context_limit_for_provider(&llm_config.provider);
        let truncated = approx_tokens > context_limit;

        let prompt_to_send = if truncated {
            let max_chars = context_limit * 4;
            if augmented.len() > max_chars {
                let safe = augmented.floor_char_boundary(max_chars);
                augmented[..safe].to_string()
            } else {
                augmented.to_string()
            }
        } else {
            augmented.to_string()
        };

        (prompt_to_send, truncated)
    } else {
        // Standard path: read input file, strip frontmatter, load template or use inline prompt.
        // Read input content
        let raw_input = fs::read_to_string(input_path)
            .map_err(|e| format!("Failed to read input file: {}", e))?;

        // Strip frontmatter from input if present
        let input_content = super::strip_frontmatter(&raw_input);

        // Load prompt text from template or use inline prompt directly
        let prompt_text = if let Some(ref template_name) = llm_config.prompt_template {
            let template = get_prompt_template_internal(template_name)?;
            template.prompt.clone()
        } else if let Some(ref inline) = llm_config.prompt_inline {
            inline.clone()
        } else {
            return Err("LLM step missing prompt_template or prompt_inline".to_string());
        };

        // Substitute variables
        let full_prompt = substitute_variables(&prompt_text, input_content);

        // Estimate token count and check against provider limits
        let approx_tokens = estimate_tokens(&full_prompt);
        let context_limit = context_limit_for_provider(&llm_config.provider);
        let truncated = approx_tokens > context_limit;

        let prompt_to_send = if truncated {
            // Truncate to fit within context window (chars ~ tokens * 3.5)
            let max_chars = context_limit * 4;
            if full_prompt.len() > max_chars {
                let safe = full_prompt.floor_char_boundary(max_chars);
                full_prompt[..safe].to_string()
            } else {
                full_prompt.clone()
            }
        } else {
            full_prompt
        };

        (prompt_to_send, truncated)
    };

    // Get API key from settings
    let settings = load_settings();

    // Execute with appropriate provider, passing the user-selected model
    let result = match llm_config.provider.as_str() {
        "openai" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_gpt4o(&api_key, &prompt_to_send, "", &llm_config.model).await
        }
        "google" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "google")
                .ok_or("Google API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_gemini(&api_key, &prompt_to_send, "", &llm_config.model).await
        }
        "anthropic" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "anthropic")
                .ok_or("Anthropic API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_claude(&api_key, &prompt_to_send, "", &llm_config.model).await
        }
        "local" => {
            let model_id = llm_config.model.clone();
            if model_id.is_empty() {
                return Err(
                    "Local LLM step needs 'model' in pipeline config (e.g. \"qwen3-4b\")"
                        .to_string(),
                );
            }
            let prompt_clone = prompt_to_send.clone();
            tokio::task::spawn_blocking(move || {
                crate::local_llm::process_with_local(&model_id, &prompt_clone, "")
            })
            .await
            .map_err(|e| format!("Local LLM task failed: {}", e))?
        }
        "ollama" => {
            let model = llm_config.model.clone();
            cloud_ai::process_with_openai_compat(
                "http://localhost:11434",
                None,
                &model,
                &prompt_to_send,
                "",
            )
            .await
        }
        "cli_agent" => {
            let cli_config = settings.cli_agent.clone();
            let model_override = if llm_config.model.is_empty() || llm_config.model == "default" {
                cli_config.model.as_deref().map(|s| s.to_string())
            } else {
                Some(llm_config.model.clone())
            };
            super::cli_agent::process_with_cli(
                &cli_config.cli,
                &prompt_to_send,
                "",
                model_override.as_deref(),
                cli_config.timeout_secs,
            ).await
        }
        other => Err(format!("Unknown LLM provider: '{}'", other)),
    };

    // Ensure output directory exists
    fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create output dir: {}", e))?;

    let output_path = output_dir.join(format!("{}.md", step_name));

    match result {
        Ok(response) => {
            let completed_at =
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            let mut frontmatter = StepFrontmatter {
                name: step_name.to_string(),
                description: step_description.unwrap_or("").to_string(),
                connector: "llm".to_string(),
                input: step_input.to_string(),
                status: "done".to_string(),
                created_at,
                completed_at: Some(completed_at),
                error: None,
                provider: llm_config.provider,
                model: llm_config.model,
            };

            if truncated {
                frontmatter.description = format!(
                    "{}. WARNING: Input was truncated to fit model context window.",
                    frontmatter.description
                );
            }

            let file_content = format!("{}\n\n{}", frontmatter.to_yaml(), response);

            // Atomic write with unique temp suffix to avoid collision
            let temp_path = output_dir.join(format!(".{}.md.tmp", step_name));
            fs::write(&temp_path, &file_content)
                .map_err(|e| format!("Failed to write output: {}", e))?;
            fs::rename(&temp_path, &output_path)
                .map_err(|e| format!("Failed to finalize output: {}", e))?;

            Ok(output_path)
        }
        Err(error) => {
            let completed_at =
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            let frontmatter = StepFrontmatter {
                name: step_name.to_string(),
                description: step_description.unwrap_or("").to_string(),
                connector: "llm".to_string(),
                input: step_input.to_string(),
                status: "failed".to_string(),
                created_at,
                completed_at: Some(completed_at),
                error: Some(error.clone()),
                provider: llm_config.provider,
                model: llm_config.model,
            };

            let file_content = format!("{}\n\nStep failed: {}", frontmatter.to_yaml(), error);

            // Write error state (best-effort)
            let temp_path = output_dir.join(format!(".{}.md.tmp", step_name));
            if let Err(write_err) = fs::write(&temp_path, &file_content) {
                eprintln!(
                    "Warning: Failed to write LLM error state for step '{}': {}",
                    step_name, write_err
                );
            } else if let Err(rename_err) = fs::rename(&temp_path, &output_path) {
                eprintln!(
                    "Warning: Failed to finalize LLM error state for step '{}': {}",
                    step_name, rename_err
                );
            }

            Err(error)
        }
    }
}

/// Execute a corrective LLM retry call for a step that returned invalid JSON.
///
/// Builds a corrective prompt that includes the failed output and instructs the
/// LLM to return only a valid JSON array. Uses the same provider/model from
/// `config` as the original LLM call.
///
/// The output is written to a retry-specific .md file:
/// `{step_name}-retry.md` in `output_dir`. The pipeline engine reads this file
/// to feed into the Notion connector for a second attempt.
///
/// # Parameters
/// - `failed_output`: The raw LLM output that failed JSON parsing (first 2000 chars included in prompt)
/// - `original_prompt`: The augmented prompt from the original LLM call (last 500 chars used as context)
/// - `config`: The LLM step config (`provider`, `model`, etc.)
/// - `output_dir`: Directory to write the retry output .md file
/// - `step_name`: Name of the original LLM step (retry file uses `{step_name}-retry`)
/// - `step_input`: Input reference name (for frontmatter)
/// - `step_description`: Optional description
pub async fn execute_retry(
    failed_output: &str,
    original_prompt: &str,
    config: &serde_json::Value,
    output_dir: &Path,
    step_name: &str,
    step_input: &str,
    step_description: Option<&str>,
) -> Result<PathBuf, String> {
    let llm_config = LlmConfig::from_value(config)?;
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // Truncate failed output to first 2000 chars for the corrective prompt
    let failed_preview = if failed_output.len() > 2000 {
        let safe = failed_output.floor_char_boundary(2000);
        &failed_output[..safe]
    } else {
        failed_output
    };

    // Use last 500 chars of the original prompt as the format spec context
    let prompt_context = if original_prompt.len() > 500 {
        let start = original_prompt.ceil_char_boundary(original_prompt.len() - 500);
        &original_prompt[start..]
    } else {
        original_prompt
    };

    let corrective_prompt = format!(
        "The previous AI response was not valid JSON. Here is what was returned:\n\n\
         ---BEGIN FAILED OUTPUT---\n\
         {}\n\
         ---END FAILED OUTPUT---\n\n\
         Fix this output to be a valid JSON array. Rules:\n\
         1. Return ONLY a JSON array: [{{...}}, ...]\n\
         2. No markdown, no code fences, no prose before or after\n\
         3. No trailing commas\n\
         4. All strings must be properly quoted\n\
         5. Preserve the original data as closely as possible\n\n\
         Original instruction (for context):\n\
         {}",
        failed_preview,
        prompt_context,
    );

    // Get API key from settings
    let settings = load_settings();

    // Execute with the same provider/model as the original call
    let result = match llm_config.provider.as_str() {
        "openai" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "openai")
                .ok_or("OpenAI API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_gpt4o(&api_key, &corrective_prompt, "", &llm_config.model).await
        }
        "google" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "google")
                .ok_or("Google API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_gemini(&api_key, &corrective_prompt, "", &llm_config.model).await
        }
        "anthropic" => {
            let api_key = crate::config::get_api_key_for_provider(&settings, "anthropic")
                .ok_or("Anthropic API key not configured. Set it in Settings.")?;
            cloud_ai::process_with_claude(&api_key, &corrective_prompt, "", &llm_config.model).await
        }
        "local" => {
            let model_id = llm_config.model.clone();
            if model_id.is_empty() {
                return Err(
                    "Local LLM step needs 'model' in pipeline config (e.g. \"qwen3-4b\")"
                        .to_string(),
                );
            }
            let prompt_clone = corrective_prompt.clone();
            tokio::task::spawn_blocking(move || {
                crate::local_llm::process_with_local(&model_id, &prompt_clone, "")
            })
            .await
            .map_err(|e| format!("Local LLM task failed: {}", e))?
        }
        "ollama" => {
            let model = llm_config.model.clone();
            cloud_ai::process_with_openai_compat(
                "http://localhost:11434",
                None,
                &model,
                &corrective_prompt,
                "",
            )
            .await
        }
        "cli_agent" => {
            let cli_config = settings.cli_agent.clone();
            let model_override = if llm_config.model.is_empty() || llm_config.model == "default" {
                cli_config.model.as_deref().map(|s| s.to_string())
            } else {
                Some(llm_config.model.clone())
            };
            super::cli_agent::process_with_cli(
                &cli_config.cli,
                &corrective_prompt,
                "",
                model_override.as_deref(),
                cli_config.timeout_secs,
            ).await
        }
        other => Err(format!("Unknown LLM provider: '{}'", other)),
    };

    // Ensure output directory exists
    fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create output dir: {}", e))?;

    // Retry output uses a distinct file name to avoid overwriting the original
    let retry_step_name = format!("{}-retry", step_name);
    let output_path = output_dir.join(format!("{}.md", retry_step_name));

    match result {
        Ok(response) => {
            let completed_at =
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            let frontmatter = StepFrontmatter {
                name: retry_step_name.clone(),
                description: "Retry attempt for JSON correction".to_string(),
                connector: "llm".to_string(),
                input: step_input.to_string(),
                status: "done".to_string(),
                created_at,
                completed_at: Some(completed_at),
                error: None,
                provider: llm_config.provider,
                model: llm_config.model,
            };

            let file_content = format!("{}\n\n{}", frontmatter.to_yaml(), response);

            // Atomic write
            let temp_path = output_dir.join(format!(".{}.md.tmp", retry_step_name));
            fs::write(&temp_path, &file_content)
                .map_err(|e| format!("Failed to write retry output: {}", e))?;
            fs::rename(&temp_path, &output_path)
                .map_err(|e| format!("Failed to finalize retry output: {}", e))?;

            Ok(output_path)
        }
        Err(error) => {
            let completed_at =
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            let frontmatter = StepFrontmatter {
                name: retry_step_name.clone(),
                description: step_description.unwrap_or("").to_string(),
                connector: "llm".to_string(),
                input: step_input.to_string(),
                status: "failed".to_string(),
                created_at,
                completed_at: Some(completed_at),
                error: Some(error.clone()),
                provider: llm_config.provider,
                model: llm_config.model,
            };

            let file_content = format!("{}\n\nRetry failed: {}", frontmatter.to_yaml(), error);

            let temp_path = output_dir.join(format!(".{}.md.tmp", retry_step_name));
            if let Err(write_err) = fs::write(&temp_path, &file_content) {
                eprintln!(
                    "Warning: Failed to write LLM retry error state for step '{}': {}",
                    retry_step_name, write_err
                );
            } else if let Err(rename_err) = fs::rename(&temp_path, &output_path) {
                eprintln!(
                    "Warning: Failed to finalize LLM retry error state for step '{}': {}",
                    retry_step_name, rename_err
                );
            }

            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_config_from_value() {
        let config = serde_json::json!({
            "prompt_template": "meeting-notes",
            "provider": "openai",
            "model": "gpt-4o"
        });
        let llm_config = LlmConfig::from_value(&config).unwrap();
        assert_eq!(llm_config.prompt_template, Some("meeting-notes".to_string()));
        assert_eq!(llm_config.provider, "openai");
        assert_eq!(llm_config.model, "gpt-4o");
    }

    #[test]
    fn test_llm_config_defaults() {
        let config = serde_json::json!({
            "prompt_template": "brainstorm"
        });
        let llm_config = LlmConfig::from_value(&config).unwrap();
        assert_eq!(llm_config.provider, "openai");
        assert_eq!(llm_config.model, "gpt-4o");
    }

    #[test]
    fn test_llm_config_missing_template() {
        let config = serde_json::json!({
            "provider": "openai"
        });
        let result = LlmConfig::from_value(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("prompt_template"));
    }

    #[test]
    fn test_frontmatter_generation() {
        let fm = StepFrontmatter {
            name: "summarize".to_string(),
            description: "Extract meeting notes".to_string(),
            connector: "llm".to_string(),
            input: "transcript".to_string(),
            status: "done".to_string(),
            created_at: "2026-02-03T12:00:00Z".to_string(),
            completed_at: Some("2026-02-03T12:00:05Z".to_string()),
            error: None,
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
        };
        let yaml = fm.to_yaml();
        assert!(yaml.starts_with("---"));
        assert!(yaml.ends_with("---"));
        assert!(yaml.contains("status: done"));
        assert!(yaml.contains("error: null"));
    }

    #[test]
    fn test_frontmatter_with_error() {
        let fm = StepFrontmatter {
            name: "summarize".to_string(),
            description: "".to_string(),
            connector: "llm".to_string(),
            input: "transcript".to_string(),
            status: "failed".to_string(),
            created_at: "2026-02-03T12:00:00Z".to_string(),
            completed_at: Some("2026-02-03T12:00:05Z".to_string()),
            error: Some("API error: 401 Unauthorized".to_string()),
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
        };
        let yaml = fm.to_yaml();
        assert!(yaml.contains("status: failed"));
        assert!(yaml.contains("API error: 401 Unauthorized"));
    }

    #[test]
    fn test_strip_frontmatter() {
        let content = "---\nname: test\nstatus: done\n---\n\nActual content here";
        let stripped = super::super::strip_frontmatter(content);
        assert_eq!(stripped, "Actual content here");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let content = "Just plain content";
        let stripped = super::super::strip_frontmatter(content);
        assert_eq!(stripped, "Just plain content");
    }

    #[test]
    fn test_default_model_for_provider() {
        assert_eq!(default_model_for_provider("openai"), "gpt-4o");
        assert_eq!(default_model_for_provider("google"), "gemini-1.5-flash");
        assert_eq!(
            default_model_for_provider("anthropic"),
            "claude-sonnet-4-20250514"
        );
        assert_eq!(default_model_for_provider("unknown"), "gpt-4o");
    }

    #[test]
    fn test_estimate_tokens() {
        // Short text
        let tokens = estimate_tokens("Hello world");
        assert!(tokens > 0);
        assert!(tokens < 10);

        // Empty text
        assert_eq!(estimate_tokens(""), 0);

        // Longer text uses word-based estimate
        let text = "The quick brown fox jumps over the lazy dog repeatedly in a sentence";
        let tokens = estimate_tokens(text);
        assert!(tokens > 10);
        assert!(tokens < 30);
    }

    #[test]
    fn test_context_limit_for_provider() {
        assert_eq!(context_limit_for_provider("openai"), 120_000);
        assert_eq!(context_limit_for_provider("google"), 900_000);
        assert_eq!(context_limit_for_provider("anthropic"), 190_000);
        assert_eq!(context_limit_for_provider("unknown"), 120_000);
    }
}
