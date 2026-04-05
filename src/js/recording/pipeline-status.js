import { invoke, listen } from '../core/tauri.js';
import * as state from '../core/state.js';
import { escapeHtml } from '../core/utils.js';
import { emit } from '../core/events.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { showToast } from '../ui/toast.js';
import { looksLikeMarkdown } from '../ui/markdown.js';
import { loadRecordings } from './list.js';
import { allPipelineDefs } from '../pipeline/state.js';
import { renderPipelineFlowHTML } from '../pipeline/flow-renderer.js';

let pipelineProgressUnlisten = null;
let pipelineRunningSteps = {};
let stepElapsedTimer = null;

const PIPELINE_STATUS_DISPLAY = {
  waiting: 'Waiting',
  running: 'Running',
  done: 'Done',
  partial: 'Failed'
};

export function cleanupPipelineProgress() {
  if (pipelineProgressUnlisten) {
    pipelineProgressUnlisten();
    pipelineProgressUnlisten = null;
  }
  pipelineRunningSteps = {};
  if (stepElapsedTimer) { clearInterval(stepElapsedTimer); stepElapsedTimer = null; }
}

export async function subscribeToProgress(recordingId) {
  cleanupPipelineProgress();

  pipelineProgressUnlisten = await listen('pipeline-progress', (event) => {
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

export async function renderPipelineStatus(recordingId) {
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
    for (const st of states) {
      const isDeletedPipeline = st.status === 'partial' && st.error && st.error.includes('deleted');
      const displayText = isDeletedPipeline ? 'Skipped' : (PIPELINE_STATUS_DISPLAY[st.status] || st.status);
      const badgeStatus = isDeletedPipeline ? 'skipped' : st.status;
      const runBtn = st.status === 'waiting'
        ? `<button class="pipeline-run-btn" data-pipeline="${escapeHtml(st.name)}" data-run-index="${st.run_index || 0}">Run</button>`
        : '';
      const deleteBtn = st.status !== 'running'
        ? `<button class="pipeline-run-delete" data-run-id="${escapeHtml(st.id)}" title="Delete run">&times;</button>`
        : '';
      const pipelineErrorHtml = isDeletedPipeline
        ? `<div class="pipeline-state-error">Pipeline was deleted before execution</div>`
        : '';

      html += `<div class="pipeline-status-row">
  <span class="pipeline-status-name">${escapeHtml(st.name)}</span>
  <span class="pipeline-status-badge status-${escapeHtml(badgeStatus)}">${escapeHtml(displayText)}</span>
  ${runBtn}${deleteBtn}
</div>${pipelineErrorHtml}`;

      const pipelineDef = allPipelineDefs && allPipelineDefs.find(d => d.name === st.name);
      const canRenderFlow = !!pipelineDef;
      const runningKey = `${recordingId}:${st.name}`;
      const runningInfo = (st.status === 'running') ? pipelineRunningSteps[runningKey] : null;

      if (st.status === 'running' || st.status === 'partial' || st.status === 'done') {
        html += await buildStepDetailHtml(recordingId, st, pipelineDef, canRenderFlow, runningInfo);
      } else if (canRenderFlow && pipelineDef.steps && pipelineDef.steps.length > 0) {
        html += `<div class="pipeline-run-flow">${renderPipelineFlowHTML(pipelineDef.steps, { compact: false, statuses: {} })}</div>`;
      }
    }

    content.innerHTML = html;
    wireStatusInteractions(content, recordingId);
  } catch (e) {
    console.error('Failed to load pipeline states:', e);
    section.style.display = 'none';
  }
}

async function buildStepDetailHtml(recordingId, st, pipelineDef, canRenderFlow, runningInfo) {
  let html = '';
  try {
    const steps = await invoke('get_step_outputs', { recordingId, pipelineName: st.name, runIndex: st.run_index || 0 });
    if (!steps || steps.length === 0) return html;

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
      html += buildStepRowHtml(step, runningInfo);
    }
    html += '</div>';
  } catch (e) {
    console.error('Failed to load step outputs:', e);
  }
  return html;
}

function buildStepRowHtml(step, runningInfo) {
  let rowClass = 'pipeline-step-row';
  let iconHtml = '', extraHtml = '', progressBarHtml = '';

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
    rowClass += ' step-running';
    iconHtml = '<span class="step-status-icon">&#9679;</span>';
    const elapsedSecs = Math.round((Date.now() - runningInfo.start_time) / 1000);
    extraHtml = `<span class="step-elapsed" data-start="${runningInfo.start_time}">${elapsedSecs}s</span>`;
    progressBarHtml = '<div class="step-progress-bar-container"><div class="step-progress-bar-inner"></div></div>';
  } else {
    rowClass += ' step-pending';
    iconHtml = '<span class="step-status-icon">&#9675;</span>';
  }

  let durationHtml = '';
  if (step.duration_secs != null && step.duration_secs > 0) {
    const formatted = step.duration_secs >= 60
      ? `${Math.floor(step.duration_secs / 60)}m ${Math.round(step.duration_secs % 60)}s`
      : `${step.duration_secs.toFixed(1)}s`;
    durationHtml = `<span class="step-duration">${formatted}</span>`;
  }

  let togglesHtml = '';
  if (step.output) togglesHtml += `<button class="step-inline-toggle" data-target="output" title="Show details">Details</button>`;
  if (step.augmented_prompt) togglesHtml += `<button class="step-inline-toggle" data-target="prompt" title="Show augmented prompt">Prompt</button>`;

  let html = `<div class="${rowClass}">
  ${iconHtml}
  <span class="step-name">${escapeHtml(step.name)}</span>
  ${durationHtml}${extraHtml}
  <span class="step-toggles">${togglesHtml}</span>
</div>${progressBarHtml}`;

  if (step.output) {
    const outputHtml = looksLikeMarkdown(step.output)
      ? marked.parse(step.output)
      : `<pre>${escapeHtml(step.output)}</pre>`;
    html += `<div class="step-expand-panel" data-panel="output" style="display:none;">${outputHtml}</div>`;
  }
  if (step.augmented_prompt) {
    html += `<div class="step-expand-panel" data-panel="prompt" style="display:none;"><pre>${escapeHtml(step.augmented_prompt)}</pre></div>`;
  }
  return html;
}

function wireStatusInteractions(content, recordingId) {
  // Elapsed time ticker
  if (stepElapsedTimer) { clearInterval(stepElapsedTimer); stepElapsedTimer = null; }
  if (Object.keys(pipelineRunningSteps).length > 0) {
    stepElapsedTimer = setInterval(() => {
      content.querySelectorAll('.step-elapsed[data-start]').forEach(el => {
        const start = parseInt(el.dataset.start, 10);
        el.textContent = Math.round((Date.now() - start) / 1000) + 's';
      });
    }, 1000);
  }

  // Run buttons
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
      if (state.selectedRecordingId === recordingId) emit('recording:showDetail', recordingId);
    });
  });

  // Expand toggles
  content.querySelectorAll('.step-inline-toggle').forEach(btn => {
    btn.addEventListener('click', () => {
      const panelType = btn.dataset.target;
      const stepRow = btn.closest('.pipeline-step-row');
      if (!stepRow) return;
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

  // Delete buttons
  content.querySelectorAll('.pipeline-run-delete').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const ok = await showConfirm('Delete Pipeline Run?', 'This will remove the run and its output.');
      if (!ok) return;
      const runId = btn.dataset.runId;
      const row = btn.closest('.pipeline-status-row');
      const pipelineName = row ? row.querySelector('.pipeline-status-name')?.textContent : null;
      try {
        await invoke('remove_pipeline_run', { recordingId, runId });
        if (pipelineName && state.currentAssignedPipelines.has(pipelineName)) {
          state.currentAssignedPipelines.delete(pipelineName);
          emit('pipelines:renderChips');
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
      if (state.selectedRecordingId === recordingId) {
        renderPipelineStatus(recordingId);
      }
    });
  });
}
