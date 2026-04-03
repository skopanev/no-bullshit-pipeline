// integrations/notion-wizard.js — Full Notion setup wizard (all steps)

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { loadAllIntegrations } from './load.js';

let wizardState = {
  step: 0, integrationId: null, databases: [], selectedDbId: null,
  selectedDbName: null, profile: null, mappings: [], error: null,
};

function reset() {
  wizardState = {
    step: 0, integrationId: null, databases: [], selectedDbId: null,
    selectedDbName: null, profile: null, mappings: [], error: null,
  };
}

function close() {
  reset();
  const modal = document.getElementById('notion-wizard-modal');
  if (modal) modal.style.display = 'none';
}

export function openNotionWizard() {
  reset();
  const modal = document.getElementById('notion-wizard-modal');
  if (!modal) return;
  modal.style.display = 'flex';

  const cancelBtn = document.getElementById('notion-wizard-cancel');
  if (cancelBtn) {
    cancelBtn.replaceWith(cancelBtn.cloneNode(true));
    const freshCancel = document.getElementById('notion-wizard-cancel');
    freshCancel.addEventListener('click', async () => {
      if (wizardState.integrationId) {
        try { await invoke('remove_notion_integration', { integrationId: wizardState.integrationId }); }
        catch (err) { console.error('Failed to clean up partial integration on cancel:', err); }
      }
      close();
    });
  }
  renderStep();
}

const STEP_PROGRESS = ['20%', '40%', '60%', '80%', '100%'];

function replaceNextBtn() {
  const btn = document.getElementById('notion-wizard-next');
  if (!btn) return btn;
  const clone = btn.cloneNode(true);
  btn.parentNode.replaceChild(clone, btn);
  return clone;
}

async function renderStep() {
  const body = document.getElementById('notion-wizard-body');
  const progressBar = document.getElementById('notion-wizard-progress');
  const nextBtn = document.getElementById('notion-wizard-next');
  if (!body || !nextBtn) return;
  if (progressBar) progressBar.style.width = STEP_PROGRESS[wizardState.step] || '20%';
  nextBtn.textContent = wizardState.step === 4 ? 'Finish' : 'Next';
  nextBtn.disabled = false;

  switch (wizardState.step) {
    case 0: renderStep0(body); break;
    case 1: renderStep1(body); break;
    case 2: await renderStep2(body); break;
    case 3: renderStep3(body); break;
    case 4: renderStep4(body); break;
  }
}

function renderStep0(body) {
  body.innerHTML = `
    <div class="wizard-step-title">Enter Notion API Key</div>
    <p class="wizard-step-description">Create an internal integration at notion.so/my-integrations, then paste the API key below.</p>
    <div class="wizard-input-group">
      <div>
        <label for="wizard-notion-apikey">API Key</label>
        <input id="wizard-notion-apikey" type="password" placeholder="ntn_..." autocomplete="off" spellcheck="false"
          style="font-family: 'SF Mono', monospace; font-size: 0.85rem;" />
      </div>
    </div>
    ${wizardState.error ? `<div class="wizard-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  const freshNext = replaceNextBtn();
  freshNext.addEventListener('click', async () => {
    const apiKey = (document.getElementById('wizard-notion-apikey').value || '').trim();
    if (!apiKey) { wizardState.error = 'Please enter an API key.'; renderStep(); return; }
    freshNext.disabled = true; freshNext.textContent = '...';
    try {
      const result = await invoke('add_notion_integration', { apiKey });
      wizardState.integrationId = result.id || result;
      wizardState.error = null; wizardState.step = 1; renderStep();
    } catch (err) {
      wizardState.error = String(err); freshNext.disabled = false; freshNext.textContent = 'Next'; renderStep();
    }
  });
}

function renderStep1(body) {
  body.innerHTML = `
    <div class="wizard-step-title">Share Your Database</div>
    <div class="wizard-info-box">
      <strong>Before selecting a database, you must share it with your integration:</strong>
      <ol>
        <li>Open your Notion database in the browser</li>
        <li>Click the "..." menu in the top-right corner</li>
        <li>Go to "Connections" (or "Add connections")</li>
        <li>Find and add your integration by name</li>
      </ol>
    </div>
    <p class="wizard-step-description" style="margin-top: 12px;">After sharing, click Next to continue.</p>
  `;
  const freshNext = replaceNextBtn();
  freshNext.textContent = 'Next';
  freshNext.addEventListener('click', () => { wizardState.step = 2; renderStep(); });
}

async function renderStep2(body) {
  body.innerHTML = `<div style="color: var(--text-secondary); font-size: 0.85rem;">Loading databases...</div>`;
  document.getElementById('notion-wizard-next').disabled = true;
  try {
    wizardState.databases = await invoke('list_notion_databases', { integrationId: wizardState.integrationId });
    wizardState.error = null;
    renderStep2Databases(body);
  } catch (err) { renderStep2Error(body, String(err)); }
}

function renderStep2Databases(body) {
  const { databases, selectedDbId } = wizardState;
  if (!databases || databases.length === 0) { renderStep2Error(body, 'No databases found. Make sure you shared your database with the integration (see previous step).'); return; }
  const items = databases.map(db => {
    const sel = db.id === selectedDbId;
    return `<div class="wizard-db-item${sel ? ' selected' : ''}" data-db-id="${escapeHtml(db.id)}" data-db-name="${escapeHtml(db.name)}">${escapeHtml(db.name)}</div>`;
  }).join('');
  body.innerHTML = `
    <div class="wizard-step-title">Select Database</div>
    <div class="wizard-db-list">${items}</div>
    ${wizardState.error ? `<div class="wizard-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  const freshNext = replaceNextBtn();
  freshNext.disabled = !selectedDbId; freshNext.textContent = 'Next';
  body.querySelectorAll('.wizard-db-item').forEach(item => {
    item.addEventListener('click', () => {
      body.querySelectorAll('.wizard-db-item').forEach(i => i.classList.remove('selected'));
      item.classList.add('selected');
      wizardState.selectedDbId = item.dataset.dbId;
      wizardState.selectedDbName = item.dataset.dbName;
      freshNext.disabled = false;
    });
  });
  freshNext.addEventListener('click', async () => {
    if (!wizardState.selectedDbId) return;
    freshNext.disabled = true; freshNext.textContent = '...';
    try {
      wizardState.profile = await invoke('sync_notion_schema', { integrationId: wizardState.integrationId, databaseId: wizardState.selectedDbId, databaseName: wizardState.selectedDbName });
      wizardState.error = null; wizardState.step = 3; renderStep();
    } catch (err) {
      wizardState.error = String(err); freshNext.disabled = false; freshNext.textContent = 'Next';
      renderStep2Databases(body);
    }
  });
}

function renderStep2Error(body, errorMsg) {
  body.innerHTML = `
    <div class="wizard-step-title">Select Database</div>
    <div class="wizard-info-box">
      <strong>No databases found. Please share your database first:</strong>
      <ol><li>Open your Notion database in the browser</li><li>Click the "..." menu in the top-right corner</li><li>Go to "Connections" (or "Add connections")</li><li>Find and add your integration by name</li></ol>
    </div>
    <div class="wizard-error" style="margin-top: 8px;">${escapeHtml(errorMsg)}</div>
    <button id="wizard-retry-btn" class="mini-action-btn" style="margin-top: 12px;">Retry</button>
  `;
  const retryBtn = document.getElementById('wizard-retry-btn');
  if (retryBtn) retryBtn.addEventListener('click', async () => { await renderStep2(body); });
  document.getElementById('notion-wizard-next').disabled = true;
}

function renderStep3(body) {
  const profile = wizardState.profile;
  if (!profile) { body.innerHTML = '<div class="wizard-error">No schema loaded.</div>'; return; }
  const properties = profile.properties || [];
  const syncedAt = profile.synced_at ? new Date(profile.synced_at).toLocaleString() : 'Unknown';
  const rows = properties.map(prop => {
    const options = (prop.type === 'select' || prop.type === 'multi_select')
      ? escapeHtml((prop.select_options || []).join(', ') || '\u2014') : '\u2014';
    return `<tr><td>${escapeHtml(prop.name)}</td><td>${escapeHtml(prop.type)}</td><td>${options}</td></tr>`;
  }).join('');
  body.innerHTML = `
    <div class="wizard-step-title">Database Schema</div>
    <div style="max-height: 260px; overflow-y: auto;">
      <table class="wizard-schema-table"><thead><tr><th>Property Name</th><th>Type</th><th>Options</th></tr></thead><tbody>${rows}</tbody></table>
    </div>
    <div class="wizard-schema-synced">Last synced: ${escapeHtml(syncedAt)}</div>
    <button id="wizard-resync-btn" class="mini-action-btn" style="margin-top: 10px;">Re-sync Schema</button>
    ${wizardState.error ? `<div class="wizard-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  const resyncBtn = document.getElementById('wizard-resync-btn');
  if (resyncBtn) {
    resyncBtn.addEventListener('click', async () => {
      resyncBtn.disabled = true; resyncBtn.textContent = '...';
      try {
        wizardState.profile = await invoke('sync_notion_schema', { integrationId: wizardState.integrationId, databaseId: wizardState.selectedDbId, databaseName: wizardState.selectedDbName });
        wizardState.error = null; renderStep();
      } catch (err) { wizardState.error = String(err); renderStep(); }
    });
  }
  const freshNext = replaceNextBtn();
  freshNext.textContent = 'Next';
  freshNext.addEventListener('click', () => {
    const peopleProps = (profile.properties || []).filter(p => p.type === 'people');
    if (peopleProps.length > 0) {
      wizardState.mappings = peopleProps.map(p => ({ alias: p.name, notionUserId: '', displayName: '' }));
    } else if (wizardState.mappings.length === 0) {
      wizardState.mappings = [{ alias: '', notionUserId: '', displayName: '' }];
    }
    wizardState.step = 4; renderStep();
  });
}

function renderStep4(body) {
  const profile = wizardState.profile;
  const workspaceUsers = (profile && profile.workspace_users) ? profile.workspace_users : [];

  function renderMappingRows() {
    const rowsEl = document.getElementById('wizard-mapping-rows');
    if (!rowsEl) return;
    const userOptions = workspaceUsers.map(u =>
      `<option value="${escapeHtml(u.id)}" data-name="${escapeHtml(u.name)}">${escapeHtml(u.name)}</option>`
    ).join('');
    rowsEl.innerHTML = wizardState.mappings.map((m, idx) => `
      <div class="wizard-mapping-row" data-mapping-idx="${idx}">
        <input type="text" class="wizard-mapping-alias" placeholder="Alias (e.g. me)" value="${escapeHtml(m.alias)}" />
        <select class="wizard-mapping-user"><option value="">Select user...</option>${userOptions}</select>
        <button class="wizard-mapping-remove" title="Remove">x</button>
      </div>
    `).join('');
    rowsEl.querySelectorAll('.wizard-mapping-row').forEach((row, idx) => {
      const select = row.querySelector('.wizard-mapping-user');
      if (select && wizardState.mappings[idx].notionUserId) select.value = wizardState.mappings[idx].notionUserId;
      row.querySelector('.wizard-mapping-alias').addEventListener('input', (e) => { wizardState.mappings[idx].alias = e.target.value; });
      select.addEventListener('change', () => {
        const opt = select.options[select.selectedIndex];
        wizardState.mappings[idx].notionUserId = select.value;
        wizardState.mappings[idx].displayName = opt ? (opt.dataset.name || '') : '';
      });
      row.querySelector('.wizard-mapping-remove').addEventListener('click', () => { wizardState.mappings.splice(idx, 1); renderMappingRows(); });
    });
  }

  body.innerHTML = `
    <div class="wizard-step-title">People Mapping</div>
    <p class="wizard-step-description">Map aliases (like 'me' or 'team') to Notion workspace users. These aliases can be used in AI output to assign people.</p>
    <div id="wizard-mapping-rows"></div>
    <button id="wizard-add-mapping-btn" class="mini-action-btn" style="margin-top: 4px;">+ Add mapping</button>
    ${wizardState.error ? `<div class="wizard-error" id="wizard-mapping-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  renderMappingRows();
  const addBtn = document.getElementById('wizard-add-mapping-btn');
  if (addBtn) addBtn.addEventListener('click', () => { wizardState.mappings.push({ alias: '', notionUserId: '', displayName: '' }); renderMappingRows(); });

  const freshNext = replaceNextBtn();
  freshNext.textContent = 'Finish';
  freshNext.addEventListener('click', async () => {
    const clean = wizardState.mappings.filter(m => m.alias.trim() && m.notionUserId);
    freshNext.disabled = true; freshNext.textContent = '...';
    try {
      if (clean.length > 0) {
        const payload = clean.map(m => ({ alias: m.alias.trim(), notion_user_id: m.notionUserId, display_name: m.displayName }));
        await invoke('update_notion_people_mappings', { integrationId: wizardState.integrationId, mappings: payload });
      }
      close(); await loadAllIntegrations();
    } catch (err) {
      wizardState.error = String(err); freshNext.disabled = false; freshNext.textContent = 'Finish';
      const errEl = document.getElementById('wizard-mapping-error');
      if (errEl) { errEl.textContent = wizardState.error; }
      else { const d = document.createElement('div'); d.id = 'wizard-mapping-error'; d.className = 'wizard-error'; d.textContent = wizardState.error; body.appendChild(d); }
    }
  });
}
