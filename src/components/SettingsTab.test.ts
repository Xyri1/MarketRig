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
import { installFakeDaemon } from "../test/fakeDaemon";
import { client } from "../client/client.gen";
import { mountWithI18n } from "../test/mountWithI18n";
import SettingsTab from "./SettingsTab.vue";

const policy = {
  trigger_code_policy: "ALWAYS_ALLOW",
  paper_order_policy: "REQUIRE_APPROVAL",
  delivery_mode: "QUEUE",
  steer_available: false,
  updated_at_ns: 1,
};

let put: string | null = null;

beforeEach(() => {
  put = null;
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  installFakeDaemon({
    "GET /runtimes": () => ({
      status: 200,
      body: {
        runtimes: [
          { runtime: "codex", state: "AVAILABLE", version: "1.0.0" },
          { runtime: "claude", state: "UNAVAILABLE" },
        ],
      },
    }),
    "GET /memory": () => ({
      status: 200,
      body: {
        child: { state: "UNCONFIGURED", live: "NOT_STARTED" },
        provider: {
          api_key_present: false,
          llm_model: "m-1",
          embedding_model: "m-2",
        },
      },
    }),
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
