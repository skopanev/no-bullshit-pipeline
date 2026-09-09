import { invoke, listen } from '../core/tauri.js';
import * as state from '../core/state.js';
import { setSelectedRecordingId } from '../core/state.js';
import { formatDuration, getDuration, escapeHtml } from '../core/utils.js';
import { applyMarkdownRendering } from '../ui/markdown.js';
import { showToast } from '../ui/toast.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { ViewManager } from '../ui/view-manager.js';
import { on } from '../core/events.js';
import { updateMainButton } from './controls.js';
import { loadRecordings } from './list.js';
import { subscribeToProgress, renderPipelineStatus, cleanupPipelineProgress } from './pipeline-status.js';
import { commitSpeakerName, ensureNamesDatalist } from '../settings/people.js';
import * as pipelineState from '../pipeline/state.js';

const dateOptions = {
  month: 'short',
  day: 'numeric',
  year: 'numeric',
  hour: 'numeric',
  minute: '2-digit'
};

// Transcription timer state
let transcriptionElapsedTimer = null;
let transcriptionStartTime = null;
let transcriptionCurrentStage = '';

// Processing-poll state (single active poller per detail view)
let processingPollIntervalId = null;
let processingPollGeneration = 0;

// Artifact workspace state. The raw transcript is always the primary source;
// speaker view and pipeline results are optional sibling artifacts.
let detailRecordingId = null;
let activeArtifact = 'transcript';
let transcriptCopyText = '';
let speakerArtifact = null;
let pipelineArtifacts = [];
let artifactRefreshGeneration = 0;

// Compact detail-player state.
let playbackPollInterval = null;
let detailAudioDurationMs = 0;
let detailPlaybackRecordingId = null;
let detailPlaybackOwnsAudio = false;

function stopProcessingPoll() {
  processingPollGeneration++;
  if (processingPollIntervalId !== null) {
    clearInterval(processingPollIntervalId);
    processingPollIntervalId = null;
  }
}

export function clearTranscriptionTimer() {
  if (transcriptionElapsedTimer) {
    clearInterval(transcriptionElapsedTimer);
    transcriptionElapsedTimer = null;
  }
  transcriptionStartTime = null;
  transcriptionCurrentStage = '';
}

export function hideDetailView() {
  setSelectedRecordingId(null);
  detailRecordingId = null;
  artifactRefreshGeneration++;
  stopProcessingPoll();
  stopDetailPlayback();
  closeDetailMenus();
  updateMainButton();
  ViewManager.showRecordings();
  cleanupPipelineProgress();
}

// Calendar match row under the title: matched event → attendee chips +
// re-match / clear; unmatched (with calendar access) → a one-tap match button.
function renderCalendarRow(rec) {
  const row = document.getElementById('detail-calendar-row');
  if (!row) return;

  const attendees = Array.isArray(rec.attendees) ? rec.attendees : [];
  // A matched recording always shows the matched header — even with no
  // attendees / id (room-only events). Empty attendees render no chips below.
  const matched = !!rec.calendar_matched;
  const calGranted = !!state.permissions?.calendar;

  if (matched) {
    const chips = attendees.map((a) => {
      const label = a.name || a.email || 'Unknown';
      const tip = a.name && a.email ? a.email : '';
      return `<span class="attendee-chip" title="${escapeHtml(tip)}">${escapeHtml(label)}</span>`;
    }).join('');
    const n = attendees.length;
    row.innerHTML = `
      <div class="cal-row-head">
        <span class="cal-badge">📅 Calendar</span>
        ${n ? `<span class="cal-count">${n} attendee${n === 1 ? '' : 's'}</span>` : ''}
        <button class="cal-mini-btn js-cal-rematch" type="button">Re-match</button>
        <button class="cal-mini-btn danger js-cal-clear" type="button">Clear</button>
      </div>
      ${chips ? `<div class="attendee-chips">${chips}</div>` : ''}
    `;
    row.style.display = '';
  } else if (calGranted && rec.source !== 'dictation') {
    row.innerHTML = `<button class="cal-mini-btn js-cal-rematch" type="button">📅 Match to calendar event</button>`;
    row.style.display = '';
  } else {
    row.innerHTML = '';
    row.style.display = 'none';
    return;
  }

  const rematchBtn = row.querySelector('.js-cal-rematch');
  if (rematchBtn) rematchBtn.addEventListener('click', () => calendarAction('rematch_calendar', rec.id, rematchBtn));
  const clearBtn = row.querySelector('.js-cal-clear');
  if (clearBtn) clearBtn.addEventListener('click', () => calendarAction('clear_calendar_match', rec.id, clearBtn));
}

async function calendarAction(cmd, id, btn) {
  const prev = btn ? btn.textContent : '';
  if (btn) { btn.disabled = true; btn.textContent = '…'; }
  try {
    await invoke(cmd, { recordingId: id });
    await loadRecordings();
    if (state.selectedRecordingId === id) showDetailView(id);
  } catch (err) {
    if (btn) { btn.disabled = false; btn.textContent = prev; }
    showToast(typeof err === 'string' ? err : 'Calendar action failed', 'warning');
  }
}

// --- Diarization (secondary speaker view) -----------------------------------

function renderDiarProgress(stage, percent) {
  const statusEl = document.getElementById('speaker-inline-status');
  if (!statusEl) return;
  statusEl.style.display = '';
  statusEl.innerHTML = `
    <span>Identifying speakers · ${escapeHtml(stage || 'Working')}</span>
    <div class="diar-progress"><div class="diar-progress-fill" style="width:${Math.min(100, percent || 0)}%"></div></div>
    <span class="diar-progress-pct">${percent || 0}%</span>`;
}

let diarListenerStarted = false;
/// One global subscription: live progress + auto-refresh when a job finishes,
/// even if the user navigated away and back mid-run.
function ensureDiarListener() {
  if (diarListenerStarted) return;
  diarListenerStarted = true;
  listen('diarization_progress', (event) => {
    const p = event.payload || {};
    if (p.recording_id !== state.selectedRecordingId) return;
    if (p.status === 'running') {
      renderDiarProgress(p.stage, p.percent);
    } else {
      const rec = state.allRecordings.find(r => r.id === p.recording_id);
      if (rec) renderDiarization(rec);
    }
  });
}

/// Inline rename: swap the speaker chip for an input (window.prompt is
/// unreliable in WKWebView). Enter/blur saves, Escape cancels. Known people
/// are suggested via the shared datalist; picking one is an explicit merge.
function startSpeakerRename(el, rec, spk, current) {
  invoke('list_speaker_profiles').then(p => ensureNamesDatalist(p || [])).catch(() => {});
  const input = document.createElement('input');
  input.type = 'text';
  input.value = current;
  input.className = 'diar-rename-input';
  input.placeholder = `Speaker ${spk + 1}`;
  input.setAttribute('list', 'people-names');
  el.replaceWith(input);
  input.focus();
  input.select();
  let done = false;
  const finish = async (save) => {
    if (done) return;
    done = true;
    if (save) {
      try {
        const name = input.value.trim();
        if (name) await commitSpeakerName(rec.id, spk, name);
        else await invoke('rename_speaker', { recordingId: rec.id, speaker: spk, name: '' });
      } catch (e) {
        showToast('Rename failed: ' + e, 'error');
      }
    }
    renderDiarization(rec);
  };
  input.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') finish(true);
    else if (ev.key === 'Escape') finish(false);
  });
  input.addEventListener('blur', () => finish(true));
}

async function renderDiarization(rec) {
  ensureDiarListener();
  const statusEl = document.getElementById('speaker-inline-status');

  let status = null;
  let diar = null;
  let profiles = [];
  try { status = await invoke('get_diarization_status', { recordingId: rec.id }); } catch { status = null; }
  try { diar = await invoke('get_diarization', { recordingId: rec.id }); } catch { diar = null; }
  try { profiles = (await invoke('list_speaker_profiles')) || []; } catch { profiles = []; }
  // The user may have switched recordings while we awaited — don't paint stale.
  if (state.selectedRecordingId !== rec.id) return;

  // Names live in the global profiles (uid OR merged alias → person): naming
  // someone anywhere labels every recording instantly at render time.
  const nameByUid = {};
  for (const p of profiles) {
    if (!p.name) continue;
    nameByUid[p.uid] = p.name;
    for (const a of p.aliases || []) nameByUid[a] = p.name;
  }

  const running = !!(status && status.status === 'running');

  if (running) {
    renderDiarProgress(status.stage, status.percent);
  } else if (statusEl) {
    statusEl.style.display = 'none';
    statusEl.innerHTML = '';
  }

  if (!diar || !diar.segments || diar.segments.length === 0) {
    speakerArtifact = null;
    if (!running && status && status.status === 'failed' && status.error && statusEl) {
      statusEl.style.display = '';
      statusEl.innerHTML = `
        <span>Speakers could not be identified: ${escapeHtml(status.error)}</span>
        <button class="speaker-retry-btn" type="button">Retry</button>`;
      statusEl.querySelector('.speaker-retry-btn')?.addEventListener('click', () => startDiarization(rec));
    }
    renderArtifactTabs();
    return;
  }

  // Identity per local speaker: different cluster ids can be the SAME person
  // (same uid/name) — render by identity so one person keeps one color and
  // merges into one flow instead of looking like two speakers.
  const ident = {};
  (diar.speakers || []).forEach(s => {
    const uid = s.uid || `local-${s.local_id}`;
    ident[s.local_id] = { uid, name: nameByUid[uid] || s.name || '', isMe: !!s.is_me };
  });
  const idKey = (spk) => {
    const i = ident[spk];
    if (!i) return `local-${spk}`;
    return i.name ? `name:${i.name.toLowerCase()}` : i.uid;
  };
  const label = (spk) => {
    const i = ident[spk];
    return (i && i.name) || (i && i.isMe ? 'You' : `Speaker ${spk + 1}`);
  };
  const fmt = (t) => { t = Math.floor(t); return `${String(Math.floor(t / 60)).padStart(2, '0')}:${String(t % 60).padStart(2, '0')}`; };

  // Coalesce consecutive same-person segments into one block (keep the first
  // timestamp) — but only across small gaps, so real pauses (>2s) still break
  // a long monologue instead of collapsing into a wall of text.
  const kept = diar.segments.filter(s => /[\p{L}\p{N}]/u.test(s.text || ''));
  const blocks = [];
  for (const s of kept) {
    const last = blocks[blocks.length - 1];
    if (last && idKey(last.speaker) === idKey(s.speaker) && s.start - last.end <= 2.0) {
      last.text += ' ' + (s.text || '').trim();
      last.end = Math.max(last.end, s.end);
    } else {
      blocks.push({ speaker: s.speaker, start: s.start, end: s.end, text: (s.text || '').trim() });
    }
  }
  // Second-level grouping: consecutive blocks of the SAME person (any gap)
  // share one header — the name isn't repeated for every pause, paragraphs
  // inside carry their own quiet timecodes.
  const groups = [];
  for (const b of blocks) {
    const last = groups[groups.length - 1];
    if (last && idKey(last.speaker) === idKey(b.speaker)) {
      last.paras.push(b);
    } else {
      groups.push({ speaker: b.speaker, start: b.start, paras: [b] });
    }
  }

  // Stable color per PERSON (hash of the identity key) — the same person
  // keeps one color even across different cluster ids.
  const colorClass = (spk) => {
    const i = ident[spk];
    if (i && i.isMe) return 'diar-me';
    const k = idKey(spk);
    let h = 0;
    for (let c = 0; c < k.length; c++) h = (h * 31 + k.charCodeAt(c)) >>> 0;
    return `diar-c${h % 6}`;
  };
  const rows = groups
    .map(g => {
      const paras = g.paras
        .map((p, i) => `<div class="diar-para">${i === 0 ? '' : `<span class="diar-ptime">${fmt(p.start)}</span>`}${escapeHtml(p.text)}</div>`)
        .join('');
      return `<div class="diar-seg ${colorClass(g.speaker)}"><div class="diar-head"><span class="diar-spk" data-spk="${g.speaker}" title="Click to rename">${escapeHtml(label(g.speaker))}</span><span class="diar-time">${fmt(g.start)}</span></div>${paras}</div>`;
    })
    .join('');

  if (!rows) {
    speakerArtifact = null;
    renderArtifactTabs();
    return;
  }

  const copyText = groups
    .map(g => `${label(g.speaker)}:\n${g.paras.map(p => p.text).join('\n\n')}`)
    .join('\n\n');

  speakerArtifact = {
    key: 'speakers',
    label: 'Speakers',
    html: rows,
    copyText,
    wire(container) {
      container.querySelectorAll('.diar-spk').forEach(el => {
        el.addEventListener('click', () => {
          const spk = parseInt(el.dataset.spk, 10);
          startSpeakerRename(el, rec, spk, (ident[spk] && ident[spk].name) || '');
        });
      });
    },
  };
  renderArtifactTabs();
}

async function startDiarization(rec) {
  if (!rec || rec.status === 'processing') return;
  renderDiarProgress('Starting', 0);
  try {
    await invoke('diarize_recording', { recordingId: rec.id });
  } catch (e) {
    showToast('Speaker identification failed: ' + e, 'error');
  }
  if (state.selectedRecordingId === rec.id) renderDiarization(rec);
}

function artifactKeyForPipeline(name) {
  return `pipeline:${name}`;
}

function setCopyAction(text, label = 'Copy transcript') {
  const btn = document.getElementById('copy-transcript-btn-header');
  const labelEl = document.getElementById('copy-artifact-label');
  if (!btn || !labelEl) return;
  btn.disabled = !text;
  btn.dataset.copyText = text || '';
  btn.dataset.defaultLabel = label;
  labelEl.textContent = label;
  btn.title = label;
}

function renderActiveArtifact() {
  const transcriptSection = document.getElementById('transcript-section');
  const resultSection = document.getElementById('result-section');
  const resultContent = document.getElementById('result-content');
  if (!transcriptSection || !resultSection || !resultContent) return;

  if (activeArtifact === 'transcript') {
    transcriptSection.style.display = '';
    resultSection.style.display = 'none';
    setCopyAction(transcriptCopyText, 'Copy transcript');
    return;
  }

  transcriptSection.style.display = 'none';
  resultSection.style.display = '';
  resultContent.className = 'artifact-content';

  if (activeArtifact === 'speakers' && speakerArtifact) {
    resultContent.innerHTML = speakerArtifact.html;
    speakerArtifact.wire(resultContent);
    setCopyAction(speakerArtifact.copyText, 'Copy speaker view');
    return;
  }

  const artifact = pipelineArtifacts.find(item => item.key === activeArtifact);
  if (!artifact) {
    activeArtifact = 'transcript';
    renderArtifactTabs();
    return;
  }

  if (artifact.status === 'running' || artifact.status === 'waiting') {
    resultContent.className = 'artifact-content empty';
    resultContent.innerHTML = `
      <div class="artifact-result-state">
        <span class="btn-spinner"></span>
        <span>${artifact.status === 'waiting' ? 'Waiting to run' : 'Creating result…'}</span>
      </div>`;
    setCopyAction('', 'Copy result');
  } else if (!artifact.loaded) {
    resultContent.className = 'artifact-content empty';
    resultContent.innerHTML = `
      <div class="artifact-result-state">
        <span class="btn-spinner"></span>
        <span>Loading result…</span>
      </div>`;
    setCopyAction('', 'Copy result');
    loadPipelineArtifact(detailRecordingId, artifact);
  } else if (artifact.output) {
    applyMarkdownRendering(resultContent, artifact.output);
    setCopyAction(artifact.output, 'Copy result');
  } else {
    resultContent.className = 'artifact-content empty';
    resultContent.innerHTML = `
      <div class="artifact-result-state">
        <strong>This result could not be created.</strong>
        ${artifact.error ? `<span>${escapeHtml(artifact.error)}</span>` : ''}
        <button class="detail-secondary-action artifact-retry-action" type="button">Run again</button>
      </div>`;
    resultContent.querySelector('.artifact-retry-action')?.addEventListener('click', () => {
      const rec = state.allRecordings.find(r => r.id === state.selectedRecordingId);
      if (rec) runAction(rec, artifact.label);
    });
    setCopyAction('', 'Copy result');
  }
}

function renderArtifactTabs() {
  const tabs = document.getElementById('artifact-tabs');
  if (!tabs) return;

  const availableKeys = new Set(['transcript']);
  let html = '<button class="artifact-tab" type="button" role="tab" data-artifact="transcript">Transcript</button>';
  if (speakerArtifact) {
    availableKeys.add('speakers');
    html += '<button class="artifact-tab" type="button" role="tab" data-artifact="speakers">Speakers</button>';
  }
  for (const artifact of pipelineArtifacts) {
    availableKeys.add(artifact.key);
    const stateClass = artifact.status === 'running' || artifact.status === 'waiting'
      ? 'running'
      : artifact.status === 'partial' ? 'failed' : '';
    const statusDot = stateClass ? `<span class="artifact-tab-status ${stateClass}"></span>` : '';
    html += `<button class="artifact-tab" type="button" role="tab" data-artifact="${escapeHtml(artifact.key)}">${escapeHtml(artifact.label)}${statusDot}</button>`;
  }

  if (!availableKeys.has(activeArtifact)) activeArtifact = 'transcript';
  tabs.innerHTML = html;
  tabs.querySelectorAll('.artifact-tab').forEach(tab => {
    const isActive = tab.dataset.artifact === activeArtifact;
    tab.classList.toggle('active', isActive);
    tab.setAttribute('aria-selected', String(isActive));
    tab.addEventListener('click', () => {
      activeArtifact = tab.dataset.artifact;
      renderArtifactTabs();
    });
  });
  renderActiveArtifact();
}

function syncPipelineArtifacts(states) {
  const latestByName = new Map();
  for (const pipelineRun of states || []) {
    const previous = latestByName.get(pipelineRun.name);
    if (!previous || (pipelineRun.run_index || 0) >= (previous.run_index || 0)) {
      latestByName.set(pipelineRun.name, pipelineRun);
    }
  }

  pipelineArtifacts = [...latestByName.values()].map(pipelineRun => {
    const runIndex = pipelineRun.run_index || 0;
    const existing = pipelineArtifacts.find(item =>
      item.label === pipelineRun.name && item.runIndex === runIndex
    );
    return {
      key: artifactKeyForPipeline(pipelineRun.name),
      label: pipelineRun.name,
      runIndex,
      status: String(pipelineRun.status || 'waiting').toLowerCase(),
      output: existing?.output || '',
      error: pipelineRun.error || existing?.error || '',
      loaded: existing?.loaded || false,
      loading: existing?.loading || false,
    };
  });

  const detailsBtn = document.getElementById('show-processing-details-btn');
  if (detailsBtn) detailsBtn.style.display = pipelineArtifacts.length > 0 ? '' : 'none';
  renderArtifactTabs();
}

async function loadPipelineArtifact(recordingId, artifact) {
  if (!recordingId || !artifact || artifact.loaded || artifact.loading) return;
  artifact.loading = true;
  try {
    const steps = await invoke('get_step_outputs', {
      recordingId,
      pipelineName: artifact.label,
      runIndex: artifact.runIndex,
    }) || [];
    const current = pipelineArtifacts.find(item =>
      item.key === artifact.key && item.runIndex === artifact.runIndex
    );
    if (!current || state.selectedRecordingId !== recordingId) return;
    const finalWithOutput = [...steps].reverse().find(step => step.output);
    const failedStep = steps.find(step => step.status === 'failed');
    current.output = finalWithOutput?.output || '';
    current.error = failedStep?.error || current.error;
    current.loaded = true;
    current.loading = false;
  } catch (error) {
    const current = pipelineArtifacts.find(item =>
      item.key === artifact.key && item.runIndex === artifact.runIndex
    );
    if (current) {
      current.error = String(error);
      current.loaded = true;
      current.loading = false;
    }
  }
  if (state.selectedRecordingId === recordingId && activeArtifact === artifact.key) {
    renderActiveArtifact();
  }
}

async function refreshPipelineArtifacts(recordingId) {
  const generation = ++artifactRefreshGeneration;
  try {
    const states = await invoke('get_all_pipeline_states', { recordingId }) || [];
    if (generation !== artifactRefreshGeneration || state.selectedRecordingId !== recordingId) return;
    syncPipelineArtifacts(states);
  } catch (error) {
    console.error('Failed to load recording results:', error);
  }
}

function closeDetailMenus() {
  for (const [buttonId, menuId] of [
    ['run-action-btn', 'run-action-menu'],
    ['detail-more-btn', 'detail-more-menu'],
  ]) {
    const button = document.getElementById(buttonId);
    const menu = document.getElementById(menuId);
    if (menu) menu.style.display = 'none';
    if (button) button.setAttribute('aria-expanded', 'false');
  }
}

function openDetailMenu(button, menu) {
  const willOpen = menu.style.display === 'none';
  closeDetailMenus();
  if (willOpen) {
    menu.style.display = '';
    button.setAttribute('aria-expanded', 'true');
  }
}

function populateActionMenu(rec, hasTranscript) {
  const button = document.getElementById('run-action-btn');
  const menu = document.getElementById('run-action-menu');
  if (!button || !menu) return;

  const actions = pipelineState.allPipelineDefs || [];
  button.disabled = !hasTranscript || rec.status === 'processing';
  button.title = !hasTranscript ? 'A transcript is needed before running an action' : 'Create another result';

  if (actions.length === 0) {
    menu.innerHTML = `
      <div class="detail-menu-empty">No actions configured yet.</div>
      <button class="detail-menu-item js-manage-actions" type="button" role="menuitem">Manage actions…</button>`;
  } else {
    menu.innerHTML = actions
      .map(action => `<button class="detail-menu-item" type="button" role="menuitem" data-action="${escapeHtml(action.name)}">${escapeHtml(action.name)}</button>`)
      .join('') + '<div class="detail-menu-separator"></div><button class="detail-menu-item js-manage-actions" type="button" role="menuitem">Manage actions…</button>';
  }

  menu.querySelectorAll('[data-action]').forEach(item => {
    item.addEventListener('click', () => runAction(rec, item.dataset.action));
  });
  menu.querySelector('.js-manage-actions')?.addEventListener('click', () => {
    closeDetailMenus();
    ViewManager.showSettings();
    if (window.__nbpSwitchSettingsTab) window.__nbpSwitchSettingsTab('pipelines');
  });
}

async function runAction(rec, actionName) {
  closeDetailMenus();
  activeArtifact = artifactKeyForPipeline(actionName);
  try {
    await invoke('assign_pipeline', { recordingId: rec.id, pipelineName: actionName });
    await refreshPipelineArtifacts(rec.id);
    const result = await invoke('execute_pipeline', { recordingId: rec.id, pipelineName: actionName });
    const artifact = pipelineArtifacts.find(item => item.key === activeArtifact);
    if (artifact) {
      artifact.status = result === 'partial' ? 'partial' : 'done';
      artifact.loaded = false;
      await loadPipelineArtifact(rec.id, artifact);
    }
  } catch (error) {
    console.error(`Action "${actionName}" failed:`, error);
    showToast(`Action failed: ${error}`, 'error');
  }
  await loadRecordings();
  if (state.selectedRecordingId === rec.id) renderArtifactTabs();
}

function formatPlaybackTime(milliseconds) {
  const seconds = Math.max(0, Math.floor((milliseconds || 0) / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, '0')}:${String(remainder).padStart(2, '0')}`
    : `${String(minutes).padStart(2, '0')}:${String(remainder).padStart(2, '0')}`;
}

function updatePlaybackUi(playback = null) {
  const playingThis = playback && playback.recording_id === detailPlaybackRecordingId;
  const status = playingThis ? playback.status : 'Stopped';
  const position = playingThis ? playback.current_position_ms : 0;
  const duration = playingThis
    ? (playback.duration_ms || detailAudioDurationMs)
    : detailAudioDurationMs;
  const currentEl = document.getElementById('detail-audio-current');
  const durationEl = document.getElementById('detail-audio-duration');
  const progressEl = document.getElementById('detail-playback-progress');
  const playButton = document.getElementById('detail-play-btn');
  const playIcon = document.querySelector('#detail-play-btn .detail-play-icon');
  const pauseIcon = document.querySelector('#detail-play-btn .detail-pause-icon');
  if (currentEl) currentEl.textContent = formatPlaybackTime(position);
  if (durationEl) durationEl.textContent = formatPlaybackTime(duration);
  if (progressEl) progressEl.style.width = duration > 0 ? `${Math.min(100, position / duration * 100)}%` : '0';
  const isPlaying = status === 'Playing';
  if (playIcon) playIcon.style.display = isPlaying ? 'none' : '';
  if (pauseIcon) pauseIcon.style.display = isPlaying ? '' : 'none';
  if (playButton) {
    const label = isPlaying ? 'Pause recording' : 'Play recording';
    playButton.title = label;
    playButton.setAttribute('aria-label', label);
  }
}

function startPlaybackPolling() {
  if (playbackPollInterval) clearInterval(playbackPollInterval);
  playbackPollInterval = setInterval(async () => {
    try {
      const playback = await invoke('get_playback_state');
      updatePlaybackUi(playback);
      if (playback.status === 'Stopped') {
        clearInterval(playbackPollInterval);
        playbackPollInterval = null;
        detailPlaybackOwnsAudio = false;
      }
    } catch (_) { /* The player is optional; keep the rest of detail usable. */ }
  }, 250);
}

function stopDetailPlayback() {
  if (playbackPollInterval) {
    clearInterval(playbackPollInterval);
    playbackPollInterval = null;
  }
  if (detailPlaybackOwnsAudio) invoke('stop_audio').catch(() => {});
  detailPlaybackRecordingId = null;
  detailAudioDurationMs = 0;
  detailPlaybackOwnsAudio = false;
  const container = document.getElementById('detail-audio-player');
  if (container) container.style.display = 'none';
  updatePlaybackUi();
}

function loadDetailAudio(recordingId, rec) {
  const container = document.getElementById('detail-audio-player');
  if (!container) return;
  detailPlaybackRecordingId = recordingId;
  detailAudioDurationMs = Math.max(0, Math.round(getDuration(rec) * 1000));
  container.style.display = rec.status === 'processing' ? 'none' : '';
  updatePlaybackUi();
}

export async function showDetailView(id) {
  const rec = state.allRecordings.find(r => r.id === id);
  if (!rec) return;

  if (detailRecordingId !== id) {
    stopDetailPlayback();
    detailRecordingId = id;
    activeArtifact = 'transcript';
    transcriptCopyText = '';
    speakerArtifact = null;
    pipelineArtifacts = [];
    artifactRefreshGeneration++;
    const processingSection = document.getElementById('pipeline-status-section');
    if (processingSection) {
      processingSection.dataset.userVisible = 'false';
      processingSection.style.display = 'none';
    }
    const processingDetailsBtn = document.getElementById('show-processing-details-btn');
    if (processingDetailsBtn) processingDetailsBtn.style.display = 'none';
  }

  setSelectedRecordingId(id);
  stopProcessingPoll();
  clearTranscriptionTimer();
  closeDetailMenus();
  updateMainButton();

  const detailTitleInput = document.getElementById('detail-title');
  const detailMetaHeaderEl = document.getElementById('detail-meta-header');
  const detailTranscriptEl = document.getElementById('transcript-content');
  const deleteBtnHeader = document.getElementById('delete-btn-header');
  const openFolderBtnHeader = document.getElementById('open-folder-btn-header');
  const prBtn = document.getElementById('process-btn');

  if (detailTitleInput) detailTitleInput.value = rec.title || '';

  renderCalendarRow(rec);
  syncPipelineArtifacts(rec.pipelines || []);

  const isProcessing = rec.status === 'processing';

  if (detailMetaHeaderEl) {
    if (!(state.isRecording && id === state.selectedRecordingId && !isProcessing)) {
      const statusText = isProcessing
        ? '<span style="color:var(--accent)">Processing...</span>'
        : formatDuration(getDuration(rec));
      detailMetaHeaderEl.innerHTML = `${new Date(rec.created_at).toLocaleString(undefined, dateOptions)} \u00b7 ${statusText} `;
    }
  }

  ViewManager.showDetail();
  subscribeToProgress(id);
  loadDetailAudio(id, rec);
  renderDiarization(rec);

  if (deleteBtnHeader) {
    deleteBtnHeader.disabled = isProcessing;
    deleteBtnHeader.style.opacity = isProcessing ? '0.3' : '1';
    deleteBtnHeader.style.pointerEvents = isProcessing ? 'none' : 'auto';
    deleteBtnHeader.title = isProcessing ? 'Processing audio…' : 'Delete recording';
  }
  if (openFolderBtnHeader) {
    openFolderBtnHeader.title = 'Reveal source files';
  }

  if (prBtn) {
    prBtn.disabled = isProcessing;
  }

  const retranscribeBtn = document.getElementById('retranscribe-btn');
  const rediarizeBtn = document.getElementById('rediariarize-btn');
  if (retranscribeBtn) retranscribeBtn.disabled = isProcessing;
  if (rediarizeBtn) rediarizeBtn.disabled = isProcessing;

  // Polling if processing — single active poller, guarded by a generation
  // counter so an in-flight tick from a prior poller becomes a no-op.
  if (isProcessing) {
    const pollId = id;
    const generation = ++processingPollGeneration;
    processingPollIntervalId = setInterval(async () => {
      if (processingPollGeneration !== generation || state.selectedRecordingId !== pollId) {
        return;
      }
      try {
        await loadRecordings();
        if (processingPollGeneration !== generation) return;
        const updated = state.allRecordings.find(r => r.id === pollId);
        if (updated && updated.status !== 'processing') {
          stopProcessingPoll();
          showDetailView(pollId);
        }
      } catch (e) {
        console.error('Processing poll error:', e);
        if (processingPollGeneration === generation) stopProcessingPoll();
      }
    }, 1000);
  }

  // The transcript is the stable source artifact. Processing, speakers and
  // action results decorate it; they never replace or hide it.
  if (detailTranscriptEl) {
    if (isProcessing) {
      transcriptCopyText = '';
      detailTranscriptEl.innerHTML = `
        <div class="transcript-processing-state">
          <div class="transcript-processing-spinner"></div>
          <span class="transcript-processing-text">Preparing recording…</span>
        </div>`;
      detailTranscriptEl.classList.remove('empty');
      populateActionMenu(rec, false);
      renderArtifactTabs();
      return;
    }
    try {
      const transcript = await invoke('get_transcript', { recordingId: id });
      if (state.selectedRecordingId !== id) return;
      const isTranscribing = transcript
        ? false
        : await invoke('is_transcribing', { recordingId: id });
      if (state.selectedRecordingId !== id) return;
      if (prBtn) prBtn.disabled = isTranscribing;
      if (retranscribeBtn) retranscribeBtn.disabled = isTranscribing;

      if (transcript) {
        transcriptCopyText = transcript.trim();
        applyMarkdownRendering(detailTranscriptEl, transcript);
        detailTranscriptEl.classList.remove('empty');
      } else if (isTranscribing) {
        transcriptCopyText = '';
        detailTranscriptEl.innerHTML = `
          <div class="transcript-processing-state">
            <div class="transcript-processing-spinner"></div>
            <span class="transcript-processing-text">Transcribing…</span>
          </div>
        `;
        detailTranscriptEl.classList.remove('empty');
      } else {
        transcriptCopyText = '';
        detailTranscriptEl.textContent = 'Not processed yet.';
        detailTranscriptEl.classList.add('empty');
      }
      populateActionMenu(rec, !!transcriptCopyText);
      renderArtifactTabs();
    } catch (err) {
      console.error('Failed to load transcript:', err);
      transcriptCopyText = '';
      detailTranscriptEl.textContent = 'Not processed yet.';
      detailTranscriptEl.classList.add('empty');
      populateActionMenu(rec, false);
      renderArtifactTabs();
    }
  }
}

// Wire back button
const backBtn = document.getElementById('back-btn');
if (backBtn) backBtn.addEventListener('click', hideDetailView);

// The primary action always copies the selected artifact, without forcing the
// user to hunt for an export flow or understand how it is stored on disk.
const copyTranscriptBtn = document.getElementById('copy-transcript-btn-header');
if (copyTranscriptBtn) {
  copyTranscriptBtn.addEventListener('click', async () => {
    const text = (copyTranscriptBtn.dataset.copyText || '').trim();
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      const label = document.getElementById('copy-artifact-label');
      if (label) {
        label.textContent = '✓ Copied';
        setTimeout(() => {
          label.textContent = copyTranscriptBtn.dataset.defaultLabel || 'Copy transcript';
        }, 1200);
      }
    } catch (e) {
      console.error('Copy failed:', e);
      showToast('Copy failed', 'error');
    }
  });
}

const runActionBtn = document.getElementById('run-action-btn');
const runActionMenu = document.getElementById('run-action-menu');
if (runActionBtn && runActionMenu) {
  runActionBtn.addEventListener('click', (event) => {
    event.stopPropagation();
    openDetailMenu(runActionBtn, runActionMenu);
  });
  runActionMenu.addEventListener('click', event => event.stopPropagation());
}

const detailMoreBtn = document.getElementById('detail-more-btn');
const detailMoreMenu = document.getElementById('detail-more-menu');
if (detailMoreBtn && detailMoreMenu) {
  detailMoreBtn.addEventListener('click', (event) => {
    event.stopPropagation();
    openDetailMenu(detailMoreBtn, detailMoreMenu);
  });
  detailMoreMenu.addEventListener('click', event => event.stopPropagation());
}

document.addEventListener('click', closeDetailMenus);
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') closeDetailMenus();
});

const detailPlayBtn = document.getElementById('detail-play-btn');
if (detailPlayBtn) {
  detailPlayBtn.addEventListener('click', async () => {
    if (!detailPlaybackRecordingId) return;
    try {
      const playback = await invoke('get_playback_state');
      const isCurrent = playback.recording_id === detailPlaybackRecordingId;
      if (isCurrent && playback.status === 'Playing') {
        await invoke('pause_audio');
      } else if (isCurrent && playback.status === 'Paused') {
        await invoke('resume_audio');
      } else {
        await invoke('play_audio', { recordingId: detailPlaybackRecordingId });
        detailPlaybackOwnsAudio = true;
      }
      const updated = await invoke('get_playback_state');
      updatePlaybackUi(updated);
      startPlaybackPolling();
    } catch (error) {
      console.error('Playback failed:', error);
      showToast('Could not play this recording', 'error');
    }
  });
}

const retranscribeBtn = document.getElementById('retranscribe-btn');
if (retranscribeBtn) {
  retranscribeBtn.addEventListener('click', () => {
    closeDetailMenus();
    document.getElementById('process-btn')?.click();
  });
}

const rediarizeBtn = document.getElementById('rediariarize-btn');
if (rediarizeBtn) {
  rediarizeBtn.addEventListener('click', () => {
    closeDetailMenus();
    const rec = state.allRecordings.find(item => item.id === state.selectedRecordingId);
    if (rec) startDiarization(rec);
  });
}

const showProcessingDetailsBtn = document.getElementById('show-processing-details-btn');
if (showProcessingDetailsBtn) {
  showProcessingDetailsBtn.addEventListener('click', async () => {
    closeDetailMenus();
    const section = document.getElementById('pipeline-status-section');
    if (!section || !state.selectedRecordingId) return;
    section.dataset.userVisible = 'true';
    await renderPipelineStatus(state.selectedRecordingId);
    if (section.dataset.hasData === 'true') section.scrollIntoView({ behavior: 'smooth', block: 'start' });
  });
}

const hideProcessingDetailsBtn = document.getElementById('hide-processing-details-btn');
if (hideProcessingDetailsBtn) {
  hideProcessingDetailsBtn.addEventListener('click', () => {
    const section = document.getElementById('pipeline-status-section');
    if (!section) return;
    section.dataset.userVisible = 'false';
    section.style.display = 'none';
  });
}

// Wire delete button in detail header
const deleteBtnHeader = document.getElementById('delete-btn-header');
if (deleteBtnHeader) {
  deleteBtnHeader.addEventListener('click', async () => {
    closeDetailMenus();
    if (!state.selectedRecordingId) return;
    const ok = await showConfirm('Delete Recording?', 'This action cannot be undone.');
    if (!ok) return;
    try {
      await invoke('delete_recording', { recordingId: state.selectedRecordingId });
      hideDetailView();
      await loadRecordings();
    } catch (e) {
      console.error('Delete failed:', e);
      if (e && typeof e === 'string' && e.includes('finalized')) {
        showToast('Recording is still being finalized. Please wait a moment and try again.', 'info');
      } else {
        showToast('Delete failed: ' + e, 'error');
      }
    }
  });
}

// Wire open-folder button in detail header
const openFolderBtnHeader = document.getElementById('open-folder-btn-header');
if (openFolderBtnHeader) {
  openFolderBtnHeader.addEventListener('click', async () => {
    closeDetailMenus();
    if (!state.selectedRecordingId || !state.appSettings?.storage_path) return;
    const folderPath = `${state.appSettings.storage_path}/${state.selectedRecordingId}`;
    try {
      await window.__TAURI_PLUGIN_OPENER__.openPath(folderPath);
    } catch (e) {
      console.error('Failed to open folder:', e);
    }
  });
}

// Wire Transcribe button
const processBtn = document.getElementById('process-btn');
if (processBtn) {
  processBtn.addEventListener('click', async () => {
    if (!state.selectedRecordingId || processBtn.disabled) return;
    const recordingId = state.selectedRecordingId;
    const detailTranscriptEl = document.getElementById('transcript-content');

    try {
      processBtn.disabled = true;
      processBtn.style.opacity = '1';
      clearTranscriptionTimer();
      processBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Processing...</span>';
      transcriptCopyText = '';
      const currentRec = state.allRecordings.find(item => item.id === recordingId);
      if (currentRec) populateActionMenu(currentRec, false);
      renderArtifactTabs();

      if (detailTranscriptEl) {
        detailTranscriptEl.innerHTML = `
          <div class="transcript-processing-state">
            <div class="transcript-processing-spinner"></div>
            <span class="transcript-processing-text">Processing audio...</span>
          </div>
        `;
        detailTranscriptEl.classList.remove('empty');
      }
      const transcript = await invoke('transcribe_recording', { recordingId });

      if (transcript === '__already_running__') {
        processBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Processing...</span>';
        return;
      }

      if (detailTranscriptEl) {
        applyMarkdownRendering(detailTranscriptEl, transcript);
        detailTranscriptEl.classList.remove('empty');
      }
      transcriptCopyText = (transcript || '').trim();
      const rec = state.allRecordings.find(item => item.id === recordingId);
      if (rec) populateActionMenu(rec, !!transcriptCopyText);
      renderArtifactTabs();

      // Auto-execute waiting pipelines
      try {
        const states = await invoke('get_all_pipeline_states', { recordingId });
        for (const s of (states || [])) {
          if (s.status === 'waiting') {
            invoke('execute_pipeline', { recordingId, pipelineName: s.name }).catch(e =>
              console.error(`Auto-execute pipeline "${s.name}" failed:`, e)
            );
          }
        }
      } catch (e) { console.error('Failed to auto-execute waiting pipelines:', e); }

      clearTranscriptionTimer();
      processBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      processBtn.disabled = false;

    } catch (error) {
      clearTranscriptionTimer();
      console.error('Transcription failed:', error);
      showToast(`Transcription failed: ${error}`, 'error');

      if (detailTranscriptEl) {
        detailTranscriptEl.textContent = 'Transcription failed.';
        detailTranscriptEl.classList.add('empty');
      }
      transcriptCopyText = '';
      renderArtifactTabs();

      processBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      processBtn.disabled = false;
    }
  });
}

// Listen for transcription progress events
listen('transcription_progress', (event) => {
  const { recording_id, stage, percent } = event.payload;
  if (recording_id !== state.selectedRecordingId) return;
  const btn = document.getElementById('process-btn');
  if (!btn) return;
  btn.disabled = true;

  const detailTranscriptEl = document.getElementById('transcript-content');

  if (stage === 'Done') {
    clearTranscriptionTimer();
    btn.disabled = false;
    btn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
    // The progress block above replaced the transcript content with the
    // 100% bar. Without re-rendering from disk the user sees "100%" until
    // they reopen the recording. Pull the freshly-written transcript and
    // swap it in.
    if (detailTranscriptEl) {
      invoke('get_transcript', { recordingId: recording_id })
        .then((transcript) => {
          if (state.selectedRecordingId !== recording_id) return;
          if (transcript) {
            applyMarkdownRendering(detailTranscriptEl, transcript);
            detailTranscriptEl.classList.remove('empty');
            transcriptCopyText = transcript.trim();
            const rec = state.allRecordings.find(item => item.id === recording_id);
            if (rec) populateActionMenu(rec, true);
            renderArtifactTabs();
          }
        })
        .catch((err) => console.error('get_transcript post-Done failed:', err));
    }
    return;
  }

  if (transcriptCopyText) {
    transcriptCopyText = '';
    const rec = state.allRecordings.find(item => item.id === recording_id);
    if (rec) populateActionMenu(rec, false);
    renderArtifactTabs();
  }

  const STAGE_LABELS = {
    'Starting': 'Transcribing', 'Loading model': 'Loading model',
    'Transcribing': 'Transcribing', 'Preparing models': 'Preparing models',
    'Downloading ASR model': 'Downloading model', 'Downloading diarizer': 'Downloading model',
    'Diarization': 'Diarization', 'Finalizing': 'Finalizing',
  };
  const stageLabel = STAGE_LABELS[stage] || stage;

  if (percent > 0) {
    clearTranscriptionTimer();
    btn.innerHTML = `<span style="font-weight: 600; font-size: 12px;">${stageLabel} ${percent}%</span>`;
    if (detailTranscriptEl) {
      detailTranscriptEl.classList.remove('empty');
      detailTranscriptEl.innerHTML = `
        <div class="transcription-progress-wrap">
          <div class="transcription-progress-stage">${stageLabel}</div>
          <div class="transcription-progress-bar-track">
            <div class="transcription-progress-bar-fill" style="width:${percent}%"></div>
          </div>
          <div class="transcription-progress-percent">${percent}%</div>
        </div>`;
    }
  } else {
    transcriptionCurrentStage = stageLabel;
    btn.innerHTML = `<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">${stageLabel}…</span>`;
    if (detailTranscriptEl && !detailTranscriptEl.querySelector('.transcript-processing-state')) {
      detailTranscriptEl.classList.remove('empty');
      detailTranscriptEl.innerHTML = `
        <div class="transcript-processing-state">
          <div class="transcript-processing-spinner"></div>
          <span class="transcript-processing-text">${stageLabel}…</span>
        </div>`;
    }
  }
});

on('recording:artifactsChanged', change => {
  if (typeof change === 'string') {
    if (change === state.selectedRecordingId) refreshPipelineArtifacts(change);
    return;
  }

  const recordingId = change?.recording_id;
  if (!recordingId || recordingId !== state.selectedRecordingId) return;
  const key = artifactKeyForPipeline(change.pipeline_name);
  const artifact = pipelineArtifacts.find(item => item.key === key);
  if (!artifact) {
    refreshPipelineArtifacts(recordingId);
    return;
  }

  if (change.status === 'failed') {
    artifact.status = 'partial';
    artifact.loaded = false;
  } else if (change.status === 'running') {
    artifact.status = 'running';
  } else if (change.status === 'done' && change.step_index + 1 === change.total_steps) {
    artifact.status = 'done';
    artifact.loaded = false;
  }
  renderArtifactTabs();
});
