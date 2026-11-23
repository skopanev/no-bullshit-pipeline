// Persistent availability, not installation: startup/focus may only CHECK.
// The known update survives restarts and offline checks until it is installed
// or a successful check confirms that it is no longer offered.
export const UPDATE_CHECK_INTERVAL = 24 * 60 * 60 * 1000;
export const UPDATE_RETRY_INTERVAL = 5 * 60 * 1000;
export const UPDATE_CACHE_KEY = 'nbp.availableUpdate.v1';
export const UPDATE_CHECK_KEY = 'nbp.updateCheckState.v1';

function versionParts(version) {
  const match = typeof version === 'string' && /^v?(\d+)\.(\d+)\.(\d+)$/.exec(version);
  if (!match) return null;
  const parts = match.slice(1).map(Number);
  return parts.every(Number.isSafeInteger) ? parts : null;
}

export function isNewerVersion(candidate, current) {
  const next = versionParts(candidate), installed = versionParts(current);
  if (!next || !installed) return false;
  for (let index = 0; index < 3; index++) {
    if (next[index] !== installed[index]) return next[index] > installed[index];
  }
  return false;
}

function validUpdate(info, current) {
  if (!info || !isNewerVersion(info.version, current)) return null;
  try {
    const url = new URL(info.url);
    if (url.protocol !== 'https:' || url.hostname !== 'github.com' || url.port || url.username || url.password ||
        !/^\/skopanev\/no-bullshit-pipeline\/releases(?:\/|$)/.test(url.pathname)) return null;
    return { version: info.version, current, url: url.href };
  } catch { return null; }
}

export function createUpdateChecker({ currentVersion, check, onUpdate, storage, now = Date.now }) {
  const read = key => { try { return JSON.parse(storage?.getItem(key) || 'null'); } catch { return null; } };
  const write = (key, value) => { try {
    if (value === null) storage?.removeItem(key);
    else storage?.setItem(key, JSON.stringify(value));
  } catch { /* The in-memory state still works when storage is unavailable. */ } };
  let available = validUpdate(read(UPDATE_CACHE_KEY), currentVersion);
  const saved = read(UPDATE_CHECK_KEY);
  let checkedAt = saved?.current === currentVersion && Number.isFinite(saved.at) && saved.at > 0 && saved.at <= now() ? saved.at : 0;
  let failedAt = null;
  let checking = null;
  write(UPDATE_CACHE_KEY, available);
  onUpdate(available);

  function checkIfDue(force = false) {
    if (checking) return checking;
    const time = now();
    if (!force && checkedAt && time >= checkedAt && time - checkedAt < UPDATE_CHECK_INTERVAL) return Promise.resolve();
    if (!force && failedAt !== null && time >= failedAt && time - failedAt < UPDATE_RETRY_INTERVAL) return Promise.resolve();
    checking = Promise.resolve().then(check).then(info => {
      const next = validUpdate(info, currentVersion);
      if (info !== null && !next) throw new Error('Invalid update information');
      available = next;
      checkedAt = now();
      failedAt = null;
      write(UPDATE_CACHE_KEY, available);
      write(UPDATE_CHECK_KEY, { current: currentVersion, at: checkedAt });
      onUpdate(available);
    }).catch(() => {
      // Never erase a known update or throttle an offline failure for 24h.
      failedAt = now();
    }).finally(() => { checking = null; });
    return checking;
  }

  return { checkIfDue };
}
