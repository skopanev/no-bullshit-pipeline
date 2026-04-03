// Pipeline definitions list: loading and rendering

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { currentAssignedPipelines } from '../core/state.js';
import * as pipelineState from './state.js';
import { renderPipelineFlowHTML } from './flow-renderer.js';
import { loadCliAvailability } from './models.js';
import { openPipelineEditor } from './editor.js';

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
    if (typeof renderPipelineChips === 'function') renderPipelineChips();
    if (typeof populateDefaultPipelineSelect === 'function') populateDefaultPipelineSelect();
    // Pre-load CLI availability in background
    loadCliAvailability();
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
    </div>`;
  }).join('');

  pipelineDefsListEl.querySelectorAll('.pipeline-def-item').forEach(el => {
    el.addEventListener('click', () => openPipelineEditor(el.dataset.name));
  });
}

// Wire add button
if (addPipelineDefBtn) {
  addPipelineDefBtn.addEventListener('click', () => openPipelineEditor(null));
}
