// ===== PIPELINE DEFINITION MANAGEMENT =====
// Globals from main.js (loaded after this script, available by call-time): escapeHtml, invoke, allPromptTemplates, slackIntegrations
// Globals from integrations-settings.js (loaded before this script): notionProfiles, linearProfiles, savePathIntegrations, webhookProfiles (all via typeof guard)

var allPipelineDefs = [];
let editingPipelineDef = null; // null = new, string = editing name
let pipelineEditorSteps = []; // Working copy of steps
let editingStepIndex = null;  // index of step currently open in panel
let sortableInstance = null;  // Sortable.js instance for drag-and-drop reordering
let lastAutoName = '';        // Track last auto-generated pipeline name
let modelSelectExpanded = {}; // Track expanded state per provider in step editor
let cliAvailabilityCache = null; // Cache for CLI availability (null = not loaded yet)

const FALLBACK_CLI_INFO = [
  {
    id: 'claude',
    name: 'Claude Code',
    installed: false,
    install_hint: 'npm install -g @anthropic-ai/claude-code',
    models: [
      { id: 'sonnet', name: 'Claude Sonnet (latest)' },
      { id: 'opus', name: 'Claude Opus (latest)' },
      { id: 'haiku', name: 'Claude Haiku (latest)' },
      { id: 'claude-sonnet-4-20250514', name: 'Claude Sonnet 4' },
      { id: 'claude-opus-4-20250514', name: 'Claude Opus 4' },
    ],
    providers: [],
  },
  {
    id: 'codex',
    name: 'Codex CLI',
    installed: false,
    install_hint: 'npm install -g @openai/codex',
    models: [
      { id: 'o3', name: 'O3' },
      { id: 'o4-mini', name: 'O4 Mini' },
      { id: 'o3-mini', name: 'O3 Mini' },
    ],
    providers: [],
  },
  {
    id: 'opencode',
    name: 'OpenCode',
    installed: false,
    install_hint: 'npm install -g opencode-ai',
    models: [
      { id: 'openai/gpt-4o', name: 'GPT-4o' },
      { id: 'openai/gpt-4o-mini', name: 'GPT-4o Mini' },
      { id: 'openai/o3-mini', name: 'O3 Mini' },
      { id: 'anthropic/claude-sonnet-4-20250514', name: 'Claude Sonnet 4' },
      { id: 'anthropic/claude-opus-4-20250514', name: 'Claude Opus 4' },
      { id: 'anthropic/claude-3-5-sonnet-20241022', name: 'Claude 3.5 Sonnet' },
      { id: 'google/gemini-2.5-pro-preview-06-05', name: 'Gemini 2.5 Pro' },
      { id: 'google/gemini-2.0-flash', name: 'Gemini 2.0 Flash' },
    ],
    providers: [
      { id: 'openai', name: 'OpenAI' },
      { id: 'anthropic', name: 'Anthropic' },
      { id: 'google', name: 'Google' },
      { id: 'opencode', name: 'OpenCode (free)' },
      { id: 'zai-coding-plan', name: 'ZAI Coding Plan' },
    ],
  },
];

async function loadCliAvailability() {
  if (cliAvailabilityCache) return cliAvailabilityCache;
  try {
    cliAvailabilityCache = await invoke('check_cli_availability');
  } catch (err) {
    console.error('Failed to check CLI availability:', err);
    cliAvailabilityCache = FALLBACK_CLI_INFO;
  }
  return cliAvailabilityCache;
}

async function refreshCliAvailability() {
  cliAvailabilityCache = null;
  return loadCliAvailability();
}

const SLACK_SVG = `<svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor"><path d="M5.042 15.165a2.528 2.528 0 0 1-2.52 2.523A2.528 2.528 0 0 1 0 15.165a2.527 2.527 0 0 1 2.522-2.52h2.52v2.52zm1.271 0a2.527 2.527 0 0 1 2.521-2.52 2.527 2.527 0 0 1 2.521 2.52v6.313A2.528 2.528 0 0 1 8.834 24a2.528 2.528 0 0 1-2.521-2.522v-6.313zm2.521-10.123a2.528 2.528 0 0 1-2.521-2.52A2.528 2.528 0 0 1 8.834 0a2.528 2.528 0 0 1 2.521 2.522v2.52H8.834zm0 1.271a2.528 2.528 0 0 1 2.521 2.521 2.528 2.528 0 0 1-2.521 2.521H2.522A2.528 2.528 0 0 1 0 8.834a2.528 2.528 0 0 1 2.522-2.521h6.312zm10.123 2.521a2.528 2.528 0 0 1 2.522-2.521A2.528 2.528 0 0 1 24 8.834a2.528 2.528 0 0 1-2.522 2.521h-2.522V8.834zm-1.268 0a2.528 2.528 0 0 1-2.523 2.521 2.527 2.527 0 0 1-2.52-2.521V2.522A2.527 2.527 0 0 1 15.165 0a2.528 2.528 0 0 1 2.523 2.522v6.312zm-2.523 10.123a2.528 2.528 0 0 1 2.523 2.522A2.528 2.528 0 0 1 15.165 24a2.527 2.527 0 0 1-2.52-2.522v-2.522h2.52zm0-1.268a2.527 2.527 0 0 1-2.52-2.523 2.526 2.526 0 0 1 2.52-2.52h6.313A2.527 2.527 0 0 1 24 15.165a2.528 2.528 0 0 1-2.522 2.523h-6.313z"/></svg>`;
const NOTION_SVG = `<svg viewBox="0 0 100 100" width="18" height="18" fill="currentColor"><path d="M6.017 4.313l55.333-4.087c6.797-.583 8.543-.19 12.817 2.917l17.663 12.443c2.913 2.14 3.883 2.723 3.883 5.053v68.243c0 4.277-1.553 6.807-6.99 7.193L24.467 99.967c-4.08.193-6.023-.39-8.16-3.113L3.3 79.94c-2.333-3.113-3.3-5.443-3.3-8.167V11.113c0-3.497 1.553-6.413 6.017-6.8z" fill="#fff"/><path d="M61.35.227l-55.333 4.087C1.553 4.7 0 7.617 0 11.113v60.66c0 2.723.967 5.053 3.3 8.167l13.007 16.913c2.137 2.723 4.08 3.307 8.16 3.113l64.257-3.89c5.433-.387 6.99-2.917 6.99-7.193V20.64c0-2.21-.81-2.76-3.088-4.587L75.983 3.523C71.71.607 69.96.22 63.163.803L61.35.227z" fill="#000"/><path d="M26.395 18.768c-5.433.39-6.675.477-9.768-1.753L7.997 10.527c-1.163-.913-1.55-1.94-1.55-3.113.39-2.53 1.94-4.47 7.377-4.86l53.39-3.89c4.47-.39 6.603 1.553 8.157 2.723l10.133 7.577c.39.193 1.553 1.553 0 1.553l-55.14 3.11v5.14z" fill="#fff"/><path d="M19.018 88.4V30.173c0-2.527.78-3.697 3.113-3.89l57.277-3.307c2.14-.193 3.113 1.167 3.113 3.693V85.09c0 2.527-.39 4.667-3.887 4.86l-54.943 3.113c-3.5.193-4.673-1.003-4.673-4.663zm54.167-55.13c.39 1.75 0 3.5-1.75 3.697l-2.527.39v40.257c-2.14 1.163-4.277 1.75-5.833 1.75-2.723 0-3.5-.583-5.443-3.113L38.468 45.948V74.7l5.247 1.163s0 3.5-4.86 3.5l-13.393.78c-.39-.78 0-2.723 1.36-3.113l3.497-.97V38.33l-4.86-.39c-.39-1.75.583-4.277 3.307-4.473l14.363-.97 20.603 31.46V35.077l-4.47-.39c-.39-2.14 1.163-3.697 3.113-3.89l14.003-.527z" fill="#fff"/></svg>`;
const LINEAR_SVG = `<svg viewBox="0 0 100 100" width="18" height="18"><path d="M2.76 62.7a50.1 50.1 0 0 1-1.52-4.44L62.7 2.76a50.1 50.1 0 0 0-4.44-1.52L2.76 62.7zm7.66 12.48a50 50 0 0 1-3.54-4.3L75.18 4.58a50 50 0 0 0-4.3-3.54L10.42 75.18zm11.44 8.96a50 50 0 0 1-4.82-4.1L83.14 13.94a50 50 0 0 0-4.1-4.82L21.86 84.14zM0 50a49.9 49.9 0 0 0 .26 5L55 .26A50 50 0 1 0 0 50zm35.42 36.64a50 50 0 0 1-5.36-3.72L86.92 16.64a50 50 0 0 0-3.72-5.36L35.42 86.64z" fill="#5E6AD2"/></svg>`;
const SAVE_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`;
const WEBHOOK_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 007.54.54l3-3a5 5 0 00-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 00-7.54-.54l-3 3a5 5 0 007.07 7.07l1.71-1.71"/></svg>`;

const PROVIDER_META = {
  openai:    { img: 'assets/openai.svg',    filter: 'none', bgColor: 'transparent' },
  google:    { img: 'assets/gemini.svg',    filter: 'none', bgColor: 'transparent' },
  anthropic: { img: 'assets/anthropic.svg', filter: 'none', bgColor: 'transparent' },
  local:     { img: 'assets/local-llm.svg', filter: 'none', bgColor: 'transparent' },
  ollama:    { img: 'assets/ollama.svg',    filter: 'none', bgColor: 'transparent' },
};

function isChatCapableModel(modelId) {
  if (!modelId) return false;
  const id = modelId.toLowerCase();
  if (id.startsWith('whisper-') || id.includes('-transcribe')) return false;
  if (id.startsWith('text-embedding-')) return false;
  if (id.startsWith('tts-')) return false;
  if (id.startsWith('dall-e-')) return false;
  return true;
}

// Model lists read from appSettings.providers (provider-first config).
function getModelsForProvider(providerId) {
  // Check settings-stored models
  if (typeof appSettings !== 'undefined' && appSettings.providers && appSettings.providers[providerId]) {
    return (appSettings.providers[providerId].models || []).filter(isChatCapableModel);
  }
  return [];
}

function buildModelOptions(providerId, currentModel, expanded = false) {
  const LOCAL_MAX_DEFAULT = 4;
  if (providerId === 'cli_agent') {
    const cliCache = (typeof cliAvailabilityCache !== 'undefined') ? cliAvailabilityCache : [];
    const cliConfig = (typeof appSettings !== 'undefined' && appSettings.cli_agent) ? appSettings.cli_agent : { cli: 'claude' };
    const selectedCli = cliConfig.cli || 'claude';
    const cliInfo = cliCache.find(c => c.id === selectedCli);
    const models = cliInfo ? cliInfo.models : [];
    if (models.length === 0) return { html: '<option value="default">Default</option>', hasMore: false, total: 1 };
    const html = '<option value="default" ' + ((!currentModel || currentModel === 'default') ? 'selected' : '') + '>Default</option>' +
      models.map(m => `<option value="${escapeHtml(m.id)}" ${currentModel === m.id ? 'selected' : ''}>${escapeHtml(m.name)}</option>`).join('');
    return { html, hasMore: false, total: models.length + 1 };
  }
  if (providerId === 'local') {
    const models = (typeof llmModelsData !== 'undefined') ? llmModelsData.filter(m => m.downloaded) : [];
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

  const html = displayModels.map(m => {
    return `<option value="${escapeHtml(m)}" ${currentModel === m ? 'selected' : ''}>${escapeHtml(m)}</option>`;
  }).join('');

  return { html, hasMore, total: models.length };
}

function trimModelName(model, provider) {
  if (!model) return '';
  // Strip provider prefix: "claude-" → "", "gpt-" → "", "gemini-" → ""
  const prefixes = { anthropic: 'claude-', openai: 'gpt-', google: 'gemini-' };
  const prefix = prefixes[provider] || '';
  return prefix && model.startsWith(prefix) ? model.slice(prefix.length) : model;
}

const CLI_SVG = `<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>`;

const CONNECTOR_META = {
  llm:       { abbr: 'AI',  textColor: 'var(--accent)',  bgColor: 'var(--accent-soft)' },
  cli_agent: { svg: CLI_SVG, textColor: '#fff',          bgColor: 'rgba(99,102,241,0.9)' },
  save:      { svg: SAVE_SVG,    textColor: '#10b981',   bgColor: 'rgba(16,185,129,0.15)' },
  slack:     { svg: SLACK_SVG,   textColor: '#fff',      bgColor: '#4A154B' },
  notion:    { svg: NOTION_SVG,  textColor: '#fff',      bgColor: '#2f2f2f' },
  webhook:   { svg: WEBHOOK_SVG, textColor: '#60a5fa',   bgColor: 'rgba(59,130,246,0.2)' },
  linear:    { svg: LINEAR_SVG,  textColor: '#fff',      bgColor: '#5E6AD2' },
  mcp:       { abbr: 'MCP', textColor: '#f59e0b',        bgColor: 'rgba(245,158,11,0.15)' },
};

// Shared pipeline flow renderer — used in both builder preview and recording detail/status views.
// opts.compact (bool): icon-only chips (for cards); false = icon+label chips (for status/builder preview)
// opts.statuses (object): { stepName: 'done'|'failed'|'running'|'skipped' }
function renderPipelineFlowHTML(steps, opts = {}) {
  const { compact = false, statuses = {} } = opts;
  const MIC_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>`;
  const arrow = `<div class="pflow-arrow">›</div>`;

  let html = `<div class="pflow-chip pflow-chip--source" title="Transcript"><div class="pflow-chip-icon" style="background:var(--accent-soft);color:var(--accent);">${MIC_SVG}</div>${compact ? '' : '<span class="pflow-chip-label">Transcript</span>'}</div>`;

  for (const step of (steps || [])) {
    html += arrow;
    let meta = CONNECTOR_META[step.connector] || { abbr: step.connector.substring(0, 2).toUpperCase(), textColor: 'var(--text-primary)', bgColor: 'var(--bg-input)' };
    let iconContent = '';
    let bg = meta.bgColor;
    let fg = meta.textColor;

    if (step.connector === 'llm') {
      const provider = step.config?.provider || 'openai';
      const provMeta = PROVIDER_META[provider] || PROVIDER_META.openai;
      bg = provMeta.bgColor;
      iconContent = `<img src="${provMeta.img}" style="filter:${provMeta.filter};" alt="${provider}" />`;
    } else if (meta.svg) {
      iconContent = meta.svg;
    } else {
      iconContent = `<span style="font-size:7px;font-weight:800;color:${fg};">${meta.abbr}</span>`;
    }

    const st = statuses[step.name];
    const stClass = st ? ` pflow-chip--${st}` : '';
    const safeName = escapeHtml(step.name || '');

    html += `<div class="pflow-chip${stClass}" title="${safeName}"><div class="pflow-chip-icon" style="background:${bg};color:${fg};">${iconContent}</div>${compact ? '' : `<span class="pflow-chip-label">${safeName}</span>`}</div>`;
  }

  return `<div class="pflow${compact ? ' pflow--compact' : ''}">${html}</div>`;
}

const pipelineDefsListEl = document.getElementById('pipeline-defs-list');
const addPipelineDefBtn = document.getElementById('add-pipeline-def-btn');
const pipelineEditor = document.getElementById('pipeline-editor');
const pipelineEditorTitle = document.getElementById('pipeline-editor-title');
const pipelineEditorName = document.getElementById('pipeline-editor-name');
const pipelineEditorDesc = document.getElementById('pipeline-editor-desc');
const pipelineStepsListEl = document.getElementById('pipeline-steps-list');
const addPipelineStepBtn = document.getElementById('add-pipeline-step-btn');
const stepEditorPanelEl = document.getElementById('step-editor-panel');
const savePipelineDefBtn = document.getElementById('save-pipeline-def-btn');
const deletePipelineDefBtn = document.getElementById('delete-pipeline-def-btn');
const closePipelineEditorBtn = document.getElementById('close-pipeline-editor');


function addNewStep() {
  const step = {
    name: '',
    connector: 'llm',
    input: pipelineEditorSteps.length > 0 ? pipelineEditorSteps[pipelineEditorSteps.length - 1].name || 'transcript' : 'transcript',
    config: {},
  };
  pipelineEditorSteps.push(step);
  fixStepInputs();
  renderPipelineSteps();
  showStepEditor(pipelineEditorSteps.length - 1);
}

function buildDeliveryOptions() {
  const options = [];
  const profiles = (typeof notionProfiles !== 'undefined') ? notionProfiles : [];
  for (const p of profiles) {
    options.push({
      label: p.name + ' (Notion)',
      icon: NOTION_SVG,
      step: {
        name: 'send-to-' + p.name.toLowerCase().replace(/\s+/g, '-'),
        connector: 'notion',
        input: 'transcript',
        config: { integration_id: p.id },
        description: 'Send to ' + p.name
      }
    });
  }
  const linProfiles = (typeof linearProfiles !== 'undefined') ? linearProfiles : [];
  for (const p of linProfiles) {
    options.push({
      label: p.name + ' (Linear)',
      icon: LINEAR_SVG,
      step: {
        name: 'create-in-' + p.name.toLowerCase().replace(/\s+/g, '-'),
        connector: 'linear',
        input: 'transcript',
        config: { integration_id: p.id },
        description: 'Create issue in ' + p.name
      }
    });
  }
  const savePaths = (typeof savePathIntegrations !== 'undefined') ? savePathIntegrations : [];
  for (const sp of savePaths) {
    options.push({
      label: sp.name + ' (Save)',
      icon: SAVE_SVG,
      step: {
        name: 'save-to-' + sp.name.toLowerCase().replace(/\s+/g, '-'),
        connector: 'save',
        input: 'transcript',
        config: { save_path_id: sp.id, path: sp.path },
        description: 'Save to ' + sp.name
      }
    });
  }
  const slackWs = (typeof slackIntegrations !== 'undefined') ? slackIntegrations : {};
  for (const [id, data] of Object.entries(slackWs)) {
    options.push({
      label: (data.name || id) + ' (Slack)',
      icon: SLACK_SVG,
      step: {
        name: 'send-to-' + (data.name || id).toLowerCase().replace(/\s+/g, '-'),
        connector: 'slack',
        input: 'transcript',
        config: { integration_id: id },
        description: 'Send to ' + (data.name || id)
      }
    });
  }
  const whooks = (typeof webhookProfiles !== 'undefined') ? webhookProfiles : [];
  for (const wh of whooks) {
    options.push({
      label: wh.name + ' (Webhook)',
      icon: WEBHOOK_SVG,
      step: {
        name: 'send-to-' + wh.name.toLowerCase().replace(/\s+/g, '-'),
        connector: 'webhook',
        input: 'transcript',
        config: { integration_id: wh.id },
        description: 'Send to ' + wh.name
      }
    });
  }
  return options;
}


function suggestPipelineName() {
  const processing = [];
  const delivery = [];
  for (const step of pipelineEditorSteps) {
    if (step.connector === 'llm' || step.connector === 'mcp' || step.connector === 'cli_agent') {
      // Title-case the step name: "meeting-notes" → "Meeting Notes"
      const titleCased = (step.name || 'Untitled')
        .replace(/[-_]/g, ' ')
        .replace(/\b\w/g, c => c.toUpperCase());
      processing.push(titleCased);
    } else {
      // Delivery: connector name + integration profile name if available
      const connectorName = (step.connector || 'Unknown').charAt(0).toUpperCase() + (step.connector || '').slice(1);
      delivery.push(connectorName);
    }
  }
  if (processing.length === 0 && delivery.length === 0) return '';
  const parts = [];
  if (processing.length) parts.push(processing.join(', '));
  if (delivery.length) parts.push(delivery.join(', '));
  return parts.join(' \u2192 '); // → arrow
}

function maybeAutoName() {
  const currentVal = pipelineEditorName.value.trim();
  if (currentVal === '' || currentVal === lastAutoName) {
    const suggested = suggestPipelineName();
    pipelineEditorName.value = suggested;
    lastAutoName = suggested;
  }
}


async function loadPipelineDefs() {
  try {
    allPipelineDefs = await invoke('list_pipelines');
    // Prune assigned pipelines that no longer exist (e.g. deleted mid-recording)
    if (typeof currentAssignedPipelines !== 'undefined' && currentAssignedPipelines.size > 0) {
      const existingNames = new Set(allPipelineDefs.map(p => p.name));
      for (const name of [...currentAssignedPipelines]) {
        if (!existingNames.has(name)) currentAssignedPipelines.delete(name);
      }
    }
    renderPipelineDefsList();
    if (typeof renderPipelineChips === 'function') renderPipelineChips();
    if (typeof populateDefaultPipelineSelect === 'function') populateDefaultPipelineSelect();
    // Pre-load CLI availability in background
    loadCliAvailability();
  } catch (err) {
    console.error('Failed to load pipelines:', err);
    if (pipelineDefsListEl) {
      pipelineDefsListEl.innerHTML = `<div style="color: var(--danger); opacity: 0.9; font-size: 0.85rem;">Failed to load pipelines: ${escapeHtml(String(err))}</div>`;
    }
  }
}

function renderPipelineDefsList() {
  if (!pipelineDefsListEl) return;
  if (allPipelineDefs.length === 0) {
    pipelineDefsListEl.innerHTML = '<div style="color: var(--text-secondary); opacity: 0.6; font-size: 0.85rem;">No pipelines yet.</div>';
    return;
  }
  pipelineDefsListEl.innerHTML = allPipelineDefs.map(p => {
    const safeName = escapeHtml(p.name);
    const safeDesc = escapeHtml(p.description || '');
    const updated = p.updated_at ? new Date(p.updated_at).toLocaleDateString() : '';
    const flowHtml = renderPipelineFlowHTML(p.steps || [], { compact: true });
    const meta = [safeDesc, updated].filter(Boolean).join(' &middot; ');
    return `
    <div class="pipeline-def-item" data-name="${safeName}">
      <div class="pipeline-def-info">
        <div class="pipeline-def-name">${safeName}</div>
        <div class="pipeline-def-flow">${flowHtml}</div>
        ${meta ? `<div class="pipeline-def-desc">${meta}</div>` : ''}
      </div>
    </div>
  `;
  }).join('');

  pipelineDefsListEl.querySelectorAll('.pipeline-def-item').forEach(el => {
    el.addEventListener('click', () => openPipelineEditor(el.dataset.name));
  });
}

function openPipelineEditor(name) {
  if (!pipelineEditor) return;
  lastAutoName = '';
  if (name) {
    const p = allPipelineDefs.find(p => p.name === name);
    if (!p) return;
    editingPipelineDef = name;
    pipelineEditorTitle.textContent = 'Edit Pipeline';
    pipelineEditorName.value = p.name;
    pipelineEditorDesc.value = p.description || '';
    pipelineEditorSteps = JSON.parse(JSON.stringify(p.steps));
    if (deletePipelineDefBtn) deletePipelineDefBtn.style.display = 'inline-block';
  } else {
    editingPipelineDef = null;
    pipelineEditorTitle.textContent = 'New Pipeline';
    pipelineEditorName.value = '';
    pipelineEditorDesc.value = '';
    pipelineEditorSteps = [];
    if (deletePipelineDefBtn) deletePipelineDefBtn.style.display = 'none';
  }
  editingStepIndex = null;
  closeStepEditorPanel();
  pipelineEditor.style.display = 'block';
  renderPipelineSteps();
  pipelineEditorName.focus();
}

function closePipelineEditor() {
  if (pipelineEditor) pipelineEditor.style.display = 'none';
  editingPipelineDef = null;
  editingStepIndex = null;
  pipelineEditorSteps = [];
  closeStepEditorPanel();
}

function closeStepEditorPanel() {
  if (stepEditorPanelEl) {
    stepEditorPanelEl.innerHTML = '';
    stepEditorPanelEl.style.display = 'none';
  }
}

function renderPipelineSteps() {
  if (!pipelineStepsListEl) return;

  const MIC_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>`;

  // Source chip (always first, non-interactive)
  let html = `<div class="pflow-chip pflow-chip--source" title="Transcript">
    <div class="pflow-chip-icon" style="background:var(--accent-soft);color:var(--accent);">${MIC_SVG}</div>
    <span class="pflow-chip-label">Transcript</span>
  </div>`;

  for (let i = 0; i < pipelineEditorSteps.length; i++) {
    const step = pipelineEditorSteps[i];
    let meta = CONNECTOR_META[step.connector] || {
      abbr: step.connector.substring(0, 2).toUpperCase(),
      textColor: 'var(--text-primary)',
      bgColor: 'var(--bg-input)'
    };
    let iconContent = '';
    let bg = meta.bgColor;
    let fg = meta.textColor;
    let subText = escapeHtml(step.connector);

    if (step.connector === 'llm') {
      const provider = step.config?.provider || 'openai';
      const provMeta = PROVIDER_META[provider] || PROVIDER_META.openai;
      bg = provMeta.bgColor;
      iconContent = `<img src="${provMeta.img}" style="filter:${provMeta.filter};" alt="${provider}" />`;
      const model = step.config?.model || '';
      if (model) {
        let short;
        if (provider === 'local') {
          const localModel = (typeof llmModelsData !== 'undefined') ? llmModelsData.find(m => m.id === model) : null;
          short = localModel ? localModel.name : model;
        } else {
          short = trimModelName(model, provider);
        }
        subText = escapeHtml(short);
      } else {
        subText = escapeHtml(provider);
      }
    } else if (meta.svg) {
      iconContent = meta.svg;
    } else {
      iconContent = `<span style="font-size:7px;font-weight:800;color:${fg};">${meta.abbr}</span>`;
    }

    const safeName = escapeHtml(step.name || 'Unnamed');
    const isEditing = editingStepIndex === i;

    html += `<div class="pflow-chip${isEditing ? ' pflow-chip--editing' : ''}" data-index="${i}" title="${safeName}">
      <span class="pflow-chip-num">${i + 1}</span>
      <div class="pflow-chip-icon" style="background:${bg};color:${fg};">${iconContent}</div>
      <div class="pflow-chip-label-group">
        <span class="pflow-chip-label">${safeName}</span>
        <span class="pflow-chip-sub">${subText}</span>
      </div>
      <button class="pflow-chip-remove" data-index="${i}" title="Remove step" aria-label="Remove step">×</button>
    </div>`;
  }

  // Add step chip (dashed ghost)
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
      const stepName = pipelineEditorSteps[idx]?.name || `Step ${idx + 1}`;
      const ok = await showConfirm('Remove Step?', `Remove step "${stepName}" from pipeline?`);
      if (!ok) return;
      pipelineEditorSteps.splice(idx, 1);
      if (editingStepIndex === idx) {
        editingStepIndex = null;
        closeStepEditorPanel();
      } else if (editingStepIndex !== null && editingStepIndex > idx) {
        editingStepIndex--;
      }
      fixStepInputs();
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
  if (sortableInstance) {
    sortableInstance.destroy();
    sortableInstance = null;
  }
  if (typeof Sortable !== 'undefined' && pfFlowEl) {
    sortableInstance = Sortable.create(pfFlowEl, {
      draggable: '.pflow-chip[data-index]',
      filter: '.pflow-chip--source, .pflow-chip--add',
      ghostClass: 'sortable-ghost',
      chosenClass: 'sortable-chosen',
      dragClass: 'sortable-drag',
      animation: 150,
      onEnd(evt) {
        const movedChip = evt.item;
        const movedIdx = parseInt(movedChip.dataset.index);
        // Determine new semantic index from DOM order of [data-index] chips
        const allStepChips = [...pfFlowEl.querySelectorAll('.pflow-chip[data-index]')];
        const newIdx = allStepChips.indexOf(movedChip);
        if (movedIdx === newIdx || newIdx < 0) { renderPipelineSteps(); return; }
        const [moved] = pipelineEditorSteps.splice(movedIdx, 1);
        pipelineEditorSteps.splice(newIdx, 0, moved);
        if (editingStepIndex === movedIdx) editingStepIndex = newIdx;
        else if (editingStepIndex !== null) {
          if (movedIdx < editingStepIndex && newIdx >= editingStepIndex) editingStepIndex--;
          else if (movedIdx > editingStepIndex && newIdx <= editingStepIndex) editingStepIndex++;
        }
        fixStepInputs();
        renderPipelineSteps();
        if (editingStepIndex !== null) showStepEditor(editingStepIndex);
      }
    });
  }
}

function fixStepInputs() {
  // Fix input references: first step must reference 'transcript'
  // Others can reference 'transcript' or a previous step name
  for (let i = 0; i < pipelineEditorSteps.length; i++) {
    const step = pipelineEditorSteps[i];
    if (i === 0) {
      step.input = 'transcript';
    } else {
      const validInputs = ['transcript', ...pipelineEditorSteps.slice(0, i).map(s => s.name)];
      if (!validInputs.includes(step.input)) {
        step.input = pipelineEditorSteps[i - 1].name || 'transcript';
      }
    }
  }
}

function showStepEditor(index) {
  const step = pipelineEditorSteps[index];
  if (!step) return;

  // --- Reverse-map existing step to unified UI state ---
  let toolType = 'none';
  let hasPrompt = false;
  let hasDelivery = false;

  if (step.connector === 'cli_agent') {
    toolType = 'cli';
    hasPrompt = !!step.config?.prompt;
  } else if (step.connector === 'llm') {
    toolType = 'model';
    hasPrompt = !!(step.config?.prompt_template || step.config?.prompt_inline);
  } else if (step.connector === 'slack') {
    hasDelivery = true;
  }
  // For unsupported legacy connectors, default to model tool
  if (!['llm', 'cli_agent', 'slack', ''].includes(step.connector) && step.connector) {
    toolType = 'model';
  }

  // --- CLI data ---
  const cliInfo = cliAvailabilityCache || FALLBACK_CLI_INFO;
  const currentCliId = step.config?.cli || cliInfo.find(c => c.installed)?.id || 'claude';
  const cliOptions = cliInfo.map(cli => {
    const icon = cli.installed ? '\u2713' : '\u2717';
    return `<option value="${escapeHtml(cli.id)}" ${currentCliId === cli.id ? 'selected' : ''}>${escapeHtml(cli.name)} ${icon}</option>`;
  }).join('');
  const selectedCli = cliInfo.find(c => c.id === currentCliId);
  const cliModels = selectedCli?.models || [];
  const currentCliModel = step.config?.model || '';
  const cliModelOpts = (cliModels.length > 0
    ? '<option value="">Default</option>' + cliModels.map(m => `<option value="${escapeHtml(m.id)}" ${currentCliModel === m.id ? 'selected' : ''}>${escapeHtml(m.name)}</option>`).join('')
    : '<option value="">Default</option>');

  // --- LLM Model data ---
  const currentProvider = step.config?.provider || 'openai';
  const currentModel = step.config?.model || '';
  const expanded = modelSelectExpanded[currentProvider] || false;
  const modelResult = buildModelOptions(currentProvider, currentModel, expanded);
  const toggleBtn = modelResult.hasMore
    ? `<button type="button" class="model-select-toggle" data-provider="${escapeHtml(currentProvider)}" style="font-size:0.68rem;color:var(--accent);background:none;border:none;cursor:pointer;padding:2px 0;margin-top:2px;">Show all ${modelResult.total}</button>`
    : (expanded && modelResult.total > 4 ? `<button type="button" class="model-select-toggle" data-provider="${escapeHtml(currentProvider)}" style="font-size:0.68rem;color:var(--text-secondary);background:none;border:none;cursor:pointer;padding:2px 0;margin-top:2px;">Show less</button>` : '');

  // --- Prompt data ---
  const promptTemplates = (typeof allPromptTemplates !== 'undefined') ? allPromptTemplates : [];
  const promptTemplateOptions = promptTemplates.map(t =>
    `<option value="${escapeHtml(t.name)}" ${step.config?.prompt_template === t.name ? 'selected' : ''}>${escapeHtml(t.name)}</option>`
  ).join('');
  const isInlinePrompt = !!(step.config?.prompt_inline || step.config?.prompt);
  const promptText = step.config?.prompt_inline || step.config?.prompt || '';

  // --- Slack data ---
  const slackEntries = Object.entries(typeof slackIntegrations !== 'undefined' ? slackIntegrations : {});
  const hasSlack = slackEntries.length > 0;
  const slackWsOptions = slackEntries.map(([id, data]) =>
    `<option value="${escapeHtml(id)}" ${step.config?.integration_id === id ? 'selected' : ''}>${escapeHtml(data.name)}</option>`
  ).join('');

  // --- Build unified editor HTML ---
  const editorHTML = `
    <div class="step-editor">
      <div class="step-editor-header">
        <span class="step-editor-title">Step ${index + 1}</span>
        <button class="step-editor-close" title="Close">\u00d7</button>
      </div>

      <div class="step-section">
        <div class="step-section-label">Tool</div>
        <div style="display:flex;gap:12px;margin-bottom:8px;">
          <label style="display:flex;align-items:center;gap:4px;cursor:pointer;font-size:0.85rem;">
            <input type="radio" name="tool-type-${index}" value="none" ${toolType === 'none' ? 'checked' : ''} class="tool-type-radio" /> None
          </label>
          <label style="display:flex;align-items:center;gap:4px;cursor:pointer;font-size:0.85rem;">
            <input type="radio" name="tool-type-${index}" value="cli" ${toolType === 'cli' ? 'checked' : ''} class="tool-type-radio" /> CLI
          </label>
          <label style="display:flex;align-items:center;gap:4px;cursor:pointer;font-size:0.85rem;">
            <input type="radio" name="tool-type-${index}" value="model" ${toolType === 'model' ? 'checked' : ''} class="tool-type-radio" /> Model
          </label>
        </div>
        <div class="tool-cli-section" style="display:${toolType === 'cli' ? 'flex' : 'none'};flex-direction:column;gap:8px;">
          <div class="step-editor-row"><label>CLI</label><select class="cli-select">${cliOptions}</select></div>
          <div class="step-editor-row"><label>Model</label><select class="cli-model-select">${cliModelOpts}</select></div>
        </div>
        <div class="tool-model-section" style="display:${toolType === 'model' ? 'flex' : 'none'};flex-direction:column;gap:8px;">
          <div class="step-editor-row"><label>Provider</label><select class="llm-provider-select">
            <option value="openai" ${currentProvider === 'openai' ? 'selected' : ''}>OpenAI</option>
            <option value="google" ${currentProvider === 'google' ? 'selected' : ''}>Google</option>
            <option value="anthropic" ${currentProvider === 'anthropic' ? 'selected' : ''}>Anthropic</option>
            <option value="local" ${currentProvider === 'local' ? 'selected' : ''}>Local LLM</option>
            <option value="ollama" ${currentProvider === 'ollama' ? 'selected' : ''}>Ollama</option>
          </select></div>
          <div class="step-editor-row"><label>Model</label><div><select class="llm-model-select">${modelResult.html}</select>${toggleBtn}</div></div>
        </div>
      </div>

      <div class="step-section" style="border-top:1px solid var(--border-color);padding-top:12px;">
        <label style="display:flex;align-items:center;gap:6px;cursor:pointer;">
          <input type="checkbox" class="prompt-toggle" ${hasPrompt ? 'checked' : ''} />
          <span class="step-section-label" style="margin:0;">Prompt</span>
        </label>
        <div class="prompt-body" style="display:${hasPrompt ? 'flex' : 'none'};flex-direction:column;gap:8px;margin-top:8px;">
          <div class="step-editor-row">
            <select class="prompt-source-select" style="max-width:140px;">
              <option value="template" ${!isInlinePrompt ? 'selected' : ''}>Template</option>
              <option value="inline" ${isInlinePrompt ? 'selected' : ''}>Custom</option>
            </select>
          </div>
          <div class="prompt-template-row" style="display:${isInlinePrompt ? 'none' : ''};">
            <select class="prompt-template-select"><option value="">Select template...</option>${promptTemplateOptions}</select>
          </div>
          <div class="prompt-inline-row" style="display:${isInlinePrompt ? '' : 'none'};">
            <textarea class="prompt-inline-textarea" rows="3" placeholder="Write your prompt. Use {transcript} for input.">${escapeHtml(promptText)}</textarea>
          </div>
        </div>
      </div>

      <div class="step-section" style="border-top:1px solid var(--border-color);padding-top:12px;">
        <label style="display:flex;align-items:center;gap:6px;cursor:pointer;">
          <input type="checkbox" class="delivery-toggle" ${hasDelivery ? 'checked' : ''} ${!hasSlack ? 'disabled' : ''} />
          <span class="step-section-label" style="margin:0;">Delivery</span>
          ${!hasSlack ? '<span style="font-size:0.75rem;color:var(--text-secondary);">(no Slack connected)</span>' : ''}
        </label>
        <div class="delivery-body" style="display:${hasDelivery && hasSlack ? 'flex' : 'none'};flex-direction:column;gap:8px;margin-top:8px;">
          ${slackEntries.length > 1 ? `<div class="step-editor-row"><label>Workspace</label><select class="slack-workspace-select"><option value="">Select...</option>${slackWsOptions}</select></div>` : ''}
          <div class="step-editor-row"><label>Channel</label>
            <select class="slack-target-select"><option value="">Select channel or person...</option></select>
            <div class="slack-target-loading" style="display:none;font-size:0.8rem;color:var(--text-secondary);margin-top:4px;">Loading...</div>
          </div>
        </div>
      </div>

      <div class="step-section" style="border-top:1px solid var(--border-color);padding-top:12px;">
        <div class="step-editor-row"><label>Name</label><input class="step-name-input" value="${escapeHtml(step.name || '')}" placeholder="Auto-generated" /></div>
      </div>

      <div class="step-editor-actions">
        <button class="step-editor-done">Done</button>
      </div>
    </div>
  `;

  editingStepIndex = index;
  renderPipelineSteps();

  stepEditorPanelEl.innerHTML = editorHTML;
  stepEditorPanelEl.style.display = 'block';
  const editorEl = stepEditorPanelEl.querySelector('.step-editor');
  setTimeout(() => editorEl.scrollIntoView({ behavior: 'smooth', block: 'nearest' }), 50);

  // --- Wire: Close ---
  editorEl.querySelector('.step-editor-close').addEventListener('click', () => {
    // If step has no connector set (brand new, user cancelled), remove it
    if (!step.name && !step.config?.cli && !step.config?.provider && !step.config?.integration_id) {
      pipelineEditorSteps.splice(index, 1);
    }
    editingStepIndex = null;
    closeStepEditorPanel();
    renderPipelineSteps();
    maybeAutoName();
  });

  // --- Wire: Tool radio ---
  editorEl.querySelectorAll('.tool-type-radio').forEach(radio => {
    radio.addEventListener('change', () => {
      const val = radio.value;
      editorEl.querySelector('.tool-cli-section').style.display = val === 'cli' ? 'flex' : 'none';
      editorEl.querySelector('.tool-model-section').style.display = val === 'model' ? 'flex' : 'none';
    });
  });

  // --- Wire: CLI select → update model dropdown ---
  const cliSelect = editorEl.querySelector('.cli-select');
  if (cliSelect) {
    cliSelect.addEventListener('change', () => {
      const selCli = cliInfo.find(c => c.id === cliSelect.value);
      const modelSel = editorEl.querySelector('.cli-model-select');
      if (!modelSel || !selCli) return;
      const models = selCli.models || [];
      modelSel.innerHTML = '<option value="">Default</option>' +
        models.map(m => `<option value="${escapeHtml(m.id)}">${escapeHtml(m.name)}</option>`).join('');
    });
  }

  // --- Wire: LLM provider → update model dropdown ---
  const llmProviderSel = editorEl.querySelector('.llm-provider-select');
  if (llmProviderSel) {
    llmProviderSel.addEventListener('change', () => {
      const prov = llmProviderSel.value;
      const modelSel = editorEl.querySelector('.llm-model-select');
      if (!modelSel) return;
      const exp = modelSelectExpanded[prov] || false;
      const res = buildModelOptions(prov, '', exp);
      modelSel.innerHTML = res.html;
      const tb = editorEl.querySelector('.model-select-toggle');
      if (tb) {
        tb.dataset.provider = prov;
        tb.style.display = res.hasMore || (exp && res.total > 4) ? '' : 'none';
        tb.textContent = res.hasMore ? `Show all ${res.total}` : 'Show less';
      }
    });
  }

  // --- Wire: Model toggle ---
  const modelToggle = editorEl.querySelector('.model-select-toggle');
  if (modelToggle) {
    modelToggle.addEventListener('click', () => {
      const prov = modelToggle.dataset.provider;
      modelSelectExpanded[prov] = !modelSelectExpanded[prov];
      const modelSel = editorEl.querySelector('.llm-model-select');
      if (!modelSel) return;
      const res = buildModelOptions(prov, modelSel.value, modelSelectExpanded[prov]);
      modelSel.innerHTML = res.html;
      modelToggle.textContent = res.hasMore ? `Show all ${res.total}` : 'Show less';
    });
  }

  // --- Wire: Prompt toggle ---
  const promptToggle = editorEl.querySelector('.prompt-toggle');
  const promptBody = editorEl.querySelector('.prompt-body');
  if (promptToggle && promptBody) {
    promptToggle.addEventListener('change', () => {
      promptBody.style.display = promptToggle.checked ? 'flex' : 'none';
    });
  }

  // --- Wire: Prompt source (template vs custom) ---
  const promptSourceSel = editorEl.querySelector('.prompt-source-select');
  if (promptSourceSel) {
    promptSourceSel.addEventListener('change', () => {
      const isInline = promptSourceSel.value === 'inline';
      editorEl.querySelector('.prompt-template-row').style.display = isInline ? 'none' : '';
      editorEl.querySelector('.prompt-inline-row').style.display = isInline ? '' : 'none';
    });
  }

  // --- Wire: Delivery toggle ---
  const deliveryToggle = editorEl.querySelector('.delivery-toggle');
  const deliveryBody = editorEl.querySelector('.delivery-body');
  if (deliveryToggle && deliveryBody) {
    deliveryToggle.addEventListener('change', () => {
      deliveryBody.style.display = deliveryToggle.checked ? 'flex' : 'none';
      if (deliveryToggle.checked) loadSlackTargets();
    });
  }

  // --- Wire: Slack channel loading ---
  async function loadSlackTargets(wsId) {
    const wsSelect = editorEl.querySelector('.slack-workspace-select');
    const targetSelect = editorEl.querySelector('.slack-target-select');
    const loadingEl = editorEl.querySelector('.slack-target-loading');
    if (!targetSelect) return;

    const integrationId = wsId || (wsSelect ? wsSelect.value : '') || (slackEntries.length === 1 ? slackEntries[0][0] : '');
    if (!integrationId) return;

    loadingEl && (loadingEl.style.display = 'block');
    try {
      const [channels, members] = await Promise.all([
        invoke('list_slack_channels', { integrationId }),
        invoke('list_slack_members', { integrationId })
      ]);
      let opts = '<option value="">Select channel or person...</option>';
      if (channels.length > 0) {
        opts += '<optgroup label="Channels">';
        for (const ch of channels) {
          const prefix = ch.is_private ? '\uD83D\uDD12 ' : '#';
          const sel = step.config?.target === ch.id ? ' selected' : '';
          opts += `<option value="${escapeHtml(ch.id)}"${sel}>${prefix}${escapeHtml(ch.name)}</option>`;
        }
        opts += '</optgroup>';
      }
      if (members.length > 0) {
        opts += '<optgroup label="People">';
        for (const m of members) {
          const sel = step.config?.target === m.id ? ' selected' : '';
          opts += `<option value="${escapeHtml(m.id)}"${sel}>${escapeHtml(m.display_name)}</option>`;
        }
        opts += '</optgroup>';
      }
      targetSelect.innerHTML = opts;
    } catch (err) {
      targetSelect.innerHTML = `<option value="">Failed: ${escapeHtml(String(err))}</option>`;
    } finally {
      loadingEl && (loadingEl.style.display = 'none');
    }
  }

  const wsSelect = editorEl.querySelector('.slack-workspace-select');
  if (wsSelect) wsSelect.addEventListener('change', () => loadSlackTargets(wsSelect.value));
  // Auto-load if delivery is checked
  if (hasDelivery && hasSlack) loadSlackTargets();

  // --- Wire: Done ---
  editorEl.querySelector('.step-editor-done').addEventListener('click', () => {
    const tool = editorEl.querySelector('.tool-type-radio:checked')?.value || 'none';
    const promptOn = editorEl.querySelector('.prompt-toggle')?.checked;
    const deliveryOn = editorEl.querySelector('.delivery-toggle')?.checked;
    const nameInput = editorEl.querySelector('.step-name-input');
    const nameVal = nameInput ? nameInput.value.trim() : '';

    // Build processing step (CLI or Model)
    if (tool === 'cli') {
      step.connector = 'cli_agent';
      step.config = {
        cli: editorEl.querySelector('.cli-select')?.value || 'claude',
        timeout_secs: 300,
      };
      const cliModel = editorEl.querySelector('.cli-model-select')?.value;
      if (cliModel) step.config.model = cliModel;
      if (promptOn) {
        const src = editorEl.querySelector('.prompt-source-select')?.value;
        if (src === 'inline') {
          step.config.prompt = editorEl.querySelector('.prompt-inline-textarea')?.value?.trim() || '';
        } else {
          const tmpl = editorEl.querySelector('.prompt-template-select')?.value;
          if (tmpl) step.config.prompt_template = tmpl;
        }
      }
      step.name = nameVal || ('cli-' + (step.config.cli || 'agent'));
    } else if (tool === 'model') {
      step.connector = 'llm';
      step.config = {
        provider: editorEl.querySelector('.llm-provider-select')?.value || 'openai',
        model: editorEl.querySelector('.llm-model-select')?.value || '',
      };
      if (promptOn) {
        const src = editorEl.querySelector('.prompt-source-select')?.value;
        if (src === 'inline') {
          step.config.prompt_inline = editorEl.querySelector('.prompt-inline-textarea')?.value?.trim() || '';
        } else {
          const tmpl = editorEl.querySelector('.prompt-template-select')?.value;
          if (tmpl) step.config.prompt_template = tmpl;
        }
      }
      step.name = nameVal || step.config.prompt_template || ('llm-' + step.config.provider);
    } else if (deliveryOn) {
      // Delivery-only step
      step.connector = 'slack';
      const wsVal = editorEl.querySelector('.slack-workspace-select')?.value || (slackEntries.length === 1 ? slackEntries[0][0] : '');
      const targetVal = editorEl.querySelector('.slack-target-select')?.value || '';
      step.config = { integration_id: wsVal, target: targetVal };
      step.name = nameVal || 'send-to-slack';
    } else {
      // Nothing selected — remove the empty step
      pipelineEditorSteps.splice(index, 1);
      editingStepIndex = null;
      closeStepEditorPanel();
      renderPipelineSteps();
      return;
    }

    // If tool + delivery both selected → auto-add a chained Slack step
    if (tool !== 'none' && deliveryOn) {
      const wsVal = editorEl.querySelector('.slack-workspace-select')?.value || (slackEntries.length === 1 ? slackEntries[0][0] : '');
      const targetVal = editorEl.querySelector('.slack-target-select')?.value || '';
      const slackStep = {
        name: 'send-to-slack',
        connector: 'slack',
        input: step.name,
        config: { integration_id: wsVal, target: targetVal },
      };
      // Insert after current step (if not already there)
      const nextStep = pipelineEditorSteps[index + 1];
      if (!nextStep || nextStep.connector !== 'slack') {
        pipelineEditorSteps.splice(index + 1, 0, slackStep);
      } else {
        // Update existing slack step
        nextStep.config = slackStep.config;
        nextStep.input = step.name;
      }
    }

    fixStepInputs();
    editingStepIndex = null;
    closeStepEditorPanel();
    renderPipelineSteps();
    maybeAutoName();
  });

  // Focus first interactive element
  const firstRadio = editorEl.querySelector('.tool-type-radio:checked');
  if (firstRadio) firstRadio.focus();
}

if (addPipelineDefBtn) addPipelineDefBtn.addEventListener('click', () => openPipelineEditor(null));
if (closePipelineEditorBtn) closePipelineEditorBtn.addEventListener('click', closePipelineEditor);

if (savePipelineDefBtn) {
  savePipelineDefBtn.addEventListener('click', async () => {
    const name = pipelineEditorName.value.trim();
    const desc = pipelineEditorDesc.value.trim();
    if (!name) { showToast('Pipeline name is required', 'error'); return; }

    // Validate step names
    for (let i = 0; i < pipelineEditorSteps.length; i++) {
      if (!pipelineEditorSteps[i].name.trim()) {
        showToast(`Step ${i + 1} needs a name`, 'error');
        return;
      }
    }

    try {
      const pipeline = { name, description: desc, steps: pipelineEditorSteps };

      // If renaming, delete old first
      if (editingPipelineDef && editingPipelineDef !== name) {
        await invoke('delete_pipeline', { name: editingPipelineDef });
      }
      await invoke('save_pipeline', { pipeline });
      closePipelineEditor();
      await loadPipelineDefs();
    } catch (err) {
      console.error('Failed to save pipeline:', err);
      showToast('Failed to save: ' + err, 'error');
    }
  });
}

if (deletePipelineDefBtn) {
  deletePipelineDefBtn.addEventListener('click', async () => {
    if (!editingPipelineDef) return;
    const ok = await showConfirm('Delete Pipeline?', `Delete pipeline "${editingPipelineDef}"? This cannot be undone.`);
    if (!ok) return;
    try {
      await invoke('delete_pipeline', { name: editingPipelineDef });
      closePipelineEditor();
      await loadPipelineDefs();
    } catch (err) {
      console.error('Failed to delete pipeline:', err);
      showToast('Failed to delete: ' + err, 'error');
    }
  });
}

window.loadPipelineDefs = loadPipelineDefs;
window.openPipelineEditor = openPipelineEditor;
if (window.NBPModuleLoader) {
  window.NBPModuleLoader.register('pipeline-builder', {
    loadPipelineDefs,
    openPipelineEditor,
    renderPipelineDefsList,
  });
}
