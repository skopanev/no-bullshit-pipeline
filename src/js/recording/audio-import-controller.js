// Native Tauri file events only — DOM drag events also fire for pipeline chips.
export const AUDIO_EXTENSIONS = ['wav', 'mp3', 'm4a', 'aac', 'flac', 'ogg', 'qta'];

export function createAudioImportController({
  importFile, refresh, notify, setOverlay,
  onQueued = () => null, onStarted = () => {}, onFinished = () => {}, onImported = () => {},
}) {
  let queue = Promise.resolve();

  function enqueue(paths) {
    const unique = [...new Set((paths || []).filter(path => typeof path === 'string'))];
    const audio = unique.filter(path => AUDIO_EXTENSIONS.includes(path.split('.').pop().toLowerCase()));
    if (audio.length !== unique.length) {
      notify('Supported audio: WAV, MP3, M4A, AAC, FLAC, OGG, QTA.', 'warning');
    }
    if (!audio.length) return queue;
    const jobs = audio.map(path => ({ path, token: onQueued(path) }));

    queue = queue.then(async () => {
      const imported = [];
      notify(`Importing ${audio.length} audio file${audio.length === 1 ? '' : 's'}…`, 'info');
      for (const { path, token } of jobs) {
        onStarted(token);
        try {
          const recording = await importFile(path);
          await refresh();
          imported.push(recording);
          onImported([recording]);
        } catch (error) {
          const name = path.split(/[\\/]/).pop();
          notify(`${name}: ${String(error)}`, 'error');
        } finally {
          onFinished(token);
        }
      }
      if (imported.length) {
        notify(`Imported ${imported.length} audio file${imported.length === 1 ? '' : 's'}.`, 'success');
      }
    }).catch(error => {
      notify(`Audio import failed: ${String(error)}`, 'error');
    });
    return queue;
  }

  function handleDragDrop({ payload }) {
    if (payload.type === 'enter' || payload.type === 'over') {
      setOverlay(true);
    } else {
      setOverlay(false);
      if (payload.type === 'drop') return enqueue(payload.paths);
    }
  }

  return { enqueue, handleDragDrop };
}
