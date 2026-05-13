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
  // Background HEAD-probe HF for fresh remote sizes (in bytes). Result lands
  // in the on-disk cache; re-pull get_llm_models_info to get the recomputed
  // MB values + broken flags rather than parsing bytes here.
  (async () => {
    try {
      await invoke('refresh_llm_model_sizes');
      const fresh = await invoke('get_llm_models_info');
      if (Array.isArray(fresh)) {
        intState.setLlmModelsData(fresh);
        renderLocalLlmModelsInner();
      }
    } catch (err) {
      console.warn('refresh_llm_model_sizes failed', err);
    }
  })();
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

  el.innerHTML = intState.llmModelsData.map(m => {
    const sizeStr = m.size_mb === 0
      ? '—'
      : (m.size_mb >= 1000 ? `${(m.size_mb / 1000).toFixed(1)} GB` : `${m.size_mb} MB`);
    const freshness = intState.llmFreshnessData[m.id];
    const hasUpdate = freshness?.status === 'update_available';
    // Download / Delete buttons already convey downloaded state — no badge.

    return `
      <div class="provider-card${m.orphan ? ' llm-orphan' : (m.broken ? ' llm-broken' : (m.downloaded ? ' llm-downloaded' : ''))}" data-llm-id="${escapeHtml(m.id)}" style="flex-wrap:wrap;">
        <div class="provider-card-icon" style="background:rgba(139,92,246,0.15);display:flex;align-items:center;justify-content:center;font-size:14px;font-weight:700;color:var(--accent-color);">
          ${escapeHtml(m.params)}
        </div>
        <div class="provider-card-info" style="flex:1;">
          <div class="provider-card-name">
            ${escapeHtml(m.name)}
            ${m.broken ? '<span class="llm-status-badge llm-status-broken">Incomplete</span>' : ''}
            ${m.orphan ? '<span class="llm-status-badge llm-status-orphan">Removed from catalog</span>' : ''}
            ${hasUpdate ? '<span class="llm-status-badge llm-status-update">Update available</span>' : ''}
          </div>
          <div class="provider-card-detail">
            ${escapeHtml(m.desc)}
            <span class="llm-card-sep">\u00b7</span>
            <span class="llm-card-size">${sizeStr} \u2022 Q4_K_M</span>
            ${(() => {
              // Strip /resolve/.../*.gguf \u2192 bare HF repo URL. The link can't
              // be a real <a target=_blank> \u2014 Tauri's webview swallows that.
              // We render data-href and let a delegated handler call the
              // opener plugin to open in the OS default browser.
              const repo = (m.url || '').replace(/\/resolve\/.*$/, '');
              return repo ? `<a class="llm-hf-link" href="javascript:void(0)" data-href="${escapeHtml(repo)}">Hugging Face \u2197</a>` : '';
            })()}
          </div>
        </div>
        <div class="provider-card-input" style="gap:6px;">
          ${(m.downloaded || m.broken || m.orphan)
            ? `<div class="llm-hover-actions">
                 <button class="mini-action-btn llm-reveal-btn" data-llm-path="${escapeHtml(m.path)}" title="Show in Finder" style="font-size:0.75rem;width:30px;height:28px;padding:0;display:flex;align-items:center;justify-content:center;">
                   <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                 </button>
                 <button class="mini-action-btn llm-delete-btn danger" data-llm-id="${escapeHtml(m.id)}" title="${m.broken ? 'File is incomplete — delete it to retry' : (m.orphan ? 'No longer in catalog — frees disk space' : 'Delete model file')}" style="font-size:0.75rem;">Delete</button>
               </div>
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
  wireUpdateBtns(el);
  wireDeleteBtns(el);
  wireHfLinks(el);
  wireRevealBtns(el);
  toggleFreshnessRow();
  restoreProgressState();
}

function wireRevealBtns(el) {
  el.querySelectorAll('.llm-reveal-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const path = btn.dataset.llmPath;
      if (!path) return;
      const opener = window.__TAURI__?.opener;
      if (!opener || typeof opener.openPath !== 'function') return;
      // Reveal the model file itself in Finder. macOS openPath on a file
      // opens it; on a directory it reveals the directory. For "reveal the
      // file with surrounding folder open", we hand the parent dir — the
      // file shows up selected when Finder opens it.
      const parent = path.replace(/\/[^/]*$/, '');
      try { await opener.openPath(parent); } catch (err) { console.error('reveal failed', err); }
    });
  });
}

// Hide just the Check for Updates button + status when no model is
// downloaded — but keep the Show in Finder button visible at all times
// (the folder exists from day one even if empty, and users may want to
// poke at it before any download).
function toggleFreshnessRow() {
  const anyDownloaded = (intState.llmModelsData || []).some(m => m.downloaded);
  const btn = document.getElementById('llm-check-freshness-btn');
  const status = document.getElementById('llm-freshness-status');
  if (btn) btn.style.display = anyDownloaded ? '' : 'none';
  if (status) {
    status.textContent = '';
    status.style.display = anyDownloaded ? '' : 'none';
  }
}

function wireHfLinks(el) {
  el.querySelectorAll('.llm-hf-link').forEach(a => {
    a.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      const url = a.dataset.href;
      if (!url) return;
      // tauri-plugin-opener routes through the OS, so this lands in Safari /
      // Chrome / whatever the user has set as default — never inside the
      // webview. Logged-and-ignore on failure (very unlikely).
      const opener = window.__TAURI__?.opener;
      if (!opener || typeof opener.openUrl !== 'function') {
        console.error('opener plugin missing — cannot open', url);
        return;
      }
      opener.openUrl(url).catch((err) => console.error('opener.openUrl failed', err));
    });
  });
}

function wireDownloadBtns(el) {
  el.querySelectorAll('.llm-download-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const modelId = btn.dataset.llmId;
      const card = btn.closest('.provider-card');
      console.log('[llm] download click', modelId);
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
      const card = btn.closest('.provider-card');
      console.log('[llm] cancel click', modelId);

      // Visual: red flash on the card. While it plays, disable every button
      // inside so the user can't double-fire or click around. CSS also kills
      // pointer-events on the card itself as a belt-and-suspenders measure.
      // Once the animation ends → re-render → card flips back to idle
      // Download state.
      if (card) {
        card.classList.add('llm-cancel-flash');
        const buttons = card.querySelectorAll('button');
        buttons.forEach(b => { b.disabled = true; });
        // animationend bubbles — filter to our keyframe name so an unrelated
        // child animation (progress-fill gradient, meter bars, etc.) finishing
        // doesn't cut the flash short on the first cancel.
        const onEnd = (ev) => {
          if (ev.animationName !== 'llmCancelFlash') return;
          card.removeEventListener('animationend', onEnd);
          renderLocalLlmModels().catch(() => {});
        };
        card.addEventListener('animationend', onEnd);
      }

      // Mark this download as cancelling so the progress event listener
      // (which fires several times a second) stops resurrecting the Cancel
      // button and the progress bar each tick. Without this flag, the next
      // incoming progress event would `display=''` the Cancel button right
      // back after we hid it — and steal subsequent clicks.
      if (intState.activeDownloads[modelId]) {
        intState.activeDownloads[modelId].cancelling = true;
      }
      btn.style.display = 'none';
      const progressEl = document.getElementById(`llm-progress-${modelId}`);
      if (progressEl) {
        progressEl.classList.remove('visible', 'complete', 'error');
        // Belt + suspenders — kill any inline display the listener may have
        // set, and reset the fill so a future download starts cleanly.
        progressEl.style.display = 'none';
        const fill = progressEl.querySelector('.llm-progress-fill');
        if (fill) fill.style.width = '0';
      }

      try { await invoke('cancel_llm_download', { modelId }); } catch (_) { /* cancel best-effort */ }
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
  const progressEl = document.getElementById(`llm-progress-${modelId}`);

  if (cancelled) {
    // User-initiated abort: backend already removed the partial file. Drop
    // active-download state and hide progress. The card's red-flash
    // animation in wireCancelBtns owns the re-render once it finishes,
    // so we don't fire renderLocalLlmModels here — that would interrupt
    // the animation mid-way.
    delete intState.activeDownloads[modelId];
    if (progressEl) progressEl.classList.remove('visible', 'complete', 'error');
    // Fallback: if the flash listener somehow missed (e.g. card no longer
    // exists), still trigger a re-render after the expected animation time.
    setTimeout(() => {
      if (!intState.activeDownloads[modelId]) {
        renderLocalLlmModels().catch(() => {});
      }
    }, 2200);
    return;
  }

  // Real failure path \u2014 keep state visible so the user sees what happened.
  intState.activeDownloads[modelId] = { error: 'Download failed' };
  showToast('Download failed: ' + err, 'error');
  if (progressEl) {
    progressEl.classList.add('visible', 'error');
    progressEl.classList.remove('complete');
    const text = progressEl.querySelector('.llm-progress-text');
    if (text) {
      text.innerHTML = '<span class="llm-progress-percent">Error</span> \u2022 Download failed';
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
      // Download finished \u2014 clear state, hide progress bar, swap card to the
      // post-download view (Select / Delete). No lingering "100% complete"
      // banner; the button transformation is the success cue.
      delete intState.activeDownloads[model_id];
      progressEl.classList.remove('visible', 'complete', 'error');
      if (cancelBtn) cancelBtn.style.display = 'none';
      renderLocalLlmModels().catch(() => {});
      return;
    } else {
      // If the user already clicked Cancel, the wireCancelBtns handler set
      // `cancelling: true` and hid the buttons + progress. Skip everything
      // here \u2014 late progress packets from the backend would otherwise
      // un-hide the Cancel button and the bar after the user already
      // dismissed them.
      const existing = intState.activeDownloads[model_id];
      if (existing && existing.cancelling) return;

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
