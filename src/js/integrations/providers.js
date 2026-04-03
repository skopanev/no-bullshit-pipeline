// integrations/providers.js — Cloud/local provider card rendering and API key management

import { invoke } from '../core/tauri.js';
import { escapeHtml, maskApiKey, isKeyMasked } from '../core/utils.js';
import { appSettings } from '../core/state.js';
import { showToast } from '../ui/toast.js';
import * as intState from './state.js';
import { renderLocalLlmModelsInner } from './local-models.js';

export const CLOUD_PROVIDERS = [
  { id: 'openai',    name: 'OpenAI',    desc: 'GPT-4o, Whisper, real-time transcription', placeholder: 'sk-...',     icon: 'assets/openai.svg'    },
  { id: 'google',    name: 'Google AI', desc: 'Gemini long-context processing',           placeholder: 'AIza...',    icon: 'assets/gemini.svg'    },
  { id: 'anthropic', name: 'Anthropic', desc: 'Claude structured extraction',             placeholder: 'sk-ant-...', icon: 'assets/anthropic.svg' },
];

export const LOCAL_PROVIDERS = [
  { id: 'local',  name: 'Local LLM', desc: 'GGUF models stored on device', icon: 'assets/local-llm.svg' },
  { id: 'ollama', name: 'Ollama',    desc: 'Local inference via Ollama',    icon: 'assets/ollama.svg'    },
];

const CAP_BADGE_COLORS = {
  'Transcription': 'rgba(16,185,129,0.18)',
  'Processing':    'rgba(59,130,246,0.18)',
  'Embedding':     'rgba(168,85,247,0.18)',
};

export function renderCapBadges(capabilities) {
  return capabilities.map(c => {
    const bg = CAP_BADGE_COLORS[c] || 'rgba(148,163,184,0.15)';
    return `<span class="provider-cap-badge" style="background:${bg}">${escapeHtml(c)}</span>`;
  }).join('');
}

export async function validateApiKey(provider, key) {
  try {
    if (provider === 'openai') {
      const r = await fetch('https://api.openai.com/v1/models', {
        headers: { 'Authorization': `Bearer ${key}` }
      });
      return r.ok;
    } else if (provider === 'google') {
      const r = await fetch(`https://generativelanguage.googleapis.com/v1beta/models?key=${encodeURIComponent(key)}`);
      return r.ok;
    } else if (provider === 'anthropic') {
      const r = await fetch('https://api.anthropic.com/v1/models', {
        headers: { 'x-api-key': key, 'anthropic-version': '2023-06-01' }
      });
      return r.ok;
    }
  } catch { return false; }
  return false;
}

export function renderModelsProviders() {
  const el = document.getElementById('models-providers-list');
  if (!el) return;

  const apiKeys = (appSettings && appSettings.transcription && appSettings.transcription.api_keys) || {};
  const providerConfigs = (appSettings && appSettings.providers) || {};

  // Commercial Models
  const cloudItems = [];
  for (const p of CLOUD_PROVIDERS) {
    const config = providerConfigs[p.id] || {};
    const caps = config.capabilities || [];
    const key = apiKeys[p.id] || '';
    const hasKey = !!key;
    const displayValue = hasKey ? maskApiKey(key) : '';
    const validatedKeys = window.__nbpValidatedKeys || {};
    const failedKeys = window.__nbpFailedKeys || {};
    const isValidated = validatedKeys[p.id] === key && !!key;
    const isFailed = failedKeys[p.id] === key;
    const keyStatus = !hasKey ? 'missing' : isFailed ? 'failed' : isValidated ? 'valid' : 'saved';
    const btnLabel = hasKey ? 'Saved' : 'Save';

    cloudItems.push(`
      <div class="settings-item" data-provider-section="${escapeHtml(p.id)}" style="display:flex;align-items:center;gap:12px;">
        <div class="provider-card-icon ${escapeHtml(p.id)}" style="flex-shrink:0;">
          <img src="${escapeHtml(p.icon)}" style="width:28px;height:28px;display:block;" alt="${escapeHtml(p.name)}" />
        </div>
        <div style="display:flex;flex-direction:column;flex:1;min-width:0;">
          <div style="font-weight:600;font-size:0.85rem;">${escapeHtml(p.name)} ${renderCapBadges(caps)}</div>
          <div style="font-size:0.72rem;color:var(--text-secondary);">${escapeHtml(p.desc)}</div>
        </div>
        <input
          id="settings-api-key-${escapeHtml(p.id)}"
          type="password"
          placeholder="${escapeHtml(p.placeholder)}"
          class="settings-input-text"
          value="${escapeHtml(displayValue)}"
          data-original-key="${escapeHtml(key)}"
          style="flex:0 1 220px;min-width:120px;"
        />
        <button class="mini-action-btn provider-save-btn" data-provider="${escapeHtml(p.id)}">${btnLabel}</button>
      </div>
    `);
  }

  // On-device Models
  const ollamaConfig = providerConfigs['ollama'] || {};
  const ollamaBaseUrl = ollamaConfig.base_url || 'http://localhost:11434';

  const html = `
    <div class="settings-section">
      <h3>Commercial Models</h3>
      ${cloudItems.join('')}
    </div>
    <div class="settings-section">
      <h3>On-device Models</h3>
      <div class="settings-item" style="flex-direction:column;align-items:stretch;gap:10px;">
        <div style="display:flex;align-items:center;gap:8px;">
          <span style="font-weight:600;font-size:0.85rem;">Ollama Host</span>
          <input id="settings-ollama-host" type="text" class="settings-input-text" value="${escapeHtml(ollamaBaseUrl)}" placeholder="http://localhost:11434" style="flex:0 1 220px;min-width:120px;" />
          <button class="mini-action-btn save-ollama-host-btn">Save Host</button>
        </div>
      </div>
      <div id="ollama-provider-container"></div>
      <div id="local-llm-models-list" style="display:flex;flex-direction:column;gap:8px;"></div>
      <div style="display:flex;align-items:center;gap:8px;margin-top:6px;">
        <button id="llm-check-freshness-btn" class="mini-action-btn" style="font-size:0.75rem;">Check for Updates</button>
        <span id="llm-freshness-status" style="font-size:0.7rem;color:var(--text-secondary);"></span>
      </div>
      <p style="font-size:0.72rem;color:var(--text-secondary);opacity:0.7;margin:4px 0 0;">
        Location: <span class="mono-font">~/.nbp/models/llm/</span>
      </p>
    </div>
  `;

  el.innerHTML = html;
  wireOllamaHostBtn(el);
  wireProviderSaveBtns(el);
  wireFreshnessBtn(el);
  renderLocalLlmModelsInner();
}

// Backward-compat alias
export function renderProcessingProviders() { renderModelsProviders(); }

function wireOllamaHostBtn(el) {
  const saveOllamaHostBtn = el.querySelector('.save-ollama-host-btn');
  if (!saveOllamaHostBtn) return;
  saveOllamaHostBtn.addEventListener('click', async () => {
    const input = document.getElementById('settings-ollama-host');
    if (!input) return;
    const raw = input.value.trim();
    const withScheme = raw === ''
      ? 'http://localhost:11434'
      : (raw.startsWith('http://') || raw.startsWith('https://') ? raw : `http://${raw}`);
    const normalized = withScheme.replace(/\/+$/, '');

    if (!appSettings.providers) appSettings.providers = {};
    if (!appSettings.providers.ollama) appSettings.providers.ollama = {};
    appSettings.providers.ollama.base_url = normalized;

    saveOllamaHostBtn.disabled = true;
    saveOllamaHostBtn.textContent = 'Saving...';
    try {
      await invoke('save_settings', { settings: appSettings });
      input.value = normalized;
      showToast('Ollama host saved', 'success');
    } catch (err) {
      showToast('Failed to save Ollama host: ' + err, 'error');
    } finally {
      saveOllamaHostBtn.disabled = false;
      saveOllamaHostBtn.textContent = 'Save Host';
    }
  });
}

function wireProviderSaveBtns(el) {
  el.querySelectorAll('.provider-save-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      const providerId = btn.dataset.provider;
      const input = document.getElementById(`settings-api-key-${providerId}`);
      if (!input) return;

      const value = input.value.trim();
      if (isKeyMasked(value)) return;

      if (!appSettings.transcription) appSettings.transcription = {};
      if (!appSettings.transcription.api_keys) appSettings.transcription.api_keys = {};
      appSettings.transcription.api_keys[providerId] = value || null;
      if (!appSettings.providers) appSettings.providers = {};
      if (!appSettings.providers[providerId]) appSettings.providers[providerId] = {};
      appSettings.providers[providerId].api_key = value || null;

      btn.disabled = true;
      btn.textContent = '...';
      try {
        await invoke('save_settings', { settings: appSettings });
        if (typeof window.updateTranscriptionKeyStatusDot === 'function') {
          window.updateTranscriptionKeyStatusDot();
        }

        if (value) {
          btn.textContent = 'Checking...';
          const valid = await validateApiKey(providerId, value);
          if (!window.__nbpValidatedKeys) window.__nbpValidatedKeys = {};
          if (!window.__nbpFailedKeys) window.__nbpFailedKeys = {};
          if (valid) {
            window.__nbpValidatedKeys[providerId] = value;
            delete window.__nbpFailedKeys[providerId];
          } else {
            delete window.__nbpValidatedKeys[providerId];
            window.__nbpFailedKeys[providerId] = value;
            showToast(`${providerId} key verification failed`, 'error');
          }
        }
      } catch (err) {
        showToast('Failed to save: ' + err, 'error');
      } finally {
        btn.disabled = false;
        const savedValue = appSettings.transcription?.api_keys?.[providerId]
          || appSettings.providers?.[providerId]?.api_key;
        btn.textContent = savedValue ? 'Saved' : 'Save';
      }
    });
  });
}

function wireFreshnessBtn(el) {
  const freshnessBtn = el.querySelector('#llm-check-freshness-btn');
  if (!freshnessBtn) return;
  freshnessBtn.addEventListener('click', async () => {
    const statusEl = document.getElementById('llm-freshness-status');
    freshnessBtn.disabled = true;
    freshnessBtn.innerHTML = '<span class="btn-spinner"></span> Checking\u2026';
    freshnessCheckRunning = true;
    if (statusEl) statusEl.textContent = '';
    try {
      const report = await invoke('check_all_llm_freshness');
      intState.setLlmFreshnessData(report.models || {});
      const updateCount = Object.values(intState.llmFreshnessData)
        .filter(v => v.status === 'update_available').length;
      if (report.failed > 0 && report.checked === 0) {
        showToast('Could not check for updates \u2014 network error', 'error');
        if (statusEl) statusEl.textContent = `${report.failed} model(s) could not be checked`;
      } else if (report.failed > 0) {
        const msg = updateCount > 0
          ? `${updateCount} update(s) available, ${report.failed} could not be checked`
          : `${report.checked} checked, ${report.failed} could not be checked`;
        if (statusEl) statusEl.textContent = msg;
      } else if (updateCount > 0) {
        if (statusEl) statusEl.textContent = `${updateCount} update(s) available`;
      } else {
        showToast('All models are up to date', 'success');
        if (statusEl) statusEl.textContent = 'All up to date';
      }
      renderLocalLlmModelsInner();
    } catch (err) {
      if (String(err).includes('cancelled')) {
        if (statusEl) statusEl.textContent = '';
      } else {
        showToast('Freshness check failed: ' + err, 'error');
      }
    } finally {
      freshnessCheckRunning = false;
      freshnessBtn.disabled = false;
      freshnessBtn.textContent = 'Check for Updates';
    }
  });
}

// Module-level freshness check state
let freshnessCheckRunning = false;

export function cancelFreshnessCheck() {
  if (!freshnessCheckRunning) return;
  invoke('cancel_llm_freshness');
}
