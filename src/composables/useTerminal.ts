import { shallowReactive } from "vue";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
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
  opened: boolean;
  ready: boolean;
  terminalId: string | null;
  offset: bigint;
  pending: number;
  /** ConPTY: hide the caret while output is mid-burst; paint it only at rest. */
  conpty: boolean;
  cursorParked: boolean;
  cursorRestore: ReturnType<typeof setTimeout> | null;
};

// Reactive so a row reading `panes.has(id)` redraws when a session starts.
const panes = shallowReactive(new Map<string, Pane>());
// Terminals, sockets, and elements live in this module's state: a hot update
// under `tauri dev` must reload the page rather than orphan them on screen.
if (import.meta.hot) import.meta.hot.accept(() => location.reload());
const encoder = new TextEncoder();
// Local xterm only — never sent to the PTY. ConPTY re-emits CUP mid-redraw;
// painting those positions makes the caret chase Codex/Claude status rows.
const HIDE_CURSOR = new Uint8Array([0x1b, 0x5b, 0x3f, 0x32, 0x35, 0x6c]);
const SHOW_CURSOR = new Uint8Array([0x1b, 0x5b, 0x3f, 0x32, 0x35, 0x68]);
// Longer than a Codex status-tick gap: restoring sooner re-shows the caret on
// the last CUP cell (often the status row) between frames.
const CURSOR_RESTORE_MS = 250;

/** A token's value; xterm.js validates any CSS colour on a canvas itself. */
const token = (name: string) =>
  getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/** Last DECTCEM in the chunk, if any: true when the TUI asked to stay hidden. */
function lastDectcemHidden(bytes: Uint8Array): boolean | null {
  let hidden: boolean | null = null;
  for (let i = 0; i + 5 < bytes.length; i++) {
    if (
      bytes[i] === 0x1b &&
      bytes[i + 1] === 0x5b &&
      bytes[i + 2] === 0x3f &&
      bytes[i + 3] === 0x32 &&
      bytes[i + 4] === 0x35 &&
      (bytes[i + 5] === 0x6c || bytes[i + 5] === 0x68)
    ) {
      hidden = bytes[i + 5] === 0x6c;
      i += 5;
    }
  }
  return hidden;
}

/** Writes PTY bytes into xterm; on ConPTY parks the caret until output settles. */
function writeOutput(pane: Pane, bytes: Uint8Array, done?: () => void): void {
  if (!pane.conpty) {
    pane.term.write(bytes, done);
    return;
  }
  // Codex ends sync frames with ?25h at a transient CUP cell (codex#39710).
  // Only those visible frame-ends need a trailing host hide; plain idle output
  // and app-hidden frames must stay untouched so the steady idle caret remains.
  const lastHidden = lastDectcemHidden(bytes);
  if (lastHidden === true) {
    pane.cursorParked = true;
    pane.term.write(bytes, done);
    if (pane.cursorRestore !== null) clearTimeout(pane.cursorRestore);
    pane.cursorRestore = null;
    return;
  }
  if (lastHidden !== false && !pane.cursorParked) {
    pane.term.write(bytes, done);
    return;
  }
  const payload = new Uint8Array(bytes.length + HIDE_CURSOR.length);
  payload.set(bytes);
  payload.set(HIDE_CURSOR, bytes.length);
  pane.cursorParked = true;
  pane.term.write(payload, done);
  if (pane.cursorRestore !== null) clearTimeout(pane.cursorRestore);
  pane.cursorRestore = setTimeout(() => releaseCursor(pane), CURSOR_RESTORE_MS);
}

/** Show the caret again after a ConPTY redraw burst (or on user input). */
function releaseCursor(pane: Pane): void {
  if (pane.cursorRestore !== null) clearTimeout(pane.cursorRestore);
  pane.cursorRestore = null;
  if (!pane.cursorParked || pane.disposed) return;
  pane.cursorParked = false;
  pane.term.write(SHOW_CURSOR);
}

function clearCursorRestore(pane: Pane): void {
  if (pane.cursorRestore !== null) clearTimeout(pane.cursorRestore);
  pane.cursorRestore = null;
  pane.cursorParked = false;
}

/** Every frame goes through here: a socket still CONNECTING refuses a send. */
function send(pane: Pane, frame: string | Uint8Array<ArrayBuffer>): void {
  if (pane.socket?.readyState === WebSocket.OPEN) pane.socket.send(frame);
}

function sendResize(pane: Pane): void {
  if (pane.ready)
    send(
      pane,
      JSON.stringify({
        resize: { cols: pane.term.cols, rows: pane.term.rows },
      }),
    );
}

function fitVisible(pane: Pane): void {
  if (
    !pane.el.isConnected ||
    pane.el.clientWidth === 0 ||
    pane.el.clientHeight === 0
  )
    return;
  if (!pane.opened) {
    pane.term.open(pane.el);
    pane.opened = true;
    let webgl: WebglAddon | undefined;
    try {
      webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        webgl?.dispose();
        fitVisible(pane);
      });
      pane.term.loadAddon(webgl);
    } catch {
      webgl?.dispose();
    }
  }
  if (pane.ready) pane.fit.fit();
  pane.term.refresh(0, pane.term.rows - 1);
}

function stop(pane: Pane, reason: string): void {
  pane.reconnected = true;
  pane.ready = false;
  clearCursorRestore(pane);
  const socket = pane.socket;
  pane.socket = null;
  socket?.close(1000);
  pane.term.write(`\r\n\x1b[0m${reason} Reload the window to reattach.\r\n`);
}

function openSocket(deskId: string, pane: Pane): void {
  const current = endpoint;
  if (!current) return;
  const socket = new WebSocket(
    `ws://127.0.0.1:${current.port}/desks/${deskId}/terminal`,
  );
  socket.binaryType = "arraybuffer";
  pane.socket = socket;
  pane.ready = false;
  let streamOffset = 0n;
  let replay = false;
  socket.onopen = () => {
    if (pane.disposed || pane.socket !== socket) return;
    socket.send(JSON.stringify({ bearer: current.bearer }));
  };
  socket.onmessage = (message) => {
    if (pane.disposed || pane.socket !== socket) return;
    if (typeof message.data === "string") {
      const frame = JSON.parse(message.data) as {
        attached?: {
          terminal_id: string;
          offset: string;
          replay_bytes: number;
          cols: number;
          rows: number;
          windows_pty: { backend: "conpty"; buildNumber: number | null } | null;
        };
        exited?: { reason: string; code: number | null };
      };
      if (frame.attached) {
        const info = frame.attached;
        streamOffset = BigInt(info.offset);
        if (
          pane.terminalId !== null &&
          (pane.terminalId !== info.terminal_id ||
            streamOffset > pane.offset ||
            streamOffset + BigInt(info.replay_bytes) < pane.offset)
        ) {
          stop(pane, "Terminal continuity was lost.");
          return;
        }
        if (pane.terminalId === null) {
          pane.terminalId = info.terminal_id;
          pane.offset = streamOffset;
          pane.conpty = info.windows_pty !== null;
          pane.term.options.windowsPty = info.windows_pty
            ? {
                backend: info.windows_pty.backend,
                ...(info.windows_pty.buildNumber === null
                  ? {}
                  : { buildNumber: info.windows_pty.buildNumber }),
              }
            : {};
          pane.term.resize(info.cols, info.rows);
        }
        replay = info.replay_bytes > 0;
        if (!replay) {
          pane.ready = true;
          fitVisible(pane);
          sendResize(pane);
        }
      }
      // Machine surface written into the terminal itself, never localized.
      if (frame.exited) {
        pane.term.write(
          `\r\n\x1b[2mprocess exited (${frame.exited.reason}, ${frame.exited.code})\x1b[0m\r\n`,
        );
      }
      return;
    }
    const incoming = new Uint8Array(message.data as ArrayBuffer);
    const skip = Number(
      pane.offset > streamOffset ? pane.offset - streamOffset : 0n,
    );
    streamOffset += BigInt(incoming.length);
    const bytes = incoming.subarray(skip);
    // Bound the parser queue too; socket delivery is not parser completion.
    if (pane.pending + bytes.length > 1024 * 1024) {
      stop(pane, "Terminal output exceeded the display buffer.");
      return;
    }
    pane.offset += BigInt(bytes.length);
    pane.bytes += bytes.length;
    pane.pending += bytes.length;
    const completingReplay = replay;
    replay = false;
    writeOutput(pane, bytes, () => {
      pane.pending -= bytes.length;
      if (pane.disposed || pane.socket !== socket) return;
      if (completingReplay) {
        pane.ready = true;
        fitVisible(pane);
        sendResize(pane);
      }
    });
  };
  socket.onclose = (closed) => {
    if (pane.socket !== socket) return;
    pane.socket = null;
    pane.ready = false;
    if (pane.disposed) return;
    // The daemon refusing the attachment is final; a network close while the
    // process lives is a reload path and reattaches once.
    if (closed.code === 4404 || closed.code === 4409) {
      dispose(deskId);
      return;
    }
    if (pane.reconnected || closed.code === 1000 || closed.code === 4001)
      return;
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
    allowProposedApi: true,
    cols: 120,
    rows: 40,
    rescaleOverlappingGlyphs: true,
    // Steady accent block: agent TUIs (and VS Code/Pane defaults) keep blink
    // off so the caret does not fight ConPTY redraw parking or Codex DECSCUSR.
    cursorBlink: false,
    cursorStyle: "block",
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
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  // FitAddon only fits on demand; the well resizes with the window and the
  // panels around it.
  const resize = new ResizeObserver(() => fitVisible(pane));
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
    opened: false,
    ready: false,
    terminalId: null,
    offset: 0n,
    pending: 0,
    conpty: false,
    cursorParked: false,
    cursorRestore: null,
  };
  panes.set(deskId, pane);
  term.onData((data) => {
    // Don't leave the caret dark under keystrokes while a settle timer runs.
    if (pane.conpty) releaseCursor(pane);
    send(pane, encoder.encode(data));
    useEvents().clearAttention(deskId);
  });
  term.onResize(() => sendResize(pane));
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
  fitVisible(pane);
  // fit() only fires onResize when the size changed; the PTY still needs the
  // size the element was first measured at.
  sendResize(pane);
}

function dispose(deskId: string): void {
  const pane = panes.get(deskId);
  if (!pane) return;
  pane.disposed = true;
  clearCursorRestore(pane);
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
