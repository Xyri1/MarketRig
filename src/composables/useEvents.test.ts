import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { FakeWebSocket, installFakeWebSocket } from "../test/fakeDaemon";
import { useEvents } from "./useEvents";

const { connect, disconnect, on, attention } = useEvents();

function row(id: string, kind: string, payload?: unknown) {
  return { id, kind, desk_id: "d-1", occurred_at_ns: 42, payload };
}

beforeEach(() => {
  vi.useFakeTimers();
  installFakeWebSocket();
  attention.clear();
});

afterEach(() => {
  disconnect();
  vi.useRealTimers();
});

it("dispatches by kind and calls the handler with the event only", () => {
  const seen: unknown[] = [];
  const off = on(["SESSION_STARTED", "SESSION_EXITED"], (e) => seen.push(e));
  connect(7100, "b");
  const socket = FakeWebSocket.instances[0];
  socket.open();
  expect(socket.sent).toEqual([JSON.stringify({ bearer: "b" })]);
  socket.message(JSON.stringify({ tail: "1:a" }));
  socket.message(JSON.stringify(row("e-1", "SESSION_STARTED")));
  socket.message(JSON.stringify(row("e-2", "POLICY_CHANGED")));
  expect(seen).toEqual([row("e-1", "SESSION_STARTED")]);
  off();
  socket.message(JSON.stringify(row("e-3", "SESSION_EXITED")));
  expect(seen).toHaveLength(1);
});

it("sends the cursor on reconnect", () => {
  connect(7100, "b");
  const first = FakeWebSocket.instances[0];
  first.open();
  first.message(JSON.stringify(row("e-1", "POLICY_CHANGED")));
  first.close(1006);
  vi.advanceTimersByTime(1_000);
  const second = FakeWebSocket.instances[1];
  second.open();
  expect(second.sent).toEqual([
    JSON.stringify({ bearer: "b", after: "42:e-1" }),
  ]);
});

it("sets attention for every SESSION_ATTENTION but session_start", () => {
  connect(7100, "b");
  const socket = FakeWebSocket.instances[0];
  socket.open();
  socket.message(
    JSON.stringify(row("e-1", "SESSION_ATTENTION", { kind: "session_start" })),
  );
  expect(attention.has("d-1")).toBe(false);
  socket.message(
    JSON.stringify(row("e-2", "SESSION_ATTENTION", { kind: "error" })),
  );
  expect(attention.has("d-1")).toBe(true);
  useEvents().clearAttention("d-1");
  expect(attention.has("d-1")).toBe(false);
});
