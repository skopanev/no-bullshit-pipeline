// Transcription Settings (Audio tab) — provider selection + cloud key status.
// Local Whisper has been removed; FluidAudio is the only on-device option.
// OpenAI/Google/Anthropic stay as cloud providers via API keys.

import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { isKeyMasked } from '../core/utils.js';

const transcriptionEnabledCheckbox = document.getElementById('settings-transcription-enabled');
const transcriptionDetailsEl = document.getElementById('transcription-details');
const transcriptionProviderSelect = document.getElementById('settings-transcription-provider');

const PROVIDER_KEY_MAP = { OpenAI: 'openai', Google: 'google' };

export function updateTranscriptionVisibility() {
  if (!transcriptionDetailsEl) return;
  transcriptionDetailsEl.style.display = transcriptionEnabledCheckbox?.checked ? 'flex' : 'none';
}

export function updateTranscriptionProviderWarnings() {
  if (!transcriptionProviderSelect) return;
  const apiKeys = state.appSettings?.transcription?.api_keys || {};
  for (const option of transcriptionProviderSelect.options) {
    const keyId = PROVIDER_KEY_MAP[option.value];
    if (!keyId) continue;
    const baseLabel = option.dataset.baseLabel || option.textContent.replace(/^⚠️\s*/, '');
    option.dataset.baseLabel = baseLabel;
    option.textContent = apiKeys[keyId] ? baseLabel : `⚠️ ${baseLabel}`;
  }
}

function updateKeyStatusElement(el, statusState) {
  if (!el) return;
  const STATUS_CONFIG = {
    missing: { class: 'key-missing', label: 'No key',   icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>' },
    saved:   { class: 'key-saved',   label: 'Saved',    icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/></svg>' },
    valid:   { class: 'key-valid',   label: 'Verified', icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>' },
    failed:  { class: 'key-failed',  label: 'Invalid',  icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>' },
  };
  const config = STATUS_CONFIG[statusState] || STATUS_CONFIG.missing;
  el.className = `provider-key-status ${config.class}`;
  el.innerHTML = `${config.icon}<span>${config.label}</span>`;
}

export function updateTranscriptionKeyStatusDot() {
  const provider = transcriptionProviderSelect?.value || '';
  const keyId = PROVIDER_KEY_MAP[provider];
  if (!keyId) return;
  const apiKeys = state.appSettings?.transcription?.api_keys || {};
  const hasKey = !!apiKeys[keyId];
  const validatedKeys = window.__nbpValidatedKeys || {};
  const failedKeys = window.__nbpFailedKeys || {};
  const key = apiKeys[keyId] || '';
  const statusState = !hasKey ? 'missing'
    : failedKeys[keyId] === key ? 'failed'
    : validatedKeys[keyId] === key && !!key ? 'valid'
    : 'saved';
  const statusEl = document.getElementById('cloud-provider-status');
  if (statusEl) { statusEl.style.display = ''; updateKeyStatusElement(statusEl, statusState); }
  updateTranscriptionProviderWarnings();
  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (setKeyBtn) setKeyBtn.style.display = hasKey ? 'none' : '';
}

export async function updateProviderVisibility() {
  if (!transcriptionProviderSelect) return;
  const provider = transcriptionProviderSelect.value;
  const isCloud = provider !== 'FluidAudio';

  const statusEl = document.getElementById('cloud-provider-status');
  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (statusEl) statusEl.style.display = 'none';
  if (setKeyBtn) setKeyBtn.style.display = 'none';

  if (isCloud) updateTranscriptionKeyStatusDot();
}

export function initTranscriptionSettings() {
  if (transcriptionEnabledCheckbox) transcriptionEnabledCheckbox.addEventListener('change', updateTranscriptionVisibility);
  if (transcriptionProviderSelect) transcriptionProviderSelect.addEventListener('change', updateProviderVisibility);

  const setApiKeyBtn = document.getElementById('set-api-key-btn');
  if (setApiKeyBtn) {
    setApiKeyBtn.addEventListener('click', () => {
      const provider = transcriptionProviderSelect?.value || '';
      const keyId = PROVIDER_KEY_MAP[provider];
      if (typeof window.__nbpSwitchSettingsTab === 'function') window.__nbpSwitchSettingsTab('models');
      if (keyId) {
        setTimeout(() => {
          const input = document.getElementById(`settings-api-key-${keyId}`);
          if (input) { input.scrollIntoView({ behavior: 'smooth', block: 'center' }); input.focus(); }
        }, 100);
      }
    });
  }
}

export function applyTranscriptionSettings() {
  if (!state.appSettings?.transcription) return;
  if (transcriptionEnabledCheckbox) { transcriptionEnabledCheckbox.checked = state.appSettings.transcription.enabled; updateTranscriptionVisibility(); }
  if (transcriptionProviderSelect) transcriptionProviderSelect.value = state.appSettings.transcription.provider;
  updateProviderVisibility();
  updateTranscriptionProviderWarnings();
}

export function collectTranscriptionSettings() {
  if (!state.appSettings.transcription) state.appSettings.transcription = {};
  state.appSettings.transcription.enabled = transcriptionEnabledCheckbox?.checked || false;
  state.appSettings.transcription.provider = transcriptionProviderSelect?.value || 'FluidAudio';

  if (!state.appSettings.transcription.api_keys) state.appSettings.transcription.api_keys = {};
  if (!state.appSettings.providers) state.appSettings.providers = {};
  for (const providerId of ['openai', 'google', 'anthropic']) {
    const input = document.getElementById(`settings-api-key-${providerId}`);
    if (input && !isKeyMasked(input.value)) {
      const keyValue = input.value || null;
      state.appSettings.transcription.api_keys[providerId] = keyValue;
      if (!state.appSettings.providers[providerId]) state.appSettings.providers[providerId] = {};
      state.appSettings.providers[providerId].api_key = keyValue;
    }
  }

  // Drop Whisper-specific fields lingering from older configs.
  delete state.appSettings.transcription.whisper_model;
  delete state.appSettings.transcription.realtime_model;
}

// Re-export used by main.js / settings.js init wiring (kept for compat).
export async function initWhisperUI() { /* removed */ }
