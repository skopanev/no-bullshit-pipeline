export function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}

export function formatTime(ms) {
  const totalSeconds = Math.floor(ms / 1000);
  const h = Math.floor(totalSeconds / 3600).toString().padStart(2, '0');
  const m = Math.floor((totalSeconds % 3600) / 60).toString().padStart(2, '0');
  const s = (totalSeconds % 60).toString().padStart(2, '0');
  return `${h}:${m}:${s} `;
}

export function formatDuration(seconds) {
  if (!seconds || seconds <= 0) return '0s';
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds % 60);
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

export function getDuration(rec) {
  const audio = rec.audio || {};
  const info = audio.mix || audio.mic || audio.system;
  return info ? info.duration_sec : 0;
}

export function maskApiKey(key) {
  if (!key || key.length < 8) return key || '';
  return key.slice(0, 4) + '•'.repeat(Math.min(key.length - 8, 20)) + key.slice(-4);
}

export function isKeyMasked(key) {
  return key && key.includes('•');
}
