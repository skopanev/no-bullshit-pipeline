//! ASR model version tracking for the ACTIVE on-device engine only.
//!
//! Boundary: the sidecar (which embeds FluidAudio) owns "is it cached" via
//! FluidAudio's public `modelsExist`. We only: ask HuggingFace for the repo's
//! current commit sha, store per engine/variant in `~/.nbp/asr-models.json`,
//! and derive a state. FluidAudio still owns the actual download.
//!
//! Version signal = HF repo HEAD commit `sha` (these repos have no git tags;
//! `main` HEAD is what FluidAudio pulls). Coarse — a README-only commit can
//! shift the sha; acceptable for a personal tool (user sees the offer and can
//! dismiss). Per-file fingerprinting is a future refinement.

use crate::config::{TranscriptionProvider, get_config_dir, load_settings};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::Emitter;
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::CommandEvent;

/// Model updates are process-global: only one sidecar may own the stable
/// staging directory at a time, regardless of which UI button triggered it.
static DOWNLOAD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Persisted per "engine:variant" version state (`~/.nbp/asr-models.json`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct ModelVersionEntry {
    /// Commit sha believed to be installed. `inferred` when we assumed it
    /// (legacy cache downloaded before tracking) rather than recorded at download.
    installed_sha: Option<String>,
    inferred: bool,
    latest_sha: Option<String>,
    /// Unix seconds of last successful HF check.
    checked_at: Option<i64>,
    /// The latest_sha the user dismissed in the update bar. We don't re-nag for
    /// this exact version; a newer sha clears the suppression naturally.
    dismissed_sha: Option<String>,
    /// Real card facts from the last HF check (cached so we respect the throttle).
    info: Option<ModelInfo>,
}

type VersionStore = HashMap<String, ModelVersionEntry>;

fn store_path() -> PathBuf {
    get_config_dir().join("asr-models.json")
}

fn load_store() -> VersionStore {
    fs::read_to_string(store_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn save_store(store: &VersionStore) {
    let dir = get_config_dir();
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(store) {
        let _ = fs::write(store_path(), json);
    }
}

/// The active ASR engine + variant from settings, or None for engines outside
/// the version system (Apple Speech = macOS-managed; cloud).
fn active_engine() -> Option<(String, String)> {
    let s = load_settings();
    match s.transcription.provider {
        TranscriptionProvider::FluidAudio => Some(("parakeet-v3".into(), "f32".into())),
        TranscriptionProvider::Qwen3 => Some(("qwen3".into(), s.transcription.qwen3_variant)),
        _ => None,
    }
}

/// Map of engine key → HF repo id for every managed on-device engine, sourced
/// from FluidAudio's `Repo` enum via the sidecar. The repo id is a compile-time
/// constant — available before any model is downloaded — so the UI can label the
/// model picker with real names from first launch.
#[tauri::command]
pub async fn list_asr_models(
    app: tauri::AppHandle,
) -> Result<std::collections::HashMap<String, String>, String> {
    let output = app
        .shell()
        .sidecar("fluidaudio-sidecar")
        .map_err(|e| format!("sidecar command: {}", e))?
        .args(["--list-models"])
        .output()
        .await
        .map_err(|e| format!("sidecar --list-models failed: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .map_err(|e| format!("parse --list-models: {} (raw: {})", e, stdout.trim()))
}

/// Ask the sidecar whether the active engine/variant is cached, its HF repo id,
/// and (for models installed by the atomic updater) the revision marker that
/// moved into place with the model directory.
async fn sidecar_status(
    app: &tauri::AppHandle,
    engine: &str,
    variant: &str,
) -> Result<(bool, String, Option<String>), String> {
    let output = app
        .shell()
        .sidecar("fluidaudio-sidecar")
        .map_err(|e| format!("sidecar command: {}", e))?
        .args(["--status", "--engine", engine, "--variant", variant])
        .output()
        .await
        .map_err(|e| format!("sidecar --status failed: {}", e))?;

    #[derive(Deserialize)]
    struct StatusOut {
        repo: String,
        cached: bool,
        #[serde(default)]
        installed_sha: Option<String>,
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: StatusOut = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("parse --status: {} (raw: {})", e, stdout.trim()))?;
    Ok((parsed.cached, parsed.repo, parsed.installed_sha))
}

/// Real facts pulled straight from the HF model card — no embellishment.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ModelInfo {
    pub languages: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// `lastModified` truncated to YYYY-MM-DD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloads: Option<u64>,
}

/// Query HuggingFace for the repo's current main-HEAD sha + real card facts.
async fn fetch_repo_meta(repo: &str) -> Result<(String, ModelInfo), String> {
    let url = format!("https://huggingface.co/api/models/{}", repo);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "nbp-asr-version-check")
        .send()
        .await
        .map_err(|e| format!("HF request failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HF returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("HF json: {}", e))?;
    let sha = json
        .get("sha")
        .and_then(|v| v.as_str())
        .ok_or("HF response missing 'sha'")?
        .to_string();
    let card = json.get("cardData");
    let info = ModelInfo {
        languages: card
            .and_then(|c| c.get("language"))
            .and_then(|v| v.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0),
        license: card
            .and_then(|c| c.get("license"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        last_modified: json
            .get("lastModified")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(10).collect()),
        downloads: json.get("downloads").and_then(|v| v.as_u64()),
    };
    Ok((sha, info))
}

/// Result returned to the frontend.
#[derive(Serialize, Clone, Debug)]
pub struct AsrModelState {
    pub engine: String,
    pub variant: String,
    pub repo: String,
    /// "not_downloaded" | "up_to_date" | "update_available" | "unmanaged" | "unknown"
    pub state: String,
    /// True when state is `update_available` but the user already dismissed THIS
    /// version — the bar should stay hidden (settings still shows the state).
    pub update_dismissed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<ModelInfo>,
}

fn unmanaged() -> AsrModelState {
    AsrModelState {
        engine: String::new(),
        variant: String::new(),
        repo: String::new(),
        state: "unmanaged".into(),
        update_dismissed: false,
        installed_sha: None,
        latest_sha: None,
        info: None,
    }
}

/// Resolve the version state for the ACTIVE engine.
///
/// `force` bypasses the once-a-day network throttle (used by manual "Check now"
/// and by the on-switch trigger). Offline/HF failures degrade gracefully:
/// state falls back to the last known data without flagging a false update.
#[tauri::command]
pub async fn get_asr_model_state(
    app: tauri::AppHandle,
    force: bool,
) -> Result<AsrModelState, String> {
    let Some((engine, variant)) = active_engine() else {
        return Ok(unmanaged());
    };

    let (cached, repo, installed_marker) = sidecar_status(&app, &engine, &variant).await?;
    let key = format!("{}:{}", engine, variant);

    if !cached {
        return Ok(AsrModelState {
            engine,
            variant,
            repo,
            state: "not_downloaded".into(),
            update_dismissed: false,
            installed_sha: None,
            latest_sha: None,
            info: None,
        });
    }

    let mut store = load_store();
    let mut entry = store.get(&key).cloned().unwrap_or_default();

    // The marker is part of the atomically installed directory, so it is more
    // authoritative than the separate JSON store. This also repairs a crash
    // between the sidecar swap and Rust persisting the successful update.
    if let Some(installed_sha) = installed_marker {
        entry.installed_sha = Some(installed_sha);
        entry.inferred = false;
    }

    // Throttle network checks to once per 24h unless forced.
    let now = chrono::Utc::now().timestamp();
    let stale = entry.checked_at.map(|t| now - t > 86_400).unwrap_or(true);

    if force || stale {
        match fetch_repo_meta(&repo).await {
            Ok((sha, info)) => {
                entry.latest_sha = Some(sha);
                entry.info = Some(info);
                entry.checked_at = Some(now);
            }
            Err(e) => {
                // Network/offline: keep prior data, don't invent an update.
                log::warn!("asr version check failed for {}: {}", repo, e);
            }
        }
    }

    // Legacy cache with no recorded install: assume current = installed to
    // avoid a false "update available" on first sight.
    if entry.installed_sha.is_none() {
        entry.installed_sha = entry.latest_sha.clone();
        entry.inferred = true;
    }

    let state = match (&entry.installed_sha, &entry.latest_sha) {
        (Some(i), Some(l)) if i != l => "update_available",
        (Some(_), Some(_)) => "up_to_date",
        _ => "unknown",
    }
    .to_string();

    let update_dismissed = state == "update_available"
        && entry.dismissed_sha.is_some()
        && entry.dismissed_sha == entry.latest_sha;

    let result = AsrModelState {
        engine,
        variant,
        repo,
        state,
        update_dismissed,
        installed_sha: entry.installed_sha.clone(),
        latest_sha: entry.latest_sha.clone(),
        info: entry.info.clone(),
    };

    store.insert(key, entry);
    save_store(&store);

    Ok(result)
}

/// Remember that the user dismissed the update bar for the active model's
/// current latest version, so we don't nag again until a newer sha appears.
#[tauri::command]
pub fn dismiss_asr_update() -> Result<(), String> {
    let Some((engine, variant)) = active_engine() else {
        return Ok(());
    };
    let key = format!("{}:{}", engine, variant);
    let mut store = load_store();
    let mut entry = store.get(&key).cloned().unwrap_or_default();
    entry.dismissed_sha = entry.latest_sha.clone();
    store.insert(key, entry);
    save_store(&store);
    Ok(())
}

#[derive(Clone, Serialize)]
struct AsrDownloadProgress {
    stage: String,
    percent: u32,
}

fn parse_download_progress(line: &str) -> Option<AsrDownloadProgress> {
    let rest = line.trim().strip_prefix("PROGRESS:")?;
    let (stage, percent) = rest.rsplit_once(':')?;
    Some(AsrDownloadProgress {
        stage: stage.to_string(),
        percent: percent.parse().ok()?,
    })
}

fn emit_download_progress(app: &tauri::AppHandle, line: &str) {
    if let Some(progress) = parse_download_progress(line) {
        let _ = app.emit("asr-download-progress", progress);
    }
}

/// Download (force=false) or update (force=true) the ACTIVE model via the
/// sidecar. The sidecar owns staging, full model validation, and the atomic
/// directory swap; Rust resolves the target revision and persists it only
/// after the sidecar reports success.
#[tauri::command]
pub async fn download_asr_model(app: tauri::AppHandle, force: bool) -> Result<(), String> {
    let _download_guard = DOWNLOAD_LOCK
        .try_lock()
        .map_err(|_| "A model download is already in progress".to_string())?;

    let Some((engine, variant)) = active_engine() else {
        return Err("Active engine is not a managed on-device model".into());
    };

    let (_, repo, _) = sidecar_status(&app, &engine, &variant).await?;
    let key = format!("{}:{}", engine, variant);

    // Resolve the revision before staging starts. Passing it to the sidecar
    // makes an interrupted stage safely resumable only for that same target.
    // If HF metadata is temporarily unavailable, fall back to the last known
    // revision; the actual download can still succeed through FluidAudio.
    let (target_sha, target_info) = match fetch_repo_meta(&repo).await {
        Ok((sha, info)) => {
            let mut store = load_store();
            let mut entry = store.get(&key).cloned().unwrap_or_default();
            entry.latest_sha = Some(sha.clone());
            entry.info = Some(info.clone());
            entry.checked_at = Some(chrono::Utc::now().timestamp());
            store.insert(key.clone(), entry);
            save_store(&store);
            (Some(sha), Some(info))
        }
        Err(error) => {
            log::warn!("asr update: could not resolve latest sha for {repo}: {error}");
            let entry = load_store().get(&key).cloned().unwrap_or_default();
            (entry.latest_sha, entry.info)
        }
    };

    let mut args: Vec<String> = vec![
        "--download".into(),
        "--engine".into(),
        engine.clone(),
        "--variant".into(),
        variant.clone(),
    ];
    if force {
        args.push("--force".into());
    }
    if let Some(sha) = &target_sha {
        args.push("--target-sha".into());
        args.push(sha.clone());
    }

    // `_child` is held for the lifetime of the event loop so the sidecar isn't
    // dropped mid-download. If the app is killed, the live model is untouched
    // and the SHA-marked stage can resume on the next attempt.
    let (mut rx, _child) = app
        .shell()
        .sidecar("fluidaudio-sidecar")
        .map_err(|e| format!("sidecar command: {}", e))?
        .args(args)
        .spawn()
        .map_err(|e| format!("spawn --download: {}", e))?;

    let mut exit_code: Option<i32> = None;
    let mut stderr_tail = String::new();
    let mut stderr_pending = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            CommandEvent::Stderr(d) => {
                let chunk = String::from_utf8_lossy(&d);
                stderr_tail.push_str(&chunk);
                stderr_pending.push_str(&chunk);
                while let Some(newline) = stderr_pending.find('\n') {
                    let line = stderr_pending[..newline].trim_end_matches('\r').to_string();
                    stderr_pending.drain(..=newline);
                    emit_download_progress(&app, &line);
                }
            }
            CommandEvent::Terminated(payload) => {
                exit_code = payload.code;
                break;
            }
            _ => {}
        }
    }
    if !stderr_pending.is_empty() {
        emit_download_progress(&app, &stderr_pending);
    }

    if exit_code != Some(0) {
        return Err(format!(
            "Model download failed (exit {:?}): {}",
            exit_code,
            stderr_tail.lines().last().unwrap_or("")
        ));
    }

    // Prefer the marker that moved atomically with the model. It survives a
    // crash immediately after install and is the sidecar's authoritative view.
    let marker_sha = sidecar_status(&app, &engine, &variant)
        .await
        .ok()
        .and_then(|(_, _, sha)| sha);
    let mut installed_sha = marker_sha.or(target_sha);
    let mut installed_info = target_info;
    if installed_sha.is_none()
        && let Ok((sha, info)) = fetch_repo_meta(&repo).await
    {
        installed_sha = Some(sha);
        installed_info = Some(info);
    }

    if let Some(sha) = installed_sha {
        let mut store = load_store();
        let mut entry = store.get(&key).cloned().unwrap_or_default();
        entry.installed_sha = Some(sha.clone());
        entry.inferred = false;
        if entry.latest_sha.is_none() {
            entry.latest_sha = Some(sha);
        }
        if let Some(info) = installed_info {
            entry.info = Some(info);
        }
        entry.dismissed_sha = None;
        store.insert(key, entry);
        save_store(&store);
    }

    let _ = app.emit(
        "asr-download-progress",
        AsrDownloadProgress {
            stage: "Complete".into(),
            percent: 100,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_download_progress;

    #[test]
    fn parses_complete_progress_line() {
        let progress = parse_download_progress("PROGRESS:Verifying:90\r").unwrap();
        assert_eq!(progress.stage, "Verifying");
        assert_eq!(progress.percent, 90);
    }

    #[test]
    fn ignores_non_progress_stderr() {
        assert!(parse_download_progress("FluidAudio: loading model").is_none());
    }
}
