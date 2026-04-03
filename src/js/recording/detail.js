import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { setSelectedRecordingId } from '../core/state.js';
import { formatDuration, getDuration } from '../core/utils.js';
import { emit } from '../core/events.js';
import { looksLikeMarkdown, applyMarkdownRendering } from '../ui/markdown.js';
import { ViewManager } from '../ui/view-manager.js';
import { updateMainButton } from './controls.js';
import { loadRecordings } from './list.js';
import { subscribeToProgress, renderPipelineStatus, cleanupPipelineProgress } from './pipeline-status.js';

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
  updateMainButton();
  ViewManager.showRecordings();
  const detailControlsEl = document.getElementById('detail-controls');
  if (detailControlsEl) detailControlsEl.style.display = 'none';
  cleanupPipelineProgress();
}

export async function showDetailView(id) {
  const rec = state.allRecordings.find(r => r.id === id);
  if (!rec) return;

  setSelectedRecordingId(id);
  clearTranscriptionTimer();
  updateMainButton();

  const detailTitleInput = document.getElementById('detail-title');
  const detailMetaHeaderEl = document.getElementById('detail-meta-header');
  const detailTranscriptEl = document.getElementById('transcript-content');
  const detailStructuredEl = document.getElementById('structured-content');
  const detailControlsEl = document.getElementById('detail-controls');
  const deleteBtnHeader = document.getElementById('delete-btn-header');
  const openFolderBtnHeader = document.getElementById('open-folder-btn-header');
  const prBtn = document.getElementById('process-btn');
  const saveTranscriptBtn = document.getElementById('save-transcript-btn');

  if (detailTitleInput) detailTitleInput.value = rec.title || '';

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

  if (detailControlsEl) detailControlsEl.style.display = 'flex';

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

  // Polling if processing
  if (isProcessing) {
    const pollId = id;
    const pollInterval = setInterval(async () => {
      if (state.selectedRecordingId !== pollId) {
        clearInterval(pollInterval);
        return;
      }
      try {
        await loadRecordings();
        const updated = state.allRecordings.find(r => r.id === pollId);
        if (updated && updated.status !== 'processing') {
          clearInterval(pollInterval);
          showDetailView(pollId);
        }
      } catch (e) {
        console.error('Processing poll error:', e);
        clearInterval(pollInterval);
      }
    }, 1000);
  }

  // Pipeline cards section -- deferred to main.js or pipeline module via emit
  emit('detail:renderPipelineCards', { id, isProcessing });

  // Transcript section visibility
  const hideContent = isProcessing;
  const transcriptSection = document.getElementById('transcript-section');
  if (transcriptSection) transcriptSection.style.display = hideContent ? 'none' : '';

  renderPipelineStatus(id);

  // Load Transcript
  if (!hideContent && detailTranscriptEl) {
    const rawToggle = document.getElementById('transcript-raw-toggle');

    try {
      const isTranscribing = await invoke('is_transcribing', { recordingId: id });
      const transcript = await invoke('get_transcript', { recordingId: id });

      if (transcript) {
        applyMarkdownRendering(detailTranscriptEl, transcript);
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

// Wire back button
const backBtn = document.getElementById('back-btn');
if (backBtn) backBtn.addEventListener('click', hideDetailView);

// Make showDetailView available globally for onclick handlers in HTML
window.showDetailView = showDetailView;
