import { escapeHtml } from '../core/utils.js';

// No detail/delete actions: the actual recording is published atomically only
// after decoding finishes. The filename is still visible throughout the wait.
export function buildImportRow(recording) {
  const label = recording.status === 'import_queued' ? 'Queued for import…' : 'Importing…';
  return `<div class="recording-item recording-item-importing" data-id="${escapeHtml(recording.id)}" role="status" aria-busy="true">
    <div class="recording-item-header">
      <div class="recording-title">${escapeHtml(recording.title)}</div>
      <div class="recording-meta"><span><span class="btn-spinner" aria-hidden="true"></span>${label}</span></div>
    </div>
  </div>`;
}
