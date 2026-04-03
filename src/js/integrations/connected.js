// integrations/connected.js — Render all connected integration cards with inline editing

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { slackIntegrations } from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { showConfirm } from '../ui/confirm-modal.js';
import * as intState from './state.js';
import { INT_NOTION_SVG, INT_LINEAR_SVG, INT_SLACK_SVG, INT_FOLDER_SVG, INT_LINK_SVG } from './icons.js';
import { loadAllIntegrations } from './load.js';

const connectedListEl = () => document.getElementById('connected-integrations-list');

export function renderConnectedIntegrations() {
  const el = connectedListEl();
  if (!el) return;

  const cards = [];

  // Notion cards
  for (const profile of intState.notionProfiles) {
    const safeName = escapeHtml(profile.name);
    const safeDb = escapeHtml(profile.database_name || 'No database selected');
    const syncedAt = profile.synced_at ? new Date(profile.synced_at).toLocaleDateString() : 'Never synced';
    const daysSinceSync = profile.synced_at
      ? Math.floor((Date.now() - new Date(profile.synced_at).getTime()) / (1000 * 60 * 60 * 24))
      : null;
    const isStale = !profile.synced_at || daysSinceSync > 7;
    let cardDetail;
    if (!profile.synced_at) {
      cardDetail = `${safeDb} \u00b7 <span style="color: #e6a700; font-weight: 500;">Never synced \u2014 sync schema in wizard</span>`;
    } else if (isStale) {
      cardDetail = `${safeDb} \u00b7 Synced ${syncedAt} \u00b7 <span style="color: #e6a700; font-weight: 500;">Schema may be outdated \u2014 re-sync recommended</span>`;
    } else {
      cardDetail = `${safeDb} \u00b7 Synced ${syncedAt}`;
    }
    cards.push(`
      <div class="integration-card" data-type="notion" data-id="${escapeHtml(profile.id)}">
        ${profile.icon_url
          ? `<img class="integration-card-icon notion" src="${escapeHtml(profile.icon_url)}" alt="N" style="object-fit: cover;" />`
          : `<div class="integration-card-icon notion">${INT_NOTION_SVG}</div>`}
        <div class="integration-card-info">
          <div class="integration-card-name">${safeName}</div>
          <div class="integration-card-detail">${cardDetail}</div>
        </div>
        <div class="integration-card-actions">
          <button class="mini-action-btn test-notion-btn" data-id="${escapeHtml(profile.id)}">Test</button>
          <button class="mini-action-btn danger remove-notion-btn" data-id="${escapeHtml(profile.id)}">Remove</button>
        </div>
      </div>
    `);
  }

  // Linear cards
  for (const profile of intState.linearProfiles) {
    const safeName = escapeHtml(profile.name);
    const safeTeam = escapeHtml(profile.team_name || 'No team selected');
    const syncedAt = profile.synced_at ? new Date(profile.synced_at).toLocaleDateString() : 'Never synced';
    const daysSinceSync = profile.synced_at
      ? Math.floor((Date.now() - new Date(profile.synced_at).getTime()) / (1000 * 60 * 60 * 24))
      : null;
    const isStale = !profile.synced_at || daysSinceSync > 7;
    let cardDetail;
    if (!profile.synced_at) {
      cardDetail = `${safeTeam} \u00b7 <span style="color: #e6a700; font-weight: 500;">Never synced \u2014 sync schema in wizard</span>`;
    } else if (isStale) {
      cardDetail = `${safeTeam} \u00b7 Synced ${syncedAt} \u00b7 <span style="color: #e6a700; font-weight: 500;">Schema may be outdated \u2014 re-sync recommended</span>`;
    } else {
      cardDetail = `${safeTeam} \u00b7 Synced ${syncedAt}`;
    }
    cards.push(`
      <div class="integration-card" data-type="linear" data-id="${escapeHtml(profile.id)}">
        <div class="integration-card-icon linear">${INT_LINEAR_SVG}</div>
        <div class="integration-card-info">
          <div class="integration-card-name">${safeName}</div>
          <div class="integration-card-detail">${cardDetail}</div>
        </div>
        <div class="integration-card-actions">
          <button class="mini-action-btn test-linear-btn" data-id="${escapeHtml(profile.id)}">Test</button>
          <button class="mini-action-btn resync-linear-btn" data-id="${escapeHtml(profile.id)}">Re-sync</button>
          <button class="mini-action-btn danger remove-linear-btn" data-id="${escapeHtml(profile.id)}">Remove</button>
        </div>
      </div>
    `);
  }

  // Slack cards
  for (const [id, data] of Object.entries(slackIntegrations)) {
    cards.push(`
      <div class="integration-card" data-type="slack" data-id="${escapeHtml(id)}">
        ${data.icon_url
          ? `<img class="integration-card-icon slack" src="${escapeHtml(data.icon_url)}" alt="S" style="object-fit: cover;" />`
          : `<div class="integration-card-icon slack">${INT_SLACK_SVG}</div>`}
        <div class="integration-card-info">
          <div class="integration-card-name">${escapeHtml(data.name)}</div>
          <div class="integration-card-detail">${escapeHtml(data.workspace_name || 'Unknown workspace')}</div>
        </div>
        <div class="integration-card-actions">
          <button class="mini-action-btn test-slack-int-btn" data-id="${escapeHtml(id)}">Test</button>
          <button class="mini-action-btn danger remove-slack-int-btn" data-id="${escapeHtml(id)}">Remove</button>
        </div>
      </div>
    `);
  }

  // Save path cards
  for (const sp of intState.savePathIntegrations) {
    cards.push(`
      <div class="integration-card" data-type="save-path" data-id="${escapeHtml(sp.id)}">
        <div class="integration-card-icon save-path">${INT_FOLDER_SVG}</div>
        <div class="integration-card-info">
          <div class="integration-card-name">${escapeHtml(sp.name)}</div>
          <div class="integration-card-detail">${escapeHtml(sp.path)}</div>
        </div>
        <div class="integration-card-actions">
          <button class="mini-action-btn edit-save-path-btn" data-id="${escapeHtml(sp.id)}">Edit</button>
          <button class="mini-action-btn danger remove-save-path-btn" data-id="${escapeHtml(sp.id)}">Remove</button>
        </div>
      </div>
    `);
  }

  // Webhook cards
  for (const wh of intState.webhookProfiles) {
    cards.push(`
      <div class="integration-card" data-type="webhook" data-id="${escapeHtml(wh.id)}">
        <div class="integration-card-icon webhook">${INT_LINK_SVG}</div>
        <div class="integration-card-info">
          <div class="integration-card-name">${escapeHtml(wh.name)}</div>
          <div class="integration-card-detail">${escapeHtml(wh.method || 'POST')} ${escapeHtml(wh.url)}</div>
        </div>
        <div class="integration-card-actions">
          <button class="mini-action-btn test-webhook-btn" data-id="${escapeHtml(wh.id)}">Test</button>
          <button class="mini-action-btn danger remove-webhook-btn" data-id="${escapeHtml(wh.id)}">Remove</button>
        </div>
      </div>
    `);
  }

  if (cards.length === 0) {
    el.innerHTML = '<div style="text-align: center; padding: 40px 0; color: var(--text-secondary); opacity: 0.6;">No integrations connected yet</div>';
  } else {
    el.innerHTML = cards.join('');
  }

  wireNotionHandlers(el);
  wireLinearHandlers(el);
  wireSlackHandlers(el);
  wireSavePathHandlers(el);
  wireWebhookHandlers(el);
}

function wireNotionHandlers(el) {
  el.querySelectorAll('.test-notion-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      btn.disabled = true; btn.textContent = 'Testing...';
      try {
        const result = await invoke('test_notion_integration', { integrationId: btn.dataset.id });
        showToast('Notion: ' + result, 'success');
      } catch (err) { showToast('Notion test failed: ' + err, 'error'); }
      finally { btn.disabled = false; btn.textContent = 'Test'; }
    });
  });
  el.querySelectorAll('.remove-notion-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const profile = intState.notionProfiles.find(p => p.id === btn.dataset.id);
      const ok = await showConfirm('Remove Integration?', `Remove Notion integration "${profile ? profile.name : btn.dataset.id}"?`);
      if (!ok) return;
      try { await invoke('remove_notion_integration', { integrationId: btn.dataset.id }); await loadAllIntegrations(); }
      catch (err) { showToast('Failed to remove: ' + err, 'error'); }
    });
  });
}

function wireLinearHandlers(el) {
  el.querySelectorAll('.test-linear-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      btn.disabled = true; btn.textContent = 'Testing...';
      try {
        const result = await invoke('test_linear_integration', { integrationId: btn.dataset.id });
        showToast('Linear: ' + result, 'success');
      } catch (err) { showToast('Linear test failed: ' + err, 'error'); }
      finally { btn.disabled = false; btn.textContent = 'Test'; }
    });
  });
  el.querySelectorAll('.remove-linear-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const profile = intState.linearProfiles.find(p => p.id === btn.dataset.id);
      const ok = await showConfirm('Remove Integration?', `Remove Linear integration "${profile ? profile.name : btn.dataset.id}"?`);
      if (!ok) return;
      try { await invoke('remove_linear_integration', { integrationId: btn.dataset.id }); await loadAllIntegrations(); }
      catch (err) { showToast('Failed to remove: ' + err, 'error'); }
    });
  });
  el.querySelectorAll('.resync-linear-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const id = btn.dataset.id;
      const profile = intState.linearProfiles.find(p => p.id === id);
      if (!profile || !profile.team_id) { showToast('No team synced. Open the wizard to set up.', 'error'); return; }
      btn.disabled = true; btn.textContent = 'Syncing...';
      try {
        const updated = await invoke('sync_linear_schema', { integrationId: id, teamId: profile.team_id, teamName: profile.team_name });
        const idx = intState.linearProfiles.findIndex(p => p.id === id);
        if (idx >= 0) intState.linearProfiles[idx] = updated;
        renderConnectedIntegrations();
      } catch (err) { showToast('Re-sync failed: ' + err, 'error'); btn.disabled = false; btn.textContent = 'Re-sync'; }
    });
  });
}

function wireSlackHandlers(el) {
  el.querySelectorAll('.test-slack-int-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      btn.disabled = true; btn.textContent = 'Testing...';
      try {
        const name = await invoke('test_slack_integration', { id: btn.dataset.id });
        showToast('Connected to: ' + name, 'success');
      } catch (err) { showToast('Connection failed: ' + err, 'error'); }
      finally { btn.disabled = false; btn.textContent = 'Test'; }
    });
  });
  el.querySelectorAll('.remove-slack-int-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const data = slackIntegrations[btn.dataset.id];
      const ok = await showConfirm('Remove Integration?', `Remove Slack workspace "${data ? data.name : btn.dataset.id}"?`);
      if (!ok) return;
      try { await invoke('remove_slack_integration', { id: btn.dataset.id }); await loadAllIntegrations(); }
      catch (err) { showToast('Failed to remove: ' + err, 'error'); }
    });
  });
}

function wireSavePathHandlers(el) {
  el.querySelectorAll('.edit-save-path-btn').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const sp = intState.savePathIntegrations.find(p => p.id === btn.dataset.id);
      if (!sp) return;
      const card = btn.closest('.integration-card');
      if (!card) return;
      const safeId = escapeHtml(sp.id);
      card.outerHTML = `
        <div class="integration-card save-path-editor" data-id="${safeId}">
          <div class="integration-card-icon save-path">${INT_FOLDER_SVG}</div>
          <div class="integration-card-info" style="flex: 1; gap: 6px; display: flex; flex-direction: column;">
            <input id="edit-sp-name-${safeId}" type="text" value="${escapeHtml(sp.name)}" placeholder="Folder name" style="width: 100%;" />
            <div style="display: flex; align-items: center; gap: 8px;">
              <span id="edit-sp-path-display-${safeId}" style="flex: 1; font-size: 0.8rem; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">${escapeHtml(sp.path)}</span>
              <button id="edit-sp-browse-${safeId}" class="mini-action-btn">Browse</button>
            </div>
          </div>
          <div class="integration-card-actions">
            <button class="mini-action-btn save-sp-edit-btn" data-id="${safeId}">Save</button>
            <button class="mini-action-btn cancel-sp-edit-btn" data-id="${safeId}">Cancel</button>
          </div>
        </div>
      `;
      let selectedPath = sp.path;
      const pathDisplay = document.getElementById(`edit-sp-path-display-${sp.id}`);
      const browseBtn = document.getElementById(`edit-sp-browse-${sp.id}`);
      if (browseBtn) {
        browseBtn.addEventListener('click', async () => {
          try {
            const result = await window.__TAURI__.dialog.open({ directory: true, multiple: false });
            if (result) { selectedPath = result; if (pathDisplay) pathDisplay.textContent = result; }
          } catch (err) { console.error('Folder picker error:', err); }
        });
      }
      const saveBtn = el.querySelector(`.save-sp-edit-btn[data-id="${sp.id}"]`);
      if (saveBtn) {
        saveBtn.addEventListener('click', async () => {
          const nameInput = document.getElementById(`edit-sp-name-${sp.id}`);
          const name = nameInput ? nameInput.value.trim() : '';
          if (!name) { showToast('Name cannot be empty', 'error'); return; }
          if (!selectedPath) { showToast('Please select a folder', 'error'); return; }
          saveBtn.disabled = true;
          try { await invoke('update_save_path_integration', { id: sp.id, name, path: selectedPath }); await loadAllIntegrations(); }
          catch (err) { showToast('Failed to update: ' + err, 'error'); saveBtn.disabled = false; }
        });
      }
      const cancelBtn = el.querySelector(`.cancel-sp-edit-btn[data-id="${sp.id}"]`);
      if (cancelBtn) cancelBtn.addEventListener('click', () => renderConnectedIntegrations());
    });
  });
  el.querySelectorAll('.remove-save-path-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const sp = intState.savePathIntegrations.find(p => p.id === btn.dataset.id);
      const ok = await showConfirm('Remove Save Path?', `Remove save path "${sp ? sp.name : btn.dataset.id}"?`);
      if (!ok) return;
      try { await invoke('remove_save_path_integration', { id: btn.dataset.id }); await loadAllIntegrations(); }
      catch (err) { showToast('Failed to remove: ' + err, 'error'); }
    });
  });
}

function wireWebhookHandlers(el) {
  el.querySelectorAll('.test-webhook-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      btn.disabled = true; btn.textContent = 'Testing...';
      try {
        const result = await invoke('test_webhook_integration', { id: btn.dataset.id });
        showToast('Webhook: ' + result, 'success');
      } catch (err) { showToast('Webhook test failed: ' + err, 'error'); }
      finally { btn.disabled = false; btn.textContent = 'Test'; }
    });
  });
  el.querySelectorAll('.remove-webhook-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const wh = intState.webhookProfiles.find(p => p.id === btn.dataset.id);
      const ok = await showConfirm('Remove Webhook?', `Remove webhook "${wh ? wh.name : btn.dataset.id}"?`);
      if (!ok) return;
      try { await invoke('remove_webhook_integration', { id: btn.dataset.id }); await loadAllIntegrations(); }
      catch (err) { showToast('Failed to remove: ' + err, 'error'); }
    });
  });
}
