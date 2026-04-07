use crate::integrations::IntegrationsConfig;
use crate::storage::get_data_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum TranscriptionProvider {
    #[default]
    FluidAudio,
    LocalWhisper,
    OpenAI,
    Google,
    Anthropic,
    #[serde(other)]
    Unknown,
}

/// CLI agent configuration for text processing (summarization, templates)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CliAgentConfig {
    /// Which CLI to use: "claude", "codex", or "opencode"
    #[serde(default = "default_cli_agent")]
    pub cli: String,
    /// Model to use (optional, CLI default if empty)
    #[serde(default)]
    pub model: Option<String>,
    /// Timeout in seconds
    #[serde(default = "default_cli_timeout")]
    pub timeout_secs: u64,
}

fn default_cli_agent() -> String {
    "claude".to_string()
}

fn default_cli_timeout() -> u64 {
    300
}


impl Default for CliAgentConfig {
    fn default() -> Self {
        Self {
            cli: "claude".to_string(),
            model: None,
            timeout_secs: 300,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum RealtimeTranscriptionProvider {
    #[default]
    Local,
    OpenAI,
    #[serde(other)]
    Unknown,
}

/// API keys for cloud AI services
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ApiKeys {
    #[serde(default)]
    pub openai: Option<String>,
    #[serde(default)]
    pub google: Option<String>,
    #[serde(default)]
    pub anthropic: Option<String>,
}

/// Cached model metadata (preserved across sessions)
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CachedModel {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub deprecated: bool,
}

/// Per-provider configuration (API key + capabilities + available models)
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ProviderConfig {
    /// API key for this provider (None for local providers)
    #[serde(default)]
    pub api_key: Option<String>,
    /// Capability badges: e.g. ["Transcription", "Processing", "Embedding"]
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Known model IDs for this provider (legacy, kept for migration)
    #[serde(default)]
    pub models: Vec<String>,
    /// Cached model metadata with full capabilities
    #[serde(default)]
    pub cached_models: Vec<CachedModel>,
    /// Unix timestamp when models were last successfully fetched from provider API
    #[serde(default)]
    pub models_fetched_at: Option<i64>,
    /// Optional custom base URL for provider APIs (currently used by Ollama)
    #[serde(default)]
    pub base_url: Option<String>,
}

impl ProviderConfig {
    pub fn new(caps: &[&str], models: &[&str]) -> Self {
        Self {
            api_key: None,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            models: models.iter().map(|s| s.to_string()).collect(),
            cached_models: vec![],
            models_fetched_at: None,
            base_url: None,
        }
    }

    pub fn with_capabilities(caps: &[&str]) -> Self {
        Self::new(caps, &[])
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum WhisperModelSize {
    Tiny,
    Base,
    Small,
    Medium,
    Large,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TranscriptionConfig {
    pub enabled: bool,
    /// Provider selection (still used by backend execution paths)
    #[serde(default)]
    pub provider: TranscriptionProvider,
    /// Local whisper model selection (still used by backend execution paths)
    #[serde(default)]
    pub whisper_model: Option<WhisperModelSize>,
    /// Role-scoped API keys — persisted so the frontend UI can read them; migrated into
    /// providers map on load and on save to keep provider-first storage in sync.
    #[serde(default)]
    pub api_keys: ApiKeys,
    // Very old single-key legacy field — read-only for migration, never written
    #[serde(skip_serializing, default)]
    pub api_key: Option<String>,
    /// Whether real-time (live) transcription is enabled
    #[serde(default)]
    pub realtime_enabled: bool,
    /// Provider for real-time transcription (Local or OpenAI)
    #[serde(default)]
    pub realtime_provider: RealtimeTranscriptionProvider,
    /// Deprecated: realtime now uses whisper_model. Kept for backwards-compat deserialization.
    #[serde(default, skip_serializing)]
    pub realtime_model: Option<String>,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: TranscriptionProvider::FluidAudio,
            whisper_model: Some(WhisperModelSize::Base),
            api_keys: ApiKeys::default(),
            api_key: None,
            realtime_enabled: false,
            realtime_provider: RealtimeTranscriptionProvider::default(),
            realtime_model: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub storage_path: String,
    pub auto_discard_seconds: u32,
    pub theme: String,
    #[serde(default)]
    pub onboarding_completed: bool,
    #[serde(default)]
    pub transcription: TranscriptionConfig,
    #[serde(default)]
    pub show_recording_notification: bool,
    /// Save only the mixed audio file (default: true)
    #[serde(default = "default_true")]
    pub save_mix_only: bool,
    #[serde(default)]
    pub integrations: IntegrationsConfig,
    /// Default pipeline to auto-assign to new recordings (set in Settings > Audio)
    #[serde(default)]
    pub default_pipeline: Option<String>,
    /// Last pipeline used — highlighted in chip bar on next launch
    #[serde(default)]
    pub last_used_pipeline: Option<String>,
    /// Detect calls (mic activation) and send a macOS notification
    #[serde(default)]
    pub call_detection_enabled: bool,
    /// Auto-stop recording after N seconds of silence (0 = disabled)
    #[serde(default)]
    pub auto_stop_silence_seconds: u32,
    /// Whether the user has completed the interactive UI walkthrough
    #[serde(default)]
    pub walkthrough_completed: bool,
    /// Local LLM settings
    #[serde(default)]
    pub local_llm: LocalLlmConfig,
    /// CLI agent settings for text processing
    #[serde(default)]
    pub cli_agent: CliAgentConfig,
    /// Unix timestamp of last automatic model freshness check
    #[serde(default)]
    pub last_model_freshness_check: Option<i64>,
    /// Cached model freshness results from last auto-check (model_id → update_available)
    #[serde(default)]
    pub cached_freshness_results: HashMap<String, bool>,
    /// Provider-first model config: keyed by provider ID ("openai", "google", "anthropic", "local")
    #[serde(default = "default_providers")]
    pub providers: HashMap<String, ProviderConfig>,
}

fn default_true() -> bool {
    true
}

fn default_providers() -> HashMap<String, ProviderConfig> {
    let mut map = HashMap::new();
    map.insert(
        "openai".to_string(),
        ProviderConfig::new(
            &["Transcription", "Processing"],
            &["whisper-1", "gpt-4o", "gpt-4o-mini"],
        ),
    );
    map.insert(
        "google".to_string(),
        ProviderConfig::new(
            &["Transcription", "Processing"],
            &["gemini-1.5-pro", "gemini-1.5-flash"],
        ),
    );
    map.insert(
        "anthropic".to_string(),
        ProviderConfig::new(
            &["Processing"],
            &["claude-3-5-sonnet-20241022", "claude-3-haiku-20240307"],
        ),
    );
    map.insert(
        "local".to_string(),
        ProviderConfig::new(&["Processing"], &[]),
    );
    map.insert(
        "ollama".to_string(),
        ProviderConfig::new(&["Processing"], &[]),
    );
    map
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            storage_path: get_data_dir().to_string_lossy().to_string(),
            auto_discard_seconds: 3,
            theme: "neon-purple".to_string(),
            onboarding_completed: false,
            transcription: TranscriptionConfig::default(),
            show_recording_notification: true,
            save_mix_only: true,
            integrations: IntegrationsConfig::default(),
            default_pipeline: None,
            last_used_pipeline: None,
            call_detection_enabled: false,
            auto_stop_silence_seconds: 0,
            walkthrough_completed: false,
            local_llm: LocalLlmConfig::default(),
            cli_agent: CliAgentConfig::default(),
            last_model_freshness_check: None,
            cached_freshness_results: HashMap::new(),
            providers: default_providers(),
        }
    }
}

pub fn get_config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".nbp")
}

pub fn get_settings_path() -> PathBuf {
    get_config_dir().join("settings.json")
}

pub fn get_models_dir() -> PathBuf {
    get_config_dir().join("models")
}

pub fn get_llm_models_dir() -> PathBuf {
    get_config_dir().join("models").join("llm")
}

/// Local LLM configuration
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocalLlmConfig {
    pub enabled: bool,
    /// Selected local LLM model ID — still used by backend execution paths
    #[serde(default)]
    pub model_id: Option<String>,
    /// Number of layers to offload to GPU (99 = all)
    pub gpu_layers: u32,
    /// Context window size in tokens
    pub context_size: u32,
}

impl Default for LocalLlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_id: None,
            gpu_layers: 99,
            context_size: 8192,
        }
    }
}

#[tauri::command]
pub fn load_settings() -> AppSettings {
    let path = get_settings_path();
    if !path.exists() {
        return AppSettings::default();
    }

    match File::open(&path) {
        Ok(file) => {
            let mut settings: AppSettings = serde_json::from_reader(file).unwrap_or_default();

            // Migration v1: move legacy api_key to api_keys.openai
            if let Some(legacy_key) = settings.transcription.api_key.take() {
                if settings.transcription.api_keys.openai.is_none() {
                    settings.transcription.api_keys.openai = Some(legacy_key);
                }
            }

            // Ensure all default providers exist (for existing settings files missing new entries)
            // Run this BEFORE migration so any newly-inserted entries have correct capabilities.
            for (id, default_cfg) in default_providers() {
                let entry = settings.providers.entry(id).or_insert(default_cfg.clone());
                // If entry exists but has empty capabilities or models (inserted by an older
                // migration before models were added), restore the defaults.
                if entry.capabilities.is_empty() {
                    entry.capabilities = default_cfg.capabilities;
                }
                if entry.models.is_empty() {
                    entry.models = default_cfg.models;
                }
            }

            // Migration v2: sync transcription.api_keys into providers map
            // If a provider entry has no api_key set but transcription.api_keys has one, migrate it.
            // This ensures the new providers field stays populated after upgrade.
            for (provider_id, legacy_key) in [
                ("openai", settings.transcription.api_keys.openai.clone()),
                ("google", settings.transcription.api_keys.google.clone()),
                (
                    "anthropic",
                    settings.transcription.api_keys.anthropic.clone(),
                ),
            ] {
                if let Some(key) = legacy_key {
                    if let Some(entry) = settings.providers.get_mut(provider_id) {
                        if entry.api_key.is_none() {
                            entry.api_key = Some(key);
                        }
                    }
                }
            }

            // Sync providers → api_keys so the frontend UI reads accurate key state.
            // providers is the authoritative source; api_keys is kept in sync for the UI.
            if let Some(cfg) = settings.providers.get("openai") {
                settings.transcription.api_keys.openai = cfg.api_key.clone();
            }
            if let Some(cfg) = settings.providers.get("google") {
                settings.transcription.api_keys.google = cfg.api_key.clone();
            }
            if let Some(cfg) = settings.providers.get("anthropic") {
                settings.transcription.api_keys.anthropic = cfg.api_key.clone();
            }

            settings
        }
        Err(_) => AppSettings::default(),
    }
}

/// Save settings to disk (internal — no Tauri state required)
pub fn save_settings_to_disk(settings: &mut AppSettings) -> Result<(), String> {
    // Pre-save migration: sync transcription.api_keys into providers map.
    for (provider_id, legacy_key) in [
        ("openai", settings.transcription.api_keys.openai.clone()),
        ("google", settings.transcription.api_keys.google.clone()),
        (
            "anthropic",
            settings.transcription.api_keys.anthropic.clone(),
        ),
    ] {
        if let Some(key) = legacy_key {
            settings
                .providers
                .entry(provider_id.to_string())
                .or_insert_with(ProviderConfig::default)
                .api_key = if key.is_empty() { None } else { Some(key) };
        }
    }

    let config_dir = get_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    }

    let path = get_settings_path();
    let file = File::create(&path).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &settings).map_err(|e| e.to_string())?;

    // Set file permissions to 600 (user read/write only) for security
    let mut perms = fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;

    Ok(())
}

/// Tauri command: save settings + toggle call detector.
///
/// The frontend may hold a stale copy of `integrations` (Slack/Notion/Linear are
/// modified via dedicated commands that write directly to disk).  To prevent the
/// frontend from accidentally overwriting them, we always preserve the on-disk
/// integrations block.
#[tauri::command]
pub fn save_settings(
    app_handle: tauri::AppHandle,
    mut settings: AppSettings,
    detector_state: tauri::State<'_, crate::call_detector::CallDetectorState>,
) -> Result<(), String> {
    // Preserve integrations from disk — frontend doesn't own this data.
    let disk_settings = load_settings();
    settings.integrations = disk_settings.integrations;

    crate::call_detector::sync_detector(&detector_state, settings.call_detection_enabled, &app_handle);
    save_settings_to_disk(&mut settings)
}

/// Resolve the API key for a provider.
/// Checks `providers` map first, then falls back to `transcription.api_keys` for backward compat.
/// Auto-detect the best processing provider by checking which has an API key configured.
/// Priority: openai > anthropic > google > local > ollama
pub fn detect_processing_provider(settings: &AppSettings) -> String {
    for id in &["openai", "anthropic", "google"] {
        if get_api_key_for_provider(settings, id).is_some() {
            return id.to_string();
        }
    }
    if settings.local_llm.enabled && settings.local_llm.model_id.is_some() {
        return "local".to_string();
    }
    "openai".to_string()
}

pub fn get_api_key_for_provider(settings: &AppSettings, provider: &str) -> Option<String> {
    // Check new providers map first
    if let Some(cfg) = settings.providers.get(provider) {
        if cfg.api_key.is_some() {
            return cfg.api_key.clone();
        }
    }
    // Fall back to legacy transcription.api_keys
    match provider {
        "openai" => settings.transcription.api_keys.openai.clone(),
        "google" => settings.transcription.api_keys.google.clone(),
        "anthropic" => settings.transcription.api_keys.anthropic.clone(),
        _ => None,
    }
}

/// Get templates directory path
pub fn get_templates_dir() -> PathBuf {
    get_config_dir().join("templates")
}

/// Get integrations directory path (~/.nbp/integrations/)
pub fn get_integrations_dir() -> PathBuf {
    get_config_dir().join("integrations")
}
