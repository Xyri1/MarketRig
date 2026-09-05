import { reactive } from "vue";

export type DaemonEvent = {
  id: string;
  kind: string;
  desk_id?: string;
  occurred_at_ns: number;
  payload?: unknown;
};

type Handler = (event: DaemonEvent) => void;
type Subscription = { kinds: Set<string>; handler: Handler };

const handlers = new Set<Subscription>();
/** Desks whose session asked for the operator; the terminal clears them. */
const attention = reactive(new Map<string, true>());

let socket: WebSocket | null = null;
let cursor: string | null = null;
let port = 0;
let bearer = "";
let backoff = 1_000;
let timer: ReturnType<typeof setTimeout> | null = null;
let stopped = true;

function dispatch(event: DaemonEvent): void {
  cursor = `${event.occurred_at_ns}:${event.id}`;
  if (
    event.kind === "SESSION_ATTENTION" &&
    event.desk_id &&
    (event.payload as { kind?: string } | undefined)?.kind !== "session_start"
  ) {
    attention.set(event.desk_id, true);
  }
  for (const subscription of handlers) {
    if (subscription.kinds.has(event.kind)) subscription.handler(event);
  }
}

function open(): void {
  socket = new WebSocket(`ws://127.0.0.1:${port}/events`);
  socket.onopen = () =>
    socket?.send(
      JSON.stringify(cursor ? { bearer, after: cursor } : { bearer }),
    );
  socket.onmessage = (message) => {
    const frame = JSON.parse(String(message.data)) as
      { tail: string } | DaemonEvent;
    if (typeof (frame as { tail?: unknown }).tail === "string") {
      cursor = (frame as { tail: string }).tail;
      backoff = 1_000;
      return;
    }
    dispatch(frame as DaemonEvent);
  };
  socket.onclose = (closed) => {
    socket = null;
    if (stopped) return;
    // 4408 is the daemon dropping a slow consumer: reconnect at once with the
    // cursor. 4401 and 4400 back off like any other close.
    const wait = closed.code === 4408 ? 0 : backoff;
    backoff = Math.min(backoff * 2, 10_000);
    timer = setTimeout(open, wait);
  };
}

function connect(nextPort: number, nextBearer: string): void {
  port = nextPort;
  bearer = nextBearer;
  stopped = false;
  backoff = 1_000;
  open();
}

function disconnect(): void {
  stopped = true;
  if (timer) clearTimeout(timer);
  timer = null;
  socket?.close();
  socket = null;
}

/** Subscribe a refetch to one kind or several; the return unsubscribes. */
function on(kind: string | string[], handler: Handler): () => void {
  const subscription = {
    kinds: new Set(Array.isArray(kind) ? kind : [kind]),
    handler,
  };
  handlers.add(subscription);
  return () => handlers.delete(subscription);
}

function clearAttention(deskId: string): void {
  attention.delete(deskId);
}

export function useEvents() {
  return { connect, disconnect, on, attention, clearAttention };
}
