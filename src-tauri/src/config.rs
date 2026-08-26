use crate::storage::get_data_dir;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum TranscriptionProvider {
    #[default]
    FluidAudio,
    Qwen3,
    AppleSpeech,
    /// Serde catch-all so settings.json files that still reference dead cloud
    /// providers (OpenAI / Google / Anthropic — killed in asr-bakeoff per
    /// memory `asr-code-switching`) deserialize without losing every other
    /// field. Consumers treat this as FluidAudio.
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
    AppleSpeech,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TranscriptionConfig {
    pub enabled: bool,
    /// Provider selection (still used by backend execution paths)
    #[serde(default)]
    pub provider: TranscriptionProvider,
    /// Realtime-vs-batch is no longer a user toggle — driven by the chosen
    /// provider. Kept for backwards-compat deserialization of older configs
    /// so they don't fail to load, but the field is ignored at runtime.
    #[serde(default, skip_serializing)]
    pub realtime_enabled: bool,
    #[serde(default, skip_serializing)]
    pub realtime_provider: RealtimeTranscriptionProvider,
    /// Speaker labels (diarization) for recordings — tags who said what.
    /// On-device FluidAudio only; Quick Dictate always runs without it.
    #[serde(default = "default_true")]
    pub diarize: bool,
    /// Code-switch correction language: which language's Cyrillic mangling to
    /// recover (phonetic-key matching against the user's word list, Parakeet
    /// only). "off" disables; "ru" = Russian (the only map today).
    #[serde(default = "default_translit_lang")]
    pub translit_lang: String,
    /// Phonetic match sensitivity for translit (~0.5–0.9). Higher = stricter.
    #[serde(default = "default_translit_threshold")]
    pub translit_threshold: f32,
    /// Minimum word length for translit matching. Below this, words are skipped
    /// (guards against false matches on short tokens). Lower = catches short
    /// terms like "MVP"/"code" at the cost of more false positives.
    #[serde(default = "default_translit_min_len")]
    pub translit_min_len: u32,
    /// Qwen3 model variant: "f32" (quality) or "int8" (lighter/faster).
    #[serde(default = "default_qwen3_variant")]
    pub qwen3_variant: String,
    /// Apple SpeechTranscriber locale (BCP-47). Locale-locked; default en-US.
    #[serde(default = "default_apple_locale")]
    pub apple_locale: String,
    /// Keep one Parakeet process resident for Quick Dictate.
    #[serde(default)]
    pub keep_model_ready: bool,
}

impl TranscriptionConfig {
    /// Whether the currently-selected provider supports streaming/realtime
    /// transcription. The "Real-time" UI toggle was removed; the provider
    /// choice alone determines the mode.
    pub fn realtime_supported(&self) -> bool {
        matches!(
            self.provider,
            TranscriptionProvider::FluidAudio | TranscriptionProvider::AppleSpeech
        )
    }
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: TranscriptionProvider::FluidAudio,
            realtime_enabled: false,
            realtime_provider: RealtimeTranscriptionProvider::default(),
            diarize: true,
            translit_lang: "ru".to_string(),
            translit_threshold: 0.72,
            translit_min_len: 4,
            qwen3_variant: "f32".to_string(),
            apple_locale: "en-US".to_string(),
            keep_model_ready: false,
        }
    }
}

/// What a pipeline step does. Drives runtime dispatch. Each step carries its
/// own inline config (CLI binary / model / cwd / env / save folder) — there is
/// no separate reusable "Connection" object anymore. Networked delivery
/// (Notion / Slack / Telegram / Webhook) was removed: users glue their own
/// delivery with a Shell step (`curl`, `cp`, …). The one built-in destination
/// kept is `SaveLocal` — write the step's content to a local folder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    CliAgent,
    Shell,
    SaveLocal,
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
    /// Default pipeline to auto-assign to new recordings (set in Settings > Audio)
    #[serde(default)]
    pub default_pipeline: Option<String>,
    /// Last pipeline used — highlighted in chip bar on next launch
    #[serde(default)]
    pub last_used_pipeline: Option<String>,
    /// Automatically record meetings on call detection (mic activation). When
    /// off, no detection runs — no popup, no recording. Defaults on; the old
    /// `call_detection_enabled` key is aliased for config migration.
    #[serde(default = "default_true", alias = "call_detection_enabled")]
    pub auto_record_meetings: bool,
    /// Whether the user has completed the interactive UI walkthrough
    #[serde(default)]
    pub walkthrough_completed: bool,
    /// Local LLM settings
    #[serde(default)]
    pub local_llm: LocalLlmConfig,
    /// CLI agent settings for text processing
    #[serde(default)]
    pub cli_agent: CliAgentConfig,
    /// Quick Dictate (hotkey push-to-talk dictation) settings
    #[serde(default)]
    pub dictation: DictationConfig,
    /// Auto-delete recording entries (audio + transcript + metadata) older than
    /// this many days. Swept when the NBP window comes to the foreground.
    /// `0` = never delete (the default — opt-in feature).
    #[serde(default = "default_entry_retention_days")]
    pub entry_retention_days: u32,
}

fn default_entry_retention_days() -> u32 {
    0
}

/// Quick Dictate: hotkey-driven ephemeral mic capture → transcribe → optional pipeline → paste.
/// Bypasses the regular recording pipeline entirely (no storage, no recordings list).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DictationConfig {
    /// Master toggle — when false, no global shortcuts are registered
    #[serde(default)]
    pub enabled: bool,
    /// User-defined shortcuts; each pairs a hotkey with an optional pipeline
    #[serde(default)]
    pub shortcuts: Vec<DictationShortcut>,
    /// When true, Audio-input dictations are saved as full recordings (audio +
    /// transcript) into the same folder as meeting recordings and appear in the
    /// main list with title "Dictation · HH:MM". Off (default) = ephemeral
    /// paste-and-forget (legacy).
    #[serde(default)]
    pub save_dictations: bool,
    /// When true (default), the on-screen HUD overlay is shown during a
    /// dictation session. Off = dictation still records / transcribes / pastes,
    /// just without the visual overlay (silent mode).
    #[serde(default = "default_true")]
    pub show_hud: bool,
}

impl Default for DictationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            shortcuts: Vec::new(),
            save_dictations: false,
            show_hud: true,
        }
    }
}

/// What feeds into the pipeline / paste step:
/// - `Audio` — record mic, transcribe, then run pipeline / paste (classic mode)
/// - `Clipboard` — snapshot the current pasteboard text, skip mic, run pipeline / paste
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum DictationInputSource {
    #[default]
    Audio,
    Clipboard,
}

/// How a shortcut press maps to start/stop:
/// - `Toggle` — press starts, press again stops (classic; default)
/// - `PushToTalk` — hold to record, release to stop & process
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum DictationTriggerMode {
    #[default]
    Toggle,
    PushToTalk,
}

/// One Quick Dictate shortcut configuration.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DictationShortcut {
    /// Stable id for the entry (UI-managed)
    pub id: String,
    /// Display name shown in Settings
    #[serde(default)]
    pub name: String,
    /// Global hotkey string in tauri-plugin-global-shortcut format, e.g. "cmd+shift+d"
    pub hotkey: String,
    /// Where this shortcut's input comes from
    #[serde(default)]
    pub input_source: DictationInputSource,
    /// Mic device name override (None = system default)
    #[serde(default)]
    pub device_name: Option<String>,
    /// BCP-47 language tag forwarded to engines that take one
    /// (currently Apple Speech via `--lang`). None = use whatever the
    /// engine defaults to (typically system locale).
    #[serde(default)]
    pub language: Option<String>,
    /// Optional pipeline name — when set, the transcript is passed through the
    /// pipeline's CLI-agent steps (text transforms chain); Shell steps are
    /// skipped silently since the paste-only dictation flow has no output dir.
    #[serde(default)]
    pub pipeline: Option<String>,
    /// Auto-paste into the focused app via Cmd+V (true) or only copy to clipboard (false)
    #[serde(default = "default_true")]
    pub auto_paste: bool,
    /// Capture system audio (loopback via Core Audio Process Taps) in addition to mic.
    /// Useful for transcribing both sides of a call or dictating over playback.
    #[serde(default)]
    pub capture_system_audio: bool,
    /// How the hotkey press behaves. Toggle (default) or push-to-talk (hold).
    /// Push-to-talk is the natural fit for the Fn (🌐) key as a momentary modifier.
    #[serde(default)]
    pub trigger_mode: DictationTriggerMode,
}

fn default_true() -> bool {
    true
}

fn default_translit_threshold() -> f32 {
    0.72
}

fn default_translit_lang() -> String {
    "ru".to_string()
}

fn default_translit_min_len() -> u32 {
    4
}

fn default_qwen3_variant() -> String {
    "f32".to_string()
}

fn default_apple_locale() -> String {
    "en-US".to_string()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            storage_path: get_data_dir().to_string_lossy().to_string(),
            auto_discard_seconds: 3,
            theme: "auto".to_string(),
            onboarding_completed: false,
            transcription: TranscriptionConfig::default(),
            show_recording_notification: true,
            save_mix_only: true,
            default_pipeline: None,
            last_used_pipeline: None,
            auto_record_meetings: true,
            walkthrough_completed: false,
            local_llm: LocalLlmConfig::default(),
            cli_agent: CliAgentConfig::default(),
            dictation: DictationConfig::default(),
            entry_retention_days: default_entry_retention_days(),
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

/// Collapse a theme value to the current trinary set (auto/light/dark). The
/// single source of truth for the on-disk migration in `load_settings` — once a
/// settings file is migrated, the rest of the app (and the frontend) only ever
/// sees auto/light/dark and never has to know the old names existed.
fn normalize_theme(theme: &str) -> &'static str {
    match theme {
        "light" => "light",
        "dark" => "dark",
        // Legacy names from the old Neon Purple / Deep Blue / Light split.
        "neon-purple" | "deep-obsidian" | "deep-blue" => "dark",
        "light-pastel" => "light",
        _ => "auto",
    }
}

#[tauri::command]
pub fn load_settings() -> AppSettings {
    let path = get_settings_path();
    if !path.exists() {
        return AppSettings::default();
    }

    let mut settings: AppSettings = match File::open(&path) {
        Ok(file) => serde_json::from_reader(file).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    };

    // One-shot theme migration: rewrite a legacy theme name to auto/light/dark
    // on disk, once. After this the value is always current — no normalize
    // shim lingering in the runtime or the frontend.
    let normalized = normalize_theme(&settings.theme);
    if settings.theme != normalized {
        settings.theme = normalized.to_string();
        let _ = save_settings_to_disk(&mut settings);
    }

    settings
}

/// Save settings to disk (internal — no Tauri state required)
pub fn save_settings_to_disk(settings: &mut AppSettings) -> Result<(), String> {
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
#[tauri::command]
pub async fn save_settings(
    app_handle: tauri::AppHandle,
    mut settings: AppSettings,
) -> Result<(), String> {
    // Disk write is fast (JSON serialize + atomic rename) — keep it on the
    // command's hot path so the frontend sees Ok only after settings are
    // persisted.
    let auto_record = settings.auto_record_meetings;
    save_settings_to_disk(&mut settings)?;
    crate::parakeet_worker::reconcile(&app_handle);
    crate::parakeet_worker::request_warmup(&app_handle, "settings");

    // Detector sync is NOT on the hot path: call_detector::sync_detector
    // and audio_process_detector::sync_detector both call handle.join() on
    // their worker threads when disabling. Each can take 1-3 seconds
    // (CoreAudio HAL listener teardown + ~1s poll tick), so doing them
    // inline made the auto-record toggle visibly lag the UI. Fire them
    // into spawn_blocking and return Ok immediately — detectors converge
    // on their own a moment later.
    let app = app_handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let det = app.state::<crate::call_detector::CallDetectorState>();
        let pdet = app.state::<crate::audio_process_detector::AudioProcessDetectorState>();
        crate::call_detector::sync_detector(&det, auto_record, &app);
        crate::audio_process_detector::sync_detector(&pdet, auto_record, &app);
    });

    Ok(())
}
