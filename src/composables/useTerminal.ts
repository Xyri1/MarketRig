import { FitAddon, Terminal } from "ghostty-web";
import { endpoint } from "../daemon-endpoint";
import { useEvents } from "./useEvents";

type Pane = {
  term: Terminal;
  fit: FitAddon;
  el: HTMLDivElement;
  socket: WebSocket | null;
  bytes: number;
  reconnected: boolean;
  disposed: boolean;
};

const panes = new Map<string, Pane>();
const encoder = new TextEncoder();

/**
 * A token's value in a form ghostty-web parses (it takes `#rrggbb` and
 * `rgb()` only). The canvas is the browser's own colour engine, so oklch
 * tokens need no maths here; jsdom has no 2D context and gets `undefined`.
 */
function token(name: string): string | undefined {
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  if (!value) return undefined;
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return undefined;
  ctx.fillStyle = "#000000";
  ctx.fillStyle = value;
  const painted = String(ctx.fillStyle);
  if (painted.startsWith("#") || painted.startsWith("rgb(")) return painted;
  const srgb = painted.match(/color\(srgb ([\d.]+) ([\d.]+) ([\d.]+)/);
  return srgb
    ? "#" +
        srgb
          .slice(1, 4)
          .map((c) =>
            Math.round(Number(c) * 255)
              .toString(16)
              .padStart(2, "0"),
          )
          .join("")
    : undefined;
}

function sendResize(pane: Pane): void {
  const dimensions = pane.fit.proposeDimensions();
  if (dimensions && pane.socket?.readyState === WebSocket.OPEN) {
    pane.socket.send(JSON.stringify({ resize: dimensions }));
  }
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
  el.style.width = "100%";
  el.style.height = "100%";
  const term = new Terminal({
    theme: {
      background: token("--color-well"),
      foreground: token("--color-state-idle"),
      cursor: token("--color-accent"),
    },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(el);
  fit.observeResize();
  const pane: Pane = {
    term,
    fit,
    el,
    socket: null,
    bytes: 0,
    reconnected: false,
    disposed: false,
  };
  panes.set(deskId, pane);
  term.onData((data) => {
    pane.socket?.send(encoder.encode(data));
    useEvents().clearAttention(deskId);
  });
  term.onResize(({ cols, rows }) =>
    pane.socket?.send(JSON.stringify({ resize: { cols, rows } })),
  );
  openSocket(deskId, pane);
  return pane;
}

/** Moves the desk's own element into the well slot; the Terminal is kept. */
function mount(deskId: string, slot: HTMLElement): void {
  const pane = ensure(deskId);
  if (pane.el.parentElement !== slot) slot.appendChild(pane.el);
  pane.fit.fit();
}

function dispose(deskId: string): void {
  const pane = panes.get(deskId);
  if (!pane) return;
  pane.disposed = true;
  panes.delete(deskId);
  pane.socket?.close();
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
  return { ensure, mount, dispose, bytesWritten, panes };
}
