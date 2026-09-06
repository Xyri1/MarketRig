import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("@xterm/xterm", () => import("../test/fakeXterm"));
vi.mock("@xterm/addon-fit", () => import("../test/fakeXterm"));
vi.mock("@xterm/addon-unicode11", () => import("../test/fakeXterm"));
vi.mock("@xterm/addon-webgl", () => import("../test/fakeXterm"));

import { FakeWebSocket, installFakeWebSocket } from "../test/fakeDaemon";
import { setEndpoint } from "../daemon-endpoint";
import { useEvents } from "./useEvents";
import { useTerminal } from "./useTerminal";

import { FakeResizeObserver, WebglAddon } from "../test/fakeXterm";

const { ensure, mount, panes, bytesWritten } = useTerminal();

beforeEach(() => {
  installFakeWebSocket();
  setEndpoint({ port: 7100, bearer: "b", daemon_uuid: "u-1" });
  for (const id of [...panes.keys()]) useTerminal().dispose(id);
  WebglAddon.fail = false;
  WebglAddon.instances = [];
});

afterEach(() => {
  vi.useRealTimers();
  document.body.replaceChildren();
});

function attach(
  socket: FakeWebSocket,
  offset = "0",
  replayBytes = 0,
  terminalId = "terminal-1",
) {
  socket.message(
    JSON.stringify({
      attached: {
        terminal_id: terminalId,
        offset,
        replay_bytes: replayBytes,
        cols: 120,
        rows: 40,
        windows_pty: null,
      },
    }),
  );
}

function visibleSlot(pane: ReturnType<typeof ensure>) {
  const slot = document.createElement("div");
  document.body.appendChild(slot);
  Object.defineProperties(pane.el, {
    clientWidth: { value: 640, configurable: true },
    clientHeight: { value: 400, configurable: true },
  });
  return slot;
}

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
  attach(socket);
  socket.message(new Uint8Array([1, 2, 3]).buffer);
  expect(bytesWritten("d-1")).toBe(3);
});

it("opens only when measurable, retains hidden dimensions, and repaints on remount", () => {
  const pane = ensure("d-1");
  const socket = FakeWebSocket.instances[0];
  socket.open();
  attach(socket);
  expect(pane.term.open).not.toHaveBeenCalled();
  expect([pane.term.cols, pane.term.rows]).toEqual([120, 40]);
  const slot = visibleSlot(pane);
  mount("d-1", slot);
  expect(pane.term.open).toHaveBeenCalledOnce();
  expect([pane.term.cols, pane.term.rows]).toEqual([80, 24]);
  useTerminal().evict(slot);
  const before = vi.mocked(pane.fit.fit).mock.calls.length;
  (pane.resize as unknown as FakeResizeObserver).callback();
  expect(pane.fit.fit).toHaveBeenCalledTimes(before);
  mount("d-1", slot);
  expect(pane.term.open).toHaveBeenCalledOnce();
  expect(pane.term.refresh).toHaveBeenLastCalledWith(0, 23);
});

it.each([false, true])(
  "keeps the terminal usable when WebGL fails (initial failure: %s)",
  (initialFailure) => {
    WebglAddon.fail = initialFailure;
    const pane = ensure("d-1");
    mount("d-1", visibleSlot(pane));
    if (!initialFailure) WebglAddon.instances[0].loss();
    expect(WebglAddon.instances[0].dispose).toHaveBeenCalledOnce();
    expect(pane.term.refresh).toHaveBeenCalled();
    expect(panes.get("d-1")).toBe(pane);
  },
);

it("reconnects without replaying bytes already queued to the same desk parser", async () => {
  vi.useFakeTimers();
  const pane = ensure("d-1");
  const first = FakeWebSocket.instances[0];
  first.open();
  attach(first);
  first.message(new Uint8Array([65, 66]).buffer);
  first.close(1006);
  await vi.advanceTimersByTimeAsync(1000);
  const second = FakeWebSocket.instances[1];
  second.open();
  attach(second, "0", 3);
  second.message(new Uint8Array([65, 66, 67]).buffer);
  expect(pane.term.write).toHaveBeenLastCalledWith(
    new Uint8Array([67]),
    expect.any(Function),
  );
  expect(bytesWritten("d-1")).toBe(3);
});

it("uses native ConPTY metadata before parsing and ignores empty overlap on reconnect", async () => {
  vi.useFakeTimers();
  const pane = ensure("d-1");
  const first = FakeWebSocket.instances[0];
  first.open();
  first.message(
    JSON.stringify({
      attached: {
        terminal_id: "terminal-1",
        offset: "9007199254740993",
        replay_bytes: 1,
        cols: 100,
        rows: 30,
        windows_pty: { backend: "conpty", buildNumber: 26100 },
      },
    }),
  );
  first.message(new Uint8Array([65]).buffer);
  expect(pane.term.options.windowsPty).toEqual({
    backend: "conpty",
    buildNumber: 26100,
  });
  expect([pane.term.cols, pane.term.rows]).toEqual([100, 30]);
  first.close(1006);
  mount("d-1", visibleSlot(pane));
  expect(pane.fit.fit).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1000);
  const second = FakeWebSocket.instances[1];
  second.open();
  attach(second, "9007199254740993", 1);
  second.message(new Uint8Array([65]).buffer);
  expect(bytesWritten("d-1")).toBe(1);
  expect(pane.offset).toBe(9007199254740994n);
  expect(pane.ready).toBe(true);
});

it.each([
  ["10", "terminal-1"],
  ["0", "terminal-2"],
])(
  "stops rather than corrupting a warm parser on a replay gap or replaced terminal (%s, %s)",
  async (offset, id) => {
    vi.useFakeTimers();
    const pane = ensure("d-1");
    const first = FakeWebSocket.instances[0];
    first.open();
    attach(first);
    first.message(new Uint8Array([65]).buffer);
    first.close(1006);
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeWebSocket.instances[1];
    second.open();
    attach(second, offset, 1, id);
    second.message(new Uint8Array([66]).buffer);
    expect(second.readyState).toBe(3);
    expect(bytesWritten("d-1")).toBe(1);
    expect(pane.term.write).toHaveBeenLastCalledWith(
      expect.stringContaining("continuity was lost"),
    );
  },
);

it("waits for replay parsing before fitting, and bounds outstanding writes", () => {
  const pane = ensure("d-1");
  mount("d-1", visibleSlot(pane));
  const socket = FakeWebSocket.instances[0];
  socket.open();
  attach(socket, "0", 1);
  let complete: (() => void) | undefined;
  vi.mocked(pane.term.write).mockImplementationOnce((_data, callback) => {
    complete = callback;
  });
  socket.message(new Uint8Array([65]).buffer);
  expect(pane.fit.fit).not.toHaveBeenCalled();
  complete!();
  expect(pane.fit.fit).toHaveBeenCalledOnce();
  expect(pane.pending).toBe(0);
  vi.mocked(pane.term.write).mockImplementation(() => {});
  socket.message(new Uint8Array(1024 * 1024).buffer);
  socket.message(new Uint8Array([66]).buffer);
  expect(socket.readyState).toBe(3);
  expect(pane.pending).toBe(1024 * 1024);
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

it("disposes on SESSION_EXITED", () => {
  ensure("d-1");
  expect(FakeResizeObserver.observing).toBe(1);
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
  // The pane's ResizeObserver goes with it.
  expect(FakeResizeObserver.observing).toBe(0);
  disconnect();
});
