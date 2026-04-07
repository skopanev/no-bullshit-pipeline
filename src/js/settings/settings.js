import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { applyTheme } from '../ui/theme.js';
import { ViewManager } from '../ui/view-manager.js';
import { emit } from '../core/events.js';
import { applyTranscriptionSettings, collectTranscriptionSettings } from './transcription.js';

const storagePathInput = document.getElementById('settings-storage-path');
const saveMixOnlyCheckbox = document.getElementById('settings-save-mix-only');
const callDetectionCheckbox = document.getElementById('settings-call-detection');
const autoStopSilenceSelect = document.getElementById('settings-auto-stop-silence');
const settingsBtn = document.getElementById('settings-btn');
const settingsBackBtn = document.getElementById('settings-back-btn');
const browseStorageBtn = document.getElementById('browse-storage-btn');
const themeButtons = document.querySelectorAll('.theme-btn');
const settingsTabs = document.getElementById('settings-tabs');
const settingsContainer = document.getElementById('settings-view');

export async function loadSettings() {
  try {
    state.setAppSettings(await invoke('load_settings'));
    if (storagePathInput) storagePathInput.value = state.appSettings.storage_path;

    applyTranscriptionSettings();

    if (typeof window.__nbpRenderModelsProviders === 'function') window.__nbpRenderModelsProviders();

    if (saveMixOnlyCheckbox) saveMixOnlyCheckbox.checked = state.appSettings.save_mix_only !== false;
    if (callDetectionCheckbox) callDetectionCheckbox.checked = !!state.appSettings.call_detection_enabled;
    if (autoStopSilenceSelect) autoStopSilenceSelect.value = String(state.appSettings.auto_stop_silence_seconds || 0);

    applyTheme(state.appSettings.theme);
  } catch (err) {
    console.error('Failed to load settings:', err);
  }
}

export async function saveSettings() {
  try {
    state.appSettings.auto_discard_seconds = 3;
    collectTranscriptionSettings();

    if (saveMixOnlyCheckbox) state.appSettings.save_mix_only = saveMixOnlyCheckbox.checked;
    if (callDetectionCheckbox) state.appSettings.call_detection_enabled = callDetectionCheckbox.checked;
    if (autoStopSilenceSelect) state.appSettings.auto_stop_silence_seconds = parseInt(autoStopSilenceSelect.value) || 0;

    await invoke('save_settings', { settings: state.appSettings });
    showToast('Settings saved', 'success');
  } catch (err) {
    console.error('Failed to save settings:', err);
    showToast('Failed to save settings', 'error');
  }
}

export function switchSettingsTab(tabName) {
  document.querySelectorAll('.settings-tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tabName));
  document.querySelectorAll('.settings-tab-content').forEach(c => c.classList.toggle('active', c.dataset.tab === tabName));

  if (tabName === 'pipelines') emit('tab:pipelines');
  if (tabName === 'prompts') emit('tab:prompts');

  const scroller = document.querySelector('#settings-view .detail-scroller');
  if (scroller) scroller.scrollTop = 0;
}

export function initSettingsListeners() {
  if (settingsBtn) settingsBtn.addEventListener('click', () => ViewManager.showSettings());
  if (settingsBackBtn) settingsBackBtn.addEventListener('click', () => ViewManager.showRecordings());

  document.querySelectorAll('.sidebar-nav-item').forEach(item => {
    item.addEventListener('click', () => {
      const view = item.dataset.view;
      if (view === 'recordings') ViewManager.showRecordings();
      else if (view === 'prompts') ViewManager.showPrompts();
      else if (view === 'pipelines') ViewManager.showPipelines();
      else if (view === 'settings') ViewManager.showSettings();
    });
  });

  if (settingsContainer) settingsContainer.addEventListener('change', () => saveSettings());
  if (settingsTabs) settingsTabs.addEventListener('click', (e) => {
    const tab = e.target.closest('.settings-tab');
    if (tab) switchSettingsTab(tab.dataset.tab);
  });

  if (browseStorageBtn) {
    browseStorageBtn.addEventListener('click', async () => {
      try {
        const selected = await window.__TAURI__.dialog.open({ directory: true, multiple: false, defaultPath: state.appSettings.storage_path });
        if (selected) { state.appSettings.storage_path = selected; storagePathInput.value = selected; }
      } catch (err) { console.error('Failed to browse:', err); }
    });
  }

  themeButtons.forEach(btn => {
    btn.addEventListener('click', () => { applyTheme(btn.dataset.theme); saveSettings(); });
  });
}
