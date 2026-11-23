import { invoke, listen } from './core/tauri.js';
import * as state from './core/state.js';
import { createUpdateChecker } from './update-checker.js';
import { createUpdateInstaller, updateProgressText } from './update-installer.js';

let initialized = false;
const isBusy = () => state.isRecording || state.isRecordingBusy || state.transcribingIds.size > 0 ||
  state.queuedTranscriptionIds.size > 0 || state.pendingImports.size > 0 || state.pendingAutoExec.size > 0;
const setText = (element, value) => { if (element.textContent !== value) element.textContent = value; };

export function initUpdateChecks(currentVersion) {
  if (initialized) return;
  initialized = true;
  const start = async () => {
    const current = currentVersion || await invoke('get_app_version');
    const banner = document.getElementById('update-banner');
    const text = document.getElementById('update-banner-text');
    const button = document.getElementById('update-download-btn');
    const feedback = document.getElementById('update-feedback');
    const meter = document.getElementById('update-progress');
    if (!banner || !text || !button || !feedback || !meter) return;
    let available = null;
    let progress = null;
    let nativeBusy = false;
    let pollInFlight = false;
    let checker;
    const render = () => {
      const active = progress && !['error', 'idle'].includes(progress.phase);
      banner.hidden = !available && !active;
      if (banner.hidden) return;
      setText(text, available ? `Update available: v${available.version.replace(/^v/, '')} · installed v${current}` : 'App update');
      button.disabled = Boolean(active || nativeBusy || isBusy());
      setText(button, active ? 'UPDATING…' : progress?.phase === 'error' ? 'RETRY' : 'UPDATE');
      setText(feedback, updateProgressText(progress) || (button.disabled ? 'Finish current tasks before updating.' : 'Downloads, installs and restarts the app.'));
      meter.hidden = progress?.phase !== 'downloading';
      if (progress?.total > 0) meter.value = Math.min(100, progress.downloaded / progress.total * 100);
      else meter.removeAttribute('value');
    };
    const installer = createUpdateInstaller({
      status: () => invoke('get_app_update_status'),
      install: args => invoke('install_app_update', args),
      listen: callback => listen('app-update-progress', event => callback(event.payload)),
      isBusy,
      onState: next => {
        progress = next;
        render();
        if (next.phase === 'error') void checker.checkIfDue(true);
      },
    });
    button.onclick = () => { if (available) void installer.start(available.version); };
    let storage;
    try { storage = localStorage; } catch { /* Availability still works in memory. */ }
    checker = createUpdateChecker({
      currentVersion: current,
      check: () => invoke('check_for_updates'),
      storage,
      onUpdate: info => { available = info; render(); },
    });
    const poll = async () => {
      if (!available || document.hidden || pollInFlight) return;
      pollInFlight = true;
      try {
        const status = await invoke('get_app_update_status');
        nativeBusy = status.busy || status.installing;
      } catch { /* Click performs the authoritative status check again. */ }
      finally { pollInFlight = false; render(); }
    };
    const onFocus = () => { void checker.checkIfDue(); void poll(); };
    window.addEventListener('focus', onFocus);
    const statusTimer = setInterval(() => { render(); void poll(); }, 1000);
    const checkTimer = setInterval(() => { void checker.checkIfDue(); }, 15 * 60 * 1000);
    window.addEventListener('pagehide', () => {
      clearInterval(statusTimer);
      clearInterval(checkTimer);
      window.removeEventListener('focus', onFocus);
    }, { once: true });
    void checker.checkIfDue();
  };
  void start().catch(error => console.error('Cannot initialize update checks:', error));
}
