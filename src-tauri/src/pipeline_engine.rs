use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::config::StepType;
use crate::connectors;
use crate::pipelines::{
    PipelineProgressPayload, PipelineState, PipelineStatus, PipelineStep, StepStatus,
    load_pipelines, validate_pipeline,
};
use crate::storage::get_data_dir;
use crate::transcription::{TranscriptJson, render_transcript_from_json};

lazy_static::lazy_static! {
    /// Per-(recording, pipeline) execution lock.
    /// Ensures runs of the same pipeline on the same recording execute sequentially.
    static ref PIPELINE_EXEC_LOCKS: std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>> =
        std::sync::Mutex::new(HashMap::new());
}

struct LockMapGuard {
    key: String,
}

impl Drop for LockMapGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = PIPELINE_EXEC_LOCKS.lock()
            && let Some(entry) = locks.get(&self.key)
            && Arc::strong_count(entry) <= 2
        {
            locks.remove(&self.key);
        }
    }
}

/// Known topic heading labels (case-insensitive match for Latin)
const TOPIC_LABELS: &[&str] = &["topic", "тема", "tema"];

/// Default app friendly name used for `{app}` substitution when the recording
/// has none on disk (manual / dictation / pre-refactor metadata).
const DEFAULT_APP_NAME: &str = "NBP";

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
                if next.is_empty() || next == "---" {
                    continue;
                }
                let title = next
                    .trim_start_matches(['-', '*', ' '])
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
        "summary",
        "резюме",
        "краткое резюме",
        "краткое содержание",
        "краткое саммари разговора",
        "краткое резюме разговора",
        "краткое содержание разговора",
        "краткое содержание транскрипта",
        "резюме созвона",
        "резюме разговора",
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

/// Read the recording's transcript into memory.
///
/// Prefers `transcript.json` (source of truth) rendered to plain text; falls
/// back to legacy `transcript.md` for old recordings. Returns Err when neither
/// exists — caller treats that as "transcribe first".
fn load_transcript_text(recording_id: &str) -> Result<String, String> {
    let dir = get_data_dir().join(recording_id);
    let json_path = dir.join("transcript.json");
    if json_path.exists() {
        let raw = fs::read_to_string(&json_path)
            .map_err(|e| format!("Failed to read transcript.json: {}", e))?;
        let tj: TranscriptJson = serde_json::from_str(&raw)
            .map_err(|e| format!("Failed to parse transcript.json: {}", e))?;
        return Ok(render_transcript_from_json(&tj));
    }
    let md_path = dir.join("transcript.md");
    if md_path.exists() {
        let raw = fs::read_to_string(&md_path)
            .map_err(|e| format!("Failed to read transcript.md: {}", e))?;
        // Strip frontmatter so the placeholder substitutes only the body
        return Ok(connectors::strip_frontmatter(&raw).to_string());
    }
    Err("No transcript found. Transcribe the recording first.".to_string())
}

/// Resolve the friendly app name for the `{app}` placeholder.
///
/// Single source of truth: the call detector writes it into
/// `metadata.app_friendly_name` at recording start; everything else (manual,
/// dictation, pre-refactor metadata) gets the default. We deliberately do NOT
/// resolve bundle_id → friendly_name at run time — the mapping is incomplete
/// and the value is already known at capture time.
fn resolve_app_name(recording_id: &str) -> String {
    crate::storage::read_metadata(recording_id)
        .ok()
        .and_then(|m| m.app_friendly_name.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_APP_NAME.to_string())
}

/// Substitute the template placeholders. Missing keys silently render as the
/// empty string. `{transcript}` = full recording transcript, `{app}` = friendly
/// app name, `{processing_result}` = previous step's output (empty on step 1),
/// `{calendar_title}` / `{calendar_attendees}` = matched calendar event (empty
/// when unmatched / no calendar access), `{date}` = recording date (YYYY-MM-DD).
#[allow(clippy::too_many_arguments)]
fn render_template(
    template: &str,
    transcript: &str,
    app: &str,
    processing_result: &str,
    calendar_title: &str,
    calendar_attendees: &str,
    date: &str,
) -> String {
    template
        .replace("{transcript}", transcript)
        .replace("{app}", app)
        .replace("{processing_result}", processing_result)
        .replace("{calendar_title}", calendar_title)
        .replace("{calendar_attendees}", calendar_attendees)
        .replace("{date}", date)
}

/// Format a recording's attendees for the `{calendar_attendees}` placeholder:
/// names (email as fallback), comma-separated. Empty when there are none.
fn format_attendees(meta: &crate::storage::RecordingMetadata) -> String {
    meta.attendees
        .iter()
        .filter_map(|a| a.name.clone().or_else(|| a.email.clone()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Recording date as local `YYYY-MM-DD` for the `{date}` placeholder.
fn format_date(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// Result of a single dispatched step.
///
/// `body` is what the engine threads into the next step's `{processing_result}`
/// placeholder. We re-read it from the artifact rather than changing connector
/// signatures to return strings — the connectors (CLI agent / Shell) each write
/// their own `.md` artifact with frontmatter.
struct StepOutcome {
    body: String,
}

/// The per-run context a step executes against. Lets both the recording engine
/// and the dictation flow drive the *same* step dispatch — the only thing that
/// differs between them is this context (a real recording's transcript +
/// pipeline output dir, vs. dictation's text + an ephemeral temp dir).
pub(crate) struct StepContext<'a> {
    pub transcript: &'a str,
    pub app: &'a str,
    /// RFC3339 start time — feeds the save connector's filename.
    pub started_at: &'a str,
    pub output_dir: &'a Path,
    /// Matched calendar event title (empty if unmatched). `{calendar_title}`.
    pub calendar_title: &'a str,
    /// Matched event attendees, comma-separated. `{calendar_attendees}`.
    pub calendar_attendees: &'a str,
    /// Recording date, local `YYYY-MM-DD`. `{date}`.
    pub date: &'a str,
}

/// Render + dispatch a single step, returning the body that feeds the next
/// step's `{processing_result}`. The one shared entry point for running a step
/// — recording and dictation both go through here so step behaviour can't
/// drift between them.
pub(crate) async fn run_one_step(
    step: &PipelineStep,
    processing_result: &str,
    ctx: &StepContext<'_>,
) -> Result<String, String> {
    let rendered = render_template(
        &step.template,
        ctx.transcript,
        ctx.app,
        processing_result,
        ctx.calendar_title,
        ctx.calendar_attendees,
        ctx.date,
    );
    let outcome = dispatch_step(
        step,
        &rendered,
        ctx.transcript,
        ctx.app,
        processing_result,
        ctx.calendar_title,
        ctx.calendar_attendees,
        ctx.date,
        ctx.started_at,
        ctx.output_dir,
    )
    .await?;
    Ok(outcome.body)
}

/// Dispatch one step to its connector.
///
/// Steps are self-contained (no Connection lookup): the engine renders
/// `step.template` into a plain string, merges `step.config` with the injected
/// `prompt` field for cli_agent (Shell takes the raw script + NBP_* env), and
/// hands it to the connector's `execute`. Each connector writes its own
/// `output_dir/<step>.md` artifact; we read it back to extract the body for
/// chaining into the next step.
#[allow(clippy::too_many_arguments)]
async fn dispatch_step(
    step: &PipelineStep,
    rendered_input: &str,
    // Raw values backing the placeholders. CLI agent only sees the
    // pre-rendered `rendered_input`; Shell is script-mode — it gets the RAW
    // values via env vars (so `{transcript}` text can't shell-inject).
    transcript: &str,
    app: &str,
    processing_result: &str,
    calendar_title: &str,
    calendar_attendees: &str,
    date: &str,
    // Recording's RFC3339 start time — used by the save connector to name the
    // output file after the meeting (`<app> <date> <time>.md`).
    started_at: &str,
    output_dir: &Path,
) -> Result<StepOutcome, String> {
    fs::create_dir_all(output_dir).map_err(|e| format!("Failed to create output dir: {}", e))?;

    // Transient input file the CLI connector reads. Hidden (`.<step>.input.txt`)
    // so it doesn't pollute the artifact listing get_step_outputs iterates.
    let input_path = output_dir.join(format!(".{}.input.txt", step.name));
    fs::write(&input_path, rendered_input)
        .map_err(|e| format!("Failed to write step input file: {}", e))?;

    let step_input_label = if rendered_input.is_empty() {
        "empty"
    } else {
        "rendered-template"
    };

    let artifact_path = match step.step_type {
        StepType::CliAgent => {
            // Engine has already substituted `{transcript}` etc. into
            // rendered_input. Feed it as the entire prompt by injecting it into
            // the config the connector reads. Mark it complete so the
            // connector does not append the transient input file a second time.
            let mut cfg = step.config.clone();
            ensure_object(&mut cfg);
            let cfg_obj = cfg.as_object_mut().expect("ensured above");
            cfg_obj.insert(
                "prompt".to_string(),
                serde_json::Value::String(rendered_input.to_string()),
            );
            cfg_obj.insert("prompt_complete".to_string(), serde_json::Value::Bool(true));

            connectors::cli_agent::execute(
                &input_path,
                &cfg,
                output_dir,
                &step.name,
                step_input_label,
                step.description.as_deref(),
            )
            .await?
        }
        StepType::Shell => {
            // Shell is script-mode: step.template IS the shell script body.
            // Raw values go through NBP_* env vars — no inline expansion of
            // transcript text (shell-injection footgun).
            connectors::shell::execute(
                &step.template,
                transcript,
                app,
                processing_result,
                calendar_title,
                calendar_attendees,
                date,
                &step.config,
                output_dir,
                &step.name,
                step_input_label,
                step.description.as_deref(),
            )
            .await?
        }
        StepType::SaveLocal => {
            // The engine already rendered step.template (`{processing_result}`
            // or `{transcript}`) into rendered_input — that's the WHAT. The
            // connector just writes it to the configured folder (WHERE).
            connectors::save::execute(
                rendered_input,
                &step.config,
                app,
                started_at,
                output_dir,
                &step.name,
                step_input_label,
                step.description.as_deref(),
            )
            .await?
        }
    };

    // What feeds the next step's `{processing_result}`. SaveLocal is a
    // transparent side-effect — it passes its INPUT through unchanged so a
    // following step sees the content, not the "Saved to <path>" headline its
    // artifact carries for the UI. CLI/Shell chain their actual output, read
    // back from the artifact (frontmatter stripped).
    let body = if matches!(step.step_type, StepType::SaveLocal) {
        rendered_input.to_string()
    } else {
        fs::read_to_string(&artifact_path)
            .map(|raw| connectors::strip_frontmatter(&raw).trim().to_string())
            .unwrap_or_default()
    };

    let _ = fs::remove_file(&input_path);

    Ok(StepOutcome { body })
}

/// Ensure the JSON value is an object so we can insert into it.
fn ensure_object(v: &mut serde_json::Value) {
    if !v.is_object() {
        *v = serde_json::Value::Object(serde_json::Map::new());
    }
}

/// Write a `<step>.md` artifact for a skipped step so `get_step_outputs`
/// reports the right status without needing the connector to have run.
fn write_skipped_artifact(output_dir: &Path, step: &PipelineStep, reason: &str) {
    let _ = fs::create_dir_all(output_dir);
    let output_path = output_dir.join(format!("{}.md", step.name));
    if output_path.exists() {
        return;
    }
    let now = Utc::now().to_rfc3339();
    let reason_escaped = reason.replace('"', "\\\"");
    let connector_label = format!("{:?}", step.step_type).to_lowercase();
    let content = format!(
        "---\nname: {}\ndescription: \"{}\"\nconnector: {}\nstatus: skipped\ncreated_at: {}\ncompleted_at: {}\nerror: \"{}\"\n---\n\n## Skipped\n{}\n",
        step.name,
        step.description
            .as_deref()
            .unwrap_or("")
            .replace('"', "\\\""),
        connector_label,
        now,
        now,
        reason_escaped,
        reason,
    );
    let _ = fs::write(&output_path, content);
}

/// Execute a pipeline for a recording.
///
/// Flow:
///   1. Load transcript text + app friendly name into memory.
///   2. For each step: render `{transcript}` / `{app}` / `{processing_result}`
///      into `step.template`, dispatch to the step's connector (CLI agent /
///      Shell), read the artifact body back as the next step's
///      `{processing_result}`.
///   3. The chain is strictly linear — any step failure halts downstream
///      (later steps can't run without the prior step's output) and marks the
///      run Partial.
pub async fn execute_pipeline_internal(
    recording_id: &str,
    pipeline_name: &str,
    app_handle: Option<&tauri::AppHandle>,
) -> Result<PipelineStatus, String> {
    let key = format!("{}:{}", recording_id, pipeline_name);

    // Serialize execution: if the same pipeline is already running on this recording, wait.
    let lock = {
        // Tolerate poisoning — a panic elsewhere shouldn't permanently wedge
        // every future pipeline run behind a dead lock.
        let mut locks = PIPELINE_EXEC_LOCKS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    };
    let _map_guard = LockMapGuard { key };
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

    // Zero-step pipeline = label only; skip execution, return Done immediately.
    if pipeline.steps.is_empty() {
        update_pipeline_state(
            recording_id,
            pipeline_name,
            PipelineStatus::Done,
            None,
            None,
        )?;
        return Ok(PipelineStatus::Done);
    }

    // Load transcript into memory — fail fast if absent.
    let transcript = load_transcript_text(recording_id)?;
    let app_name = resolve_app_name(recording_id);
    // Recording start time (RFC3339) — feeds the save connector's filename —
    // plus the calendar-match placeholders, all from the one metadata read.
    let meta = crate::storage::read_metadata(recording_id).ok();
    let started_at = meta
        .as_ref()
        .map(|m| m.created_at.clone())
        .unwrap_or_default();
    let calendar_title = meta
        .as_ref()
        .filter(|m| m.calendar_matched)
        .map(|m| m.title.clone())
        .unwrap_or_default();
    let calendar_attendees = meta.as_ref().map(format_attendees).unwrap_or_default();
    let date = format_date(&started_at);

    // Resolve run_index from the active (waiting/running) pipeline state.
    let run_index = {
        let states = read_pipeline_states(recording_id);
        states
            .iter()
            .filter(|s| {
                s.name == pipeline_name
                    && s.status != PipelineStatus::Done
                    && s.status != PipelineStatus::Partial
            })
            .map(|s| s.run_index)
            .next_back()
            .unwrap_or(0)
    };

    update_pipeline_state(
        recording_id,
        pipeline_name,
        PipelineStatus::Running,
        None,
        None,
    )?;

    let output_dir = get_pipeline_output_dir(recording_id, pipeline_name, run_index);
    let total_steps = pipeline.steps.len();

    // Shared step context — the same one dictation builds, so both drive the
    // identical dispatch via `run_one_step`.
    let ctx = StepContext {
        transcript: &transcript,
        app: &app_name,
        started_at: &started_at,
        output_dir: &output_dir,
        calendar_title: &calendar_title,
        calendar_attendees: &calendar_attendees,
        date: &date,
    };

    // Threaded state across the step loop.
    let mut prev_processing_output = String::new();
    let mut failed_or_skipped: HashSet<String> = HashSet::new();
    let mut has_failure = false;
    let mut first_error: Option<String> = None;

    for (i, step) in pipeline.steps.iter().enumerate() {
        // Emit running event before any work so the UI flips the row state.
        emit_progress(
            app_handle,
            recording_id,
            pipeline_name,
            step,
            i,
            total_steps,
            "running",
        );
        update_pipeline_state(
            recording_id,
            pipeline_name,
            PipelineStatus::Running,
            Some(i),
            None,
        )?;

        match run_one_step(step, &prev_processing_output, &ctx).await {
            Ok(body) => {
                emit_progress(
                    app_handle,
                    recording_id,
                    pipeline_name,
                    step,
                    i,
                    total_steps,
                    "done",
                );
                // Every step feeds the next — output becomes {processing_result}.
                prev_processing_output = body;
            }
            Err(err) => {
                handle_step_failure(
                    &err,
                    step,
                    i,
                    &pipeline.steps,
                    &mut first_error,
                    &mut has_failure,
                    &mut failed_or_skipped,
                    app_handle,
                    recording_id,
                    pipeline_name,
                    total_steps,
                );
                // A failed step's output can't feed the chain — halt downstream.
                break;
            }
        }
    }

    // Write skipped artifacts for any steps the engine marked but never visited
    // (the loop `break`ed past them on processing failure).
    for step in &pipeline.steps {
        if failed_or_skipped.contains(&step.name) {
            write_skipped_artifact(
                &output_dir,
                step,
                "Skipped because a preceding processing step failed",
            );
        }
    }

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

    // Auto-title: if recording still has default title, extract one from the first
    // processing step's output that succeeded.
    if (final_status == PipelineStatus::Done || final_status == PipelineStatus::Partial)
        && let Ok(meta) = crate::storage::read_metadata(recording_id)
    {
        let needs_title = meta.title.is_empty()
            || meta.title == "Untitled Recording"
            || meta.title.starts_with("Recording ");
        if needs_title
            && let Some(step) = pipeline
                .steps
                .iter()
                .find(|s| !failed_or_skipped.contains(&s.name))
        {
            let step_output = output_dir.join(format!("{}.md", step.name));
            if let Ok(content) = fs::read_to_string(&step_output)
                && let Some(title) = extract_title_from_output(&content)
            {
                let _ = crate::storage::update_title(recording_id, title);
            }
        }
    }

    Ok(final_status)
}

/// Common bookkeeping for a step that just failed: record the error, mark this
/// step and every downstream step as skipped (the linear chain can't continue
/// without this step's output), emit the UI event.
#[allow(clippy::too_many_arguments)]
fn handle_step_failure(
    error: &str,
    step: &PipelineStep,
    step_index: usize,
    all_steps: &[PipelineStep],
    first_error: &mut Option<String>,
    has_failure: &mut bool,
    failed_or_skipped: &mut HashSet<String>,
    app_handle: Option<&tauri::AppHandle>,
    recording_id: &str,
    pipeline_name: &str,
    total_steps: usize,
) {
    if first_error.is_none() {
        *first_error = Some(error.to_string());
    }
    *has_failure = true;
    failed_or_skipped.insert(step.name.clone());

    // Linear chain: a failed step's output can't feed the next, so everything
    // after it is unreachable — mark all downstream steps skipped.
    for remaining in all_steps.iter().skip(step_index + 1) {
        failed_or_skipped.insert(remaining.name.clone());
    }

    emit_progress(
        app_handle,
        recording_id,
        pipeline_name,
        step,
        step_index,
        total_steps,
        "failed",
    );
}

fn emit_progress(
    app_handle: Option<&tauri::AppHandle>,
    recording_id: &str,
    pipeline_name: &str,
    step: &PipelineStep,
    step_index: usize,
    total_steps: usize,
    status: &str,
) {
    if let Some(app) = app_handle {
        use tauri::Emitter;
        let _ = app.emit(
            "pipeline-progress",
            PipelineProgressPayload {
                recording_id: recording_id.to_string(),
                pipeline_name: pipeline_name.to_string(),
                step_name: step.name.clone(),
                step_index,
                total_steps,
                status: status.to_string(),
            },
        );
    }
}

/// Acquire an exclusive file lock using platform-appropriate mechanism.
/// On macOS/Unix, uses flock(2). Returns a guard that releases the lock on drop.
struct FileLockGuard {
    _lock_file: fs::File,
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
        })
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Lock is released when _lock_file is dropped (fd closed).
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
    let mut json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse metadata: {}", e))?;

    // Extract existing pipeline states
    let mut pipeline_states: Vec<PipelineState> = json
        .get("pipelines")
        .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
        .unwrap_or_default();

    // Find the active (non-completed) entry for this pipeline, or create a new one.
    // Search from the end so we target the most recently assigned run when multiple
    // active entries exist (e.g. a previous run still Running when a new run starts).
    if let Some(state) = pipeline_states.iter_mut().rev().find(|s| {
        s.name == pipeline_name
            && s.status != PipelineStatus::Done
            && s.status != PipelineStatus::Partial
    }) {
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
    fs::write(&temp_path, &updated).map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;

    // This rewrites metadata.json outside storage::write_metadata, so the
    // list_recordings cache won't see the new pipeline state otherwise.
    crate::storage::invalidate_list_cache();
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

    let Ok(content) = fs::read_to_string(&metadata_path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    let Some(pipelines) = json.get("pipelines") else {
        return Vec::new();
    };

    // Backfill missing `id` fields so serde default doesn't generate a new UUID each read
    if let Some(arr) = pipelines.as_array() {
        let needs_migration = arr.iter().any(|e| {
            e.get("id")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.is_empty())
        });
        if needs_migration {
            // Acquire lock BEFORE re-reading to avoid overwriting concurrent changes
            let lock_path = recording_dir.join(".metadata.lock");
            if let Ok(_lock) = FileLockGuard::acquire(&lock_path) {
                // Re-read file under lock to get the latest state
                if let Ok(locked_content) = fs::read_to_string(&metadata_path)
                    && let Ok(mut locked_json) =
                        serde_json::from_str::<serde_json::Value>(&locked_content)
                    && let Some(locked_arr) =
                        locked_json.get("pipelines").and_then(|v| v.as_array())
                {
                    let mut migrated = locked_arr.clone();
                    for entry in &mut migrated {
                        let missing = entry
                            .get("id")
                            .and_then(|v| v.as_str())
                            .is_none_or(|s| s.is_empty());
                        if missing && let Some(obj) = entry.as_object_mut() {
                            obj.insert(
                                "id".to_string(),
                                serde_json::Value::String(uuid::Uuid::new_v4().to_string()),
                            );
                        }
                    }
                    locked_json["pipelines"] = serde_json::Value::Array(migrated);
                    if let Ok(updated) = serde_json::to_string_pretty(&locked_json) {
                        let temp_path = metadata_path.with_extension("json.tmp");
                        if fs::write(&temp_path, &updated).is_ok()
                            && fs::rename(&temp_path, &metadata_path).is_ok()
                        {
                            crate::storage::invalidate_list_cache();
                        }
                    }
                    // Return states from the locked read (authoritative)
                    return locked_json
                        .get("pipelines")
                        .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
                        .unwrap_or_default();
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
            statuses.push(StepStatus {
                name: step.name.clone(),
                status,
                error,
                duration_secs,
                // augmented_prompt was an N+1 look-ahead artifact (LLM step
                // composing a Notion/Linear schema spec). Gone in the new model
                // — users put any schema hint in their template directly.
                augmented_prompt: None,
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
        let output = if body.is_empty() {
            None
        } else {
            Some(body.to_string())
        };

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
    let mut json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse metadata: {}", e))?;

    // Work with raw JSON array to avoid serde default generating new random UUIDs
    // for legacy entries without persisted `id` fields (which would never match run_id)
    let pipelines_arr = json
        .get("pipelines")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "No pipelines array in metadata".to_string())?;

    let pos = pipelines_arr
        .iter()
        .position(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(run_id.as_str()));

    let idx = match pos {
        Some(i) => i,
        None => return Err(format!("Pipeline run '{}' not found", run_id)),
    };

    // Extract name and run_index for output dir cleanup before removing
    let removed = &pipelines_arr[idx];
    let removed_name = removed
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let removed_run_index = removed
        .get("run_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let mut arr = pipelines_arr.clone();
    arr.remove(idx);
    json["pipelines"] = serde_json::Value::Array(arr);

    let temp_path = metadata_path.with_extension("json.tmp");
    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
    fs::write(&temp_path, &updated).map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;
    crate::storage::invalidate_list_cache();

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
    let mut json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse metadata: {}", e))?;

    let mut pipeline_states: Vec<PipelineState> = json
        .get("pipelines")
        .and_then(|v| serde_json::from_value::<Vec<PipelineState>>(v.clone()).ok())
        .unwrap_or_default();

    // Count existing runs of this pipeline to determine run_index
    let run_index = pipeline_states
        .iter()
        .filter(|s| s.name == pipeline_name)
        .count();

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
    fs::write(&temp_path, &updated).map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;
    crate::storage::invalidate_list_cache();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_template_all_three_placeholders() {
        let out = render_template(
            "Hi {app}, transcript is:\n{transcript}\n\nPrev: {processing_result}",
            "hello world",
            "Zoom",
            "summary v1",
            "",
            "",
            "",
        );
        assert!(out.contains("Hi Zoom"));
        assert!(out.contains("hello world"));
        assert!(out.contains("Prev: summary v1"));
    }

    #[test]
    fn test_render_template_missing_keys_render_as_empty() {
        // No-placeholder template — passes through verbatim.
        assert_eq!(
            render_template("static text", "T", "A", "P", "CT", "CA", "D"),
            "static text"
        );
    }

    #[test]
    fn test_render_template_empty_processing_result_for_first_step() {
        let out = render_template("[{processing_result}]", "T", "A", "", "", "", "");
        assert_eq!(out, "[]");
    }

    #[test]
    fn test_render_template_calendar_and_date_placeholders() {
        let out = render_template(
            "{calendar_title} on {date}\nWith: {calendar_attendees}\n\n{transcript}",
            "the words",
            "Zoom",
            "",
            "Q3 Planning",
            "Alice, Bob",
            "2026-06-03",
        );
        assert!(out.contains("Q3 Planning on 2026-06-03"));
        assert!(out.contains("With: Alice, Bob"));
        assert!(out.contains("the words"));
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
        let content = "---\nname: test\nstatus: failed\nerror: \"API error: 401\"\n---\n\nFailed";
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
    fn test_pipeline_output_dir_isolation() {
        let dir_a = get_pipeline_output_dir("rec-1", "pipeline-a", 0);
        let dir_b = get_pipeline_output_dir("rec-1", "pipeline-b", 0);
        assert_ne!(dir_a, dir_b);
        assert!(dir_a.to_string_lossy().contains("pipelines/pipeline-a"));
        assert!(dir_b.to_string_lossy().contains("pipelines/pipeline-b"));
    }
}
