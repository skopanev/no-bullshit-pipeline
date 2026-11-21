// Shared application state — the single source of truth.
// Feature modules import getters/setters; no circular deps.

export let appSettings = null;
export let allRecordings = [];
export let selectedRecordingId = null;
export let permissions = { mic: false, system_audio: false };
export let isRecording = false;
export let isRecordingBusy = false;
export let currentAssignedPipelines = new Set();
export const pendingAutoExec = new Map();
export let pipelineRunningSteps = {};
/// IDs that currently have a transcription job in flight. Updated by the
/// transcription_progress listener in main.js. Read by list rendering to
/// show "Transcribing…" on the affected row.
export const transcribingIds = new Set();
// Imported recordings waiting for their turn; not actively transcribing yet.
export const queuedTranscriptionIds = new Set();
// Optimistic rows exist only until an external file has finished importing.
export const pendingImports = new Map();

export function setAppSettings(v) { appSettings = v; }
export function setAllRecordings(v) { allRecordings = v; }
export function setSelectedRecordingId(v) { selectedRecordingId = v; }
export function setPermissions(v) { permissions = v; }
export function setIsRecording(v) { isRecording = v; }
export function setIsRecordingBusy(v) { isRecordingBusy = v; }
export function setCurrentAssignedPipelines(v) { currentAssignedPipelines = v; }
export function setPipelineRunningSteps(v) { pipelineRunningSteps = v; }
