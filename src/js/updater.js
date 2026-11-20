// Check GitHub Releases for a newer version on startup and on window focus
// (the app lives in the tray for days, so a startup-only check goes stale),
// but at most once per 24h — the last check time survives restarts. If one
// exists, reveal the #update-banner with a Download link to the release page.
// Network failures / no-newer-version → silent no-op (banner stays hidden).
//
// Banner uses the unified .app-banner component — see styles.css for the
// component contract; do not introduce a bespoke banner class for this.

import { invoke } from './core/tauri.js';

const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;
const LAST_CHECK_KEY = 'nbp.updateLastCheck';

let lastCheckMs = 0;
try {
  lastCheckMs = Number(localStorage.getItem(LAST_CHECK_KEY)) || 0;
} catch (_e) { /* storage unavailable — fall back to in-memory only */ }

function checkIfDue() {
  if (Date.now() - lastCheckMs < CHECK_INTERVAL_MS) return;
  checkForUpdates();
}

export function initUpdateChecks() {
  checkIfDue();
  window.addEventListener('focus', checkIfDue);
}

async function checkForUpdates() {
  lastCheckMs = Date.now();
  try {
    localStorage.setItem(LAST_CHECK_KEY, String(lastCheckMs));
  } catch (_e) { /* in-memory throttle still applies */ }
  let info = null;
  try {
    info = await invoke('check_for_updates');
  } catch (_e) {
    // Offline / API error — keep quiet, banner stays hidden.
    return;
  }
  if (!info) return;

  const banner = document.getElementById('update-banner');
  const text = document.getElementById('update-banner-text');
  const dl = document.getElementById('update-download-btn');
  const dismiss = document.getElementById('update-dismiss-btn');
  if (!banner || !text || !dl || !dismiss) return;

  text.textContent = `${info.version} available — you have v${info.current}`;
  banner.style.display = '';

  dl.onclick = async () => {
    try {
      const opener = window.__TAURI__?.opener;
      if (opener?.openUrl) await opener.openUrl(info.url);
    } catch (err) {
      console.error('Failed to open release URL:', err);
    }
  };

  dismiss.onclick = () => {
    banner.style.display = 'none';
  };
}
