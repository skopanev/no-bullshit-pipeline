import { invoke } from '../core/tauri.js';
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
    onImported: recordings => {
      for (const recording of recordings) {
        transcriptionQueue = transcriptionQueue.then(async () => {
          try {
            await invoke('transcribe_recording', { recordingId: recording.id });
          } catch (error) {
            showToast(`${recording.title}: transcription failed. Retry from the recording. ${String(error)}`, 'error', 7000);
          } finally {
            state.transcribingIds.delete(recording.id);
            await loadRecordings();
            renderRecordingsList();
            if (state.selectedRecordingId === recording.id) await showDetailView(recording.id);
          }
        }).catch(error => console.error('Imported audio refresh failed:', error));
      }
    },
  });

  window.addEventListener('keydown', event => {
    if (event.key === 'Escape') setOverlay(false);
  });
  window.addEventListener('pagehide', () => setOverlay(false));
  try {
    await window.__TAURI__.webview.getCurrentWebview().onDragDropEvent(controller.handleDragDrop);
  } catch (error) {
    console.error('Cannot register audio drag-and-drop:', error);
    showToast('Audio drag-and-drop could not be initialized.', 'warning');
  }
}
