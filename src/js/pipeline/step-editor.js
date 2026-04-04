// Step editor: unified step editor panel with Tool/Prompt/Delivery sections

import { invoke } from '../core/tauri.js';
import { escapeHtml } from '../core/utils.js';
import { allPromptTemplates, slackIntegrations } from '../core/state.js';
import { FALLBACK_CLI_INFO } from './constants.js';
import * as pipelineState from './state.js';
import { cliAvailabilityCache, buildModelOptions } from './models.js';
import { maybeAutoName } from './delivery-options.js';
import { fixStepInputs, closeStepEditorPanel, renderPipelineSteps } from './editor.js';

export function addNewStep() {
  const step = {
    name: '',
    connector: 'llm',
    input: pipelineState.pipelineEditorSteps.length > 0
      ? pipelineState.pipelineEditorSteps[pipelineState.pipelineEditorSteps.length - 1].name || 'transcript'
      : 'transcript',
    config: {},
  };
  pipelineState.pipelineEditorSteps.push(step);
  fixStepInputs();
  renderPipelineSteps();
  showStepEditor(pipelineState.pipelineEditorSteps.length - 1);
}

export function showStepEditor(index) {
  const step = pipelineState.pipelineEditorSteps[index];
  if (!step) return;

  const stepEditorPanelEl = document.getElementById('step-editor-panel');

  // Reverse-map existing step to unified UI state
  let toolType = 'none';
  let hasPrompt = false;
  let hasDelivery = false;

  if (step.connector === 'cli_agent') {
    toolType = 'cli';
    hasPrompt = !!(step.config?.prompt || step.config?.prompt_template);
  } else if (step.connector === 'llm') {
    toolType = 'model';
    hasPrompt = !!(step.config?.prompt_template || step.config?.prompt_inline);
  } else if (step.connector === 'slack') {
    hasDelivery = true;
  }
  if (!['llm', 'cli_agent', 'slack', ''].includes(step.connector) && step.connector) {
    toolType = 'model';
  }

  // CLI data
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

  // LLM Model data
  const currentProvider = step.config?.provider || 'openai';
  const currentModel = step.config?.model || '';
  const expanded = pipelineState.modelSelectExpanded[currentProvider] || false;
  const modelResult = buildModelOptions(currentProvider, currentModel, expanded);
  const toggleBtn = modelResult.hasMore
    ? `<button type="button" class="model-select-toggle" data-provider="${escapeHtml(currentProvider)}" style="font-size:0.68rem;color:var(--accent);background:none;border:none;cursor:pointer;padding:2px 0;margin-top:2px;">Show all ${modelResult.total}</button>`
    : (expanded && modelResult.total > 4 ? `<button type="button" class="model-select-toggle" data-provider="${escapeHtml(currentProvider)}" style="font-size:0.68rem;color:var(--text-secondary);background:none;border:none;cursor:pointer;padding:2px 0;margin-top:2px;">Show less</button>` : '');

  // Prompt data
  const promptTemplates = allPromptTemplates || [];
  const promptTemplateOptions = promptTemplates.map(t =>
    `<option value="${escapeHtml(t.name)}" ${step.config?.prompt_template === t.name ? 'selected' : ''}>${escapeHtml(t.name)}</option>`
  ).join('');
  const isInlinePrompt = !!(step.config?.prompt_inline || step.config?.prompt);
  const promptText = step.config?.prompt_inline || step.config?.prompt || '';

  // Slack data
  const slackEntries = Object.entries(slackIntegrations || {});
  const hasSlack = slackEntries.length > 0;
  const slackWsOptions = slackEntries.map(([id, data]) =>
    `<option value="${escapeHtml(id)}" ${step.config?.integration_id === id ? 'selected' : ''}>${escapeHtml(data.name)}</option>`
  ).join('');

  // Build unified editor HTML
  const editorHTML = buildEditorHTML({
    index, toolType, hasPrompt, hasDelivery, cliOptions, cliModelOpts,
    currentProvider, modelResult, toggleBtn, promptTemplateOptions,
    isInlinePrompt, promptText, hasSlack, slackWsOptions, slackEntries, step,
  });

  pipelineState.setEditingStepIndex(index);
  renderPipelineSteps();

  stepEditorPanelEl.innerHTML = editorHTML;
  stepEditorPanelEl.style.display = 'block';
  const editorEl = stepEditorPanelEl.querySelector('.step-editor');
  setTimeout(() => editorEl.scrollIntoView({ behavior: 'smooth', block: 'nearest' }), 50);

  wireEditorEvents(editorEl, step, index, cliInfo, slackEntries, hasSlack, hasDelivery);
}

function buildEditorHTML(d) {
  return `
    <div class="step-editor">
      <div class="step-editor-header">
        <span class="step-editor-title">Step ${d.index + 1}</span>
        <button class="step-editor-close" title="Close">\u00d7</button>
      </div>
      <div class="step-section">
        <div class="step-section-label">Tool</div>
        <div style="display:flex;gap:12px;margin-bottom:8px;">
          <label style="display:flex;align-items:center;gap:4px;cursor:pointer;font-size:0.85rem;">
            <input type="radio" name="tool-type-${d.index}" value="none" ${d.toolType === 'none' ? 'checked' : ''} class="tool-type-radio" /> None
          </label>
          <label style="display:flex;align-items:center;gap:4px;cursor:pointer;font-size:0.85rem;">
            <input type="radio" name="tool-type-${d.index}" value="cli" ${d.toolType === 'cli' ? 'checked' : ''} class="tool-type-radio" /> CLI
          </label>
          <label style="display:flex;align-items:center;gap:4px;cursor:pointer;font-size:0.85rem;">
            <input type="radio" name="tool-type-${d.index}" value="model" ${d.toolType === 'model' ? 'checked' : ''} class="tool-type-radio" /> Model
          </label>
        </div>
        <div class="tool-cli-section" style="display:${d.toolType === 'cli' ? 'flex' : 'none'};flex-direction:column;gap:8px;">
          <div class="step-editor-row"><label>CLI</label><select class="cli-select">${d.cliOptions}</select></div>
          <div class="step-editor-row"><label>Model</label><select class="cli-model-select">${d.cliModelOpts}</select></div>
        </div>
        <div class="tool-model-section" style="display:${d.toolType === 'model' ? 'flex' : 'none'};flex-direction:column;gap:8px;">
          <div class="step-editor-row"><label>Provider</label><select class="llm-provider-select">
            <option value="openai" ${d.currentProvider === 'openai' ? 'selected' : ''}>OpenAI</option>
            <option value="google" ${d.currentProvider === 'google' ? 'selected' : ''}>Google</option>
            <option value="anthropic" ${d.currentProvider === 'anthropic' ? 'selected' : ''}>Anthropic</option>
            <option value="local" ${d.currentProvider === 'local' ? 'selected' : ''}>Local LLM</option>
            <option value="ollama" ${d.currentProvider === 'ollama' ? 'selected' : ''}>Ollama</option>
          </select></div>
          <div class="step-editor-row"><label>Model</label><div><select class="llm-model-select">${d.modelResult.html}</select>${d.toggleBtn}</div></div>
        </div>
      </div>
      <div class="step-section" style="border-top:1px solid var(--border-color);padding-top:12px;">
        <label style="display:flex;align-items:center;gap:6px;cursor:pointer;">
          <input type="checkbox" class="prompt-toggle" ${d.hasPrompt ? 'checked' : ''} />
          <span class="step-section-label" style="margin:0;">Prompt</span>
        </label>
        <div class="prompt-body" style="display:${d.hasPrompt ? 'flex' : 'none'};flex-direction:column;gap:8px;margin-top:8px;">
          <div class="step-editor-row">
            <select class="prompt-source-select" style="max-width:140px;">
              <option value="template" ${!d.isInlinePrompt ? 'selected' : ''}>Template</option>
              <option value="inline" ${d.isInlinePrompt ? 'selected' : ''}>Custom</option>
            </select>
          </div>
          <div class="prompt-template-row" style="display:${d.isInlinePrompt ? 'none' : ''};">
            <select class="prompt-template-select"><option value="">Select template...</option>${d.promptTemplateOptions}</select>
          </div>
          <div class="prompt-inline-row" style="display:${d.isInlinePrompt ? '' : 'none'};">
            <textarea class="prompt-inline-textarea" rows="3" placeholder="Write your prompt. Use {transcript} for input.">${escapeHtml(d.promptText)}</textarea>
          </div>
        </div>
      </div>
      <div class="step-section" style="border-top:1px solid var(--border-color);padding-top:12px;">
        <label style="display:flex;align-items:center;gap:6px;cursor:pointer;">
          <input type="checkbox" class="delivery-toggle" ${d.hasDelivery ? 'checked' : ''} ${!d.hasSlack ? 'disabled' : ''} />
          <span class="step-section-label" style="margin:0;">Delivery</span>
          ${!d.hasSlack ? '<span style="font-size:0.75rem;color:var(--text-secondary);">(no Slack connected)</span>' : ''}
        </label>
        <div class="delivery-body" style="display:${d.hasDelivery && d.hasSlack ? 'flex' : 'none'};flex-direction:column;gap:8px;margin-top:8px;">
          ${d.slackEntries.length > 1 ? `<div class="step-editor-row"><label>Workspace</label><select class="slack-workspace-select"><option value="">Select...</option>${d.slackWsOptions}</select></div>` : ''}
          <div class="step-editor-row"><label>Channel</label>
            <select class="slack-target-select"><option value="">Select channel or person...</option></select>
            <div class="slack-target-loading" style="display:none;font-size:0.8rem;color:var(--text-secondary);margin-top:4px;">Loading...</div>
          </div>
        </div>
      </div>
      <div class="step-section" style="border-top:1px solid var(--border-color);padding-top:12px;">
        <div class="step-editor-row"><label>Name</label><input class="step-name-input" value="${escapeHtml(d.step.name || '')}" placeholder="Auto-generated" /></div>
      </div>
      <div class="step-editor-actions">
        <button class="step-editor-done">Done</button>
      </div>
    </div>`;
}

function wireEditorEvents(editorEl, step, index, cliInfo, slackEntries, hasSlack, hasDelivery) {
  // Close
  editorEl.querySelector('.step-editor-close').addEventListener('click', () => {
    if (!step.name && !step.config?.cli && !step.config?.provider && !step.config?.integration_id) {
      pipelineState.pipelineEditorSteps.splice(index, 1);
    }
    pipelineState.setEditingStepIndex(null);
    closeStepEditorPanel();
    renderPipelineSteps();
    maybeAutoName();
  });

  // Tool radio
  editorEl.querySelectorAll('.tool-type-radio').forEach(radio => {
    radio.addEventListener('change', () => {
      editorEl.querySelector('.tool-cli-section').style.display = radio.value === 'cli' ? 'flex' : 'none';
      editorEl.querySelector('.tool-model-section').style.display = radio.value === 'model' ? 'flex' : 'none';
    });
  });

  // CLI select -> update model dropdown
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

  // LLM provider -> update model dropdown
  const llmProviderSel = editorEl.querySelector('.llm-provider-select');
  if (llmProviderSel) {
    llmProviderSel.addEventListener('change', () => {
      const prov = llmProviderSel.value;
      const modelSel = editorEl.querySelector('.llm-model-select');
      if (!modelSel) return;
      const exp = pipelineState.modelSelectExpanded[prov] || false;
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

  // Model toggle (show all / show less)
  const modelToggle = editorEl.querySelector('.model-select-toggle');
  if (modelToggle) {
    modelToggle.addEventListener('click', () => {
      const prov = modelToggle.dataset.provider;
      pipelineState.modelSelectExpanded[prov] = !pipelineState.modelSelectExpanded[prov];
      const modelSel = editorEl.querySelector('.llm-model-select');
      if (!modelSel) return;
      const res = buildModelOptions(prov, modelSel.value, pipelineState.modelSelectExpanded[prov]);
      modelSel.innerHTML = res.html;
      modelToggle.textContent = res.hasMore ? `Show all ${res.total}` : 'Show less';
    });
  }

  // Prompt toggle
  const promptToggle = editorEl.querySelector('.prompt-toggle');
  const promptBody = editorEl.querySelector('.prompt-body');
  if (promptToggle && promptBody) {
    promptToggle.addEventListener('change', () => {
      promptBody.style.display = promptToggle.checked ? 'flex' : 'none';
    });
  }

  // Prompt source (template vs custom)
  const promptSourceSel = editorEl.querySelector('.prompt-source-select');
  if (promptSourceSel) {
    promptSourceSel.addEventListener('change', () => {
      const isInline = promptSourceSel.value === 'inline';
      editorEl.querySelector('.prompt-template-row').style.display = isInline ? 'none' : '';
      editorEl.querySelector('.prompt-inline-row').style.display = isInline ? '' : 'none';
    });
  }

  // Delivery toggle
  const deliveryToggle = editorEl.querySelector('.delivery-toggle');
  const deliveryBody = editorEl.querySelector('.delivery-body');
  if (deliveryToggle && deliveryBody) {
    deliveryToggle.addEventListener('change', () => {
      deliveryBody.style.display = deliveryToggle.checked ? 'flex' : 'none';
      if (deliveryToggle.checked) loadSlackTargets();
    });
  }

  // Slack channel loading
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
        invoke('list_slack_members', { integrationId }),
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
  if (hasDelivery && hasSlack) loadSlackTargets();

  // Done button
  editorEl.querySelector('.step-editor-done').addEventListener('click', () => {
    const tool = editorEl.querySelector('.tool-type-radio:checked')?.value || 'none';
    const promptOn = editorEl.querySelector('.prompt-toggle')?.checked;
    const deliveryOn = editorEl.querySelector('.delivery-toggle')?.checked;
    const nameVal = (editorEl.querySelector('.step-name-input')?.value || '').trim();

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
      step.connector = 'slack';
      const wsVal = editorEl.querySelector('.slack-workspace-select')?.value || (slackEntries.length === 1 ? slackEntries[0][0] : '');
      const targetVal = editorEl.querySelector('.slack-target-select')?.value || '';
      step.config = { integration_id: wsVal, target: targetVal };
      step.name = nameVal || 'send-to-slack';
    } else {
      pipelineState.pipelineEditorSteps.splice(index, 1);
      pipelineState.setEditingStepIndex(null);
      closeStepEditorPanel();
      renderPipelineSteps();
      return;
    }

    // If tool + delivery both selected, auto-add a chained Slack step
    if (tool !== 'none' && deliveryOn) {
      const wsVal = editorEl.querySelector('.slack-workspace-select')?.value || (slackEntries.length === 1 ? slackEntries[0][0] : '');
      const targetVal = editorEl.querySelector('.slack-target-select')?.value || '';
      const slackStep = {
        name: 'send-to-slack',
        connector: 'slack',
        input: step.name,
        config: { integration_id: wsVal, target: targetVal },
      };
      const nextStep = pipelineState.pipelineEditorSteps[index + 1];
      if (!nextStep || nextStep.connector !== 'slack') {
        pipelineState.pipelineEditorSteps.splice(index + 1, 0, slackStep);
      } else {
        nextStep.config = slackStep.config;
        nextStep.input = step.name;
      }
    }

    fixStepInputs();
    pipelineState.setEditingStepIndex(null);
    closeStepEditorPanel();
    renderPipelineSteps();
    maybeAutoName();
  });

  // Focus first interactive element
  const firstRadio = editorEl.querySelector('.tool-type-radio:checked');
  if (firstRadio) firstRadio.focus();
}
