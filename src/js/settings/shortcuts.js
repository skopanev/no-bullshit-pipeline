// Quick Dictate shortcuts editor — Settings → Shortcuts tab.
// Manages a list of { id, name, hotkey, engine, whisper_model, pipeline, auto_paste }.

import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { on } from '../core/events.js';
import { DEFAULT_WHISPER_MODEL } from '../core/constants.js';

const enabledToggle = () => document.getElementById('settings-dictation-enabled');
const listEl = () => document.getElementById('dictation-shortcuts-list');
const addBtn = () => document.getElementById('add-dictation-shortcut-btn');
const editor = () => document.getElementById('dictation-shortcut-editor');
const area = () => document.getElementById('dictation-shortcuts-area');
const titleEl = () => document.getElementById('dictation-editor-title');
const closeBtn = () => document.getElementById('close-dictation-editor');
const saveBtn = () => document.getElementById('save-dictation-shortcut-btn');
const deleteBtn = () => document.getElementById('delete-dictation-shortcut-btn');
const nameInput = () => document.getElementById('dict-editor-name');
const hotkeyInput = () => document.getElementById('dict-editor-hotkey');
const inputSourceSel = () => document.getElementById('dict-editor-input-source');
const engineSel = () => document.getElementById('dict-editor-engine');
const engineField = () => document.getElementById('dict-editor-engine-field');
const modelField = () => document.getElementById('dict-editor-model-field');
const modelSel = () => document.getElementById('dict-editor-model');
const pipelineSel = () => document.getElementById('dict-editor-pipeline');
const autoPasteCb = () => document.getElementById('dict-editor-auto-paste');
const axStatusBadge = () => document.getElementById('settings-ax-status-badge');
const axStatusText = () => document.getElementById('settings-ax-status-text');
const axGrantBtn = () => document.getElementById('settings-ax-grant-btn');
const axRestartBtn = () => document.getElementById('settings-ax-restart-btn');

// Track whether the user has clicked Grant in this session. If they did and
// AX still reports as ungranted, we surface the Restart button — macOS caches
// AXIsProcessTrusted per-process, so a restart is the canonical fix.
let axGrantAttempted = false;

let editingId = null; // null = creating new
let registrationStatus = {}; // id → { status, error } from last reload

function ensureDictationConfig() {
  if (!state.appSettings) return;
  if (!state.appSettings.dictation) {
    state.appSettings.dictation = { enabled: false, shortcuts: [] };
  }
  if (!Array.isArray(state.appSettings.dictation.shortcuts)) {
    state.appSettings.dictation.shortcuts = [];
  }
}

function genId() {
  return 'sc-' + Math.random().toString(36).slice(2, 10);
}

function emptyDraft() {
  return {
    id: genId(),
    name: '',
    hotkey: '',
    input_source: 'Audio',
    engine: 'FluidAudio',
    whisper_model: DEFAULT_WHISPER_MODEL,
    device_name: null,
    pipeline: null,
    auto_paste: true,
  };
}

function renderList() {
  ensureDictationConfig();
  const container = listEl();
  if (!container) return;
  const enabled = !!state.appSettings.dictation.enabled;
  const items = state.appSettings.dictation.shortcuts;
  if (items.length === 0) {
    container.innerHTML = enabled
      ? '<div class="shortcut-empty">No shortcuts yet — click <strong>+ New Shortcut</strong>.</div>'
      : '<div class="shortcut-empty">Enable Quick Dictate above to add shortcuts.</div>';
    return;
  }

  // Detect internal duplicates (same hotkey on two shortcuts).
  const dupHotkeys = new Set();
  const seen = new Map();
  for (const sc of items) {
    const k = (sc.hotkey || '').trim().toLowerCase();
    if (!k) continue;
    if (seen.has(k)) dupHotkeys.add(k);
    seen.set(k, sc.id);
  }

  container.innerHTML = items.map((sc) => {
    const inputLabel = sc.input_source === 'Clipboard' ? 'Clipboard' :
      (sc.engine === 'LocalWhisper' ? `Mic · Local Whisper · ${escapeHtml(sc.whisper_model || 'Base')}` : `Mic · ${escapeHtml(sc.engine || '')}`);
    const engineLabel = inputLabel;
    const pipelinePart = sc.pipeline
      ? `<span class="meta-divider">·</span><span class="meta-pipe">→ ${escapeHtml(sc.pipeline)}</span>`
      : '';
    const k = (sc.hotkey || '').trim().toLowerCase();
    const reg = registrationStatus[sc.id];
    let badge = '';
    let rowClass = '';
    if (dupHotkeys.has(k)) {
      badge = `<span class="shortcut-badge bad" title="Duplicate hotkey within your shortcuts">duplicate</span>`;
      rowClass = ' has-error';
    } else if (reg && reg.status === 'error') {
      badge = `<span class="shortcut-badge bad" title="${escapeHtml(reg.error || 'Registration failed')}">conflict</span>`;
      rowClass = ' has-error';
    } else if (reg && reg.status === 'disabled') {
      badge = `<span class="shortcut-badge muted" title="Master toggle is off">off</span>`;
    } else if (reg && reg.status === 'registered') {
      badge = `<span class="shortcut-badge ok" title="Bound globally">active</span>`;
    }
    return `
      <div class="shortcut-row${rowClass}" data-shortcut-id="${escapeHtml(sc.id)}">
        <span class="shortcut-row-kbd">${escapeHtml(sc.hotkey || '—')}</span>
        <div class="shortcut-row-main">
          <div class="shortcut-row-name">${escapeHtml(sc.name || '(unnamed)')}</div>
          <div class="shortcut-row-meta">${engineLabel}${pipelinePart}</div>
        </div>
        ${badge}
      </div>
    `;
  }).join('');
  container.querySelectorAll('.shortcut-row').forEach((row) => {
    row.addEventListener('click', () => openEditor(row.dataset.shortcutId));
  });
}

function escapeHtml(s) {
  if (s == null) return '';
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function prettyModelName(filename) {
  let s = (filename || '').replace(/^ggml-/, '').replace(/\.bin$/, '');
  s = s.replace(/-/g, ' ').replace(/\bv(\d)/, 'v$1');
  return s.charAt(0).toUpperCase() + s.slice(1);
}

function formatSize(bytes) {
  if (!bytes) return '';
  if (bytes >= 1024 * 1024 * 1024) return `~${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
  return `~${Math.round(bytes / 1024 / 1024)} MB`;
}

async function populateModelDropdown(selected) {
  const sel = modelSel();
  if (!sel) return;
  sel.disabled = true;
  try {
    const list = await invoke('list_whisper_models_remote', { limit: 10 });
    const items = Array.isArray(list) ? list : [];
    sel.innerHTML = items.map((m) => {
      const sizeStr = formatSize(m.size_bytes);
      const icon = m.downloaded ? '✓' : '↓';
      const label = `${icon} ${prettyModelName(m.filename)} ${sizeStr}`.trim();
      return `<option value="${escapeHtml(m.filename)}">${escapeHtml(label)}</option>`;
    }).join('');
    if (selected && !items.some((m) => m.filename === selected)) {
      sel.insertAdjacentHTML('beforeend',
        `<option value="${escapeHtml(selected)}">${escapeHtml(prettyModelName(selected))} (current)</option>`);
    }
    if (selected) sel.value = selected;
  } catch (err) {
    console.error('Failed to load whisper models:', err);
    sel.innerHTML = '<option value="ggml-base.bin">Base (~141 MB)</option>';
    if (selected) sel.value = selected;
  } finally {
    sel.disabled = false;
  }
}

async function populatePipelineDropdown(selected) {
  const sel = pipelineSel();
  if (!sel) return;
  sel.innerHTML = '<option value="">— None (paste raw transcript) —</option>';
  try {
    const list = await invoke('list_pipelines'); // Vec<Pipeline>
    const pipelines = Array.isArray(list) ? list : [];
    pipelines
      .map((p) => p && p.name)
      .filter(Boolean)
      .sort((a, b) => a.localeCompare(b))
      .forEach((name) => {
        const opt = document.createElement('option');
        opt.value = name;
        opt.textContent = name;
        if (selected && selected === name) opt.selected = true;
        sel.appendChild(opt);
      });
  } catch (err) {
    console.error('Failed to load pipelines:', err);
  }
}

function syncModelFieldVisibility() {
  const field = modelField();
  if (!field) return;
  field.style.display = engineSel().value === 'LocalWhisper' ? '' : 'none';
}

function syncInputSourceFields() {
  const isAudio = inputSourceSel().value === 'Audio';
  const eField = engineField();
  if (eField) eField.style.display = isAudio ? '' : 'none';
  // Whisper model only matters when engine = LocalWhisper AND input = Audio
  syncModelFieldVisibility();
  if (!isAudio && modelField()) modelField().style.display = 'none';
}

async function refreshAxStatus() {
  const badge = axStatusBadge();
  const text = axStatusText();
  const btn = axGrantBtn();
  const restartBtn = axRestartBtn();
  if (!badge || !btn || !text || !restartBtn) return;
  let granted = false;
  try { granted = await invoke('is_accessibility_granted'); } catch (_) { /* probe failure → treat as not granted */ }
  if (granted) {
    badge.textContent = 'Granted';
    badge.className = 'ax-status ok';
    text.textContent = 'Auto-paste will send ⌘V to the focused app.';
    btn.style.display = 'none';
    restartBtn.style.display = 'none';
    axGrantAttempted = false;
  } else {
    badge.textContent = 'Not granted';
    badge.className = 'ax-status bad';
    btn.style.display = '';
    if (axGrantAttempted) {
      // User already clicked Grant but AXIsProcessTrusted still says no →
      // macOS is caching the stale 'untrusted' state in our process. Restart
      // is the canonical fix.
      text.textContent = 'Permission flipped in System Settings but macOS cached it. Restart NBP to apply.';
      restartBtn.style.display = '';
    } else {
      text.textContent = 'Required for auto-paste (⌘V keystroke into focused app).';
      restartBtn.style.display = 'none';
    }
  }
}

async function handleRequestAx() {
  const btn = axGrantBtn();
  axGrantAttempted = true;
  if (btn) { btn.disabled = true; btn.textContent = 'Requesting…'; }
  try {
    const granted = await invoke('request_accessibility_permission');
    if (!granted) {
      // System prompt was either dismissed or already shown earlier — open
      // Settings as a fallback so the user can flip the toggle manually.
      try { await invoke('open_accessibility_settings'); } catch (_) { /* best-effort fallback */ }
    }
  } catch (e) {
    console.error('Accessibility request failed:', e);
    showToast('Failed to request permission: ' + e, 'error');
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = 'Grant Permission'; }
    await refreshAxStatus();
  }
}

async function handleRestart() {
  try { await invoke('restart_app'); } catch (e) {
    console.error('restart_app failed', e);
    showToast('Restart failed: ' + e, 'error');
  }
}

async function openEditor(id) {
  ensureDictationConfig();
  const list = state.appSettings.dictation.shortcuts;
  const draft = id ? list.find((s) => s.id === id) : null;
  const data = draft ? { ...draft } : emptyDraft();
  editingId = draft ? draft.id : null;

  titleEl().textContent = draft ? 'Edit Shortcut' : 'New Shortcut';
  nameInput().value = data.name || '';
  hotkeyInput().value = data.hotkey || '';
  inputSourceSel().value = data.input_source || 'Audio';
  engineSel().value = data.engine || 'FluidAudio';
  autoPasteCb().checked = data.auto_paste !== false;
  syncInputSourceFields();
  await populateModelDropdown(data.whisper_model || DEFAULT_WHISPER_MODEL);
  await populatePipelineDropdown(data.pipeline);
  deleteBtn().style.display = draft ? '' : 'none';
  editor().style.display = '';
  editor().scrollIntoView({ behavior: 'smooth', block: 'nearest' });
}

function closeEditor() {
  editor().style.display = 'none';
  editingId = null;
}

function updateInteractivity() {
  const enabled = !!(state.appSettings && state.appSettings.dictation && state.appSettings.dictation.enabled);
  const a = area();
  if (a) a.classList.toggle('dictation-disabled', !enabled);
  const ab = addBtn();
  if (ab) ab.disabled = !enabled;
  if (!enabled) closeEditor();
}

async function persistAndReload() {
  try {
    await invoke('save_settings', { settings: state.appSettings });
    const results = await invoke('dictation_reload_shortcuts');
    registrationStatus = {};
    if (Array.isArray(results)) {
      for (const r of results) registrationStatus[r.id] = { status: r.status, error: r.error };
    }
    return { ok: true, results };
  } catch (err) {
    console.error('Failed to save/reload shortcuts:', err);
    showToast('Failed to save shortcuts: ' + err, 'error');
    return { ok: false };
  }
}

async function handleSave() {
  ensureDictationConfig();
  const hotkey = hotkeyInput().value.trim();
  const name = nameInput().value.trim();
  if (!name) { showToast('Name is required', 'error'); return; }
  if (!hotkey) { showToast('Hotkey is required', 'error'); return; }

  // Block internal duplicate: same hotkey assigned to a different shortcut
  const list = state.appSettings.dictation.shortcuts;
  const lower = hotkey.toLowerCase();
  const dup = list.find((s) => s.id !== editingId && (s.hotkey || '').toLowerCase() === lower);
  if (dup) {
    showToast(`Hotkey '${hotkey}' is already used by '${dup.name}'`, 'error');
    return;
  }

  const pipelineVal = pipelineSel().value.trim();
  const inputSource = inputSourceSel().value;
  const draft = {
    id: editingId || genId(),
    name,
    hotkey,
    input_source: inputSource,
    engine: engineSel().value,
    whisper_model: engineSel().value === 'LocalWhisper' ? modelSel().value : null,
    device_name: null,
    pipeline: pipelineVal === '' ? null : pipelineVal,
    auto_paste: !!autoPasteCb().checked,
  };

  if (editingId) {
    const idx = list.findIndex((s) => s.id === editingId);
    if (idx >= 0) list[idx] = draft; else list.push(draft);
  } else {
    list.push(draft);
  }

  // Auto-enable master toggle on first shortcut so the user doesn't wonder
  // why nothing fires. Reflect in UI.
  let autoEnabled = false;
  if (!state.appSettings.dictation.enabled && list.length > 0) {
    state.appSettings.dictation.enabled = true;
    if (enabledToggle()) enabledToggle().checked = true;
    updateInteractivity();
    autoEnabled = true;
  }

  const res = await persistAndReload();
  if (res.ok) {
    closeEditor();
    renderList();
    const reg = registrationStatus[draft.id];
    if (reg && reg.status === 'error') {
      showToast(`Saved, but hotkey '${draft.hotkey}' is taken by another app — pick another`, 'error');
    } else if (autoEnabled) {
      showToast('Shortcut saved · Quick Dictate enabled', 'success');
    } else {
      showToast('Shortcut saved', 'success');
    }
  }
}

async function handleDelete() {
  if (!editingId) return;
  ensureDictationConfig();
  state.appSettings.dictation.shortcuts =
    state.appSettings.dictation.shortcuts.filter((s) => s.id !== editingId);
  const res = await persistAndReload();
  if (res.ok) {
    showToast('Shortcut deleted', 'success');
    closeEditor();
    renderList();
  }
}

async function handleEnabledChange() {
  ensureDictationConfig();
  state.appSettings.dictation.enabled = !!enabledToggle().checked;
  const res = await persistAndReload();
  if (res.ok) {
    updateInteractivity();
    renderList();
  }
}

export async function applyDictationSettings() {
  ensureDictationConfig();
  if (enabledToggle()) enabledToggle().checked = !!state.appSettings.dictation.enabled;
  // Read cached registration status from the backend — the actual register
  // already happened during Rust startup, so we just want the result for the
  // badges. Don't call reload here (it would double-register the shortcuts).
  try {
    const results = await invoke('dictation_get_registration_status');
    registrationStatus = {};
    if (Array.isArray(results)) {
      for (const r of results) registrationStatus[r.id] = { status: r.status, error: r.error };
    }
  } catch (err) {
    console.warn('Failed to query shortcut status:', err);
  }
  updateInteractivity();
  renderList();
  refreshAxStatus();
}

// --- Hotkey capture -------------------------------------------------------
// Translates a KeyboardEvent into a tauri-plugin-global-shortcut accelerator
// string (e.g. "cmd+shift+d", "alt+space", "ctrl+f1"). Layout-independent: uses
// e.code rather than e.key.

const MODIFIER_CODES = new Set([
  'ShiftLeft', 'ShiftRight',
  'ControlLeft', 'ControlRight',
  'AltLeft', 'AltRight',
  'MetaLeft', 'MetaRight',
  'OSLeft', 'OSRight',
]);

const SPECIAL_CODE_MAP = {
  Space: 'space',
  Enter: 'enter',
  Tab: 'tab',
  Backspace: 'backspace',
  Delete: 'delete',
  Insert: 'insert',
  Home: 'home',
  End: 'end',
  PageUp: 'pageup',
  PageDown: 'pagedown',
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  Comma: ',',
  Period: '.',
  Slash: '/',
  Backslash: '\\',
  Semicolon: ';',
  Quote: "'",
  BracketLeft: '[',
  BracketRight: ']',
  Minus: '-',
  Equal: '=',
  Backquote: '`',
};

function codeToHotkeyPart(code) {
  if (!code) return null;
  if (MODIFIER_CODES.has(code)) return null;
  if (SPECIAL_CODE_MAP[code]) return SPECIAL_CODE_MAP[code];
  if (code.startsWith('Key') && code.length === 4) return code.slice(3).toLowerCase();
  if (code.startsWith('Digit') && code.length === 6) return code.slice(5);
  if (/^F\d{1,2}$/.test(code)) return code.toLowerCase();
  if (code.startsWith('Numpad')) {
    const rest = code.slice(6);
    if (/^\d$/.test(rest)) return 'num' + rest;
    return null;
  }
  return null;
}

function captureHotkey(e) {
  // Esc → cancel and clear
  if (e.code === 'Escape') {
    e.preventDefault();
    hotkeyInput().value = '';
    hotkeyInput().blur();
    return;
  }
  e.preventDefault();
  e.stopPropagation();

  const main = codeToHotkeyPart(e.code);
  if (!main) return; // modifier-only press → keep waiting

  const parts = [];
  if (e.metaKey) parts.push('cmd');
  if (e.ctrlKey) parts.push('ctrl');
  if (e.altKey) parts.push('alt');
  if (e.shiftKey) parts.push('shift');
  parts.push(main);

  hotkeyInput().value = parts.join('+');
}

function initHotkeyCapture() {
  const input = hotkeyInput();
  if (!input) return;
  input.addEventListener('keydown', captureHotkey);
  input.addEventListener('focus', () => input.classList.add('recording'));
  input.addEventListener('blur', () => input.classList.remove('recording'));
}

function initHotkeyClear() {
  const btn = document.getElementById('dict-editor-hotkey-clear');
  if (!btn) return;
  btn.addEventListener('click', () => {
    hotkeyInput().value = '';
    hotkeyInput().focus();
  });
}

export function initShortcutsTab() {
  const en = enabledToggle();
  if (en) en.addEventListener('change', handleEnabledChange);

  const ab = addBtn();
  if (ab) ab.addEventListener('click', () => openEditor(null));

  const cb = closeBtn();
  if (cb) cb.addEventListener('click', closeEditor);

  const sb = saveBtn();
  if (sb) sb.addEventListener('click', handleSave);

  const db = deleteBtn();
  if (db) db.addEventListener('click', handleDelete);

  const es = engineSel();
  if (es) es.addEventListener('change', syncInputSourceFields);
  const iss = inputSourceSel();
  if (iss) iss.addEventListener('change', syncInputSourceFields);

  const axb = axGrantBtn();
  if (axb) axb.addEventListener('click', handleRequestAx);

  const axr = axRestartBtn();
  if (axr) axr.addEventListener('click', handleRestart);

  // Re-check Accessibility status when the user comes back from System
  // Settings (focus returns to NBP). Avoids a stale 'Not granted' badge.
  window.addEventListener('focus', () => { refreshAxStatus(); });

  initHotkeyCapture();
  initHotkeyClear();

  // Editor field changes are local until Save — block the container-wide
  // auto-save so in-progress edits don't persist a half-baked shortcut.
  const ed = editor();
  if (ed) ed.addEventListener('change', (e) => e.stopPropagation());

  // Refresh pipeline list when tab activated (in case user added one)
  on('tab:shortcuts', () => { renderList(); });
}
