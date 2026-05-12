use std::path::{Path, PathBuf};
use std::fmt;
use std::fs;
use std::collections::BTreeMap;
use chrono::{NaiveDate, Utc};
use notion_client::endpoints::Client;
use notion_client::endpoints::pages::create::request::CreateAPageRequest;
use notion_client::objects::page::{PageProperty, SelectPropertyValue, DatePropertyValue};
use notion_client::objects::property::DateOrDateTime;
use notion_client::objects::parent::Parent;
use notion_client::objects::rich_text::{RichText, Text};
use notion_client::objects::user::User;
use crate::integrations::notion::{load_notion_profile, get_notion_token, NotionIntegrationProfile};

// ──────────────────────────────────────────────────────────────────────────────
// Error types
// ──────────────────────────────────────────────────────────────────────────────

/// Categorizes errors from the Notion connector.
///
/// `JsonParse` carries the full raw LLM output so the pipeline engine can
/// retry with a corrective prompt. `Other` covers API errors, config errors,
/// and all other failure modes where retry would not help.
#[derive(Debug)]
pub enum NotionErrorKind {
    /// JSON extraction or structural validation failed.
    /// `raw_output` contains the complete LLM output (not truncated).
    JsonParse { message: String, raw_output: String },
    /// API errors, config errors, authentication failures, etc.
    Other(String),
}

/// Structured error type for the Notion connector.
///
/// Implements `Display` and `From<NotionError> for String` so existing
/// `Result<PathBuf, String>` callers continue to work without changes.
#[derive(Debug)]
pub struct NotionError {
    pub kind: NotionErrorKind,
}

impl fmt::Display for NotionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            NotionErrorKind::JsonParse { message, .. } => write!(f, "{}", message),
            NotionErrorKind::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl From<NotionError> for String {
    fn from(e: NotionError) -> String {
        e.to_string()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Config
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct NotionConnectorConfig {
    integration_id: String,
}

impl NotionConnectorConfig {
    fn from_value(config: &serde_json::Value) -> Result<Self, NotionError> {
        let integration_id = config
            .get("integration_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| NotionError {
                kind: NotionErrorKind::Other(
                    "Notion connector config missing 'integration_id'. \
                     Add integration_id to the step config in the pipeline definition."
                        .to_string(),
                ),
            })?
            .to_string();
        Ok(NotionConnectorConfig { integration_id })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// JSON extraction from LLM output
// ──────────────────────────────────────────────────────────────────────────────

/// Extract a JSON array from LLM output content.
/// Handles:
///   1. Bare JSON array (after stripping YAML frontmatter)
///   2. JSON array wrapped in ```json ... ``` code fence
///   3. JSON array wrapped in ``` ... ``` bare code fence
///
/// Returns `NotionErrorKind::JsonParse` with the full raw body stored in
/// `raw_output` on parse failure. The display `message` includes a 500-char
/// preview for human-readable errors.
fn extract_json_array(content: &str) -> Result<Vec<serde_json::Value>, NotionError> {
    let body = crate::connectors::strip_frontmatter(content);
    let trimmed = body.trim();

    // Try direct parse (bare JSON array)
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
        return Ok(arr);
    }

    // Try extracting from ```json ... ``` fence
    if let Some(fence_start) = trimmed.find("```json") {
        let after_fence = &trimmed[fence_start + 7..];
        if let Some(fence_end) = after_fence.find("```") {
            let json_str = after_fence[..fence_end].trim();
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                return Ok(arr);
            }
        }
    }

    // Try extracting from ``` (no language tag) fence
    if let Some(fence_start) = trimmed.find("```\n") {
        let after_fence = &trimmed[fence_start + 4..];
        if let Some(fence_end) = after_fence.find("```") {
            let json_str = after_fence[..fence_end].trim();
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                return Ok(arr);
            }
        }
    }

    // Parse failed — return JsonParse error with full raw body stored for retry,
    // plus a 500-char preview in the human-readable message.
    let preview = &trimmed[..trimmed.len().min(500)];
    Err(NotionError {
        kind: NotionErrorKind::JsonParse {
            message: format!(
                "Notion connector: could not parse LLM output as JSON array.\n\
                 Expected a JSON array like: [{{\"Title\": \"...\", ...}}]\n\
                 Raw LLM output (first 500 chars): {}",
                preview
            ),
            raw_output: trimmed.to_string(),
        },
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// LLM output validation
// ──────────────────────────────────────────────────────────────────────────────

/// Writable property types — used for validation to check that LLM output
/// contains at least one key matching a property the connector can map.
const WRITABLE_TYPES: &[&str] = &[
    "title", "rich_text", "select", "multi_select", "people",
    "date", "number", "checkbox", "url", "email", "phone_number", "status",
];

/// Validate that each item in the parsed JSON array has at least one key
/// matching a writable property from the integration profile.
///
/// Returns `NotionErrorKind::JsonParse` on structural mismatch — these errors
/// benefit from the same corrective-prompt retry as parse failures.
fn validate_llm_output_for_notion(
    items: &[serde_json::Value],
    profile: &NotionIntegrationProfile,
    raw_output: &str,
) -> Result<(), NotionError> {
    if items.is_empty() {
        return Err(NotionError {
            kind: NotionErrorKind::JsonParse {
                message: format!(
                    "Notion connector: LLM output parsed as empty JSON array — no pages to create.\n\
                     Raw LLM output (first 500 chars): {}",
                    &raw_output[..raw_output.len().min(500)]
                ),
                raw_output: raw_output.to_string(),
            },
        });
    }

    // Collect writable property names from the profile
    let writable_names: std::collections::HashSet<&str> = profile.properties.iter()
        .filter(|p| WRITABLE_TYPES.contains(&p.property_type.as_str()))
        .map(|p| p.name.as_str())
        .collect();

    for (idx, item) in items.iter().enumerate() {
        let obj = match item.as_object() {
            Some(o) => o,
            None => return Err(NotionError {
                kind: NotionErrorKind::JsonParse {
                    message: format!(
                        "Notion connector: JSON array element {} is not an object.\n\
                         Expected objects like {{\"Title\": \"...\", ...}}\n\
                         Raw LLM output (first 500 chars): {}",
                        idx,
                        &raw_output[..raw_output.len().min(500)]
                    ),
                    raw_output: raw_output.to_string(),
                },
            }),
        };

        // Check that at least one key matches a writable profile property
        let has_valid_key = obj.keys().any(|k| writable_names.contains(k.as_str()));
        if !has_valid_key {
            return Err(NotionError {
                kind: NotionErrorKind::JsonParse {
                    message: format!(
                        "Notion connector: JSON array element {} has no keys matching the database schema.\n\
                         Expected property names: {}\n\
                         Got keys: {}\n\
                         Raw LLM output (first 500 chars): {}",
                        idx,
                        writable_names.iter().copied().collect::<Vec<_>>().join(", "),
                        obj.keys().cloned().collect::<Vec<_>>().join(", "),
                        &raw_output[..raw_output.len().min(500)]
                    ),
                    raw_output: raw_output.to_string(),
                },
            });
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Select value resolution
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve a select/status value against the profile's known options using
/// case-insensitive matching. Returns the canonical (profile) casing on match,
/// or the original value unchanged if no match (passes through to Notion API).
fn resolve_select_value(
    value: &str,
    property_name: &str,
    profile: &NotionIntegrationProfile,
) -> String {
    // Find the property definition in the profile
    let prop_def = profile
        .properties
        .iter()
        .find(|p| p.name == property_name);

    let Some(prop_def) = prop_def else {
        // Property not in profile — pass value through unchanged
        return value.to_string();
    };

    if prop_def.select_options.is_empty() {
        // Empty options (e.g. Status property with unextracted options) — pass through
        return value.to_string();
    }

    // Case-insensitive match against known options
    match prop_def
        .select_options
        .iter()
        .find(|opt| opt.eq_ignore_ascii_case(value))
    {
        Some(canonical) => canonical.clone(),
        // Value not in known options — pass through; Notion API will reject if truly invalid
        None => value.to_string(),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// People alias resolution
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve an array of alias values to Notion User structs via the profile's
/// people_mappings. Aliases that don't match any mapping are silently skipped.
fn resolve_people_aliases(
    aliases: &[serde_json::Value],
    profile: &NotionIntegrationProfile,
) -> Vec<User> {
    aliases
        .iter()
        .filter_map(|alias_val| {
            let alias_str = alias_val.as_str()?;
            // Find mapping by alias (case-insensitive)
            let mapping = profile
                .people_mappings
                .iter()
                .find(|m| m.alias.eq_ignore_ascii_case(alias_str))?;
            // Construct User — notion-client User may not implement Default,
            // so we explicitly set all known fields.
            // Note: "avator_url" is the crate's typo (not "avatar_url").
            Some(User {
                object: "user".to_string(),
                id: mapping.notion_user_id.clone(),
                name: Some(mapping.display_name.clone()),
                avatar_url: None,
                user_type: None,
            })
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Property building
// ──────────────────────────────────────────────────────────────────────────────

/// Build the `BTreeMap<String, PageProperty>` needed for `CreateAPageRequest`.
/// Iterates the profile's property definitions (not the JSON keys) to ensure
/// only known schema properties are sent to Notion.
///
/// Returns an error if the resulting property map is empty (nothing could be mapped).
fn build_notion_properties(
    item: &serde_json::Value,
    profile: &NotionIntegrationProfile,
) -> Result<BTreeMap<String, PageProperty>, NotionError> {
    let obj = item
        .as_object()
        .ok_or_else(|| NotionError {
            kind: NotionErrorKind::Other(
                "JSON item is not an object — expected a JSON object with property names as keys"
                    .to_string(),
            ),
        })?;

    let mut properties: BTreeMap<String, PageProperty> = BTreeMap::new();

    for prop_def in &profile.properties {
        let value = match obj.get(&prop_def.name) {
            Some(v) => v,
            None => continue, // LLM didn't provide this property — skip
        };

        // Skip null values entirely
        if value.is_null() {
            continue;
        }

        let page_prop = match prop_def.property_type.as_str() {
            "title" => {
                let text = value.as_str().unwrap_or("").to_string();
                PageProperty::Title {
                    id: None,
                    title: vec![RichText::Text {
                        text: Text {
                            content: text,
                            link: None,
                        },
                        annotations: None,
                        plain_text: None,
                        href: None,
                    }],
                }
            }

            "rich_text" => {
                let text = value.as_str().unwrap_or("").to_string();
                PageProperty::RichText {
                    id: None,
                    rich_text: vec![RichText::Text {
                        text: Text {
                            content: text,
                            link: None,
                        },
                        annotations: None,
                        plain_text: None,
                        href: None,
                    }],
                }
            }

            "select" => {
                let raw = value.as_str().unwrap_or("");
                let canonical = resolve_select_value(raw, &prop_def.name, profile);
                PageProperty::Select {
                    id: None,
                    select: Some(SelectPropertyValue {
                        id: None,
                        name: Some(canonical),
                        color: None,
                    }),
                }
            }

            "multi_select" => {
                let values: Vec<SelectPropertyValue> = match value {
                    serde_json::Value::Array(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| resolve_select_value(s, &prop_def.name, profile))
                        .map(|name| SelectPropertyValue {
                            id: None,
                            name: Some(name),
                            color: None,
                        })
                        .collect(),
                    serde_json::Value::String(s) => {
                        vec![SelectPropertyValue {
                            id: None,
                            name: Some(resolve_select_value(s, &prop_def.name, profile)),
                            color: None,
                        }]
                    }
                    _ => vec![],
                };
                PageProperty::MultiSelect {
                    id: None,
                    multi_select: values,
                }
            }

            "people" => {
                let aliases: Vec<serde_json::Value> = match value {
                    serde_json::Value::Array(arr) => arr.clone(),
                    serde_json::Value::String(s) => {
                        vec![serde_json::Value::String(s.clone())]
                    }
                    // null was already skipped above; other types: skip property
                    _ => continue,
                };
                let users = resolve_people_aliases(&aliases, profile);
                // Do NOT send empty people array — could clear existing assignees
                if users.is_empty() {
                    continue;
                }
                PageProperty::People {
                    id: None,
                    people: users,
                }
            }

            "date" => {
                let date_str = value.as_str().unwrap_or("").to_string();
                // Use DateOrDateTime::DateTime for strings containing 'T' (ISO 8601 datetime),
                // otherwise DateOrDateTime::Date for date-only strings.
                let date_value = if date_str.contains('T') {
                    let dt = date_str.parse::<chrono::DateTime<Utc>>()
                        .map_err(|e| NotionError { kind: NotionErrorKind::Other(format!("Invalid datetime '{}': {}", date_str, e)) })?;
                    DateOrDateTime::DateTime(dt)
                } else {
                    let d = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
                        .map_err(|e| NotionError { kind: NotionErrorKind::Other(format!("Invalid date '{}': {}", date_str, e)) })?;
                    DateOrDateTime::Date(d)
                };
                PageProperty::Date {
                    id: None,
                    date: Some(DatePropertyValue {
                        start: Some(date_value),
                        end: None,
                        time_zone: None,
                    }),
                }
            }

            "number" => {
                // Convert to f64 then to serde_json::Number for the PageProperty::Number variant.
                // The notion-client Number type is serde_json::Number (re-exported or aliased).
                if let Some(num_f64) = value.as_f64() {
                    if let Some(num) = serde_json::Number::from_f64(num_f64) {
                        PageProperty::Number {
                            id: None,
                            number: Some(num),
                        }
                    } else {
                        // NaN/Infinity — skip
                        continue;
                    }
                } else {
                    // Value is not a number — skip
                    continue;
                }
            }

            "checkbox" => PageProperty::Checkbox {
                id: None,
                checkbox: value.as_bool().unwrap_or(false),
            },

            "url" => PageProperty::Url {
                id: None,
                url: value.as_str().map(|s| s.to_string()),
            },

            "email" => PageProperty::Email {
                id: None,
                email: value.as_str().map(|s| s.to_string()),
            },

            "phone_number" => PageProperty::PhoneNumber {
                id: None,
                phone_number: value.as_str().map(|s| s.to_string()),
            },

            "status" => {
                // Treat status like select — resolve_select_value passes through when options empty
                let raw = value.as_str().unwrap_or("");
                let canonical = resolve_select_value(raw, &prop_def.name, profile);
                PageProperty::Status {
                    id: None,
                    status: Some(SelectPropertyValue {
                        id: None,
                        name: Some(canonical),
                        color: None,
                    }),
                }
            }

            // Computed/read-only property types — skip silently
            "formula"
            | "rollup"
            | "relation"
            | "created_time"
            | "last_edited_time"
            | "created_by"
            | "last_edited_by"
            | "unique_id"
            | "unknown" => continue,

            _ => continue,
        };

        properties.insert(prop_def.name.clone(), page_prop);
    }

    if properties.is_empty() {
        return Err(NotionError {
            kind: NotionErrorKind::Other(
                "No properties could be mapped from LLM output to Notion schema. \
                 Verify the integration profile is synced (Settings > Integrations > Sync Schema), \
                 and that the LLM output contains at least one key matching a writable property name."
                    .to_string(),
            ),
        });
    }

    Ok(properties)
}

// ──────────────────────────────────────────────────────────────────────────────
// Output file helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Write the step output .md file on success.
fn write_success_output(
    output_path: &Path,
    step_name: &str,
    description: Option<&str>,
    input_step: &str,
    integration_id: &str,
    page_ids: &[String],
) -> Result<(), NotionError> {
    let now = Utc::now().to_rfc3339();
    let pages_summary = if page_ids.is_empty() {
        "No pages created (empty input array)".to_string()
    } else {
        format!("Created {} Notion page(s): {}", page_ids.len(), page_ids.join(", "))
    };

    let frontmatter = format!(
        r#"---
name: {}
description: "{}"
connector: notion
input: {}
status: done
created_at: {}
completed_at: {}
integration_id: {}
pages_created: {}
error: null
---

{}
"#,
        step_name,
        description.unwrap_or("Create Notion pages"),
        input_step,
        now,
        now,
        integration_id,
        page_ids.len(),
        pages_summary
    );

    fs::write(output_path, frontmatter)
        .map_err(|e| NotionError {
            kind: NotionErrorKind::Other(format!("Failed to write output file: {}", e)),
        })
}

/// Write the step output .md file on failure, optionally preserving raw AI output.
///
/// When `raw_llm_output` is `Some`, the raw output is appended to the file body
/// so users can inspect what the AI actually returned and diagnose prompt issues.
fn write_failure_output(
    output_path: &Path,
    step_name: &str,
    description: Option<&str>,
    input_step: &str,
    integration_id: &str,
    error_message: &str,
    raw_llm_output: Option<&str>,
) -> Result<(), NotionError> {
    let now = Utc::now().to_rfc3339();
    let error_escaped = error_message.replace('"', "\\\"").replace('\n', " ");

    let frontmatter = format!(
        "---\nname: {}\ndescription: \"{}\"\nconnector: notion\ninput: {}\nstatus: failed\ncreated_at: {}\ncompleted_at: {}\nintegration_id: {}\npages_created: 0\nerror: \"{}\"\n---\n",
        step_name,
        description.unwrap_or("Create Notion pages"),
        input_step,
        now,
        now,
        integration_id,
        error_escaped,
    );

    let body = match raw_llm_output {
        Some(raw) => format!(
            "\n## Error\n{}\n\n## Raw AI Output\n{}\n",
            error_message, raw
        ),
        None => format!("\n## Error\n{}\n", error_message),
    };

    let content = format!("{}{}", frontmatter, body);

    fs::write(output_path, content)
        .map_err(|e| NotionError {
            kind: NotionErrorKind::Other(format!("Failed to write failure output file: {}", e)),
        })
}

// ──────────────────────────────────────────────────────────────────────────────
// Core execution logic (shared between execute() and execute_structured())
// ──────────────────────────────────────────────────────────────────────────────

/// Inner async function: validates and creates Notion pages from an LLM output file.
/// Returns `Result<(PathBuf, Vec<String>), NotionError>` where the Vec contains page IDs.
async fn execute_inner(
    input_path: &Path,
    config: &serde_json::Value,
    output_dir: &Path,
    step_name: &str,
    input_step: &str,
    description: Option<&str>,
) -> Result<(PathBuf, Vec<String>, String), NotionError> {
    // Parse connector config
    let connector_config = NotionConnectorConfig::from_value(config)?;

    // Load integration profile from disk
    let profile = load_notion_profile(&connector_config.integration_id)
        .map_err(|e| NotionError { kind: NotionErrorKind::Other(e) })?;

    // Get Notion API token (never logged or included in errors)
    let token = get_notion_token(&connector_config.integration_id)
        .map_err(|e| NotionError { kind: NotionErrorKind::Other(e) })?;

    // Create Notion client
    let client = Client::new(token, None)
        .map_err(|e| NotionError {
            kind: NotionErrorKind::Other(format!("Failed to create Notion client: {:?}", e)),
        })?;

    // Read the input file (previous step's output)
    let raw = fs::read_to_string(input_path)
        .map_err(|e| NotionError {
            kind: NotionErrorKind::Other(format!(
                "Failed to read input file '{}': {}",
                input_path.display(),
                e
            )),
        })?;

    // Extract JSON array from LLM output (handles bare JSON and code fences)
    // Returns NotionErrorKind::JsonParse on failure.
    let items = extract_json_array(&raw)?;

    // Validate LLM output structure against the integration profile schema.
    // Returns NotionErrorKind::JsonParse on structural mismatch.
    validate_llm_output_for_notion(&items, &profile, &raw)?;

    // Create one Notion page per JSON array element
    let mut page_ids: Vec<String> = Vec::new();
    for item in &items {
        let properties = build_notion_properties(item, &profile)?;

        let request = CreateAPageRequest {
            parent: Parent::DatabaseId {
                database_id: profile.database_id.clone(),
            },
            icon: None,
            cover: None,
            properties,
            children: None,
        };

        let page = client
            .pages
            .create_a_page(request)
            .await
            .map_err(|e| NotionError {
                kind: NotionErrorKind::Other(format!("Failed to create Notion page: {:?}", e)),
            })?;

        page_ids.push(page.id.clone());
    }

    // Ensure output directory exists
    fs::create_dir_all(output_dir)
        .map_err(|e| NotionError {
            kind: NotionErrorKind::Other(format!("Failed to create output directory: {}", e)),
        })?;

    let output_path = output_dir.join(format!("{}.md", step_name));

    write_success_output(
        &output_path,
        step_name,
        description,
        input_step,
        &connector_config.integration_id,
        &page_ids,
    )?;

    Ok((output_path, page_ids, connector_config.integration_id))
}

// ──────────────────────────────────────────────────────────────────────────────
// Execute entry points
// ──────────────────────────────────────────────────────────────────────────────

/// Execute Notion connector: parse LLM JSON output, build Notion PageProperty maps,
/// and create one Notion database page per JSON array element.
///
/// Matches the standard connector signature used by pipeline_engine.rs.
/// For structured error handling (JSON retry logic), use `execute_structured()`.
/// Execute Notion connector with structured error return.
///
/// Returns `NotionError` instead of `String` so the pipeline engine can
/// distinguish `JsonParse` failures (which benefit from a corrective-prompt
/// retry) from `Other` failures (which do not).
///
/// On `JsonParse` failure the step output .md file is NOT written — the
/// pipeline engine must call `execute_with_raw_preservation()` after a failed
/// retry to persist the failure state with raw output.
pub async fn execute_structured(
    input_path: &Path,
    config: &serde_json::Value,
    output_dir: &Path,
    step_name: &str,
    input_step: &str,
    description: Option<&str>,
) -> Result<PathBuf, NotionError> {
    execute_inner(input_path, config, output_dir, step_name, input_step, description)
        .await
        .map(|(path, _, _)| path)
}

/// Execute Notion connector and write the step .md file with raw AI output
/// preserved on any failure.
///
/// This is the final-failure write path called by the pipeline engine after a
/// retry also fails. It ensures the raw LLM output (from whichever attempt
/// failed last) is always visible in the step output file.
///
/// - `raw_llm_output`: The complete raw LLM output to preserve. If `None`,
///   falls back to writing a failure file without raw output (same as `execute()`).
pub async fn execute_with_raw_preservation(
    input_path: &Path,
    config: &serde_json::Value,
    output_dir: &Path,
    step_name: &str,
    input_step: &str,
    description: Option<&str>,
    raw_llm_output: Option<&str>,
) -> Result<PathBuf, String> {
    let result = execute_inner(input_path, config, output_dir, step_name, input_step, description).await;

    match result {
        Ok((path, _, _)) => Ok(path),
        Err(e) => {
            // On failure, write the step .md file with the raw output preserved
            let error_message = e.to_string();

            // Ensure output dir exists (inner may not have reached that point)
            let _ = fs::create_dir_all(output_dir);

            let output_path = output_dir.join(format!("{}.md", step_name));

            // Extract integration_id for the failure output file
            let integration_id = config
                .get("integration_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let _ = write_failure_output(
                &output_path,
                step_name,
                description,
                input_step,
                integration_id,
                &error_message,
                raw_llm_output,
            );

            Err(error_message)
        }
    }
}
