import { invoke } from '../core/tauri.js';
import * as state from '../core/state.js';

const WALKTHROUGH_STEPS = [
  { selector: '#pipeline-chip-bar',  title: 'Pipeline Chips',  desc: 'Click any chip to instantly start recording with that pipeline pre-assigned.' },
  { selector: '#record-toggle-btn',  title: 'Record Button',   desc: 'Start or stop recording. When a recording is selected, this plays it back instead.' },
  { selector: '#settings-btn',       title: 'Settings',        desc: 'Configure audio, pipelines, templates, integrations, and appearance.' },
  { selector: '#recordings-list',    title: 'Recordings',      desc: 'All your recordings appear here. Click any recording to open its detail view.' },
];

let walkthroughStep = 0;

export function startWalkthrough() {
  walkthroughStep = 0;
  const overlay = document.getElementById('walkthrough-overlay');
  if (overlay) overlay.style.display = 'flex';
  showWalkthroughStep(0);
}

function showWalkthroughStep(stepIndex) {
  const step = WALKTHROUGH_STEPS[stepIndex];
  if (!step) return;

  const target = document.querySelector(step.selector);
  const spotlight = document.getElementById('walkthrough-spotlight');
  const titleEl = document.getElementById('walkthrough-title');
  const descEl = document.getElementById('walkthrough-desc');
  const stepCounter = document.getElementById('walkthrough-step');
  const prevBtn = document.getElementById('walkthrough-prev');
  const nextBtn = document.getElementById('walkthrough-next');
  const card = document.getElementById('walkthrough-card');

  if (titleEl) titleEl.textContent = step.title;
  if (descEl) descEl.textContent = step.desc;
  if (stepCounter) stepCounter.textContent = (stepIndex + 1) + ' / ' + WALKTHROUGH_STEPS.length;
  if (prevBtn) prevBtn.style.display = stepIndex === 0 ? 'none' : '';
  if (nextBtn) nextBtn.textContent = stepIndex === WALKTHROUGH_STEPS.length - 1 ? 'Done' : 'Next';

  if (target && card && spotlight) {
    const rect = target.getBoundingClientRect();
    if (rect.width > 0 && rect.height > 0) {
      const pad = 8;
      spotlight.style.cssText = `position:fixed;top:${rect.top - pad}px;left:${rect.left - pad}px;width:${rect.width + pad * 2}px;height:${rect.height + pad * 2}px;box-shadow:0 0 0 9999px rgba(0,0,0,0.6);display:block;`;

      const spotlightBottom = rect.bottom + pad + 12;
      const cardHeight = 180;
      const cardWidth = 280;
      card.style.position = 'fixed';
      card.style.left = Math.min(Math.max(8, rect.left - pad), window.innerWidth - cardWidth - 8) + 'px';
      card.style.top = (spotlightBottom + cardHeight > window.innerHeight)
        ? (rect.top - pad - cardHeight - 12) + 'px'
        : spotlightBottom + 'px';
      return;
    }
  }

  if (spotlight) spotlight.style.display = 'none';
  if (card) { card.style.position = 'fixed'; card.style.top = '50%'; card.style.left = '50%'; card.style.transform = 'translate(-50%, -50%)'; }
}

async function finishWalkthrough() {
  const overlay = document.getElementById('walkthrough-overlay');
  if (overlay) overlay.style.display = 'none';
  if (state.appSettings) state.appSettings.walkthrough_completed = true;
  try { await invoke('save_settings', { settings: state.appSettings }); } catch (e) { console.error('Failed to save walkthrough_completed:', e); }
}

export function initWalkthrough() {
  const prevBtn = document.getElementById('walkthrough-prev');
  const nextBtn = document.getElementById('walkthrough-next');
  const skipBtn = document.getElementById('walkthrough-skip');
  const startBtn = document.getElementById('start-walkthrough-btn');

  if (prevBtn) prevBtn.addEventListener('click', () => { if (walkthroughStep > 0) showWalkthroughStep(--walkthroughStep); });
  if (nextBtn) nextBtn.addEventListener('click', () => { if (walkthroughStep >= WALKTHROUGH_STEPS.length - 1) finishWalkthrough(); else showWalkthroughStep(++walkthroughStep); });
  if (skipBtn) skipBtn.addEventListener('click', finishWalkthrough);
  if (startBtn) startBtn.addEventListener('click', startWalkthrough);
}
