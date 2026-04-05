const handlers = new Map();

export function on(event, fn) {
  if (!handlers.has(event)) handlers.set(event, []);
  handlers.get(event).push(fn);
}

export function emit(event, data) {
  const list = handlers.get(event);
  if (!list) return;
  for (const fn of list) fn(data);
}
