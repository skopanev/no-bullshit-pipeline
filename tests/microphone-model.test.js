import { expect, test } from 'bun:test';
import { microphoneOptions } from '../src/js/recording/microphone-model.js';

const devices = [{ name: 'AirPods', is_default: true }, { name: 'MacBook Microphone', is_default: false }];

test('settings select preserves a pinned missing microphone without substituting the default', () => {
  const options = microphoneOptions({ devices, selected: 'MacBook Microphone' });
  expect(options.filter(option => option.selected).map(option => option.name)).toEqual(['MacBook Microphone']);
  expect(options[0].label).toContain('AirPods');
  const missing = microphoneOptions({ devices, selected: 'Unplugged USB Mic' }).at(-1);
  expect(missing).toMatchObject({ name: 'Unplugged USB Mic', selected: true, unavailable: true });
  expect(microphoneOptions({ devices, busy: true }).some(option => option.disabled)).toBe(false);
});

test('capture area has no microphone indicator; choice lives only in Recording settings', async () => {
  const html = await Bun.file(new URL('../src/index.html', import.meta.url)).text();
  const header = html.split('</header>')[0];
  expect(header).not.toContain('recording-mic-indicator');
  expect(header).not.toContain('recording-mic-meter');
  expect(header).not.toContain('recording-mic-btn');
  expect(header).not.toContain('recording-mic-popover');
  expect(header).not.toContain('settings-recording-device');
  expect(html).toMatch(/<label for="settings-recording-device">Microphone<\/label>/);
  expect(html).toMatch(/<select id="settings-recording-device" class="settings-select"/);
});

test('settings use a targeted save and no preview/capture commands remain', async () => {
  const js = await Bun.file(new URL('../src/js/recording/microphone.js', import.meta.url)).text();
  expect(js).toContain("event.stopPropagation()");
  expect(js).toContain("invoke('set_recording_microphone'");
  expect(js).toContain("document.createElement('option')");
  expect(js).not.toContain('mic_preview');
  expect(js).not.toContain('paintStatus');
});
