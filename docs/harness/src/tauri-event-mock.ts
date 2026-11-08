type UnlistenFn = () => void;
type EventHandler<T> = (event: { event: string; id: number; payload: T }) => void;

const handlers = new Map<string, Set<EventHandler<unknown>>>();
let started = false;

function emit(event: string, payload: unknown) {
  for (const handler of handlers.get(event) ?? []) {
    handler({ event, id: 0, payload });
  }
}

function startDemo() {
  if (started) return;
  started = true;

  window.setTimeout(() => emit("hotkey-press", true), 350);
  window.setTimeout(() => emit("hotkey-release", false), 2100);
  window.setTimeout(() => emit("transcribe-done", true), 3800);
}

export async function listen<T>(event: string, handler: EventHandler<T>): Promise<UnlistenFn> {
  const eventHandlers = handlers.get(event) ?? new Set<EventHandler<unknown>>();
  const storedHandler = handler as EventHandler<unknown>;
  eventHandlers.add(storedHandler);
  handlers.set(event, eventHandlers);
  queueMicrotask(startDemo);

  return () => {
    eventHandlers.delete(storedHandler);
  };
}
