import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import * as state from '../src/js/core/state.js';

let invokeCalls, notifications, nativeHandler, windowHandlers;
let failImport, failTranscription, failRegistration;
let refreshes;
let importGate;
const overlay = { hidden: true };

mock.module('../src/js/core/tauri.js', () => ({
  invoke: async (command, args) => {
    invokeCalls.push([command, args]);
    if (command === 'import_audio_file') {
      if (importGate) await importGate;
      if (failImport) throw new Error('Cannot decode audio');
      return { id: args.path, title: 'Imported audio', source: 'import' };
    }
    if (command === 'transcribe_recording' && failTranscription) throw new Error('ASR failed');
    return 'Transcript';
  },
}));
mock.module('../src/js/ui/toast.js', () => ({ showToast: (...args) => notifications.push(args) }));
mock.module('../src/js/ui/view-manager.js', () => ({ ViewManager: { showRecordings: () => {} } }));
mock.module('../src/js/recording/list.js', () => ({
  loadRecordings: async () => { refreshes++; },
  renderRecordingsList: () => {},
}));
mock.module('../src/js/recording/detail.js', () => ({ showDetailView: async () => {} }));
const { initAudioImport } = await import('../src/js/recording/audio-import.js');

const oldWindow = globalThis.window;
const oldDocument = globalThis.document;

beforeEach(() => {
  invokeCalls = [];
  notifications = [];
  nativeHandler = null;
  refreshes = 0;
  importGate = null;
  failImport = failTranscription = failRegistration = false;
  windowHandlers = new Map();
  overlay.hidden = true;
  globalThis.document = { getElementById: id => id === 'audio-drop-overlay' ? overlay : null };
  globalThis.window = {
    addEventListener: (name, handler) => windowHandlers.set(name, handler),
    __TAURI__: { webview: { getCurrentWebview: () => ({
      onDragDropEvent: async handler => {
        if (failRegistration) throw new Error('Not available');
        nativeHandler = handler;
        return () => {};
      },
    }) } },
  };
});

afterEach(() => {
  globalThis.window = oldWindow;
  globalThis.document = oldDocument;
  state.setIsRecording(false);
  state.transcribingIds.clear();
  state.pendingImports.clear();
});

test('native file events toggle the overlay; Escape and navigation clear it', async () => {
  await initAudioImport();
  nativeHandler({ payload: { type: 'enter', paths: ['/tmp/a.wav'] } });
  expect(overlay.hidden).toBe(false);
  windowHandlers.get('keydown')({ key: 'Escape' });
  expect(overlay.hidden).toBe(true);
  nativeHandler({ payload: { type: 'over' } });
  expect(overlay.hidden).toBe(false);
  windowHandlers.get('pagehide')();
  expect(overlay.hidden).toBe(true);
  expect(invokeCalls).toEqual([]);
});

test('drop imports and transcribes without resetting a live recording or invoking pipelines', async () => {
  state.setIsRecording(true);
  await initAudioImport();
  await nativeHandler({ payload: { type: 'drop', paths: ['/tmp/demo.wav'] } });
  await Bun.sleep(0);
  expect(invokeCalls).toEqual([
    ['import_audio_file', { path: '/tmp/demo.wav' }],
    ['transcribe_recording', { recordingId: '/tmp/demo.wav' }],
  ]);
  expect(state.isRecording).toBe(true);
  expect(overlay.hidden).toBe(true);
  expect(refreshes).toBe(2);
});

test('import adds no button to the application', async () => {
  const html = await Bun.file(new URL('../src/index.html', import.meta.url)).text();
  expect(html).not.toContain('import-audio-btn');
  expect(html).not.toContain('Import audio</button>');
  await initAudioImport();
  expect(typeof nativeHandler).toBe('function');
});

test('failed import clears the overlay and does not start ASR', async () => {
  failImport = true;
  await initAudioImport();
  nativeHandler({ payload: { type: 'enter' } });
  await nativeHandler({ payload: { type: 'drop', paths: ['/tmp/bad.mp3'] } });
  expect(overlay.hidden).toBe(true);
  expect(invokeCalls).toHaveLength(1);
  expect(notifications.some(([text, type]) => type === 'error' && text.includes('bad.mp3'))).toBe(true);
  expect(state.pendingImports.size).toBe(0);
});

test('a named row is queued immediately and remains visible while decoding', async () => {
  let finish;
  importGate = new Promise(resolve => { finish = resolve; });
  await initAudioImport();
  const importing = nativeHandler({ payload: { type: 'drop', paths: ['/tmp/New Recording 65.qta'] } });
  expect(state.pendingImports.size).toBe(1);
  const [row] = state.pendingImports.values();
  expect(row.title).toBe('New Recording 65.qta');
  expect(row.status).toBe('import_queued');
  await Promise.resolve();
  expect(row.status).toBe('importing');
  expect(state.pendingImports.size).toBe(1);
  finish();
  await importing;
  await Bun.sleep(0);
  expect(state.pendingImports.size).toBe(0);
  expect(refreshes).toBe(2);
});

test('ASR errors remain visible and remove the stale transcribing badge', async () => {
  failTranscription = true;
  state.transcribingIds.add('/tmp/a.wav');
  await initAudioImport();
  await nativeHandler({ payload: { type: 'drop', paths: ['/tmp/a.wav'] } });
  await Bun.sleep(0);
  expect(state.transcribingIds.has('/tmp/a.wav')).toBe(false);
  expect(notifications.some(([text, type]) => type === 'error' && text.includes('transcription failed'))).toBe(true);
});
