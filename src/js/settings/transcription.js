// Transcription Settings (Audio tab) — provider selection + cloud key status.
// Local Whisper has been removed; FluidAudio is the only on-device option.
// OpenAI/Google/Anthropic stay as cloud providers via API keys.

import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { isKeyMasked } from '../core/utils.js';
import { showToast } from '../ui/toast.js';
import { repoToName } from './model-version.js';

const transcriptionProviderSelect = document.getElementById('settings-transcription-provider');
const keepModelReadyCheckbox = document.getElementById('settings-keep-model-ready');
const diarizeCheckbox = document.getElementById('settings-diarize');
const translitLangSelect = document.getElementById('settings-translit-lang');
const translitThresholdInput = document.getElementById('settings-translit-threshold');
const translitMinLenInput = document.getElementById('settings-translit-min-len');
const vocabTextarea = document.getElementById('settings-vocab');
const vocabSaveBtn = document.getElementById('settings-vocab-save');
const qwen3VariantSelect = document.getElementById('settings-qwen3-variant');
const appleLocaleSelect = document.getElementById('settings-apple-locale');

const PROVIDER_KEY_MAP = { OpenAI: 'openai', Google: 'google' };

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

/// Show only the settings the selected engine actually supports. Each option
/// declares its engines via `data-engines` (the single source — mirrors the
/// per-engine flag wiring in transcription.rs). A section left with no visible
/// items is hidden too, so we never strand a lone header.
function applyEngineCapabilities(provider) {
  const tab = document.querySelector('.settings-tab-content[data-tab="asr"]');
  if (!tab) return;
  tab.querySelectorAll('[data-engines]').forEach((el) => {
    const ok = el.dataset.engines.split(',').map((s) => s.trim()).includes(provider);
    el.style.display = ok ? '' : 'none';
  });
  const shown = [];
  tab.querySelectorAll('.settings-section').forEach((sec) => {
    const items = sec.querySelectorAll('.settings-item');
    if (!items.length) return;
    const anyVisible = [...items].some((it) => it.style.display !== 'none');
    sec.style.display = anyVisible ? '' : 'none';
    if (anyVisible) shown.push(sec.querySelector('h3')?.textContent || '?');
  });
  console.debug(`[asr-ui] capabilities provider=${provider} sections=[${shown.join(', ')}]`);
}

/// Within the (Parakeet-only) Custom vocabulary group, sensitivity + word list
/// only matter when recovery is on — hide them when it's "Off". Also respects
/// the engine gate, so they never reappear for a non-Parakeet engine.
function applyRecoveryVisibility(provider) {
  const off = !translitLangSelect || translitLangSelect.value === 'off';
  const tab = document.querySelector('.settings-tab-content[data-tab="asr"]');
  if (!tab) return;
  tab.querySelectorAll('[data-when-recovery]').forEach((el) => {
    const engines = (el.dataset.engines || '').split(',').map((s) => s.trim()).filter(Boolean);
    const engineOk = !engines.length || engines.includes(provider);
    el.style.display = engineOk && !off ? '' : 'none';
  });
}

export async function updateProviderVisibility() {
  if (!transcriptionProviderSelect) return;
  const provider = transcriptionProviderSelect.value;
  applyEngineCapabilities(provider);
  applyRecoveryVisibility(provider);
  const isCloud = provider !== 'FluidAudio' && provider !== 'AppleSpeech' && provider !== 'Qwen3';

  const statusEl = document.getElementById('cloud-provider-status');
  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (statusEl) statusEl.style.display = 'none';
  if (setKeyBtn) setKeyBtn.style.display = 'none';

  if (isCloud) updateTranscriptionKeyStatusDot();
}

/// Label the on-device engine options with their real model names, sourced from
/// FluidAudio's `Repo` enum (no hand-written copy, no drift). The repo id is a
/// compile-time constant, so this works before any model is downloaded. Apple
/// Speech is an OS framework with no HF repo — its label stays as-is.
async function labelModelOptions() {
  if (!transcriptionProviderSelect) return;
  let map;
  try {
    map = await invoke('list_asr_models');
  } catch (e) {
    console.warn('list_asr_models failed, keeping fallback labels:', e);
    return;
  }
  const setLabel = (value, repo) => {
    const name = repoToName(repo);
    const opt = transcriptionProviderSelect.querySelector(`option[value="${value}"]`);
    if (opt && name) opt.textContent = name;
  };
  setLabel('FluidAudio', map['parakeet-v3']);
  setLabel('Qwen3', map['qwen3']);
}

/// Hide Apple Speech option from any select that contains
/// `[data-needs-apple-speech="1"]` when running on macOS < 26 (or non-mac).
async function hideAppleSpeechIfUnavailable() {
  let supported = false;
  try { supported = await invoke('has_apple_speech'); } catch (_e) { /* treat as unsupported */ }
  if (supported) return;
  document.querySelectorAll('option[data-needs-apple-speech="1"]').forEach((opt) => {
    opt.remove();
  });
  const appleLocaleRow = document.getElementById('apple-locale-row');
  if (appleLocaleRow) appleLocaleRow.style.display = 'none';
}

export function initTranscriptionSettings() {
  if (transcriptionProviderSelect) transcriptionProviderSelect.addEventListener('change', updateProviderVisibility);
  if (translitLangSelect) {
    translitLangSelect.addEventListener('change', () =>
      applyRecoveryVisibility(transcriptionProviderSelect?.value));
  }

  hideAppleSpeechIfUnavailable();
  labelModelOptions();

  if (vocabSaveBtn) {
    vocabSaveBtn.addEventListener('click', async () => {
      if (!vocabTextarea) return;
      const terms = vocabTextarea.value.split('\n').map((t) => t.trim()).filter(Boolean);
      try {
        await invoke('save_vocab', { terms });
        showToast('Words saved', 'success');
      } catch (e) {
        console.error('save_vocab failed:', e);
        showToast('Failed to save words', 'error');
      }
    });
  }

  // set-api-key-btn used to route to the deleted Models tab. Cloud STT was
  // killed in the asr-bakeoff branch (on-device only — see memory
  // `asr-code-switching`), so the button is permanently hidden via
  // `display:none` in the HTML and no longer has a meaningful target.
}

export function applyTranscriptionSettings() {
  if (!state.appSettings?.transcription) return;
  if (transcriptionProviderSelect) transcriptionProviderSelect.value = state.appSettings.transcription.provider;
  if (keepModelReadyCheckbox) {
    keepModelReadyCheckbox.checked = state.appSettings.transcription.keep_model_ready === true;
  }
  if (diarizeCheckbox) diarizeCheckbox.checked = state.appSettings.transcription.diarize !== false;
  if (translitLangSelect) translitLangSelect.value = state.appSettings.transcription.translit_lang || 'ru';
  if (translitThresholdInput) translitThresholdInput.value = state.appSettings.transcription.translit_threshold ?? 0.72;
  if (translitMinLenInput) translitMinLenInput.value = state.appSettings.transcription.translit_min_len ?? 4;
  if (vocabTextarea) invoke('get_vocab').then((terms) => { vocabTextarea.value = (terms || []).join('\n'); }).catch(() => {});
  if (qwen3VariantSelect) qwen3VariantSelect.value = state.appSettings.transcription.qwen3_variant || 'f32';
  if (appleLocaleSelect) appleLocaleSelect.value = state.appSettings.transcription.apple_locale || 'en-US';
  updateProviderVisibility();
  updateTranscriptionProviderWarnings();
}

export function collectTranscriptionSettings() {
  if (!state.appSettings.transcription) state.appSettings.transcription = {};
  // Transcription is always on — the toggle was removed. FluidAudio (default)
  // runs on-device so this never fails offline.
  state.appSettings.transcription.enabled = true;
  state.appSettings.transcription.provider = transcriptionProviderSelect?.value || 'FluidAudio';
  state.appSettings.transcription.keep_model_ready = keepModelReadyCheckbox?.checked === true;
  state.appSettings.transcription.diarize = diarizeCheckbox ? diarizeCheckbox.checked : true;
  state.appSettings.transcription.translit_lang = translitLangSelect ? translitLangSelect.value : 'ru';
  if (translitThresholdInput) {
    const th = parseFloat(translitThresholdInput.value);
    state.appSettings.transcription.translit_threshold = Number.isFinite(th) ? th : 0.72;
  }
  if (translitMinLenInput) {
    const ml = parseInt(translitMinLenInput.value, 10);
    state.appSettings.transcription.translit_min_len = Number.isFinite(ml) ? ml : 4;
  }
  if (vocabTextarea) {
    const terms = vocabTextarea.value.split('\n').map((t) => t.trim()).filter(Boolean);
    invoke('save_vocab', { terms }).catch((e) => console.error('save_vocab failed:', e));
  }
  if (qwen3VariantSelect) state.appSettings.transcription.qwen3_variant = qwen3VariantSelect.value || 'f32';
  if (appleLocaleSelect) state.appSettings.transcription.apple_locale = appleLocaleSelect.value || 'en-US';

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
