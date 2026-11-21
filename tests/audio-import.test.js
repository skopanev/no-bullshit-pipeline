import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import * as state from '../src/js/core/state.js';

let invokeCalls, notifications, nativeHandler, windowHandlers;
let failImport, failTranscription, failRegistration;
let refreshes;
let importGate;
let nativeListeners;
let transcriptionGates, transcriptionFailures;
const overlay = { hidden: true };

mock.module('../src/js/core/tauri.js', () => ({
  listen: async (name, callback) => {
    nativeListeners.set(name, callback);
    return () => nativeListeners.delete(name);
  },
  invoke: async (command, args) => {
    invokeCalls.push([command, args]);
    if (command === 'import_audio_file') {
      if (importGate) await importGate;
      if (failImport) throw new Error('Cannot decode audio');
      return { id: args.path, title: 'Imported audio', source: 'import' };
    }
    if (command === 'transcribe_recording') {
      if (transcriptionGates.has(args.recordingId)) await transcriptionGates.get(args.recordingId);
      if (failTranscription || transcriptionFailures.has(args.recordingId)) throw new Error('ASR failed');
    }
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
  nativeListeners = new Map();
  transcriptionGates = new Map();
  transcriptionFailures = new Set();
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
  state.queuedTranscriptionIds.clear();
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
  await initAudioImport();
  await nativeHandler({ payload: { type: 'drop', paths: ['/tmp/a.wav'] } });
  await Bun.sleep(0);
  expect(state.transcribingIds.has('/tmp/a.wav')).toBe(false);
  expect(notifications.some(([text, type]) => type === 'error' && text.includes('transcription failed'))).toBe(true);
});

test('an already active recording is not rescheduled or stripped of its badge', async () => {
  state.transcribingIds.add('saved-active');
  await initAudioImport();
  const receive = nativeListeners.get('audio-import-native');
  await receive({ payload: { id: 'import-active', phase: 'complete', recording_id: 'saved-active' } });
  await Bun.sleep(0);
  expect(invokeCalls).toEqual([]);
  expect([...state.transcribingIds]).toEqual(['saved-active']);
  expect(state.queuedTranscriptionIds.size).toBe(0);
});

test('app-to-app promises show receiving progress and transcribe the already imported file once', async () => {
  await initAudioImport();
  const receive = nativeListeners.get('audio-import-native');
  await receive({ payload: { id: 'promise-1', phase: 'receiving', title: 'Voice Memo.m4a' } });
  expect(state.pendingImports.get('promise-1').status).toBe('import_receiving');
  await receive({ payload: { id: 'import-1', phase: 'importing', title: 'Voice Memo.m4a' } });
  await receive({ payload: { id: 'promise-1', phase: 'received' } });
  expect(state.pendingImports.size).toBe(1);
  await receive({ payload: { id: 'import-1', phase: 'complete', title: 'Voice Memo', recording_id: 'saved-1' } });
  await Bun.sleep(0);
  expect(state.pendingImports.size).toBe(0);
  expect(invokeCalls).toEqual([['transcribe_recording', { recordingId: 'saved-1' }]]);
  expect(refreshes).toBe(2);
});

test('promise export errors remove their row and surface an actionable error', async () => {
  await initAudioImport();
  const receive = nativeListeners.get('audio-import-native');
  await receive({ payload: { id: 'promise-1', phase: 'receiving', title: 'Voice Memo' } });
  await receive({ payload: { id: 'promise-1', phase: 'error', title: 'Voice Memo', error: 'Export failed' } });
  expect(state.pendingImports.size).toBe(0);
  expect(notifications.some(([text, type]) => type === 'error' && text.includes('Export failed'))).toBe(true);
  expect(invokeCalls).toEqual([]);
});

test('68 stays active while 69 is visibly queued, then status moves to 69 by ID', async () => {
  let finish68, finish69;
  transcriptionGates.set('saved-68', new Promise(resolve => { finish68 = resolve; }));
  transcriptionGates.set('saved-69', new Promise(resolve => { finish69 = resolve; }));
  await initAudioImport();
  const receive = nativeListeners.get('audio-import-native');
  await receive({ payload: { id: 'import-68', phase: 'complete', title: 'New Recording 68', recording_id: 'saved-68' } });
  await receive({ payload: { id: 'import-69', phase: 'complete', title: 'New Recording 69', recording_id: 'saved-69' } });
  expect([...state.transcribingIds]).toEqual(['saved-68']);
  expect([...state.queuedTranscriptionIds]).toEqual(['saved-69']);
  expect(invokeCalls).toEqual([['transcribe_recording', { recordingId: 'saved-68' }]]);

  // A repeated completion event must not schedule a second ASR job.
  await receive({ payload: { id: 'import-69', phase: 'complete', recording_id: 'saved-69' } });
  finish68();
  await Bun.sleep(0);
  expect([...state.transcribingIds]).toEqual(['saved-69']);
  expect(state.queuedTranscriptionIds.size).toBe(0);
  expect(invokeCalls).toEqual([
    ['transcribe_recording', { recordingId: 'saved-68' }],
    ['transcribe_recording', { recordingId: 'saved-69' }],
  ]);
  finish69();
  await Bun.sleep(0);
  expect(state.transcribingIds.size).toBe(0);
  expect(state.queuedTranscriptionIds.size).toBe(0);
});

test('a failed ASR clears its status and starts the next queued recording', async () => {
  let finish68, finish69;
  transcriptionGates.set('saved-68', new Promise(resolve => { finish68 = resolve; }));
  transcriptionGates.set('saved-69', new Promise(resolve => { finish69 = resolve; }));
  transcriptionFailures.add('saved-68');
  await initAudioImport();
  const receive = nativeListeners.get('audio-import-native');
  await receive({ payload: { id: 'import-68', phase: 'complete', title: 'New Recording 68', recording_id: 'saved-68' } });
  await receive({ payload: { id: 'import-69', phase: 'complete', title: 'New Recording 69', recording_id: 'saved-69' } });
  finish68();
  await Bun.sleep(0);
  expect([...state.transcribingIds]).toEqual(['saved-69']);
  expect(state.queuedTranscriptionIds.size).toBe(0);
  expect(notifications.some(([text, type]) => type === 'error' && text.includes('New Recording 68'))).toBe(true);
  finish69();
  await Bun.sleep(0);
  expect(state.transcribingIds.size).toBe(0);
});

test('normal file drops use the same queued/active transcription states', async () => {
  let finishFirst, finishSecond;
  transcriptionGates.set('/tmp/first.wav', new Promise(resolve => { finishFirst = resolve; }));
  transcriptionGates.set('/tmp/second.wav', new Promise(resolve => { finishSecond = resolve; }));
  await initAudioImport();
  await nativeHandler({ payload: { type: 'drop', paths: ['/tmp/first.wav', '/tmp/second.wav'] } });
  expect([...state.transcribingIds]).toEqual(['/tmp/first.wav']);
  expect([...state.queuedTranscriptionIds]).toEqual(['/tmp/second.wav']);
  finishFirst();
  await Bun.sleep(0);
  expect([...state.transcribingIds]).toEqual(['/tmp/second.wav']);
  expect(state.queuedTranscriptionIds.size).toBe(0);
  finishSecond();
  await Bun.sleep(0);
  expect(state.transcribingIds.size).toBe(0);
});
