import { transcribingIds, queuedTranscriptionIds } from '../core/state.js';

export function getTranscriptionStatus(recordingId) {
  if (transcribingIds.has(recordingId)) return 'Transcribing…';
  if (queuedTranscriptionIds.has(recordingId)) return 'Queued for transcription…';
  return null;
}
