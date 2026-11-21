import { invoke, listen } from '../core/tauri.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { ViewManager } from '../ui/view-manager.js';
import { loadRecordings, renderRecordingsList } from './list.js';
import { showDetailView } from './detail.js';
import { createAudioImportController } from './audio-import-controller.js';

export async function initAudioImport() {
  const overlay = document.getElementById('audio-drop-overlay');
  const setOverlay = visible => { overlay.hidden = !visible; };
  // Transcribe sequentially, independently of the import queue. Imports never
  // emit recording_complete (which would reset an active capture and run
  // auto-run pipelines). No external pipeline is launched for dropped files.
  let transcriptionQueue = Promise.resolve();
  const scheduleTranscriptions = recordings => {
    for (const recording of recordings) {
      if (state.queuedTranscriptionIds.has(recording.id) || state.transcribingIds.has(recording.id)) continue;
      state.queuedTranscriptionIds.add(recording.id);
      transcriptionQueue = transcriptionQueue.then(async () => {
        state.queuedTranscriptionIds.delete(recording.id);
        state.transcribingIds.add(recording.id);
        renderRecordingsList();
        try {
          await invoke('transcribe_recording', { recordingId: recording.id });
        } catch (error) {
          showToast(`${recording.title}: transcription failed. Retry from the recording. ${String(error)}`, 'error', 7000);
        } finally {
          state.queuedTranscriptionIds.delete(recording.id);
          state.transcribingIds.delete(recording.id);
          await loadRecordings();
          renderRecordingsList();
          if (state.selectedRecordingId === recording.id) await showDetailView(recording.id);
        }
      }).catch(error => console.error('Imported audio refresh failed:', error));
    }
    renderRecordingsList();
  };
  const controller = createAudioImportController({
    importFile: path => invoke('import_audio_file', { path }),
    refresh: loadRecordings,
    notify: showToast,
    setOverlay,
    onQueued: path => {
      const id = `import-${crypto.randomUUID()}`;
      state.pendingImports.set(id, {
        id, title: path.split(/[\\/]/).pop(), created_at: new Date().toISOString(), status: 'import_queued',
      });
      ViewManager.showRecordings();
      renderRecordingsList();
      return id;
    },
    onStarted: id => {
      state.pendingImports.get(id).status = 'importing';
      renderRecordingsList();
    },
    onFinished: id => {
      state.pendingImports.delete(id);
      renderRecordingsList();
    },
    onImported: scheduleTranscriptions,
  });

  const unlistenNative = await listen('audio-import-native', async ({ payload }) => {
    if (!payload?.id || !payload.phase) return;
    const { id, phase, recording_id: recordingId, title = 'Audio import', error } = payload;
    setOverlay(false);
    if (phase === 'receiving' || phase === 'importing') {
      state.pendingImports.set(id, {
        id, title, created_at: new Date().toISOString(),
        status: phase === 'receiving' ? 'import_receiving' : 'importing',
      });
      ViewManager.showRecordings();
    } else if (phase === 'received' || phase === 'complete' || phase === 'error') {
      state.pendingImports.delete(id);
      if (phase === 'error') showToast(`${title}: ${error || 'Audio import failed.'}`, 'error', 7000);
      if (phase === 'complete' && recordingId) {
        scheduleTranscriptions([{ id: recordingId, title }]);
        await loadRecordings();
      }
    }
    renderRecordingsList();
  });

  window.addEventListener('keydown', event => {
    if (event.key === 'Escape') setOverlay(false);
  });
  window.addEventListener('pagehide', () => { setOverlay(false); unlistenNative(); });
  try {
    await window.__TAURI__.webview.getCurrentWebview().onDragDropEvent(controller.handleDragDrop);
  } catch (error) {
    console.error('Cannot register audio drag-and-drop:', error);
    showToast('Audio drag-and-drop could not be initialized.', 'warning');
  }
}
