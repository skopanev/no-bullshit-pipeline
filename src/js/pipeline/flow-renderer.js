// Shared pipeline flow renderer -- used in both builder preview and recording cards.

import { escapeHtml } from '../core/utils.js';
import { CONNECTOR_META, PROVIDER_META } from './constants.js';

const MIC_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>`;

/**
 * Render a pipeline flow as HTML chips.
 * @param {Array} steps - pipeline steps
 * @param {Object} opts - { compact: bool, statuses: { stepName: status } }
 */
export function renderPipelineFlowHTML(steps, opts = {}) {
  const { compact = false, statuses = {} } = opts;
  const arrow = `<div class="pflow-arrow">\u203a</div>`;

  let html = `<div class="pflow-chip pflow-chip--source" title="Transcript"><div class="pflow-chip-icon" style="background:var(--accent-soft);color:var(--accent);">${MIC_SVG}</div>${compact ? '' : '<span class="pflow-chip-label">Transcript</span>'}</div>`;

  for (const step of (steps || [])) {
    html += arrow;
    let meta = CONNECTOR_META[step.connector] || {
      abbr: step.connector.substring(0, 2).toUpperCase(),
      textColor: 'var(--text-primary)',
      bgColor: 'var(--bg-input)',
    };
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
