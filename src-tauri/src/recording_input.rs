//! Recording input preference and read-only source/level indicators.
use crate::{AudioState, capture_coordinator, config, devices, mic_audio};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

#[derive(Serialize)]
pub(crate) struct InputStatus {
    selected: Option<String>,
    devices: Vec<devices::AudioDeviceInfo>,
    active_device: Option<String>,
    busy: bool,
    level: f32,
}

pub(crate) fn active_device(app: &tauri::AppHandle) -> Option<String> {
    app.state::<AudioState>()
        .mic_recorder
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|mic| mic.device_name().to_string())
}

#[tauri::command]
pub(crate) async fn get_recording_input(app: tauri::AppHandle) -> Result<InputStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(InputStatus {
            selected: config::load_settings().recording_device_name,
            devices: devices::list_input_devices().map_err(|e| e.to_string())?,
            active_device: active_device(&app),
            busy: !matches!(
                capture_coordinator::snapshot(),
                capture_coordinator::CaptureActivity::Idle
            ),
            level: mic_audio::get_current_audio_level(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn set_recording_microphone(
    app: tauri::AppHandle,
    device_name: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let name = device_name.filter(|name| !name.is_empty());
        // Save for the NEXT recording. Never re-route an active capture.
        devices::recording_device(name.as_deref())?;
        config::set_recording_device(name.clone())?;
        let _ = app.emit("recording-microphone-changed", name);
        crate::refresh_tray_menu(&app);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

static TRAY_MIC: Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>> = Mutex::new(None);

pub(crate) fn tray_active_item(
    app: &tauri::AppHandle,
) -> tauri::Result<tauri::menu::MenuItem<tauri::Wry>> {
    let item = tauri::menu::MenuItemBuilder::with_id("recording-mic-active", active_label(app))
        .enabled(false)
        .build(app)?;
    *TRAY_MIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(item.clone());
    Ok(item)
}

fn active_label(app: &tauri::AppHandle) -> String {
    let name = active_device(app).unwrap_or_else(|| "Microphone unavailable".into());
    let level = mic_audio::get_current_audio_level();
    let signal = if level < 0.001 {
        "No mic signal".into()
    } else {
        format!("{:.0} dBFS", 20.0 * level.log10())
    };
    format!("Mic: {} · {signal}", name.replace('&', "&&"))
}

pub(crate) fn update_tray_meter(app: &tauri::AppHandle) {
    if matches!(
        capture_coordinator::snapshot(),
        capture_coordinator::CaptureActivity::Recording { .. }
    ) {
        if let Some(item) = TRAY_MIC.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = item.set_text(active_label(app));
        }
    } else {
        TRAY_MIC.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Read-only even when idle: selection belongs exclusively to Settings.
pub(crate) fn tray_source_item(
    app: &tauri::AppHandle,
) -> tauri::Result<tauri::menu::MenuItem<tauri::Wry>> {
    let selected = config::load_settings().recording_device_name;
    let devices = devices::list_input_devices().unwrap_or_default();
    let name = selected
        .as_deref()
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.is_default)
                .map(|device| device.name.as_str())
        })
        .unwrap_or("unavailable");
    let unavailable = !devices.iter().any(|device| device.name == name);
    tauri::menu::MenuItemBuilder::with_id(
        "recording-mic-source",
        format!(
            "Mic: {}{}",
            name.replace('&', "&&"),
            if unavailable { " (unavailable)" } else { "" }
        ),
    )
    .enabled(false)
    .build(app)
}
