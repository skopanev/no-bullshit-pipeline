use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::process::Command as AsyncCommand;

pub const SUPPORTED_CLIS: &[(&str, &str, &str)] = &[
    ("claude", "Claude Code", "npm install -g @anthropic-ai/claude-code"),
    ("codex", "Codex CLI", "npm install -g @openai/codex"),
    ("opencode", "OpenCode", "npm install -g opencode-ai"),
];

pub const CLI_MODELS: &[(&str, &str, &str)] = &[
    ("claude", "sonnet", "Claude Sonnet (latest)"),
    ("claude", "opus", "Claude Opus (latest)"),
    ("claude", "haiku", "Claude Haiku (latest)"),
    ("claude", "claude-sonnet-4-20250514", "Claude Sonnet 4"),
    ("claude", "claude-opus-4-20250514", "Claude Opus 4"),
    ("codex", "o3", "O3"),
    ("codex", "o4-mini", "O4 Mini"),
    ("codex", "o3-mini", "O3 Mini"),
    ("opencode", "openai/gpt-4o", "GPT-4o"),
    ("opencode", "openai/gpt-4o-mini", "GPT-4o Mini"),
    ("opencode", "openai/o3-mini", "O3 Mini"),
    ("opencode", "anthropic/claude-sonnet-4-20250514", "Claude Sonnet 4"),
    ("opencode", "anthropic/claude-opus-4-20250514", "Claude Opus 4"),
    ("opencode", "anthropic/claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet"),
    ("opencode", "google/gemini-2.5-pro-preview-06-05", "Gemini 2.5 Pro"),
    ("opencode", "google/gemini-2.0-flash", "Gemini 2.0 Flash"),
];

pub const OPENCODE_PROVIDERS: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("anthropic", "Anthropic"),
    ("google", "Google"),
    ("opencode", "OpenCode (free)"),
    ("zai-coding-plan", "ZAI Coding Plan"),
];

#[derive(serde::Serialize, Clone)]
pub struct CliModelInfo {
    pub id: String,
    pub name: String,
}

#[derive(serde::Serialize, Clone)]
pub struct CliProviderInfo {
    pub id: String,
    pub name: String,
}

#[derive(serde::Serialize, Clone)]
pub struct CliInfo {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub install_hint: String,
    pub models: Vec<CliModelInfo>,
    pub providers: Vec<CliProviderInfo>,
}

#[tauri::command]
pub fn check_cli_availability() -> Vec<CliInfo> {
    SUPPORTED_CLIS.iter().map(|(id, name, hint)| {
        let installed = check_cli_installed(id);
        let models: Vec<CliModelInfo> = CLI_MODELS
            .iter()
            .filter(|(cli_id, _, _)| *cli_id == *id)
            .map(|(_, model_id, model_name)| CliModelInfo {
                id: model_id.to_string(),
                name: model_name.to_string(),
            })
            .collect();
        let providers: Vec<CliProviderInfo> = if *id == "opencode" {
            OPENCODE_PROVIDERS
                .iter()
                .map(|(prov_id, prov_name)| CliProviderInfo {
                    id: prov_id.to_string(),
                    name: prov_name.to_string(),
                })
                .collect()
        } else {
            vec![]
        };
        CliInfo {
            id: id.to_string(),
            name: name.to_string(),
            installed,
            install_hint: hint.to_string(),
            models,
            providers,
        }
    }).collect()
}

pub fn check_cli_installed(cli: &str) -> bool {
    Command::new("which")
        .arg(cli)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Execute a CLI agent (Claude Code, Codex, or OpenCode) as a pipeline step.
///
/// Config fields:
///   cli             - "claude", "codex", or "opencode" (required)
///   prompt          - Prompt text sent along with the input (required)
///   working_directory - Working directory for the subprocess (optional, default: home dir)
///   timeout_secs    - Timeout in seconds (optional, default: 300)
///   model_mode      - "default" or "advanced" (optional, default: "default")
///   model           - Model ID for default mode (optional)
///   provider        - Provider ID for OpenCode provider-model selection (optional)
///   model_args      - Custom args for advanced mode (optional)
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
        .ok_or("CLI agent config missing 'cli' (must be 'claude', 'codex', or 'opencode')")?;

    let valid_clis: Vec<&str> = SUPPORTED_CLIS.iter().map(|(id, _, _)| *id).collect();
    if !valid_clis.contains(&cli) {
        return Err(format!(
            "CLI agent 'cli' must be 'claude', 'codex', or 'opencode', got '{}'",
            cli
        ));
    }

    // Check if CLI is installed
    if !check_cli_installed(cli) {
        let cli_info = SUPPORTED_CLIS.iter().find(|(id, _, _)| *id == cli);
        let install_hint = cli_info.map(|(_, _, hint)| *hint).unwrap_or("npm install -g <cli>");
        return Err(format!(
            "CLI '{}' is not installed or not in PATH. Install it first: {}",
            cli, install_hint
        ));
    }

    let prompt = if let Some(template_name) = config
        .get("prompt_template")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let template = crate::prompt_templates::get_prompt_template_internal(template_name)?;
        template.prompt.clone()
    } else if let Some(inline) = config
        .get("prompt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        inline.to_string()
    } else {
        return Err("CLI agent config missing 'prompt' or 'prompt_template'".to_string());
    };

    let timeout_secs = config
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(300);

    let model_mode = config
        .get("model_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let model = config.get("model").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    let provider = config.get("provider").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    let model_args = config.get("model_args").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let working_directory = config
        .get("working_directory")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|p| expand_tilde(p, &home))
        .unwrap_or_else(|| PathBuf::from(&home));

    // Read input content
    let raw_content = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read input file: {}", e))?;
    let content = super::strip_frontmatter(&raw_content);

    // Substitute {transcript} variable, or append content if no placeholder
    let full_prompt = if prompt.contains("{transcript}") {
        crate::prompt_templates::substitute_variables(&prompt, content)
    } else {
        format!("{}\n\n{}", prompt, content)
    };

    let mut cmd = match cli {
        "claude" => {
            let mut c = AsyncCommand::new("claude");
            c.arg("-p").arg(&full_prompt);
            if model_mode == "advanced" {
                if let Some(args) = model_args {
                    for arg in shell_words::split(args).map_err(|e| format!("Invalid model_args: {}", e))? {
                        c.arg(arg);
                    }
                }
            } else if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            c
        }
        "codex" => {
            let mut c = AsyncCommand::new("codex");
            c.arg("exec").arg(&full_prompt);
            if model_mode == "advanced" {
                if let Some(args) = model_args {
                    for arg in shell_words::split(args).map_err(|e| format!("Invalid model_args: {}", e))? {
                        c.arg(arg);
                    }
                }
            } else if let Some(m) = model {
                c.arg("-m").arg(m);
            }
            c
        }
        "opencode" => {
            let mut c = AsyncCommand::new("opencode");
            c.arg("run").arg(&full_prompt);
            if model_mode == "advanced" {
                if let Some(args) = model_args {
                    for arg in shell_words::split(args).map_err(|e| format!("Invalid model_args: {}", e))? {
                        c.arg(arg);
                    }
                }
            } else if let Some(m) = model {
                let model_value = if m.contains('/') {
                    m.to_string()
                } else if let Some(p) = provider {
                    format!("{}/{}", p, m)
                } else {
                    m.to_string()
                };
                c.arg("-m").arg(&model_value);
            }
            c
        }
        _ => unreachable!(),
    };

    cmd.current_dir(&working_directory);
    // Prevent interactive prompts
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

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
    let wait_result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        child.wait(),
    )
    .await;

    let status = match wait_result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            return write_error(
                output_dir, step_name, step_input, step_description,
                &created_at, cli, &format!("Process I/O error: {}", e),
            );
        }
        Err(_timeout) => {
            // Timeout expired — kill the subprocess and reap it
            let _ = child.kill().await;
            let _ = child.wait().await;
            return write_error(
                output_dir, step_name, step_input, step_description,
                &created_at, cli,
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
            output_dir, step_name, step_input, step_description,
            &created_at, cli,
            &format!("CLI agent '{}' failed: {}", cli, details.trim()),
        );
    }

    let agent_output = String::from_utf8_lossy(&stdout_bytes).to_string();

    // Write success output
    fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create output dir: {}", e))?;

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
    fs::write(&temp_path, &file_content)
        .map_err(|e| format!("Failed to write output: {}", e))?;
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
        step_name,
        desc_escaped,
        step_input,
        created_at,
        completed_at,
        err_escaped,
        cli,
        error,
    );
    let temp_path = output_dir.join(format!(".{}.md.tmp", step_name));
    if let Err(e) = fs::write(&temp_path, &file_content) {
        eprintln!("[cli_agent] Failed to write error state for step '{}': {}", step_name, e);
    } else if let Err(e) = fs::rename(&temp_path, &output_path) {
        eprintln!("[cli_agent] Failed to finalize error state for step '{}': {}", step_name, e);
    }
    Err(error.to_string())
}

/// Process text with a CLI agent and return the output as a string.
/// Used by summarize_recording / process_with_template when CliAgent provider is selected.
pub async fn process_with_cli(
    cli: &str,
    prompt: &str,
    transcript: &str,
    model: Option<&str>,
    timeout_secs: u64,
) -> Result<String, String> {
    if !check_cli_installed(cli) {
        let cli_info = SUPPORTED_CLIS.iter().find(|(id, _, _)| *id == cli);
        let install_hint = cli_info.map(|(_, _, hint)| *hint).unwrap_or("npm install -g <cli>");
        return Err(format!(
            "CLI '{}' is not installed. Install: {}",
            cli, install_hint
        ));
    }

    let full_prompt = format!("{}\n\n{}", prompt, transcript);
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    let mut cmd = match cli {
        "claude" => {
            let mut c = AsyncCommand::new("claude");
            c.arg("-p").arg(&full_prompt);
            if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            c
        }
        "codex" => {
            let mut c = AsyncCommand::new("codex");
            c.arg("exec").arg(&full_prompt);
            if let Some(m) = model {
                c.arg("-m").arg(m);
            }
            c
        }
        "opencode" => {
            let mut c = AsyncCommand::new("opencode");
            c.arg("run").arg(&full_prompt);
            if let Some(m) = model {
                c.arg("-m").arg(m);
            }
            c
        }
        _ => {
            return Err(format!("Unsupported CLI: '{}'. Use 'claude', 'codex', or 'opencode'", cli));
        }
    };

    cmd.current_dir(&home);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        format!("Failed to spawn '{}': {} (is it installed and in PATH?)", cli, e)
    })?;

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let wait_result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        child.wait(),
    )
    .await;

    let status = match wait_result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(format!("CLI process error: {}", e)),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(format!("CLI agent '{}' timed out after {}s", cli, timeout_secs));
        }
    };

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
        return Err(format!("CLI '{}' failed: {}", cli, details.trim()));
    }

    Ok(String::from_utf8_lossy(&stdout_bytes).trim().to_string())
}
