//! # NBP (No Bullshit Pipeline)
//!
//! Privacy-first macOS audio capture application built with Tauri 2.
//! Records microphone and system audio simultaneously, processes with
//! AI-powered transcription, and pipes results through configurable pipelines.
//!
//! ## Module Overview
//!
//! ### Core Audio
//! - [`audio`] - Recording lifecycle (start/stop/pause/resume) and state management
//! - [`mic_audio`] - Microphone capture via cpal
//! - [`system_audio`] - System audio capture via Core Audio Process Taps (cidre)
//! - [`audio_processing`] - EBU R128 normalization, real-time mixing, shared buffers
//! - [`playback`] - In-app audio playback via rodio
//! - [`waveform`] - Waveform data extraction for visualization
//! - [`devices`] - Audio input device enumeration
//!
//! ### AI & Transcription
//! - [`transcription`] - Local Whisper and cloud transcription
//! - [`cloud_ai`] - Cloud AI providers (OpenAI, Google, Anthropic)
//! - [`templates`] - Legacy output templates for structured extraction
//!
//! ### Pipeline System
//! - [`pipelines`] - Pipeline definition model and validation
//! - [`pipeline_engine`] - Sequential step execution engine with progress events
//! - [`connectors`] - Built-in connectors (LLM, Save, Webhook)
//! - [`prompt_templates`] - Reusable prompt template registry
//! - [`transcript_migration`] - Migration from plain text to frontmatter transcript format
//!
//! ### Infrastructure
//! - [`storage`] - Recording metadata, filesystem layout, CRUD operations
//! - [`config`] - App settings, API key management
//! - [`permissions`] - macOS permission checks (microphone, screen recording)

mod audio;
pub mod audio_processing;
pub mod storage;
mod system_audio;
mod mic_audio;
mod permissions;
pub mod config;
pub mod transcription;
mod cloud_ai;
mod templates;
pub mod playback;
mod waveform;
mod devices;
pub mod pipelines;
mod prompt_templates;
mod connectors;
mod pipeline_engine;
mod transcript_migration;
mod integrations;
pub mod local_llm;
pub mod realtime_transcription;
mod call_detector;
mod dictation;
use audio::AudioState;
use transcription::TranscriptionState;
use tauri::menu::{AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn get_audio_level() -> f32 {
    // Return max of mic and system audio levels
    let mic_level = mic_audio::get_current_audio_level();
    let system_level = system_audio::get_system_audio_level();
    mic_level.max(system_level)
}

#[derive(serde::Serialize)]
struct AudioLevels {
    mic: f32,
    system: f32,
}

#[tauri::command]
fn get_audio_levels() -> AudioLevels {
    AudioLevels {
        mic: mic_audio::get_current_audio_level(),
        system: system_audio::get_system_audio_level(),
    }
}

pub fn run() {
    // Init logging so log::info!/error! prints to stderr
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("nbp=debug"))
        .init();

    // Load settings for managed state
    let settings = std::sync::Arc::new(std::sync::Mutex::new(config::load_settings()));
    
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Focus the existing window when a second instance is launched
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AudioState::new())
        .manage(TranscriptionState::new())
        .manage(dictation::DictationState::new())
        .manage(permissions::PermissionsStateCache(std::sync::Arc::new(std::sync::Mutex::new(
            permissions::PermissionsState::default()
        ))))
        .manage(settings)
        .setup(|app| {
            // Ensure the app appears in Dock and Cmd+Tab
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);

            // Create custom menu with only NBP submenu (no File, Edit, etc.)
            let about_metadata = AboutMetadataBuilder::new()
                .name(Some("NBP"))
                .version(Some(env!("CARGO_PKG_VERSION")))
                .copyright(Some("© 2024-2026"))
                .comments(Some("No Bullshit Pipeline - Audio recording and transcription"))
                .build();

            let app_submenu = SubmenuBuilder::new(app, "NBP")
                .about(Some(about_metadata))
                .separator()
                .services()
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;

            let edit_submenu = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;

            let window_submenu = SubmenuBuilder::new(app, "Window")
                .minimize()
                .close_window()
                .build()?;

            let menu = MenuBuilder::new(app)
                .item(&app_submenu)
                .item(&edit_submenu)
                .item(&window_submenu)
                .build()?;

            app.set_menu(menu)?;

            // Run transcript migration on startup
            transcript_migration::run_migration_if_needed();

            // Auto-check model freshness if >24h since last check
            let settings = config::load_settings();
            let now = chrono::Utc::now().timestamp();
            let needs_check = match settings.last_model_freshness_check {
                Some(last) => now - last > 86400,
                None => true,
            };
            if needs_check {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    local_llm::auto_check_model_freshness(handle).await;
                });
            }

            // Request notification permission if call detection is enabled
            if settings.call_detection_enabled {
                use tauri_plugin_notification::NotificationExt;
                use tauri::plugin::PermissionState;
                if app.notification().permission_state()
                    .unwrap_or(PermissionState::Prompt) != PermissionState::Granted
                {
                    let _ = app.notification().request_permission();
                }
            }

            // Set up UNUserNotificationCenter delegate for handling notification clicks
            call_detector::init_notification_delegate(app.handle());

            // Initialize call detector state (always managed, started conditionally)
            let detector = if settings.call_detection_enabled {
                Some(call_detector::CallDetector::start(app.handle().clone()))
            } else {
                None
            };
            app.manage(call_detector::CallDetectorState(std::sync::Mutex::new(detector)));

            // Build system tray menu
            build_tray(app)?;

            // Quick Dictate: init plugin (always, even if disabled — so reload works
            // at runtime without restart). Shortcuts are then registered if enabled.
            #[cfg(desktop)]
            {
                let init = app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new().build(),
                );
                if let Err(e) = init {
                    log::warn!("global-shortcut plugin init failed: {}", e);
                } else if settings.dictation.enabled {
                    match reload_dictation_shortcuts(app.handle()) {
                        Ok(results) => {
                            for r in &results {
                                if r.status != "registered" {
                                    log::warn!(
                                        "dictation startup: {} ({}) → {} {}",
                                        r.id,
                                        r.hotkey,
                                        r.status,
                                        r.error.clone().unwrap_or_default()
                                    );
                                }
                            }
                        }
                        Err(e) => log::warn!("dictation: initial registration failed: {}", e),
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_version,
            audio::start_recording,
            audio::stop_recording,
            audio::pause_recording,
            audio::resume_recording,
            storage::list_recordings,
            storage::read_metadata,
            storage::update_tags,
            storage::update_title,
            storage::delete_recording,
            storage::list_projects,
            storage::save_projects,
            permissions::check_permissions,
            permissions::request_mic_permission,
            permissions::request_system_audio_permission,
            permissions::open_privacy_settings,
            config::load_settings,
            config::save_settings,
            transcription::get_whisper_models_info,
            transcription::download_whisper_model,
            transcription::delete_whisper_model,
            transcription::transcribe_recording,
            transcription::is_transcribing,
            transcription::get_transcript,
            transcription::export_transcript_md,
            transcription::summarize_recording,
            transcription::process_with_template,
            templates::list_templates,
            templates::get_template,
            playback::play_audio,
            playback::pause_audio,
            playback::resume_audio,
            playback::stop_audio,
            playback::seek_audio,
            playback::get_playback_state,
            waveform::get_waveform_data,
            devices::get_input_devices,
            get_audio_level,
            get_audio_levels,
            // Pipeline system
            pipelines::list_pipelines,
            pipelines::get_pipeline,
            pipelines::save_pipeline,
            pipelines::delete_pipeline,
            // Prompt templates
            prompt_templates::list_prompt_templates,
            prompt_templates::get_prompt_template,
            prompt_templates::save_prompt_template,
            prompt_templates::delete_prompt_template,
            // Pipeline execution
            pipeline_engine::execute_pipeline,
            pipeline_engine::get_pipeline_status,
            pipeline_engine::get_all_pipeline_states,
            pipeline_engine::get_step_outputs,
            pipeline_engine::assign_pipeline,
            pipeline_engine::remove_pipeline_run,
            connectors::cli_agent::check_cli_availability,
            // Integrations
            integrations::list_slack_integrations,
            integrations::add_slack_integration,
            integrations::remove_slack_integration,
            integrations::test_slack_integration,
            integrations::list_slack_channels,
            integrations::list_slack_members,
            // Notion integrations
            integrations::notion::add_notion_integration,
            integrations::notion::list_notion_databases,
            integrations::notion::sync_notion_schema,
            integrations::notion::update_notion_people_mappings,
            integrations::notion::test_notion_integration,
            integrations::notion::remove_notion_integration,
            integrations::notion::list_notion_profiles,
            // Save path integrations
            integrations::save_path::add_save_path_integration,
            integrations::save_path::list_save_path_integrations,
            integrations::save_path::update_save_path_integration,
            integrations::save_path::remove_save_path_integration,
            // Linear integrations
            integrations::linear::add_linear_integration,
            integrations::linear::list_linear_teams,
            integrations::linear::sync_linear_schema,
            integrations::linear::test_linear_integration,
            integrations::linear::remove_linear_integration,
            integrations::linear::list_linear_profiles,
            integrations::linear::update_linear_member_aliases,
            // Webhook integrations
            integrations::webhook::add_webhook_integration,
            integrations::webhook::list_webhook_profiles,
            integrations::webhook::remove_webhook_integration,
            integrations::webhook::test_webhook_integration,
            // Local LLM
            local_llm::get_llm_models_info,
            local_llm::download_llm_model,
            local_llm::cancel_llm_download,
            local_llm::delete_llm_model,
            local_llm::check_model_freshness,
            local_llm::check_all_llm_freshness,
            local_llm::cancel_llm_freshness,
            local_llm::get_cached_freshness_results,
            // Provider models
            cloud_ai::models::fetch_provider_models,
            // Real-time transcription
            audio::start_realtime_transcription,
            audio::stop_realtime_transcription,
            // Call detection
            call_detector::test_call_notification,
            // Quick Dictate
            dictation::dictation_start,
            dictation::dictation_stop,
            dictation::dictation_toggle,
            dictation::dictation_is_active,
            dictation::dictation_reload_shortcuts,
        ])
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    // Hide window instead of closing — tray keeps the app alive
                    api.prevent_close();
                    let _ = window.hide();
                    // Hide from dock too
                    #[cfg(target_os = "macos")]
                    {
                        let app = window.app_handle();
                        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                    }
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Per-shortcut registration result returned to the frontend so it can show
/// inline status badges (✓ / ⚠ conflict with another app / disabled).
#[derive(serde::Serialize, Clone, Debug)]
pub struct ShortcutRegistration {
    pub id: String,
    pub hotkey: String,
    /// "registered" — bound OK; "disabled" — master toggle off; "error" — OS rejected
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Unregister all current dictation hotkeys and re-register from the live settings.
/// Returns a per-shortcut status array so the UI can render conflicts.
#[cfg(desktop)]
pub fn reload_dictation_shortcuts(
    app: &tauri::AppHandle,
) -> Result<Vec<ShortcutRegistration>, String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let settings = config::load_settings();
    let mut results: Vec<ShortcutRegistration> = Vec::with_capacity(settings.dictation.shortcuts.len());

    if !settings.dictation.enabled {
        log::info!("dictation: disabled — no shortcuts registered");
        for sc in &settings.dictation.shortcuts {
            results.push(ShortcutRegistration {
                id: sc.id.clone(),
                hotkey: sc.hotkey.clone(),
                status: "disabled".into(),
                error: None,
            });
        }
        return Ok(results);
    }

    for sc in &settings.dictation.shortcuts {
        if sc.hotkey.trim().is_empty() {
            results.push(ShortcutRegistration {
                id: sc.id.clone(),
                hotkey: sc.hotkey.clone(),
                status: "error".into(),
                error: Some("Empty hotkey".into()),
            });
            continue;
        }
        let sid = sc.id.clone();
        let name = sc.name.clone();
        let hotkey = sc.hotkey.clone();
        let result = gs.on_shortcut(hotkey.as_str(), move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let app = app.clone();
                let sid = sid.clone();
                tauri::async_runtime::spawn(async move {
                    let active = app
                        .state::<dictation::DictationState>()
                        .is_active
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let result = if active {
                        dictation::stop_inner(&app).await.map(|_| ())
                    } else {
                        dictation::start_inner(&app, &sid)
                    };
                    if let Err(e) = result {
                        log::warn!("dictation toggle for '{}' failed: {}", sid, e);
                    }
                });
            }
        });
        match result {
            Ok(()) => {
                log::info!("dictation: registered '{}' ({} → {})", name, hotkey, sc.id);
                results.push(ShortcutRegistration {
                    id: sc.id.clone(),
                    hotkey: hotkey.clone(),
                    status: "registered".into(),
                    error: None,
                });
            }
            Err(e) => {
                let msg = e.to_string();
                log::warn!("dictation: failed to register '{}' ({}): {}", name, hotkey, msg);
                results.push(ShortcutRegistration {
                    id: sc.id.clone(),
                    hotkey: hotkey.clone(),
                    status: "error".into(),
                    error: Some(msg),
                });
            }
        }
    }
    Ok(results)
}

/// Show the main window and restore dock presence
fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Build the system tray icon with menu
fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{Menu, PredefinedMenuItem};

    let mut items: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> = Vec::new();

    // Add pipeline items
    if let Ok(pipeline_list) = pipelines::load_pipelines() {
        let settings = config::load_settings();
        let mut names: Vec<String> = Vec::new();

        // Last-used first
        if let Some(ref last) = settings.last_used_pipeline {
            if pipeline_list.contains_key(last) {
                names.push(last.clone());
            }
        }
        // Default second
        if let Some(ref default) = settings.default_pipeline {
            if pipeline_list.contains_key(default) && !names.contains(default) {
                names.push(default.clone());
            }
        }
        // Rest
        for name in pipeline_list.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names.truncate(5);

        for name in &names {
            let item = MenuItemBuilder::with_id(
                format!("pipeline:{}", name),
                format!("▶ {}", name),
            )
            .build(app)?;
            items.push(Box::new(item));
        }

        if !names.is_empty() {
            items.push(Box::new(PredefinedMenuItem::separator(app)?));
        }
    }

    // Show NBP
    let show_item = MenuItemBuilder::with_id("show", "Show NBP").build(app)?;
    items.push(Box::new(show_item));

    // Separator + Quit
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    let quit_item = MenuItemBuilder::with_id("quit", "Quit NBP").build(app)?;
    items.push(Box::new(quit_item));

    // Build menu from items
    let item_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        items.iter().map(|i| i.as_ref()).collect();
    let menu = Menu::with_items(app, &item_refs)?;

    let _tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("NBP")
        .on_menu_event(|app: &tauri::AppHandle, event| {
            let id = event.id.as_ref();
            if id.starts_with("pipeline:") {
                let pipeline_name = id.strip_prefix("pipeline:").unwrap().to_string();
                show_main_window(app);
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.emit("tray-start-pipeline", pipeline_name);
                }
            } else {
                match id {
                    "show" => {
                        show_main_window(app);
                    }
                    "quit" => {
                        // Graceful shutdown
                        if let Some(window) = app.get_webview_window("main") {
                            let state: tauri::State<AudioState> = window.state();
                            {
                                let is_recording = state.is_recording.lock().unwrap_or_else(|e| e.into_inner());
                                if *is_recording {
                                    drop(is_recording);
                                    let mut mic = state.mic_recorder.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(mut r) = mic.take() { r.stop(); }
                                    let mut sys = state.system_recorder.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(mut r) = sys.take() { r.stop(); }
                                    let mut rt = state.realtime_transcriber.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(mut h) = rt.take() { h.stop(); }
                                    let mut mix = state.realtime_mixer.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(mut m) = mix.take() { m.stop(); }
                                }
                            }
                            state.wait_for_finalization();
                        }
                        app.exit(0);
                    }
                    _ => {}
                }
            }
        })
        .on_tray_icon_event(|tray: &tauri::tray::TrayIcon, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}
