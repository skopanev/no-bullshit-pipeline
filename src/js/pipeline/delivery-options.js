// Auto-naming logic for pipeline editor

import * as pipelineState from './state.js';

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
