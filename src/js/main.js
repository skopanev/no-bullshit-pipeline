// Entry point — imports all modules, wires events, bootstraps the app.
import { invoke, listen } from './core/tauri.js';
import * as state from './core/state.js';
import { escapeHtml } from './core/utils.js';
import { on, emit } from './core/events.js';
import { trace } from './ignore-trace.js';
import { checkForUpdates } from './updater.js';

// UI
import './ui/confirm-modal.js';
import { showToast } from './ui/toast.js';
import { ViewManager } from './ui/view-manager.js';

// Settings
import { loadSettings, switchSettingsTab, initSettingsListeners } from './settings/settings.js';
import { initTranscriptionSettings } from './settings/transcription.js';
import { updatePermissionStatus, initPermissions } from './settings/permissions.js';
import { initModelVersion, refreshModelVersion } from './settings/model-version.js';

// Recording
import './recording/timer.js';
import { startWaveformAnimation, stopWaveformAnimation } from './recording/waveform.js';
import './recording/live-transcript.js';
import { toggleRecording, startRecording, setRecordingUI } from './recording/controls.js';
import { loadRecordings, renderRecordingsList } from './recording/list.js';
// renderRecordingsList is invoked from transcription_progress handler when
// only the "Transcribing…" status changes — no need to refetch all metadata.
import { showDetailView, hideDetailView } from './recording/detail.js';
import { autoTranscribeAndExecute } from './recording/auto-execute.js';

// Pipeline
import { loadPipelineDefs } from './pipeline/defs-list.js';
import { renderPipelineFlowHTML } from './pipeline/flow-renderer.js';
import { renderPipelineChips, startRecordingWithPipeline } from './pipeline/chips.js';
import { allPipelineDefs } from './pipeline/state.js';

// Health
import { initHealthCheck, scheduleAudit } from './health/init.js';

// The window itself must never scroll — only the inner `.detail-scroller`
// does. A stray scrollIntoView()/focus() (step editor, shortcut editor, tab
// switch) can still nudge the page, dragging the fixed-offset settings content
// — including the sticky "Back / Settings" header — up behind the fixed
// app-bar. styles.css caps body to 100vh to prevent this, but transient
// overflow (animations, sub-pixel on fractional-scaled displays) can reopen a
// gap. Snap any page-level scroll straight back to 0 so the header can never
// end up hidden; the inner scroller (which doesn't fire window 'scroll') is
// untouched, so editors still scroll into view.
window.addEventListener('scroll', () => {
  if (window.scrollY !== 0 || window.scrollX !== 0) window.scrollTo(0, 0);
}, { passive: true });

// Expose globals needed by other modules and onclick handlers
window.showDetailView = showDetailView;
window.escapeHtml = escapeHtml;
window.__nbpLoadPipelineDefs = loadPipelineDefs;
window.__nbpRenderPipelineFlowHTML = renderPipelineFlowHTML;
window.__nbpSwitchSettingsTab = switchSettingsTab;

// Wire record button
const recordToggleBtn = document.getElementById('record-toggle-btn');
if (recordToggleBtn) recordToggleBtn.addEventListener('click', toggleRecording);

// Wire cross-module events
on('tab:pipelines', () => loadPipelineDefs());
on('recording:showDetail', (id) => showDetailView(id));
on('recording:hideDetail', () => hideDetailView());
on('recordings:reload', async () => { await loadRecordings(); renderRecordingsList(); });
on('pipelines:renderChips', () => renderPipelineChips());

// ===== INIT =====
async function init() {
  await loadSettings();
  await loadRecordings();
  await loadPipelineDefs();
  renderRecordingsList();

  try {
    const version = await invoke('get_app_version');
    const versionEl = document.getElementById('app-version');
    if (versionEl) versionEl.textContent = `v${version} `;
  } catch (err) { console.error('Failed to fetch version:', err); }

  // Background update check — silent on failure / no-newer-version.
  checkForUpdates();

  // ASR model version check for the active engine (server-side throttled to
  // daily; the banner only appears on a real, non-dismissed signal).
  refreshModelVersion(false);

  await updatePermissionStatus();

  // System audio warnings
  listen('recording_warning', (event) => {
    console.warn('Recording warning:', event.payload);
    showToast(event.payload, 'warning');
  });

  // Tray menu: start recording with preselected pipeline
  listen('tray-start-pipeline', async (event) => {
    const pipelineName = event.payload;
    if (!state.isRecording && !state.isRecordingBusy) {
      await startRecordingWithPipeline(pipelineName);
    }
  });

  // Tray menu: "New Record" — clean start, no pipeline preselection.
  // User picks pipelines later via chip bar (or doesn't).
  listen('tray-record-new', async () => {
    if (!state.isRecording && !state.isRecordingBusy) {
      await startRecording();
    }
  });

  listen('tray-open-recording', async (event) => {
    await showDetailView(event.payload);
  });

  listen('tray-open-settings', () => {
    ViewManager.showSettings();
  });

  listen('tray-open-pipelines', () => {
    ViewManager.showSettings();
    switchSettingsTab('pipelines');
  });

  // Auto-start recording when a call is detected
  listen('call-detected', async () => {
    if (!state.isRecording && !state.isRecordingBusy) {
      await startRecording();
    }
  });

  // Auto-transcribe + auto-execute on recording completion. Transcription is
  // always on (the toggle was removed) — FluidAudio runs on-device so it never
  // fails offline.
  listen('recording_complete', async (event) => {
    const recordingId = event.payload;
    trace('JS recording_complete:', recordingId);
    state.setIsRecording(false);
    setRecordingUI(false);
    stopWaveformAnimation();
    await loadRecordings();
    if (state.selectedRecordingId === recordingId) showDetailView(recordingId);

    // Saved dictations reuse this event only to refresh the list — they were
    // already transcribed and ran their shortcut pipeline (+ pasted). Don't
    // re-transcribe or fire meeting auto_run on them. (auto_run is recording-
    // only by intent; dictation gets its pipeline per-shortcut.)
    const rec = (state.allRecordings || []).find((r) => r.id === recordingId);
    if (rec?.source === 'dictation') return;

    // Pipelines to run: explicitly-assigned (pendingAutoExec) plus any pipeline
    // flagged auto_run — those fire after every recording. Deduped, assigned
    // ones first so their order is preserved.
    const assigned = state.pendingAutoExec.get(recordingId) || [];
    const autoRun = (allPipelineDefs || []).filter((p) => p.auto_run).map((p) => p.name);
    const pipelines = [...new Set([...assigned, ...autoRun])];
    state.pendingAutoExec.delete(recordingId);
    autoTranscribeAndExecute(recordingId, pipelines);
  });

  // Call-detector auto-started a recording. The main-window list refreshes
  // and we mirror server-side is_recording so the main Record button reflects
  // the running state. Default pipeline (if any) is queued for auto-execute
  // after recording_complete fires — same pendingAutoExec pathway the manual
  // flow uses on stop.
  listen('recording_started', async (event) => {
    const { id, pipelines, source } = event.payload || {};
    trace('JS recording_started:', { id, pipelines, source });
    if (!id) return;
    state.setIsRecording(true);
    // Reflect the active recording on the global top-bar control — works for
    // auto/call recordings too, not just the manual flow (controls.js).
    setRecordingUI(true);
    startWaveformAnimation();
    await loadRecordings();
    if (Array.isArray(pipelines) && pipelines.length > 0) {
      state.pendingAutoExec.set(id, pipelines);
    }
  });

  // Recording was discarded server-side (duration < auto_discard_seconds).
  // The row from the optimistic `recording_started` refresh needs to go;
  // recording_complete never fires for discarded sessions so this is the
  // only signal we get.
  listen('recording_discarded', async (event) => {
    const recordingId = typeof event.payload === 'string' ? event.payload : null;
    trace('JS recording_discarded:', recordingId);
    state.setIsRecording(false);
    setRecordingUI(false);
    stopWaveformAnimation();
    if (recordingId) state.pendingAutoExec.delete(recordingId);
    await loadRecordings();
  });

  // Retention sweep removed expired entries on window focus — refresh the list
  // so the deleted rows disappear immediately.
  listen('recordings_pruned', async () => {
    await loadRecordings();
    renderRecordingsList();
  });

  // Track in-flight transcriptions so the recordings list can render a
  // "Transcribing…" status on the affected row instead of just a duration.
  // detail.js has its own per-recording progress bar; this is purely for
  // list visibility. On 'Done' we refresh the list to pick up the freshly-
  // written `transcript_preview`.
  listen('transcription_progress', async (event) => {
    const { recording_id, stage } = event.payload || {};
    if (!recording_id) return;
    if (stage === 'Done') {
      state.transcribingIds.delete(recording_id);
      await loadRecordings();
    } else {
      if (!state.transcribingIds.has(recording_id)) {
        state.transcribingIds.add(recording_id);
        renderRecordingsList();
      }
    }
  });

  // Show onboarding if never completed
  const onboardingOverlay = document.getElementById('onboarding-overlay');
  if (onboardingOverlay && !state.appSettings.onboarding_completed) {
    onboardingOverlay.style.display = 'flex';
  }
}

async function bootstrapApp() {
  // Wire all event listeners
  initSettingsListeners();
  initTranscriptionSettings();
  initPermissions();
  // controls.js and detail.js wire their own event listeners on import
  initModelVersion();

  await init().catch(e => console.error('Init failed:', e));
  initHealthCheck();
  scheduleAudit(state.appSettings);
}

if (document.readyState === 'complete') {
  setTimeout(bootstrapApp, 0);
} else {
  window.addEventListener('load', bootstrapApp, { once: true });
}
