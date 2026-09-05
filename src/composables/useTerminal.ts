import { shallowReactive } from "vue";
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

// Reactive so a row reading `panes.has(id)` redraws when a session starts.
const panes = shallowReactive(new Map<string, Pane>());
const encoder = new TextEncoder();

/**
 * A token's value in a form ghostty-web parses (it takes `#rrggbb` and
 * `rgb()` only). Reading back `fillStyle` keeps an `oklch()` string verbatim
 * in Chrome, so the colour is painted on a 1x1 canvas and read back as
 * pixels — the browser's own colour engine, no maths here; jsdom has no 2D
 * context and gets `undefined`.
 */
function token(name: string): string | undefined {
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  if (!value) return undefined;
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const ctx = canvas.getContext("2d");
  if (!ctx) return undefined;
  ctx.fillStyle = value;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data;
  return (
    "#" +
    [r, g, b].map((channel) => channel.toString(16).padStart(2, "0")).join("")
  );
}

/** Every frame goes through here: a socket still CONNECTING refuses a send. */
function send(pane: Pane, frame: string | Uint8Array<ArrayBuffer>): void {
  if (pane.socket?.readyState === WebSocket.OPEN) pane.socket.send(frame);
}

function sendResize(pane: Pane): void {
  const dimensions = pane.fit.proposeDimensions();
  if (dimensions) send(pane, JSON.stringify({ resize: dimensions }));
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
  // The well is dark before ghostty-web's own theme paints anything.
  el.className = "bg-well";
  el.style.width = "100%";
  el.style.height = "100%";
  // ghostty-web parks its hidden input textarea at absolute 0,0; without a
  // positioned host that is the window's corner, blinking caret and all.
  el.style.position = "relative";
  const term = new Terminal({
    // ghostty-web's defaults are the browser's generic monospace at 15px.
    fontFamily: getComputedStyle(document.documentElement)
      .getPropertyValue("--font-terminal")
      .trim(),
    fontSize: 13,
    // ghostty-web ignores DECSCUSR and paints its default block over the
    // glyph; a bar reads as a caret and hides nothing.
    cursorStyle: "bar",
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
    send(pane, encoder.encode(data));
    useEvents().clearAttention(deskId);
  });
  term.onResize(({ cols, rows }) =>
    send(pane, JSON.stringify({ resize: { cols, rows } })),
  );
  // The wheel on the alternate screen, the way a terminal does it: SGR mouse
  // reports when the application asked for them (Claude Code does), nothing
  // when it switched alternate scroll off, ghostty-web's own arrow keys
  // otherwise. The normal screen keeps ghostty-web's scrollback scrolling.
  term.attachCustomWheelEventHandler((event) => {
    if (term.buffer.active.type !== "alternate") return false;
    const tracking = [1000, 1002, 1003].some((mode) => term.getMode(mode));
    if (!tracking || !term.getMode(1006)) return !term.getMode(1007);
    const box = term.element?.querySelector("canvas")?.getBoundingClientRect();
    const cell = (offset: number, span: number | undefined, count: number) =>
      span ? Math.min(count, Math.floor((offset / span) * count) + 1) : 1;
    const col = cell(event.clientX - (box?.left ?? 0), box?.width, term.cols);
    const row = cell(event.clientY - (box?.top ?? 0), box?.height, term.rows);
    const button = event.deltaY < 0 ? 64 : 65;
    const notches = Math.min(
      5,
      Math.max(1, Math.round(Math.abs(event.deltaY) / 33)),
    );
    for (let i = 0; i < notches; i++) {
      send(pane, encoder.encode(`[<${button};${col};${row}M`));
    }
    return true;
  });
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
