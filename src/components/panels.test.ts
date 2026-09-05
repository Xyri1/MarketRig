import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));
vi.mock("@xterm/xterm", () => import("../test/fakeXterm"));
vi.mock("@xterm/addon-fit", () => import("../test/fakeXterm"));

import { nextTick } from "vue";
import { installFakeDaemon, installFakeWebSocket } from "../test/fakeDaemon";
import { mountWithI18n } from "../test/mountWithI18n";
import { client } from "../client/client.gen";
import { useApprovals } from "../composables/useApprovals";
import { useTerminal } from "../composables/useTerminal";
import ActivityTab from "./ActivityTab.vue";
import DeskList from "./DeskList.vue";
import RightPanel from "./RightPanel.vue";
import SessionHeader from "./SessionHeader.vue";
import TerminalWell from "./TerminalWell.vue";

function desk(id: string, name: string, extra: Record<string, unknown> = {}) {
  return {
    id,
    name,
    state: "READY",
    selected_runtime: "codex",
    workspace_path: `/tmp/${name}`,
    created_at_ns: 1,
    ...extra,
  };
}

/** Lets the mounted component's awaited fetches settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 8; i++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

beforeEach(() => {
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
});

it("renders desks in creation order with their gutter and pending count", async () => {
  installFakeDaemon({
    "GET /approvals": () => ({
      status: 200,
      body: {
        approvals: [
          {
            kind: "PAPER_ORDER",
            id: "a-1",
            desk_id: "d-2",
            desk_name: "beta",
            approval: "PENDING",
            requested_at_ns: 1,
            detail: {},
          },
        ],
      },
    }),
  });
  await useApprovals().refetch();
  const wrapper = mountWithI18n(DeskList, {
    desks: [
      desk("d-1", "alpha"),
      desk("d-2", "beta"),
      desk("d-3", "gamma", { state: "FAILED" }),
    ],
    selected: "d-1",
  });
  const rows = wrapper.findAll("[data-state]");
  expect(rows.map((row) => row.text().split("\n")[0].trim())).toEqual([
    "alpha",
    "beta1",
    "gammaRetry",
  ]);
  expect(rows.map((row) => row.attributes("data-state"))).toEqual([
    "idle",
    "pending",
    "failure",
  ]);
  expect(rows[1].find("span").classes()).toContain("bg-state-pending");
});

it("attaches the pane when a reload finds the session already live", async () => {
  installFakeWebSocket();
  installFakeDaemon({
    "GET /desks/d-1/session": () => ({
      status: 200,
      body: { process: { runtime: "codex" } },
    }),
    "GET /desks/d-1": () => ({ status: 200, body: desk("d-1", "alpha") }),
    "GET /runtimes": () => ({ status: 200, body: { runtimes: [] } }),
  });
  const { panes, dispose } = useTerminal();
  mountWithI18n(SessionHeader, { desk: desk("d-1", "alpha") });
  await settle();
  expect(panes.has("d-1")).toBe(true);
  dispose("d-1");
});

it("mounts a pane that appears after the well is up", async () => {
  installFakeWebSocket();
  const { ensure, panes, dispose } = useTerminal();
  const wrapper = mountWithI18n(TerminalWell, {
    deskId: "d-1",
    state: "idle",
  });
  await settle();
  const pane = ensure("d-1");
  await settle();
  expect(wrapper.element.contains(pane.el)).toBe(true);
  dispose("d-1");
  await settle();
  expect(wrapper.element.contains(pane.el)).toBe(false);
  expect(panes.has("d-1")).toBe(false);
});

it("disables Interrupt for a claude desk and starts a NEW session", async () => {
  const posted: string[] = [];
  installFakeDaemon({
    "GET /desks/d-1/session": () => ({ status: 200, body: { process: null } }),
    "GET /desks/d-1": () => ({ status: 200, body: desk("d-1", "alpha") }),
    "GET /runtimes": () => ({ status: 200, body: { runtimes: [] } }),
    "POST /desks/d-1/session/activate": (request) => {
      posted.push(String(request.body));
      return { status: 202, body: { process: {} } };
    },
  });
  const wrapper = mountWithI18n(SessionHeader, {
    desk: desk("d-1", "alpha", { selected_runtime: "claude" }),
  });
  await settle();
  const interrupt = wrapper
    .findAll("button")
    .find((b) => b.text() === "Interrupt")!;
  expect(interrupt.attributes("disabled")).toBeDefined();
  expect(interrupt.attributes("title")).toBe(
    "Claude Code has no structured interrupt.",
  );
  await wrapper
    .findAll("button")
    .find((b) => b.text() === "Start")!
    .trigger("click");
  await settle();
  expect(posted).toEqual([JSON.stringify({ mode: "NEW" })]);
});

it("selects Settings while no runtime is AVAILABLE", async () => {
  installFakeDaemon({
    "GET /runtimes": () => ({
      status: 200,
      body: { runtimes: [{ runtime: "codex", state: "UNDISCOVERED" }] },
    }),
  });
  const wrapper = mountWithI18n(RightPanel, { deskId: null });
  await settle();
  expect(wrapper.vm.tab).toBe("settings");
});

it("lists events newest first and pages with before", async () => {
  const queries: string[] = [];
  installFakeDaemon({
    "GET /events": (request) => {
      queries.push(request.query.toString());
      return request.query.get("before")
        ? { status: 200, body: { events: [row("e-1", 1)] } }
        : {
            status: 200,
            body: {
              events: [row("e-3", 3), row("e-2", 2)],
              next_before: "2:e-2",
            },
          };
    },
  });
  const wrapper = mountWithI18n(ActivityTab, { deskId: "d-1" });
  await settle();
  expect(wrapper.text()).toContain("SESSION_STARTED");
  expect(wrapper.findAll("pre")).toHaveLength(2);
  await wrapper.find("button").trigger("click");
  await settle();
  expect(wrapper.findAll("pre")).toHaveLength(3);
  expect(queries[1]).toContain("before=2%3Ae-2");
});

function row(id: string, ns: number) {
  return {
    id,
    kind: "SESSION_STARTED",
    desk_id: "d-1",
    occurred_at_ns: ns,
    payload: {},
  };
}
