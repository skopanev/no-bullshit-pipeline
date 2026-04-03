// Mutable pipeline editor state — use setters to mutate from other modules.

export let allPipelineDefs = [];
export let editingPipelineDef = null;
export let pipelineEditorSteps = [];
export let editingStepIndex = null;
export let sortableInstance = null;
export let lastAutoName = '';
export let modelSelectExpanded = {};

export function setAllPipelineDefs(v) { allPipelineDefs = v; }
export function setEditingPipelineDef(v) { editingPipelineDef = v; }
export function setPipelineEditorSteps(v) { pipelineEditorSteps = v; }
export function setEditingStepIndex(v) { editingStepIndex = v; }
export function setSortableInstance(v) { sortableInstance = v; }
export function setLastAutoName(v) { lastAutoName = v; }
export function setModelSelectExpanded(v) { modelSelectExpanded = v; }
