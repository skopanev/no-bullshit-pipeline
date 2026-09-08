// Pipeline definitions list: loading and rendering

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { showToast } from '../ui/toast.js';
import { currentAssignedPipelines } from '../core/state.js';
import * as pipelineState from './state.js';
import { renderPipelineFlowHTML } from './flow-renderer.js';
import { openPipelineEditor } from './editor.js';
import { renderPipelineChips } from './chips.js';

// DOM refs
const pipelineDefsListEl = document.getElementById('pipeline-defs-list');
const addPipelineDefBtn = document.getElementById('add-pipeline-def-btn');

export async function loadPipelineDefs() {
  try {
    pipelineState.setAllPipelineDefs(await invoke('list_pipelines'));

    // Prune assigned pipelines that no longer exist
    if (currentAssignedPipelines && currentAssignedPipelines.size > 0) {
      const existingNames = new Set(pipelineState.allPipelineDefs.map(p => p.name));
      for (const name of [...currentAssignedPipelines]) {
        if (!existingNames.has(name)) currentAssignedPipelines.delete(name);
      }
    }

    renderPipelineDefsList();
    renderPipelineChips();
  } catch (err) {
    console.error('Failed to load pipelines:', err);
    if (pipelineDefsListEl) {
      pipelineDefsListEl.innerHTML = `<div style="color: var(--danger); opacity: 0.9; font-size: 0.85rem;">Failed to load pipelines: ${escapeHtml(String(err))}</div>`;
    }
  }
}

export function renderPipelineDefsList() {
  if (!pipelineDefsListEl) return;
  if (pipelineState.allPipelineDefs.length === 0) {
    pipelineDefsListEl.innerHTML = '<div style="color: var(--text-secondary); opacity: 0.6; font-size: 0.85rem;">No pipelines yet.</div>';
    return;
  }
  pipelineDefsListEl.innerHTML = pipelineState.allPipelineDefs.map(p => {
    const safeName = escapeHtml(p.name);
    const safeDesc = escapeHtml(p.description || '');
    const updated = p.updated_at ? new Date(p.updated_at).toLocaleDateString() : '';
    const flowHtml = renderPipelineFlowHTML(p.steps || [], { compact: true });
    const meta = [safeDesc, updated].filter(Boolean).join(' &middot; ');
    return `
    <div class="pipeline-def-item" data-name="${safeName}">
      <div class="pipeline-def-info">
        <div class="pipeline-def-name">${safeName}</div>
        <div class="pipeline-def-flow">${flowHtml}</div>
        ${meta ? `<div class="pipeline-def-desc">${meta}</div>` : ''}
      </div>
      <div class="pipeline-def-autorun" title="Run automatically after every recording">
        <span class="pipeline-def-autorun-label">Auto-run</span>
        <label class="toggle-switch">
          <input type="checkbox" data-name="${safeName}" ${p.auto_run ? 'checked' : ''} />
          <span class="slider round"></span>
        </label>
      </div>
    </div>`;
  }).join('');

  pipelineDefsListEl.querySelectorAll('.pipeline-def-item').forEach(el => {
    el.addEventListener('click', (e) => {
      // Clicking the auto-run toggle must not open the editor.
      if (e.target.closest('.pipeline-def-autorun')) return;
      openPipelineEditor(el.dataset.name);
    });
  });

  // Toggle auto_run inline — saves immediately without opening the editor.
  pipelineDefsListEl.querySelectorAll('.pipeline-def-autorun input').forEach(input => {
    input.addEventListener('change', async (e) => {
      e.stopPropagation();
      const name = input.dataset.name;
      const p = pipelineState.allPipelineDefs.find(x => x.name === name);
      if (!p) return;
      const updated = { ...p, auto_run: input.checked };
      try {
        await invoke('save_pipeline', { pipeline: updated, previousName: name });
        showToast('Pipeline saved', 'success');
        await loadPipelineDefs();
      } catch (err) {
        console.error('Failed to toggle auto-run:', err);
        input.checked = !input.checked;
        showToast(`Failed to save: ${err}`, 'error');
      }
    });
  });
}

// Wire add button
if (addPipelineDefBtn) {
  addPipelineDefBtn.addEventListener('click', () => openPipelineEditor(null));
}
