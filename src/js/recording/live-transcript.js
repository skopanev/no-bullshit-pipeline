import { invoke, listen } from '../core/tauri.js';
import * as state from '../core/state.js';
import { applyMarkdownRendering } from '../ui/markdown.js';
import { showToast } from '../ui/toast.js';

let liveTranscriptGeneration = 0;
let liveTranscriptUnlisten = null;
let liveErrorUnlisten = null;

function renderLiveTranscript(text) {
  const content = document.getElementById('live-transcript-content');
  if (!content) return;
  if (!text) {
    content.innerHTML = '<span style="color: var(--text-secondary); opacity: 0.5; font-style: italic; font-size: 0.85rem;">Listening...</span>';
    return;
  }
  content.textContent = text;
  content.scrollTop = content.scrollHeight;
}

export async function startLiveTranscript(recordingId) {
  const generation = ++liveTranscriptGeneration;

  if (!state.appSettings?.transcription?.realtime_enabled) return;

  const panel = document.getElementById('live-transcript-panel');
  if (panel) panel.style.display = '';
  renderLiveTranscript('');

  // Clean up previous listeners
  if (liveTranscriptUnlisten) { liveTranscriptUnlisten(); liveTranscriptUnlisten = null; }
  if (liveErrorUnlisten) { liveErrorUnlisten(); liveErrorUnlisten = null; }

  const detailTranscriptEl = document.getElementById('transcript-content');

  const unlisten = await listen('realtime_transcript_updated', (event) => {
    if (liveTranscriptGeneration !== generation) return;
    const { text } = event.payload;
    renderLiveTranscript(text);

    if (detailTranscriptEl && state.selectedRecordingId === recordingId) {
      applyMarkdownRendering(detailTranscriptEl, text);
      detailTranscriptEl.classList.remove('empty');
    }
  });

  const errorUnlisten = await listen('realtime_transcription_error', (event) => {
    if (liveTranscriptGeneration !== generation) return;
    showToast('Live transcription error: ' + event.payload, 'error');
    if (panel) panel.style.display = 'none';
  });

  if (liveTranscriptGeneration !== generation) {
    unlisten();
    errorUnlisten();
    return;
  }
  liveTranscriptUnlisten = unlisten;
  liveErrorUnlisten = errorUnlisten;

  try {
    await invoke('start_realtime_transcription', { recordingId });
  } catch (err) {
    if (liveTranscriptGeneration !== generation) return;
    console.error('Failed to start realtime transcription:', err);
    showToast('Live transcription: ' + err, 'error');
    if (panel) panel.style.display = 'none';
    if (liveTranscriptUnlisten) { liveTranscriptUnlisten(); liveTranscriptUnlisten = null; }
    if (liveErrorUnlisten) { liveErrorUnlisten(); liveErrorUnlisten = null; }
  }
}

export function stopLiveTranscript() {
  liveTranscriptGeneration++;
  if (liveTranscriptUnlisten) { liveTranscriptUnlisten(); liveTranscriptUnlisten = null; }
  if (liveErrorUnlisten) { liveErrorUnlisten(); liveErrorUnlisten = null; }
  const panel = document.getElementById('live-transcript-panel');
  if (panel) panel.style.display = 'none';
}
