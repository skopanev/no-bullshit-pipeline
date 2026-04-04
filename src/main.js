const { invoke } = window.__TAURI__.core;

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}

// ===== MARKDOWN RENDERING (via marked) =====
function looksLikeMarkdown(text) {
  if (!text || text.length < 20) return false;
  return /^#{1,6}\s/m.test(text) || /\*\*.+?\*\*/s.test(text) ||
    /^[-*]\s/m.test(text) || /^\d+\.\s/m.test(text) ||
    /^```/m.test(text) || /^>\s/m.test(text);
}

function applyMarkdownRendering(el, rawText) {
  if (!el || !rawText) return;
  el.dataset.rawText = rawText;
  if (looksLikeMarkdown(rawText)) {
    el.innerHTML = marked.parse(rawText);
    el.classList.add('md-rendered');
    el.classList.remove('md-raw');
  } else {
    el.textContent = rawText;
    el.classList.remove('md-rendered');
    el.classList.add('md-raw');
  }
}

function toggleMarkdownRaw(el) {
  if (!el || !el.dataset.rawText) return;
  if (el.classList.contains('md-rendered')) {
    el.textContent = el.dataset.rawText;
    el.classList.remove('md-rendered');
    el.classList.add('md-raw');
    return true;
  } else {
    el.innerHTML = marked.parse(el.dataset.rawText);
    el.classList.add('md-rendered');
    el.classList.remove('md-raw');
    return false;
  }
}

// ===== VIEW STATE MANAGER =====
const ViewManager = {
  closeAll() {
    document.body.classList.remove('detail-open', 'settings-open', 'prompts-open', 'pipelines-open');
  },

  showRecordings() {
    this.closeAll();
    this.updateSidebar('recordings');
  },

  showDetail() {
    this.closeAll();
    document.body.classList.add('detail-open');
    this.updateSidebar('recordings');
  },

  showSettings() {
    this.closeAll();
    document.body.classList.add('settings-open');
    this.updateSidebar('settings');
  },

  showPrompts() {
    this.closeAll();
    document.body.classList.add('prompts-open');
    this.updateSidebar('prompts');
    if (typeof loadPromptTemplates === 'function') loadPromptTemplates();
  },

  showPipelines() {
    this.closeAll();
    document.body.classList.add('pipelines-open');
    this.updateSidebar('pipelines');
    switchSettingsTab('pipelines');
  },

  updateSidebar(view) {
    document.querySelectorAll('.sidebar-nav-item').forEach(item => {
      item.classList.toggle('active', item.dataset.view === view);
    });
  }
};

// ===== STATE =====
let timerInterval;
let startTime;
let isRecording = false;
let isRecordingBusy = false; // Guard against double-click during async start/stop

let allRecordings = [];
let selectedRecordingId = null;

let permissions = { mic: false, system_audio: false };
let appSettings = null;
let currentAssignedPipelines = new Set(); // pipelines assigned to the current/last recording
const pendingAutoExec = new Map(); // recordingId → pipeline names, consumed by recording_complete handler
let pipelineProgressUnlisten = null;
let pipelineRunningSteps = {}; // key: "recordingId:pipelineName" -> { step_name, start_time }
let stepElapsedTimer = null;

// ===== TRANSCRIPTION PROGRESS STATE =====
let transcriptionElapsedTimer = null;
let transcriptionStartTime = null;
let transcriptionCurrentStage = '';

function clearTranscriptionTimer() {
  if (transcriptionElapsedTimer) {
    clearInterval(transcriptionElapsedTimer);
    transcriptionElapsedTimer = null;
  }
  transcriptionStartTime = null;
  transcriptionCurrentStage = '';
}

// ===== LIVE TRANSCRIPT STATE =====
let liveTranscriptGeneration = 0;
let liveTranscriptUnlisten = null;

// ===== DOM ELEMENTS =====
const statusIndicator = document.getElementById("status-indicator");
const timerDisplay = document.getElementById("timer");
const recordToggleBtn = document.getElementById("record-toggle-btn");

const recordingsListEl = document.getElementById("recordings-list");
const emptyStateEl = document.getElementById("empty-state");
const detailViewEl = document.getElementById("detail-view");
const appLayoutEl = document.querySelector(".app-layout");
const backBtn = document.getElementById("back-btn");

const detailTitleInput = document.getElementById("detail-title");
const detailMetaHeaderEl = document.getElementById("detail-meta-header");
const detailTranscriptEl = document.getElementById("transcript-content");
const detailStructuredEl = document.getElementById("structured-content");

const onboardingOverlay = document.getElementById("onboarding-overlay");
const requestMicBtn = document.getElementById("request-mic-btn");
const requestSysBtn = document.getElementById("request-sys-btn");
const onboardingContinueBtn = document.getElementById("onboarding-continue-btn");
const permissionWarning = document.getElementById("permission-warning");
const fixPermissionsBtn = document.getElementById("fix-permissions-btn");

const settingsViewEl = document.getElementById("settings-view");
const settingsBtn = document.getElementById("settings-btn");
const settingsBackBtn = document.getElementById("settings-back-btn");
const browseStorageBtn = document.getElementById("browse-storage-btn");
// Save button removed — auto-save on change

const storagePathInput = document.getElementById("settings-storage-path");
// Auto-discard hardcoded to 3s — no UI input needed
const themeButtons = document.querySelectorAll(".theme-btn");

const detailControlsEl = document.getElementById("detail-controls");
const captureSectionEl = document.getElementById("capture-section");

// ===== TIMER =====
function formatTime(ms) {
  const totalSeconds = Math.floor(ms / 1000);
  const h = Math.floor(totalSeconds / 3600).toString().padStart(2, "0");
  const m = Math.floor((totalSeconds % 3600) / 60).toString().padStart(2, "0");
  const s = (totalSeconds % 60).toString().padStart(2, "0");
  return `${h}:${m}:${s} `;
}

function startTimer() {
  startTime = Date.now();
  timerInterval = setInterval(() => {
    const now = Date.now();
    timerDisplay.textContent = formatTime(now - startTime);

    if (isRecording && selectedRecordingId && detailViewEl.style.display !== 'none') {
      if (detailMetaHeaderEl) {
        detailMetaHeaderEl.textContent = `Recording... · ${timerDisplay.textContent}`;
      }
    }
  }, 100);
}

function stopTimer() {
  clearInterval(timerInterval);
  timerDisplay.textContent = "00:00:00";
}

// ===== RECORDING WAVEFORM (SPECTRUM STYLE) =====
let waveformInterval = null;
let displayMicLevel = 0;    // Mic level with decay
let displaySystemLevel = 0; // System level with decay

function getWaveformCanvas() {
  return document.getElementById("recording-waveform-canvas");
}

function startWaveformAnimation() {
  const canvas = getWaveformCanvas();
  const ctx = canvas ? canvas.getContext("2d") : null;
  if (!canvas || !ctx) return;

  displayMicLevel = 0;
  displaySystemLevel = 0;

  waveformInterval = setInterval(async () => {
    try {
      const levels = await invoke("get_audio_levels");

      // Amplify both (RMS is naturally low)
      const ampMic = Math.min(1.0, levels.mic * 6);
      const ampSys = Math.min(1.0, levels.system * 6);

      // Instant attack, medium decay
      displayMicLevel = ampMic > displayMicLevel ? ampMic : Math.max(0, displayMicLevel - 0.06);
      displaySystemLevel = ampSys > displaySystemLevel ? ampSys : Math.max(0, displaySystemLevel - 0.06);

      drawSpectrum();
    } catch (e) {
      // Ignore errors
    }
  }, 30); // ~33fps for smoother animation
}

function stopWaveformAnimation() {
  if (waveformInterval) {
    clearInterval(waveformInterval);
    waveformInterval = null;
  }
  displayMicLevel = 0;
  displaySystemLevel = 0;
  const canvas = getWaveformCanvas();
  const ctx = canvas ? canvas.getContext("2d") : null;
  if (ctx && canvas) {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
  }
}

function drawSpectrum() {
  const canvas = getWaveformCanvas();
  const ctx = canvas ? canvas.getContext("2d") : null;
  if (!ctx || !canvas) return;

  const style = getComputedStyle(document.documentElement);
  // Mic = accent color (purple/blue depending on theme)
  const micColor = style.getPropertyValue("--accent").trim() || "#a855f7";
  // System = success color (green in all themes)
  const sysColor = style.getPropertyValue("--success").trim() || "#10b981";

  const width = canvas.width;
  const height = canvas.height;

  ctx.clearRect(0, 0, width, height);

  // Symmetric: bars grow from the center line outward
  // Mic (accent) grows UP, System (success) grows DOWN
  const NUM_BARS = 5;
  const barW = Math.floor(width / NUM_BARS) - 2;
  const barGap = 2;
  const halfH = height / 2; // center line
  const maxBarH = halfH * 0.85;
  const multipliers = [0.6, 0.9, 1.0, 0.9, 0.6];

  for (let i = 0; i < NUM_BARS; i++) {
    const x = i * (barW + barGap) + barGap;

    // Mic — grows upward from center
    const micH = Math.max(2, displayMicLevel * multipliers[i] * maxBarH);
    ctx.fillStyle = micColor;
    ctx.fillRect(x, halfH - micH, barW, micH);

    // System — grows downward from center
    const sysH = Math.max(2, displaySystemLevel * multipliers[i] * maxBarH);
    ctx.fillStyle = sysColor;
    ctx.fillRect(x, halfH, barW, sysH);
  }
}

// ===== LIVE TRANSCRIPT =====
function renderLiveTranscript(text) {
  const content = document.getElementById('live-transcript-content');
  if (!content) return;
  if (!text) {
    content.innerHTML = '<span style="color: var(--text-secondary); opacity: 0.5; font-style: italic; font-size: 0.85rem;">Listening...</span>';
    return;
  }
  content.textContent = text;
  content.scrollTop = content.scrollHeight;
}

async function startLiveTranscript(recordingId) {
  const generation = ++liveTranscriptGeneration;

  if (!appSettings?.transcription?.realtime_enabled) return;

  const panel = document.getElementById('live-transcript-panel');
  if (panel) panel.style.display = '';
  renderLiveTranscript('');

  if (liveTranscriptUnlisten) {
    liveTranscriptUnlisten();
    liveTranscriptUnlisten = null;
  }

  const unlisten = await window.__TAURI__.event.listen('realtime_transcript_updated', (event) => {
    if (liveTranscriptGeneration !== generation) return;
    const { text } = event.payload;
    renderLiveTranscript(text);
    
    if (detailTranscriptEl && selectedRecordingId === recordingId) {
      applyMarkdownRendering(detailTranscriptEl, text);
      detailTranscriptEl.classList.remove('empty');
    }
  });

  if (liveTranscriptGeneration !== generation) {
    unlisten();
    return;
  }
  liveTranscriptUnlisten = unlisten;

  try {
    await invoke('start_realtime_transcription', { recordingId });
  } catch (err) {
    if (liveTranscriptGeneration !== generation) return;
    console.error('Failed to start realtime transcription:', err);
    if (panel) panel.style.display = 'none';
    if (liveTranscriptUnlisten) {
      liveTranscriptUnlisten();
      liveTranscriptUnlisten = null;
    }
  }
}

function stopLiveTranscript() {
  liveTranscriptGeneration++;
  if (liveTranscriptUnlisten) {
    liveTranscriptUnlisten();
    liveTranscriptUnlisten = null;
  }
  const panel = document.getElementById('live-transcript-panel');
  if (panel) panel.style.display = 'none';
}

// ===== RECORDING CONTROLS =====
async function toggleRecording() {
  if (isRecordingBusy) return; // Prevent double-click during async operations

  // If we have a recording selected and not recording → play it
  if (selectedRecordingId && !isRecording) {
    try {
      await invoke('stop_audio'); // Stop any previous playback
      await invoke('play_audio', { recordingId: selectedRecordingId });
    } catch (err) {
      console.error('Playback error:', err);
    }
    return;
  }

  if (isRecording) {
    await stopRecording();
  } else {
    await startRecording();
  }
}

function setRecordingUI(recording) {
  if (recording) {
    if (statusIndicator) statusIndicator.className = "status-recording";
    document.body.classList.add("is-recording-active");
    if (recordToggleBtn) {
      recordToggleBtn.innerHTML = '<svg class="stop-icon" width="11" height="11" viewBox="0 0 11 11" fill="currentColor"><rect width="11" height="11" rx="1.5"/></svg>';
      recordToggleBtn.classList.add("is-active");
      recordToggleBtn.classList.remove("is-play-mode");
      recordToggleBtn.title = "Stop Recording";
    }
  } else {
    if (statusIndicator) statusIndicator.className = "status-idle";
    document.body.classList.remove("is-recording-active");
    updateMainButton();
  }
}

// Update main button based on context: play (when recording selected) or record (default)
function updateMainButton() {
  if (!recordToggleBtn) return;
  recordToggleBtn.classList.remove("is-active");

  if (selectedRecordingId && !isRecording) {
    // Show play button
    recordToggleBtn.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg>';
    recordToggleBtn.classList.add("is-play-mode");
    recordToggleBtn.title = "Play Recording";
  } else {
    // Show record button
    recordToggleBtn.innerHTML = '<svg width="11" height="11" viewBox="0 0 11 11" fill="currentColor"><circle cx="5.5" cy="5.5" r="5.5"/></svg>';
    recordToggleBtn.classList.remove("is-play-mode");
    recordToggleBtn.title = "Start Recording";
  }
}

async function startRecording() {
  isRecordingBusy = true;
  ViewManager.showRecordings();

  const saveMixOnly = appSettings?.save_mix_only !== false;
  try {
    const metadata = await invoke("start_recording", { saveMixOnly: saveMixOnly });
    isRecording = true;
    setRecordingUI(true);

    await loadRecordings();
    startTimer();
    startWaveformAnimation();
    showDetailView(metadata.id);
    startLiveTranscript(metadata.id);

    showToast('Recording started', 'info');

  } catch (error) {
    // Revert all state on failure
    isRecording = false;
    stopTimer();
    stopWaveformAnimation();
    setRecordingUI(false);
    console.error("Failed to start recording:", error);
    showToast("Failed to start: " + error, 'error');
  } finally {
    isRecordingBusy = false;
  }
}

async function stopRecording() {
  isRecordingBusy = true;
  try {
    const currentId = selectedRecordingId;
    const stoppedPipelines = [...currentAssignedPipelines]; // Capture before clearing (for 06-03 auto-execute)

    if (detailTitleInput && selectedRecordingId) {
      try {
        await invoke('update_title', { recordingId: selectedRecordingId, title: detailTitleInput.value });
      } catch (e) {
        console.error('Title sync failed (non-fatal):', e);
      }
    }

    await invoke("stop_recording");
    isRecording = false;
    currentAssignedPipelines = new Set(); // Clear global after capturing to local

    showToast('Recording stopped', 'info');

    // Stash pipelines BEFORE async UI work so recording_complete handler always finds them
    pendingAutoExec.set(currentId, stoppedPipelines);

    stopTimer();
    stopWaveformAnimation();
    stopLiveTranscript();
    setRecordingUI(false);
    renderPipelineChips(); // Reset chip visual state after recording stops

    await loadRecordings();

    if (selectedRecordingId === currentId) {
      showDetailView(currentId);
      renderPipelineChips(); // Ensure chips are updated after detail view re-renders
    }

  } catch (error) {
    // Always reset UI state, even on error
    isRecording = false;
    currentAssignedPipelines = new Set();
    stopTimer();
    stopWaveformAnimation();
    stopLiveTranscript();
    setRecordingUI(false);
    renderPipelineChips();
    console.error("Failed to stop:", error);
    if (error && error.includes && error.includes("discarded")) {
      hideDetailView();
      await loadRecordings();
      showToast('Recording discarded (too short)', 'info');
    } else {
      showToast('Failed to stop: ' + error, 'error');
    }
  } finally {
    isRecordingBusy = false;
  }
}

async function autoTranscribeAndExecute(recordingId, pipelineNames) {
  if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(recordingId);

  const prBtn = document.getElementById('process-btn');
  
  const existingTranscript = await invoke('get_transcript', { recordingId });
  
  if (existingTranscript) {
    if (detailTranscriptEl && selectedRecordingId === recordingId) {
      applyMarkdownRendering(detailTranscriptEl, existingTranscript);
      detailTranscriptEl.classList.remove('empty');
    }
    if (prBtn && selectedRecordingId === recordingId) {
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      prBtn.disabled = false;
      prBtn.style.opacity = '1';
    }
    if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(null);
    
    await loadRecordings();
    if (selectedRecordingId === recordingId) showDetailView(recordingId);
    
    for (const pipelineName of pipelineNames) {
      try {
        await invoke('execute_pipeline', { recordingId, pipelineName });
      } catch (err) {
        console.error(`Auto-pipeline execution failed for "${pipelineName}":`, err);
      }
      await loadRecordings();
      if (selectedRecordingId === recordingId) showDetailView(recordingId);
    }
    return;
  }

  if (prBtn && selectedRecordingId === recordingId) {
    prBtn.disabled = true;
    prBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Auto-transcribing...</span>';
    prBtn.style.opacity = '0.6';
  }

  if (detailTranscriptEl && selectedRecordingId === recordingId) {
          detailTranscriptEl.innerHTML = `
      <div class="is-loading transcript-processing-state">
        <div class="loading-spinner transcript-processing-spinner"></div>
        <span class="transcript-processing-text">Processing audio...</span>
      </div>
    `;
    detailTranscriptEl.classList.remove('empty');
  }

  try {
    await invoke('transcribe_recording', { recordingId });
  } catch (err) {
    console.error('Auto-transcription failed:', err);
    if (detailTranscriptEl && selectedRecordingId === recordingId) {
      detailTranscriptEl.innerHTML = `
        <div class="transcript-error-state">
          <span class="transcript-error-text">Transcription failed: ${escapeHtml(err)}</span>
          <button class="mini-action-btn transcript-retry-btn" onclick="retryTranscription('${recordingId}')">Retry</button>
        </div>
      `;
      detailTranscriptEl.classList.add('empty');
    }
    if (prBtn && selectedRecordingId === recordingId) {
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Retry</span>';
      prBtn.disabled = false;
      prBtn.style.opacity = '1';
      prBtn.style.display = '';
    }
    if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(null);
    return;
  }

  if (detailTranscriptEl && selectedRecordingId === recordingId) {
    const transcript = await invoke('get_transcript', { recordingId });
    if (transcript) {
      applyMarkdownRendering(detailTranscriptEl, transcript);
      detailTranscriptEl.classList.remove('empty');
    }
  }

  if (prBtn && selectedRecordingId === recordingId) {
    prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
    prBtn.disabled = false;
    prBtn.style.opacity = '1';
  }
  if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(null);

  await loadRecordings();
  if (selectedRecordingId === recordingId) showDetailView(recordingId);

  for (const pipelineName of pipelineNames) {
    try {
      await invoke('execute_pipeline', { recordingId, pipelineName });
    } catch (err) {
      console.error(`Auto-pipeline execution failed for "${pipelineName}":`, err);
    }
    await loadRecordings();
    if (selectedRecordingId === recordingId) showDetailView(recordingId);
  }
}

window.retryTranscription = async function(recordingId) {
  const prBtn = document.getElementById('process-btn');
  if (prBtn) {
    prBtn.click();
  }
};

async function subscribeToProgress(recordingId) {
  // Clean up previous listener (Research Pitfall 3)
  if (pipelineProgressUnlisten) {
    pipelineProgressUnlisten();
    pipelineProgressUnlisten = null;
  }
  pipelineRunningSteps = {};
  if (stepElapsedTimer) { clearInterval(stepElapsedTimer); stepElapsedTimer = null; }

  pipelineProgressUnlisten = await window.__TAURI__.event.listen('pipeline-progress', (event) => {
    const payload = event.payload;
    if (payload.recording_id !== recordingId) return;
    const key = `${payload.recording_id}:${payload.pipeline_name}`;
    if (payload.status === 'running') {
      pipelineRunningSteps[key] = { step_name: payload.step_name, start_time: Date.now() };
    } else {
      delete pipelineRunningSteps[key];
    }
    renderPipelineStatus(recordingId);
  });
}

const PIPELINE_STATUS_DISPLAY = {
  waiting: 'Waiting',
  running: 'Running',
  done: 'Done',
  partial: 'Failed'
};

async function renderPipelineStatus(recordingId) {
  const section = document.getElementById('pipeline-status-section');
  const content = document.getElementById('pipeline-status-content');
  if (!section || !content) return;

  try {
    const states = await invoke('get_all_pipeline_states', { recordingId });
    if (!states || states.length === 0) {
      section.style.display = 'none';
      return;
    }

    section.style.display = '';

    let html = '';
    for (let ai = 0; ai < states.length; ai++) {
      const state = states[ai];
      const isDeletedPipeline = state.status === 'partial' && state.error && state.error.includes('deleted');
      const displayText = isDeletedPipeline ? 'Skipped' : (PIPELINE_STATUS_DISPLAY[state.status] || state.status);
      const badgeStatus = isDeletedPipeline ? 'skipped' : state.status;
      const runBtn = state.status === 'waiting'
        ? `<button class="pipeline-run-btn" data-pipeline="${escapeHtml(state.name)}" data-run-index="${state.run_index || 0}">Run</button>`
        : '';
      const deleteBtn = state.status !== 'running'
        ? `<button class="pipeline-run-delete" data-run-id="${escapeHtml(state.id)}" title="Delete run">&times;</button>`
        : '';
      const pipelineErrorHtml = isDeletedPipeline
        ? `<div class="pipeline-state-error">Pipeline was deleted before execution</div>`
        : '';
      html += `<div class="pipeline-status-row">
  <span class="pipeline-status-name">${escapeHtml(state.name)}</span>
  <span class="pipeline-status-badge status-${escapeHtml(badgeStatus)}">${escapeHtml(displayText)}</span>
  ${runBtn}
  ${deleteBtn}
</div>${pipelineErrorHtml}`;

      // Show pipeline flow visualization + per-step detail
      const pipelineDef = typeof allPipelineDefs !== 'undefined' && allPipelineDefs.find(d => d.name === state.name);
      const canRenderFlow = pipelineDef && typeof renderPipelineFlowHTML !== 'undefined';

      const runningKey = `${recordingId}:${state.name}`;
      const runningInfo = (state.status === 'running') ? pipelineRunningSteps[runningKey] : null;

      if (state.status === 'running' || state.status === 'partial' || state.status === 'done') {
        try {
          const steps = await invoke('get_step_outputs', { recordingId, pipelineName: state.name, runIndex: state.run_index || 0 });
          if (steps && steps.length > 0) {
            // Build step status map for flow visualization
            const stepStatuses = {};
            for (const step of steps) {
              if (runningInfo && step.name === runningInfo.step_name && step.status === 'pending') {
                stepStatuses[step.name] = 'running';
              } else {
                stepStatuses[step.name] = step.status;
              }
            }

            if (canRenderFlow) {
              html += `<div class="pipeline-run-flow">${renderPipelineFlowHTML(pipelineDef.steps, { compact: false, statuses: stepStatuses })}</div>`;
            }

            html += '<div class="pipeline-steps-detail">';
            for (const step of steps) {
              let rowClass = 'pipeline-step-row';
              let iconHtml = '';
              let extraHtml = '';
              let progressBarHtml = '';

              if (step.status === 'done') {
                rowClass += ' step-done';
                iconHtml = '<span class="step-status-icon">&#10003;</span>';
              } else if (step.status === 'failed') {
                rowClass += ' step-failed';
                iconHtml = '<span class="step-status-icon">&#10007;</span>';
                if (step.error) {
                  const shortError = step.error.length > 80 ? step.error.substring(0, 80) + '...' : step.error;
                  extraHtml = `<span class="step-error" title="${escapeHtml(step.error)}">${escapeHtml(shortError)}</span>`;
                }
              } else if (step.status === 'skipped') {
                rowClass += ' step-skipped';
                iconHtml = '<span class="step-status-icon">&#9675;</span>';
                extraHtml = '<span class="step-skipped-label">(skipped)</span>';
              } else if (runningInfo && step.name === runningInfo.step_name) {
                // Actively running step: animated progress bar + elapsed time
                rowClass += ' step-running';
                iconHtml = '<span class="step-status-icon">&#9679;</span>';
                const elapsedSecs = Math.round((Date.now() - runningInfo.start_time) / 1000);
                extraHtml = `<span class="step-elapsed" data-start="${runningInfo.start_time}">${elapsedSecs}s</span>`;
                progressBarHtml = '<div class="step-progress-bar-container"><div class="step-progress-bar-inner"></div></div>';
              } else {
                // Not yet reached
                rowClass += ' step-pending';
                iconHtml = '<span class="step-status-icon">&#9675;</span>';
              }

              // Per-step wall-clock duration (only for completed steps)
              let durationHtml = '';
              if (step.duration_secs != null && step.duration_secs > 0) {
                const formatted = step.duration_secs >= 60
                  ? `${Math.floor(step.duration_secs / 60)}m ${Math.round(step.duration_secs % 60)}s`
                  : `${step.duration_secs.toFixed(1)}s`;
                durationHtml = `<span class="step-duration">${formatted}</span>`;
              }

              // Inline expand toggles on the step row itself
              let togglesHtml = '';
              if (step.output) {
                togglesHtml += `<button class="step-inline-toggle" data-target="output" title="Show details">Details</button>`;
              }
              if (step.augmented_prompt) {
                togglesHtml += `<button class="step-inline-toggle" data-target="prompt" title="Show augmented prompt">Prompt</button>`;
              }

              html += `<div class="${rowClass}">
  ${iconHtml}
  <span class="step-name">${escapeHtml(step.name)}</span>
  ${durationHtml}
  ${extraHtml}
  <span class="step-toggles">${togglesHtml}</span>
</div>${progressBarHtml}`;

              // Collapsible output panel (hidden by default)
              if (step.output) {
                const outputHtml = looksLikeMarkdown(step.output)
                  ? marked.parse(step.output)
                  : `<pre>${escapeHtml(step.output)}</pre>`;
                html += `<div class="step-expand-panel" data-panel="output" style="display:none;">${outputHtml}</div>`;
              }
              if (step.augmented_prompt) {
                html += `<div class="step-expand-panel" data-panel="prompt" style="display:none;"><pre>${escapeHtml(step.augmented_prompt)}</pre></div>`;
              }
            }
            html += '</div>';
          }
        } catch (e) {
          console.error('Failed to load step outputs:', e);
        }
      } else if (canRenderFlow && pipelineDef.steps && pipelineDef.steps.length > 0) {
        // Waiting pipeline: show flow with no statuses
        html += `<div class="pipeline-run-flow">${renderPipelineFlowHTML(pipelineDef.steps, { compact: false, statuses: {} })}</div>`;
      }
    }

    content.innerHTML = html;

    // Manage elapsed time ticker for running steps
    if (stepElapsedTimer) { clearInterval(stepElapsedTimer); stepElapsedTimer = null; }
    if (Object.keys(pipelineRunningSteps).length > 0) {
      stepElapsedTimer = setInterval(() => {
        content.querySelectorAll('.step-elapsed[data-start]').forEach(el => {
          const start = parseInt(el.dataset.start, 10);
          el.textContent = Math.round((Date.now() - start) / 1000) + 's';
        });
      }, 1000);
    }

    // Wire "Run" buttons for waiting pipelines
    content.querySelectorAll('.pipeline-run-btn').forEach(btn => {
      btn.addEventListener('click', async () => {
        const pipelineName = btn.dataset.pipeline;
        btn.disabled = true;
        btn.textContent = 'Running...';
        try {
          await invoke('execute_pipeline', { recordingId, pipelineName });
        } catch (err) {
          console.error(`Pipeline execution failed for "${pipelineName}":`, err);
        }
        await loadRecordings();
        if (selectedRecordingId === recordingId) showDetailView(recordingId);
      });
    });

    // Wire inline expand toggles (Output / Prompt buttons on step rows)
    content.querySelectorAll('.step-inline-toggle').forEach(btn => {
      btn.addEventListener('click', () => {
        const panelType = btn.dataset.target;
        const stepRow = btn.closest('.pipeline-step-row');
        if (!stepRow) return;
        // Find the matching panel after the step row
        let sibling = stepRow.nextElementSibling;
        while (sibling && sibling.classList.contains('step-expand-panel')) {
          if (sibling.dataset.panel === panelType) {
            const isOpen = sibling.style.display !== 'none';
            sibling.style.display = isOpen ? 'none' : 'block';
            btn.classList.toggle('is-active', !isOpen);
            return;
          }
          sibling = sibling.nextElementSibling;
        }
      });
    });

    // Wire delete buttons for completed/failed/waiting runs
    content.querySelectorAll('.pipeline-run-delete').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const ok = await showConfirm('Delete Pipeline Run?', 'This will remove the run and its output.');
        if (!ok) return;
        const runId = btn.dataset.runId;
        // Also find pipeline name so we can update assigned set during recording
        const row = btn.closest('.pipeline-status-row');
        const pipelineName = row ? row.querySelector('.pipeline-status-name')?.textContent : null;
        try {
          await invoke('remove_pipeline_run', { recordingId, runId });
          if (pipelineName && currentAssignedPipelines.has(pipelineName)) {
            currentAssignedPipelines.delete(pipelineName);
            if (typeof renderPipelineChips === 'function') renderPipelineChips();
            // Unhide the card in available pipelines
            const pipelineCardsEl = document.getElementById('pipeline-cards');
            if (pipelineCardsEl) {
              const card = pipelineCardsEl.querySelector(`.pipeline-card[data-pipeline="${pipelineName}"]`);
              if (card) {
                card.style.display = '';
                card.style.maxWidth = '';
                card.style.minWidth = '';
                card.style.padding = '';
                card.style.borderWidth = '';
                card.style.margin = '';
                card.style.opacity = '';
                card.style.overflow = '';
                card.style.pointerEvents = '';
                card.style.transition = '';
              }
            }
          }
        } catch (err) {
          console.error('Failed to delete pipeline run:', err);
          showToast('Failed to delete run: ' + err, 'error');
        }
        await loadRecordings();
        if (selectedRecordingId === recordingId) {
          renderPipelineStatus(recordingId);
        }
      });
    });
  } catch (e) {
    console.error('Failed to load pipeline states:', e);
    section.style.display = 'none';
  }
}

// ===== PERMISSIONS =====
async function updatePermissionStatus() {
  try {
    const onboardingCompleted = appSettings?.onboarding_completed || false;
    permissions = await invoke("check_permissions", { onboardingCompleted });

    // Update Onboarding UI
    const micItem = document.getElementById("perm-mic-item");
    const sysItem = document.getElementById("perm-sys-item");

    if (micItem) {
      const btn = micItem.querySelector(".modal-btn");
      btn.style.display = permissions.mic ? 'none' : 'block';
      micItem.querySelector(".perm-status-ok").style.display = permissions.mic ? 'block' : 'none';

      if (!permissions.mic && btn.dataset.requested === "true") {
        btn.textContent = "Open Settings";
      }
    }
    if (sysItem) {
      const btn = sysItem.querySelector(".modal-btn");
      btn.style.display = permissions.system_audio ? 'none' : 'block';
      sysItem.querySelector(".perm-status-ok").style.display = permissions.system_audio ? 'block' : 'none';

      if (!permissions.system_audio && btn.dataset.requested === "true") {
        btn.textContent = "Open Settings";
      }
    }

    if (onboardingContinueBtn) {
      onboardingContinueBtn.disabled = false; // Always allow continue
      const bothMissing = !permissions.mic && !permissions.system_audio;
      onboardingContinueBtn.textContent = bothMissing ? "I'll do that later" : "Continue";
    }

    // Update Warning Banner based on actual permissions
    // Now using real recording test instead of unreliable CGPreflightScreenCaptureAccess
    if (permissionWarning) {
      permissionWarning.style.display = (permissions.mic && permissions.system_audio) ? 'none' : 'flex';
    }

  } catch (err) {
    console.error("Failed to check permissions:", err);
  }
}

async function requestMic() {
  const btn = document.getElementById("request-mic-btn");
  if (btn.textContent === "Open Settings") {
    await invoke("open_privacy_settings", { pane: "mic" });
    return;
  }

  const success = await invoke("request_mic_permission");
  btn.dataset.requested = "true";

  if (!success) {
    btn.textContent = "Open Settings";
  }

  // Poll for status updates every 100ms for fast UI response
  for (let i = 0; i < 100; i++) {
    await new Promise(r => setTimeout(r, 100));
    await updatePermissionStatus();
    if (permissions.mic) break;
  }
}

async function requestSys() {
  const btn = document.getElementById("request-sys-btn");
  if (btn.textContent === "Open Settings") {
    await invoke("open_privacy_settings", { pane: "system_audio" });
    return;
  }

  const success = await invoke("request_system_audio_permission");
  btn.dataset.requested = "true";

  if (!success) {
    btn.textContent = "Open Settings";
  }

  // Poll for status updates every 100ms for fast UI response
  for (let i = 0; i < 100; i++) {
    await new Promise(r => setTimeout(r, 100));
    await updatePermissionStatus();
    if (permissions.system_audio) break;
  }
}

if (requestMicBtn) requestMicBtn.addEventListener("click", requestMic);
if (requestSysBtn) requestSysBtn.addEventListener("click", requestSys);
if (onboardingContinueBtn) {
  onboardingContinueBtn.addEventListener("click", async () => {
    // Mark onboarding as completed
    appSettings.onboarding_completed = true;
    await invoke("save_settings", { settings: appSettings });
    onboardingOverlay.style.display = 'none';
  });
}
if (fixPermissionsBtn) {
  fixPermissionsBtn.addEventListener("click", () => {
    onboardingOverlay.style.display = 'flex';
    updatePermissionStatus();
  });
}

// ===== RECORDINGS LIST =====
async function loadRecordings() {
  try {
    const recordings = await invoke("list_recordings");
    allRecordings = recordings || [];
    renderRecordingsList();
  } catch (error) {
    console.error("Failed to load recordings:", error);
  }
}

const dateOptions = {
  month: 'short',
  day: 'numeric',
  year: 'numeric',
  hour: 'numeric',
  minute: '2-digit'
};

function renderRecordingsList() {
  if (!recordingsListEl) return;

  if (allRecordings.length === 0) {
    recordingsListEl.innerHTML = "";
    if (emptyStateEl) emptyStateEl.style.display = "block";
    return;
  }
  if (emptyStateEl) emptyStateEl.style.display = "none";
  recordingsListEl.innerHTML = allRecordings.map(rec => {
    const isProcessing = rec.status === 'processing';
    const isCurrentlyRecording = isRecording && selectedRecordingId === rec.id;
    const metaText = isProcessing ? '<span style="color:var(--accent)">Processing...</span>' : formatDuration(getDuration(rec));

    // Health indicator
    const hasIssues = rec.health && rec.health.status !== 'ok';
    const healthIcon = hasIssues ? '<span class="health-warning" title="Issues occurred during recording"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--warning, #f59e0b)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg></span>' : '';

    const safeTitle = escapeHtml(rec.title || "Untitled");
    const safeId = escapeHtml(rec.id);

    // Pipeline tags with step chips
    const pipelineTags = (rec.pipelines || []).map(p => {
      const statusClass = p.status === 'Done' ? 'tag-done' : p.status === 'Partial' ? 'tag-partial' : p.status === 'Running' ? 'tag-running' : 'tag-waiting';
      const def = typeof allPipelineDefs !== 'undefined' ? allPipelineDefs.find(d => d.name === p.name) : null;
      const flowHtml = def && def.steps && def.steps.length > 0 && typeof renderPipelineFlowHTML !== 'undefined'
        ? renderPipelineFlowHTML(def.steps, { compact: true })
        : '';
      return `<div class="recording-pipeline-entry ${statusClass}"><span class="recording-pipeline-name">${escapeHtml(p.name)}</span>${flowHtml}</div>`;
    }).join('');

    const deleteDisabled = isCurrentlyRecording || isProcessing;
    const deleteBtnHtml = deleteDisabled ? '' : `<button class="recording-item-delete" data-id="${safeId}" title="Delete recording"><span class="icon-trash"></span></button>`;

    return `
    <div class="recording-item ${isCurrentlyRecording ? 'recording-active' : ''}" data-id="${safeId}" onclick="showDetailView(this.dataset.id)">
        <div class="recording-item-header">
          <div class="recording-title">${healthIcon}${safeTitle}${isCurrentlyRecording ? ' <span style="color:var(--accent)">●</span>' : ''}</div>
          <div class="recording-meta">
            <span>${new Date(rec.created_at).toLocaleString(undefined, dateOptions)}</span>
            <span>·</span>
            <span>${metaText}</span>
          </div>
        </div>
        ${pipelineTags ? `<div class="recording-pipeline-tags">${pipelineTags}</div>` : ''}
        ${deleteBtnHtml}
      </div>
    `;
  }).join("");

  // Wire delete buttons on recording cards
  recordingsListEl.querySelectorAll('.recording-item-delete').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation(); // Don't open detail view
      const recordingId = btn.dataset.id;
      const ok = await showConfirm('Delete Recording?', 'This action cannot be undone.');
      if (!ok) return;
      try {
        await invoke('delete_recording', { recordingId });
        if (selectedRecordingId === recordingId) hideDetailView();
        await loadRecordings();
      } catch (err) {
        console.error('Delete failed:', err);
        if (err && typeof err === 'string' && err.includes('finalized')) {
          showToast('Recording is still being finalized. Please wait a moment and try again.', 'info');
        } else {
          showToast('Delete failed: ' + err, 'error');
        }
      }
    });
  });
}

function getDuration(rec) {
  if (!rec.audio) return 0;
  return rec.audio.mix?.duration_sec || rec.audio.mic?.duration_sec || rec.audio.system?.duration_sec || 0;
}

function formatDuration(seconds) {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")} `;
}



let _pipelineCardAnimating = false; // guard to prevent double-clicks during animation

// ===== DETAIL VIEW =====
window.showDetailView = async (id) => {
  const rec = allRecordings.find(r => r.id === id);
  if (!rec) return;

  selectedRecordingId = id;
  clearTranscriptionTimer();
  updateMainButton(); // Switch to play mode

  if (detailTitleInput) detailTitleInput.value = rec.title || "";

  // Check Status
  const isProcessing = rec.status === 'processing';

  // Update Metadata in the Header
  if (detailMetaHeaderEl) {
    if (isRecording && id === selectedRecordingId && !isProcessing) {
      // While recording, handled by timer
    } else {
      const statusText = isProcessing ? '<span style="color:var(--accent)">Processing...</span>' : formatDuration(getDuration(rec));
      detailMetaHeaderEl.innerHTML = `${new Date(rec.created_at).toLocaleString(undefined, dateOptions)} · ${statusText} `;
    }
  }

  // Handle visibility
  ViewManager.showDetail();
  subscribeToProgress(id);

  if (detailControlsEl) detailControlsEl.style.display = 'flex';

  // LOCK BUTTONS if Processing
  if (deleteBtnHeader) {
    deleteBtnHeader.style.opacity = isProcessing ? '0.3' : '1';
    deleteBtnHeader.style.pointerEvents = isProcessing ? 'none' : 'auto';
    deleteBtnHeader.title = isProcessing ? "Processing audio..." : "Delete";
  }
  if (openFolderBtnHeader) {
    openFolderBtnHeader.title = "Open Folder";
  }
  if (prBtn) {
    prBtn.disabled = isProcessing;
    prBtn.style.opacity = '1';
    if (isProcessing) {
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Mixing Audio...</span>';
    } else {
      prBtn.disabled = true;
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      const checkId = id;
      const isAutoTranscribe = appSettings?.transcription?.enabled;
      invoke('is_transcribing', { recordingId: checkId }).then(active => {
        if (selectedRecordingId === checkId && prBtn) {
          if (active) {
            prBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Auto-transcribing...</span>';
            prBtn.style.opacity = '0.6';
          } else {
            prBtn.disabled = false;
            prBtn.style.display = '';
          }
        }
      }).catch(() => {
        if (selectedRecordingId === checkId && prBtn) {
          prBtn.disabled = false;
          prBtn.style.display = '';
        }
      });
    }
  }

  // POLLING if processing — non-recursive, self-contained polling loop
  if (isProcessing) {
    const pollId = id;
    const pollInterval = setInterval(async () => {
      if (selectedRecordingId !== pollId) {
        clearInterval(pollInterval); // User navigated away — stop polling
        return;
      }
      try {
        await loadRecordings();
        const updated = allRecordings.find(r => r.id === pollId);
        if (updated && updated.status !== 'processing') {
          clearInterval(pollInterval);
          showDetailView(pollId);
        }
      } catch (e) {
        console.error('Processing poll error:', e);
        clearInterval(pollInterval);
      }
    }, 1000);
  }

  // Detail view pipeline cards — clickable tiles with connector icons
  // During recording: show cards to queue pipelines (no execution)
  // Post-recording: show cards to assign + execute immediately if transcript exists
  // ===== AVAILABLE PIPELINES (unified for recording + post-recording) =====
  const detailPipelineAssignment = document.getElementById('detail-pipeline-assignment');
  const pipelineCardsEl = document.getElementById('pipeline-cards');
  if (detailPipelineAssignment && pipelineCardsEl) {
    if (!isProcessing && typeof allPipelineDefs !== 'undefined' && allPipelineDefs.length > 0) {
      // Filter out pipelines already assigned/run on this recording
      let alreadyUsed = new Set();
      try {
        const states = await invoke('get_all_pipeline_states', { recordingId: id });
        if (states) for (const s of states) alreadyUsed.add(s.name);
      } catch (_) {}
      const availableDefs = allPipelineDefs.filter(p => !alreadyUsed.has(p.name));

      let cardsHtml = '';
      for (const p of availableDefs) {
        const flowHtml = typeof renderPipelineFlowHTML !== 'undefined'
          ? renderPipelineFlowHTML(p.steps || [], { compact: true })
          : '';
        cardsHtml += `<div class="pipeline-card" data-pipeline="${escapeHtml(p.name)}">${flowHtml}<div class="pipeline-card-name">${escapeHtml(p.name)}</div></div>`;
      }
      pipelineCardsEl.innerHTML = cardsHtml;
      // Hide section if all pipelines already used
      detailPipelineAssignment.style.display = availableDefs.length > 0 ? '' : 'none';

      // Unified click: assign → animate to runs → execute when possible
      pipelineCardsEl.querySelectorAll('.pipeline-card').forEach(card => {
        card.addEventListener('click', async () => {
          if (_pipelineCardAnimating) return;
          _pipelineCardAnimating = true;
          const pipelineName = card.dataset.pipeline;

          const statusSection = document.getElementById('pipeline-status-section');
          const statusContent = document.getElementById('pipeline-status-content');
          const target = statusContent || statusSection;
          const cardRect = card.getBoundingClientRect();

          // Create flying clone
          const clone = card.cloneNode(true);
          clone.style.cssText = `position:fixed;left:${cardRect.left}px;top:${cardRect.top}px;width:${cardRect.width}px;height:${cardRect.height}px;z-index:10000;pointer-events:none;margin:0;border-radius:var(--radius-sm);`;
          document.body.appendChild(clone);

          // Collapse original card
          card.style.overflow = 'hidden';
          card.style.pointerEvents = 'none';
          card.style.maxWidth = card.offsetWidth + 'px';
          card.style.minWidth = '0';
          void card.offsetWidth;
          card.style.transition = 'max-width 0.35s ease, padding 0.35s ease, border-width 0.35s ease, opacity 0.15s ease, margin 0.35s ease';
          card.style.maxWidth = '0';
          card.style.padding = '0';
          card.style.borderWidth = '0';
          card.style.margin = '0';
          card.style.opacity = '0';

          // Ensure runs section is visible for measuring
          if (statusSection && statusSection.style.display === 'none') {
            statusSection.style.display = '';
            statusSection.style.opacity = '0';
          }

          // Fly clone to runs section
          let dx = 0, dy = 120;
          if (target) {
            const targetRect = target.getBoundingClientRect();
            dx = targetRect.left - cardRect.left;
            dy = (targetRect.top + Math.min(targetRect.height, 40)) - cardRect.top;
          }
          clone.style.transition = 'transform 0.4s cubic-bezier(0.4, 0, 0.15, 1), opacity 0.35s ease-in, box-shadow 0.4s ease';
          void clone.offsetWidth;
          clone.style.transform = `translate(${dx}px, ${dy}px) scale(0.7)`;
          clone.style.opacity = '0';
          clone.style.boxShadow = '0 4px 24px rgba(var(--accent-rgb, 99,102,241), 0.3)';

          // Backend: assign pipeline
          currentAssignedPipelines.add(pipelineName);
          let assigned = false;
          try {
            await invoke('assign_pipeline', { recordingId: id, pipelineName });
            assigned = true;
          } catch (err) {
            console.error('Failed to assign pipeline:', err);
          }

          setTimeout(async () => {
            clone.remove();
            _pipelineCardAnimating = false;

            if (statusSection) {
              statusSection.style.display = '';
              statusSection.style.opacity = '';
            }

            if (assigned) {
              card.style.display = 'none';
              // Execute when possible (transcript exists)
              try {
                const hasTranscript = detailTranscriptEl && !detailTranscriptEl.classList.contains('empty');
                if (hasTranscript) {
                  await invoke('execute_pipeline', { recordingId: id, pipelineName });
                }
              } catch (err) {
                console.error('Failed to execute pipeline:', err);
              }
            } else {
              // Restore card on failure
              card.style.display = '';
              card.style.maxWidth = '';
              card.style.minWidth = '';
              card.style.padding = '';
              card.style.borderWidth = '';
              card.style.margin = '';
              card.style.opacity = '';
              card.style.overflow = '';
              card.style.pointerEvents = '';
              card.style.transition = '';
            }

            if (typeof renderPipelineChips === 'function') renderPipelineChips();
            await loadRecordings();
            if (selectedRecordingId === id) renderPipelineStatus(id);

            // Flash runs section
            const flashTarget = document.getElementById('pipeline-status-content');
            if (flashTarget) {
              flashTarget.classList.remove('pipeline-status-flash');
              void flashTarget.offsetWidth;
              flashTarget.classList.add('pipeline-status-flash');
            }
          }, 420);
        });
      });
    } else {
      pipelineCardsEl.innerHTML = '<div style="color: var(--text-secondary); opacity: 0.75; font-size: 0.82rem;">No pipelines yet. Add one in Settings -> Pipelines.</div>';
      detailPipelineAssignment.style.display = '';
    }
  }

  // Keep transcript section visible during recording so detail view is never empty.
  // Only hide while post-recording processing is running.
  const hideContent = isProcessing;
  const transcriptSection = document.getElementById('transcript-section');
  if (transcriptSection) transcriptSection.style.display = hideContent ? 'none' : '';

  // Render pipeline status (shown during recording and after)
  renderPipelineStatus(id);

  // Load Transcript only if not recording/processing
  if (!hideContent && detailTranscriptEl) {
    const rawToggle = document.getElementById('transcript-raw-toggle');
    const isAutoTranscribe = appSettings?.transcription?.enabled;

    try {
      const isTranscribing = await invoke('is_transcribing', { recordingId: id });
      const transcript = await invoke("get_transcript", { recordingId: id });

      if (transcript) {
        applyMarkdownRendering(detailTranscriptEl, transcript);
        detailTranscriptEl.classList.remove('empty');
        if (saveTranscriptBtn) saveTranscriptBtn.style.display = '';
        if (rawToggle) rawToggle.style.display = looksLikeMarkdown(transcript) ? '' : 'none';
      } else if (isTranscribing) {
        detailTranscriptEl.innerHTML = `
          <div class="transcript-processing-state">
            <div class="transcript-processing-spinner"></div>
            <span class="transcript-processing-text">Processing audio...</span>
          </div>
        `;
        detailTranscriptEl.classList.remove('empty');
        if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';
        if (rawToggle) rawToggle.style.display = 'none';
      } else {
        detailTranscriptEl.textContent = "Not processed yet.";
        detailTranscriptEl.classList.add('empty');
        if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';
        if (rawToggle) rawToggle.style.display = 'none';
      }
    } catch (err) {
      console.error("Failed to load transcript:", err);
      detailTranscriptEl.textContent = "Not processed yet.";
      detailTranscriptEl.classList.add('empty');
      if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';
      if (rawToggle) rawToggle.style.display = 'none';
    }
  }

  if (!hideContent && detailStructuredEl) {
    detailStructuredEl.textContent = "Not processed yet.";
    detailStructuredEl.classList.add('empty');

    // Try to load summary if exists
    try {
      const summaryPath = `${appSettings.storage_path}/${id}/summary.md`;
      // We'd need a backend call to read this - for now just show placeholder
    } catch (err) {
      // Ignore
    }
  }

  // Reset audio player
  if (!hideContent) {
    isPlaying = false;
    if (playPauseBtn) {
      playPauseBtn.innerHTML = '<svg width="24" height="24" viewBox="0 0 24 24" fill="currentColor"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg>';
    }
    if (currentTimeEl) currentTimeEl.textContent = '0:00';
  }
};

function hideDetailView() {
  selectedRecordingId = null;
  updateMainButton(); // Switch back to record mode
  ViewManager.showRecordings();
  if (detailControlsEl) detailControlsEl.style.display = 'none';
  if (pipelineProgressUnlisten) {
    pipelineProgressUnlisten();
    pipelineProgressUnlisten = null;
  }
  pipelineRunningSteps = {};
  if (stepElapsedTimer) { clearInterval(stepElapsedTimer); stepElapsedTimer = null; }
}

// ===== EVENT LISTENERS =====
if (recordToggleBtn) recordToggleBtn.addEventListener("click", toggleRecording);
if (backBtn) backBtn.addEventListener("click", hideDetailView);

if (detailTitleInput) {
  // Save title immediately on blur (even during recording)
  detailTitleInput.addEventListener('blur', async (e) => {
    if (!selectedRecordingId) return;
    try {
      await invoke('update_title', { recordingId: selectedRecordingId, title: e.target.value });
      await loadRecordings();
    } catch (err) {
      console.error('Failed to update title:', err);
    }
  });

  // Also save on Enter key
  detailTitleInput.addEventListener('keypress', async (e) => {
    if (e.key === 'Enter') {
      e.target.blur(); // Trigger blur event which saves
    }
  });
}

// ===== UNIVERSAL CONFIRMATION MODAL =====
const confirmModal = document.getElementById('confirm-modal');
const confirmModalTitle = document.getElementById('confirm-modal-title');
const confirmModalMessage = document.getElementById('confirm-modal-message');
const confirmModalCancel = document.getElementById('confirm-modal-cancel');
const confirmModalOk = document.getElementById('confirm-modal-ok');
let _confirmResolve = null;

function showConfirm(title = 'Are you sure?', message = 'This action cannot be undone.', okLabel = 'Delete') {
  confirmModalTitle.textContent = title;
  confirmModalMessage.textContent = message;
  confirmModalOk.textContent = okLabel;
  confirmModal.style.display = 'flex';
  return new Promise(resolve => { _confirmResolve = resolve; });
}

function _closeConfirm(result) {
  confirmModal.style.display = 'none';
  if (_confirmResolve) { _confirmResolve(result); _confirmResolve = null; }
}

if (confirmModalCancel) confirmModalCancel.addEventListener('click', () => _closeConfirm(false));
if (confirmModalOk) confirmModalOk.addEventListener('click', () => _closeConfirm(true));
if (confirmModal) confirmModal.addEventListener('click', (e) => { if (e.target === confirmModal) _closeConfirm(false); });

// ===== TOAST NOTIFICATIONS =====
function showToast(message, type = 'info') {
  const ICONS = {
    success: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`,
    error:   `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>`,
    warning: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>`,
    info:    `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>`,
  };

  let container = document.getElementById('toast-container');
  if (!container) {
    container = document.createElement('div');
    container.id = 'toast-container';
    document.body.appendChild(container);
  }

  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.innerHTML = `
    <span class="toast-icon">${ICONS[type] || ICONS.info}</span>
    <span class="toast-body">${escapeHtml(message)}</span>
    <button class="toast-close" aria-label="Dismiss">&times;</button>
  `;

  const closeBtn = toast.querySelector('.toast-close');
  const dismiss = () => {
    toast.classList.remove('show');
    toast.addEventListener('transitionend', () => toast.remove(), { once: true });
  };
  const timeout = setTimeout(dismiss, 3500);
  closeBtn.onclick = () => { clearTimeout(timeout); dismiss(); };

  container.appendChild(toast);
  requestAnimationFrame(() => toast.classList.add('show'));
}

// ===== DELETE RECORDING =====
const deleteBtnHeader = document.getElementById('delete-btn-header');
if (deleteBtnHeader) {
  deleteBtnHeader.addEventListener('click', async () => {
    if (!selectedRecordingId) return;
    const ok = await showConfirm('Delete Recording?', 'This action cannot be undone.');
    if (!ok) return;
    try {
      await invoke('delete_recording', { recordingId: selectedRecordingId });
      hideDetailView();
      await loadRecordings();
    } catch (e) {
      console.error('Delete failed:', e);
      if (e && typeof e === 'string' && e.includes('finalized')) {
        showToast('Recording is still being finalized. Please wait a moment and try again.', 'info');
      } else {
        showToast('Delete failed: ' + e, 'error');
      }
    }
  });
}

const openFolderBtnHeader = document.getElementById('open-folder-btn-header');
if (openFolderBtnHeader) {
  openFolderBtnHeader.addEventListener('click', async () => {
    if (!selectedRecordingId || !appSettings?.storage_path) return;
    const folderPath = `${appSettings.storage_path}/${selectedRecordingId}`;
    try {
      await window.__TAURI_PLUGIN_OPENER__.openPath(folderPath);
    } catch (e) {
      console.error('Failed to open folder:', e);
    }
  });
}

// Listen for live transcription segments
if (window.__TAURI__) {
  const { listen } = window.__TAURI_PLATFORM_EVENT || {
    listen: (name, cb) => {
      // Fallback if platform event not accessible directly
      (async () => {
        const { listen: tauriListen } = await import('@tauri-apps/api/event');
        tauriListen(name, cb);
      })();
    }
  };

  // Transcription segment listener — scoped to the recording being transcribed
  try {
    let transcribingRecordingId = null;

    // Track which recording is being transcribed
    window.__NBP_setTranscribingId = (id) => { transcribingRecordingId = id; };

    window.__TAURI__.event.listen('recording_warning', (event) => {
      console.warn('Recording warning:', event.payload);
      showToast(event.payload, 'warning');
    });

    // Tray: start recording with pipeline from tray menu
    window.__TAURI__.event.listen('tray-start-pipeline', async (event) => {
      const pipelineName = event.payload;
      if (!isRecording && !isRecordingBusy) {
        await startRecordingWithPipeline(pipelineName);
      }
    });

    // Call detected: auto-start recording
    window.__TAURI__.event.listen('call-detected', async () => {
      if (!isRecording && !isRecordingBusy) {
        await startRecording();
      }
    });

    window.__TAURI__.event.listen('transcription_segment', (event) => {
      const segmentText = event.payload;
      // Only append if we're still viewing the recording that's being transcribed
      if (detailTranscriptEl && selectedRecordingId && selectedRecordingId === transcribingRecordingId) {
        if (detailTranscriptEl.classList.contains('empty')) {
          detailTranscriptEl.textContent = '';
          detailTranscriptEl.classList.remove('empty');
        }
        detailTranscriptEl.textContent += segmentText + ' ';

        const scroller = detailTranscriptEl.closest('.detail-scroller');
        if (scroller) scroller.scrollTop = scroller.scrollHeight;
      }
    });

    // Listen for transcription progress
    window.__TAURI__.event.listen('transcription_progress', (event) => {
      const { recording_id, stage, percent } = event.payload;
      if (recording_id !== transcribingRecordingId || selectedRecordingId !== transcribingRecordingId) return;
      const btn = document.getElementById('process-btn');
      if (!btn) return;
      // Ensure button stays disabled while transcription events are coming in
      btn.disabled = true;

      if (stage === 'Done') {
        clearTranscriptionTimer();
        btn.disabled = false;
        btn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
        return;
      }

      const STAGE_LABELS = {
        'Starting': 'Transcribing', 'Loading model': 'Loading model',
        'Transcribing': 'Transcribing', 'Preparing models': 'Preparing models',
        'Downloading ASR model': 'Downloading model', 'Downloading diarizer': 'Downloading model',
        'Diarization': 'Diarization', 'Finalizing': 'Finalizing',
      };
      const stageLabel = STAGE_LABELS[stage] || stage;

      if (percent > 0) {
        // Determinate progress — bold full-area progress display
        clearTranscriptionTimer();
        btn.innerHTML = `<span style="font-weight: 600; font-size: 12px;">${stageLabel} ${percent}%</span>`;
        if (detailTranscriptEl) {
          detailTranscriptEl.classList.remove('empty');
          detailTranscriptEl.innerHTML = `
            <div class="transcription-progress-wrap">
              <div class="transcription-progress-stage">${stageLabel}</div>
              <div class="transcription-progress-bar-track">
                <div class="transcription-progress-bar-fill" style="width:${percent}%"></div>
              </div>
              <div class="transcription-progress-percent">${percent}%</div>
            </div>`;
        }
      } else {
        // Indeterminate progress — spinner only
        transcriptionCurrentStage = stageLabel;
        btn.innerHTML = `<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">${stageLabel}…</span>`;
        if (detailTranscriptEl && !detailTranscriptEl.querySelector('.transcript-processing-state')) {
          detailTranscriptEl.classList.remove('empty');
          detailTranscriptEl.innerHTML = `
            <div class="transcript-processing-state">
              <div class="transcript-processing-spinner"></div>
              <span class="transcript-processing-text">${stageLabel}…</span>
            </div>`;
        }
      }
    });
  } catch (e) {
    console.error("Failed to setup transcription listener:", e);
  }
}

const saveTranscriptBtn = document.getElementById('save-transcript-btn');
const prBtn = document.getElementById('process-btn');
if (prBtn) {
  prBtn.addEventListener('click', async () => {
    if (!selectedRecordingId || prBtn.disabled) return;

    try {
      prBtn.disabled = true;
      prBtn.style.display = '';
      prBtn.style.opacity = '1';
      clearTranscriptionTimer();
      prBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Processing...</span>';

      if (detailTranscriptEl) {
        detailTranscriptEl.innerHTML = `
          <div class="transcript-processing-state">
            <div class="transcript-processing-spinner"></div>
            <span class="transcript-processing-text">Processing audio...</span>
          </div>
        `;
        detailTranscriptEl.classList.remove('empty');
      }
      if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';

      if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(selectedRecordingId);

      const transcript = await invoke('transcribe_recording', { recordingId: selectedRecordingId });

      if (transcript === '__already_running__') {
        prBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Processing...</span>';
        return;
      }

      if (detailTranscriptEl) {
        applyMarkdownRendering(detailTranscriptEl, transcript);
        detailTranscriptEl.classList.remove('empty');
      }
      if (saveTranscriptBtn) saveTranscriptBtn.style.display = '';

      try {
        const states = await invoke('get_all_pipeline_states', { recordingId: selectedRecordingId });
        for (const s of (states || [])) {
          if (s.status === 'waiting') {
            invoke('execute_pipeline', { recordingId: selectedRecordingId, pipelineName: s.name }).catch(e =>
              console.error(`Auto-execute pipeline "${s.name}" failed:`, e)
            );
          }
        }
      } catch (e) { console.error('Failed to auto-execute waiting pipelines:', e); }

      clearTranscriptionTimer();
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      prBtn.disabled = false;

    } catch (error) {
      clearTranscriptionTimer();
      console.error('Transcription failed:', error);
      showToast(`Transcription failed: ${error}`, 'error');

      if (detailTranscriptEl) {
        detailTranscriptEl.textContent = 'Transcription failed.';
        detailTranscriptEl.classList.add('empty');
      }

      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      prBtn.disabled = false;
    }
  });
}

// ===== SAVE TRANSCRIPT BUTTON =====
if (saveTranscriptBtn) {
  saveTranscriptBtn.addEventListener('click', async () => {
    if (!selectedRecordingId) return;
    try {
      saveTranscriptBtn.disabled = true;
      await invoke('export_transcript_md', { recordingId: selectedRecordingId });
    } catch (error) {
      console.error('Save transcript failed:', error);
      showToast(`Save failed: ${error}`, 'error');
    } finally {
      saveTranscriptBtn.disabled = false;
    }
  });
}

// ===== RAW / MARKDOWN TOGGLE =====
const transcriptRawToggle = document.getElementById('transcript-raw-toggle');
if (transcriptRawToggle) {
  transcriptRawToggle.addEventListener('click', () => {
    const isRaw = toggleMarkdownRaw(detailTranscriptEl);
    transcriptRawToggle.querySelector('span').textContent = isRaw ? 'Formatted' : 'Raw';
  });
}

// ===== SUMMARIZE & TEMPLATE PROCESSING =====
const summarizeBtn = document.getElementById('summarize-btn');
const extractBtn = document.getElementById('extract-btn');
const templateSelect = document.getElementById('template-select');

if (templateSelect) {
  templateSelect.addEventListener('change', () => {
    if (extractBtn) {
      extractBtn.disabled = !templateSelect.value;
    }
  });
}

if (summarizeBtn) {
  summarizeBtn.addEventListener('click', async () => {
    if (!selectedRecordingId) return;

    try {
      summarizeBtn.disabled = true;
      summarizeBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Summarizing...</span>';

      const summary = await invoke('summarize_recording', {
        recordingId: selectedRecordingId,
        provider: null
      });

      if (detailStructuredEl) {
        applyMarkdownRendering(detailStructuredEl, summary);
        detailStructuredEl.classList.remove('empty');
      }

      summarizeBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Summarize</span>';
      summarizeBtn.disabled = false;
    } catch (error) {
      console.error('Summarization failed:', error);
      showToast(`Summarization failed: ${error}`, 'error');
      summarizeBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Summarize</span>';
      summarizeBtn.disabled = false;
    }
  });
}

if (extractBtn) {
  extractBtn.addEventListener('click', async () => {
    if (!selectedRecordingId || !templateSelect.value) return;

    try {
      extractBtn.disabled = true;
      extractBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Extracting...</span>';

      const result = await invoke('process_with_template', {
        recordingId: selectedRecordingId,
        templateName: templateSelect.value,
        provider: null
      });

      if (detailStructuredEl) {
        applyMarkdownRendering(detailStructuredEl, result);
        detailStructuredEl.classList.remove('empty');
      }

      extractBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Extract</span>';
      extractBtn.disabled = !templateSelect.value;
    } catch (error) {
      console.error('Extraction failed:', error);
      showToast(`Extraction failed: ${error}`, 'error');
      extractBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Extract</span>';
      extractBtn.disabled = !templateSelect.value;
    }
  });
}


// ===== SETTINGS ELEMENTS =====
const transcriptionEnabledCheckbox = document.getElementById("settings-transcription-enabled");
const transcriptionDetailsEl = document.getElementById("transcription-details");
const transcriptionProviderSelect = document.getElementById("settings-transcription-provider");
const providerLocalSection = document.getElementById("provider-local-section");
const whisperModelSelect = document.getElementById("settings-whisper-model");
const downloadModelBtn = document.getElementById("download-model-btn");
const realtimeEnabledCheckbox = document.getElementById("settings-realtime-enabled");
const saveMixOnlyCheckbox = document.getElementById("settings-save-mix-only");
const callDetectionCheckbox = document.getElementById("settings-call-detection");

// Helper to mask API keys (show last 4 chars)
function maskApiKey(key) {
  if (!key || key.length < 8) return key || "";
  return "•".repeat(key.length - 4) + key.slice(-4);
}

// Helper to unmask API key if it was already masked
function isKeyMasked(value) {
  return value && value.includes("•");
}

// ===== SETTINGS =====
async function loadSettings() {
  try {
    appSettings = await invoke("load_settings");

    if (storagePathInput) storagePathInput.value = appSettings.storage_path;
    // auto_discard_seconds hardcoded to 3, no UI input

    // Transcription Settings
    if (appSettings.transcription) {
      if (transcriptionEnabledCheckbox) {
        transcriptionEnabledCheckbox.checked = appSettings.transcription.enabled;
        updateTranscriptionVisibility();
      }
      if (transcriptionProviderSelect) transcriptionProviderSelect.value = appSettings.transcription.provider;
      if (whisperModelSelect) {
        whisperModelSelect.value = appSettings.transcription.whisper_model || "Base";
      }

      // Render provider-first Models UI (API keys, capabilities, models)
      if (typeof renderModelsProviders === 'function') renderModelsProviders();

      updateProviderVisibility();
      updateTranscriptionProviderWarnings();
    }

    // Real-time transcription settings
    if (appSettings.transcription) {
      if (realtimeEnabledCheckbox) {
        realtimeEnabledCheckbox.checked = !!appSettings.transcription.realtime_enabled;
      }
    }

    // Save mix only setting
    if (saveMixOnlyCheckbox) {
      saveMixOnlyCheckbox.checked = appSettings.save_mix_only !== false; // default true
    }

    // Call detection setting
    if (callDetectionCheckbox) {
      callDetectionCheckbox.checked = !!appSettings.call_detection_enabled;
    }

    applyTheme(appSettings.theme);
  } catch (err) {
    console.error("Failed to load settings:", err);
  }
}

async function saveSettings() {
  try {
    appSettings.auto_discard_seconds = 3; // Hardcoded: discard recordings under 3s

    if (!appSettings.transcription) appSettings.transcription = {};

    appSettings.transcription.enabled = transcriptionEnabledCheckbox.checked;
    appSettings.transcription.provider = transcriptionProviderSelect.value;
    appSettings.transcription.whisper_model = whisperModelSelect.value;

    // Real-time transcription
    appSettings.transcription.realtime_enabled = realtimeEnabledCheckbox ? realtimeEnabledCheckbox.checked : false;

    // Handle API keys - fresh DOM lookups (cards are dynamically rendered)
    if (!appSettings.transcription.api_keys) appSettings.transcription.api_keys = {};
    if (!appSettings.providers) appSettings.providers = {};

    for (const providerId of ['openai', 'google', 'anthropic']) {
      const input = document.getElementById(`settings-api-key-${providerId}`);
      if (input && !isKeyMasked(input.value)) {
        const keyValue = input.value || null;
        appSettings.transcription.api_keys[providerId] = keyValue;
        // Also write to provider-first storage
        if (!appSettings.providers[providerId]) appSettings.providers[providerId] = {};
        appSettings.providers[providerId].api_key = keyValue;
      }
    }

    // Save mix only setting
    if (saveMixOnlyCheckbox) {
      appSettings.save_mix_only = saveMixOnlyCheckbox.checked;
    }

    // Call detection setting
    if (callDetectionCheckbox) {
      appSettings.call_detection_enabled = callDetectionCheckbox.checked;
    }

    await invoke("save_settings", { settings: appSettings });
    showToast('Settings saved', 'success');
  } catch (err) {
    console.error("Failed to save settings:", err);
    showToast('Failed to save settings', 'error');
  }
}

function updateTranscriptionVisibility() {
  if (!transcriptionDetailsEl) return;
  transcriptionDetailsEl.style.display = transcriptionEnabledCheckbox.checked ? 'flex' : 'none';
}


let availableModels = [];

// Map transcription provider value → provider key for API key lookup
const PROVIDER_KEY_MAP = {
  OpenAI: 'openai',
  Google: 'google',
};

const PROVIDER_LABEL_MAP = {
  OpenAI: 'OpenAI',
  Google: 'Google AI',
};

// Add ⚠️ to cloud provider options that have no API key configured
function updateTranscriptionProviderWarnings() {
  if (!transcriptionProviderSelect) return;
  const apiKeys = (appSettings && appSettings.transcription && appSettings.transcription.api_keys) || {};
  for (const option of transcriptionProviderSelect.options) {
    const keyId = PROVIDER_KEY_MAP[option.value];
    if (!keyId) continue; // local providers — no warning needed
    const baseLabel = option.dataset.baseLabel || option.textContent.replace(/^\u26A0\uFE0F\s*/, '');
    option.dataset.baseLabel = baseLabel;
    option.textContent = apiKeys[keyId] ? baseLabel : `\u26A0\uFE0F ${baseLabel}`;
  }
}

function updateKeyStatusElement(el, state) {
  if (!el) return;
  const STATUS_CONFIG = {
    missing: { class: 'key-missing', label: 'No key', ariaLabel: 'API key not configured', icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>` },
    saved: { class: 'key-saved', label: 'Saved', ariaLabel: 'API key saved (not yet verified)', icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/></svg>` },
    checking: { class: 'key-checking', label: 'Verifying', ariaLabel: 'Verifying API key', icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="2" x2="12" y2="6"/><line x1="12" y1="18" x2="12" y2="22"/><line x1="4.93" y1="4.93" x2="7.76" y2="7.76"/><line x1="16.24" y1="16.24" x2="19.07" y2="19.07"/><line x1="2" y1="12" x2="6" y2="12"/><line x1="18" y1="12" x2="22" y2="12"/><line x1="4.93" y1="19.07" x2="7.76" y2="16.24"/><line x1="16.24" y1="7.76" x2="19.07" y2="4.93"/></svg>` },
    valid: { class: 'key-valid', label: 'Verified', ariaLabel: 'API key verified successfully', icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>` },
    failed: { class: 'key-failed', label: 'Invalid', ariaLabel: 'API key verification failed', icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>` }
  };
  const config = STATUS_CONFIG[state] || STATUS_CONFIG.missing;
  el.className = `provider-key-status ${config.class}`;
  el.setAttribute('aria-label', config.ariaLabel);
  el.innerHTML = `${config.icon}<span>${config.label}</span>`;
}

function updateTranscriptionKeyStatusDot() {
  const provider = transcriptionProviderSelect ? transcriptionProviderSelect.value : '';
  const keyId = PROVIDER_KEY_MAP[provider];
  if (!keyId) return;

  const apiKeys = (appSettings && appSettings.transcription && appSettings.transcription.api_keys) || {};
  const hasKey = !!apiKeys[keyId];
  const validatedKeys = window.__nbpValidatedKeys || {};
  const failedKeys = window.__nbpFailedKeys || {};
  const key = apiKeys[keyId] || '';
  const isValidated = validatedKeys[keyId] === key && !!key;
  const isFailed = failedKeys[keyId] === key;
  const state = !hasKey ? 'missing' : isFailed ? 'failed' : isValidated ? 'valid' : 'saved';

  const statusEl = document.getElementById('cloud-provider-status');
  if (statusEl) {
    statusEl.style.display = '';
    updateKeyStatusElement(statusEl, state);
  }
  updateTranscriptionProviderWarnings();

  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (setKeyBtn) {
    setKeyBtn.style.display = hasKey ? 'none' : '';
  }
}

async function updateProviderVisibility() {
  if (!providerLocalSection) return;
  const provider = transcriptionProviderSelect.value;
  const isCloud = provider !== "FluidAudio" && provider !== "LocalWhisper";

  providerLocalSection.style.display = 'none';
  const statusEl = document.getElementById('cloud-provider-status');
  const setKeyBtn = document.getElementById('set-api-key-btn');
  if (statusEl) statusEl.style.display = 'none';
  if (setKeyBtn) setKeyBtn.style.display = 'none';

  if (provider === "LocalWhisper") {
    providerLocalSection.style.display = 'flex';
    await loadWhisperModelsAndState();
  } else if (isCloud) {
    updateTranscriptionKeyStatusDot();
  }
}




async function loadWhisperModelsAndState() {
  if (!whisperModelSelect) return;
  whisperModelSelect.disabled = true;
  try {
    availableModels = await invoke("get_whisper_models_info");
    const currentVal = appSettings?.transcription?.whisper_model || whisperModelSelect.value || "Base";

    whisperModelSelect.innerHTML = availableModels.map(m => {
      const sizeStr = m.size_mb ? `(~${m.size_mb} MB)` : '';
      const statusIcon = m.downloaded ? '✓' : '↓';

      let label = `${statusIcon} ${m.size} ${sizeStr}`;
      if (m.size === 'Base') label += ' (Recommended)';
      if (m.size === 'Large') label += ' (Best Quality)';

      return `<option value="${m.size}">${label}</option>`;
    }).join('');

    whisperModelSelect.value = currentVal;
    updateDownloadButton();
  } catch (e) {
    console.error(e);
  } finally {
    whisperModelSelect.disabled = false;
  }
}



function updateDownloadButton() {
  if (!downloadModelBtn || !whisperModelSelect) return;

  // If we are currently downloading, don't reset unless finished
  if (downloadModelBtn.dataset.downloading === "true") return;

  const selectedSize = whisperModelSelect.value;
  const model = availableModels.find(m => m.size === selectedSize);

  if (model) {
    if (model.downloaded) {
      // Show TRASH / DELETE
      downloadModelBtn.dataset.action = "delete";
      downloadModelBtn.title = "Delete Model";
      downloadModelBtn.classList.remove("mini-action-btn-primary");
      downloadModelBtn.style.color = "var(--text-danger, #ff4d4d)"; // Red for danger
      downloadModelBtn.innerHTML = `
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
           <polyline points="3 6 5 6 21 6"></polyline>
           <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
        </svg>`;
    } else {
      // Show DOWNLOAD
      downloadModelBtn.dataset.action = "download";
      downloadModelBtn.title = "Download Model";
      downloadModelBtn.classList.add("mini-action-btn-primary");
      downloadModelBtn.style.color = "var(--accent)";
      downloadModelBtn.innerHTML = `
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
            <polyline points="7 10 12 15 17 10"></polyline>
            <line x1="12" y1="15" x2="12" y2="3"></line>
        </svg>`;
    }
  }
}

// Download/Delete Logic
if (downloadModelBtn) {
  downloadModelBtn.addEventListener("click", async () => {
    if (!whisperModelSelect) return;
    const size = whisperModelSelect.value;
    const action = downloadModelBtn.dataset.action;

    // Prevent multiple clicks
    if (downloadModelBtn.dataset.downloading === "true") return;

    if (action === "delete") {
      const ok = await showConfirm('Delete Model?', `Are you sure you want to delete the ${size} model?`);
      if (!ok) return;
      try {
        await invoke("delete_whisper_model", { size });
        await loadWhisperModelsAndState();
      } catch (err) {
        console.error("Delete failed:", err);
      }
      return;
    }

    // --- DOWNLOAD LOGIC ---
    downloadModelBtn.dataset.downloading = "true";
    downloadModelBtn.title = "Downloading...";

    // Replace icon with PIE progress
    downloadModelBtn.innerHTML = `
          <svg class="progress-pie" width="24" height="24" viewBox="0 0 24 24">
             <circle cx="12" cy="12" r="10" stroke="var(--border)" stroke-width="1" fill="none" opacity="0.5"/>
             <path class="progress-pie__slice" fill="var(--accent)" d="" />
          </svg>
        `;

    try {
      // Listen for progress
      const unlisten = await window.__TAURI__.event.listen('download_progress', (event) => {
        const { percent } = event.payload;
        const slice = downloadModelBtn.querySelector('.progress-pie__slice');
        if (slice) {
          const d = getPiePath(12, 12, 10, percent);
          slice.setAttribute('d', d);
        } else {
          console.error("Progress pie slice element missing!");
        }
      });

      await invoke("download_whisper_model", { size });

      // Success
      unlisten();
      downloadModelBtn.dataset.downloading = "false";
      // Refresh state
      await loadWhisperModelsAndState();


    } catch (err) {
      console.error("Download failed:", err);
      // alert("Download failed: " + err); // Removed per user request
      downloadModelBtn.dataset.downloading = "false";
      updateDownloadButton();
    }
  });
}


function getPiePath(cx, cy, r, percentage) {
  if (percentage >= 100) {
    return `M ${cx}, ${cy} m -${r}, 0 a ${r},${r} 0 1,0 ${r * 2},0 a ${r},${r} 0 1,0 -${r * 2},0`;
  }

  // Start at top ( -90 deg)
  const startAngle = -Math.PI / 2;
  const angle = (percentage / 100) * 2 * Math.PI;
  const endAngle = startAngle + angle;

  const x1 = cx + r * Math.cos(startAngle);
  const y1 = cy + r * Math.sin(startAngle);

  const x2 = cx + r * Math.cos(endAngle);
  const y2 = cy + r * Math.sin(endAngle);

  const largeArc = percentage > 50 ? 1 : 0;

  // Move to center, Line to start, Arc to end, Close path
  return `M ${cx} ${cy} L ${x1} ${y1} A ${r} ${r} 0 ${largeArc} 1 ${x2} ${y2} Z`;
}

// Add change listener to update button state
if (whisperModelSelect) {
  whisperModelSelect.addEventListener('change', () => {
    updateDownloadButton();
    // Update settings immediately? Or wait for save?
    // Wait for save, but update internal state if needed
  });
}

function applyTheme(themeName) {
  // Migrate old theme names
  if (themeName === "light-pastel") themeName = "light";
  if (themeName === "deep-obsidian") themeName = "neon-purple";

  document.body.classList.remove("neon-purple", "deep-blue", "light");
  if (themeName !== "neon-purple") {
    document.body.classList.add(themeName);
  }

  appSettings.theme = themeName;

  themeButtons.forEach(btn => {
    btn.classList.toggle("active", btn.dataset.theme === themeName);
  });
}

// ===== SETTINGS TABS =====
function switchSettingsTab(tabName) {
  document.querySelectorAll('.settings-tab').forEach(t => {
    t.classList.toggle('active', t.dataset.tab === tabName);
  });
  document.querySelectorAll('.settings-tab-content').forEach(c => {
    c.classList.toggle('active', c.dataset.tab === tabName);
  });

  // Lazy-refresh tab data on activation as a hard safety net.
  if (tabName === 'pipelines') {
    if (typeof loadPipelineDefs === 'function') loadPipelineDefs();
  }
  if (tabName === 'prompts') {
    loadPromptTemplates();
  }

  const scroller = document.querySelector('#settings-view .detail-scroller');
  if (scroller) scroller.scrollTop = 0;
}

const settingsTabs = document.getElementById('settings-tabs');
if (settingsTabs) {
  settingsTabs.addEventListener('click', (e) => {
    const tab = e.target.closest('.settings-tab');
    if (tab) switchSettingsTab(tab.dataset.tab);
  });
}



function renderPipelineChips() {
  const chipBar = document.getElementById('pipeline-chip-bar');
  if (!chipBar) return;

  if (typeof allPipelineDefs === 'undefined' || allPipelineDefs.length === 0) {
    chipBar.innerHTML = '';
    chipBar.style.display = 'none';
    return;
  }

  chipBar.style.display = '';

  const MAX_CHIPS = 5;
  const visible = allPipelineDefs.slice(0, MAX_CHIPS);
  const overflow = allPipelineDefs.slice(MAX_CHIPS);

  let html = '';
  for (const p of visible) {
    let cls = 'pipeline-chip';
    if (isRecording && currentAssignedPipelines.has(p.name)) {
      cls += ' is-assigned';
    } else if (!isRecording && appSettings?.last_used_pipeline === p.name) {
      cls += ' is-last-used';
    }
    html += `<button class="${cls}" data-pipeline-name="${escapeHtml(p.name)}">${escapeHtml(p.name)}</button>`;
  }

  if (overflow.length > 0) {
    html += `<button class="chip-overflow-btn" id="chip-overflow-btn" aria-label="Show more pipelines" aria-haspopup="true">+${overflow.length}</button>`;
  }

  chipBar.innerHTML = html;

  chipBar.querySelectorAll('.pipeline-chip').forEach(chip => {
    chip.addEventListener('click', () => handleChipClick(chip.dataset.pipelineName));
  });

  const overflowBtn = chipBar.querySelector('#chip-overflow-btn');
  if (overflowBtn) {
    overflowBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      showOverflowPopover(overflow);
    });
  }
}

function showOverflowPopover(pipelines) {
  const existing = document.querySelector('.chip-overflow-popover');
  if (existing) existing.remove();

  const chipBar = document.getElementById('pipeline-chip-bar');
  if (!chipBar) return;

  const popover = document.createElement('div');
  popover.className = 'chip-overflow-popover';
  popover.setAttribute('role', 'menu');
  popover.style.maxHeight = '240px';
  popover.style.overflowY = 'auto';

  for (const p of pipelines) {
    let cls = 'pipeline-chip';
    if (isRecording && currentAssignedPipelines.has(p.name)) {
      cls += ' is-assigned';
    } else if (!isRecording && appSettings?.last_used_pipeline === p.name) {
      cls += ' is-last-used';
    }
    const btn = document.createElement('button');
    btn.className = cls;
    btn.dataset.pipelineName = p.name;
    btn.textContent = p.name;
    btn.setAttribute('role', 'menuitem');
    btn.addEventListener('click', () => {
      popover.remove();
      handleChipClick(p.name);
    });
    popover.appendChild(btn);
  }

  chipBar.appendChild(popover);

  function dismissOverflow(e) {
    if (!popover.contains(e.target)) {
      popover.remove();
    }
  }

  setTimeout(() => document.addEventListener('click', dismissOverflow, { once: true }), 0);
}

async function handleChipClick(pipelineName) {
  if (isRecordingBusy) return;
  if (isRecording) {
    // Mid-recording: assign pipeline, hide chips, let detail view handle it
    currentAssignedPipelines.add(pipelineName);
    try {
      if (selectedRecordingId) await invoke('assign_pipeline', { recordingId: selectedRecordingId, pipelineName });
    } catch (err) { console.error('Failed to assign pipeline via chip:', err); }

    // Hide chip bar — pipelines are managed in the detail view
    const chipBar = document.getElementById('pipeline-chip-bar');
    if (chipBar) chipBar.style.display = 'none';

    // Hide the available card + update runs
    if (selectedRecordingId) {
      const pipelineCardsEl = document.getElementById('pipeline-cards');
      if (pipelineCardsEl) {
        const card = pipelineCardsEl.querySelector(`.pipeline-card[data-pipeline="${pipelineName}"]`);
        if (card) card.style.display = 'none';
      }
      renderPipelineChips();
      renderPipelineStatus(selectedRecordingId);
    }
  } else {
    await startRecordingWithPipeline(pipelineName);
  }
}

async function startRecordingWithPipeline(pipelineName) {
  isRecordingBusy = true;
  ViewManager.showRecordings();

  const saveMixOnly = appSettings?.save_mix_only !== false;
  try {
    const metadata = await invoke('start_recording', { saveMixOnly });
    isRecording = true;
    currentAssignedPipelines = new Set([pipelineName]);
    await invoke('assign_pipeline', { recordingId: metadata.id, pipelineName });
    // Save last-used pipeline so chip bar highlights it on next launch
    appSettings.last_used_pipeline = pipelineName;
    await invoke('save_settings', { settings: appSettings });
    setRecordingUI(true);
    await loadRecordings();
    startTimer();
    startWaveformAnimation();
    showDetailView(metadata.id);
    // Hide chip bar — pipelines managed in detail view now
    const chipBar = document.getElementById('pipeline-chip-bar');
    if (chipBar) chipBar.style.display = 'none';
    startLiveTranscript(metadata.id);

    showToast('Recording started', 'info');
  } catch (error) {
    // Revert all state on failure
    isRecording = false;
    currentAssignedPipelines = new Set();
    stopTimer();
    stopWaveformAnimation();
    setRecordingUI(false);
    console.error('Failed to start recording with pipeline:', error);
    showToast('Failed to start: ' + error, 'error');
  } finally {
    isRecordingBusy = false;
  }
}


// ===== SETTINGS EVENT LISTENERS =====
if (settingsBtn) settingsBtn.addEventListener("click", () => {
  ViewManager.showSettings();
});
if (settingsBackBtn) settingsBackBtn.addEventListener("click", () => {
  ViewManager.showRecordings();
});

// ===== SIDEBAR NAVIGATION =====
document.querySelectorAll('.sidebar-nav-item').forEach(item => {
  item.addEventListener('click', () => {
    const view = item.dataset.view;
    if (view === 'recordings') ViewManager.showRecordings();
    else if (view === 'prompts') ViewManager.showPrompts();
    else if (view === 'pipelines') ViewManager.showPipelines();
    else if (view === 'settings') ViewManager.showSettings();
  });
});

// Auto-save: any change in settings triggers save + toast
const settingsContainer = document.getElementById('settings-view');
if (settingsContainer) {
  settingsContainer.addEventListener('change', () => saveSettings());
}

if (transcriptionEnabledCheckbox) {
  transcriptionEnabledCheckbox.addEventListener("change", updateTranscriptionVisibility);
}

if (transcriptionProviderSelect) {
  transcriptionProviderSelect.addEventListener("change", updateProviderVisibility);
}

// "Set API Key" button — switch to Models tab and focus the key input for the selected provider
const setApiKeyBtn = document.getElementById('set-api-key-btn');
if (setApiKeyBtn) {
  setApiKeyBtn.addEventListener('click', () => {
    const provider = transcriptionProviderSelect ? transcriptionProviderSelect.value : '';
    const keyId = PROVIDER_KEY_MAP[provider];
    switchSettingsTab('models');
    if (keyId) {
      setTimeout(() => {
        const input = document.getElementById(`settings-api-key-${keyId}`);
        if (input) {
          input.scrollIntoView({ behavior: 'smooth', block: 'center' });
          input.focus();
        }
      }, 100);
    }
  });
}


if (browseStorageBtn) {
  browseStorageBtn.addEventListener("click", async () => {
    try {
      const selected = await window.__TAURI__.dialog.open({
        directory: true,
        multiple: false,
        defaultPath: appSettings.storage_path
      });
      if (selected) {
        appSettings.storage_path = selected;
        storagePathInput.value = selected;
      }
    } catch (err) {
      console.error("Failed to browse:", err);
    }
  });
}

themeButtons.forEach(btn => {
  btn.addEventListener("click", () => {
    applyTheme(btn.dataset.theme);
    saveSettings();
  });
});

// ===== PROMPT TEMPLATE MANAGEMENT =====
let allPromptTemplates = [];
let editingPromptTemplate = null;

const promptTemplatesListEl = document.getElementById('prompt-templates-list');
const promptsListEl = document.getElementById('prompts-list');
const addPromptTemplateBtn = document.getElementById('add-prompt-template-btn');
const addPromptViewBtn = document.getElementById('add-prompt-view-btn');
const promptTemplateEditor = document.getElementById('prompt-template-editor');
const promptViewEditor = document.getElementById('prompt-view-editor');
const promptEditorTitle = document.getElementById('prompt-editor-title');
const promptViewEditorTitle = document.getElementById('prompt-view-editor-title');
const promptEditorName = document.getElementById('prompt-editor-name');
const promptViewName = document.getElementById('prompt-view-name');
const promptEditorText = document.getElementById('prompt-editor-text');
const promptViewText = document.getElementById('prompt-view-text');
const savePromptTemplateBtn = document.getElementById('save-prompt-template-btn');
const savePromptViewBtn = document.getElementById('save-prompt-view-btn');
const deletePromptTemplateBtn = document.getElementById('delete-prompt-template-btn');
const deletePromptViewBtn = document.getElementById('delete-prompt-view-btn');
const closePromptEditorBtn = document.getElementById('close-prompt-editor');
const closePromptViewBtn = document.getElementById('close-prompt-view-btn');

function pickVisible(primaryEl, secondaryEl) {
  if (primaryEl && primaryEl.offsetParent !== null) return primaryEl;
  if (secondaryEl && secondaryEl.offsetParent !== null) return secondaryEl;
  return primaryEl || secondaryEl;
}

function getActivePromptList() {
  return pickVisible(promptTemplatesListEl, promptsListEl);
}

function getActivePromptEditor() {
  return pickVisible(promptTemplateEditor, promptViewEditor);
}

function getActivePromptTitle() {
  return pickVisible(promptEditorTitle, promptViewEditorTitle);
}

function getActivePromptName() {
  return pickVisible(promptEditorName, promptViewName);
}

function getActivePromptText() {
  return pickVisible(promptEditorText, promptViewText);
}

function getActiveSaveBtn() {
  return pickVisible(savePromptTemplateBtn, savePromptViewBtn);
}

function getActiveDeleteBtn() {
  return pickVisible(deletePromptTemplateBtn, deletePromptViewBtn);
}

function getActiveCloseBtn() {
  return pickVisible(closePromptEditorBtn, closePromptViewBtn);
}

async function loadPromptTemplates() {
  try {
    allPromptTemplates = await invoke('list_prompt_templates');
    renderPromptTemplatesList();
  } catch (err) {
    console.error('Failed to load prompt templates:', err);
    const listEl = getActivePromptList();
    if (listEl) {
      listEl.innerHTML = `<div style="color: var(--danger); opacity: 0.9; font-size: 0.85rem; text-align: center; padding: 1rem;">Failed to load templates: ${escapeHtml(String(err))}</div>`;
    }
  }
}

function renderPromptTemplatesList() {
  const listEl = getActivePromptList();
  if (!listEl) return;
  if (allPromptTemplates.length === 0) {
    listEl.innerHTML = '<div style="color: var(--text-secondary); opacity: 0.6; font-size: 0.85rem; text-align: center; padding: 2rem;">No prompt templates yet.\n\nClick "+ New Prompt" to create one.</div>';
    return;
  }
  listEl.innerHTML = allPromptTemplates.map(t => {
    const safeName = escapeHtml(t.name);
    const safePreview = escapeHtml((t.prompt || '').substring(0, 100)) + (t.prompt && t.prompt.length > 100 ? '...' : '');
    const updated = t.updated_at ? new Date(t.updated_at).toLocaleDateString() : '';
    return `
    <div class="template-item" data-name="${safeName}">
      <div class="template-item-info">
        <div class="template-item-name">${safeName}</div>
        ${safePreview ? `<div class="template-item-preview">${safePreview}</div>` : ''}
        ${updated ? `<div class="template-item-date">${updated}</div>` : ''}
      </div>
      <button class="template-item-delete" data-name="${safeName}" title="Delete template"><span class="icon-trash"></span></button>
    </div>
  `;
  }).join('');

  listEl.querySelectorAll('.template-item').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.template-item-delete')) return;
      openPromptEditor(el.dataset.name);
    });
  });

  listEl.querySelectorAll('.template-item-delete').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      deletePromptTemplateWithConfirm(btn.dataset.name);
    });
  });
}

async function openPromptEditor(name) {
  const editorEl = getActivePromptEditor();
  if (!editorEl) return;
  if (name) {
    const t = allPromptTemplates.find(t => t.name === name);
    if (!t) return;
    editingPromptTemplate = name;
    const titleEl = getActivePromptTitle();
    const nameEl = getActivePromptName();
    const textEl = getActivePromptText();
    const deleteBtn = getActiveDeleteBtn();
    if (titleEl) titleEl.textContent = 'Edit Prompt';
    if (nameEl) nameEl.value = t.name;
    if (textEl) textEl.value = t.prompt || '';
    if (deleteBtn) deleteBtn.style.display = 'inline-block';
    
    const usageSection = document.getElementById('prompt-usage-section');
    const usageList = document.getElementById('prompt-usage-list');
    if (usageSection && usageList) {
      const pipelines = typeof allPipelineDefs !== 'undefined' ? allPipelineDefs : [];
      const referencing = [];
      for (const p of pipelines) {
        for (const step of (p.steps || [])) {
          if (step.config?.prompt_template === name) {
            referencing.push(p.name);
            break;
          }
        }
      }
      if (referencing.length > 0) {
        usageSection.style.display = 'block';
        usageList.innerHTML = referencing.map(pn => `
          <div class="prompt-usage-item">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <polyline points="16 18 22 12 16 6"/>
              <polyline points="8 6 2 12 8 18"/>
            </svg>
            ${escapeHtml(pn)}
          </div>
        `).join('');
      } else {
        usageSection.style.display = 'none';
      }
    }
  } else {
    editingPromptTemplate = null;
    const titleEl = getActivePromptTitle();
    const nameEl = getActivePromptName();
    const textEl = getActivePromptText();
    const deleteBtn = getActiveDeleteBtn();
    if (titleEl) titleEl.textContent = 'New Prompt';
    if (nameEl) nameEl.value = '';
    if (textEl) textEl.value = '';
    if (deleteBtn) deleteBtn.style.display = 'none';
    const usageSection = document.getElementById('prompt-usage-section');
    if (usageSection) usageSection.style.display = 'none';
  }
  editorEl.style.display = 'block';
  const nameEl = getActivePromptName();
  if (nameEl) nameEl.focus();
}

function closePromptEditor() {
  const editorEl = getActivePromptEditor();
  if (editorEl) editorEl.style.display = 'none';
  editingPromptTemplate = null;
}

if (addPromptTemplateBtn) addPromptTemplateBtn.addEventListener('click', () => openPromptEditor(null));
if (addPromptViewBtn) addPromptViewBtn.addEventListener('click', () => openPromptEditor(null));
if (closePromptEditorBtn) closePromptEditorBtn.addEventListener('click', closePromptEditor);
if (closePromptViewBtn) closePromptViewBtn.addEventListener('click', closePromptEditor);

function findPipelinesReferencingPrompt(promptName) {
  const pipelines = typeof allPipelineDefs !== 'undefined' ? allPipelineDefs : [];
  const referencing = [];
  for (const p of pipelines) {
    for (const step of (p.steps || [])) {
      if (step.config?.prompt_template === promptName) {
        referencing.push(p);
        break;
      }
    }
  }
  return referencing;
}

async function savePromptTemplate() {
  const nameEl = getActivePromptName();
  const textEl = getActivePromptText();
  const name = nameEl?.value.trim() || '';
  const prompt = textEl?.value.trim() || '';
  if (!name) { showToast('Name is required', 'error'); return; }
  if (!prompt) { showToast('Prompt text is required', 'error'); return; }

  try {
    const template = {
      name,
      prompt,
      created_at: editingPromptTemplate ? (allPromptTemplates.find(t => t.name === editingPromptTemplate)?.created_at || new Date().toISOString()) : new Date().toISOString(),
      updated_at: new Date().toISOString()
    };

    if (editingPromptTemplate && editingPromptTemplate !== name) {
      const referencing = findPipelinesReferencingPrompt(editingPromptTemplate);
      if (referencing.length > 0) {
        const pipelineNames = referencing.map(p => p.name).join(', ');
        const ok = await showConfirm('Rename will update pipelines', `Renaming "${editingPromptTemplate}" to "${name}" will update ${referencing.length} pipeline(s): ${pipelineNames}. Continue?`);
        if (!ok) return;
        for (const pipeline of referencing) {
          for (const step of (pipeline.steps || [])) {
            if (step.config?.prompt_template === editingPromptTemplate) {
              step.config.prompt_template = name;
            }
          }
          await invoke('save_pipeline', { pipeline });
        }
        if (typeof loadPipelineDefs === 'function') await loadPipelineDefs();
      }
      await invoke('delete_prompt_template', { name: editingPromptTemplate, force: false });
    }
    await invoke('save_prompt_template', { template });
    closePromptEditor();
    await loadPromptTemplates();
    showToast('Prompt saved', 'info');
  } catch (err) {
    console.error('Failed to save prompt template:', err);
    showToast('Failed to save: ' + err, 'error');
  }
}

async function deletePromptTemplate() {
  if (!editingPromptTemplate) return;
  const referencing = findPipelinesReferencingPrompt(editingPromptTemplate);
  let ok;
  if (referencing.length > 0) {
    const pipelineNames = referencing.map(p => p.name).join(', ');
    ok = await showConfirm('Prompt is in use', `Prompt "${editingPromptTemplate}" is used by ${referencing.length} pipeline(s): ${pipelineNames}. Delete anyway? Steps using this prompt will need manual fix.`);
  } else {
    ok = await showConfirm('Delete Prompt?', `Delete prompt "${editingPromptTemplate}"? This cannot be undone.`);
  }
  if (!ok) return;
  try {
    await invoke('delete_prompt_template', { name: editingPromptTemplate, force: referencing.length > 0 });
    closePromptEditor();
    await loadPromptTemplates();
    showToast('Prompt deleted', 'info');
  } catch (err) {
    console.error('Failed to delete prompt template:', err);
    showToast('Failed to delete: ' + err, 'error');
  }
}

async function deletePromptTemplateWithConfirm(name) {
  if (!name) return;
  const referencing = findPipelinesReferencingPrompt(name);
  let ok;
  if (referencing.length > 0) {
    const pipelineNames = referencing.map(p => p.name).join(', ');
    ok = await showConfirm('Prompt is in use', `Prompt "${name}" is used by ${referencing.length} pipeline(s): ${pipelineNames}. Delete anyway? Steps using this prompt will need manual fix.`);
  } else {
    ok = await showConfirm('Delete Prompt?', `Delete prompt "${name}"? This cannot be undone.`);
  }
  if (!ok) return;
  try {
    await invoke('delete_prompt_template', { name, force: referencing.length > 0 });
    await loadPromptTemplates();
    showToast('Prompt deleted', 'info');
  } catch (err) {
    console.error('Failed to delete prompt template:', err);
    showToast('Failed to delete: ' + err, 'error');
  }
}

if (savePromptTemplateBtn) savePromptTemplateBtn.addEventListener('click', savePromptTemplate);
if (savePromptViewBtn) savePromptViewBtn.addEventListener('click', savePromptTemplate);
if (deletePromptTemplateBtn) deletePromptTemplateBtn.addEventListener('click', deletePromptTemplate);
if (deletePromptViewBtn) deletePromptViewBtn.addEventListener('click', deletePromptTemplate);

// ===== SLACK INTEGRATIONS =====
let slackIntegrations = {};

const addSlackBtn = document.getElementById('add-slack-btn');
const addSlackModal = document.getElementById('add-slack-modal');
const slackTokenInput = document.getElementById('slack-token-input');
const slackSaveBtn = document.getElementById('slack-save-btn');
const slackCancelBtn = document.getElementById('slack-cancel-btn');
async function loadSlackIntegrations() {
  try {
    slackIntegrations = await invoke('list_slack_integrations');
  } catch (err) {
    console.error('Failed to load Slack integrations:', err);
  }
}

// Save original modal HTML for restore after success
const slackModalOriginalHTML = addSlackModal ? addSlackModal.querySelector('.modal-card')?.innerHTML : '';

function wireSlackModalButtons() {
  const cancelBtn = document.getElementById('slack-cancel-btn');
  const saveBtn = document.getElementById('slack-save-btn');

  if (cancelBtn) {
    cancelBtn.addEventListener('click', () => {
      addSlackModal.style.display = 'none';
    });
  }

  if (saveBtn) {
    saveBtn.addEventListener('click', async () => {
      const tokenInput = document.getElementById('slack-token-input');
      const token = tokenInput?.value.trim();
      if (!token) {
        showToast('Please enter a bot token', 'error');
        return;
      }
      if (!token.startsWith('xoxb-')) {
        showToast('Invalid token format. Bot tokens start with xoxb-', 'error');
        return;
      }

      const id = crypto.randomUUID();
      saveBtn.disabled = true;
      saveBtn.textContent = 'Connecting...';

      try {
        const workspaceName = await invoke('add_slack_integration', { id, token });
        const modalCard = addSlackModal.querySelector('.modal-card');
        if (modalCard) {
          modalCard.innerHTML = `
            <div style="text-align: center; padding: 32px 16px;">
              <div style="font-size: 2.5rem; margin-bottom: 12px;">&#10003;</div>
              <h3 style="margin: 0 0 8px;">${escapeHtml(workspaceName)} connected</h3>
              <p style="color: var(--text-secondary); margin: 0 0 24px;">Slack workspace added successfully</p>
              <button class="modal-btn primary" id="slack-success-done">Done</button>
            </div>
          `;
          document.getElementById('slack-success-done').addEventListener('click', () => {
            addSlackModal.style.display = 'none';
            modalCard.innerHTML = slackModalOriginalHTML;
            wireSlackModalButtons();
          });
        }
        await loadSlackIntegrations();
        // Reload appSettings so the next saveSettings() won't overwrite Slack data
        appSettings = await invoke('load_settings');
        if (typeof renderConnectedIntegrations === 'function') renderConnectedIntegrations();
      } catch (err) {
        showToast(`Failed to add Slack workspace: ${err}`, 'error');
        saveBtn.disabled = false;
        saveBtn.textContent = 'Save';
      }
    });
  }
}

wireSlackModalButtons();

// ===== LOAD TEMPLATES =====
async function loadTemplates() {
  if (!templateSelect) return;

  try {
    const templates = await invoke('list_templates');
    templateSelect.innerHTML = '<option value="">Select template...</option>' +
      templates.map(t => `<option value="${escapeHtml(t.name)}">${escapeHtml(t.description)}</option>`).join('');
  } catch (err) {
    console.error('Failed to load templates:', err);
  }
}

// ===== INIT =====
async function init() {
  await loadSettings();
  await loadRecordings();
  await loadPipelineDefs();
  renderRecordingsList(); // re-render after pipeline defs loaded so flow chips appear
  await loadTemplates();
  await loadPromptTemplates();
  await loadSlackIntegrations();
  try {
    const version = await invoke("get_app_version");
    const versionEl = document.getElementById("app-version");
    if (versionEl) versionEl.textContent = `v${version} `;
  } catch (err) {
    console.error("Failed to fetch version:", err);
  }

  await updatePermissionStatus();

  // Auto-transcribe + auto-execute when recording finalization completes (EXEC-01)
  window.__TAURI__.event.listen('recording_complete', async (event) => {
    const recordingId = event.payload;
    await loadRecordings();
    if (selectedRecordingId === recordingId) showDetailView(recordingId);

    if (appSettings?.transcription?.enabled) {
      const pipelines = pendingAutoExec.get(recordingId) || [];
      pendingAutoExec.delete(recordingId);
      autoTranscribeAndExecute(recordingId, pipelines);
    } else {
      pendingAutoExec.delete(recordingId);
    }
  });

  // Show onboarding only if never completed before
  if (!appSettings.onboarding_completed) {
    onboardingOverlay.style.display = 'flex';
  }
}

async function bootstrapApp() {
  await init().catch(e => console.error('Init failed:', e));

  // Initialize health check controls (event listeners wired once)
  if (typeof initHealthCheck === 'function') initHealthCheck();

  // Schedule DOM audit after browser is idle
  const scheduleAudit = () => {
    if (typeof runHealthAudit === 'function') {
      runHealthAudit();
      // Trigger walkthrough on first launch (after audit so badge is visible)
      // Only if onboarding is already completed (permissions granted)
      if (typeof appSettings !== 'undefined' &&
          appSettings.onboarding_completed &&
          !appSettings.walkthrough_completed &&
          typeof startWalkthrough === 'function') {
        startWalkthrough();
      }
    }
  };
  if (typeof requestIdleCallback !== 'undefined') {
    requestIdleCallback(scheduleAudit, { timeout: 2000 });
  } else {
    setTimeout(scheduleAudit, 500);
  }
}

if (document.readyState === 'complete') {
  setTimeout(() => { bootstrapApp(); }, 0);
} else {
  window.addEventListener('load', () => { bootstrapApp(); }, { once: true });
}
