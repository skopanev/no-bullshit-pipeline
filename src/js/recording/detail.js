import { invoke, listen } from '../core/tauri.js';
import * as state from '../core/state.js';
import { setSelectedRecordingId } from '../core/state.js';
import { formatDuration, getDuration, escapeHtml } from '../core/utils.js';
import { looksLikeMarkdown, applyMarkdownRendering } from '../ui/markdown.js';
import { showToast } from '../ui/toast.js';
import { showConfirm } from '../ui/confirm-modal.js';
import { ViewManager } from '../ui/view-manager.js';
import { updateMainButton } from './controls.js';
import { loadRecordings } from './list.js';
import { subscribeToProgress, renderPipelineStatus, cleanupPipelineProgress } from './pipeline-status.js';
import { renderPipelineFlowHTML } from '../pipeline/flow-renderer.js';
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
  stopProcessingPoll();
  clearSpeakerBar();
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

export async function showDetailView(id) {
  const rec = state.allRecordings.find(r => r.id === id);
  if (!rec) return;

  setSelectedRecordingId(id);
  stopProcessingPoll();
  clearTranscriptionTimer();
  updateMainButton();

  const detailTitleInput = document.getElementById('detail-title');
  const detailMetaHeaderEl = document.getElementById('detail-meta-header');
  const detailTranscriptEl = document.getElementById('transcript-content');
  const detailStructuredEl = document.getElementById('structured-content');
  const deleteBtnHeader = document.getElementById('delete-btn-header');
  const openFolderBtnHeader = document.getElementById('open-folder-btn-header');
  const prBtn = document.getElementById('process-btn');
  const saveTranscriptBtn = document.getElementById('save-transcript-btn');

  if (detailTitleInput) detailTitleInput.value = rec.title || '';

  renderCalendarRow(rec);

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

  if (deleteBtnHeader) {
    deleteBtnHeader.style.opacity = isProcessing ? '0.3' : '1';
    deleteBtnHeader.style.pointerEvents = isProcessing ? 'none' : 'auto';
    deleteBtnHeader.title = isProcessing ? 'Processing audio...' : 'Delete';
  }
  if (openFolderBtnHeader) {
    openFolderBtnHeader.title = 'Open Folder';
  }

  if (prBtn) {
    prBtn.disabled = isProcessing;
    prBtn.style.opacity = '1';
    if (isProcessing) {
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Mixing Audio...</span>';
    } else {
      prBtn.disabled = true;
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      const checkId = id;
      invoke('is_transcribing', { recordingId: checkId }).then(active => {
        if (state.selectedRecordingId === checkId && prBtn) {
          if (active) {
            prBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Auto-transcribing...</span>';
            prBtn.style.opacity = '0.6';
          } else {
            prBtn.disabled = false;
            prBtn.style.display = '';
          }
        }
      }).catch(() => {
        if (state.selectedRecordingId === checkId && prBtn) {
          prBtn.disabled = false;
          prBtn.style.display = '';
        }
      });
    }
  }

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

  // Pipeline cards section
  renderPipelineCards(id, isProcessing);

  // Transcript section visibility
  const hideContent = isProcessing;
  const transcriptSection = document.getElementById('transcript-section');
  if (transcriptSection) transcriptSection.style.display = hideContent ? 'none' : '';

  renderPipelineStatus(id);

  // Load Transcript
  if (!hideContent && detailTranscriptEl) {
    const rawToggle = document.getElementById('transcript-raw-toggle');

    try {
      clearSpeakerBar();
      const isTranscribing = await invoke('is_transcribing', { recordingId: id });
      const transcript = await invoke('get_transcript', { recordingId: id });

      if (transcript) {
        applyMarkdownRendering(detailTranscriptEl, transcript);
        renderSpeakerBar(id);
        detailTranscriptEl.classList.remove('empty');
        if (saveTranscriptBtn) saveTranscriptBtn.style.display = '';
        if (rawToggle) rawToggle.style.display = looksLikeMarkdown(transcript) ? '' : 'none';
      } else if (isTranscribing) {
        detailTranscriptEl.innerHTML = `
          <div class="transcript-processing-state">
            <div class="transcript-processing-spinner"></div>
            <span class="transcript-processing-text">Processing audio...</span>
          </div>
        `;
        detailTranscriptEl.classList.remove('empty');
        if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';
        if (rawToggle) rawToggle.style.display = 'none';
      } else {
        detailTranscriptEl.textContent = 'Not processed yet.';
        detailTranscriptEl.classList.add('empty');
        if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';
        if (rawToggle) rawToggle.style.display = 'none';
      }
    } catch (err) {
      console.error('Failed to load transcript:', err);
      detailTranscriptEl.textContent = 'Not processed yet.';
      detailTranscriptEl.classList.add('empty');
      if (saveTranscriptBtn) saveTranscriptBtn.style.display = 'none';
      if (rawToggle) rawToggle.style.display = 'none';
    }
  }

  if (!hideContent && detailStructuredEl) {
    detailStructuredEl.textContent = 'Not processed yet.';
    detailStructuredEl.classList.add('empty');
  }
}

// Pipeline cards: show available pipelines to assign to a recording
let _pipelineCardAnimating = false;

async function renderPipelineCards(id, isProcessing) {
  const detailPipelineAssignment = document.getElementById('detail-pipeline-assignment');
  const pipelineCardsEl = document.getElementById('pipeline-cards');
  if (!detailPipelineAssignment || !pipelineCardsEl) return;

  if (!isProcessing && pipelineState.allPipelineDefs && pipelineState.allPipelineDefs.length > 0) {
    let alreadyUsed = new Set();
    try {
      const states = await invoke('get_all_pipeline_states', { recordingId: id });
      if (states) for (const s of states) alreadyUsed.add(s.name);
    } catch (_) { /* states may not exist yet */ }
    const availableDefs = pipelineState.allPipelineDefs.filter(p => !alreadyUsed.has(p.name));

    let cardsHtml = '';
    for (const p of availableDefs) {
      const flowHtml = renderPipelineFlowHTML(p.steps || [], { compact: true });
      cardsHtml += `<div class="pipeline-card" data-pipeline="${escapeHtml(p.name)}">${flowHtml}<div class="pipeline-card-name">${escapeHtml(p.name)}</div></div>`;
    }
    pipelineCardsEl.innerHTML = cardsHtml;
    detailPipelineAssignment.style.display = availableDefs.length > 0 ? '' : 'none';

    // Click: assign → animate to runs → execute when possible
    pipelineCardsEl.querySelectorAll('.pipeline-card').forEach(card => {
      card.addEventListener('click', async () => {
        if (_pipelineCardAnimating) return;
        _pipelineCardAnimating = true;
        const pipelineName = card.dataset.pipeline;

        const statusSection = document.getElementById('pipeline-status-section');
        const statusContent = document.getElementById('pipeline-status-content');
        const target = statusContent || statusSection;
        const cardRect = card.getBoundingClientRect();

        // Create flying clone
        const clone = card.cloneNode(true);
        clone.style.cssText = `position:fixed;left:${cardRect.left}px;top:${cardRect.top}px;width:${cardRect.width}px;height:${cardRect.height}px;z-index:10000;pointer-events:none;margin:0;border-radius:var(--radius-sm);`;
        document.body.appendChild(clone);

        // Collapse original card
        card.style.overflow = 'hidden';
        card.style.pointerEvents = 'none';
        card.style.maxWidth = card.offsetWidth + 'px';
        card.style.minWidth = '0';
        void card.offsetWidth;
        card.style.transition = 'max-width 0.35s ease, padding 0.35s ease, border-width 0.35s ease, opacity 0.15s ease, margin 0.35s ease';
        card.style.maxWidth = '0';
        card.style.padding = '0';
        card.style.borderWidth = '0';
        card.style.margin = '0';
        card.style.opacity = '0';

        // Ensure runs section is visible for measuring
        if (statusSection && statusSection.style.display === 'none') {
          statusSection.style.display = '';
          statusSection.style.opacity = '0';
        }

        // Fly clone to runs section
        let dx = 0, dy = 120;
        if (target) {
          const targetRect = target.getBoundingClientRect();
          dx = targetRect.left - cardRect.left;
          dy = (targetRect.top + Math.min(targetRect.height, 40)) - cardRect.top;
        }
        clone.style.transition = 'transform 0.4s cubic-bezier(0.4, 0, 0.15, 1), opacity 0.35s ease-in, box-shadow 0.4s ease';
        void clone.offsetWidth;
        clone.style.transform = `translate(${dx}px, ${dy}px) scale(0.7)`;
        clone.style.opacity = '0';
        clone.style.boxShadow = '0 4px 24px rgba(var(--accent-rgb, 99,102,241), 0.3)';

        // Backend: assign pipeline
        state.currentAssignedPipelines.add(pipelineName);
        let assigned = false;
        try {
          await invoke('assign_pipeline', { recordingId: id, pipelineName });
          assigned = true;
        } catch (err) {
          console.error('Failed to assign pipeline:', err);
        }

        setTimeout(async () => {
          clone.remove();
          _pipelineCardAnimating = false;

          if (statusSection) {
            statusSection.style.display = '';
            statusSection.style.opacity = '';
          }

          if (assigned) {
            card.style.display = 'none';
            // Execute when possible (transcript exists)
            try {
              const detailTranscriptEl = document.getElementById('transcript-content');
              const hasTranscript = detailTranscriptEl && !detailTranscriptEl.classList.contains('empty');
              if (hasTranscript) {
                await invoke('execute_pipeline', { recordingId: id, pipelineName });
              }
            } catch (err) {
              console.error('Failed to execute pipeline:', err);
            }
          } else {
            // Restore card on failure
            card.style.display = '';
            card.style.maxWidth = '';
            card.style.minWidth = '';
            card.style.padding = '';
            card.style.borderWidth = '';
            card.style.margin = '';
            card.style.opacity = '';
            card.style.overflow = '';
            card.style.pointerEvents = '';
            card.style.transition = '';
          }

          await loadRecordings();
          if (state.selectedRecordingId === id) renderPipelineStatus(id);

          // Flash runs section
          const flashTarget = document.getElementById('pipeline-status-content');
          if (flashTarget) {
            flashTarget.classList.remove('pipeline-status-flash');
            void flashTarget.offsetWidth;
            flashTarget.classList.add('pipeline-status-flash');
          }
        }, 420);
      });
    });
  } else {
    pipelineCardsEl.innerHTML = '<div style="color: var(--text-secondary); opacity: 0.75; font-size: 0.82rem;">No pipelines yet. Add one in Settings -> Pipelines.</div>';
    detailPipelineAssignment.style.display = '';
  }
}

// Wire back button
const backBtn = document.getElementById('back-btn');
if (backBtn) backBtn.addEventListener('click', hideDetailView);

// Wire copy-transcript button in detail header. Copies what's actually shown
// in #transcript-content (innerText keeps line breaks / speaker labels) so
// the user gets the same view they see — not raw vs processed surprises.
const copyTranscriptBtn = document.getElementById('copy-transcript-btn-header');
if (copyTranscriptBtn) {
  copyTranscriptBtn.addEventListener('click', async () => {
    const el = document.getElementById('transcript-content');
    const text = (el?.innerText || el?.textContent || '').trim();
    if (!text) {
      showToast('No transcript to copy', 'info');
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      showToast('Transcript copied', 'success');
    } catch (e) {
      console.error('copy transcript failed:', e);
      showToast('Copy failed', 'error');
    }
  });
}

// Wire delete button in detail header
const deleteBtnHeader = document.getElementById('delete-btn-header');
if (deleteBtnHeader) {
  deleteBtnHeader.addEventListener('click', async () => {
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
const saveTranscriptBtnGlobal = document.getElementById('save-transcript-btn');
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

      if (detailTranscriptEl) {
        detailTranscriptEl.innerHTML = `
          <div class="transcript-processing-state">
            <div class="transcript-processing-spinner"></div>
            <span class="transcript-processing-text">Processing audio...</span>
          </div>
        `;
        detailTranscriptEl.classList.remove('empty');
      }
      if (saveTranscriptBtnGlobal) saveTranscriptBtnGlobal.style.display = 'none';

      const transcript = await invoke('transcribe_recording', { recordingId });

      if (transcript === '__already_running__') {
        processBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Processing...</span>';
        return;
      }

      if (detailTranscriptEl) {
        applyMarkdownRendering(detailTranscriptEl, transcript);
        renderSpeakerBar(recordingId);
        detailTranscriptEl.classList.remove('empty');
      }
      if (saveTranscriptBtnGlobal) saveTranscriptBtnGlobal.style.display = '';

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
            renderSpeakerBar(recording_id);
            detailTranscriptEl.classList.remove('empty');
          }
        })
        .catch((err) => console.error('get_transcript post-Done failed:', err));
    }
    return;
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

// Close fn of the currently-open contact picker, so it can be torn down (with
// its document/window listeners) from outside — not just by clicking away.
let activePickerClose = null;

/** Remove any speaker rename bar(s) and fully close the floating contact picker. */
function clearSpeakerBar() {
  document.querySelectorAll('.speaker-bar').forEach((el) => el.remove());
  if (activePickerClose) activePickerClose();
}

/**
 * Render an inline speaker-rename bar above the transcript. Only shown for
 * multi-speaker (diarized) recordings. Renaming applies at render time on the
 * backend — the transcript record itself is never rewritten — so it's safe and
 * reversible. Re-renders the transcript and refreshes the list preview on rename.
 */
async function renderSpeakerBar(recordingId) {
  let speakers = [];
  try {
    speakers = await invoke('get_speakers', { recordingId });
  } catch (err) {
    console.error('get_speakers failed:', err);
    return;
  }
  // The user may have navigated to another recording while we awaited — the
  // detail DOM now belongs to a different one. Bail rather than paint into it.
  if (state.selectedRecordingId !== recordingId) return;
  const transcriptEl = document.getElementById('transcript-content');
  if (!transcriptEl || !transcriptEl.parentNode) return;

  // Clear right before inserting so concurrent calls can't stack bars.
  clearSpeakerBar();
  // Only worth a rename bar when diarization actually split >1 speaker.
  if (!speakers || speakers.length < 2) return;

  const bar = document.createElement('div');
  bar.className = 'speaker-bar';
  const label = document.createElement('span');
  label.className = 'speaker-bar-label';
  label.textContent = 'Speakers';
  bar.appendChild(label);
  // DOM nodes + textContent (never innerHTML) so a display name can't inject.
  for (const s of speakers) {
    const chip = document.createElement('button');
    chip.className = 'speaker-chip';
    chip.title = 'Rename speaker';
    chip.textContent = s.display_name || s.speaker_id;
    chip.addEventListener('click', (e) => {
      e.stopPropagation();
      openSpeakerPicker(chip, recordingId, s.speaker_id);
    });
    bar.appendChild(chip);
  }
  transcriptEl.parentNode.insertBefore(bar, transcriptEl);
}

/** Assign `name` (a real person / contact, or empty to reset) to a speaker. */
async function applySpeakerName(recordingId, speakerId, name) {
  try {
    const rendered = await invoke('rename_speaker', {
      recordingId,
      speakerId,
      displayName: name,
    });
    // Only touch the transcript DOM if we're still on this recording.
    if (state.selectedRecordingId === recordingId) {
      const el = document.getElementById('transcript-content');
      if (el) applyMarkdownRendering(el, rendered);
      await renderSpeakerBar(recordingId);
    }
    loadRecordings();
  } catch (err) {
    console.error('rename_speaker failed:', err);
    showToast('Failed to rename speaker: ' + err, 'error');
  }
}

/**
 * Open a small picker under a speaker chip: pick a real person from the
 * recording's calendar attendees, type a free-form name, or reset to the raw
 * "Speaker N" label. (macOS Contacts — with photos — can feed this list later.)
 */
async function openSpeakerPicker(chip, recordingId, speakerId) {
  if (activePickerClose) activePickerClose();

  const rec = state.allRecordings.find((r) => r.id === recordingId);
  const attendees = rec && Array.isArray(rec.attendees) ? rec.attendees : [];
  // Distinct attendee labels, in first-seen order.
  const people = [...new Set(attendees.map((a) => a.name || a.email).filter(Boolean))];

  // Voice samples so the user can recognize who this is before naming them.
  let samples = [];
  try {
    samples = await invoke('get_speaker_samples', { recordingId, speakerId });
  } catch (err) {
    console.error('get_speaker_samples failed:', err);
  }

  const menu = document.createElement('div');
  menu.className = 'speaker-picker';

  function close() {
    menu.remove();
    document.removeEventListener('click', onDoc, true);
    document.removeEventListener('keydown', onKey, true);
    window.removeEventListener('scroll', close, true);
    if (activePickerClose === close) activePickerClose = null;
  }
  activePickerClose = close;
  function onDoc(e) {
    if (!menu.contains(e.target)) close();
  }
  function onKey(e) {
    if (e.key === 'Escape') close();
  }

  // Build items as DOM nodes (textContent) so a name can't inject markup.
  const addItem = (text, extraClass, onClick) => {
    const b = document.createElement('button');
    b.className = 'speaker-picker-item' + (extraClass ? ' ' + extraClass : '');
    b.textContent = text;
    b.addEventListener('click', onClick);
    menu.appendChild(b);
  };

  // Voice-sample row: play a few of this speaker's turns to recognize them
  // before assigning a name. Buttons don't close the picker.
  if (samples && samples.length) {
    const hint = document.createElement('div');
    hint.className = 'speaker-picker-hint';
    hint.textContent = '🔊 Whose voice is this?';
    menu.appendChild(hint);
    const row = document.createElement('div');
    row.className = 'speaker-picker-listen';
    samples.forEach((s, i) => {
      const b = document.createElement('button');
      b.className = 'speaker-listen-btn';
      b.textContent = `▶ ${i + 1}`;
      b.title = s.text || 'Play sample';
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        invoke('play_audio_segment', {
          recordingId,
          startMs: s.start_ms,
          endMs: s.end_ms,
        }).catch((err) => {
          console.error('play_audio_segment failed:', err);
          showToast('Failed to play sample: ' + err, 'error');
        });
      });
      row.appendChild(b);
    });
    menu.appendChild(row);
  }

  if (people.length) {
    for (const p of people) {
      addItem(p, '', () => {
        close();
        applySpeakerName(recordingId, speakerId, p);
      });
    }
  } else {
    const empty = document.createElement('div');
    empty.className = 'speaker-picker-empty';
    empty.textContent = 'No calendar attendees';
    menu.appendChild(empty);
  }
  addItem('✎ Type a name…', 'speaker-picker-custom', () => {
    close();
    const name = prompt(`Name for "${speakerId}":`, chip.textContent);
    if (name !== null) applySpeakerName(recordingId, speakerId, name.trim());
  });
  addItem(`↺ Reset to ${speakerId}`, 'speaker-picker-reset', () => {
    close();
    applySpeakerName(recordingId, speakerId, '');
  });

  document.body.appendChild(menu);
  // Position under the chip, clamped into the viewport.
  const r = chip.getBoundingClientRect();
  const left = Math.max(8, Math.min(r.left, window.innerWidth - menu.offsetWidth - 8));
  const top = Math.max(8, Math.min(r.bottom + 4, window.innerHeight - menu.offsetHeight - 8));
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(top)}px`;

  // Defer so the click that opened the menu doesn't immediately close it.
  setTimeout(() => {
    document.addEventListener('click', onDoc, true);
    document.addEventListener('keydown', onKey, true);
    window.addEventListener('scroll', close, true);
  }, 0);
}
