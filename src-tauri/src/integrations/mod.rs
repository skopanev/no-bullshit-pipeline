use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod slack;
pub mod notion;
pub mod save_path;
pub mod linear;
pub mod webhook;

#[cfg(not(debug_assertions))]
pub const KEYCHAIN_SERVICE: &str = "one.nbp.skk";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct IntegrationsConfig {
    #[serde(default)]
    pub slack: HashMap<String, slack::SlackIntegration>,
}

pub use slack::*;

// ──────────────────────────────────────────────────────────────────────────────
// Shared credential helpers with dev-mode bypass
// ──────────────────────────────────────────────────────────────────────────────

/// Save a credential token. In debug builds, writes to `.dev-credentials.json`
/// at the project root. In release builds, uses macOS Keychain.
pub fn save_token(account_key: &str, token: &str) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        save_token_dev(account_key, token)
    }
    #[cfg(not(debug_assertions))]
    {
        save_token_keychain(account_key, token)
    }
}

/// Retrieve a credential token. In debug builds, reads from `.dev-credentials.json`.
/// In release builds, reads from macOS Keychain.
pub fn get_token(account_key: &str) -> Result<String, String> {
    #[cfg(debug_assertions)]
    {
        get_token_dev(account_key)
    }
    #[cfg(not(debug_assertions))]
    {
        get_token_keychain(account_key)
    }
}

/// Delete a credential token. In debug builds, removes from `.dev-credentials.json`.
/// In release builds, removes from macOS Keychain.
pub fn delete_token(account_key: &str) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        delete_token_dev(account_key)
    }
    #[cfg(not(debug_assertions))]
    {
        delete_token_keychain(account_key)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Dev-mode implementations (debug builds only)
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(debug_assertions)]
fn dev_credentials_path() -> std::path::PathBuf {
    // `.dev-credentials.json` lives at the workspace root (where `cargo tauri dev` runs)
    std::path::PathBuf::from(".dev-credentials.json")
}

#[cfg(debug_assertions)]
fn read_dev_credentials() -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let path = dev_credentials_path();
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read .dev-credentials.json: {}", e))?;
    let value: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse .dev-credentials.json: {}", e))?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err("dev-credentials.json must be a JSON object".to_string()),
    }
}

#[cfg(debug_assertions)]
fn write_dev_credentials(map: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let path = dev_credentials_path();
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(map.clone()))
        .map_err(|e| format!("Failed to serialize credentials: {}", e))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write .dev-credentials.json: {}", e))
}

#[cfg(debug_assertions)]
fn save_token_dev(account_key: &str, token: &str) -> Result<(), String> {
    let mut map = read_dev_credentials()?;
    map.insert(account_key.to_string(), serde_json::Value::String(token.to_string()));
    write_dev_credentials(&map)
}

#[cfg(debug_assertions)]
fn get_token_dev(account_key: &str) -> Result<String, String> {
    let map = read_dev_credentials()?;
    map.get(account_key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Token '{}' not found in .dev-credentials.json", account_key))
}

#[cfg(debug_assertions)]
fn delete_token_dev(account_key: &str) -> Result<(), String> {
    let mut map = read_dev_credentials()?;
    map.remove(account_key);
    write_dev_credentials(&map)
}

// ──────────────────────────────────────────────────────────────────────────────
// Release-mode implementations (Keychain, non-debug builds only)
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(not(debug_assertions))]
fn save_token_keychain(account_key: &str, token: &str) -> Result<(), String> {
    use security_framework::passwords::set_generic_password;
    set_generic_password(KEYCHAIN_SERVICE, account_key, token.as_bytes())
        .map_err(|e| format!("Failed to save token to Keychain: {}", e))
}

#[cfg(not(debug_assertions))]
fn get_token_keychain(account_key: &str) -> Result<String, String> {
    use security_framework::passwords::get_generic_password;
    let password = get_generic_password(KEYCHAIN_SERVICE, account_key)
        .map_err(|e| format!("Token not found in Keychain: {}", e))?;
    String::from_utf8(password.to_vec())
        .map_err(|e| format!("Invalid token encoding: {}", e))
}

#[cfg(not(debug_assertions))]
fn delete_token_keychain(account_key: &str) -> Result<(), String> {
    use security_framework::passwords::delete_generic_password;
    delete_generic_password(KEYCHAIN_SERVICE, account_key)
        .map_err(|e| format!("Failed to delete token from Keychain: {}", e))
}
