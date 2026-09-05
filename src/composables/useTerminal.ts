import { shallowReactive } from "vue";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { endpoint } from "../daemon-endpoint";
import { useEvents } from "./useEvents";

type Pane = {
  term: Terminal;
  fit: FitAddon;
  el: HTMLDivElement;
  resize: ResizeObserver;
  socket: WebSocket | null;
  bytes: number;
  reconnected: boolean;
  disposed: boolean;
};

// Reactive so a row reading `panes.has(id)` redraws when a session starts.
const panes = shallowReactive(new Map<string, Pane>());
// Terminals, sockets, and elements live in this module's state: a hot update
// under `tauri dev` must reload the page rather than orphan them on screen.
if (import.meta.hot) import.meta.hot.accept(() => location.reload());
const encoder = new TextEncoder();

/** A token's value; xterm.js validates any CSS colour on a canvas itself. */
const token = (name: string) =>
  getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/** Every frame goes through here: a socket still CONNECTING refuses a send. */
function send(pane: Pane, frame: string | Uint8Array<ArrayBuffer>): void {
  if (pane.socket?.readyState === WebSocket.OPEN) pane.socket.send(frame);
}

function sendResize(pane: Pane): void {
  const dimensions = pane.fit.proposeDimensions();
  // NaN while the element is detached (no parent to measure).
  if (dimensions && Number.isFinite(dimensions.cols))
    send(pane, JSON.stringify({ resize: dimensions }));
}

function openSocket(deskId: string, pane: Pane): void {
  const current = endpoint;
  if (!current) return;
  const socket = new WebSocket(
    `ws://127.0.0.1:${current.port}/desks/${deskId}/terminal`,
  );
  socket.binaryType = "arraybuffer";
  pane.socket = socket;
  socket.onopen = () => {
    socket.send(JSON.stringify({ bearer: current.bearer }));
    sendResize(pane);
  };
  socket.onmessage = (message) => {
    if (typeof message.data === "string") {
      const frame = JSON.parse(message.data) as {
        exited?: { reason: string; code: number | null };
      };
      // Machine surface written into the terminal itself, never localized.
      if (frame.exited) {
        pane.term.write(
          `\r\n\x1b[2mprocess exited (${frame.exited.reason}, ${frame.exited.code})\x1b[0m\r\n`,
        );
      }
      return;
    }
    const bytes = new Uint8Array(message.data as ArrayBuffer);
    pane.bytes += bytes.length;
    pane.term.write(bytes);
  };
  socket.onclose = (closed) => {
    pane.socket = null;
    if (pane.disposed) return;
    // The daemon refusing the attachment is final; a network close while the
    // process lives is a reload path and reattaches once.
    if (closed.code === 4404 || closed.code === 4409) {
      dispose(deskId);
      return;
    }
    if (pane.reconnected) return;
    pane.reconnected = true;
    setTimeout(() => {
      if (panes.get(deskId) === pane && !pane.disposed)
        openSocket(deskId, pane);
    }, 1_000);
  };
}

/** The desk's Terminal, created once and never recreated while it lives. */
function ensure(deskId: string): Pane {
  const existing = panes.get(deskId);
  if (existing) return existing;
  const el = document.createElement("div");
  // The well is dark before xterm.js's own theme paints anything.
  el.className = "bg-well";
  el.style.width = "100%";
  el.style.height = "100%";
  const term = new Terminal({
    // xterm.js defaults to a generic courier stack at 15px.
    fontFamily: token("--font-terminal"),
    fontSize: 13,
    theme: {
      background: token("--color-well"),
      foreground: token("--color-state-idle"),
      cursor: token("--color-accent"),
    },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(el);
  // FitAddon only fits on demand; the well resizes with the window and the
  // panels around it.
  const resize = new ResizeObserver(() => fit.fit());
  resize.observe(el);
  const pane: Pane = {
    term,
    fit,
    el,
    resize,
    socket: null,
    bytes: 0,
    reconnected: false,
    disposed: false,
  };
  panes.set(deskId, pane);
  term.onData((data) => {
    send(pane, encoder.encode(data));
    useEvents().clearAttention(deskId);
  });
  term.onResize(({ cols, rows }) =>
    send(pane, JSON.stringify({ resize: { cols, rows } })),
  );
  openSocket(deskId, pane);
  return pane;
}

/** Takes every other desk's element out of the slot; the Terminals are kept. */
function evict(slot: HTMLElement, keep?: Pane): void {
  for (const pane of panes.values()) {
    if (pane !== keep && pane.el.parentElement === slot) pane.el.remove();
  }
}

/** Moves the desk's own element into the well slot; the Terminal is kept. */
function mount(deskId: string, slot: HTMLElement): void {
  const pane = ensure(deskId);
  evict(slot, pane);
  if (pane.el.parentElement !== slot) slot.appendChild(pane.el);
  pane.fit.fit();
  // fit() only fires onResize when the size changed; the PTY still needs the
  // size the element was first measured at.
  sendResize(pane);
}

function dispose(deskId: string): void {
  const pane = panes.get(deskId);
  if (!pane) return;
  pane.disposed = true;
  panes.delete(deskId);
  pane.socket?.close();
  pane.resize.disconnect();
  pane.fit.dispose();
  pane.term.dispose();
  pane.el.remove();
}

function bytesWritten(deskId: string): number {
  return panes.get(deskId)?.bytes ?? 0;
}

let wired = false;

export function useTerminal() {
  if (!wired) {
    wired = true;
    const { on } = useEvents();
    on("SESSION_STARTED", (event) => event.desk_id && ensure(event.desk_id));
    on("SESSION_EXITED", (event) => event.desk_id && dispose(event.desk_id));
  }
  return { ensure, mount, evict, dispose, bytesWritten, panes };
}
