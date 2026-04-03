// integrations/state.js — Local mutable state for integrations module

export let notionProfiles = [];
export let linearProfiles = [];
export let savePathIntegrations = [];
export let webhookProfiles = [];
export let llmModelsData = [];
export let llmFreshnessData = {};
export let activeDownloads = {};

export function setNotionProfiles(v) { notionProfiles = v; }
export function setLinearProfiles(v) { linearProfiles = v; }
export function setSavePathIntegrations(v) { savePathIntegrations = v; }
export function setWebhookProfiles(v) { webhookProfiles = v; }
export function setLlmModelsData(v) { llmModelsData = v; }
export function setLlmFreshnessData(v) { llmFreshnessData = v; }
export function setActiveDownloads(v) { activeDownloads = v; }
