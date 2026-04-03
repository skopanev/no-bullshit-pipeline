import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { escapeHtml } from '../core/utils.js';
import { applyMarkdownRendering } from '../ui/markdown.js';
import { loadRecordings } from './list.js';
import { showDetailView } from './detail.js';

export async function autoTranscribeAndExecute(recordingId, pipelineNames) {
  if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(recordingId);

  const prBtn = document.getElementById('process-btn');
  const detailTranscriptEl = document.getElementById('transcript-content');

  const existingTranscript = await invoke('get_transcript', { recordingId });

  if (existingTranscript) {
    if (detailTranscriptEl && state.selectedRecordingId === recordingId) {
      applyMarkdownRendering(detailTranscriptEl, existingTranscript);
      detailTranscriptEl.classList.remove('empty');
    }
    if (prBtn && state.selectedRecordingId === recordingId) {
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
      prBtn.disabled = false;
      prBtn.style.opacity = '1';
    }
    if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(null);

    await loadRecordings();
    if (state.selectedRecordingId === recordingId) showDetailView(recordingId);

    for (const pipelineName of pipelineNames) {
      try {
        await invoke('execute_pipeline', { recordingId, pipelineName });
      } catch (err) {
        console.error(`Auto-pipeline execution failed for "${pipelineName}":`, err);
      }
      await loadRecordings();
      if (state.selectedRecordingId === recordingId) showDetailView(recordingId);
    }
    return;
  }

  if (prBtn && state.selectedRecordingId === recordingId) {
    prBtn.disabled = true;
    prBtn.innerHTML = '<span class="btn-spinner"></span><span style="font-weight: 600; font-size: 12px;">Auto-transcribing...</span>';
    prBtn.style.opacity = '0.6';
  }

  if (detailTranscriptEl && state.selectedRecordingId === recordingId) {
    detailTranscriptEl.innerHTML = `
      <div class="is-loading transcript-processing-state">
        <div class="loading-spinner transcript-processing-spinner"></div>
        <span class="transcript-processing-text">Processing audio...</span>
      </div>
    `;
    detailTranscriptEl.classList.remove('empty');
  }

  try {
    await invoke('transcribe_recording', { recordingId });
  } catch (err) {
    console.error('Auto-transcription failed:', err);
    if (detailTranscriptEl && state.selectedRecordingId === recordingId) {
      detailTranscriptEl.innerHTML = `
        <div class="transcript-error-state">
          <span class="transcript-error-text">Transcription failed: ${escapeHtml(err)}</span>
          <button class="mini-action-btn transcript-retry-btn" onclick="retryTranscription('${recordingId}')">Retry</button>
        </div>
      `;
      detailTranscriptEl.classList.add('empty');
    }
    if (prBtn && state.selectedRecordingId === recordingId) {
      prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Retry</span>';
      prBtn.disabled = false;
      prBtn.style.opacity = '1';
      prBtn.style.display = '';
    }
    if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(null);
    return;
  }

  if (detailTranscriptEl && state.selectedRecordingId === recordingId) {
    const transcript = await invoke('get_transcript', { recordingId });
    if (transcript) {
      applyMarkdownRendering(detailTranscriptEl, transcript);
      detailTranscriptEl.classList.remove('empty');
    }
  }

  if (prBtn && state.selectedRecordingId === recordingId) {
    prBtn.innerHTML = '<span style="font-weight: 600; font-size: 12px;">Transcribe</span>';
    prBtn.disabled = false;
    prBtn.style.opacity = '1';
  }
  if (window.__NBP_setTranscribingId) window.__NBP_setTranscribingId(null);

  await loadRecordings();
  if (state.selectedRecordingId === recordingId) showDetailView(recordingId);

  for (const pipelineName of pipelineNames) {
    try {
      await invoke('execute_pipeline', { recordingId, pipelineName });
    } catch (err) {
      console.error(`Auto-pipeline execution failed for "${pipelineName}":`, err);
    }
    await loadRecordings();
    if (state.selectedRecordingId === recordingId) showDetailView(recordingId);
  }
}

window.retryTranscription = async function(recordingId) {
  const prBtn = document.getElementById('process-btn');
  if (prBtn) {
    prBtn.click();
  }
};
