#!/usr/bin/env node
// Build (or rebuild) the FluidAudio Swift sidecar and stage it in
// src-tauri/binaries/. The binary is .gitignored — it has to be produced
// locally on every fresh checkout or after `bun run clean`. Skips the build
// when the binary is already up-to-date relative to the swift sources.

import { spawnSync } from 'node:child_process';
import { existsSync, copyFileSync, mkdirSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve, join } from 'node:path';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const swiftDir = join(repoRoot, 'fluidaudio-sidecar');
const swiftSourcesDir = join(swiftDir, 'Sources');
const swiftPackage = join(swiftDir, 'Package.swift');
const builtBinary = join(swiftDir, '.build', 'release', 'fluidaudio-sidecar');
const stagedBinary = join(
  repoRoot,
  'src-tauri',
  'binaries',
  'fluidaudio-sidecar-aarch64-apple-darwin',
);

function latestMtime(path) {
  const st = statSync(path, { throwIfNoEntry: false });
  if (!st) return 0;
  if (st.isFile()) return st.mtimeMs;
  let max = st.mtimeMs;
  for (const entry of readdirSync(path)) {
    max = Math.max(max, latestMtime(join(path, entry)));
  }
  return max;
}

const force = process.argv.includes('--force');
const stagedMtime = existsSync(stagedBinary) ? statSync(stagedBinary).mtimeMs : 0;
const sourceMtime = Math.max(latestMtime(swiftSourcesDir), latestMtime(swiftPackage));

if (!force && stagedMtime > 0 && stagedMtime >= sourceMtime) {
  console.log('[sidecar] up-to-date, skipping rebuild');
  process.exit(0);
}

console.log('[sidecar] building fluidaudio-sidecar (release) …');
const build = spawnSync('swift', ['build', '-c', 'release'], {
  cwd: swiftDir,
  stdio: 'inherit',
});
if (build.status !== 0) {
  console.error('[sidecar] swift build failed');
  process.exit(build.status ?? 1);
}

if (!existsSync(builtBinary)) {
  console.error(`[sidecar] expected binary missing: ${builtBinary}`);
  process.exit(1);
}

mkdirSync(dirname(stagedBinary), { recursive: true });
copyFileSync(builtBinary, stagedBinary);
console.log(`[sidecar] staged ${stagedBinary}`);
