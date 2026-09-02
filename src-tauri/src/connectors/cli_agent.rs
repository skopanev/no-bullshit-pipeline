use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::process::Command as AsyncCommand;

// CLIs we know how to invoke non-interactively. Each is `(id, friendly_name,
// install_hint)`. The id IS the binary name on PATH — `which <id>` is how
// we detect installation. Adding a new CLI = one row here + one match arm
// in `process_with_cli` below.
pub const SUPPORTED_CLIS: &[(&str, &str, &str)] = &[
    (
        "claude",
        "Claude Code",
        "curl -fsSL https://claude.ai/install.sh | bash",
    ),
    ("codex", "Codex CLI", "npm install -g @openai/codex"),
    ("opencode", "OpenCode", "npm install -g opencode-ai"),
    (
        "agy",
        "Antigravity (Google)",
        "curl -fsSL https://antigravity.google/cli/install.sh | bash",
    ),
];

#[derive(serde::Serialize, Clone)]
pub struct CliInfo {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub install_hint: String,
}

#[tauri::command]
pub fn check_cli_availability() -> Vec<CliInfo> {
    SUPPORTED_CLIS
        .iter()
        .map(|(id, name, hint)| CliInfo {
            id: id.to_string(),
            name: name.to_string(),
            installed: check_cli_installed(id),
            install_hint: hint.to_string(),
        })
        .collect()
}

pub fn check_cli_installed(cli: &str) -> bool {
    Command::new("which")
        .arg(cli)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build the `tokio::process::Command` that runs `cli` non-interactively with
/// `full_prompt`. Adding a new CLI = one match arm here. Caller is
/// responsible for `current_dir`, stdio piping, kill_on_drop.
///
/// Invocation contract per CLI:
///   - `claude -p <prompt> [--model M]`     (Anthropic)
///   - `codex  exec <prompt> [-m M]`        (OpenAI)
///   - `opencode run <prompt> [-m M]`       (model = `provider/id`, e.g. `openai/gpt-4o`)
///   - `agy    -p <prompt> [-m M]`          (Google Antigravity — same flags as Claude)
///
/// Caller assumes the CLI is already verified installed via `check_cli_installed`.
fn build_cli_command(cli: &str, full_prompt: &str, model: Option<&str>) -> AsyncCommand {
    match cli {
        "claude" => {
            let mut c = AsyncCommand::new("claude");
            c.arg("-p").arg(full_prompt);
            if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            c
        }
        "codex" => {
            let mut c = AsyncCommand::new("codex");
            // --skip-git-repo-check: codex exec otherwise refuses to run
            // outside a git repo (the recording folder isn't one).
            c.arg("exec").arg("--skip-git-repo-check").arg(full_prompt);
            if let Some(m) = model {
                c.arg("-m").arg(m);
            }
            c
        }
        "opencode" => {
            let mut c = AsyncCommand::new("opencode");
            c.arg("run").arg(full_prompt);
            if let Some(m) = model {
                c.arg("-m").arg(m);
            }
            c
        }
        "agy" => {
            // Google Antigravity CLI — same `-p` / `-m` shape as Claude.
            // See https://antigravity.google/docs/cli-using
            let mut c = AsyncCommand::new("agy");
            c.arg("-p").arg(full_prompt);
            if let Some(m) = model {
                c.arg("-m").arg(m);
            }
            c
        }
        _ => {
            // Unreachable in normal flow — execute()/process_with_cli() both
            // validate `cli` against SUPPORTED_CLIS before reaching this point.
            // Use a sentinel that will fail fast if it ever does.
            log::error!("build_cli_command: unknown cli '{}'", cli);
            AsyncCommand::new(cli)
        }
    }
}

/// Resolve the final CLI prompt across the current engine contract and the
/// legacy connector contract. Current pipeline steps pass an already-rendered
/// prompt; legacy callers may still pass transcript content separately.
fn compose_full_prompt(prompt: &str, content: &str, prompt_complete: bool) -> String {
    if prompt_complete {
        prompt.to_string()
    } else if prompt.contains("{transcript}") {
        prompt.replace("{transcript}", content)
    } else {
        format!("{}\n\n{}", prompt, content)
    }
}

/// Execute a CLI agent as a pipeline step.
///
/// Config fields (simplified — the old `model_mode` / `model_args` / `provider`
/// surface was overkill for a 3-user personal tool and got cut):
///   cli           - one of `SUPPORTED_CLIS` ids (required)
///   prompt        - prompt text — for the new Connection model the engine
///                   substitutes placeholders into `step.template` and feeds
///                   the result here (required)
///   prompt_complete - internal engine flag: `prompt` is already the complete
///                   rendered input and must not be combined with the input
///                   file again
///   model         - model id string passed verbatim to the CLI's -m flag
///                   (optional; CLI default if omitted). Free-text — we don't
///                   maintain a list, the CLI validates at runtime.
///   timeout_secs  - kills the subprocess if exceeded (optional, default 3600)
///   working_directory - cwd for the subprocess (optional, default: $HOME)
pub async fn execute(
    input_path: &Path,
    config: &serde_json::Value,
    output_dir: &Path,
    step_name: &str,
    step_input: &str,
    step_description: Option<&str>,
) -> Result<PathBuf, String> {
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let cli = config
        .get("cli")
        .and_then(|v| v.as_str())
        .ok_or("CLI agent config missing 'cli'")?;

    let valid_clis: Vec<&str> = SUPPORTED_CLIS.iter().map(|(id, _, _)| *id).collect();
    if !valid_clis.contains(&cli) {
        return Err(format!(
            "CLI agent 'cli' must be one of {:?}, got '{}'",
            valid_clis, cli
        ));
    }

    if !check_cli_installed(cli) {
        let install_hint = SUPPORTED_CLIS
            .iter()
            .find(|(id, _, _)| *id == cli)
            .map(|(_, _, hint)| *hint)
            .unwrap_or("see project docs");
        return Err(format!(
            "CLI '{}' is not installed or not in PATH. Install: {}",
            cli, install_hint
        ));
    }

    let prompt = config
        .get("prompt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or("CLI agent config missing 'prompt'")?
        .to_string();

    let timeout_secs = config
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    let model = config
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let working_directory = config
        .get("working_directory")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|p| expand_tilde(p, &home))
        .unwrap_or_else(|| PathBuf::from(&home));

    let prompt_complete = config
        .get("prompt_complete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let full_prompt = if prompt_complete {
        compose_full_prompt(&prompt, "", true)
    } else {
        // Legacy connector contract: the config carried a prompt/template and
        // the input file carried the transcript. Keep supporting it for old
        // callers, but the shared pipeline engine always uses prompt_complete.
        let raw_content = fs::read_to_string(input_path)
            .map_err(|e| format!("Failed to read input file: {}", e))?;
        let content = super::strip_frontmatter(&raw_content);
        compose_full_prompt(&prompt, content, false)
    };

    let mut cmd = build_cli_command(cli, &full_prompt, model);

    cmd.current_dir(&working_directory);
    // Prevent interactive prompts
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "Failed to spawn '{}': {} (is it installed and in PATH?)",
            cli, e
        )
    })?;

    let pid = child.id();

    // Take stdout/stderr handles before waiting (wait_with_output takes ownership)
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    // Enforce timeout: wait with timeout, then kill if exceeded
    let wait_result = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;

    let status = match wait_result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            return write_error(
                output_dir,
                step_name,
                step_input,
                step_description,
                &created_at,
                cli,
                &format!("Process I/O error: {}", e),
            );
        }
        Err(_timeout) => {
            // Timeout expired — kill the subprocess and reap it
            let _ = child.kill().await;
            let _ = child.wait().await;
            return write_error(
                output_dir,
                step_name,
                step_input,
                step_description,
                &created_at,
                cli,
                &format!(
                    "CLI agent timed out after {}s (pid: {:?})",
                    timeout_secs, pid
                ),
            );
        }
    };

    // Read captured output
    let stdout_bytes = if let Some(mut h) = stdout_handle {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let _ = h.read_to_end(&mut buf).await;
        buf
    } else {
        Vec::new()
    };
    let stderr_bytes = if let Some(mut h) = stderr_handle {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let _ = h.read_to_end(&mut buf).await;
        buf
    } else {
        Vec::new()
    };

    let completed_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let details = if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!("exit code: {:?}", status.code())
        };
        return write_error(
            output_dir,
            step_name,
            step_input,
            step_description,
            &created_at,
            cli,
            &format!("CLI agent '{}' failed: {}", cli, details.trim()),
        );
    }

    let agent_output = String::from_utf8_lossy(&stdout_bytes).to_string();

    // Write success output
    fs::create_dir_all(output_dir).map_err(|e| format!("Failed to create output dir: {}", e))?;

    let output_path = output_dir.join(format!("{}.md", step_name));
    let desc_escaped = step_description.unwrap_or("").replace('"', "\\\"");
    let file_content = format!(
        "---\nname: {}\ndescription: \"{}\"\nconnector: cli_agent\ninput: {}\nstatus: done\ncreated_at: {}\ncompleted_at: {}\nerror: null\ncli: {}\n---\n\n{}\n",
        step_name,
        desc_escaped,
        step_input,
        created_at,
        completed_at,
        cli,
        agent_output.trim(),
    );

    let temp_path = output_dir.join(format!(".{}.md.tmp", step_name));
    fs::write(&temp_path, &file_content).map_err(|e| format!("Failed to write output: {}", e))?;
    fs::rename(&temp_path, &output_path)
        .map_err(|e| format!("Failed to finalize output: {}", e))?;

    Ok(output_path)
}

/// Expand `~` to the user's home directory.
fn expand_tilde(path: &str, home: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else if path == "~" {
        PathBuf::from(home)
    } else {
        PathBuf::from(path)
    }
}

/// Write a failure output .md file and return Err.
fn write_error(
    output_dir: &Path,
    step_name: &str,
    step_input: &str,
    step_description: Option<&str>,
    created_at: &str,
    cli: &str,
    error: &str,
) -> Result<PathBuf, String> {
    let completed_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let _ = fs::create_dir_all(output_dir);
    let output_path = output_dir.join(format!("{}.md", step_name));
    let desc_escaped = step_description.unwrap_or("").replace('"', "\\\"");
    let err_escaped = error.replace('"', "\\\"").replace('\n', " ");
    let file_content = format!(
        "---\nname: {}\ndescription: \"{}\"\nconnector: cli_agent\ninput: {}\nstatus: failed\ncreated_at: {}\ncompleted_at: {}\nerror: \"{}\"\ncli: {}\n---\n\n## Error\n{}\n",
        step_name, desc_escaped, step_input, created_at, completed_at, err_escaped, cli, error,
    );
    let temp_path = output_dir.join(format!(".{}.md.tmp", step_name));
    if let Err(e) = fs::write(&temp_path, &file_content) {
        eprintln!(
            "[cli_agent] Failed to write error state for step '{}': {}",
            step_name, e
        );
    } else if let Err(e) = fs::rename(&temp_path, &output_path) {
        eprintln!(
            "[cli_agent] Failed to finalize error state for step '{}': {}",
            step_name, e
        );
    }
    Err(error.to_string())
}

// `execute_inline` / `process_with_cli` were the in-memory CLI path Quick
// Dictate used to call directly. Dictation now routes through the shared
// `pipeline_engine::run_one_step` (same dispatch as recordings), so they're
// gone — no second CLI-spawning path to drift from `execute()`.

#[cfg(test)]
mod tests {
    use super::compose_full_prompt;

    #[test]
    fn complete_rendered_prompt_is_not_appended_twice() {
        let prompt = "Structure this:\nA short but real thought.";
        let full = compose_full_prompt(prompt, prompt, true);

        assert_eq!(full, prompt);
        assert_eq!(full.matches("A short but real thought.").count(), 1);
    }

    #[test]
    fn legacy_transcript_placeholder_is_still_supported() {
        let full = compose_full_prompt("Structure this:\n{transcript}", "Raw thought", false);
        assert_eq!(full, "Structure this:\nRaw thought");
    }
}
