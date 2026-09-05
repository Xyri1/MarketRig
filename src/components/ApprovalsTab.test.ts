import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));

import { flushPromises } from "@vue/test-utils";
import { client } from "../client/client.gen";
import { mountWithI18n } from "../test/mountWithI18n";
import ApprovalsTab from "./ApprovalsTab.vue";

const items = [
  {
    kind: "PAPER_ORDER",
    id: "a-1",
    desk_id: "d-1",
    desk_name: "alpha",
    approval: "PENDING",
    requested_at_ns: 1_700_000_000_000_000_000,
    detail: { request: { side: "BUY", quantity: "100" } },
  },
  {
    kind: "TRIGGER_CODE",
    id: "a-2",
    desk_id: "d-1",
    desk_name: "alpha",
    approval: "PENDING",
    requested_at_ns: 1_700_000_000_000_000_000,
    detail: { trigger_name: "morning" },
  },
];

/** The decision hangs until the test releases it. */
let release: (() => void) | null = null;

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  release = null;
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = new URL(
      typeof input === "string" ? input : ((input as Request).url ?? input),
    );
    if (url.pathname === "/approvals") return json({ approvals: items });
    await new Promise<void>((resolve) => (release = resolve));
    return json(items[0]);
  }) as typeof fetch;
});

it("renders both kinds and holds the buttons while a decision is in flight", async () => {
  const wrapper = mountWithI18n(ApprovalsTab);
  await flushPromises();

  expect(wrapper.text()).toContain("Paper order");
  expect(wrapper.text()).toContain("Trigger code");
  expect(wrapper.text()).toContain("alpha");

  const buttons = wrapper.findAll("button");
  const approve = buttons.filter((b) => b.text() === "Approve")[0];
  expect(approve.attributes("disabled")).toBeUndefined();

  await approve.trigger("click");
  await flushPromises();
  const held = wrapper
    .findAll("button")
    .filter((b) => b.text() === "Approve" || b.text() === "Deny");
  expect(held[0].attributes("disabled")).toBeDefined();
  expect(held[1].attributes("disabled")).toBeDefined();

  release?.();
  await flushPromises();
  expect(
    wrapper
      .findAll("button")
      .filter((b) => b.text() === "Approve")[0]
      .attributes("disabled"),
  ).toBeUndefined();
});
