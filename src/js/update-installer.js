// Installation is exclusively an explicit user action. Kept independent of the
// availability checker so startup/focus checks cannot accidentally install.
export function createUpdateInstaller({ status, install, listen, isBusy, onState, id = () => crypto.randomUUID() }) {
  let running = false;
  async function start(version) {
    if (running) return;
    running = true;
    let unlisten;
    onState({ phase: 'checking' });
    try {
      if (isBusy()) throw new Error('Finish recording, imports and transcription before updating.');
      const native = await status();
      if (native.unavailable_reason) throw new Error(native.unavailable_reason);
      if (native.busy || native.installing || isBusy()) throw new Error('Finish current tasks before updating.');
      const requestId = id();
      unlisten = await listen(event => {
        if (event.request_id === requestId) onState(event);
      });
      await install({ expectedVersion: version, requestId });
      onState({ phase: 'restarting' });
    } catch (error) {
      onState({ phase: 'error', message: String(error?.message || error) });
    } finally {
      running = false;
      unlisten?.();
    }
  }
  return { start };
}

export function updateProgressText(progress) {
  switch (progress?.phase) {
    case 'checking': return 'Checking update…';
    case 'downloading': {
      const bytes = Math.max(0, Number(progress.downloaded) || 0);
      if (progress.total > 0) return `Downloading… ${Math.min(100, Math.floor(bytes / progress.total * 100))}%`;
      return `Downloading… ${(bytes / 1048576).toFixed(1)} MB`;
    }
    case 'verifying': return 'Verifying update signature…';
    case 'installing': return 'Installing update…';
    case 'restarting': return 'Restarting…';
    case 'error': return `Update failed: ${progress.message}`;
    default: return '';
  }
}
