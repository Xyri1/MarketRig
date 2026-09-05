import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));

import { fakeInvoke, installFakeDaemon } from "../test/fakeDaemon";
import { client } from "../client/client.gen";
import { useApprovals } from "./useApprovals";

function approval(id: string, deskId: string) {
  return {
    kind: "PAPER_ORDER",
    id,
    desk_id: deskId,
    desk_name: deskId,
    approval: "PENDING",
    requested_at_ns: 1,
    detail: {},
  };
}

const { refetch, byDesk, total, pending } = useApprovals();

beforeEach(() => {
  fakeInvoke.mockReset();
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
});

it("counts per desk and calls set_tray_pending", async () => {
  installFakeDaemon({
    "GET /approvals": (request) => {
      expect(request.query.get("state")).toBe("PENDING");
      return {
        status: 200,
        body: {
          approvals: [
            approval("a-1", "d-1"),
            approval("a-2", "d-1"),
            approval("a-3", "d-2"),
          ],
        },
      };
    },
  });
  await refetch();
  expect(total.value).toBe(3);
  expect(pending.value).toHaveLength(3);
  expect([...byDesk.entries()]).toEqual([
    ["d-1", 2],
    ["d-2", 1],
  ]);
  expect(fakeInvoke).toHaveBeenCalledWith("set_tray_pending", { n: 3 });
});
