// Pipeline editor: open/close, step chip rendering, save/delete

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { showToast } from '../ui/toast.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { CONNECTOR_META } from './constants.js';
import * as appState from '../core/state.js';
import * as pipelineState from './state.js';
import { stepSubLabel, stepDisplayLabel } from './flow-renderer.js';
import { maybeAutoName } from './delivery-options.js';
import { showStepEditor, addNewStep } from './step-editor.js';
import { loadPipelineDefs } from './defs-list.js';

// DOM refs
const pipelineEditor = document.getElementById('pipeline-editor');
const pipelineEditorTitle = document.getElementById('pipeline-editor-title');
const pipelineEditorName = document.getElementById('pipeline-editor-name');
const pipelineEditorDesc = document.getElementById('pipeline-editor-desc');
const pipelineStepsListEl = document.getElementById('pipeline-steps-list');
const stepEditorPanelEl = document.getElementById('step-editor-panel');
const savePipelineDefBtn = document.getElementById('save-pipeline-def-btn');
const deletePipelineDefBtn = document.getElementById('delete-pipeline-def-btn');
const closePipelineEditorBtn = document.getElementById('close-pipeline-editor');

// Kept as a no-op for legacy call sites — the old `step.input` field is gone
// (the step chain is strictly linear: each step eats the previous step's
// output). Removed when no callers remain.
export function fixStepInputs() {}

export function openPipelineEditor(name) {
  if (!pipelineEditor) return;
  pipelineState.setLastAutoName('');
  if (name) {
    const p = pipelineState.allPipelineDefs.find(p => p.name === name);
    if (!p) return;
    pipelineState.setEditingPipelineDef(name);
    pipelineEditorTitle.textContent = 'Edit Pipeline';
    pipelineEditorName.value = p.name;
    pipelineEditorDesc.value = p.description || '';
    pipelineState.setPipelineEditorSteps(JSON.parse(JSON.stringify(p.steps)));
    if (deletePipelineDefBtn) deletePipelineDefBtn.style.display = 'inline-block';
  } else {
    pipelineState.setEditingPipelineDef(null);
    pipelineEditorTitle.textContent = 'New Pipeline';
    pipelineEditorName.value = '';
    pipelineEditorDesc.value = '';
    pipelineState.setPipelineEditorSteps([]);
    if (deletePipelineDefBtn) deletePipelineDefBtn.style.display = 'none';
  }
  pipelineState.setEditingStepIndex(null);
  closeStepEditorPanel();
  pipelineEditor.style.display = 'block';
  renderPipelineSteps();
  pipelineEditorName.focus();
}

export function closePipelineEditor() {
  if (pipelineEditor) pipelineEditor.style.display = 'none';
  pipelineState.setEditingPipelineDef(null);
  pipelineState.setEditingStepIndex(null);
  pipelineState.setPipelineEditorSteps([]);
  closeStepEditorPanel();
}

export function closeStepEditorPanel() {
  if (stepEditorPanelEl) {
    stepEditorPanelEl.innerHTML = '';
    stepEditorPanelEl.style.display = 'none';
  }
}

export function renderPipelineSteps() {
  if (!pipelineStepsListEl) return;

  const MIC_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>`;

  let html = `<div class="pflow-chip pflow-chip--source" title="Transcript">
    <div class="pflow-chip-icon" style="background:var(--accent-soft);color:var(--accent);">${MIC_SVG}</div>
    <span class="pflow-chip-label">Transcript</span>
  </div>`;

  for (let i = 0; i < pipelineState.pipelineEditorSteps.length; i++) {
    const step = pipelineState.pipelineEditorSteps[i];
    const typeKey = step.step_type || 'cli_agent';
    const meta = CONNECTOR_META[typeKey] || {
      abbr: typeKey.substring(0, 2).toUpperCase(),
      textColor: 'var(--text-primary)',
      bgColor: 'var(--bg-input)',
    };
    const bg = meta.bgColor;
    const fg = meta.textColor;
    const iconContent = meta.svg
      ? meta.svg
      : `<span style="font-size:7px;font-weight:800;color:${fg};">${meta.abbr}</span>`;

    // Label = step type (CLI binary / Shell / Save to folder); sub = the
    // distinguishing detail (model / folder), shown only when present.
    const label = escapeHtml(stepDisplayLabel(step));
    const subLabel = stepSubLabel(step);
    const subHtml = subLabel ? `<span class="pflow-chip-sub">${escapeHtml(subLabel)}</span>` : '';
    const isEditing = pipelineState.editingStepIndex === i;

    html += `<div class="pflow-chip${isEditing ? ' pflow-chip--editing' : ''}" data-index="${i}" title="${label}">
      <span class="pflow-chip-num">${i + 1}</span>
      <div class="pflow-chip-icon" style="background:${bg};color:${fg};">${iconContent}</div>
      <div class="pflow-chip-label-group">
        <span class="pflow-chip-label">${label}</span>
        ${subHtml}
      </div>
      <button class="pflow-chip-remove" data-index="${i}" title="Remove step" aria-label="Remove step">\u00d7</button>
    </div>`;
  }

  html += `<div class="pflow-chip pflow-chip--add" id="add-step-tile" title="Add step">
    <div class="pflow-chip-icon">+</div>
    <span class="pflow-chip-label">Add Step</span>
  </div>`;

  pipelineStepsListEl.innerHTML = `<div class="pflow pflow--builder">${html}</div>`;
  const pfFlowEl = pipelineStepsListEl.querySelector('.pflow--builder');

  // Wire: click step chip to open editor
  pfFlowEl.querySelectorAll('.pflow-chip[data-index]').forEach(chip => {
    chip.addEventListener('click', (e) => {
      if (e.target.closest('.pflow-chip-remove')) return;
      showStepEditor(parseInt(chip.dataset.index));
    });
  });

  // Wire: remove buttons
  pfFlowEl.querySelectorAll('.pflow-chip-remove').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const idx = parseInt(btn.dataset.index);
      const stepName = pipelineState.pipelineEditorSteps[idx]?.name || `Step ${idx + 1}`;
      const ok = await showConfirm('Remove Step?', `Remove step "${stepName}" from pipeline?`);
      if (!ok) return;
      pipelineState.pipelineEditorSteps.splice(idx, 1);
      if (pipelineState.editingStepIndex === idx) {
        pipelineState.setEditingStepIndex(null);
        closeStepEditorPanel();
      } else if (pipelineState.editingStepIndex !== null && pipelineState.editingStepIndex > idx) {
        pipelineState.setEditingStepIndex(pipelineState.editingStepIndex - 1);
      }
      renderPipelineSteps();
      maybeAutoName();
    });
  });

  // Wire: add step chip
  const addChip = document.getElementById('add-step-tile');
  if (addChip) {
    addChip.addEventListener('click', (e) => {
      e.stopPropagation();
      addNewStep();
    });
  }

  // Initialize Sortable.js for drag-and-drop reordering
  if (pipelineState.sortableInstance) {
    pipelineState.sortableInstance.destroy();
    pipelineState.setSortableInstance(null);
  }
  if (typeof Sortable !== 'undefined' && pfFlowEl) {
    pipelineState.setSortableInstance(Sortable.create(pfFlowEl, {
      draggable: '.pflow-chip[data-index]',
      filter: '.pflow-chip--source, .pflow-chip--add',
      ghostClass: 'sortable-ghost',
      chosenClass: 'sortable-chosen',
      dragClass: 'sortable-drag',
      animation: 150,
      onEnd(evt) {
        const movedChip = evt.item;
        const movedIdx = parseInt(movedChip.dataset.index);
        const allStepChips = [...pfFlowEl.querySelectorAll('.pflow-chip[data-index]')];
        const newIdx = allStepChips.indexOf(movedChip);
        if (movedIdx === newIdx || newIdx < 0) { renderPipelineSteps(); return; }
        const [moved] = pipelineState.pipelineEditorSteps.splice(movedIdx, 1);
        pipelineState.pipelineEditorSteps.splice(newIdx, 0, moved);
        if (pipelineState.editingStepIndex === movedIdx) {
          pipelineState.setEditingStepIndex(newIdx);
        } else if (pipelineState.editingStepIndex !== null) {
          if (movedIdx < pipelineState.editingStepIndex && newIdx >= pipelineState.editingStepIndex) pipelineState.setEditingStepIndex(pipelineState.editingStepIndex - 1);
          else if (movedIdx > pipelineState.editingStepIndex && newIdx <= pipelineState.editingStepIndex) pipelineState.setEditingStepIndex(pipelineState.editingStepIndex + 1);
        }
        renderPipelineSteps();
        if (pipelineState.editingStepIndex !== null) showStepEditor(pipelineState.editingStepIndex);
      },
    }));
  }
}

// Wire close button
if (closePipelineEditorBtn) closePipelineEditorBtn.addEventListener('click', closePipelineEditor);

// Wire save button
if (savePipelineDefBtn) {
  savePipelineDefBtn.addEventListener('click', async () => {
    const name = pipelineEditorName.value.trim();
    const desc = pipelineEditorDesc.value.trim();
    if (!name) { showToast('Pipeline name is required', 'error'); return; }
    if (pipelineState.pipelineEditorSteps.length === 0) {
      showToast('Add at least one step to the pipeline', 'error');
      return;
    }

    for (let i = 0; i < pipelineState.pipelineEditorSteps.length; i++) {
      const s = pipelineState.pipelineEditorSteps[i];
      if (!s.name?.trim()) {
        showToast(`Step ${i + 1} needs a name`, 'error');
        return;
      }
      // Surface required-config gaps client-side rather than letting the Rust
      // connector bounce them back at run time — keeps the editor on the
      // broken step. CLI needs a CLI picked; Shell needs a working dir.
      const cfg = s.config || {};
      if (s.step_type === 'cli_agent' && !cfg.cli) {
        showToast(`Step ${i + 1} ("${s.name}") needs a CLI selected.`, 'error');
        return;
      }
      if (s.step_type === 'shell' && !(cfg.cwd || '').trim()) {
        showToast(`Step ${i + 1} ("${s.name}") needs a working directory.`, 'error');
        return;
      }
      if (s.step_type === 'save_local' && !(cfg.folder_path || '').trim()) {
        showToast(`Step ${i + 1} ("${s.name}") needs a folder.`, 'error');
        return;
      }
    }

    try {
      // auto_run is toggled from the pipeline list, not here — preserve the
      // existing value so editing a pipeline doesn't silently reset it (the
      // Rust field is serde-default false when omitted).
      const existing = pipelineState.editingPipelineDef
        ? pipelineState.allPipelineDefs.find(p => p.name === pipelineState.editingPipelineDef)
        : null;
      const pipeline = {
        name,
        description: desc,
        steps: pipelineState.pipelineEditorSteps,
        auto_run: existing?.auto_run || false,
      };
      // Rename is one backend read-modify-write transaction. The old
      // delete-then-save sequence could lose the pipeline if the second write
      // failed and left shortcut/default references dangling.
      await invoke('save_pipeline', {
        pipeline,
        previousName: pipelineState.editingPipelineDef || null,
      });
      appState.setAppSettings(await invoke('load_settings'));
      closePipelineEditor();
      await loadPipelineDefs();
    } catch (err) {
      console.error('Failed to save pipeline:', err);
      showToast('Failed to save: ' + err, 'error');
    }
  });
}

// Wire delete button
if (deletePipelineDefBtn) {
  deletePipelineDefBtn.addEventListener('click', async () => {
    if (!pipelineState.editingPipelineDef) return;
    const ok = await showConfirm('Delete Pipeline?', `Delete pipeline "${pipelineState.editingPipelineDef}"? This cannot be undone.`);
    if (!ok) return;
    try {
      await invoke('delete_pipeline', { name: pipelineState.editingPipelineDef });
      appState.setAppSettings(await invoke('load_settings'));
      closePipelineEditor();
      await loadPipelineDefs();
    } catch (err) {
      console.error('Failed to delete pipeline:', err);
      showToast('Failed to delete: ' + err, 'error');
    }
  });
}
