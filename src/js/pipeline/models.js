// Model helpers and CLI availability cache

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { appSettings } from '../core/state.js';
import { llmModelsData } from '../integrations/state.js';
import { FALLBACK_CLI_INFO } from './constants.js';

export let cliAvailabilityCache = null;

export async function loadCliAvailability() {
  if (cliAvailabilityCache) return cliAvailabilityCache;
  try {
    cliAvailabilityCache = await invoke('check_cli_availability');
  } catch (err) {
    console.error('Failed to check CLI availability:', err);
    cliAvailabilityCache = FALLBACK_CLI_INFO;
  }
  return cliAvailabilityCache;
}

export function isChatCapableModel(modelId) {
  if (!modelId) return false;
  const id = modelId.toLowerCase();
  if (id.startsWith('whisper-') || id.includes('-transcribe')) return false;
  if (id.startsWith('text-embedding-')) return false;
  if (id.startsWith('tts-')) return false;
  if (id.startsWith('dall-e-')) return false;
  return true;
}

export function getModelsForProvider(providerId) {
  if (appSettings && appSettings.providers && appSettings.providers[providerId]) {
    return (appSettings.providers[providerId].models || []).filter(isChatCapableModel);
  }
  return [];
}

export function buildModelOptions(providerId, currentModel, expanded = false) {
  const LOCAL_MAX_DEFAULT = 4;

  if (providerId === 'cli_agent') {
    const cliCache = cliAvailabilityCache || [];
    const cliConfig = (appSettings && appSettings.cli_agent) ? appSettings.cli_agent : { cli: 'claude' };
    const selectedCli = cliConfig.cli || 'claude';
    const cliInfo = cliCache.find(c => c.id === selectedCli);
    const models = cliInfo ? cliInfo.models : [];
    if (models.length === 0) return { html: '<option value="default">Default</option>', hasMore: false, total: 1 };
    const html = '<option value="default" ' + ((!currentModel || currentModel === 'default') ? 'selected' : '') + '>Default</option>' +
      models.map(m => `<option value="${escapeHtml(m.id)}" ${currentModel === m.id ? 'selected' : ''}>${escapeHtml(m.name)}</option>`).join('');
    return { html, hasMore: false, total: models.length + 1 };
  }

  if (providerId === 'local') {
    const models = (llmModelsData || []).filter(m => m.downloaded);
    if (models.length === 0) return { html: '<option value="" disabled>No local models downloaded</option>', hasMore: false, total: 0 };
    let displayModels = expanded ? models : models.slice(0, LOCAL_MAX_DEFAULT);
    if (currentModel && !displayModels.some(m => m.id === currentModel)) {
      const selectedModel = models.find(m => m.id === currentModel);
      if (selectedModel) displayModels = [...displayModels, selectedModel];
    }
    const html = displayModels.map(m =>
      `<option value="${escapeHtml(m.id)}" ${currentModel === m.id ? 'selected' : ''}>${escapeHtml(m.name)} (${escapeHtml(m.params)})</option>`
    ).join('');
    return { html, hasMore: displayModels.length < models.length, total: models.length };
  }

  const models = getModelsForProvider(providerId);
  if (models.length === 0) return { html: '<option value="" disabled>No models available</option>', hasMore: false, total: 0 };

  const CLOUD_MAX_DEFAULT = 10;
  let displayModels;
  let hasMore = false;

  if (expanded) {
    displayModels = models;
  } else {
    displayModels = models.slice(0, CLOUD_MAX_DEFAULT);
    if (currentModel && !displayModels.includes(currentModel) && models.includes(currentModel)) {
      displayModels = [...displayModels, currentModel].filter((v, i, a) => a.indexOf(v) === i);
    }
    hasMore = displayModels.length < models.length;
  }

  const html = displayModels.map(m =>
    `<option value="${escapeHtml(m)}" ${currentModel === m ? 'selected' : ''}>${escapeHtml(m)}</option>`
  ).join('');

  return { html, hasMore, total: models.length };
}

export function trimModelName(model, provider) {
  if (!model) return '';
  const prefixes = { anthropic: 'claude-', openai: 'gpt-', google: 'gemini-' };
  const prefix = prefixes[provider] || '';
  return prefix && model.startsWith(prefix) ? model.slice(prefix.length) : model;
}
