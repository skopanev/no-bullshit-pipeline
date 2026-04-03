// integrations/local-models.js — Local LLM model download/delete/select UI + progress

import { invoke, listen } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { appSettings, setAppSettings } from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { showConfirm } from '../ui/confirm-modal.js';
import * as intState from './state.js';

export async function renderLocalLlmModels() {
  try {
    intState.setLlmModelsData(await invoke('get_llm_models_info'));
  } catch (err) {
    console.error('Failed to load LLM models:', err);
    intState.setLlmModelsData([]);
  }
  // Load cached freshness results
  try {
    const cached = await invoke('get_cached_freshness_results');
    if (cached && typeof cached === 'object') {
      const fresh = {};
      for (const [modelId, hasUpdate] of Object.entries(cached)) {
        fresh[modelId] = { status: hasUpdate ? 'update_available' : 'up_to_date' };
      }
      intState.setLlmFreshnessData(fresh);
    }
  } catch (_) { /* cached results are optional */ }
  renderLocalLlmModelsInner();
}

function setDownloadUi(card, downloading) {
  if (!card) return;
  const cancelBtn = card.querySelector('.llm-cancel-btn');
  card.querySelectorAll('.provider-card-input button').forEach(btn => {
    if (btn.classList.contains('llm-cancel-btn')) return;
    btn.style.display = downloading ? 'none' : '';
  });
  if (cancelBtn) {
    cancelBtn.style.display = downloading ? '' : 'none';
    cancelBtn.disabled = false;
    cancelBtn.textContent = 'Cancel';
  }
}

export function renderLocalLlmModelsInner() {
  const el = document.getElementById('local-llm-models-list');
  if (!el) return;

  const selectedId = appSettings?.local_llm?.model_id || null;

  el.innerHTML = intState.llmModelsData.map(m => {
    const isSelected = m.id === selectedId;
    const sizeStr = m.size_mb >= 1000 ? `${(m.size_mb / 1000).toFixed(1)} GB` : `${m.size_mb} MB`;
    const freshness = intState.llmFreshnessData[m.id];
    const hasUpdate = freshness?.status === 'update_available';
    const statusBadge = m.downloaded
      ? `<span class="llm-status-badge llm-status-downloaded">Downloaded</span>`
      : `<span class="llm-status-badge llm-status-not-downloaded">Not downloaded</span>`;

    return `
      <div class="provider-card${isSelected ? ' llm-selected' : ''}" data-llm-id="${escapeHtml(m.id)}" style="cursor:pointer;flex-wrap:wrap;">
        <div class="provider-card-icon" style="background:rgba(139,92,246,0.15);display:flex;align-items:center;justify-content:center;font-size:14px;font-weight:700;color:var(--accent-color);">
          ${escapeHtml(m.params)}
        </div>
        <div class="provider-card-info" style="flex:1;">
          <div class="provider-card-name">
            ${escapeHtml(m.name)}
            ${statusBadge}
            ${isSelected ? '<span class="llm-status-badge llm-status-active">Active</span>' : ''}
            ${hasUpdate ? '<span class="llm-status-badge llm-status-update">Update available</span>' : ''}
          </div>
          <div class="provider-card-detail">${escapeHtml(m.desc)}</div>
          <div class="provider-card-detail" style="opacity:0.6;font-size:0.65rem;">${sizeStr} \u2022 Q4_K_M</div>
        </div>
        <div class="provider-card-input" style="gap:6px;">
          ${m.downloaded
            ? `<button class="mini-action-btn llm-select-btn${isSelected ? ' active' : ''}" data-llm-id="${escapeHtml(m.id)}" style="font-size:0.75rem;">${isSelected ? 'Active' : 'Select'}</button>
               <button class="mini-action-btn llm-update-btn${hasUpdate ? ' update-available' : ''}" data-llm-id="${escapeHtml(m.id)}" title="${hasUpdate ? 'Update available \u2014 download latest version' : 'Re-download latest version'}" style="width:34px;height:34px;padding:0;display:flex;align-items:center;justify-content:center;">
                 <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>
               </button>
               <button class="mini-action-btn llm-delete-btn" data-llm-id="${escapeHtml(m.id)}" title="Delete model" style="width:34px;height:34px;padding:0;display:flex;align-items:center;justify-content:center;">
                 <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="m19 6-.867 12.14A2 2 0 0 1 16.138 20H7.862a2 2 0 0 1-1.995-1.86L5 6m5 0V4a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v2"/></svg>
                </button>
                <button class="mini-action-btn llm-cancel-btn" data-llm-id="${escapeHtml(m.id)}" style="font-size:0.75rem;display:none;">Cancel</button>`
            : `<button class="mini-action-btn llm-download-btn" data-llm-id="${escapeHtml(m.id)}" style="font-size:0.75rem;">Download</button>
               <button class="mini-action-btn llm-cancel-btn" data-llm-id="${escapeHtml(m.id)}" style="font-size:0.75rem;display:none;">Cancel</button>`
          }
        </div>
        <div id="llm-progress-${escapeHtml(m.id)}" class="llm-progress-container">
          <div class="llm-progress-track">
            <div class="llm-progress-fill"></div>
          </div>
          <div class="llm-progress-text"><span class="llm-progress-percent">0%</span> \u2022 Preparing...</div>
        </div>
      </div>
    `;
  }).join('');

  wireDownloadBtns(el);
  wireCancelBtns(el);
  wireSelectBtns(el);
  wireUpdateBtns(el);
  wireDeleteBtns(el);
  restoreProgressState();
}

function wireDownloadBtns(el) {
  el.querySelectorAll('.llm-download-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const modelId = btn.dataset.llmId;
      const card = btn.closest('.provider-card');
      setDownloadUi(card, true);
      intState.activeDownloads[modelId] = { percent: 0, downloaded: 0, total: 0 };
      showProgress(modelId, 0, 'Preparing...');
      try {
        await invoke('download_llm_model', { modelId });
        delete intState.activeDownloads[modelId];
        await renderLocalLlmModels();
      } catch (err) {
        handleDownloadError(modelId, err, card);
      }
    });
  });
}

function wireCancelBtns(el) {
  el.querySelectorAll('.llm-cancel-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const modelId = btn.dataset.llmId;
      btn.disabled = true;
      btn.textContent = 'Cancelling...';
      try { await invoke('cancel_llm_download', { modelId }); } catch (_) {}
    });
  });
}

function wireSelectBtns(el) {
  el.querySelectorAll('.llm-select-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const modelId = btn.dataset.llmId;
      if (!appSettings.local_llm) appSettings.local_llm = {};
      appSettings.local_llm.model_id = modelId;
      appSettings.local_llm.enabled = true;
      await invoke('save_settings', { settings: appSettings });
      await renderLocalLlmModels();
    });
  });
}

function wireUpdateBtns(el) {
  el.querySelectorAll('.llm-update-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const modelId = btn.dataset.llmId;
      const model = intState.llmModelsData.find(m => m.id === modelId);
      const hasUpdate = intState.llmFreshnessData[modelId]?.status === 'update_available';
      const title = hasUpdate ? 'Update Model?' : 'Re-download Model?';
      const msg = hasUpdate
        ? `A newer version of ${model?.name || modelId} is available. Download it now?`
        : `Re-download ${model?.name || modelId}? This will replace the current file.`;
      const ok = await showConfirm(title, msg);
      if (!ok) return;
      const card = btn.closest('.provider-card');
      setDownloadUi(card, true);
      intState.activeDownloads[modelId] = { percent: 0, downloaded: 0, total: 0 };
      showProgress(modelId, 0, 'Preparing...');
      try {
        await invoke('delete_llm_model', { modelId });
        delete intState.llmFreshnessData[modelId];
        await invoke('download_llm_model', { modelId });
        delete intState.activeDownloads[modelId];
        await renderLocalLlmModels();
      } catch (err) {
        handleDownloadError(modelId, err, card);
      }
    });
  });
}

function wireDeleteBtns(el) {
  el.querySelectorAll('.llm-delete-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const modelId = btn.dataset.llmId;
      const model = intState.llmModelsData.find(m => m.id === modelId);
      const ok = await showConfirm('Delete Model?', `Delete ${model?.name || modelId}? The model file will be removed.`);
      if (!ok) return;
      try {
        await invoke('delete_llm_model', { modelId });
        delete intState.llmFreshnessData[modelId];
        setAppSettings(await invoke('load_settings'));
        await renderLocalLlmModels();
      } catch (err) {
        showToast('Delete failed: ' + err, 'error');
      }
    });
  });
}

function showProgress(modelId, pct, label) {
  const progressEl = document.getElementById(`llm-progress-${modelId}`);
  if (!progressEl) return;
  progressEl.classList.add('visible');
  progressEl.classList.remove('complete', 'error');
  const fill = progressEl.querySelector('.llm-progress-fill');
  if (fill) fill.style.width = `${pct}%`;
  const text = progressEl.querySelector('.llm-progress-text');
  if (text) text.innerHTML = `<span class="llm-progress-percent">${pct}%</span> \u2022 ${label}`;
}

function handleDownloadError(modelId, err, card) {
  const cancelled = String(err).includes('cancelled');
  intState.activeDownloads[modelId] = { error: cancelled ? 'Cancelled' : 'Download failed' };
  if (!cancelled) showToast('Download failed: ' + err, 'error');
  const progressEl = document.getElementById(`llm-progress-${modelId}`);
  if (progressEl) {
    progressEl.classList.add('visible', 'error');
    progressEl.classList.remove('complete');
    const text = progressEl.querySelector('.llm-progress-text');
    if (text) {
      text.innerHTML = cancelled
        ? '<span class="llm-progress-percent">Cancelled</span> \u2022 Download cancelled'
        : '<span class="llm-progress-percent">Error</span> \u2022 Download failed';
    }
  }
  setDownloadUi(card, false);
}

function restoreProgressState() {
  for (const [modelId, state] of Object.entries(intState.activeDownloads)) {
    const progressEl = document.getElementById(`llm-progress-${modelId}`);
    if (!progressEl) continue;
    const card = progressEl.closest('.provider-card');
    const cancelBtn = card ? card.querySelector('.llm-cancel-btn') : null;
    if (card) {
      card.querySelectorAll('.provider-card-input button').forEach(btn => {
        if (btn.classList.contains('llm-cancel-btn')) return;
        btn.style.display = (state.error || state.complete) ? '' : 'none';
      });
    }
    if (cancelBtn) {
      cancelBtn.style.display = (state.error || state.complete) ? 'none' : '';
      cancelBtn.disabled = false;
      cancelBtn.textContent = 'Cancel';
    }
    const fill = progressEl.querySelector('.llm-progress-fill');
    const text = progressEl.querySelector('.llm-progress-text');
    progressEl.classList.add('visible');
    progressEl.classList.remove('complete', 'error');
    if (state.error) {
      progressEl.classList.add('error');
      if (text) text.innerHTML = '<span class="llm-progress-percent">Error</span> \u2022 ' + state.error;
    } else if (state.complete) {
      progressEl.classList.add('complete');
      if (fill) fill.style.width = '100%';
      if (text) text.innerHTML = '<span class="llm-progress-percent">100%</span> \u2022 Download complete';
    } else {
      const pct = Math.min(100, Math.max(0, state.percent || 0)).toFixed(1);
      const dlMB = state.downloaded ? (state.downloaded / 1024 / 1024).toFixed(1) : '0';
      const totalMB = state.total ? (state.total / 1024 / 1024).toFixed(1) : '?';
      if (fill) fill.style.width = `${pct}%`;
      if (text) text.innerHTML = `<span class="llm-progress-percent">${pct}%</span> \u2022 ${dlMB} / ${totalMB} MB`;
    }
  }
}

// Event listeners for download progress and freshness
export function setupLocalModelListeners() {
  listen('llm_download_progress', (event) => {
    const { model_id, downloaded, total, percent } = event.payload;
    const progressEl = document.getElementById(`llm-progress-${model_id}`);
    if (!progressEl) return;
    const card = progressEl.closest('.provider-card');
    const cancelBtn = card ? card.querySelector('.llm-cancel-btn') : null;
    const fill = progressEl.querySelector('.llm-progress-fill');
    const text = progressEl.querySelector('.llm-progress-text');
    const isComplete = percent >= 100 || (total > 0 && downloaded >= total);
    const pct = Math.min(100, Math.max(0, percent || 0)).toFixed(1);
    const dlMB = downloaded ? (downloaded / 1024 / 1024).toFixed(1) : '0';
    const totalMB = total ? (total / 1024 / 1024).toFixed(1) : '?';

    if (isComplete) {
      intState.activeDownloads[model_id] = { percent: 100, downloaded: total, total, complete: true };
      progressEl.classList.add('visible', 'complete');
      progressEl.classList.remove('error');
      if (card) {
        card.querySelectorAll('.provider-card-input button').forEach(btn => {
          if (btn.classList.contains('llm-cancel-btn')) return;
          btn.style.display = '';
        });
      }
      if (cancelBtn) cancelBtn.style.display = 'none';
      if (fill) fill.style.width = '100%';
      if (text) text.innerHTML = '<span class="llm-progress-percent">100%</span> \u2022 Download complete';
    } else {
      intState.activeDownloads[model_id] = { percent, downloaded, total };
      progressEl.classList.add('visible');
      progressEl.classList.remove('complete', 'error');
      if (card) {
        card.querySelectorAll('.provider-card-input button').forEach(btn => {
          if (btn.classList.contains('llm-cancel-btn')) return;
          btn.style.display = 'none';
        });
      }
      if (cancelBtn) {
        cancelBtn.style.display = '';
        cancelBtn.disabled = false;
        cancelBtn.textContent = 'Cancel';
      }
      if (fill) fill.style.width = `${pct}%`;
      if (text) text.innerHTML = `<span class="llm-progress-percent">${pct}%</span> \u2022 ${dlMB} / ${totalMB} MB`;
    }
  });

  listen('llm_freshness_progress', (event) => {
    const { model_name, current, total } = event.payload;
    const statusEl = document.getElementById('llm-freshness-status');
    if (statusEl) statusEl.textContent = `Checking ${model_name} (${current}/${total})\u2026`;
  });

  listen('model_freshness_auto_result', (event) => {
    const results = event.payload;
    if (!Array.isArray(results)) return;
    const fresh = {};
    for (const info of results) {
      fresh[info.model_id] = { status: info.update_available ? 'update_available' : 'up_to_date' };
    }
    intState.setLlmFreshnessData(fresh);
    renderLocalLlmModelsInner();
  });
}
