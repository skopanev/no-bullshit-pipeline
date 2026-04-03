const handlers = new Map();

export function on(event, fn) {
  if (!handlers.has(event)) handlers.set(event, []);
  handlers.get(event).push(fn);
}

export function off(event, fn) {
  const list = handlers.get(event);
  if (!list) return;
  const idx = list.indexOf(fn);
  if (idx >= 0) list.splice(idx, 1);
}

export function emit(event, data) {
  const list = handlers.get(event);
  if (!list) return;
  for (const fn of list) fn(data);
}
