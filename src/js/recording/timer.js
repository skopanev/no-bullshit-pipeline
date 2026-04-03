import { formatTime } from '../core/utils.js';
import * as state from '../core/state.js';
import { emit } from '../core/events.js';

let timerInterval = null;
let startTime = null;

export function startTimer() {
  const timerDisplay = document.getElementById('timer');
  const detailMetaHeaderEl = document.getElementById('detail-meta-header');

  startTime = Date.now();
  timerInterval = setInterval(() => {
    const now = Date.now();
    const text = formatTime(now - startTime);
    if (timerDisplay) timerDisplay.textContent = text;

    if (state.isRecording && state.selectedRecordingId) {
      const detailView = document.getElementById('detail-view');
      if (detailView && detailView.style.display !== 'none' && detailMetaHeaderEl) {
        detailMetaHeaderEl.textContent = `Recording... \u00b7 ${text}`;
      }
    }
  }, 100);
}

export function stopTimer() {
  if (timerInterval) {
    clearInterval(timerInterval);
    timerInterval = null;
  }
  startTime = null;
  const timerDisplay = document.getElementById('timer');
  if (timerDisplay) timerDisplay.textContent = '00:00:00';
}
