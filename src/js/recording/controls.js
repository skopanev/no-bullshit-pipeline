import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { setIsRecording, setIsRecordingBusy, setCurrentAssignedPipelines } from '../core/state.js';
import { emit } from '../core/events.js';
import { showToast } from '../ui/toast.js';
import { ViewManager } from '../ui/view-manager.js';
import { startTimer, stopTimer } from './timer.js';
import { startWaveformAnimation, stopWaveformAnimation } from './waveform.js';
import { startLiveTranscript, stopLiveTranscript } from './live-transcript.js';

export async function toggleRecording() {
  if (state.isRecordingBusy) return;

  // If we have a recording selected and not recording, play it
  if (state.selectedRecordingId && !state.isRecording) {
    try {
      await invoke('stop_audio');
      await invoke('play_audio', { recordingId: state.selectedRecordingId });
    } catch (err) {
      console.error('Playback error:', err);
    }
    return;
  }

  if (state.isRecording) {
    await stopRecording();
  } else {
    await startRecording();
  }
}

export function setRecordingUI(recording) {
  const statusIndicator = document.getElementById('status-indicator');
  const recordToggleBtn = document.getElementById('record-toggle-btn');

  if (recording) {
    if (statusIndicator) statusIndicator.className = 'status-recording';
    document.body.classList.add('is-recording-active');
    if (recordToggleBtn) {
      recordToggleBtn.innerHTML = '<svg class="stop-icon" width="11" height="11" viewBox="0 0 11 11" fill="currentColor"><rect width="11" height="11" rx="1.5"/></svg>';
      recordToggleBtn.classList.add('is-active');
      recordToggleBtn.classList.remove('is-play-mode');
      recordToggleBtn.title = 'Stop Recording';
    }
  } else {
    if (statusIndicator) statusIndicator.className = 'status-idle';
    document.body.classList.remove('is-recording-active');
    updateMainButton();
  }
}

export function updateMainButton() {
  const recordToggleBtn = document.getElementById('record-toggle-btn');
  if (!recordToggleBtn) return;
  recordToggleBtn.classList.remove('is-active');

  if (state.selectedRecordingId && !state.isRecording) {
    recordToggleBtn.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg>';
    recordToggleBtn.classList.add('is-play-mode');
    recordToggleBtn.title = 'Play Recording';
  } else {
    recordToggleBtn.innerHTML = '<svg width="11" height="11" viewBox="0 0 11 11" fill="currentColor"><circle cx="5.5" cy="5.5" r="5.5"/></svg>';
    recordToggleBtn.classList.remove('is-play-mode');
    recordToggleBtn.title = 'Start Recording';
  }
}

export async function startRecording() {
  setIsRecordingBusy(true);
  ViewManager.showRecordings();

  const saveMixOnly = state.appSettings?.save_mix_only !== false;
  try {
    const metadata = await invoke('start_recording', { saveMixOnly });
    setIsRecording(true);
    setRecordingUI(true);

    emit('recordings:reload');
    startTimer();
    startWaveformAnimation();
    emit('recording:showDetail', metadata.id);
    startLiveTranscript(metadata.id);

    showToast('Recording started', 'info');
  } catch (error) {
    setIsRecording(false);
    stopTimer();
    stopWaveformAnimation();
    setRecordingUI(false);
    console.error('Failed to start recording:', error);
    showToast('Failed to start: ' + error, 'error');
  } finally {
    setIsRecordingBusy(false);
  }
}

export async function stopRecording() {
  setIsRecordingBusy(true);
  try {
    const currentId = state.selectedRecordingId;
    const stoppedPipelines = [...state.currentAssignedPipelines];

    const detailTitleInput = document.getElementById('detail-title');
    if (detailTitleInput && state.selectedRecordingId) {
      try {
        await invoke('update_title', { recordingId: state.selectedRecordingId, title: detailTitleInput.value });
      } catch (e) {
        console.error('Title sync failed (non-fatal):', e);
      }
    }

    await invoke('stop_recording');
    setIsRecording(false);
    setCurrentAssignedPipelines(new Set());

    showToast('Recording stopped', 'info');

    state.pendingAutoExec.set(currentId, stoppedPipelines);

    stopTimer();
    stopWaveformAnimation();
    stopLiveTranscript();
    setRecordingUI(false);
    emit('pipelines:renderChips');

    emit('recordings:reload');

    if (state.selectedRecordingId === currentId) {
      emit('recording:showDetail', currentId);
      emit('pipelines:renderChips');
    }
  } catch (error) {
    setIsRecording(false);
    setCurrentAssignedPipelines(new Set());
    stopTimer();
    stopWaveformAnimation();
    stopLiveTranscript();
    setRecordingUI(false);
    emit('pipelines:renderChips');
    console.error('Failed to stop:', error);
    if (error && error.includes && error.includes('discarded')) {
      emit('recording:hideDetail');
      emit('recordings:reload');
      showToast('Recording discarded (too short)', 'info');
    } else {
      showToast('Failed to stop: ' + error, 'error');
    }
  } finally {
    setIsRecordingBusy(false);
  }
}
