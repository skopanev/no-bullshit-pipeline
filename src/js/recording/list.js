import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { setAllRecordings } from '../core/state.js';
import { escapeHtml, formatDuration, getDuration } from '../core/utils.js';
import { emit } from '../core/events.js';
import { trace } from '../ignore-trace.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { showToast } from '../ui/toast.js';
import { allPipelineDefs } from '../pipeline/state.js';
import { renderPipelineFlowHTML } from '../pipeline/flow-renderer.js';

// Stable per-day key for grouping recordings into date sections.
function dateSectionKey(createdAt) {
  const d = new Date(createdAt);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

// Human label for a date section: "Today" / "Yesterday" / "MAY 15, 2026".
function dateSectionLabel(createdAt) {
  const d = new Date(createdAt);
  const startOfDay = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate());
  const diffDays = Math.round((startOfDay(new Date()) - startOfDay(d)) / 86400000);
  if (diffDays === 0) return 'Today';
  if (diffDays === 1) return 'Yesterday';
  return d
    .toLocaleDateString(undefined, { month: 'long', day: 'numeric', year: 'numeric' })
    .toUpperCase();
}

// key → { element, signature } — preserves DOM nodes across polls so scroll
// and selection survive when nothing (or only some nodes) changed. Keys are
// `sec:<dayKey>` for section headers and the recording id for rows.
const renderedNodes = new Map();

// Live timer for any row in `status: "recording"`. Ticks once per second
// and patches just the meta span of those rows — avoids full list re-render.
// Started/stopped by `syncActiveRecordingTimers()` based on whether any
// recording is currently in the active state.
let activeTimerInterval = null;

function formatElapsedSince(createdAt) {
  const start = new Date(createdAt).getTime();
  if (!Number.isFinite(start)) return '';
  const elapsedSec = Math.max(0, Math.floor((Date.now() - start) / 1000));
  const h = Math.floor(elapsedSec / 3600);
  const m = Math.floor((elapsedSec % 3600) / 60);
  const s = elapsedSec % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

function recordingStatusHtml(rec) {
  const elapsed = formatElapsedSince(rec.created_at);
  return `<span class="recording-text">Recording ${elapsed}</span>`;
}

function tickActiveRecordingRows() {
  for (const rec of state.allRecordings) {
    if (rec.status !== 'recording') continue;
    const row = document.querySelector(`.recording-item[data-id="${CSS.escape(rec.id)}"]`);
    if (!row) continue;
    const metaSpans = row.querySelectorAll('.recording-meta > span');
    if (metaSpans.length === 0) continue;
    metaSpans[metaSpans.length - 1].outerHTML = recordingStatusHtml(rec);
  }
}

function syncActiveRecordingTimers() {
  const anyActive = state.allRecordings.some((r) => r.status === 'recording');
  if (anyActive && activeTimerInterval == null) {
    activeTimerInterval = setInterval(tickActiveRecordingRows, 1000);
  } else if (!anyActive && activeTimerInterval != null) {
    clearInterval(activeTimerInterval);
    activeTimerInterval = null;
  }
}

let __loadSeq = 0;
export async function loadRecordings() {
  const seq = ++__loadSeq;
  try {
    const t0 = performance.now();
    const recordings = await invoke('list_recordings');
    const dt = (performance.now() - t0).toFixed(1);
    const active = (recordings || [])
      .filter(r => r.status === 'recording' || r.status === 'processing')
      .map(r => `${r.id.slice(0,8)}=${r.status}`);
    trace(`JS loadRecordings#${seq} resolved in ${dt}ms, n=${(recordings||[]).length}, active=`, active);
    setAllRecordings(recordings || []);
    renderRecordingsList();
  } catch (error) {
    trace('JS loadRecordings failed:', String(error));
  }
}

// Neutral, unobtrusive fallback shown while an icon resolves or when the
// bundle id can't be resolved to an installed app (muted rounded-square glyph).
const DEFAULT_APP_ICON_SVG =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="4"/></svg>';

// NBP's own mark for manual recordings — inline waveform so it uses
// currentColor and stays visible in both light and dark themes (the app icon
// PNG is black-on-transparent and would vanish on a dark background).
const NBP_WAVEFORM_SVG =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><line x1="4" y1="10" x2="4" y2="14"/><line x1="8.5" y1="5" x2="8.5" y2="19"/><line x1="13" y1="2.5" x2="13" y2="21.5"/><line x1="17.5" y1="7" x2="17.5" y2="17"/><line x1="21" y1="10.5" x2="21" y2="13.5"/></svg>';

// bundle_id → data URL (or null when unresolved). Avoids re-invoking the Rust
// command on every list re-render; the backend also caches on disk.
const appIconCache = new Map();

function applyResolvedIcon(el, url) {
  if (url) {
    el.innerHTML = `<img src="${url}" alt="" width="16" height="16" />`;
    el.classList.add('app-icon-resolved');
  }
}

// Fill in app icons after the rows are in the DOM. Marked rows are skipped so
// repeated renders don't re-process; rebuilt rows (new signature) come back
// without the marker and get re-hydrated from the cache (cheap, no FFI).
async function hydrateAppIcons() {
  const els = document.querySelectorAll('.app-icon[data-bundle]:not([data-hydrated])');
  for (const el of els) {
    el.dataset.hydrated = '1';
    const bundle = el.dataset.bundle;
    if (!bundle) continue;
    if (appIconCache.has(bundle)) {
      applyResolvedIcon(el, appIconCache.get(bundle));
      continue;
    }
    try {
      const url = await invoke('get_app_icon', { bundleId: bundle });
      appIconCache.set(bundle, url || null);
      applyResolvedIcon(el, url || null);
    } catch {
      appIconCache.set(bundle, null);
    }
  }
}

function buildRowHtml(rec) {
  const isProcessing = rec.status === 'processing';
  const isRecordingStatus = rec.status === 'recording';
  const isCurrentlyRecording = state.isRecording && state.selectedRecordingId === rec.id;
  const isTranscribing = state.transcribingIds && state.transcribingIds.has(rec.id);

  // Status text takes priority over duration when an action is in flight.
  // Order: recording > processing > transcribing > duration (idle).
  let metaText;
  if (isRecordingStatus) {
    metaText = recordingStatusHtml(rec);
  } else if (isProcessing) {
    metaText = '<span style="color:var(--accent)">Processing…</span>';
  } else if (isTranscribing) {
    metaText = '<span style="color:var(--accent)">Transcribing…</span>';
  } else {
    metaText = formatDuration(getDuration(rec));
  }

  // Transcript preview — populated by `transcription` after the run lands.
  // Tiny 1-3 line snippet so the user can tell what the meeting was about
  // without opening the recording. Falls back to absent for un-transcribed
  // or empty-transcript recordings.
  const previewHtml = rec.transcript_preview
    ? `<div class="recording-preview">${escapeHtml(rec.transcript_preview)}</div>`
    : '';

  const hasIssues = rec.health && rec.health.status !== 'ok';
  const healthIcon = hasIssues
    ? '<span class="health-warning" title="Issues occurred during recording"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--warning, #f59e0b)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg></span>'
    : '';

  const safeTitle = escapeHtml(rec.title || 'Untitled');
  const safeId = escapeHtml(rec.id);

  // App icon: call recordings resolve their app's icon async from the bundle
  // id; manual recordings (New Record / pipeline) have no bundle id and show
  // our own app icon instead.
  const appIconHtml = rec.app_bundle_id
    ? `<span class="app-icon" data-bundle="${escapeHtml(rec.app_bundle_id)}">${DEFAULT_APP_ICON_SVG}</span>`
    : `<span class="app-icon app-icon-resolved">${NBP_WAVEFORM_SVG}</span>`;

  // Pipeline run history with step chips
  const pipelineRuns = (rec.pipelines || []).map(p => {
    const statusClass = p.status === 'Done' ? 'tag-done' : p.status === 'Partial' ? 'tag-partial' : p.status === 'Running' ? 'tag-running' : 'tag-waiting';
    const def = allPipelineDefs ? allPipelineDefs.find(d => d.name === p.name) : null;
    const flowHtml = def && def.steps && def.steps.length > 0
      ? renderPipelineFlowHTML(def.steps, { compact: true })
      : '';
    return `<div class="recording-pipeline-entry ${statusClass}"><span class="recording-pipeline-name">${escapeHtml(p.name)}</span>${flowHtml}</div>`;
  }).join('');

  const deleteDisabled = isCurrentlyRecording || isProcessing;
  const deleteBtnHtml = deleteDisabled
    ? ''
    : `<button class="recording-item-delete" data-id="${safeId}" title="Delete recording"><span class="icon-trash"></span></button>`;

  // Copy-transcript shown only when there's a transcript to copy (preview is
  // the cheapest signal). Click fetches the full text via get_transcript so we
  // copy what the user would see in the detail view, not just the snippet.
  const copyBtnHtml = rec.transcript_preview
    ? `<button class="recording-item-copy" data-id="${safeId}" title="Copy transcript"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path></svg></button>`
    : '';

  // Run-pipeline button — only when there's a transcript to feed into one
  // and at least one pipeline defined. Click opens a small dropdown listing
  // every saved pipeline; pick one to launch it on this recording without
  // navigating to the detail view.
  const canRunPipeline = !!rec.transcript_preview && (allPipelineDefs || []).length > 0;
  const runPipelineBtnHtml = canRunPipeline
    ? `<button class="recording-item-run-pipeline" data-id="${safeId}" title="Run pipeline"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg></button>`
    : '';

  return `<div class="recording-item ${isCurrentlyRecording ? 'recording-active' : ''}" data-id="${safeId}" onclick="showDetailView(this.dataset.id)">
        <div class="recording-item-header">
          <div class="recording-title">${healthIcon}${appIconHtml}${safeTitle}${isCurrentlyRecording ? ' <span style="color:var(--accent)">●</span>' : ''}</div>
          <div class="recording-meta">
            <span>${metaText}</span>
          </div>
        </div>
        ${previewHtml}
        ${pipelineRuns ? `<div class="recording-pipeline-runs">${pipelineRuns}</div>` : ''}
        ${copyBtnHtml}
        ${runPipelineBtnHtml}
        ${deleteBtnHtml}
      </div>`;
}

function htmlToElement(html) {
  const tmp = document.createElement('div');
  tmp.innerHTML = html.trim();
  return tmp.firstElementChild;
}

function wireRowCopy(rowEl) {
  const btn = rowEl.querySelector('.recording-item-copy');
  if (!btn) return;
  btn.addEventListener('click', async (e) => {
    e.stopPropagation(); // don't open the detail view on copy click
    const recordingId = btn.dataset.id;
    try {
      const text = await invoke('get_transcript', { recordingId });
      const trimmed = (text || '').trim();
      if (!trimmed) {
        showToast('No transcript to copy', 'info');
        return;
      }
      await navigator.clipboard.writeText(trimmed);
      showToast('Transcript copied', 'success');
    } catch (err) {
      console.error('copy transcript failed:', err);
      showToast('Copy failed', 'error');
    }
  });
}

// Fetch per-step outputs after a pipeline finishes and surface a meaningful
// summary toast. The Tauri command returns "done" / "partial" / other; we
// translate that into a coloured toast and append the last step's first
// content line as a headline (e.g. a summary's first sentence, or a shell
// step's first stdout line).
async function reportPipelineOutcome(recordingId, pipelineName, result) {
  let steps = [];
  try {
    steps = await invoke('get_step_outputs', { recordingId, pipelineName }) || [];
  } catch (e) {
    // Non-fatal — we still report the high-level result below.
    console.warn('get_step_outputs failed:', e);
  }

  const lastWithOutput = [...steps].reverse().find(s => s.output || s.error);
  const headline = lastWithOutput
    ? firstNonEmptyLine(lastWithOutput.error || lastWithOutput.output)
    : '';
  const failedStep = steps.find(s => s.status === 'failed' || s.status === 'error');

  if (result === 'done') {
    const msg = headline
      ? `✓ "${pipelineName}" done — ${truncate(headline, 90)}`
      : `✓ "${pipelineName}" completed`;
    showToast(msg, 'success', 6000);
  } else if (result === 'partial') {
    const msg = failedStep
      ? `⚠ "${pipelineName}" partial — step "${failedStep.name}" failed${failedStep.error ? ': ' + truncate(failedStep.error, 80) : ''}`
      : `⚠ "${pipelineName}" partially completed — open recording for details`;
    showToast(msg, 'warning', 8000);
  } else {
    // Engine returned some other status (in_progress? not_started?). Surface
    // verbatim — should be rare.
    showToast(`"${pipelineName}": ${result}`, 'info', 6000);
  }
}

function firstNonEmptyLine(text) {
  if (!text) return '';
  for (const raw of String(text).split('\n')) {
    const line = raw.trim();
    if (line) return line;
  }
  return '';
}

function truncate(s, max) {
  if (!s) return '';
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
}

function wireRowRunPipeline(rowEl) {
  const btn = rowEl.querySelector('.recording-item-run-pipeline');
  if (!btn) return;
  btn.addEventListener('click', (e) => {
    e.stopPropagation(); // don't open the detail view
    openRunPipelineDropdown(btn);
  });
}

// Single global dropdown anchored to the clicked button. Click outside or
// pressing Esc closes it. Re-clicking the same button toggles.
function openRunPipelineDropdown(anchorBtn) {
  // Close any existing dropdown first — clicking another button replaces.
  const existing = document.querySelector('.run-pipeline-dropdown');
  if (existing) {
    const wasFor = existing.dataset.anchorId;
    existing.remove();
    if (wasFor === anchorBtn.dataset.id) return; // toggle off
  }

  const recordingId = anchorBtn.dataset.id;
  const pipelines = allPipelineDefs || [];
  if (pipelines.length === 0) {
    showToast('No pipelines defined — create one in Settings → Pipelines', 'info');
    return;
  }

  const items = pipelines.map(p => {
    const stepCount = (p.steps || []).length;
    const stepsLabel = stepCount === 1 ? '1 step' : `${stepCount} steps`;
    return `<button class="run-pipeline-item" data-name="${escapeHtml(p.name)}" role="menuitem">
      <span class="run-pipeline-item-name">${escapeHtml(p.name)}</span>
      <span class="run-pipeline-item-meta">${stepsLabel}</span>
    </button>`;
  }).join('');

  const dropdown = document.createElement('div');
  dropdown.className = 'run-pipeline-dropdown';
  dropdown.dataset.anchorId = recordingId;
  dropdown.setAttribute('role', 'menu');
  dropdown.innerHTML = `
    <div class="run-pipeline-header">Run Pipeline</div>
    <div class="run-pipeline-items">${items}</div>
  `;

  // Position right below the button. Append to body so it can escape the
  // recording-item's overflow/relative bounds.
  document.body.appendChild(dropdown);
  const rect = anchorBtn.getBoundingClientRect();
  dropdown.style.position = 'fixed';
  dropdown.style.top = `${rect.bottom + 4}px`;
  // Right-align to button so the menu opens leftward into the visible area
  // (the button is near the right edge of the row).
  dropdown.style.right = `${window.innerWidth - rect.right}px`;
  dropdown.style.zIndex = '10000';

  // Wire item clicks.
  dropdown.querySelectorAll('.run-pipeline-item').forEach(item => {
    item.addEventListener('click', async (e) => {
      e.stopPropagation();
      const pipelineName = item.dataset.name;
      dropdown.remove();
      // Long timeout on the "running" toast so it sticks around for the whole
      // run (auto-dismiss would lie about the state). The result toast fires
      // right after the await resolves and replaces the user's mental model.
      showToast(`Running "${pipelineName}"…`, 'info', 60_000);
      try {
        const result = await invoke('execute_pipeline', { recordingId, pipelineName });
        await loadRecordings();
        await reportPipelineOutcome(recordingId, pipelineName, result);
      } catch (err) {
        console.error('execute_pipeline failed:', err);
        showToast(`✗ "${pipelineName}" failed: ${err}`, 'error', 8000);
      }
    });
  });

  // Close on outside click / Esc — defer to next tick so the click that
  // opened us doesn't immediately close us.
  setTimeout(() => {
    const closeOnOutside = (e) => {
      if (!dropdown.contains(e.target) && e.target !== anchorBtn) {
        dropdown.remove();
        document.removeEventListener('click', closeOnOutside);
        document.removeEventListener('keydown', closeOnEsc);
      }
    };
    const closeOnEsc = (e) => {
      if (e.key === 'Escape') {
        dropdown.remove();
        document.removeEventListener('click', closeOnOutside);
        document.removeEventListener('keydown', closeOnEsc);
      }
    };
    document.addEventListener('click', closeOnOutside);
    document.addEventListener('keydown', closeOnEsc);
  }, 0);
}

function wireRowDelete(rowEl) {
  const btn = rowEl.querySelector('.recording-item-delete');
  if (!btn) return;
  btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    const recordingId = btn.dataset.id;
    const ok = await showConfirm('Delete Recording?', 'This action cannot be undone.');
    if (!ok) return;
    try {
      await invoke('delete_recording', { recordingId });
      if (state.selectedRecordingId === recordingId) emit('recording:hideDetail');
      await loadRecordings();
    } catch (err) {
      console.error('Delete failed:', err);
      if (err && typeof err === 'string' && err.includes('finalized')) {
        showToast('Recording is still being finalized. Please wait a moment and try again.', 'info');
      } else {
        showToast('Delete failed: ' + err, 'error');
      }
    }
  });
}

export function renderRecordingsList() {
  const recordingsListEl = document.getElementById('recordings-list');
  const emptyStateEl = document.getElementById('empty-state');
  if (!recordingsListEl) return;

  if (state.allRecordings.length === 0) {
    if (renderedNodes.size > 0 || recordingsListEl.firstChild) {
      recordingsListEl.innerHTML = '';
      renderedNodes.clear();
    }
    if (emptyStateEl) emptyStateEl.style.display = 'block';
    return;
  }
  if (emptyStateEl) emptyStateEl.style.display = 'none';

  // Build the ordered node list: a date-section header before each new day,
  // then the rows for that day. `allRecordings` is already sorted newest-first
  // so days fall out in order.
  const nodes = [];
  let lastSectionKey = null;
  for (const rec of state.allRecordings) {
    const secKey = dateSectionKey(rec.created_at);
    if (secKey !== lastSectionKey) {
      lastSectionKey = secKey;
      nodes.push({
        key: `sec:${secKey}`,
        html: `<div class="recordings-section-header">${escapeHtml(dateSectionLabel(rec.created_at))}</div>`,
        isRow: false,
      });
    }
    nodes.push({ key: rec.id, html: buildRowHtml(rec), isRow: true });
  }

  // Remove nodes no longer present (rows and now-empty section headers).
  const currentKeys = new Set(nodes.map((n) => n.key));
  for (const [key, info] of renderedNodes) {
    if (!currentKeys.has(key)) {
      info.element.remove();
      renderedNodes.delete(key);
    }
  }

  // Walk nodes in order; insert/update/move per-node as needed.
  let prevNode = null;
  for (const node of nodes) {
    let info = renderedNodes.get(node.key);
    if (info) {
      if (info.signature !== node.html) {
        const newEl = htmlToElement(node.html);
        info.element.replaceWith(newEl);
        if (node.isRow) { wireRowDelete(newEl); wireRowCopy(newEl); wireRowRunPipeline(newEl); }
        info.element = newEl;
        info.signature = node.html;
      }
    } else {
      const newEl = htmlToElement(node.html);
      info = { element: newEl, signature: node.html };
      renderedNodes.set(node.key, info);
      if (node.isRow) { wireRowDelete(newEl); wireRowCopy(newEl); wireRowRunPipeline(newEl); }
    }

    const expectedAtPos = prevNode ? prevNode.nextSibling : recordingsListEl.firstChild;
    if (info.element !== expectedAtPos) {
      recordingsListEl.insertBefore(info.element, expectedAtPos);
    }
    prevNode = info.element;
  }

  // Drive the per-second tick for any in-progress recording rows so the
  // "Recording 0:42" stays current. Started here (not on every loadRecordings
  // call) so it reflects whatever's in state.allRecordings right now and
  // self-stops when the last active recording finalizes.
  syncActiveRecordingTimers();

  // Resolve + swap in real app icons once rows are in the DOM.
  hydrateAppIcons();
}
