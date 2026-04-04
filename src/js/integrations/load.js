// integrations/load.js — Load all integration data from backend

import { invoke } from '../core/tauri.js';
import { slackIntegrations } from '../core/state.js';
import * as intState from './state.js';
import { renderModelsProviders } from './providers.js';
import { renderLocalLlmModels } from './local-models.js';
import { renderConnectedIntegrations } from './connected.js';
import { renderAvailableIntegrations } from './available.js';

export async function loadAllIntegrations() {
  await Promise.all([
    loadNotionProfiles(),
    loadLinearProfiles(),
    loadSlackForIntegrations(),
    loadSavePathIntegrations(),
    loadWebhookProfiles(),
  ]);
  renderModelsProviders();
  renderLocalLlmModels();
  renderConnectedIntegrations();
  renderAvailableIntegrations();
}

async function loadNotionProfiles() {
  try {
    intState.setNotionProfiles(await invoke('list_notion_profiles'));
  } catch (err) {
    console.error('Failed to load Notion profiles:', err);
    intState.setNotionProfiles([]);
  }
}

async function loadLinearProfiles() {
  try {
    intState.setLinearProfiles(await invoke('list_linear_profiles'));
  } catch (err) {
    console.error('Failed to load Linear profiles:', err);
    intState.setLinearProfiles([]);
  }
}

async function loadSlackForIntegrations() {
  // slack is loaded via main.js init — no-op here to avoid circular deps
}

async function loadSavePathIntegrations() {
  try {
    intState.setSavePathIntegrations(await invoke('list_save_path_integrations'));
  } catch (err) {
    console.error('Failed to load save path integrations:', err);
    intState.setSavePathIntegrations([]);
  }
}

async function loadWebhookProfiles() {
  try {
    intState.setWebhookProfiles(await invoke('list_webhook_profiles'));
  } catch (err) {
    console.error('Failed to load webhook profiles:', err);
    intState.setWebhookProfiles([]);
  }
}
