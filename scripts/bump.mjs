#!/usr/bin/env node
// Bump version across all project files.
// Usage:
//   node scripts/bump.mjs          — bump patch  (0.4.0 → 0.4.1)
//   node scripts/bump.mjs minor    — bump minor  (0.4.1 → 0.5.0)
//   node scripts/bump.mjs major    — bump major  (0.5.0 → 1.0.0)
//   node scripts/bump.mjs 1.2.3    — set exact version

import { readFileSync, writeFileSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');

const FILES = [
  { path: 'package.json',               type: 'json',  key: 'version' },
  { path: 'src-tauri/tauri.conf.json',  type: 'json',  key: 'version' },
  { path: 'src-tauri/Cargo.toml',       type: 'toml' },
];

function readVersion() {
  const pkg = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf-8'));
  return pkg.version;
}

function bump(current, part) {
  // Exact version override
  if (/^\d+\.\d+\.\d+$/.test(part)) return part;

  const [major, minor, patch] = current.split('.').map(Number);
  switch (part) {
    case 'major': return `${major + 1}.0.0`;
    case 'minor': return `${major}.${minor + 1}.0`;
    case 'patch':
    default:      return `${major}.${minor}.${patch + 1}`;
  }
}

function updateFile(file, newVersion) {
  const fullPath = join(ROOT, file.path);
  let content = readFileSync(fullPath, 'utf-8');

  if (file.type === 'json') {
    const json = JSON.parse(content);
    json[file.key] = newVersion;
    content = JSON.stringify(json, null, 2) + '\n';
  } else if (file.type === 'toml') {
    content = content.replace(
      /^version\s*=\s*"[^"]*"/m,
      `version = "${newVersion}"`
    );
  }

  writeFileSync(fullPath, content);
}

const arg = process.argv[2] || 'patch';
const current = readVersion();
const next = bump(current, arg);

if (current === next) {
  console.log(`Version already ${current}`);
  process.exit(0);
}

for (const file of FILES) {
  updateFile(file, next);
}

console.log(`${current} → ${next}`);
