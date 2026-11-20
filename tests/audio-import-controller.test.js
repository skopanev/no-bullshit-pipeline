import { describe, expect, test } from 'bun:test';
import { createAudioImportController } from '../src/js/recording/audio-import-controller.js';

function setup(overrides = {}) {
  const calls = { imports: [], overlays: [], messages: [], batches: [], refreshes: 0 };
  const controller = createAudioImportController({
    importFile: async path => { calls.imports.push(path); return { id: path }; },
    refresh: async () => { calls.refreshes++; },
    notify: (...message) => calls.messages.push(message),
    setOverlay: visible => calls.overlays.push(visible),
    onImported: batch => calls.batches.push(batch),
    ...overrides,
  });
  return { controller, calls };
}

describe('audio file drag-and-drop', () => {
  test('shows blur on enter/over and clears it on leave or drop', async () => {
    const { controller, calls } = setup();
    for (const type of ['enter', 'over', 'leave', 'enter', 'drop']) {
      await controller.handleDragDrop({ payload: { type, paths: ['/tmp/voice.wav'] } });
    }
    expect(calls.overlays).toEqual([true, true, false, true, false]);
    expect(calls.imports).toEqual(['/tmp/voice.wav']);
    expect(calls.refreshes).toBe(1);
  });

  test('deduplicates paths, accepts uppercase and ignores non-audio', async () => {
    const { controller, calls } = setup();
    await controller.enqueue(['/tmp/Запись 1.WAV', '/tmp/Запись 1.WAV', '/tmp/a.m4a', '/tmp/no.txt', null]);
    expect(calls.imports).toEqual(['/tmp/Запись 1.WAV', '/tmp/a.m4a']);
    expect(calls.messages.some(([, type]) => type === 'warning')).toBe(true);
    expect(calls.batches.flat()).toHaveLength(2);
  });

  test('continues a mixed batch after a corrupt file fails', async () => {
    const { controller, calls } = setup({
      importFile: async path => {
        if (path.endsWith('bad.mp3')) throw new Error('Damaged audio');
        return { id: path };
      },
    });
    await controller.enqueue(['/tmp/bad.mp3', '/tmp/good.flac']);
    expect(calls.messages.some(([message, type]) => type === 'error' && message.includes('bad.mp3'))).toBe(true);
    expect(calls.batches).toEqual([[{ id: '/tmp/good.flac' }]]);
    expect(calls.refreshes).toBe(1);
  });

  test('serializes consecutive drops', async () => {
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    const order = [];
    const { controller, calls } = setup({
      importFile: async path => {
        order.push(path);
        if (path === '/tmp/first.ogg') await gate;
        return { id: path };
      },
    });
    const first = controller.enqueue(['/tmp/first.ogg']);
    const second = controller.enqueue(['/tmp/second.aac']);
    await Promise.resolve();
    expect(order).toEqual(['/tmp/first.ogg']);
    release();
    await first;
    await second;
    expect(order).toEqual(['/tmp/first.ogg', '/tmp/second.aac']);
  });

  test('empty and invalid-only drops never start an import', async () => {
    const { controller, calls } = setup();
    await controller.enqueue(null);
    await controller.enqueue([]);
    await controller.enqueue(['/tmp/document.pdf']);
    expect(calls.imports).toEqual([]);
    expect(calls.refreshes).toBe(0);
  });

  test('a failed refresh cannot poison subsequent imports', async () => {
    let attempts = 0;
    const { controller, calls } = setup({
      refresh: async () => { if (++attempts === 1) throw new Error('refresh failed'); },
    });
    await controller.enqueue(['/tmp/a.wav']);
    await controller.enqueue(['/tmp/b.wav']);
    expect(calls.imports).toEqual(['/tmp/a.wav', '/tmp/b.wav']);
    expect(calls.batches).toEqual([[{ id: '/tmp/b.wav' }]]);
  });

  test('accepts Voice Memos QTA filenames with spaces and uppercase extensions', async () => {
    const { controller, calls } = setup();
    await controller.handleDragDrop({ payload: { type: 'drop', paths: ['/tmp/New Recording 65.qta', '/tmp/Voice.QTA'] } });
    expect(calls.imports).toEqual(['/tmp/New Recording 65.qta', '/tmp/Voice.QTA']);
    expect(calls.messages.some(([, type]) => type === 'warning')).toBe(false);
  });
});
