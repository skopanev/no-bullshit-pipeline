import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { createUpdateChecker, isNewerVersion, UPDATE_CACHE_KEY, UPDATE_CHECK_INTERVAL, UPDATE_RETRY_INTERVAL } from '../src/js/update-checker.js';
import { createUpdateInstaller, updateProgressText } from '../src/js/update-installer.js';

const info = { version: '0.7.9', url: 'https://github.com/skopanev/no-bullshit-pipeline/releases' };
const storage = () => {
  const values = new Map();
  return { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: key => values.delete(key) };
};

test('versions compare numerically and malformed versions are rejected', () => {
  expect(isNewerVersion('v0.7.10', '0.7.9')).toBe(true);
  for (const version of ['0.7.8', '0.7.7', '0.7.9-beta', '0.8', 'garbage']) expect(isNewerVersion(version, '0.7.8')).toBe(false);
});

test('known update survives restart and offline check, then disappears after installation', async () => {
  const disk = storage();
  let visible;
  const options = { currentVersion: '0.7.8', storage: disk, now: () => 1000, onUpdate: value => { visible = value; } };
  await createUpdateChecker({ ...options, check: async () => info }).checkIfDue();
  expect(visible.version).toBe('0.7.9');
  const offline = createUpdateChecker({ ...options, now: () => 1000 + UPDATE_CHECK_INTERVAL, check: async () => { throw new Error('offline'); } });
  expect(visible.version).toBe('0.7.9');
  await offline.checkIfDue();
  expect(visible.version).toBe('0.7.9');
  createUpdateChecker({ ...options, currentVersion: '0.7.9', check: async () => null });
  expect(visible).toBeNull();
  expect(disk.getItem(UPDATE_CACHE_KEY)).toBeNull();
});

test('checks deduplicate focus events, throttle successes and retry offline failures sooner', async () => {
  let now = 1000, calls = 0, fail = true;
  const checker = createUpdateChecker({ currentVersion: '0.7.8', now: () => now, onUpdate() {}, check: async () => {
    calls++;
    if (fail) throw new Error('offline');
    return info;
  } });
  await Promise.all([checker.checkIfDue(), checker.checkIfDue(), checker.checkIfDue()]);
  expect(calls).toBe(1);
  now += UPDATE_RETRY_INTERVAL - 1;
  await checker.checkIfDue();
  expect(calls).toBe(1);
  now++;
  fail = false;
  await checker.checkIfDue();
  expect(calls).toBe(2);
  now += UPDATE_CHECK_INTERVAL - 1;
  await checker.checkIfDue();
  expect(calls).toBe(2);
  now++;
  await checker.checkIfDue();
  expect(calls).toBe(3);
});

test('untrusted cached data is discarded and storage failures do not break checks', async () => {
  const disk = storage();
  disk.setItem(UPDATE_CACHE_KEY, JSON.stringify({ ...info, url: 'https://evil.test/update' }));
  let visible;
  createUpdateChecker({ currentVersion: '0.7.8', storage: disk, check: async () => info, onUpdate: value => { visible = value; } });
  expect(visible).toBeNull();
  const broken = { getItem() { throw new Error(); }, setItem() { throw new Error(); }, removeItem() { throw new Error(); } };
  await createUpdateChecker({ currentVersion: '0.7.8', storage: broken, check: async () => info, onUpdate: value => { visible = value; } }).checkIfDue();
  expect(visible.version).toBe('0.7.9');
});

test('successful withdrawal clears cache; malformed response and network failure do not', async () => {
  const disk = storage();
  let response = info, visible;
  const checker = createUpdateChecker({ currentVersion: '0.7.8', storage: disk, check: async () => response, onUpdate: value => { visible = value; } });
  await checker.checkIfDue();
  response = { ...info, url: 'file:///tmp/install' };
  await checker.checkIfDue(true);
  expect(visible.version).toBe('0.7.9');
  response = null;
  await checker.checkIfDue(true);
  expect(visible).toBeNull();
});

test('installer never starts by construction; blocks frontend and native work', async () => {
  for (const blocked of ['frontend', 'native', 'unpackaged']) {
    let installs = 0, phase;
    const installer = createUpdateInstaller({
      isBusy: () => blocked === 'frontend',
      status: async () => ({ busy: blocked === 'native', unavailable_reason: blocked === 'unpackaged' ? 'Install a release first' : null }),
      install: async () => { installs++; }, listen: async () => () => {}, onState: value => { phase = value; },
    });
    expect(installs).toBe(0);
    await installer.start(info.version);
    expect(installs).toBe(0);
    expect(phase.phase).toBe('error');
  }
});

test('explicit install subscribes first, correlates progress, deduplicates clicks and releases listener', async () => {
  let finish, callback, unsubscribed = 0, installs = 0;
  const phases = [];
  const installer = createUpdateInstaller({
    id: () => 'request-1', isBusy: () => false, status: async () => ({}),
    listen: async cb => { callback = cb; return () => { unsubscribed++; }; },
    install: async args => {
      installs++;
      expect(args).toEqual({ expectedVersion: info.version, requestId: 'request-1' });
      callback({ request_id: 'unrelated', phase: 'installing' });
      callback({ request_id: 'request-1', phase: 'downloading', downloaded: 5, total: 10 });
      await new Promise(resolve => { finish = resolve; });
    },
    onState: value => phases.push(value.phase),
  });
  const first = installer.start(info.version);
  await installer.start(info.version);
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(installs).toBe(1);
  expect(phases).toEqual(['checking', 'downloading']);
  finish();
  await first;
  expect(phases.at(-1)).toBe('restarting');
  expect(unsubscribed).toBe(1);
});

test('failed signature/download leaves actionable error and supports explicit retry', async () => {
  let fail = true, latest, released = 0;
  const installer = createUpdateInstaller({ id: () => 'id', isBusy: () => false, status: async () => ({}),
    listen: async () => () => { released++; },
    install: async () => { if (fail) throw new Error('Signature verification failed'); },
    onState: value => { latest = value; },
  });
  await installer.start(info.version);
  expect(latest).toEqual({ phase: 'error', message: 'Signature verification failed' });
  fail = false;
  await installer.start(info.version);
  expect(latest.phase).toBe('restarting');
  expect(released).toBe(2);
});

test('progress handles unknown lengths and caps percentages', () => {
  expect(updateProgressText({ phase: 'downloading', downloaded: 50, total: 100 })).toContain('50%');
  expect(updateProgressText({ phase: 'downloading', downloaded: 200, total: 100 })).toContain('100%');
  expect(updateProgressText({ phase: 'downloading', downloaded: 1048576 })).toContain('1.0 MB');
  expect(updateProgressText({ phase: 'verifying' })).toContain('signature');
});

test('banner is outside all views, cannot be dismissed and button has no browser route', () => {
  const html = readFileSync(new URL('../src/index.html', import.meta.url), 'utf8');
  const banner = html.slice(html.indexOf('id="update-banner"'), html.indexOf('id="model-update-banner"'));
  expect(banner).toContain('id="update-download-btn"');
  expect(banner).not.toContain('dismiss');
  expect(banner).not.toContain('onclick=');
  const ui = readFileSync(new URL('../src/js/updater.js', import.meta.url), 'utf8');
  expect(ui).not.toContain('openUrl');
  expect(ui).toContain("invoke('install_app_update', args)");
});
