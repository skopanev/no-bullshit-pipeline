import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { applyTheme } from '../ui/theme.js';
import { ViewManager } from '../ui/view-manager.js';
import { emit } from '../core/events.js';
import { applyTranscriptionSettings, collectTranscriptionSettings } from './transcription.js';
import { renderPeople } from './people.js';
import { applyDictationSettings, initShortcutsTab } from './shortcuts.js';
import { refreshModelVersion } from './model-version.js';

const storagePathInput = document.getElementById('settings-storage-path');
const saveMixOnlyCheckbox = document.getElementById('settings-save-mix-only');
const entryRetentionSelect = document.getElementById('settings-entry-retention');
const autoRecordCheckbox = document.getElementById('settings-auto-record');
const settingsBtn = document.getElementById('settings-btn');
const settingsBackBtn = document.getElementById('settings-back-btn');
const browseStorageBtn = document.getElementById('browse-storage-btn');
const themeButtons = document.querySelectorAll('.theme-btn');
const settingsTabs = document.getElementById('settings-tabs');
const settingsContainer = document.getElementById('settings-view');

// Last-saved ASR engine identity (provider + Qwen3 variant). The model-version
// check only matters when this changes — every other settings tweak (diarize,
// sensitivity, …) leaves the model untouched, so we must not spawn the sidecar
// or hit HuggingFace on those. Set on load, compared on save.
let lastEngineKey = null;

function engineKey() {
  const t = state.appSettings?.transcription || {};
  return `${t.provider}:${t.qwen3_variant || ''}`;
}

export async function loadSettings() {
  try {
    state.setAppSettings(await invoke('load_settings'));
    lastEngineKey = engineKey();
    if (storagePathInput) storagePathInput.value = state.appSettings.storage_path;

    applyTranscriptionSettings();
    renderPeople();

    if (saveMixOnlyCheckbox) saveMixOnlyCheckbox.checked = state.appSettings.save_mix_only !== false;
    if (entryRetentionSelect) {
      // Preserve a hand-edited value that isn't one of the presets (e.g. 7) by
      // injecting a matching option — otherwise the select would silently snap
      // to a preset and clobber it on the next save.
      const days = String(state.appSettings.entry_retention_days ?? 0);
      if (![...entryRetentionSelect.options].some((o) => o.value === days)) {
        const opt = document.createElement('option');
        opt.value = days;
        opt.textContent = days === '0' ? 'Never' : `${days} days`;
        entryRetentionSelect.appendChild(opt);
      }
      entryRetentionSelect.value = days;
    }
    if (autoRecordCheckbox) autoRecordCheckbox.checked = state.appSettings.auto_record_meetings !== false;

    applyDictationSettings();

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
    if (entryRetentionSelect) state.appSettings.entry_retention_days = parseInt(entryRetentionSelect.value, 10) || 0;
    if (autoRecordCheckbox) state.appSettings.auto_record_meetings = autoRecordCheckbox.checked;

    await invoke('save_settings', { settings: state.appSettings });
    showToast('Settings saved', 'success');

    // Re-check the model ONLY when the engine actually changed. force=false so
    // the 24h cache answers instantly when switching back to an engine we've
    // already checked — no redundant sidecar spawn / HuggingFace request on
    // every settings tweak or back-and-forth switch.
    const key = engineKey();
    if (key !== lastEngineKey) {
      lastEngineKey = key;
      refreshModelVersion(false);
    }
  } catch (err) {
    console.error('Failed to save settings:', err);
    showToast('Failed to save settings', 'error');
  }
}

export function switchSettingsTab(tabName) {
  document.querySelectorAll('.settings-tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tabName));
  document.querySelectorAll('.settings-tab-content').forEach(c => c.classList.toggle('active', c.dataset.tab === tabName));

  if (tabName === 'pipelines') emit('tab:pipelines');
  if (tabName === 'dictation') emit('tab:dictation');
  if (tabName === 'recording') emit('tab:recording');

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

  initShortcutsTab();
}
