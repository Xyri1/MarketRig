import { beforeEach, expect, it, vi } from "vitest";

// The WASM parser does not run in jsdom.
vi.mock("ghostty-web", () => {
  class Terminal {
    static made = 0;
    written: unknown[] = [];
    disposed = false;
    constructor() {
      Terminal.made += 1;
    }
    loadAddon() {}
    open() {}
    write(data: unknown) {
      this.written.push(data);
    }
    onData() {}
    onResize() {}
    static wheel: ((event: WheelEvent) => boolean) | undefined;
    attachCustomWheelEventHandler(handler: (event: WheelEvent) => boolean) {
      Terminal.wheel = handler;
    }
    buffer = { active: { type: "normal" } };
    modes = new Set<number>();
    getMode(mode: number) {
      return this.modes.has(mode);
    }
    element: HTMLElement | undefined;
    cols = 80;
    rows = 24;
    dispose() {
      this.disposed = true;
    }
  }
  class FitAddon {
    fit() {}
    observeResize() {}
    dispose() {}
    proposeDimensions() {
      return { cols: 80, rows: 24 };
    }
  }
  return { Terminal, FitAddon, init: async () => {} };
});

import { Terminal } from "ghostty-web";
import { FakeWebSocket, installFakeWebSocket } from "../test/fakeDaemon";
import { setEndpoint } from "../daemon-endpoint";
import { useEvents } from "./useEvents";
import { useTerminal } from "./useTerminal";

const { ensure, mount, panes, bytesWritten } = useTerminal();

beforeEach(() => {
  installFakeWebSocket();
  setEndpoint({ port: 7100, bearer: "b", daemon_uuid: "u-1" });
  for (const id of [...panes.keys()]) useTerminal().dispose(id);
});

it("keeps one Terminal per desk across selection changes", () => {
  const pane = ensure("d-1");
  const first = document.createElement("div");
  const second = document.createElement("div");
  mount("d-1", first);
  mount("d-1", second);
  expect(panes.get("d-1")).toBe(pane);
  expect(second.firstChild).toBe(pane.el);
  expect(first.childNodes).toHaveLength(0);

  const socket = FakeWebSocket.instances[0];
  // mount() fits, which resizes: nothing may be sent before the socket opens.
  expect(socket.sent).toHaveLength(0);
  socket.open();
  expect(socket.sent[0]).toBe(JSON.stringify({ bearer: "b" }));
  socket.message(new Uint8Array([1, 2, 3]).buffer);
  expect(bytesWritten("d-1")).toBe(3);
});

it("shows one desk at a time in the slot", () => {
  const slot = document.createElement("div");
  const first = ensure("d-1");
  mount("d-1", slot);
  mount("d-2", slot);
  expect([...slot.children]).toEqual([panes.get("d-2")!.el]);
  expect(first.el.parentElement).toBeNull();
  expect(panes.has("d-1")).toBe(true);
  // A desk without a session leaves the slot empty of every pane.
  useTerminal().evict(slot);
  expect(slot.childNodes).toHaveLength(0);
});

it("reports the wheel as SGR mouse on the alternate screen when asked", () => {
  const pane = ensure("d-1");
  const socket = FakeWebSocket.instances[0];
  socket.open();
  const term = pane.term as unknown as {
    buffer: { active: { type: string } };
    modes: Set<number>;
  };
  const wheel = (Terminal as unknown as { wheel: (e: WheelEvent) => boolean })
    .wheel;
  const down = { deltaY: 100, clientX: 0, clientY: 0 } as WheelEvent;

  // Normal screen: ghostty-web scrolls its own scrollback.
  expect(wheel(down)).toBe(false);
  term.buffer.active.type = "alternate";
  // Alternate scroll off and no mouse tracking: swallowed, no arrow keys.
  expect(wheel(down)).toBe(true);
  term.modes.add(1007);
  expect(wheel(down)).toBe(false);
  // SGR mouse tracking: one report per notch, 1-based cell 1;1 in jsdom.
  term.modes.add(1000).add(1006);
  const before = socket.sent.length;
  expect(wheel(down)).toBe(true);
  const frames = socket.sent.slice(before) as Uint8Array[];
  expect(frames).toHaveLength(3);
  expect(new TextDecoder().decode(frames[0])).toBe("[<65;1;1M");
});

it("disposes on SESSION_EXITED", () => {
  ensure("d-1");
  const { connect, disconnect } = useEvents();
  connect(7100, "b");
  const events = FakeWebSocket.instances.at(-1)!;
  events.open();
  events.message(
    JSON.stringify({
      id: "e-1",
      kind: "SESSION_EXITED",
      desk_id: "d-1",
      occurred_at_ns: 1,
    }),
  );
  expect(panes.has("d-1")).toBe(false);
  disconnect();
});
