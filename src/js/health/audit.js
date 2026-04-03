import { escapeHtml } from '../core/utils.js';

const AUDIT_ELEMENTS = [
  { id: 'record-toggle-btn',         desc: 'Record button' },
  { id: 'pipeline-chip-bar',         desc: 'Pipeline chip bar container' },
  { id: 'status-indicator',          desc: 'Status indicator dot' },
  { id: 'timer',                     desc: 'Timer display' },
  { id: 'permission-warning',        desc: 'Permission warning banner' },
  { id: 'settings-btn',              desc: 'Settings button' },
  { id: 'settings-view',             desc: 'Settings view section' },
  { id: 'settings-tabs',             desc: 'Settings tab bar' },
  { id: 'save-settings-btn',         desc: 'Save Settings button' },
  { id: 'settings-back-btn',         desc: 'Settings back button' },
  { id: 'settings-transcription-enabled', desc: 'Auto-transcribe toggle' },
  { id: 'settings-storage-path',     desc: 'Storage path input' },
  { id: 'browse-storage-btn',        desc: 'Browse storage button' },
  { id: 'pipeline-defs-list',        desc: 'Pipeline definitions list' },
  { id: 'add-pipeline-def-btn',      desc: 'Add pipeline button' },
  { id: 'pipeline-editor',           desc: 'Pipeline editor (hidden)' },
  { id: 'prompt-templates-list',     desc: 'Prompt templates list' },
  { id: 'add-prompt-template-btn',   desc: 'Add template button' },
  { id: 'connected-integrations-list',  desc: 'Connected integrations container' },
  { id: 'available-integrations-list',  desc: 'Available integrations container' },
  { id: 'theme-purple-btn',          desc: 'Neon Purple theme button' },
  { id: 'theme-blue-btn',            desc: 'Deep Blue theme button' },
  { id: 'theme-light-btn',           desc: 'Light theme button' },
  { id: 'detail-view',               desc: 'Detail view section' },
  { id: 'back-btn',                  desc: 'Back button in detail view' },
  { id: 'detail-title',              desc: 'Recording title input' },
  { id: 'process-btn',               desc: 'Transcribe button' },
  { id: 'pipeline-cards',            desc: 'Pipeline cards container' },
  { id: 'delete-modal',              desc: 'Delete confirmation modal' },
  { id: 'add-slack-modal',           desc: 'Add Slack workspace modal' },
  { id: 'notion-wizard-modal',       desc: 'Notion setup wizard modal' },
  { id: 'onboarding-overlay',        desc: 'Onboarding overlay' },
];

export function runHealthAudit() {
  const issues = [];
  for (const spec of AUDIT_ELEMENTS) {
    if (!document.getElementById(spec.id)) {
      issues.push({
        element: spec.id,
        description: spec.desc + ' is missing from the DOM',
        fix: 'Check index.html for element with id="' + spec.id + '"'
      });
    }
  }
  const result = { passed: AUDIT_ELEMENTS.length - issues.length, failed: issues.length, issues };
  window._lastHealthResult = result;
  return result;
}

export function renderHealthBadge(result) {
  const badge = document.getElementById('health-badge');
  if (!badge) return;

  if (result.failed === 0) {
    badge.className = 'health-badge health-badge-ok';
    badge.textContent = '\u2713';
    badge.title = 'UI health: all ' + result.passed + ' elements verified';
    badge.style.cursor = 'default';
    badge.onclick = null;
  } else {
    badge.className = 'health-badge health-badge-fail';
    badge.textContent = '\u26a0 ' + result.failed;
    badge.title = result.failed + ' element(s) failed — click for report';
    badge.style.cursor = 'pointer';
    badge.onclick = () => showHealthReport(result.issues);
  }
  badge.style.display = '';
}

export function showHealthReport(issues) {
  const modal = document.getElementById('health-report-modal');
  const body = document.getElementById('health-report-body');
  if (!modal || !body) return;

  body.innerHTML = issues.map(issue =>
    '<div class="health-issue-row">' +
    '<div class="health-issue-id">' + escapeHtml(issue.element) + '</div>' +
    '<div class="health-issue-desc">' + escapeHtml(issue.description) + '</div>' +
    '<div class="health-issue-fix">Fix: ' + escapeHtml(issue.fix) + '</div>' +
    '</div>'
  ).join('');

  modal.style.display = 'flex';
}
