// ASR model version UI: the top-bar update banner + the Settings version row.
// Backend (asr_models.rs) scopes everything to the ACTIVE engine only.
// Banner shows only when there's a real signal (update_available / not_downloaded)
// and the user hasn't dismissed THIS version (dismiss memory lives server-side).

import { invoke, listen } from '../core/tauri.js';
import { showToast } from '../ui/toast.js';

let downloading = false;
let lastState = null;
// Monotonic token: each refresh bumps it. A slow get_asr_model_state for a
// previously selected engine must not overwrite the UI for the engine selected
// since — when its response arrives, its token is stale and we drop it.
let refreshSeq = 0;

const el = (id) => document.getElementById(id);

// Clean model name from the real HF repo id (no invented label).
// "FluidInference/parakeet-tdt-0.6b-v3-coreml" -> "parakeet-tdt-0.6b-v3".
export function repoToName(repo) {
  return (repo || '').split('/').pop().replace(/-coreml$/, '');
}

function modelName(s) {
  const slug = repoToName(s.repo);
  if (!slug) return s.engine || 'Model';
  return s.engine === 'qwen3' && s.variant ? `${slug} (${s.variant})` : slug;
}

// Name already carries size/version; only the update date is worth showing.
function factsFor(s) {
  const lm = s.info && s.info.last_modified;
  return lm ? `updated ${lm}` : '';
}

function openHf(repo) {
  const opener = window.__TAURI__?.opener;
  if (opener?.openUrl) opener.openUrl(`https://huggingface.co/${repo}`).catch(() => {});
}

/// Fetch current state and re-render banner + settings. `force` bypasses the
/// server-side 24h check throttle (used on engine switch).
export async function refreshModelVersion(force = false) {
  const seq = ++refreshSeq;
  console.debug(`[asr-ui] refreshModelVersion seq=${seq} force=${force}`);
  // Non-blocking feedback only where the wait actually is (the sidecar --status
  // spawn for Parakeet/Qwen3). The engine dropdown + options stay responsive;
  // renderSettings overwrites this the moment the real state arrives.
  const statusEl = el('model-version-status');
  if (statusEl) statusEl.textContent = 'Checking…';
  let s;
  try {
    s = await invoke('get_asr_model_state', { force });
  } catch (e) {
    console.error('[asr-ui] get_asr_model_state failed:', e);
    return;
  }
  // Drop out-of-order responses (engine switched while this was in flight).
  if (seq !== refreshSeq) {
    console.debug(
      `[asr-ui] DROP stale model-state seq=${seq} (latest=${refreshSeq}) engine=${s.engine || 'unmanaged'} state=${s.state}`
    );
    return;
  }
  console.debug(
    `[asr-ui] apply model-state seq=${seq} engine=${s.engine || 'unmanaged'} variant=${s.variant || '-'} state=${s.state} repo=${s.repo || '-'}`
  );
  lastState = s;
  if (!downloading) renderBanner(s);
  renderSettings(s);
}

function renderBanner(s) {
  const banner = el('model-update-banner');
  if (!banner) return;
  banner.style.background = ''; // clear any leftover progress fill → CSS color
  const show =
    !s.update_dismissed && (s.state === 'update_available' || s.state === 'not_downloaded');
  if (!show) {
    banner.style.display = 'none';
    return;
  }
  el('model-update-text').textContent =
    s.state === 'update_available'
      ? `Update available for ${modelName(s)}`
      : `${modelName(s)} isn't downloaded yet`;
  el('model-update-action').textContent = s.state === 'update_available' ? 'Update' : 'Download';
  el('model-update-action').style.display = '';
  el('model-update-progress').style.display = 'none';
  el('model-update-dismiss').style.display = '';
  banner.style.display = '';
}

function renderSettings(s) {
  const status = el('model-version-status');
  const actionBtn = el('model-version-action-btn');
  const facts = el('model-version-facts');
  const link = el('model-version-link');
  if (!status) return;
  if (s.state === 'unmanaged') {
    status.textContent = 'Managed automatically (cloud / Apple)';
    if (actionBtn) actionBtn.style.display = 'none';
    if (facts) facts.textContent = '';
    if (link) link.style.display = 'none';
    return;
  }
  if (facts) facts.textContent = factsFor(s);
  if (link) {
    if (s.repo) {
      link.style.display = '';
      link.onclick = () => openHf(s.repo);
    } else {
      link.style.display = 'none';
    }
  }
  const labels = {
    up_to_date: 'up to date',
    update_available: 'update available',
    not_downloaded: 'not downloaded',
    unknown: 'unknown',
  };
  status.textContent = `${modelName(s)} — ${labels[s.state] || s.state}`;
  if (!actionBtn) return;
  if (s.state === 'update_available') {
    actionBtn.textContent = 'Update';
    actionBtn.style.display = '';
  } else if (s.state === 'not_downloaded') {
    actionBtn.textContent = 'Download';
    actionBtn.style.display = '';
  } else {
    actionBtn.style.display = 'none';
  }
}

async function startDownload() {
  if (downloading) return;
  const force = lastState?.state === 'update_available';
  downloading = true;

  const banner = el('model-update-banner');
  if (banner) {
    banner.style.display = '';
    banner.style.background = 'linear-gradient(to right, #bae6fd 0 0%, #e0f2fe 0% 100%)';
    el('model-update-text').textContent = modelName(lastState || {});
    el('model-update-action').style.display = 'none';
    el('model-update-dismiss').style.display = 'none';
    const p = el('model-update-progress');
    p.style.display = '';
    p.textContent = 'Starting…';
  }

  try {
    await invoke('download_asr_model', { force });
    showToast(force ? 'Model updated' : 'Model downloaded', 'success');
  } catch (e) {
    console.error('download_asr_model failed:', e);
    const detail = String(e || 'Unknown error');
    showToast(`Model update failed: ${detail}`, 'error', 10000);
    if (banner) {
      el('model-update-text').textContent = 'Model update failed';
      const p = el('model-update-progress');
      if (p) {
        p.style.display = '';
        p.textContent = detail;
      }
    }
  } finally {
    downloading = false;
    await refreshModelVersion(true);
  }
}

export function initModelVersion() {
  listen('asr-download-progress', (ev) => {
    if (!downloading) return;
    const d = ev.payload || {};
    const pct = Math.max(0, Math.min(100, d.percent ?? 0));
    const stage = d.stage && d.stage !== 'Complete' ? d.stage : '';
    const p = el('model-update-progress');
    if (p) {
      if (pct >= 100) p.textContent = 'finishing…';
      else if (stage === 'Downloading') p.textContent = `${stage}… ${pct}% · progress may jump`;
      else p.textContent = stage ? `${stage}… ${pct}%` : `${pct}%`;
    }
    const banner = el('model-update-banner');
    if (banner) {
      banner.style.background = `linear-gradient(to right, #bae6fd 0 ${pct}%, #e0f2fe ${pct}% 100%)`;
    }
  });

  const action = el('model-update-action');
  if (action) action.onclick = () => startDownload();

  const dismiss = el('model-update-dismiss');
  if (dismiss) {
    dismiss.onclick = async () => {
      if (lastState?.state === 'update_available') {
        await invoke('dismiss_asr_update').catch(() => {});
      }
      el('model-update-banner').style.display = 'none';
    };
  }

  const settingsAction = el('model-version-action-btn');
  if (settingsAction) settingsAction.onclick = () => startDownload();
}
