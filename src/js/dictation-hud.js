// Quick Dictate floating HUD — small always-on-top window showing recording /
// transcribing / processing / paste status with a live mic level meter.
//
// The window itself stays `visible: true` at the Tauri level; visibility is
// controlled here via a CSS class on <body>. When idle, body.hidden gives
// opacity: 0 + pointer-events: none → window is invisible and click-through.

import { watchEffectiveTheme } from './ui/theme-core.js';

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
const partialEl = document.getElementById('partial');

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
let waveTimer = null;
let hideTimer = null;
let statusGeneration = 0;
// Bumped on every meter stop. Async level-probe callbacks capture the value at
// schedule time and bail if it changed — kills late writes that re-stick a bar
// after we've already reset to flat.
let meterGen = 0;
// Cache of shortcut_id → hotkey string so we don't re-query settings on every
// recording start. Populated on demand from the dictation_status payload.
const hotkeyCache = new Map();

// Belt + suspenders + duct tape for focus stealing:
//   1. window.hide() / show()  — NSWindow off-screen entirely when idle
//   2. set_hud_clickthrough     — NSWindow.ignoresMouseEvents (OS-level guard
//                                 even if the window happens to be visible
//                                 during a race or paint warmup)
//   3. body.hidden CSS class    — pointer-events:none + opacity:0 in webview
async function setHudActive(active, generation = statusGeneration) {
  if (!active && generation !== statusGeneration) return;
  try {
    invoke('set_hud_clickthrough', { clickthrough: !active }).catch(() => {});
  } catch (_e) { /* ignore */ }
  // When the HUD truly hides, release the global Esc hook so other apps get
  // their Esc key back. Esc is re-registered automatically on the next
  // recording start in Rust.
  if (!active) {
    try { invoke('dictation_release_esc', { generation }).catch(() => {}); } catch (_e) { /* ignore */ }
  }
  if (!hud) return;
  try {
    if (active) {
      if (typeof hud.show === 'function') await hud.show();
    } else {
      if (typeof hud.hide === 'function') await hud.hide();
    }
  } catch (_e) { /* ignore */ }
}

function showHud() {
  clearTimeout(hideTimer);
  hideTimer = null;
  body.classList.remove('hidden');
  setHudActive(true);
}

function scheduleHide(delayMs) {
  const generation = statusGeneration;
  clearTimeout(hideTimer);
  hideTimer = setTimeout(() => {
    // Safety net: whatever animation/state we were in, a hidden HUD must
    // never keep a timer running or leave a bar stuck (e.g. the centre bar
    // frozen at max from the last level/wave frame on some race path).
    stopAllMeters();
    // Start the CSS opacity fade (body.hidden → opacity 0 over 300ms)...
    body.classList.add('hidden');
    // ...and only hide the NSWindow once the fade has played out — otherwise
    // window.hide() yanks it instantly and the fade is never seen. showHud()
    // clears hideTimer, so a new session mid-fade cancels this cleanly.
    hideTimer = setTimeout(() => setHudActive(false, generation), 300);
  }, delayMs);
}

function startLevelPolling() {
  stopLevelPolling();
  stopWaveAnimation();
  showMeter();
  meter.classList.remove('flat');
  // Re-enable the height transition that resetBars() turned off.
  bars.forEach((bar) => { bar.style.transition = ''; });
  const gen = meterGen;
  levelTimer = setInterval(async () => {
    try {
      const levels = await invoke('get_audio_levels');
      // The probe is async — if the meter was stopped (or restarted) while this
      // call was in flight, the generation no longer matches: bail so a late
      // write can't re-stick the bars after we've moved to processing/flat.
      if (!levelTimer || gen !== meterGen) return;
      const mic = levels?.mic ?? 0;
      const sys = levels?.system ?? 0;
      // Show whichever source is louder so the meter reacts to system audio
      // (calls, video playback) when capture_system_audio is on, and to the
      // mic the rest of the time. Both branches are already sqrt-compressed
      // on the Rust side.
      const base = Math.max(0.06, mic, sys);
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
        bar.style.opacity = (0.8 + norm * 0.2).toFixed(2);
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
}

// Wipe the inline height/opacity the animation loops set per-frame. We clear
// to '' so the bar falls back to the base `.bar { height: 4px }` rule — which
// is already flat — instead of leaving inline values that could linger if the
// `.flat` class ever gets stripped by a racing startLevel/Wave call. Combined
// with the meterGen guard, this kills the leftover "tallest centre bar" dots.
function resetBars() {
  // Disable the height transition for the reset so a tall bar can't leave an
  // animated tail "frozen" on screen when the HUD hides mid-animation — the
  // classic stuck centre-bar dot. Force a reflow to commit the flat values
  // immediately; the animators re-enable the transition when they restart.
  bars.forEach((bar) => {
    bar.style.transition = 'none';
    bar.style.height = `${BAR_MIN}px`;
    bar.style.opacity = '0.5';
  });
  void meter.offsetHeight; // force synchronous reflow
}

// Pulse-wave animation on the same 5 bars used by the live mic meter — a
// single peak sweeps L→R, bars go dark, ~2s pause, repeat. Used while we're
// thinking (transcribing / pipeline / pasting).
function startWaveAnimation() {
  stopWaveAnimation();
  stopLevelPolling();
  showMeter();
  meter.classList.remove('flat');
  // Re-enable the height transition that resetBars() turned off.
  bars.forEach((bar) => { bar.style.transition = ''; });
  const PASS_MS = 700;
  const PAUSE_MS = 1000;
  const CYCLE_MS = PASS_MS + PAUSE_MS;
  const FRAME_MS = 40; // 25 fps
  const startTs = Date.now();
  waveTimer = setInterval(() => {
    const elapsed = (Date.now() - startTs) % CYCLE_MS;
    if (elapsed >= PASS_MS) {
      // Gap between passes — bars sit at min height, dim opacity.
      bars.forEach((bar) => {
        bar.style.height = `${BAR_MIN}px`;
        bar.style.opacity = '0.5';
      });
      return;
    }
    // Wave pass: a single peak travels from bar 0 to bar (N-1), with a
    // gaussian falloff per bar so neighbors light up alongside the peak.
    const t = elapsed / PASS_MS;
    const peakPos = t * (BAR_COUNT - 1);
    bars.forEach((bar, i) => {
      const dist = i - peakPos;
      const amp = Math.exp(-(dist * dist));
      const h = BAR_MIN + amp * (BAR_MAX - BAR_MIN);
      bar.style.height = `${h.toFixed(1)}px`;
      bar.style.opacity = (0.55 + amp * 0.45).toFixed(2);
    });
  }, FRAME_MS);
}

function stopWaveAnimation() {
  if (waveTimer) {
    clearInterval(waveTimer);
    waveTimer = null;
  }
}

function stopAllMeters() {
  // Invalidate any in-flight async level probe so a late resolve can't paint
  // over the reset we're about to do.
  meterGen += 1;
  stopLevelPolling();
  stopWaveAnimation();
  resetBars();
  meter.classList.add('flat');
  // Bulletproof: physically remove the meter from the render tree. Whatever may
  // have stuck in an inline height/opacity or a half-finished transition simply
  // cannot paint a pixel while display:none. Every state that needs the meter
  // explicitly sets display:'' via showMeter() when it starts an animation.
  meter.style.display = 'none';
}

function showMeter() {
  meter.style.display = '';
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

// --- Theme: match the app's auto/light/dark setting --------------------------
// The HUD is a separate window, so it reads the theme from settings.json (same
// `load_settings` it already uses for hotkeys) and applies it via the SHARED
// theme-core helper (no duplicated normalize/media logic — same code the main
// window uses). `auto` follows the macOS appearance via prefers-color-scheme.
let detachHudTheme = null;

function setHudThemeClass(effective) {
  body.classList.remove('dark', 'light');
  body.classList.add(effective);
}

async function applyHudTheme() {
  let theme = 'auto';
  try {
    const settings = await invoke('load_settings');
    theme = settings?.theme;
  } catch (_e) {
    /* fall back to default (dark) styling */
  }
  if (detachHudTheme) detachHudTheme();
  detachHudTheme = watchEffectiveTheme(theme, setHudThemeClass);
}

async function fetchShortcutMeta(shortcutId) {
  try {
    const settings = await invoke('load_settings');
    const list = settings?.dictation?.shortcuts || [];
    const sc = list.find((s) => s.id === shortcutId);
    if (!sc) return null;
    return { hotkey: sc.hotkey || null, trigger_mode: sc.trigger_mode || 'Toggle' };
  } catch (_e) {
    return null;
  }
}

// Build the "how to stop" hint for the recording state. Push-to-talk stops on
// release, so "press again" would be wrong — tell the user to let go instead.
function stopHint(meta) {
  if (!meta) return '';
  if (meta.trigger_mode === 'PushToTalk') {
    return meta.hotkey ? `Release ${prettyHotkey(meta.hotkey)} to stop` : 'Release to stop';
  }
  return meta.hotkey ? `${prettyHotkey(meta.hotkey)} again to stop` : '';
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
  const { state, message, shortcut_id, generation } = payload;
  if (Number.isSafeInteger(generation)) statusGeneration = generation;
  console.log('dictation-hud: state →', state, shortcut_id || '');

  // Refresh the theme at the start of a session in case it changed in Settings.
  if (state === 'recording' || state === 'reading_clipboard') {
    applyHudTheme();
  }

  switch (state) {
    case 'recording':
      showHud();
      setDot('recording');
      statusEl.textContent = 'Listening…';
      startLevelPolling();
      setActionButton(null);
      if (partialEl) { partialEl.textContent = ''; partialEl.classList.remove('visible'); }
      // Show the correct "how to stop" hint — "again to stop" for toggle,
      // "release to stop" for push-to-talk. Use the cache when we already know
      // the binding to avoid a settings roundtrip on every press.
      if (shortcut_id) {
        const cached = hotkeyCache.get(shortcut_id);
        if (cached) {
          hint.textContent = stopHint(cached);
        } else {
          fetchShortcutMeta(shortcut_id).then((meta) => {
            if (meta) {
              hotkeyCache.set(shortcut_id, meta);
              hint.textContent = stopHint(meta);
            }
          });
        }
      }
      break;
    case 'reading_clipboard':
      showHud();
      setDot('processing');
      statusEl.textContent = 'Reading clipboard…';
      startWaveAnimation();
      hint.textContent = '';
      setActionButton(null);
      break;
    case 'transcribing':
      showHud();
      setDot('processing');
      statusEl.textContent = 'Transcribing…';
      startWaveAnimation();
      hint.textContent = 'one sec';
      setActionButton(null);
      break;
    case 'processing':
      showHud();
      setDot('processing');
      statusEl.textContent = 'Processing pipeline…';
      startWaveAnimation();
      hint.textContent = 'running LLM steps';
      setActionButton(null);
      break;
    case 'pasting':
      showHud();
      setDot('processing');
      statusEl.textContent = 'Pasting…';
      startWaveAnimation();
      hint.textContent = '';
      setActionButton(null);
      break;
    case 'accessibility_needed':
      showHud();
      stopAllMeters();
      setDot('error');
      statusEl.textContent = 'Accessibility needed';
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
      stopAllMeters();
      setDot('error');
      statusEl.textContent = 'Pipeline failed';
      hint.textContent = message || 'Pasted raw transcript';
      setActionButton(null);
      scheduleHide(4000);
      break;
    case 'error':
      showHud();
      stopAllMeters();
      setDot('error');
      statusEl.textContent = message || 'Error';
      hint.textContent = '';
      setActionButton(null);
      scheduleHide(4000);
      break;
    case 'idle':
    default:
      stopAllMeters();
      if (partialEl) { partialEl.textContent = ''; partialEl.classList.remove('visible'); }
      if (message) {
        // Informative end states ("No speech detected", "No audio captured", …).
        // Cancellation is a user-initiated dismissal — fade out instantly.
        setDot('done');
        statusEl.textContent = message;
        hint.textContent = '';
        scheduleHide(message === 'Cancelled' ? 0 : 1400);
      } else {
        // Successful completion — no "Done" flash, just fade the HUD out from
        // whatever it was last showing.
        scheduleHide(0);
      }
      break;
  }
}

// Initial state: HUD body starts with class="hidden". Also hide the NSWindow
// and force clickthrough so even a brief webview-paint warmup at startup
// can't grab focus from whatever the user is doing.
setHudActive(false);

listen('dictation_status', (event) => {
  try {
    onStatus(event.payload);
  } catch (e) {
    console.error('hud status err', e);
  }
})
  .then(() => console.log('dictation-hud: subscribed to dictation_status'))
  .catch((e) => console.error('dictation-hud: subscribe failed', e));

// Apply the saved theme on load so the HUD already matches light/dark the first
// time it shows (refreshed on each recording start in case it changed).
applyHudTheme();

// Streaming partials emitted by dictation_streaming.rs as the user talks.
// We render them inline so the user sees their words land in real time.
listen('dictation_partial', (event) => {
  try {
    const text = event?.payload?.text || '';
    if (!partialEl) return;
    partialEl.textContent = text;
    partialEl.classList.toggle('visible', text.length > 0);
  } catch (e) {
    console.error('hud partial err', e);
  }
});
