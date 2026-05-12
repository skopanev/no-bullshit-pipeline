import { invoke, listen } from '../core/tauri.js';
import * as state from '../core/state.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { isKeyMasked } from '../core/utils.js';
import { showToast } from '../ui/toast.js';
import { DEFAULT_WHISPER_MODEL } from '../core/constants.js';

const transcriptionEnabledCheckbox = document.getElementById('settings-transcription-enabled');
const transcriptionDetailsEl = document.getElementById('transcription-details');
const transcriptionProviderSelect = document.getElementById('settings-transcription-provider');
const providerLocalSection = document.getElementById('provider-local-section');
const whisperModelSelect = document.getElementById('settings-whisper-model');
const downloadModelBtn = document.getElementById('download-model-btn');
const realtimeEnabledCheckbox = document.getElementById('settings-realtime-enabled');

let availableModels = [];

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
    const baseLabel = option.dataset.baseLabel || option.textContent.replace(/^\u26A0\uFE0F\s*/, '');
    option.dataset.baseLabel = baseLabel;
    option.textContent = apiKeys[keyId] ? baseLabel : `\u26A0\uFE0F ${baseLabel}`;
  }
}

function updateKeyStatusElement(el, statusState) {
  if (!el) return;
  const STATUS_CONFIG = {
    missing:  { class: 'key-missing',  label: 'No key',    icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>` },
    saved:    { class: 'key-saved',    label: 'Saved',     icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/></svg>` },
    valid:    { class: 'key-valid',    label: 'Verified',  icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>` },
    failed:   { class: 'key-failed',   label: 'Invalid',   icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>` },
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
  const statusState = !hasKey ? 'missing' : failedKeys[keyId] === key ? 'failed' : validatedKeys[keyId] === key && !!key ? 'valid' : 'saved';

  const statusEl = document.getElementById('cloud-provider-status');
  if (statusEl) { statusEl.style.display = ''; updateKeyStatusElement(statusEl, statusState); }
  updateTranscriptionProviderWarnings();

  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (setKeyBtn) setKeyBtn.style.display = hasKey ? 'none' : '';
}

export async function updateProviderVisibility() {
  if (!providerLocalSection) return;
  const provider = transcriptionProviderSelect.value;
  const isCloud = provider !== 'FluidAudio' && provider !== 'LocalWhisper';

  providerLocalSection.style.display = 'none';
  const statusEl = document.getElementById('cloud-provider-status');
  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (statusEl) statusEl.style.display = 'none';
  if (setKeyBtn) setKeyBtn.style.display = 'none';

  if (provider === 'LocalWhisper') {
    providerLocalSection.style.display = 'flex';
    await loadWhisperModelsAndState();
  } else if (isCloud) {
    updateTranscriptionKeyStatusDot();
  }
}

function prettyModelName(filename) {
  // ggml-large-v3-turbo.bin \u2192 Large v3-turbo
  let s = filename.replace(/^ggml-/, '').replace(/\.bin$/, '');
  s = s.replace(/-/g, ' ').replace(/\bv(\d)/, 'v$1');
  return s.charAt(0).toUpperCase() + s.slice(1);
}

function formatSize(bytes) {
  if (!bytes) return '';
  if (bytes >= 1024 * 1024 * 1024) return `~${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
  return `~${Math.round(bytes / 1024 / 1024)} MB`;
}

async function loadWhisperModelsAndState() {
  if (!whisperModelSelect) return;
  whisperModelSelect.disabled = true;
  try {
    availableModels = await invoke('list_whisper_models_remote', { limit: 10 });
    const currentVal = state.appSettings?.transcription?.whisper_model || DEFAULT_WHISPER_MODEL;
    whisperModelSelect.innerHTML = availableModels.map(m => {
      const sizeStr = formatSize(m.size_bytes);
      const statusIcon = m.downloaded ? '\u2713' : '\u2193';
      const label = `${statusIcon} ${prettyModelName(m.filename)} ${sizeStr}`.trim();
      return `<option value="${m.filename}">${label}</option>`;
    }).join('');
    // If the saved value isn't in the fresh list, keep it as a stale entry
    if (currentVal && !availableModels.some(m => m.filename === currentVal)) {
      whisperModelSelect.insertAdjacentHTML('beforeend',
        `<option value="${currentVal}">${prettyModelName(currentVal)} (current)</option>`);
    }
    whisperModelSelect.value = currentVal;
    updateDownloadButton();
  } catch (e) { console.error(e); }
  finally { whisperModelSelect.disabled = false; }
}

function getPiePath(cx, cy, r, percentage) {
  if (percentage >= 100) return `M ${cx}, ${cy} m -${r}, 0 a ${r},${r} 0 1,0 ${r * 2},0 a ${r},${r} 0 1,0 -${r * 2},0`;
  const startAngle = -Math.PI / 2;
  const angle = (percentage / 100) * 2 * Math.PI;
  const endAngle = startAngle + angle;
  const x1 = cx + r * Math.cos(startAngle), y1 = cy + r * Math.sin(startAngle);
  const x2 = cx + r * Math.cos(endAngle), y2 = cy + r * Math.sin(endAngle);
  const largeArc = percentage > 50 ? 1 : 0;
  return `M ${cx} ${cy} L ${x1} ${y1} A ${r} ${r} 0 ${largeArc} 1 ${x2} ${y2} Z`;
}

function updateDownloadButton() {
  if (!downloadModelBtn || !whisperModelSelect) return;
  if (downloadModelBtn.dataset.downloading === 'true') return;
  const model = availableModels.find(m => m.filename === whisperModelSelect.value);
  if (!model) return;

  if (model.downloaded) {
    downloadModelBtn.dataset.action = 'delete';
    downloadModelBtn.title = 'Delete Model';
    downloadModelBtn.classList.remove('mini-action-btn-primary');
    downloadModelBtn.style.color = 'var(--text-danger, #ff4d4d)';
    downloadModelBtn.innerHTML = `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>`;
  } else {
    downloadModelBtn.dataset.action = 'download';
    downloadModelBtn.title = 'Download Model';
    downloadModelBtn.classList.add('mini-action-btn-primary');
    downloadModelBtn.style.color = 'var(--accent)';
    downloadModelBtn.innerHTML = `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`;
  }
}

export function initTranscriptionSettings() {
  if (transcriptionEnabledCheckbox) transcriptionEnabledCheckbox.addEventListener('change', updateTranscriptionVisibility);
  if (transcriptionProviderSelect) transcriptionProviderSelect.addEventListener('change', updateProviderVisibility);
  if (whisperModelSelect) whisperModelSelect.addEventListener('change', updateDownloadButton);

  if (downloadModelBtn) {
    downloadModelBtn.addEventListener('click', async () => {
      if (!whisperModelSelect) return;
      const size = whisperModelSelect.value; // filename like "ggml-base.bin"
      const action = downloadModelBtn.dataset.action;
      if (downloadModelBtn.dataset.downloading === 'true') return;

      if (action === 'delete') {
        const ok = await showConfirm('Delete Model?', `Delete ${prettyModelName(size)}?`);
        if (!ok) return;
        try { await invoke('delete_whisper_model', { size }); await loadWhisperModelsAndState(); } catch (err) { console.error('Delete failed:', err); }
        return;
      }

      downloadModelBtn.dataset.downloading = 'true';
      downloadModelBtn.innerHTML = `<svg class="progress-pie" width="24" height="24" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" stroke="var(--border)" stroke-width="1" fill="none" opacity="0.5"/><path class="progress-pie__slice" fill="var(--accent)" d="" /></svg>`;

      try {
        const unlisten = await listen('download_progress', (event) => {
          const slice = downloadModelBtn.querySelector('.progress-pie__slice');
          if (slice) slice.setAttribute('d', getPiePath(12, 12, 10, event.payload.percent));
        });
        await invoke('download_whisper_model', { size });
        unlisten();
        downloadModelBtn.dataset.downloading = 'false';
        await loadWhisperModelsAndState();
      } catch (err) {
        console.error('Download failed:', err);
        downloadModelBtn.dataset.downloading = 'false';
        updateDownloadButton();
      }
    });
  }

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
  if (whisperModelSelect) whisperModelSelect.value = state.appSettings.transcription.whisper_model || DEFAULT_WHISPER_MODEL;
  if (realtimeEnabledCheckbox) realtimeEnabledCheckbox.checked = !!state.appSettings.transcription.realtime_enabled;
  updateProviderVisibility();
  updateTranscriptionProviderWarnings();
}

export function collectTranscriptionSettings() {
  if (!state.appSettings.transcription) state.appSettings.transcription = {};
  state.appSettings.transcription.enabled = transcriptionEnabledCheckbox?.checked || false;
  state.appSettings.transcription.provider = transcriptionProviderSelect?.value || 'FluidAudio';
  state.appSettings.transcription.whisper_model = whisperModelSelect?.value || DEFAULT_WHISPER_MODEL;
  state.appSettings.transcription.realtime_enabled = realtimeEnabledCheckbox?.checked || false;

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
}
