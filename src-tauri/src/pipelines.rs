use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::PathBuf;
use crate::config::get_config_dir;
use crate::storage::get_data_dir;

/// Connector types available for pipeline steps
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorType {
    Llm,
    Save,
    Webhook,
    Slack,
    Mcp,
    Notion,
    Linear,
    #[serde(rename = "cli_agent")]
    CliAgent,
}

impl ConnectorType {
    /// Delivery connectors send output to external services.
    /// Their failure does not block other independent steps — the pipeline continues
    /// executing subsequent steps whose inputs are not derived from this step.
    ///
    /// Processing connectors (Llm, Mcp) produce output consumed by downstream steps.
    /// Their failure halts all downstream steps because those steps depend on the output.
    pub fn is_delivery(&self) -> bool {
        matches!(self, ConnectorType::Notion | ConnectorType::Linear | ConnectorType::Slack | ConnectorType::Webhook | ConnectorType::Save)
    }
}

/// A single step in a pipeline
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PipelineStep {
    pub name: String,
    pub connector: ConnectorType,
    pub input: String, // "transcript" or previous step name
    pub config: serde_json::Value, // Connector-specific config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Pipeline definition (stored in pipelines.json)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Pipeline {
    pub name: String,
    pub description: String,
    pub steps: Vec<PipelineStep>,
    #[serde(default = "default_now")]
    pub created_at: String,
    #[serde(default = "default_now")]
    pub updated_at: String,
}

fn default_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Pipeline execution status
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PipelineStatus {
    Waiting,  // Assigned but transcript not ready
    Running,  // Currently executing
    Done,     // All steps completed successfully
    Partial,  // Stopped due to step failure
}

/// Pipeline execution state stored in recording metadata
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PipelineState {
    /// Unique run ID (UUID)
    #[serde(default = "generate_run_id")]
    pub id: String,
    pub name: String,
    pub status: PipelineStatus,
    #[serde(default)]
    pub run_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn generate_run_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Step execution status for UI updates
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StepStatus {
    pub name: String,
    pub status: String, // "pending", "running", "done", "failed", "skipped"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub augmented_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Pipeline progress event payload
#[derive(Serialize, Clone, Debug)]
pub struct PipelineProgressPayload {
    pub recording_id: String,
    pub pipeline_name: String,
    pub step_name: String,
    pub step_index: usize,
    pub total_steps: usize,
    pub status: String,
}

/// Get the path to pipelines.json
fn get_pipelines_path() -> PathBuf {
    get_config_dir().join("pipelines.json")
}

/// Migrate pipelines.json from old data dir to config dir (one-time)
fn migrate_pipelines_if_needed() {
    let old_path = get_data_dir().join("pipelines.json");
    let new_path = get_config_dir().join("pipelines.json");
    if old_path.exists() && !new_path.exists() {
        let _ = fs::rename(&old_path, &new_path);
    }
}

/// Validate a pipeline definition
pub fn validate_pipeline(pipeline: &Pipeline) -> Result<(), String> {
    // Pipeline must have non-empty name
    if pipeline.name.trim().is_empty() {
        return Err("Pipeline name cannot be empty".to_string());
    }

    // Pipeline name must be filesystem-safe (no slashes, colons, null bytes)
    if pipeline.name.contains('/') || pipeline.name.contains('\\')
        || pipeline.name.contains('\0') || pipeline.name.contains(':')
    {
        return Err("Pipeline name contains invalid characters (/, \\, :, or null)".to_string());
    }

    let mut defined_steps: Vec<String> = Vec::new();

    for (i, step) in pipeline.steps.iter().enumerate() {
        // Step must have non-empty name
        if step.name.trim().is_empty() {
            return Err(format!("Step {} has empty name", i + 1));
        }

        // Step name must be filesystem-safe
        if step.name.contains('/') || step.name.contains('\\')
            || step.name.contains('\0') || step.name.contains(':')
        {
            return Err(format!(
                "Step '{}' name contains invalid characters (/, \\, :, or null)",
                step.name
            ));
        }

        // Validate input references
        if i == 0 {
            // First step input must be "transcript"
            if step.input != "transcript" {
                return Err(format!(
                    "First step '{}' input must be 'transcript', got '{}'",
                    step.name, step.input
                ));
            }
        } else {
            // Subsequent steps can reference "transcript" or a previous step name
            if step.input != "transcript" && !defined_steps.contains(&step.input) {
                return Err(format!(
                    "Step '{}' references unknown input '{}'. Must be 'transcript' or a previous step name",
                    step.name, step.input
                ));
            }
        }

        // Validate connector-specific config
        validate_step_config(step)?;

        // Check for duplicate step names
        if defined_steps.contains(&step.name) {
            return Err(format!("Duplicate step name '{}'", step.name));
        }

        defined_steps.push(step.name.clone());
    }

    Ok(())
}

/// Validate connector-specific config for a pipeline step
fn validate_step_config(step: &PipelineStep) -> Result<(), String> {
    match step.connector {
        ConnectorType::Llm => {
            let has_template = step.config.get("prompt_template")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .is_some();
            let has_inline = step.config.get("prompt_inline")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .is_some();
            if !has_template && !has_inline {
                return Err(format!(
                    "Step '{}': LLM connector requires 'prompt_template' or 'prompt_inline' in config",
                    step.name
                ));
            }
            // Validate provider if specified
            if let Some(provider) = step.config.get("provider").and_then(|v| v.as_str())
                && !["openai", "google", "anthropic", "local", "ollama", "cli_agent"].contains(&provider)
            {
                return Err(format!(
                    "Step '{}': Unknown LLM provider '{}'. Must be openai, google, anthropic, local, ollama, or cli_agent",
                    step.name, provider
                ));
            }
        }
        ConnectorType::Save => {
            if step.config.get("path").and_then(|v| v.as_str()).is_none() {
                return Err(format!(
                    "Step '{}': Save connector requires 'path' in config",
                    step.name
                ));
            }
        }
        ConnectorType::Webhook => {
            let url = step.config.get("url").and_then(|v| v.as_str());
            if url.is_none() {
                return Err(format!(
                    "Step '{}': Webhook connector requires 'url' in config",
                    step.name
                ));
            }
            // Validate URL starts with http(s)
            if let Some(u) = url
                && !u.starts_with("http://") && !u.starts_with("https://")
            {
                return Err(format!(
                    "Step '{}': Webhook URL must start with http:// or https://",
                    step.name
                ));
            }
            // Validate HTTP method if specified
            if let Some(method) = step.config.get("method").and_then(|v| v.as_str()) {
                let m = method.to_uppercase();
                if !["POST", "PUT", "PATCH"].contains(&m.as_str()) {
                    return Err(format!(
                        "Step '{}': Unsupported HTTP method '{}'. Must be POST, PUT, or PATCH",
                        step.name, method
                    ));
                }
            }
        }
        ConnectorType::Slack => {
            if step.config.get("integration_id").and_then(|v| v.as_str()).is_none() {
                return Err(format!(
                    "Step '{}': Slack connector requires 'integration_id' in config",
                    step.name
                ));
            }
            if step.config.get("target").and_then(|v| v.as_str()).is_none() {
                return Err(format!(
                    "Step '{}': Slack connector requires 'target' in config",
                    step.name
                ));
            }
        }
        ConnectorType::Mcp => {
            let url = step.config.get("url").and_then(|v| v.as_str());
            if url.is_none() {
                return Err(format!(
                    "Step '{}': MCP connector requires 'url' in config",
                    step.name
                ));
            }
            if let Some(u) = url
                && !u.starts_with("http://") && !u.starts_with("https://")
            {
                return Err(format!(
                    "Step '{}': MCP URL must start with http:// or https://",
                    step.name
                ));
            }
            match step.config.get("tool").and_then(|v| v.as_str()) {
                None | Some("") => {
                    return Err(format!(
                        "Step '{}': MCP connector requires 'tool' in config",
                        step.name
                    ));
                }
                _ => {}
            }
        }
        ConnectorType::Notion => {
            if step.config.get("integration_id").and_then(|v| v.as_str()).is_none() {
                return Err(format!(
                    "Step '{}': Notion connector requires 'integration_id' in config",
                    step.name
                ));
            }
        }
        ConnectorType::Linear => {
            if step.config.get("integration_id").and_then(|v| v.as_str()).is_none() {
                return Err(format!(
                    "Step '{}': Linear connector requires 'integration_id' in config",
                    step.name
                ));
            }
        }
        ConnectorType::CliAgent => {
            let cli = step.config.get("cli").and_then(|v| v.as_str());
            match cli {
                None | Some("") => {
                    return Err(format!(
                        "Step '{}': CLI agent connector requires 'cli' in config ('claude' or 'codex')",
                        step.name
                    ));
                }
                Some(c) if c != "claude" && c != "codex" => {
                    return Err(format!(
                        "Step '{}': CLI agent 'cli' must be 'claude' or 'codex', got '{}'",
                        step.name, c
                    ));
                }
                _ => {}
            }
            let has_prompt = step.config.get("prompt").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
            let has_prompt_template = step.config.get("prompt_template").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
            if !has_prompt && !has_prompt_template {
                return Err(format!(
                    "Step '{}': CLI agent connector requires 'prompt' or 'prompt_template' in config",
                    step.name
                ));
            }
            let model_mode = step.config.get("model_mode").and_then(|v| v.as_str()).unwrap_or("default");
            if model_mode != "default" && model_mode != "advanced" {
                return Err(format!(
                    "Step '{}': CLI agent 'model_mode' must be 'default' or 'advanced', got '{}'",
                    step.name, model_mode
                ));
            }
            let model_args = step.config.get("model_args").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
            let model = step.config.get("model").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
            let provider = step.config.get("provider").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
            if model_mode == "default" && model_args.is_some() {
                return Err(format!(
                    "Step '{}': CLI agent in default mode cannot have 'model_args' - use dropdown selection or switch to advanced mode",
                    step.name
                ));
            }
            if model_mode == "advanced" && (model.is_some() || provider.is_some()) {
                return Err(format!(
                    "Step '{}': CLI agent in advanced mode cannot have 'model' or 'provider' - use 'model_args' for custom CLI arguments",
                    step.name
                ));
            }
        }
    }
    Ok(())
}

/// Load all pipelines from disk
pub fn load_pipelines() -> Result<HashMap<String, Pipeline>, String> {
    migrate_pipelines_if_needed();
    let path = get_pipelines_path();

    if !path.exists() {
        return Ok(HashMap::new());
    }

    let file = File::open(&path).map_err(|e| format!("Failed to open pipelines.json: {}", e))?;
    let pipelines: HashMap<String, Pipeline> =
        serde_json::from_reader(file).map_err(|e| format!("Failed to parse pipelines.json: {}", e))?;

    Ok(pipelines)
}

/// Save all pipelines to disk
pub fn save_pipelines_to_disk(pipelines: &HashMap<String, Pipeline>) -> Result<(), String> {
    let config_dir = get_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| format!("Failed to create config dir: {}", e))?;
    }

    let path = get_pipelines_path();
    let content =
        serde_json::to_string_pretty(pipelines).map_err(|e| format!("Failed to serialize pipelines: {}", e))?;
    fs::write(&path, content).map_err(|e| format!("Failed to write pipelines.json: {}", e))?;

    Ok(())
}

/// List all pipeline definitions
#[tauri::command]
pub fn list_pipelines() -> Result<Vec<Pipeline>, String> {
    let pipelines = load_pipelines()?;
    Ok(pipelines.into_values().collect())
}

/// Get a specific pipeline by name
#[tauri::command]
pub fn get_pipeline(name: String) -> Result<Pipeline, String> {
    let pipelines = load_pipelines()?;
    pipelines
        .get(&name)
        .cloned()
        .ok_or_else(|| format!("Pipeline '{}' not found", name))
}

/// Save (create or update) a pipeline definition
#[tauri::command]
pub fn save_pipeline(mut pipeline: Pipeline) -> Result<(), String> {
    validate_pipeline(&pipeline)?;

    let mut pipelines = load_pipelines()?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // Preserve created_at if updating existing pipeline
    if let Some(existing) = pipelines.get(&pipeline.name) {
        pipeline.created_at = existing.created_at.clone();
    } else {
        pipeline.created_at = now.clone();
    }
    pipeline.updated_at = now;
    pipelines.insert(pipeline.name.clone(), pipeline);
    save_pipelines_to_disk(&pipelines)?;

    Ok(())
}

/// Delete a pipeline definition
#[tauri::command]
pub fn delete_pipeline(name: String) -> Result<(), String> {
    let mut pipelines = load_pipelines()?;
    if pipelines.remove(&name).is_none() {
        return Err(format!("Pipeline '{}' not found", name));
    }
    save_pipelines_to_disk(&pipelines)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_valid_pipeline() -> Pipeline {
        Pipeline {
            name: "test-pipeline".to_string(),
            description: "A test pipeline".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                PipelineStep {
                    name: "summarize".to_string(),
                    connector: ConnectorType::Llm,
                    input: "transcript".to_string(),
                    config: serde_json::json!({
                        "prompt_template": "meeting-notes",
                        "provider": "openai",
                        "model": "gpt-4o"
                    }),
                    description: Some("Summarize the transcript".to_string()),
                },
                PipelineStep {
                    name: "save-to-obsidian".to_string(),
                    connector: ConnectorType::Save,
                    input: "summarize".to_string(),
                    config: serde_json::json!({
                        "path": "~/Documents/{date}-{pipeline-name}.md"
                    }),
                    description: None,
                },
            ],
        }
    }

    #[test]
    fn test_valid_pipeline_passes_validation() {
        let pipeline = make_valid_pipeline();
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_empty_name_fails() {
        let mut pipeline = make_valid_pipeline();
        pipeline.name = "".to_string();
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("name cannot be empty"));
    }

    #[test]
    fn test_empty_steps_passes() {
        let mut pipeline = make_valid_pipeline();
        pipeline.steps = vec![];
        let result = validate_pipeline(&pipeline);
        assert!(result.is_ok(), "Zero-step pipelines should be valid (labels)");
    }

    #[test]
    fn test_first_step_must_use_transcript_input() {
        let mut pipeline = make_valid_pipeline();
        pipeline.steps[0].input = "something-else".to_string();
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be 'transcript'"));
    }

    #[test]
    fn test_step_referencing_future_step_fails() {
        let pipeline = Pipeline {
            name: "bad-pipeline".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                PipelineStep {
                    name: "step-a".to_string(),
                    connector: ConnectorType::Llm,
                    input: "transcript".to_string(),
                    config: serde_json::json!({"prompt_template": "meeting-notes"}),
                    description: None,
                },
                PipelineStep {
                    name: "step-b".to_string(),
                    connector: ConnectorType::Save,
                    input: "step-c".to_string(), // references future step
                    config: serde_json::json!({"path": "~/out.md"}),
                    description: None,
                },
                PipelineStep {
                    name: "step-c".to_string(),
                    connector: ConnectorType::Webhook,
                    input: "step-a".to_string(),
                    config: serde_json::json!({"url": "https://example.com/hook"}),
                    description: None,
                },
            ],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown input 'step-c'"));
    }

    #[test]
    fn test_duplicate_step_names_fails() {
        let pipeline = Pipeline {
            name: "dup-pipeline".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                PipelineStep {
                    name: "step-a".to_string(),
                    connector: ConnectorType::Llm,
                    input: "transcript".to_string(),
                    config: serde_json::json!({"prompt_template": "meeting-notes"}),
                    description: None,
                },
                PipelineStep {
                    name: "step-a".to_string(),
                    connector: ConnectorType::Save,
                    input: "step-a".to_string(),
                    config: serde_json::json!({"path": "~/out.md"}),
                    description: None,
                },
            ],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Duplicate step name"));
    }

    #[test]
    fn test_step_with_empty_name_fails() {
        let mut pipeline = make_valid_pipeline();
        pipeline.steps[0].name = "".to_string();
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty name"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let pipeline = make_valid_pipeline();
        let json = serde_json::to_string_pretty(&pipeline).unwrap();
        let deserialized: Pipeline = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, pipeline.name);
        assert_eq!(deserialized.steps.len(), pipeline.steps.len());
        assert_eq!(deserialized.steps[0].connector, ConnectorType::Llm);
        assert_eq!(deserialized.steps[1].connector, ConnectorType::Save);
    }

    #[test]
    fn test_connector_type_serialization() {
        assert_eq!(
            serde_json::to_string(&ConnectorType::Llm).unwrap(),
            "\"llm\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorType::Save).unwrap(),
            "\"save\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorType::Webhook).unwrap(),
            "\"webhook\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorType::Mcp).unwrap(),
            "\"mcp\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorType::Slack).unwrap(),
            "\"slack\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorType::Notion).unwrap(),
            "\"notion\""
        );
    }

    #[test]
    fn test_notion_connector_type_serialization() {
        assert_eq!(
            serde_json::to_string(&ConnectorType::Notion).unwrap(),
            "\"notion\""
        );
    }

    #[test]
    fn test_notion_connector_type_deserialization() {
        let ct: ConnectorType = serde_json::from_str("\"notion\"").unwrap();
        assert_eq!(ct, ConnectorType::Notion);
    }

    #[test]
    fn test_notion_step_missing_integration_id_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "post-to-notion".to_string(),
                connector: ConnectorType::Notion,
                input: "transcript".to_string(),
                config: serde_json::json!({}),
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("integration_id"));
    }

    #[test]
    fn test_notion_step_valid_config_passes() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "post-to-notion".to_string(),
                connector: ConnectorType::Notion,
                input: "transcript".to_string(),
                config: serde_json::json!({"integration_id": "abc-123"}),
                description: None,
            }],
        };
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_llm_step_with_prompt_inline_passes() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "custom".to_string(),
                connector: ConnectorType::Llm,
                input: "transcript".to_string(),
                config: serde_json::json!({ "prompt_inline": "Summarize this: {transcript}" }),
                description: None,
            }],
        };
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_llm_step_missing_prompt_template_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "summarize".to_string(),
                connector: ConnectorType::Llm,
                input: "transcript".to_string(),
                config: serde_json::json!({"provider": "openai"}), // missing prompt_template
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("prompt_template"));
    }

    #[test]
    fn test_llm_step_invalid_provider_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "summarize".to_string(),
                connector: ConnectorType::Llm,
                input: "transcript".to_string(),
                config: serde_json::json!({
                    "prompt_template": "meeting-notes",
                    "provider": "invalid-provider"
                }),
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown LLM provider"));
    }

    #[test]
    fn test_save_step_missing_path_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "save-it".to_string(),
                connector: ConnectorType::Save,
                input: "transcript".to_string(),
                config: serde_json::json!({}), // missing path
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("path"));
    }

    #[test]
    fn test_webhook_step_missing_url_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "notify".to_string(),
                connector: ConnectorType::Webhook,
                input: "transcript".to_string(),
                config: serde_json::json!({"method": "POST"}), // missing url
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("url"));
    }

    #[test]
    fn test_webhook_step_invalid_url_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "notify".to_string(),
                connector: ConnectorType::Webhook,
                input: "transcript".to_string(),
                config: serde_json::json!({"url": "ftp://not-http.com"}),
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("http://"));
    }

    #[test]
    fn test_webhook_step_invalid_method_fails() {
        let pipeline = Pipeline {
            name: "test".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![PipelineStep {
                name: "notify".to_string(),
                connector: ConnectorType::Webhook,
                input: "transcript".to_string(),
                config: serde_json::json!({"url": "https://example.com", "method": "DELETE"}),
                description: None,
            }],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported HTTP method"));
    }

    #[test]
    fn test_pipeline_name_with_slash_fails() {
        let mut pipeline = make_valid_pipeline();
        pipeline.name = "bad/name".to_string();
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid characters"));
    }

    #[test]
    fn test_subsequent_step_can_reference_transcript() {
        let pipeline = Pipeline {
            name: "multi-input".to_string(),
            description: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                PipelineStep {
                    name: "step-a".to_string(),
                    connector: ConnectorType::Llm,
                    input: "transcript".to_string(),
                    config: serde_json::json!({"prompt_template": "meeting-notes"}),
                    description: None,
                },
                PipelineStep {
                    name: "step-b".to_string(),
                    connector: ConnectorType::Llm,
                    input: "transcript".to_string(), // also reads from transcript
                    config: serde_json::json!({"prompt_template": "brainstorm"}),
                    description: None,
                },
            ],
        };
        assert!(validate_pipeline(&pipeline).is_ok());
    }
}
