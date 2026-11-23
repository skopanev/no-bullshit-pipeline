import { expect, test } from 'bun:test';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createManifest, publishRelease } from '../scripts/updater-release.mjs';

test('manifest addresses versioned signed Apple Silicon artifact', () => {
  const manifest = createManifest({ version: '0.7.9', signature: 'a'.repeat(128) + '\n' });
  expect(manifest.platforms['darwin-aarch64'].url).toBe('https://github.com/skopanev/no-bullshit-pipeline/releases/download/v0.7.9/nbp.app.tar.gz');
  expect(manifest.platforms['darwin-aarch64'].signature).toBe('a'.repeat(128));
  expect(() => createManifest({ version: '../x', signature: 'a'.repeat(128) })).toThrow();
  expect(() => createManifest({ version: '0.7.9', signature: '' })).toThrow();
});

test('publishing uploads manifest last and only marks latest after asset verification', () => {
  const temp = mkdtempSync(join(tmpdir(), 'nbp-release-test-'));
  try {
    const files = ['nbp.dmg', 'nbp.app.tar.gz', 'nbp.app.tar.gz.sig', 'latest.json'];
    files.forEach(file => writeFileSync(join(temp, file), 'artifact'));
    const calls = [];
    const gh = args => {
      calls.push(args);
      if (args[0] === 'api') return '[[]]';
      if (args[1] === 'view') return JSON.stringify({ assets: files.map(name => ({ name, size: 8 })) });
      return '';
    };
    publishRelease({ version: '0.7.9', dmg: join(temp, files[0]), archive: join(temp, files[1]), signature: join(temp, files[2]), manifest: join(temp, files[3]), gh });
    expect(calls[1]).toContain('--draft');
    expect(calls[3]).toContain(join(temp, 'latest.json'));
    expect(calls.at(-1)).toContain('--draft=false');
    expect(calls.at(-1)).toContain('--latest');
    expect(() => publishRelease({ version: '0.7.9', gh: () => '[[{"tag_name":"v0.7.9","draft":false}]]' })).toThrow('already published');
    const incomplete = args => args[0] === 'api' ? '[[]]' : args[1] === 'view' ? '{"assets":[]}' : '';
    expect(() => publishRelease({ version: '0.7.9', dmg: join(temp, files[0]), archive: join(temp, files[1]), signature: join(temp, files[2]), manifest: join(temp, files[3]), gh: incomplete })).toThrow('Upload verification failed');
  } finally { rmSync(temp, { recursive: true, force: true }); }
});
