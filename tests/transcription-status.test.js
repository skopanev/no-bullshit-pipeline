import { afterEach, expect, test } from 'bun:test';
import { transcribingIds, queuedTranscriptionIds } from '../src/js/core/state.js';
import { getTranscriptionStatus } from '../src/js/recording/transcription-status.js';

afterEach(() => {
  transcribingIds.clear();
  queuedTranscriptionIds.clear();
});

test('labels are tied to recording IDs, never row positions or selection', () => {
  transcribingIds.add('saved-68');
  queuedTranscriptionIds.add('saved-69');
  expect(getTranscriptionStatus('saved-68')).toBe('Transcribing…');
  expect(getTranscriptionStatus('saved-69')).toBe('Queued for transcription…');
  expect(getTranscriptionStatus('unrelated')).toBeNull();
  transcribingIds.delete('saved-68');
  queuedTranscriptionIds.delete('saved-69');
  transcribingIds.add('saved-69');
  expect(getTranscriptionStatus('saved-68')).toBeNull();
  expect(getTranscriptionStatus('saved-69')).toBe('Transcribing…');
});

test('an active job takes priority and cleared jobs return to idle', () => {
  transcribingIds.add('saved-69');
  queuedTranscriptionIds.add('saved-69');
  expect(getTranscriptionStatus('saved-69')).toBe('Transcribing…');
  transcribingIds.clear();
  queuedTranscriptionIds.clear();
  expect(getTranscriptionStatus('saved-69')).toBeNull();
});
