// Quick Dictate floating HUD — small always-on-top window showing recording /
// transcribing / processing / paste status with a live mic level meter.
//
// The window itself stays `visible: true` at the Tauri level; visibility is
// controlled here via a CSS class on <body>. When idle, body.hidden gives
// opacity: 0 + pointer-events: none → window is invisible and click-through.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
// Native Window handle — used for startDragging() which talks straight to
// NSWindow. CSS `-webkit-app-region: drag` is unreliable on wry/macOS with
// transparent + non-decorated windows, so we drive the drag from JS instead.
const windowApi = window.__TAURI__.window || {};
const getHud = windowApi.getCurrentWindow || windowApi.getCurrent;
const hud = typeof getHud === 'function' ? getHud() : null;

const body = document.body;
const card = document.getElementById('card');
const dot = document.getElementById('dot');
const statusEl = document.getElementById('status');
const meter = document.getElementById('meter');
const bars = Array.from(meter.querySelectorAll('.bar'));
const hint = document.getElementById('hint');
const actionBtn = document.getElementById('action-btn');

if (card && hud && typeof hud.startDragging === 'function') {
  card.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    // Let interactive controls handle their own clicks
    if (e.target.closest('button, a, input, select, textarea, [data-no-drag]')) return;
    e.preventDefault();
    hud.startDragging().catch((err) => console.error('startDragging failed', err));
  });
}

const BAR_COUNT = bars.length;
const BAR_MIN = 4; // px
const BAR_MAX = 20; // px

console.log('dictation-hud: script loaded');

let levelTimer = null;
let hideTimer = null;
// Cache of shortcut_id → hotkey string so we don't re-query settings on every
// recording start. Populated on demand from the dictation_status payload.
const hotkeyCache = new Map();

function showHud() {
  clearTimeout(hideTimer);
  hideTimer = null;
  body.classList.remove('hidden');
}

function scheduleHide(delayMs) {
  clearTimeout(hideTimer);
  hideTimer = setTimeout(() => {
    body.classList.add('hidden');
  }, delayMs);
}

function startLevelPolling() {
  stopLevelPolling();
  meter.classList.remove('flat');
  levelTimer = setInterval(async () => {
    try {
      const levels = await invoke('get_audio_levels');
      const mic = levels?.mic ?? 0;
      // The Rust side already applies a sqrt-compressed curve (see
      // dictation::push_level), so use mic directly with a small floor.
      const base = Math.max(0.06, mic);
      // Spread the base across N bars with a center-weighted bell curve and a
      // touch of jitter so the visualization feels alive even on steady tone.
      bars.forEach((bar, i) => {
        const center = (BAR_COUNT - 1) / 2;
        const distFromCenter = Math.abs(i - center) / center; // 0 at center, 1 at edges
        const bell = 1 - distFromCenter * 0.55;               // edges shrink ~45%
        const jitter = 0.85 + Math.random() * 0.3;             // ±15%
        const norm = Math.min(1, base * bell * jitter);
        const h = BAR_MIN + norm * (BAR_MAX - BAR_MIN);
        bar.style.height = `${h.toFixed(1)}px`;
        bar.style.opacity = (0.6 + norm * 0.4).toFixed(2);
      });
    } catch (_e) {
      /* level probe failures aren't actionable here */
    }
  }, 70);
}

function stopLevelPolling() {
  if (levelTimer) {
    clearInterval(levelTimer);
    levelTimer = null;
  }
  meter.classList.add('flat');
}

function setActionButton(label, onClick) {
  if (label) {
    actionBtn.textContent = label;
    actionBtn.style.display = '';
    actionBtn.onclick = onClick;
  } else {
    actionBtn.style.display = 'none';
    actionBtn.onclick = null;
  }
}

function setDot(kind) {
  dot.className = `dot ${kind || ''}`;
}

async function fetchCurrentHotkey(shortcutId) {
  try {
    const settings = await invoke('load_settings');
    const list = settings?.dictation?.shortcuts || [];
    const sc = list.find((s) => s.id === shortcutId);
    return sc?.hotkey || null;
  } catch (_e) {
    return null;
  }
}

function prettyHotkey(hk) {
  if (!hk) return '';
  return hk
    .split('+')
    .map((part) => {
      switch (part.toLowerCase()) {
        case 'cmd': case 'meta': case 'super': return '⌘';
        case 'shift': return '⇧';
        case 'alt': case 'option': return '⌥';
        case 'ctrl': case 'control': return '⌃';
        case 'space': return 'Space';
        case 'enter': case 'return': return '↩';
        case 'escape': case 'esc': return 'Esc';
        case 'tab': return '⇥';
        default: return part.length === 1 ? part.toUpperCase() : part;
      }
    })
    .join('');
}

async function onStatus(payload) {
  if (!payload) return;
  const { state, message, shortcut_id } = payload;
  console.log('dictation-hud: state →', state, shortcut_id || '');

  switch (state) {
    case 'recording':
      showHud();
      setDot('recording');
      statusEl.textContent = 'Listening…';
      meter.style.display = '';
      startLevelPolling();
      setActionButton(null);
      // Always (re)show the "<hotkey> again to stop" hint on recording start.
      // Use the cache when we already know the binding to avoid a settings
      // roundtrip on every press of an existing shortcut.
      if (shortcut_id) {
        const cached = hotkeyCache.get(shortcut_id);
        if (cached) {
          hint.textContent = `${prettyHotkey(cached)} again to stop`;
        } else {
          fetchCurrentHotkey(shortcut_id).then((hk) => {
            if (hk) {
              hotkeyCache.set(shortcut_id, hk);
              hint.textContent = `${prettyHotkey(hk)} again to stop`;
            }
          });
        }
      }
      break;
    case 'reading_clipboard':
      showHud();
      stopLevelPolling();
      setDot('processing');
      statusEl.textContent = 'Reading clipboard…';
      meter.style.display = 'none';
      hint.textContent = '';
      setActionButton(null);
      break;
    case 'transcribing':
      showHud();
      stopLevelPolling();
      setDot('processing');
      statusEl.textContent = 'Transcribing…';
      meter.style.display = '';
      hint.textContent = 'one sec';
      setActionButton(null);
      break;
    case 'processing':
      showHud();
      stopLevelPolling();
      setDot('processing');
      statusEl.textContent = 'Processing pipeline…';
      meter.style.display = 'none';
      hint.textContent = 'running LLM steps';
      setActionButton(null);
      break;
    case 'pasting':
      showHud();
      stopLevelPolling();
      setDot('processing');
      statusEl.textContent = 'Pasting…';
      meter.style.display = 'none';
      hint.textContent = '';
      setActionButton(null);
      break;
    case 'accessibility_needed':
      showHud();
      stopLevelPolling();
      setDot('error');
      statusEl.textContent = 'Accessibility needed';
      meter.style.display = 'none';
      hint.textContent = 'Text copied — ⌘V to paste manually';
      setActionButton('Open Settings', async () => {
        try {
          await invoke('open_accessibility_settings');
        } catch (e) {
          console.error('open_accessibility_settings failed', e);
        }
      });
      scheduleHide(8000);
      break;
    case 'pipeline_error':
      showHud();
      stopLevelPolling();
      setDot('error');
      statusEl.textContent = 'Pipeline failed';
      meter.style.display = 'none';
      hint.textContent = message || 'Pasted raw transcript';
      setActionButton(null);
      scheduleHide(4000);
      break;
    case 'error':
      showHud();
      stopLevelPolling();
      setDot('error');
      statusEl.textContent = message || 'Error';
      meter.style.display = 'none';
      hint.textContent = '';
      setActionButton(null);
      scheduleHide(4000);
      break;
    case 'idle':
    default:
      stopLevelPolling();
      setDot('done');
      statusEl.textContent = message || 'Done';
      hint.textContent = '';
      // Cancellation is a user-initiated dismissal — hide instantly instead
      // of the soft 500ms fade used after a normal "Done" completion.
      scheduleHide(message === 'Cancelled' ? 0 : 500);
      break;
  }
}

listen('dictation_status', (event) => {
  try {
    onStatus(event.payload);
  } catch (e) {
    console.error('hud status err', e);
  }
})
  .then(() => console.log('dictation-hud: subscribed to dictation_status'))
  .catch((e) => console.error('dictation-hud: subscribe failed', e));
