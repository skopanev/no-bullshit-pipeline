use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::PathBuf;
use chrono::Utc;
use crate::storage::get_data_dir;
use crate::config::{get_config_dir, get_templates_dir};

/// A reusable prompt template
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PromptTemplate {
    pub name: String,
    pub prompt: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Get the path to prompt-templates.json
fn get_prompt_templates_path() -> PathBuf {
    get_config_dir().join("prompt-templates.json")
}

/// Migrate prompt-templates.json from old data dir to config dir (one-time)
fn migrate_prompt_templates_if_needed() {
    let old_path = get_data_dir().join("prompt-templates.json");
    let new_path = get_config_dir().join("prompt-templates.json");
    if old_path.exists() && !new_path.exists() {
        let _ = fs::rename(&old_path, &new_path);
    }
}

/// Get built-in default templates
fn get_builtin_templates() -> HashMap<String, PromptTemplate> {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut templates = HashMap::new();

    templates.insert(
        "summary".to_string(),
        PromptTemplate {
            name: "summary".to_string(),
            prompt: r#"Summarize this transcript. Respond in the same language as the transcript.

Use EXACTLY this format (no top-level heading, start directly with ## Topic):

## Topic
A short title — 3 to 5 words — capturing what the conversation is about. No trailing period.

## Key Points
- 3-7 bullet points, most important information discussed

## Decisions
- Conclusions or agreements reached (skip section if none)

## Action Items
- Who does what, with deadlines if mentioned (skip section if none)

## Open Questions
- Unresolved items needing follow-up (skip section if none)

Rules:
- Be concise and actionable
- No disclaimers about transcript quality
- No preamble or commentary — output only the summary

Transcript:
{transcript}"#.to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
    );

    templates.insert(
        "action-plan".to_string(),
        PromptTemplate {
            name: "action-plan".to_string(),
            prompt: r#"Extract a detailed action plan from this transcript. Respond in the same language as the transcript.

For each task or discussion topic, capture the full context — not just the headline. Include technical details, constraints, dependencies, and reasoning that was discussed.

Use EXACTLY this format:

## Topic
A short title — 3 to 5 words — capturing what the conversation is about. No trailing period.

## Tasks

For each task use this format:

### <task title>
- **Owner**: who is responsible (name or role, if mentioned)
- **Priority**: high / medium / low (infer from conversation urgency)
- **Deadline**: if mentioned
- **Context**: why this task exists, what problem it solves, relevant background discussed
- **Details**: specific technical details, approaches discussed, constraints, edge cases mentioned
- **Acceptance criteria**: what "done" looks like (infer from discussion if not explicit)
- **Dependencies**: what needs to happen first or what this blocks
- **Open questions**: unresolved aspects of this specific task

Skip any field that has no information. Include ALL tasks — even small ones.

## Parking Lot
- Ideas, tangents, or future considerations that were mentioned but not assigned

Rules:
- Preserve technical details and specific numbers/names from the conversation
- If people discussed HOW to do something, capture the approach
- No disclaimers about transcript quality
- No preamble — output only the plan

Transcript:
{transcript}"#.to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
    );

    templates
}

/// Migrate existing templates from ~/.nbp/templates/ to new format
fn migrate_existing_templates(templates: &mut HashMap<String, PromptTemplate>) {
    let old_templates_dir = get_templates_dir();
    if !old_templates_dir.exists() {
        return;
    }

    if let Ok(entries) = fs::read_dir(&old_templates_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && let Ok(content) = fs::read_to_string(&path)
                && let Ok(old_template) = serde_json::from_str::<serde_json::Value>(&content)
            {
                let name = old_template
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if !name.is_empty() && !templates.contains_key(&name) {
                    let now =
                        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    let prompt_template = PromptTemplate {
                        name: name.clone(),
                        prompt: old_template
                            .get("prompt")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        created_at: now.clone(),
                        updated_at: now,
                    };
                    templates.insert(name, prompt_template);
                }
            }
        }
    }
}

/// Load all prompt templates from disk
pub fn load_prompt_templates() -> Result<HashMap<String, PromptTemplate>, String> {
    migrate_prompt_templates_if_needed();
    let path = get_prompt_templates_path();

    if !path.exists() {
        // First load: create with built-ins + migrate old templates
        let mut templates = get_builtin_templates();
        migrate_existing_templates(&mut templates);
        save_prompt_templates_to_disk(&templates)?;
        return Ok(templates);
    }

    let file =
        File::open(&path).map_err(|e| format!("Failed to open prompt-templates.json: {}", e))?;
    let templates: HashMap<String, PromptTemplate> = serde_json::from_reader(file)
        .map_err(|e| format!("Failed to parse prompt-templates.json: {}", e))?;

    Ok(templates)
}

/// Save all prompt templates to disk
fn save_prompt_templates_to_disk(
    templates: &HashMap<String, PromptTemplate>,
) -> Result<(), String> {
    let config_dir = get_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
    }

    let path = get_prompt_templates_path();
    let content = serde_json::to_string_pretty(templates)
        .map_err(|e| format!("Failed to serialize templates: {}", e))?;
    fs::write(&path, content)
        .map_err(|e| format!("Failed to write prompt-templates.json: {}", e))?;

    Ok(())
}

/// Validate a prompt template
fn validate_prompt_template(template: &PromptTemplate) -> Result<(), String> {
    if template.name.trim().is_empty() {
        return Err("Template name cannot be empty".to_string());
    }
    if template.prompt.trim().is_empty() {
        return Err("Template prompt cannot be empty".to_string());
    }
    Ok(())
}

/// Substitute variables in a prompt template
pub fn substitute_variables(prompt: &str, transcript: &str) -> String {
    prompt.replace("{transcript}", transcript)
}

/// Get a single prompt template by name
pub fn get_prompt_template_internal(name: &str) -> Result<PromptTemplate, String> {
    let templates = load_prompt_templates()?;
    templates
        .get(name)
        .cloned()
        .ok_or_else(|| format!("Prompt template '{}' not found", name))
}

/// List all prompt templates
#[tauri::command]
pub fn list_prompt_templates() -> Result<Vec<PromptTemplate>, String> {
    let templates = load_prompt_templates()?;
    Ok(templates.into_values().collect())
}

/// Get a specific prompt template by name
#[tauri::command]
pub fn get_prompt_template(name: String) -> Result<PromptTemplate, String> {
    get_prompt_template_internal(&name)
}

/// Save (create or update) a prompt template
#[tauri::command]
pub fn save_prompt_template(mut template: PromptTemplate) -> Result<(), String> {
    validate_prompt_template(&template)?;

    let mut templates = load_prompt_templates()?;
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    if let Some(existing) = templates.get(&template.name) {
        // Update: preserve created_at, update updated_at
        template.created_at = existing.created_at.clone();
        template.updated_at = now;
    } else {
        // Create: set both timestamps
        template.created_at = now.clone();
        template.updated_at = now;
    }

    templates.insert(template.name.clone(), template);
    save_prompt_templates_to_disk(&templates)?;

    Ok(())
}

/// Delete a prompt template
#[tauri::command]
pub fn delete_prompt_template(name: String, force: Option<bool>) -> Result<(), String> {
    let mut templates = load_prompt_templates()?;

    if !templates.contains_key(&name) {
        return Err(format!("Template '{}' not found", name));
    }

    // Check if any pipelines reference this template
    if !force.unwrap_or(false) {
        let pipelines = crate::pipelines::load_pipelines().unwrap_or_default();
        let mut referencing: Vec<String> = Vec::new();

        for (pipeline_name, pipeline) in &pipelines {
            for step in &pipeline.steps {
                if let Some(template_name) = step.config.get("prompt_template").and_then(|v| v.as_str())
                    && template_name == name
                {
                    referencing.push(pipeline_name.clone());
                    break;
                }
            }
        }

        if !referencing.is_empty() {
            return Err(format!(
                "Template '{}' is used by pipelines: {}. Use force=true to delete anyway.",
                name,
                referencing.join(", ")
            ));
        }
    }

    templates.remove(&name);
    save_prompt_templates_to_disk(&templates)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_templates_exist() {
        let templates = get_builtin_templates();
        assert_eq!(templates.len(), 1);
        assert!(templates.contains_key("summary"));
    }

    #[test]
    fn test_builtin_templates_have_transcript_variable() {
        let templates = get_builtin_templates();
        for t in templates.values() {
            assert!(
                t.prompt.contains("{transcript}"),
                "Template '{}' missing {{transcript}} variable",
                t.name
            );
        }
    }

    #[test]
    fn test_validate_empty_name_fails() {
        let template = PromptTemplate {
            name: "".to_string(),
            prompt: "test prompt".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert!(validate_prompt_template(&template).is_err());
    }

    #[test]
    fn test_validate_empty_prompt_fails() {
        let template = PromptTemplate {
            name: "test".to_string(),
            prompt: "  ".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert!(validate_prompt_template(&template).is_err());
    }

    #[test]
    fn test_substitute_variables() {
        let prompt = "Process this: {transcript}\nEnd.";
        let result = substitute_variables(prompt, "Hello world");
        assert_eq!(result, "Process this: Hello world\nEnd.");
    }

    #[test]
    fn test_substitute_no_variable() {
        let prompt = "No variable here";
        let result = substitute_variables(prompt, "Hello world");
        assert_eq!(result, "No variable here");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let template = PromptTemplate {
            name: "test".to_string(),
            prompt: "Analyze: {transcript}".to_string(),
            created_at: "2026-02-03T12:00:00Z".to_string(),
            updated_at: "2026-02-03T12:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&template).unwrap();
        let deserialized: PromptTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, template.name);
        assert_eq!(deserialized.prompt, template.prompt);
    }
}
