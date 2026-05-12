#!/usr/bin/env node
// Parallel dev runner: esbuild --watch alongside `tauri dev`. When either
// child exits (or this process gets SIGINT/SIGTERM), the other is killed so
// nothing lingers between dev restarts.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const procs = new Map();
let exiting = false;

function killAll(signal = 'SIGTERM') {
  for (const [name, p] of procs) {
    if (p.exitCode != null || p.killed) continue;
    try { p.kill(signal); } catch (e) { console.error(`[dev] kill ${name}:`, e.message); }
  }
}

function start(name, cmd, args) {
  const p = spawn(cmd, args, { stdio: 'inherit', cwd: repoRoot, env: process.env });
  procs.set(name, p);
  p.on('exit', (code, signal) => {
    if (exiting) return;
    exiting = true;
    console.log(`[dev] ${name} exited (code=${code}, signal=${signal}) — shutting down siblings`);
    killAll();
    process.exit(code ?? 0);
  });
}

process.on('SIGINT', () => { exiting = true; killAll('SIGINT'); });
process.on('SIGTERM', () => { exiting = true; killAll('SIGTERM'); });

start('esbuild', 'bun', ['esbuild.mjs', '--watch']);
start('tauri', 'bun', ['tauri', 'dev', '--config', 'src-tauri/tauri.dev.conf.json']);
