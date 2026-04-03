// integrations/available.js — Render available integrations to add

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { showToast } from '../ui/toast.js';
import { INT_NOTION_SVG, INT_LINEAR_SVG, INT_SLACK_SVG, INT_FOLDER_SVG, INT_LINK_SVG } from './icons.js';
import { loadAllIntegrations } from './load.js';
import { openNotionWizard } from './notion-wizard.js';
import { openLinearWizard } from './linear-wizard.js';

const availableListEl = () => document.getElementById('available-integrations-list');

export function renderAvailableIntegrations() {
  const el = availableListEl();
  if (!el) return;

  const available = [
    buildCard('notion', 'add-notion-integration-btn', INT_NOTION_SVG, 'Notion', 'Connect a Notion database for automatic page creation'),
    buildCard('linear', 'add-linear-integration-btn', INT_LINEAR_SVG, 'Linear', 'Connect Linear to create issues from pipeline output'),
    buildCard('slack', 'add-slack-integration-btn', INT_SLACK_SVG, 'Slack', 'Send pipeline output to Slack channels or DMs'),
    buildCard('save-path', 'add-save-path-btn', INT_FOLDER_SVG, 'Save Path', 'Save pipeline output to a named folder location'),
    buildCard('webhook', 'add-webhook-btn', INT_LINK_SVG, 'Webhook', 'Send pipeline output to any HTTP endpoint'),
  ];

  el.innerHTML = available.join('');

  wireNotionAdd();
  wireLinearAdd();
  wireSlackAdd();
  wireSavePathAdd(el);
  wireWebhookAdd(el);
}

function buildCard(type, id, icon, name, detail) {
  return `
    <div class="available-integration-card" data-type="${type}" id="${id}">
      <div class="integration-card-icon ${type}">${icon}</div>
      <div class="integration-card-info">
        <div class="integration-card-name">${name}</div>
        <div class="integration-card-detail">${detail}</div>
      </div>
      <span class="available-add-label">+ Add</span>
    </div>
  `;
}

function wireNotionAdd() {
  const btn = document.getElementById('add-notion-integration-btn');
  if (btn) btn.addEventListener('click', () => openNotionWizard());
}

function wireLinearAdd() {
  const btn = document.getElementById('add-linear-integration-btn');
  if (btn) btn.addEventListener('click', () => openLinearWizard());
}

function wireSlackAdd() {
  const btn = document.getElementById('add-slack-integration-btn');
  if (btn) {
    btn.addEventListener('click', () => {
      const modal = document.getElementById('add-slack-modal');
      const tokenInput = document.getElementById('slack-token-input');
      if (modal) {
        if (tokenInput) tokenInput.value = '';
        modal.style.display = 'flex';
      }
    });
  }
}

function wireSavePathAdd(el) {
  const addBtn = document.getElementById('add-save-path-btn');
  if (!addBtn) return;
  addBtn.addEventListener('click', () => {
    let selectedPath = '';
    addBtn.outerHTML = `
      <div class="available-integration-card save-path-add-form" id="add-save-path-form">
        <div class="integration-card-icon save-path">${INT_FOLDER_SVG}</div>
        <div class="integration-card-info" style="flex: 1; gap: 6px; display: flex; flex-direction: column;">
          <input id="new-sp-name" type="text" placeholder="Folder name (e.g. Notes)" style="width: 100%;" />
          <div style="display: flex; align-items: center; gap: 8px;">
            <span id="new-sp-path-display" style="flex: 1; font-size: 0.8rem; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">No folder selected</span>
            <button id="new-sp-browse-btn" class="mini-action-btn">Browse</button>
          </div>
        </div>
        <div class="integration-card-actions">
          <button id="new-sp-save-btn" class="mini-action-btn">Save</button>
          <button id="new-sp-cancel-btn" class="mini-action-btn">Cancel</button>
        </div>
      </div>
    `;

    const browseBtn = document.getElementById('new-sp-browse-btn');
    if (browseBtn) {
      browseBtn.addEventListener('click', async () => {
        try {
          const result = await window.__TAURI__.dialog.open({ directory: true, multiple: false });
          if (result) {
            selectedPath = result;
            const display = document.getElementById('new-sp-path-display');
            if (display) display.textContent = result;
          }
        } catch (err) { console.error('Folder picker error:', err); }
      });
    }

    const saveBtn = document.getElementById('new-sp-save-btn');
    if (saveBtn) {
      saveBtn.addEventListener('click', async () => {
        const nameInput = document.getElementById('new-sp-name');
        const name = nameInput ? nameInput.value.trim() : '';
        if (!name) { showToast('Please enter a name', 'error'); return; }
        if (!selectedPath) { showToast('Please select a folder', 'error'); return; }
        saveBtn.disabled = true;
        try { await invoke('add_save_path_integration', { name, path: selectedPath }); await loadAllIntegrations(); }
        catch (err) { showToast('Failed to add save path: ' + err, 'error'); saveBtn.disabled = false; }
      });
    }

    const cancelBtn = document.getElementById('new-sp-cancel-btn');
    if (cancelBtn) cancelBtn.addEventListener('click', () => renderAvailableIntegrations());
  });
}

function wireWebhookAdd(el) {
  const addBtn = document.getElementById('add-webhook-btn');
  if (!addBtn) return;
  addBtn.addEventListener('click', () => {
    addBtn.outerHTML = `
      <div class="available-integration-card webhook-add-form" id="add-webhook-form">
        <div class="integration-card-icon webhook">${INT_LINK_SVG}</div>
        <div class="integration-card-info" style="flex: 1; gap: 6px; display: flex; flex-direction: column;">
          <input id="new-wh-name" type="text" placeholder="Endpoint name (e.g. n8n Meetings)" style="width: 100%;" />
          <input id="new-wh-url" type="text" placeholder="https://hooks.example.com/..." style="width: 100%;" />
          <select id="new-wh-method" style="width: 100px;">
            <option value="POST">POST</option>
            <option value="PUT">PUT</option>
            <option value="PATCH">PATCH</option>
          </select>
        </div>
        <div class="integration-card-actions">
          <button id="new-wh-save-btn" class="mini-action-btn">Save</button>
          <button id="new-wh-cancel-btn" class="mini-action-btn">Cancel</button>
        </div>
      </div>
    `;

    const saveBtn = document.getElementById('new-wh-save-btn');
    if (saveBtn) {
      saveBtn.addEventListener('click', async () => {
        const name = (document.getElementById('new-wh-name')?.value || '').trim();
        const url = (document.getElementById('new-wh-url')?.value || '').trim();
        const method = document.getElementById('new-wh-method')?.value || 'POST';
        if (!name) { showToast('Please enter a name', 'error'); return; }
        if (!url) { showToast('Please enter the webhook URL', 'error'); return; }
        if (!url.startsWith('http://') && !url.startsWith('https://')) {
          showToast('URL must start with http:// or https://', 'error'); return;
        }
        saveBtn.disabled = true;
        try { await invoke('add_webhook_integration', { name, url, method }); await loadAllIntegrations(); }
        catch (err) { showToast('Failed to add webhook: ' + err, 'error'); saveBtn.disabled = false; }
      });
    }

    const cancelBtn = document.getElementById('new-wh-cancel-btn');
    if (cancelBtn) cancelBtn.addEventListener('click', () => renderAvailableIntegrations());
  });
}
