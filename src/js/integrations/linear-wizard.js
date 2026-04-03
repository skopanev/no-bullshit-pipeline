// integrations/linear-wizard.js — Full Linear setup wizard (all steps)

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { loadAllIntegrations } from './load.js';

let wizardState = {
  step: 0, integrationId: null, teams: [], selectedTeamId: null,
  selectedTeamName: null, profile: null, aliases: [], error: null,
};

function reset() {
  wizardState = {
    step: 0, integrationId: null, teams: [], selectedTeamId: null,
    selectedTeamName: null, profile: null, aliases: [], error: null,
  };
}

function close() {
  reset();
  const modal = document.getElementById('linear-wizard-modal');
  if (modal) modal.style.display = 'none';
}

export function openLinearWizard() {
  reset();
  const modal = document.getElementById('linear-wizard-modal');
  if (!modal) return;
  modal.style.display = 'flex';

  const cancelBtn = document.getElementById('linear-wizard-cancel');
  if (cancelBtn) {
    cancelBtn.replaceWith(cancelBtn.cloneNode(true));
    const freshCancel = document.getElementById('linear-wizard-cancel');
    freshCancel.addEventListener('click', async () => {
      if (wizardState.integrationId) {
        try { await invoke('remove_linear_integration', { integrationId: wizardState.integrationId }); }
        catch (err) { console.error('Failed to clean up partial Linear integration on cancel:', err); }
      }
      close();
    });
  }
  renderStep();
}

const STEP_PROGRESS = ['25%', '50%', '75%', '100%'];

function replaceNextBtn() {
  const btn = document.getElementById('linear-wizard-next');
  if (!btn) return btn;
  const clone = btn.cloneNode(true);
  btn.parentNode.replaceChild(clone, btn);
  return clone;
}

async function renderStep() {
  const body = document.getElementById('linear-wizard-body');
  const progressBar = document.getElementById('linear-wizard-progress');
  const nextBtn = document.getElementById('linear-wizard-next');
  if (!body || !nextBtn) return;
  if (progressBar) progressBar.style.width = STEP_PROGRESS[wizardState.step] || '25%';
  nextBtn.textContent = wizardState.step === 3 ? 'Finish' : 'Next';
  nextBtn.disabled = false;

  switch (wizardState.step) {
    case 0: renderStep0(body); break;
    case 1: await renderStep1(body); break;
    case 2: renderStep2(body); break;
    case 3: renderStep3(body); break;
  }
}

function renderStep0(body) {
  body.innerHTML = `
    <div class="wizard-step-title">Enter Linear API Key</div>
    <p class="wizard-step-description">Create a personal API key at linear.app/settings/api, then paste it below.</p>
    <div class="wizard-input-group">
      <div>
        <label for="wizard-linear-name">Integration Name</label>
        <input id="wizard-linear-name" type="text" placeholder="Linear" value="Linear" autocomplete="off" />
      </div>
      <div>
        <label for="wizard-linear-apikey">API Key</label>
        <input id="wizard-linear-apikey" type="password" placeholder="lin_api_..." autocomplete="off" spellcheck="false"
          style="font-family: 'SF Mono', monospace; font-size: 0.85rem;" />
      </div>
    </div>
    ${wizardState.error ? `<div class="wizard-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  const freshNext = replaceNextBtn();
  freshNext.addEventListener('click', async () => {
    const name = (document.getElementById('wizard-linear-name').value || 'Linear').trim();
    const apiKey = (document.getElementById('wizard-linear-apikey').value || '').trim();
    if (!apiKey) { wizardState.error = 'Please enter an API key.'; renderStep(); return; }
    freshNext.disabled = true; freshNext.textContent = '...';
    try {
      wizardState.integrationId = await invoke('add_linear_integration', { name, apiKey });
      wizardState.error = null; wizardState.step = 1; renderStep();
    } catch (err) {
      wizardState.error = String(err); freshNext.disabled = false; freshNext.textContent = 'Next'; renderStep();
    }
  });
}

async function renderStep1(body) {
  body.innerHTML = `<div style="color: var(--text-secondary); font-size: 0.85rem;">Loading teams...</div>`;
  document.getElementById('linear-wizard-next').disabled = true;
  try {
    wizardState.teams = await invoke('list_linear_teams', { integrationId: wizardState.integrationId });
    wizardState.error = null;
    renderStep1Teams(body);
  } catch (err) {
    wizardState.error = String(err);
    body.innerHTML = `
      <div class="wizard-step-title">Select Team</div>
      <div class="wizard-error">${escapeHtml(String(err))}</div>
      <button id="linear-retry-teams-btn" class="mini-action-btn" style="margin-top: 12px;">Retry</button>
    `;
    const retryBtn = document.getElementById('linear-retry-teams-btn');
    if (retryBtn) retryBtn.addEventListener('click', async () => { await renderStep1(body); });
    document.getElementById('linear-wizard-next').disabled = true;
  }
}

function renderStep1Teams(body) {
  const { teams, selectedTeamId } = wizardState;
  const items = teams.map(team => {
    const sel = team.id === selectedTeamId;
    return `<div class="wizard-db-item${sel ? ' selected' : ''}" data-team-id="${escapeHtml(team.id)}" data-team-name="${escapeHtml(team.name)}">${escapeHtml(team.name)}</div>`;
  }).join('');
  body.innerHTML = `
    <div class="wizard-step-title">Select Team</div>
    <div class="wizard-db-list">${items}</div>
    ${wizardState.error ? `<div class="wizard-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  const freshNext = replaceNextBtn();
  freshNext.disabled = !selectedTeamId; freshNext.textContent = 'Next';
  body.querySelectorAll('.wizard-db-item').forEach(item => {
    item.addEventListener('click', () => {
      body.querySelectorAll('.wizard-db-item').forEach(i => i.classList.remove('selected'));
      item.classList.add('selected');
      wizardState.selectedTeamId = item.dataset.teamId;
      wizardState.selectedTeamName = item.dataset.teamName;
      freshNext.disabled = false;
    });
  });
  freshNext.addEventListener('click', async () => {
    if (!wizardState.selectedTeamId) return;
    freshNext.disabled = true; freshNext.textContent = '...';
    try {
      wizardState.profile = await invoke('sync_linear_schema', { integrationId: wizardState.integrationId, teamId: wizardState.selectedTeamId, teamName: wizardState.selectedTeamName });
      wizardState.error = null; wizardState.step = 2; renderStep();
    } catch (err) {
      wizardState.error = String(err); freshNext.disabled = false; freshNext.textContent = 'Next';
      renderStep1Teams(body);
    }
  });
}

function renderStep2(body) {
  const profile = wizardState.profile;
  if (!profile) { body.innerHTML = '<div class="wizard-error">No schema loaded.</div>'; return; }
  const syncedAt = profile.synced_at ? new Date(profile.synced_at).toLocaleString() : 'Unknown';

  const stateRows = (profile.workflow_states || []).map(s =>
    `<tr><td>${escapeHtml(s.name)}</td><td>${escapeHtml(s.type_name)}</td></tr>`
  ).join('') || '<tr><td colspan="2" style="color: var(--text-secondary);">None</td></tr>';

  const labelRows = (profile.labels || []).map(l =>
    `<tr><td>${escapeHtml(l.name)}</td><td><span style="display:inline-block;width:10px;height:10px;border-radius:50%;background:${escapeHtml(l.color)};margin-right:4px;"></span>${escapeHtml(l.color)}</td></tr>`
  ).join('') || '<tr><td colspan="2" style="color: var(--text-secondary);">None</td></tr>';

  const memberRows = (profile.members || []).map(m =>
    `<tr><td>${escapeHtml(m.display_name)}</td><td>${escapeHtml(m.email || '\u2014')}</td></tr>`
  ).join('') || '<tr><td colspan="2" style="color: var(--text-secondary);">None</td></tr>';

  const priorityRows = (profile.priorities || []).map(p =>
    `<tr><td>${escapeHtml(p.label)}</td><td>${p.priority}</td></tr>`
  ).join('') || '<tr><td colspan="2" style="color: var(--text-secondary);">None</td></tr>';

  body.innerHTML = `
    <div class="wizard-step-title">Team Schema: ${escapeHtml(profile.team_name)}</div>
    <div style="max-height: 280px; overflow-y: auto; font-size: 0.82rem;">
      <div style="margin-bottom: 10px;"><strong>Workflow States</strong>
        <table class="wizard-schema-table"><thead><tr><th>Name</th><th>Type</th></tr></thead><tbody>${stateRows}</tbody></table>
      </div>
      <div style="margin-bottom: 10px;"><strong>Labels</strong>
        <table class="wizard-schema-table"><thead><tr><th>Name</th><th>Color</th></tr></thead><tbody>${labelRows}</tbody></table>
      </div>
      <div style="margin-bottom: 10px;"><strong>Members</strong>
        <table class="wizard-schema-table"><thead><tr><th>Display Name</th><th>Email</th></tr></thead><tbody>${memberRows}</tbody></table>
      </div>
      <div style="margin-bottom: 10px;"><strong>Priorities</strong>
        <table class="wizard-schema-table"><thead><tr><th>Label</th><th>Value</th></tr></thead><tbody>${priorityRows}</tbody></table>
      </div>
    </div>
    <div class="wizard-schema-synced">Last synced: ${escapeHtml(syncedAt)}</div>
    <button id="linear-resync-btn" class="mini-action-btn" style="margin-top: 10px;">Re-sync Schema</button>
    ${wizardState.error ? `<div class="wizard-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  const resyncBtn = document.getElementById('linear-resync-btn');
  if (resyncBtn) {
    resyncBtn.addEventListener('click', async () => {
      resyncBtn.disabled = true; resyncBtn.textContent = '...';
      try {
        wizardState.profile = await invoke('sync_linear_schema', { integrationId: wizardState.integrationId, teamId: wizardState.selectedTeamId, teamName: wizardState.selectedTeamName });
        wizardState.error = null; renderStep();
      } catch (err) { wizardState.error = String(err); renderStep(); }
    });
  }
  const freshNext = replaceNextBtn();
  freshNext.textContent = 'Next';
  freshNext.addEventListener('click', () => {
    if (wizardState.aliases.length === 0) wizardState.aliases = [{ alias: '', memberId: '', displayName: '' }];
    wizardState.step = 3; renderStep();
  });
}

function renderStep3(body) {
  const profile = wizardState.profile;
  const members = (profile && profile.members) ? profile.members : [];

  function renderAliasRows() {
    const rowsEl = document.getElementById('linear-alias-rows');
    if (!rowsEl) return;
    const memberOptions = members.map(m =>
      `<option value="${escapeHtml(m.id)}" data-name="${escapeHtml(m.display_name)}">${escapeHtml(m.display_name)}</option>`
    ).join('');
    rowsEl.innerHTML = wizardState.aliases.map((a, idx) => `
      <div class="wizard-mapping-row" data-alias-idx="${idx}">
        <input type="text" class="wizard-mapping-alias" placeholder="Alias (e.g. me)" value="${escapeHtml(a.alias)}" />
        <select class="wizard-mapping-user"><option value="">Select member...</option>${memberOptions}</select>
        <button class="wizard-mapping-remove" title="Remove">x</button>
      </div>
    `).join('');
    rowsEl.querySelectorAll('.wizard-mapping-row').forEach((row, idx) => {
      const select = row.querySelector('.wizard-mapping-user');
      if (select && wizardState.aliases[idx].memberId) select.value = wizardState.aliases[idx].memberId;
      row.querySelector('.wizard-mapping-alias').addEventListener('input', (e) => { wizardState.aliases[idx].alias = e.target.value; });
      select.addEventListener('change', () => {
        const opt = select.options[select.selectedIndex];
        wizardState.aliases[idx].memberId = select.value;
        wizardState.aliases[idx].displayName = opt ? (opt.dataset.name || '') : '';
      });
      row.querySelector('.wizard-mapping-remove').addEventListener('click', () => { wizardState.aliases.splice(idx, 1); renderAliasRows(); });
    });
  }

  body.innerHTML = `
    <div class="wizard-step-title">Member Alias Mapping</div>
    <p class="wizard-step-description">Map aliases (like 'me' or 'john') to Linear team members. These aliases can be used in AI output to assign issues.</p>
    <div id="linear-alias-rows"></div>
    <button id="linear-add-alias-btn" class="mini-action-btn" style="margin-top: 4px;">+ Add mapping</button>
    ${wizardState.error ? `<div class="wizard-error" id="linear-alias-error">${escapeHtml(wizardState.error)}</div>` : ''}
  `;
  renderAliasRows();
  const addBtn = document.getElementById('linear-add-alias-btn');
  if (addBtn) addBtn.addEventListener('click', () => { wizardState.aliases.push({ alias: '', memberId: '', displayName: '' }); renderAliasRows(); });

  const freshNext = replaceNextBtn();
  freshNext.textContent = 'Finish';
  freshNext.addEventListener('click', async () => {
    const clean = wizardState.aliases.filter(a => a.alias.trim() && a.memberId);
    freshNext.disabled = true; freshNext.textContent = '...';
    try {
      if (clean.length > 0) {
        const payload = clean.map(a => ({ alias: a.alias.trim(), member_id: a.memberId, display_name: a.displayName }));
        await invoke('update_linear_member_aliases', { integrationId: wizardState.integrationId, aliases: payload });
      }
      close(); await loadAllIntegrations();
    } catch (err) {
      wizardState.error = String(err); freshNext.disabled = false; freshNext.textContent = 'Finish';
      const errEl = document.getElementById('linear-alias-error');
      if (errEl) { errEl.textContent = wizardState.error; }
      else { const d = document.createElement('div'); d.id = 'linear-alias-error'; d.className = 'wizard-error'; d.textContent = wizardState.error; body.appendChild(d); }
    }
  });
}
