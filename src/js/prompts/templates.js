import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { allPipelineDefs } from '../pipeline/state.js';

let editingPromptTemplate = null;

const promptTemplatesListEl = document.getElementById('prompt-templates-list');
const addPromptTemplateBtn = document.getElementById('add-prompt-template-btn');
const promptTemplateEditor = document.getElementById('prompt-template-editor');
const promptEditorTitle = document.getElementById('prompt-editor-title');
const promptEditorName = document.getElementById('prompt-editor-name');
const promptEditorText = document.getElementById('prompt-editor-text');
const savePromptTemplateBtn = document.getElementById('save-prompt-template-btn');
const deletePromptTemplateBtn = document.getElementById('delete-prompt-template-btn');
const closePromptEditorBtn = document.getElementById('close-prompt-editor');

export async function loadPromptTemplates() {
  try {
    state.setAllPromptTemplates(await invoke('list_prompt_templates'));
    renderPromptTemplatesList();
  } catch (err) {
    console.error('Failed to load prompt templates:', err);
    if (promptTemplatesListEl) {
      promptTemplatesListEl.innerHTML = `<div style="color: var(--danger); opacity: 0.9; font-size: 0.85rem; text-align: center; padding: 1rem;">Failed to load templates: ${escapeHtml(String(err))}</div>`;
    }
  }
}

function renderPromptTemplatesList() {
  if (!promptTemplatesListEl) return;
  if (state.allPromptTemplates.length === 0) {
    promptTemplatesListEl.innerHTML = '<div style="color: var(--text-secondary); opacity: 0.6; font-size: 0.85rem; text-align: center; padding: 2rem;">No prompt templates yet.\n\nClick "+ New Prompt" to create one.</div>';
    return;
  }
  promptTemplatesListEl.innerHTML = state.allPromptTemplates.map(t => {
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

  promptTemplatesListEl.querySelectorAll('.template-item').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.template-item-delete')) return;
      openPromptEditor(el.dataset.name);
    });
  });

  promptTemplatesListEl.querySelectorAll('.template-item-delete').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      deletePromptTemplateWithConfirm(btn.dataset.name);
    });
  });
}

function findPipelinesReferencingPrompt(promptName) {
  const pipelines = allPipelineDefs || [];
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

export async function openPromptEditor(name) {
  if (!promptTemplateEditor) return;
  if (name) {
    const t = state.allPromptTemplates.find(t => t.name === name);
    if (!t) return;
    editingPromptTemplate = name;
    if (promptEditorTitle) promptEditorTitle.textContent = 'Edit Prompt';
    if (promptEditorName) promptEditorName.value = t.name;
    if (promptEditorText) promptEditorText.value = t.prompt || '';
    if (deletePromptTemplateBtn) deletePromptTemplateBtn.style.display = 'inline-block';

    const usageSection = document.getElementById('prompt-usage-section');
    const usageList = document.getElementById('prompt-usage-list');
    if (usageSection && usageList) {
      const referencing = findPipelinesReferencingPrompt(name);
      if (referencing.length > 0) {
        usageSection.style.display = 'block';
        usageList.innerHTML = referencing.map(p => `
          <div class="prompt-usage-item">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>
            </svg>
            ${escapeHtml(p.name)}
          </div>
        `).join('');
      } else {
        usageSection.style.display = 'none';
      }
    }
  } else {
    editingPromptTemplate = null;
    if (promptEditorTitle) promptEditorTitle.textContent = 'New Prompt';
    if (promptEditorName) promptEditorName.value = '';
    if (promptEditorText) promptEditorText.value = '';
    if (deletePromptTemplateBtn) deletePromptTemplateBtn.style.display = 'none';
    const usageSection = document.getElementById('prompt-usage-section');
    if (usageSection) usageSection.style.display = 'none';
  }
  promptTemplateEditor.style.display = 'block';
  if (promptEditorName) promptEditorName.focus();
}

function closePromptEditor() {
  if (promptTemplateEditor) promptTemplateEditor.style.display = 'none';
  editingPromptTemplate = null;
}

async function savePromptTemplate() {
  const name = promptEditorName?.value.trim() || '';
  const prompt = promptEditorText?.value.trim() || '';
  if (!name) { showToast('Name is required', 'error'); return; }
  if (!prompt) { showToast('Prompt text is required', 'error'); return; }

  try {
    const template = {
      name, prompt,
      created_at: editingPromptTemplate ? (state.allPromptTemplates.find(t => t.name === editingPromptTemplate)?.created_at || new Date().toISOString()) : new Date().toISOString(),
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
    ok = await showConfirm('Prompt is in use', `Prompt "${editingPromptTemplate}" is used by ${referencing.length} pipeline(s): ${pipelineNames}. Delete anyway?`);
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
    showToast('Failed to delete: ' + err, 'error');
  }
}

async function deletePromptTemplateWithConfirm(name) {
  if (!name) return;
  const referencing = findPipelinesReferencingPrompt(name);
  let ok;
  if (referencing.length > 0) {
    const pipelineNames = referencing.map(p => p.name).join(', ');
    ok = await showConfirm('Prompt is in use', `Prompt "${name}" is used by ${referencing.length} pipeline(s): ${pipelineNames}. Delete anyway?`);
  } else {
    ok = await showConfirm('Delete Prompt?', `Delete prompt "${name}"? This cannot be undone.`);
  }
  if (!ok) return;
  try {
    await invoke('delete_prompt_template', { name, force: referencing.length > 0 });
    await loadPromptTemplates();
    showToast('Prompt deleted', 'info');
  } catch (err) {
    showToast('Failed to delete: ' + err, 'error');
  }
}

export function initPromptTemplates() {
  if (addPromptTemplateBtn) addPromptTemplateBtn.addEventListener('click', () => openPromptEditor(null));
  if (closePromptEditorBtn) closePromptEditorBtn.addEventListener('click', closePromptEditor);
  if (savePromptTemplateBtn) savePromptTemplateBtn.addEventListener('click', savePromptTemplate);
  if (deletePromptTemplateBtn) deletePromptTemplateBtn.addEventListener('click', deletePromptTemplate);
}
