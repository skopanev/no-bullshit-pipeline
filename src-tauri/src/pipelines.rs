use crate::config::{AppSettings, CURRENT_DATA_SCHEMA_VERSION, StepType, get_config_dir};
use crate::storage::{RecordingMetadata, get_data_dir};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

/// Definitions have one source of truth (`~/.nbp/pipelines.json`). Serialize
/// every read-modify-write sequence so two UI actions cannot overwrite each
/// other's snapshot.
static PIPELINES_LOCK: Mutex<()> = Mutex::new(());

/// A single step in a pipeline.
///
/// Self-contained: the step carries its own type + inline config (CLI binary
/// & model, or shell cwd/env/timeout). No separate Connection object. The
/// chain is strictly linear — `{processing_result}` is the immediately
/// previous step's output; step 1 sees an empty `{processing_result}`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PipelineStep {
    pub name: String,
    /// Which connector dispatches this step.
    #[serde(alias = "connection_type")]
    pub step_type: StepType,
    /// Free-form text with `{transcript}` / `{app}` / `{processing_result}`
    /// placeholders (CLI agent) or a bash script body (Shell). Engine renders
    /// placeholders for CLI; Shell gets raw values via NBP_* env vars.
    pub template: String,
    /// Inline non-secret config for this step's connector.
    ///   CLI:   { cli, model?, timeout_secs?, working_directory? }
    ///   Shell: { cwd, shell?, env?, timeout_secs? }
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Pipeline definition (stored in pipelines.json)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Pipeline {
    pub name: String,
    pub description: String,
    /// Defaulted so a legacy entry missing `steps` loads as a zero-step
    /// (label-only) pipeline instead of being dropped by the loose loader.
    #[serde(default)]
    pub steps: Vec<PipelineStep>,
    /// When true, this pipeline runs automatically after every recording
    /// finishes transcribing (in addition to any explicitly-assigned pipelines).
    #[serde(default)]
    pub auto_run: bool,
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
    Waiting, // Assigned but transcript not ready
    Running, // Currently executing
    Done,    // All steps completed successfully
    Partial, // Stopped due to step failure
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

/// Move the legacy pipelines.json out of the recordings directory. This is
/// called only by the schema migration at startup; normal reads stay pure.
fn migrate_pipelines_to_config_dir() -> Result<(), String> {
    let old_path = get_data_dir().join("pipelines.json");
    let config_dir = get_config_dir();
    let new_path = config_dir.join("pipelines.json");
    if new_path.exists() {
        let mut permissions = fs::metadata(&new_path)
            .map_err(|e| format!("Failed to inspect pipelines.json: {e}"))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&new_path, permissions)
            .map_err(|e| format!("Failed to protect pipelines.json: {e}"))?;
        return Ok(());
    }
    if !old_path.exists() {
        return Ok(());
    }

    fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create pipeline config directory: {e}"))?;

    // A custom recordings directory can live on another filesystem, where a
    // direct rename is not supported. Fall back to copy + atomic finalize.
    if fs::rename(&old_path, &new_path).is_ok() {
        let mut permissions = fs::metadata(&new_path)
            .map_err(|e| format!("Failed to inspect migrated pipelines.json: {e}"))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&new_path, permissions)
            .map_err(|e| format!("Failed to protect migrated pipelines.json: {e}"))?;
        return Ok(());
    }

    let temp_path = new_path.with_extension("json.migrating");
    fs::copy(&old_path, &temp_path)
        .map_err(|e| format!("Failed to copy legacy pipelines.json: {e}"))?;
    let mut permissions = fs::metadata(&temp_path)
        .map_err(|e| format!("Failed to inspect migrated pipelines.json: {e}"))?
        .permissions();
    permissions.set_mode(0o600);
    if let Err(error) = fs::set_permissions(&temp_path, permissions) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to protect migrated pipelines.json: {error}"
        ));
    }
    if let Err(error) = fs::rename(&temp_path, &new_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to finalize migrated pipelines.json: {error}"
        ));
    }
    if let Err(error) = fs::remove_file(&old_path) {
        log::warn!(
            "pipeline storage migration: legacy file remains at {}: {}",
            old_path.display(),
            error
        );
    }
    Ok(())
}

/// Validate a pipeline definition.
pub fn validate_pipeline(pipeline: &Pipeline) -> Result<(), String> {
    // Pipeline must have non-empty name
    if pipeline.name.trim().is_empty() {
        return Err("Pipeline name cannot be empty".to_string());
    }

    // Pipeline name must be filesystem-safe (no slashes, colons, null bytes)
    if pipeline.name.contains('/')
        || pipeline.name.contains('\\')
        || pipeline.name.contains('\0')
        || pipeline.name.contains(':')
    {
        return Err("Pipeline name contains invalid characters (/, \\, :, or null)".to_string());
    }

    if pipeline.steps.is_empty() {
        return Err("Pipeline must contain at least one step".to_string());
    }

    let mut defined_steps: Vec<String> = Vec::new();

    for (i, step) in pipeline.steps.iter().enumerate() {
        // Step must have non-empty name
        if step.name.trim().is_empty() {
            return Err(format!("Step {} has empty name", i + 1));
        }

        // Step name must be filesystem-safe
        if step.name.contains('/')
            || step.name.contains('\\')
            || step.name.contains('\0')
            || step.name.contains(':')
        {
            return Err(format!(
                "Step '{}' name contains invalid characters (/, \\, :, or null)",
                step.name
            ));
        }

        // Check for duplicate step names
        if defined_steps.contains(&step.name) {
            return Err(format!("Duplicate step name '{}'", step.name));
        }

        defined_steps.push(step.name.clone());
    }

    Ok(())
}

/// Load all pipelines from disk.
///
/// Legacy migration (Option A): pre-simplification pipelines carried steps of
/// now-removed delivery types (notion / slack / telegram / webhook /
/// save_local) and a `connection_id` field. We parse loosely first and drop
/// any step whose type isn't a current [`StepType`] — the leftover current
/// steps survive and unknown fields like `connection_id` are ignored by serde.
/// Invalid JSON or a pipeline that still won't parse is returned as an error so
/// a later write cannot silently overwrite user data with a partial snapshot.
fn load_pipelines_from_disk() -> Result<HashMap<String, Pipeline>, String> {
    let path = get_pipelines_path();

    if !path.exists() {
        return Ok(HashMap::new());
    }

    let raw =
        fs::read_to_string(&path).map_err(|e| format!("Failed to read pipelines.json: {}", e))?;

    // Parse through Value so removed step types can be pruned before the
    // strongly typed pass. Any remaining error aborts the snapshot load.
    let map: HashMap<String, serde_json::Value> = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => return Err(format!("pipelines.json is invalid: {e}")),
    };

    let mut pipelines = HashMap::new();
    for (name, value) in map {
        match prune_and_parse_pipeline(value) {
            Ok(mut p) => {
                // The object key is the durable identity in the legacy format.
                // Normalize an old mismatched embedded name in memory instead
                // of exposing two identities to callers.
                p.name = name.clone();
                pipelines.insert(name, p);
            }
            Err(e) => {
                return Err(format!("Pipeline '{name}' is invalid: {e}"));
            }
        }
    }
    Ok(pipelines)
}

pub fn load_pipelines() -> Result<HashMap<String, Pipeline>, String> {
    let _guard = PIPELINES_LOCK
        .lock()
        .map_err(|_| "Pipeline storage lock poisoned".to_string())?;
    load_pipelines_from_disk()
}

/// Drop steps of removed networked-delivery types (notion / slack / telegram /
/// webhook) from a loosely-parsed pipeline value, then deserialize. The step
/// type lives under `step_type` (new) or `connection_type` (legacy alias);
/// only the current [`StepType`]s survive (`cli_agent` / `shell` /
/// `save_local`). Legacy `save_local` steps carried their folder on a
/// Connection, so they load with empty config — the user re-picks the folder.
fn prune_and_parse_pipeline(mut value: serde_json::Value) -> Result<Pipeline, serde_json::Error> {
    if let Some(steps) = value.get_mut("steps").and_then(|v| v.as_array_mut()) {
        steps.retain(|s| {
            let ty = s
                .get("step_type")
                .or_else(|| s.get("connection_type"))
                .and_then(|v| v.as_str());
            matches!(ty, Some("cli_agent") | Some("shell") | Some("save_local"))
        });
    }
    serde_json::from_value::<Pipeline>(value)
}

/// Save all pipelines to disk
fn save_pipelines_to_disk_unlocked(pipelines: &HashMap<String, Pipeline>) -> Result<(), String> {
    let config_dir = get_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
    }

    let path = get_pipelines_path();
    let temp_path = path.with_extension("json.tmp");
    // Stable key ordering keeps the file reviewable and avoids churn whenever
    // HashMap iteration order changes.
    let ordered: BTreeMap<&String, &Pipeline> = pipelines.iter().collect();
    let content = serde_json::to_string_pretty(&ordered)
        .map_err(|e| format!("Failed to serialize pipelines: {}", e))?;
    fs::write(&temp_path, content)
        .map_err(|e| format!("Failed to write temporary pipelines.json: {e}"))?;
    let mut permissions = fs::metadata(&temp_path)
        .map_err(|e| format!("Failed to inspect temporary pipelines.json: {e}"))?
        .permissions();
    permissions.set_mode(0o600);
    if let Err(error) = fs::set_permissions(&temp_path, permissions) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to protect temporary pipelines.json: {error}"
        ));
    }
    if let Err(error) = fs::rename(&temp_path, &path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("Failed to finalize pipelines.json: {error}"));
    }

    Ok(())
}

/// List all pipeline definitions
#[tauri::command]
pub fn list_pipelines() -> Result<Vec<Pipeline>, String> {
    let pipelines = load_pipelines()?;
    let mut list: Vec<Pipeline> = pipelines.into_values().collect();
    // HashMap iteration order is non-deterministic and shifts whenever the map
    // is mutated (e.g. after save_pipeline), which made the list reshuffle on
    // every reload — toggling one pipeline's auto-run looked like it changed a
    // different row. Sort by name for a stable, predictable order.
    list.sort_by_key(|a| a.name.to_lowercase());
    Ok(list)
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
fn rewrite_pipeline_reference(
    reference: &mut Option<String>,
    old_name: &str,
    new_name: Option<&str>,
) -> bool {
    if reference.as_deref() != Some(old_name) {
        return false;
    }
    *reference = new_name.map(str::to_string);
    true
}

fn rewrite_settings_pipeline_references(
    settings: &mut AppSettings,
    old_name: &str,
    new_name: Option<&str>,
) -> usize {
    let mut changed = 0;
    changed +=
        rewrite_pipeline_reference(&mut settings.default_pipeline, old_name, new_name) as usize;
    changed +=
        rewrite_pipeline_reference(&mut settings.last_used_pipeline, old_name, new_name) as usize;
    for shortcut in &mut settings.dictation.shortcuts {
        changed += rewrite_pipeline_reference(&mut shortcut.pipeline, old_name, new_name) as usize;
    }
    changed
}

#[tauri::command]
pub fn save_pipeline(
    app: tauri::AppHandle,
    mut pipeline: Pipeline,
    previous_name: Option<String>,
) -> Result<(), String> {
    validate_pipeline(&pipeline)?;

    let _guard = PIPELINES_LOCK
        .lock()
        .map_err(|_| "Pipeline storage lock poisoned".to_string())?;
    let mut pipelines = load_pipelines_from_disk()?;
    let original = pipelines.clone();
    let previous_name = previous_name.filter(|name| !name.trim().is_empty());
    let new_name = pipeline.name.clone();

    if let Some(old_name) = previous_name.as_deref() {
        if !pipelines.contains_key(old_name) {
            return Err(format!("Pipeline '{old_name}' not found"));
        }
        if old_name != pipeline.name && pipelines.contains_key(&pipeline.name) {
            return Err(format!("Pipeline '{}' already exists", pipeline.name));
        }
    } else if pipelines.contains_key(&pipeline.name) {
        return Err(format!("Pipeline '{}' already exists", pipeline.name));
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // Preserve created_at across updates and renames.
    if let Some(existing) = previous_name
        .as_deref()
        .and_then(|old_name| pipelines.get(old_name))
    {
        pipeline.created_at = existing.created_at.clone();
    } else {
        pipeline.created_at = now.clone();
    }
    pipeline.updated_at = now;
    if let Some(old_name) = previous_name.as_deref()
        && old_name != pipeline.name
    {
        pipelines.remove(old_name);
    }
    pipelines.insert(pipeline.name.clone(), pipeline);
    save_pipelines_to_disk_unlocked(&pipelines)?;

    // Name-based references live in settings. Update them as part of the same
    // backend operation; if settings persistence fails, restore the original
    // definitions so callers never receive an error after a half-rename.
    if let Some(old_name) = previous_name.as_deref()
        && old_name != new_name
    {
        let mut settings = crate::config::load_settings();
        if rewrite_settings_pipeline_references(&mut settings, old_name, Some(&new_name)) > 0
            && let Err(error) = crate::config::save_settings_to_disk(&mut settings)
        {
            let _ = save_pipelines_to_disk_unlocked(&original);
            return Err(format!("Failed to update pipeline references: {error}"));
        }
    }

    // Live-update tray submenu so the "Record" list reflects the change
    // without an app restart.
    crate::refresh_tray_menu(&app);
    Ok(())
}

/// Delete a pipeline definition
#[tauri::command]
pub fn delete_pipeline(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let _guard = PIPELINES_LOCK
        .lock()
        .map_err(|_| "Pipeline storage lock poisoned".to_string())?;
    let mut pipelines = load_pipelines_from_disk()?;
    let original = pipelines.clone();
    if pipelines.remove(&name).is_none() {
        return Err(format!("Pipeline '{}' not found", name));
    }
    save_pipelines_to_disk_unlocked(&pipelines)?;

    let mut settings = crate::config::load_settings();
    if rewrite_settings_pipeline_references(&mut settings, &name, None) > 0
        && let Err(error) = crate::config::save_settings_to_disk(&mut settings)
    {
        let _ = save_pipelines_to_disk_unlocked(&original);
        return Err(format!("Failed to clear pipeline references: {error}"));
    }

    crate::refresh_tray_menu(&app);
    Ok(())
}

const LEGACY_TAG_PIPELINE_DESCRIPTION: &str = "Label (migrated from tag)";

fn is_legacy_tag_definition(pipeline: &Pipeline) -> bool {
    pipeline.description == LEGACY_TAG_PIPELINE_DESCRIPTION
        && pipeline.steps.is_empty()
        && !pipeline.auto_run
}

fn sanitize_legacy_tag_name(tag: &str) -> String {
    tag.replace(['/', '\\', ':', '\0'], "-")
}

fn is_legacy_tag_state(
    state: &PipelineState,
    tag_names: &HashSet<String>,
    valid_definitions: &HashSet<String>,
    removed_legacy_definitions: &HashSet<String>,
) -> bool {
    let synthetic_shape = state.status == PipelineStatus::Done
        && state.run_index == 0
        && state.current_step.is_none()
        && state.error.is_none()
        && state.started_at.is_some()
        && state.started_at == state.completed_at;
    synthetic_shape
        && tag_names.contains(&state.name)
        && (removed_legacy_definitions.contains(&state.name)
            || !valid_definitions.contains(&state.name))
}

fn clear_missing_pipeline_references(
    settings: &mut AppSettings,
    valid_names: &HashSet<String>,
) -> usize {
    fn clear(reference: &mut Option<String>, valid_names: &HashSet<String>) -> bool {
        if reference
            .as_ref()
            .is_some_and(|name| !valid_names.contains(name))
        {
            *reference = None;
            true
        } else {
            false
        }
    }

    let mut changed = 0;
    changed += clear(&mut settings.default_pipeline, valid_names) as usize;
    changed += clear(&mut settings.last_used_pipeline, valid_names) as usize;
    for shortcut in &mut settings.dictation.shortcuts {
        changed += clear(&mut shortcut.pipeline, valid_names) as usize;
    }
    changed
}

#[derive(Default, Debug)]
struct PipelineStorageMigrationStats {
    definitions_removed: usize,
    recording_states_removed: usize,
    recordings_rewritten: usize,
    settings_references_cleared: usize,
    malformed_recordings_skipped: usize,
}

/// Undo the old lazy tag migration. That migration ran from read_metadata /
/// list_recordings and therefore let a read create global pipeline definitions.
/// V1 restores the boundaries: obsolete tags and their synthetic pipeline
/// states are removed, recording pipeline arrays stay real execution history,
/// and only explicit pipeline-editor actions mutate global definitions.
fn migrate_pipeline_storage_v1(
    settings: &mut AppSettings,
) -> Result<PipelineStorageMigrationStats, String> {
    let mut stats = PipelineStorageMigrationStats::default();

    let (valid_definitions, removed_legacy_definitions) = {
        let _guard = PIPELINES_LOCK
            .lock()
            .map_err(|_| "Pipeline storage lock poisoned".to_string())?;
        migrate_pipelines_to_config_dir()?;
        let mut definitions = load_pipelines_from_disk()?;
        let legacy: HashSet<String> = definitions
            .iter()
            .filter(|(_, pipeline)| is_legacy_tag_definition(pipeline))
            .map(|(name, _)| name.clone())
            .collect();
        if !legacy.is_empty() {
            definitions.retain(|name, _| !legacy.contains(name));
            save_pipelines_to_disk_unlocked(&definitions)?;
        }
        stats.definitions_removed = legacy.len();
        (definitions.into_keys().collect(), legacy)
    };

    let data_dir = get_data_dir();
    if data_dir.exists() {
        for entry in fs::read_dir(&data_dir)
            .map_err(|e| format!("Failed to scan recordings for pipeline migration: {e}"))?
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    stats.malformed_recordings_skipped += 1;
                    continue;
                }
            };
            let metadata_path = entry.path().join("metadata.json");
            if !metadata_path.is_file() {
                continue;
            }
            let raw = match fs::read_to_string(&metadata_path) {
                Ok(raw) => raw,
                Err(_) => {
                    stats.malformed_recordings_skipped += 1;
                    continue;
                }
            };
            let raw_json: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!(
                        "pipeline storage migration: skipping malformed {}: {}",
                        metadata_path.display(),
                        error
                    );
                    stats.malformed_recordings_skipped += 1;
                    continue;
                }
            };
            let had_legacy_tags = raw_json.get("tags").is_some();
            let tag_names: HashSet<String> = raw_json
                .get("tags")
                .and_then(|tags| tags.as_array())
                .into_iter()
                .flatten()
                .filter_map(|tag| tag.as_str())
                .map(|tag| sanitize_legacy_tag_name(tag))
                .collect();
            let mut metadata: RecordingMetadata = match serde_json::from_value(raw_json) {
                Ok(metadata) => metadata,
                Err(error) => {
                    log::warn!(
                        "pipeline storage migration: skipping incompatible {}: {}",
                        metadata_path.display(),
                        error
                    );
                    stats.malformed_recordings_skipped += 1;
                    continue;
                }
            };
            let before = metadata.pipelines.len();
            metadata.pipelines.retain(|state| {
                !is_legacy_tag_state(
                    state,
                    &tag_names,
                    &valid_definitions,
                    &removed_legacy_definitions,
                )
            });
            let removed = before - metadata.pipelines.len();
            if removed > 0 || had_legacy_tags {
                crate::storage::write_metadata(&metadata).map_err(|error| {
                    format!("Failed to migrate recording {}: {error}", metadata.id)
                })?;
                stats.recording_states_removed += removed;
                stats.recordings_rewritten += 1;
            }
        }
    }

    // Also repairs already-stale references. This makes the migration fully
    // retryable if an earlier launch removed legacy data but crashed before
    // settings.json received the schema version.
    stats.settings_references_cleared =
        clear_missing_pipeline_references(settings, &valid_definitions);
    Ok(stats)
}

/// One-time, schema-versioned startup migration. The version is persisted only
/// after every required write succeeds, so a crash simply retries the same
/// idempotent migration on the next launch.
pub fn run_storage_migration_if_needed() {
    let mut settings = crate::config::load_settings();
    if settings.data_schema_version >= CURRENT_DATA_SCHEMA_VERSION {
        return;
    }

    match migrate_pipeline_storage_v1(&mut settings) {
        Ok(stats) => {
            settings.data_schema_version = CURRENT_DATA_SCHEMA_VERSION;
            match crate::config::save_settings_to_disk(&mut settings) {
                Ok(()) => eprintln!(
                    "Pipeline storage migration v1 complete: removed {} definitions and {} synthetic runs from {} recordings; cleared {} settings references; skipped {} malformed recordings",
                    stats.definitions_removed,
                    stats.recording_states_removed,
                    stats.recordings_rewritten,
                    stats.settings_references_cleared,
                    stats.malformed_recordings_skipped,
                ),
                Err(error) => eprintln!(
                    "Warning: pipeline storage migration completed but version could not be saved: {error}"
                ),
            }
        }
        Err(error) => eprintln!("Warning: pipeline storage migration failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, st: StepType, template: &str) -> PipelineStep {
        PipelineStep {
            name: name.to_string(),
            step_type: st,
            template: template.to_string(),
            config: serde_json::Value::Null,
            description: None,
        }
    }

    fn make_valid_pipeline() -> Pipeline {
        Pipeline {
            name: "test-pipeline".to_string(),
            description: "A test pipeline".to_string(),
            auto_run: false,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                step("summarize", StepType::CliAgent, "Summarize: {transcript}"),
                step("format", StepType::Shell, "echo \"$NBP_PROCESSING_RESULT\""),
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
    fn test_empty_steps_fails() {
        let mut pipeline = make_valid_pipeline();
        pipeline.steps = vec![];
        let result = validate_pipeline(&pipeline);
        assert_eq!(
            result.unwrap_err(),
            "Pipeline must contain at least one step"
        );
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
    fn test_duplicate_step_names_fails() {
        let pipeline = Pipeline {
            name: "dup-pipeline".to_string(),
            description: "test".to_string(),
            auto_run: false,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                step("step-a", StepType::CliAgent, "{transcript}"),
                step("step-a", StepType::Shell, "{processing_result}"),
            ],
        };
        let result = validate_pipeline(&pipeline);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Duplicate step name"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let pipeline = make_valid_pipeline();
        let json = serde_json::to_string_pretty(&pipeline).unwrap();
        let deserialized: Pipeline = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, pipeline.name);
        assert_eq!(deserialized.steps.len(), pipeline.steps.len());
        assert_eq!(deserialized.steps[0].step_type, StepType::CliAgent);
        assert_eq!(deserialized.steps[1].step_type, StepType::Shell);
        assert_eq!(deserialized.steps[0].template, "Summarize: {transcript}");
    }

    #[test]
    fn test_legacy_connection_type_alias_deserializes() {
        // Old pipelines.json used `connection_type`; serde alias keeps them readable.
        let json = r#"{
            "name": "legacy",
            "description": "",
            "steps": [
                { "name": "s1", "connection_type": "cli_agent", "template": "{transcript}" }
            ]
        }"#;
        let p: Pipeline = serde_json::from_str(json).unwrap();
        assert_eq!(p.steps[0].step_type, StepType::CliAgent);
    }

    #[test]
    fn test_prune_drops_legacy_delivery_steps_keeps_cli_shell() {
        // Legacy pipeline mixing a CLI step (connection_type alias + dead
        // connection_id field) with a Notion delivery step. The Notion step
        // must be dropped; the CLI step survives with its template intact.
        let json = serde_json::json!({
            "name": "mixed",
            "description": "",
            "steps": [
                { "name": "summarize", "connection_type": "cli_agent", "connection_id": "dead-id", "template": "{transcript}" },
                { "name": "to-notion", "connection_type": "notion", "connection_id": "abc", "template": "{processing_result}" },
                { "name": "post", "step_type": "shell", "template": "echo hi" }
            ]
        });
        let p = prune_and_parse_pipeline(json).unwrap();
        assert_eq!(p.steps.len(), 2, "notion step should be dropped");
        assert_eq!(p.steps[0].name, "summarize");
        assert_eq!(p.steps[0].step_type, StepType::CliAgent);
        assert_eq!(p.steps[0].template, "{transcript}");
        assert_eq!(p.steps[1].name, "post");
        assert_eq!(p.steps[1].step_type, StepType::Shell);
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
    fn test_only_exact_legacy_tag_definition_is_classified() {
        let legacy = Pipeline {
            name: "storage".to_string(),
            description: LEGACY_TAG_PIPELINE_DESCRIPTION.to_string(),
            steps: vec![],
            auto_run: false,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(is_legacy_tag_definition(&legacy));

        let mut user_created = legacy.clone();
        user_created.description = "My intentionally empty old pipeline".to_string();
        assert!(!is_legacy_tag_definition(&user_created));

        let mut with_step = legacy;
        with_step
            .steps
            .push(step("real", StepType::Shell, "echo ok"));
        assert!(!is_legacy_tag_definition(&with_step));
    }

    #[test]
    fn test_legacy_tag_state_requires_exact_synthetic_shape() {
        let tags = HashSet::from(["storage".to_string()]);
        let definitions = HashSet::new();
        let legacy_definitions = HashSet::from(["storage".to_string()]);
        let timestamp = Some("2026-01-01T00:00:00Z".to_string());
        let state = PipelineState {
            id: "legacy-run".to_string(),
            name: "storage".to_string(),
            status: PipelineStatus::Done,
            run_index: 0,
            started_at: timestamp.clone(),
            completed_at: timestamp,
            current_step: None,
            error: None,
        };
        assert!(is_legacy_tag_state(
            &state,
            &tags,
            &definitions,
            &legacy_definitions
        ));

        let mut real_run = state;
        real_run.run_index = 1;
        assert!(!is_legacy_tag_state(
            &real_run,
            &tags,
            &definitions,
            &legacy_definitions
        ));
    }

    #[test]
    fn test_missing_settings_references_are_cleared() {
        let mut settings = AppSettings::default();
        settings.default_pipeline = Some("kept".to_string());
        settings.last_used_pipeline = Some("gone".to_string());
        settings
            .dictation
            .shortcuts
            .push(crate::config::DictationShortcut {
                id: "shortcut".to_string(),
                name: "Shortcut".to_string(),
                hotkey: "cmd+e".to_string(),
                input_source: crate::config::DictationInputSource::Audio,
                device_name: None,
                language: None,
                pipeline: Some("gone".to_string()),
                auto_paste: true,
                capture_system_audio: false,
                trigger_mode: crate::config::DictationTriggerMode::Toggle,
            });

        let changed =
            clear_missing_pipeline_references(&mut settings, &HashSet::from(["kept".to_string()]));
        assert_eq!(changed, 2);
        assert_eq!(settings.default_pipeline.as_deref(), Some("kept"));
        assert_eq!(settings.last_used_pipeline, None);
        assert_eq!(settings.dictation.shortcuts[0].pipeline, None);
    }
}
