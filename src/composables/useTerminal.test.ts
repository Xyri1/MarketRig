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
