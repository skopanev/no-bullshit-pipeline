import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { emit } from '../core/events.js';

const addSlackBtn = document.getElementById('add-slack-btn');
const addSlackModal = document.getElementById('add-slack-modal');
const slackModalOriginalHTML = addSlackModal ? addSlackModal.querySelector('.modal-card')?.innerHTML : '';

export async function loadSlackIntegrations() {
  try {
    state.setSlackIntegrations(await invoke('list_slack_integrations'));
  } catch (err) {
    console.error('Failed to load Slack integrations:', err);
  }
}

export function wireSlackModalButtons() {
  const cancelBtn = document.getElementById('slack-cancel-btn');
  const saveBtn = document.getElementById('slack-save-btn');

  if (cancelBtn) {
    cancelBtn.addEventListener('click', () => {
      addSlackModal.style.display = 'none';
    });
  }

  if (saveBtn) {
    saveBtn.addEventListener('click', async () => {
      const tokenInput = document.getElementById('slack-token-input');
      const token = tokenInput?.value.trim();
      if (!token) { showToast('Please enter a bot token', 'error'); return; }
      if (!token.startsWith('xoxb-')) { showToast('Invalid token format. Bot tokens start with xoxb-', 'error'); return; }

      const id = crypto.randomUUID();
      saveBtn.disabled = true;
      saveBtn.textContent = 'Connecting...';

      try {
        const workspaceName = await invoke('add_slack_integration', { id, token });
        const modalCard = addSlackModal.querySelector('.modal-card');
        if (modalCard) {
          modalCard.innerHTML = `
            <div style="text-align: center; padding: 32px 16px;">
              <div style="font-size: 2.5rem; margin-bottom: 12px;">&#10003;</div>
              <h3 style="margin: 0 0 8px;">${escapeHtml(workspaceName)} connected</h3>
              <p style="color: var(--text-secondary); margin: 0 0 24px;">Slack workspace added successfully</p>
              <button class="modal-btn primary" id="slack-success-done">Done</button>
            </div>
          `;
          document.getElementById('slack-success-done').addEventListener('click', () => {
            addSlackModal.style.display = 'none';
            modalCard.innerHTML = slackModalOriginalHTML;
            wireSlackModalButtons();
          });
        }
        await loadSlackIntegrations();
        state.setAppSettings(await invoke('load_settings'));
        emit('integrations:changed');
      } catch (err) {
        showToast(`Failed to add Slack workspace: ${err}`, 'error');
        saveBtn.disabled = false;
        saveBtn.textContent = 'Save';
      }
    });
  }
}

export function initSlack() {
  wireSlackModalButtons();
}
