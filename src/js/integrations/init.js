// integrations/init.js — Initialization and MutationObserver setup

import { loadAllIntegrations } from './load.js';
import { cancelFreshnessCheck } from './providers.js';
import { setupLocalModelListeners } from './local-models.js';

export function initIntegrationsSettings() {
  const observer = new MutationObserver(() => {
    const modelsTab = document.querySelector('.settings-tab-content[data-tab="models"]');
    const intTab = document.querySelector('.settings-tab-content[data-tab="integrations"]');
    if ((modelsTab && modelsTab.classList.contains('active')) ||
        (intTab && intTab.classList.contains('active'))) {
      loadAllIntegrations();
    }
    if (modelsTab && modelsTab.classList.contains('active')) {
      loadAllIntegrations();
    }
  });

  const modelsTab = document.querySelector('.settings-tab-content[data-tab="models"]');
  const intTab = document.querySelector('.settings-tab-content[data-tab="integrations"]');
  if (modelsTab) observer.observe(modelsTab, { attributes: true, attributeFilter: ['class'] });
  if (intTab) observer.observe(intTab, { attributes: true, attributeFilter: ['class'] });

  // Cancel freshness check when navigating away from settings
  new MutationObserver(() => {
    if (!document.body.classList.contains('settings-open')) {
      cancelFreshnessCheck();
    }
  }).observe(document.body, { attributes: true, attributeFilter: ['class'] });

  // Set up Tauri event listeners for download progress / freshness
  setupLocalModelListeners();
}

// Re-export public API for external consumers
export { loadAllIntegrations } from './load.js';
export { renderConnectedIntegrations } from './connected.js';
export { renderAvailableIntegrations } from './available.js';
export { renderModelsProviders } from './providers.js';
export { renderLocalLlmModels } from './local-models.js';
export { openNotionWizard } from './notion-wizard.js';
export { openLinearWizard } from './linear-wizard.js';
