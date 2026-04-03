import { invoke } from '../core/tauri.js';

let waveformInterval = null;
let displayMicLevel = 0;
let displaySystemLevel = 0;

function getWaveformCanvas() {
  return document.getElementById('recording-waveform-canvas');
}

export function startWaveformAnimation() {
  const canvas = getWaveformCanvas();
  const ctx = canvas ? canvas.getContext('2d') : null;
  if (!canvas || !ctx) return;

  displayMicLevel = 0;
  displaySystemLevel = 0;

  waveformInterval = setInterval(async () => {
    try {
      const levels = await invoke('get_audio_levels');

      const ampMic = Math.min(1.0, levels.mic * 6);
      const ampSys = Math.min(1.0, levels.system * 6);

      displayMicLevel = ampMic > displayMicLevel ? ampMic : Math.max(0, displayMicLevel - 0.06);
      displaySystemLevel = ampSys > displaySystemLevel ? ampSys : Math.max(0, displaySystemLevel - 0.06);

      drawSpectrum();
    } catch (e) {
      // Ignore errors
    }
  }, 30);
}

export function stopWaveformAnimation() {
  if (waveformInterval) {
    clearInterval(waveformInterval);
    waveformInterval = null;
  }
  displayMicLevel = 0;
  displaySystemLevel = 0;
  const canvas = getWaveformCanvas();
  const ctx = canvas ? canvas.getContext('2d') : null;
  if (ctx && canvas) {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
  }
}

function drawSpectrum() {
  const canvas = getWaveformCanvas();
  const ctx = canvas ? canvas.getContext('2d') : null;
  if (!ctx || !canvas) return;

  const style = getComputedStyle(document.documentElement);
  const micColor = style.getPropertyValue('--accent').trim() || '#a855f7';
  const sysColor = style.getPropertyValue('--success').trim() || '#10b981';

  const width = canvas.width;
  const height = canvas.height;

  ctx.clearRect(0, 0, width, height);

  const NUM_BARS = 5;
  const barW = Math.floor(width / NUM_BARS) - 2;
  const barGap = 2;
  const halfH = height / 2;
  const maxBarH = halfH * 0.85;
  const multipliers = [0.6, 0.9, 1.0, 0.9, 0.6];

  for (let i = 0; i < NUM_BARS; i++) {
    const x = i * (barW + barGap) + barGap;

    const micH = Math.max(2, displayMicLevel * multipliers[i] * maxBarH);
    ctx.fillStyle = micColor;
    ctx.fillRect(x, halfH - micH, barW, micH);

    const sysH = Math.max(2, displaySystemLevel * multipliers[i] * maxBarH);
    ctx.fillStyle = sysColor;
    ctx.fillRect(x, halfH, barW, sysH);
  }
}
