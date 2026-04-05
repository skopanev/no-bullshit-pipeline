// Pipeline chip bar: quick-assign pipelines from the main recording view

import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { escapeHtml } from '../core/utils.js';
import { showToast } from '../ui/toast.js';
import { ViewManager } from '../ui/view-manager.js';
import * as pipelineState from './state.js';
import { renderPipelineStatus } from '../recording/pipeline-status.js';
import { startRecording } from '../recording/controls.js';
import { setRecordingUI } from '../recording/controls.js';
import { loadRecordings } from '../recording/list.js';
import { showDetailView } from '../recording/detail.js';
import { startTimer } from '../recording/timer.js';
import { startWaveformAnimation } from '../recording/waveform.js';
import { startLiveTranscript } from '../recording/live-transcript.js';

const chipBar = document.getElementById('pipeline-chip-bar');

export function renderPipelineChips() {
  if (!chipBar) return;

  if (!pipelineState.allPipelineDefs || pipelineState.allPipelineDefs.length === 0) {
    chipBar.innerHTML = '';
    chipBar.style.display = 'none';
    return;
  }

  chipBar.style.display = '';
  const MAX_CHIPS = 5;
  const visible = pipelineState.allPipelineDefs.slice(0, MAX_CHIPS);
  const overflow = pipelineState.allPipelineDefs.slice(MAX_CHIPS);

  let html = '';
  for (const p of visible) {
    let cls = 'pipeline-chip';
    if (state.isRecording && state.currentAssignedPipelines.has(p.name)) {
      cls += ' is-assigned';
    } else if (!state.isRecording && state.appSettings?.last_used_pipeline === p.name) {
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
  if (!chipBar) return;

  const popover = document.createElement('div');
  popover.className = 'chip-overflow-popover';
  popover.setAttribute('role', 'menu');
  popover.style.maxHeight = '240px';
  popover.style.overflowY = 'auto';

  for (const p of pipelines) {
    let cls = 'pipeline-chip';
    if (state.isRecording && state.currentAssignedPipelines.has(p.name)) {
      cls += ' is-assigned';
    } else if (!state.isRecording && state.appSettings?.last_used_pipeline === p.name) {
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
  setTimeout(() => document.addEventListener('click', (e) => {
    if (!popover.contains(e.target)) popover.remove();
  }, { once: true }), 0);
}

async function handleChipClick(pipelineName) {
  if (state.isRecordingBusy) return;

  if (state.isRecording) {
    state.currentAssignedPipelines.add(pipelineName);
    try {
      if (state.selectedRecordingId) await invoke('assign_pipeline', { recordingId: state.selectedRecordingId, pipelineName });
    } catch (err) { console.error('Failed to assign pipeline via chip:', err); }

    if (chipBar) chipBar.style.display = 'none';

    if (state.selectedRecordingId) {
      const pipelineCardsEl = document.getElementById('pipeline-cards');
      if (pipelineCardsEl) {
        const card = pipelineCardsEl.querySelector(`.pipeline-card[data-pipeline="${pipelineName}"]`);
        if (card) card.style.display = 'none';
      }
      renderPipelineChips();
      renderPipelineStatus(state.selectedRecordingId);
    }
  } else {
    await startRecordingWithPipeline(pipelineName);
  }
}

export async function startRecordingWithPipeline(pipelineName) {
  state.setIsRecordingBusy(true);
  ViewManager.showRecordings();

  const saveMixOnly = state.appSettings?.save_mix_only !== false;
  try {
    const metadata = await invoke('start_recording', { saveMixOnly });
    state.setIsRecording(true);
    state.setCurrentAssignedPipelines(new Set([pipelineName]));
    await invoke('assign_pipeline', { recordingId: metadata.id, pipelineName });
    state.appSettings.last_used_pipeline = pipelineName;
    await invoke('save_settings', { settings: state.appSettings });
    setRecordingUI(true);
    await loadRecordings();
    startTimer();
    startWaveformAnimation();
    showDetailView(metadata.id);
    if (chipBar) chipBar.style.display = 'none';
    startLiveTranscript(metadata.id);
    showToast('Recording started', 'info');
  } catch (error) {
    state.setIsRecording(false);
    state.setCurrentAssignedPipelines(new Set());
    setRecordingUI(false);
    console.error('Failed to start recording with pipeline:', error);
    showToast('Failed to start: ' + error, 'error');
  } finally {
    state.setIsRecordingBusy(false);
  }
}
