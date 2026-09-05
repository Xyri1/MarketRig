import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));

import { fakeInvoke, installFakeDaemon } from "../test/fakeDaemon";
import { useDaemon } from "./useDaemon";

const good = { port: 7100, bearer: "b", daemon_uuid: "u-1" };
const { status, boot, error } = useDaemon();

beforeEach(() => {
  fakeInvoke.mockReset();
  status.value = "STARTING";
  error.value = "";
});

it("verifies the UUID match and never starts a daemon", async () => {
  fakeInvoke.mockResolvedValue(good);
  installFakeDaemon({ "GET /health": () => ({ status: 200, body: good }) });
  await boot();
  expect(status.value).toBe("READY");
  expect(fakeInvoke.mock.calls.map((c) => c[0])).toEqual(["read_endpoint"]);
});

it("falls back to start_daemon exactly once", async () => {
  let reads = 0;
  fakeInvoke.mockImplementation(async (name: string) =>
    name === "read_endpoint" && reads++ === 0 ? null : good,
  );
  installFakeDaemon({ "GET /health": () => ({ status: 200, body: good }) });
  await boot();
  // read_endpoint (null) → start_daemon → read_endpoint again.
  expect(fakeInvoke.mock.calls.map((c) => c[0])).toEqual([
    "read_endpoint",
    "start_daemon",
    "read_endpoint",
  ]);
  expect(status.value).toBe("READY");
});

it("treats a UUID mismatch as a failure", async () => {
  fakeInvoke.mockResolvedValue(good);
  installFakeDaemon({
    "GET /health": () => ({ status: 200, body: { daemon_uuid: "u-2" } }),
  });
  await boot();
  expect(status.value).toBe("UNAVAILABLE");
  expect(error.value).toBe("DAEMON_UUID_MISMATCH");
  expect(
    fakeInvoke.mock.calls.filter((c) => c[0] === "start_daemon"),
  ).toHaveLength(1);
});

it("treats a 401 as a failure", async () => {
  fakeInvoke.mockResolvedValue(good);
  installFakeDaemon({
    "GET /health": () => ({
      status: 401,
      body: { code: "UNAUTHORIZED", message: "no" },
    }),
  });
  await boot();
  expect(status.value).toBe("UNAVAILABLE");
  expect(error.value).toBe("HEALTH_REFUSED");
});
