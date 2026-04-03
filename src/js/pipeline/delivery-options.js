// Delivery options builder and auto-naming logic

import { NOTION_SVG, LINEAR_SVG, SAVE_SVG, SLACK_SVG, WEBHOOK_SVG } from './constants.js';
import { slackIntegrations } from '../core/state.js';
import * as pipelineState from './state.js';

export function buildDeliveryOptions() {
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
        description: 'Send to ' + p.name,
      },
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
        description: 'Create issue in ' + p.name,
      },
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
        description: 'Save to ' + sp.name,
      },
    });
  }

  const slackWs = slackIntegrations || {};
  for (const [id, data] of Object.entries(slackWs)) {
    options.push({
      label: (data.name || id) + ' (Slack)',
      icon: SLACK_SVG,
      step: {
        name: 'send-to-' + (data.name || id).toLowerCase().replace(/\s+/g, '-'),
        connector: 'slack',
        input: 'transcript',
        config: { integration_id: id },
        description: 'Send to ' + (data.name || id),
      },
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
        description: 'Send to ' + wh.name,
      },
    });
  }

  return options;
}

export function suggestPipelineName() {
  const processing = [];
  const delivery = [];
  for (const step of pipelineState.pipelineEditorSteps) {
    if (step.connector === 'llm' || step.connector === 'mcp' || step.connector === 'cli_agent') {
      const titleCased = (step.name || 'Untitled')
        .replace(/[-_]/g, ' ')
        .replace(/\b\w/g, c => c.toUpperCase());
      processing.push(titleCased);
    } else {
      const connectorName = (step.connector || 'Unknown').charAt(0).toUpperCase() + (step.connector || '').slice(1);
      delivery.push(connectorName);
    }
  }
  if (processing.length === 0 && delivery.length === 0) return '';
  const parts = [];
  if (processing.length) parts.push(processing.join(', '));
  if (delivery.length) parts.push(delivery.join(', '));
  return parts.join(' \u2192 ');
}

export function maybeAutoName() {
  const nameEl = document.getElementById('pipeline-editor-name');
  if (!nameEl) return;
  const currentVal = nameEl.value.trim();
  if (currentVal === '' || currentVal === pipelineState.lastAutoName) {
    const suggested = suggestPipelineName();
    nameEl.value = suggested;
    pipelineState.setLastAutoName(suggested);
  }
}
