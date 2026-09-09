import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  isEnabled: vi.fn(async () => false),
  enable: vi.fn(async () => {}),
  disable: vi.fn(async () => {}),
}));

import { flushPromises } from "@vue/test-utils";
import { enable, isEnabled } from "@tauri-apps/plugin-autostart";
import {
  FakeWebSocket,
  installFakeDaemon,
  installFakeWebSocket,
} from "../test/fakeDaemon";
import { client } from "../client/client.gen";
import { useEvents } from "../composables/useEvents";
import { mountWithI18n } from "../test/mountWithI18n";
import SettingsTab from "./SettingsTab.vue";

const policy = {
  trigger_code_policy: "ALWAYS_ALLOW",
  paper_order_policy: "REQUIRE_APPROVAL",
  delivery_mode: "QUEUE",
  steer_available: false,
  updated_at_ns: 1,
};

/** `GET /openviking`; a test swaps it for the row the daemon would answer. */
function status(
  setup: Record<string, unknown>,
  child = "NOT_STARTED",
): Record<string, unknown> {
  return { setup, child, desks: {} };
}

/** `GET /research/hithink`; the row never carries a key (feature SPEC §1.2). */
function hithinkRow(
  over: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    state: "UNCONFIGURED",
    a_share_feed: "YAHOO",
    api_key_present: false,
    base_url: "https://fuyao.aicubes.cn",
    ...over,
  };
}

let put: string | null = null;
let hithinkSent: string | null = null;
let hithink: Record<string, unknown>;
let runtimes: unknown[] = [];
let ov: Record<string, unknown>;
let candidates: Record<string, unknown>;
let setupAnswer: () => { status: number; body?: unknown };

beforeEach(() => {
  put = null;
  hithinkSent = null;
  hithink = hithinkRow();
  vi.mocked(enable).mockClear();
  vi.mocked(isEnabled).mockResolvedValue(false);
  runtimes = [
    { runtime: "codex", state: "AVAILABLE", version: "1.0.0" },
    { runtime: "claude", state: "UNAVAILABLE" },
  ];
  ov = status({ state: "UNCONFIGURED" });
  candidates = { python: null, node: null };
  setupAnswer = () => ({ status: 202, body: { state: "PROVISIONING" } });
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  installFakeDaemon({
    "GET /runtimes": () => ({ status: 200, body: { runtimes } }),
    "GET /memory/provider": () => ({
      status: 200,
      body: {
        api_key_present: false,
        llm_model: "m-1",
        embedding_model: "m-2",
      },
    }),
    "GET /research/hithink": () => ({ status: 200, body: hithink }),
    "PUT /research/hithink": (request) => {
      hithinkSent = request.body;
      hithink = hithinkRow({
        state: "AVAILABLE",
        a_share_feed: "HITHINK",
        api_key_present: true,
        validated_at_ns: 1_700_000_000_000_000_000,
      });
      return { status: 200, body: hithink };
    },
    "PATCH /research/hithink": (request) => {
      hithinkSent = request.body;
      return { status: 200, body: hithink };
    },
    "DELETE /research/hithink": () => {
      hithink = hithinkRow();
      return { status: 200, body: hithink };
    },
    "GET /openviking": () => ({ status: 200, body: ov }),
    "GET /openviking/candidates": () => ({ status: 200, body: candidates }),
    "PUT /openviking/setup": () => setupAnswer(),
    "POST /openviking/retry": () => ({ status: 202, body: ov }),
    "GET /settings/policies": () => ({ status: 200, body: policy }),
    "PUT /settings/policies": (request) => {
      put = request.body;
      return { status: 200, body: policy };
    },
  });
});

it("shows delivery as a disabled select whose Steer item is disabled", async () => {
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  const select = wrapper.get("select");
  expect(select.attributes("disabled")).toBeDefined();
  const options = select.findAll("option");
  expect(options.map((o) => o.text())).toEqual([
    "Queue next turn",
    "Steer the current turn",
  ]);
  expect(options[1].attributes("disabled")).toBeDefined();
});

it("takes the two model ids as typed text", async () => {
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  const inputs = wrapper
    .findAll("input")
    .filter((input) => input.attributes("aria-label")?.endsWith(" model"));
  expect(
    inputs.map((input) => (input.element as HTMLInputElement).value),
  ).toEqual(["m-1", "m-2"]);
});

it("sends only the changed policy field", async () => {
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  const paperOrder = wrapper
    .findAllComponents({ name: "SelectRoot" })
    .find((select) => select.props("modelValue") === "REQUIRE_APPROVAL");
  paperOrder!.vm.$emit("update:modelValue", "ALWAYS_ALLOW");
  await flushPromises();

  expect(put).toBe('{"paper_order_policy":"ALWAYS_ALLOW"}');
});

it("turns autostart on once when no runtime is AVAILABLE yet", async () => {
  runtimes = [{ runtime: "codex", state: "UNAVAILABLE" }];
  mountWithI18n(SettingsTab);
  await flushPromises();

  expect(enable).toHaveBeenCalledTimes(1);
});

it("leaves autostart alone once a runtime is AVAILABLE", async () => {
  mountWithI18n(SettingsTab);
  await flushPromises();

  expect(enable).not.toHaveBeenCalled();
});

it("provisions from Settings and follows the row out of PROVISIONING", async () => {
  vi.useFakeTimers();
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  expect(wrapper.get('[data-testid="openviking"]').text()).toContain(
    "UNCONFIGURED",
  );

  await wrapper
    .get('[data-testid="openviking-python"]')
    .setValue("/opt/py/bin/python3.12");
  await wrapper.get('[data-testid="openviking-node"]').setValue("/opt/n/node");
  ov = status({
    state: "PROVISIONING",
    python_path: "/opt/py/bin/python3.12",
    node_path: "/opt/n/node",
  });
  await wrapper.get('[data-testid="openviking"] form').trigger("submit");
  await flushPromises();

  expect(wrapper.get('[data-testid="openviking"]').text()).toContain(
    "PROVISIONING",
  );
  expect(
    wrapper.get('[data-testid="openviking-setup"]').attributes("disabled"),
  ).toBeDefined();

  // The 2 s refetch, not a reload, is what shows the finished environment.
  ov = status(
    {
      state: "AVAILABLE",
      python_path: "/opt/py/bin/python3.12",
      python_version: "3.12",
      node_path: "/opt/n/node",
    },
    "READY",
  );
  await vi.advanceTimersByTimeAsync(2_000);
  await flushPromises();

  expect(wrapper.get('[data-testid="openviking"]').text()).toContain(
    "AVAILABLE READY",
  );
  expect(
    wrapper.get('[data-testid="openviking-setup"]').attributes("disabled"),
  ).toBeUndefined();
  vi.useRealTimers();
});

it("prefills both fields from the candidate search on an UNCONFIGURED row", async () => {
  candidates = { python: "/x/python3.12", node: "/y/node" };
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  const value = (testid: string) =>
    (wrapper.get(`[data-testid="${testid}"]`).element as HTMLInputElement)
      .value;
  expect(value("openviking-python")).toBe("/x/python3.12");
  expect(value("openviking-node")).toBe("/y/node");
});

it("shows a PYTHON_UNSUPPORTED refusal beside the fields", async () => {
  setupAnswer = () => ({
    status: 400,
    body: {
      code: "PYTHON_UNSUPPORTED",
      message: "That interpreter reports 3.11; MarketRig needs 3.12.",
    },
  });
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  await wrapper
    .get('[data-testid="openviking-python"]')
    .setValue("/opt/py/bin/python3.11");
  await wrapper.get('[data-testid="openviking"] form').trigger("submit");
  await flushPromises();

  expect(wrapper.get('[data-testid="openviking-error"]').text()).toContain(
    "3.11",
  );
  expect(wrapper.get('[data-testid="openviking"]').text()).toContain(
    "UNCONFIGURED",
  );
});

it("renders each HiThink state, with the message the row carries", async () => {
  let wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  expect(wrapper.get('[data-testid="hithink"]').text()).toContain(
    "UNCONFIGURED",
  );
  expect(wrapper.find('[data-testid="hithink-failure"]').exists()).toBe(false);

  hithink = hithinkRow({
    state: "AVAILABLE",
    a_share_feed: "HITHINK",
    api_key_present: true,
    validated_at_ns: 1_700_000_000_000_000_000,
  });
  wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  expect(wrapper.get('[data-testid="hithink"]').text()).toContain("AVAILABLE");

  hithink = hithinkRow({
    state: "UNAVAILABLE",
    a_share_feed: "HITHINK",
    api_key_present: true,
    failure_code: "KEY_REJECTED",
    failure_message: "HiThink rejected the stored key.",
  });
  wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  expect(wrapper.get('[data-testid="hithink-failure"]').text()).toContain(
    "KEY_REJECTED HiThink rejected the stored key.",
  );
});

it("saves the HiThink key, clears the field, and never shows it back", async () => {
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  await wrapper.get('[data-testid="hithink-key"]').setValue("k-1");
  await wrapper.get('[data-testid="hithink"] form').trigger("submit");
  await flushPromises();

  expect(hithinkSent).toBe('{"api_key":"k-1"}');
  const field = wrapper.get('[data-testid="hithink-key"]')
    .element as HTMLInputElement;
  expect(field.value).toBe("");
  expect(wrapper.get('[data-testid="hithink"]').text()).not.toContain("k-1");

  // Remove appears only once a key is stored, and returns the row.
  await wrapper.get('[data-testid="hithink-remove"]').trigger("click");
  await flushPromises();
  expect(wrapper.get('[data-testid="hithink"]').text()).toContain(
    "UNCONFIGURED",
  );
});

it("gates the A-share tick box on a stored key and PATCHes the feed", async () => {
  let wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  const unkeyed = wrapper.get('[data-testid="hithink-feed"]');
  expect(unkeyed.attributes("disabled")).toBeDefined();
  expect((unkeyed.element as HTMLInputElement).checked).toBe(false);

  hithink = hithinkRow({
    state: "AVAILABLE",
    a_share_feed: "HITHINK",
    api_key_present: true,
  });
  wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  const box = wrapper.get('[data-testid="hithink-feed"]');
  expect(box.attributes("disabled")).toBeUndefined();
  expect((box.element as HTMLInputElement).checked).toBe(true);

  await box.setValue(false);
  await flushPromises();
  expect(hithinkSent).toBe('{"a_share_feed":"YAHOO"}');
});

it("refetches the HiThink row when the tail reports a change", async () => {
  installFakeWebSocket();
  const { connect, disconnect } = useEvents();
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();

  connect(7100, "b");
  FakeWebSocket.instances[0].open();
  hithink = hithinkRow({
    state: "AVAILABLE",
    a_share_feed: "HITHINK",
    api_key_present: true,
  });
  FakeWebSocket.instances[0].message(
    JSON.stringify({
      id: "e-1",
      kind: "HITHINK_PROVIDER_CHANGED",
      occurred_at_ns: 1,
      payload: { state: "AVAILABLE", a_share_feed: "HITHINK" },
    }),
  );
  await flushPromises();

  expect(wrapper.get('[data-testid="hithink"]').text()).toContain("AVAILABLE");
  disconnect();
});

it("turns UNAVAILABLE with Retry when the tail reports a loss", async () => {
  installFakeWebSocket();
  const { connect, disconnect } = useEvents();
  const wrapper = mountWithI18n(SettingsTab);
  await flushPromises();
  expect(wrapper.find('[data-testid="openviking-retry"]').exists()).toBe(false);

  connect(7100, "b");
  FakeWebSocket.instances[0].open();
  ov = status(
    {
      state: "UNAVAILABLE",
      failure_code: "CHILD_FAILED",
      failure_message: "openviking-server: address already in use",
    },
    "LOST",
  );
  FakeWebSocket.instances[0].message(
    JSON.stringify({
      id: "e-1",
      kind: "OPENVIKING_LOST",
      occurred_at_ns: 1,
      payload: { exit_code: 1 },
    }),
  );
  await flushPromises();

  expect(wrapper.get('[data-testid="openviking-failure"]').text()).toContain(
    "address already in use",
  );
  expect(wrapper.find('[data-testid="openviking-retry"]').exists()).toBe(true);
  disconnect();
});
