import { runHealthAudit, renderHealthBadge } from './audit.js';
import { initWalkthrough, startWalkthrough } from './walkthrough.js';

export { runHealthAudit, startWalkthrough };

export function initHealthCheck() {
  // Wire modal buttons
  const closeBtn = document.getElementById('health-report-close-btn');
  if (closeBtn) closeBtn.addEventListener('click', () => {
    const modal = document.getElementById('health-report-modal');
    if (modal) modal.style.display = 'none';
  });

  const rerunBtn = document.getElementById('health-report-rerun-btn');
  if (rerunBtn) rerunBtn.addEventListener('click', () => {
    const result = runHealthAudit();
    renderHealthBadge(result);
    if (result.failed === 0) {
      const modal = document.getElementById('health-report-modal');
      if (modal) modal.style.display = 'none';
    }
  });

  initWalkthrough();
}

export function scheduleAudit(appSettings) {
  const doAudit = () => {
    const result = runHealthAudit();
    renderHealthBadge(result);
    if (appSettings && appSettings.onboarding_completed && !appSettings.walkthrough_completed) {
      startWalkthrough();
    }
  };
  if (typeof requestIdleCallback !== 'undefined') {
    requestIdleCallback(doAudit, { timeout: 2000 });
  } else {
    setTimeout(doAudit, 500);
  }
}
