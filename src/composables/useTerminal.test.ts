import { beforeEach, expect, it, vi } from "vitest";

// The renderer does not run in jsdom.
vi.mock("@xterm/xterm", () => {
  class Terminal {
    loadAddon() {}
    open() {}
    write() {}
    onData() {}
    onResize() {}
    dispose() {}
  }
  return { Terminal };
});

vi.mock("@xterm/addon-fit", () => {
  class FitAddon {
    fit() {}
    dispose() {}
    proposeDimensions() {
      return { cols: 80, rows: 24 };
    }
  }
  return { FitAddon };
});

import { FakeWebSocket, installFakeWebSocket } from "../test/fakeDaemon";
import { setEndpoint } from "../daemon-endpoint";
import { useEvents } from "./useEvents";
import { useTerminal } from "./useTerminal";

// jsdom has no ResizeObserver.
class FakeResizeObserver {
  static observing = 0;
  observe() {
    FakeResizeObserver.observing += 1;
  }
  unobserve() {}
  disconnect() {
    FakeResizeObserver.observing -= 1;
  }
}
globalThis.ResizeObserver =
  FakeResizeObserver as unknown as typeof ResizeObserver;

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
