import { invoke, listen } from '../core/tauri.js';
import { on } from '../core/events.js';
import * as state from '../core/state.js';
import { showToast } from '../ui/toast.js';
import { microphoneOptions } from './microphone-model.js';

let status = { devices: [], selected: null, active_device: null, busy: false };
let refreshSequence = 0;
let saving = false;
let optionsSignature = '';

const element = id => document.getElementById(id);

function populateSettings() {
  const select = element('settings-recording-device');
  if (!select || saving || document.activeElement === select) return;
  const options = microphoneOptions(status);
  const signature = JSON.stringify(options);
  if (signature !== optionsSignature) {
    const nodes = options.map(option => {
      const node = document.createElement('option');
      node.value = option.name || '';
      node.textContent = option.label;
      return node;
    });
    select.replaceChildren(...nodes);
    optionsSignature = signature;
  }
  select.value = status.selected || '';
}

export async function refreshRecordingMicrophone() {
  const sequence = ++refreshSequence;
  try {
    const next = await invoke('get_recording_input');
    if (sequence !== refreshSequence) return;
    status = next;
    if (state.appSettings) state.appSettings.recording_device_name = status.selected;
    populateSettings();
  } catch (error) {
    console.error('Cannot load recording microphone:', error);
  }
}

export async function initRecordingMicrophone() {
  const select = element('settings-recording-device');
  select?.addEventListener('change', async event => {
    // This preference has a targeted save, separate from bulk settings autosave.
    event.stopPropagation();
    saving = true;
    select.disabled = true;
    try {
      await invoke('set_recording_microphone', { deviceName: select.value || null });
      showToast('Recording microphone saved. Applies to the next recording.', 'success');
    } catch (error) {
      showToast(String(error), 'error');
      // A failed save must restore the actual persisted choice.
      select.value = status.selected || '';
    } finally {
      saving = false;
      select.disabled = false;
      await refreshRecordingMicrophone();
    }
  });
  select?.addEventListener('blur', () => { populateSettings(); });
  on('tab:recording', () => { void refreshRecordingMicrophone(); });
  window.addEventListener('focus', () => { void refreshRecordingMicrophone(); });
  await listen('recording-microphone-changed', async ({ payload }) => {
    if (state.appSettings) state.appSettings.recording_device_name = payload;
    await refreshRecordingMicrophone();
  });
  await refreshRecordingMicrophone();
}
