import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("./test/fakeDaemon")).fakeInvoke,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => true),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: vi.fn(),
}));
vi.mock("@xterm/xterm", () => import("./test/fakeXterm"));
vi.mock("@xterm/addon-fit", () => import("./test/fakeXterm"));
vi.mock("@xterm/addon-unicode11", () => import("./test/fakeXterm"));
vi.mock("@xterm/addon-webgl", () => import("./test/fakeXterm"));

import { flushPromises } from "@vue/test-utils";
import {
  fakeInvoke,
  fakeLocale,
  installFakeDaemon,
  installFakeWebSocket,
  localeRoutes,
} from "./test/fakeDaemon";
import { client } from "./client/client.gen";
import i18n from "./i18n";
import zhHans from "./locales/zh-Hans.json";
import { mountWithI18n } from "./test/mountWithI18n";
import App from "./App.vue";

const endpoint = { port: 7100, bearer: "b", daemon_uuid: "u-1" };

/** The system list `detectLocale` reads at startup (feature SPEC §2.2). */
function systemLanguages(tags: string[]): void {
  vi.spyOn(navigator, "languages", "get").mockReturnValue(tags);
}

beforeEach(() => {
  fakeInvoke.mockReset();
  fakeInvoke.mockResolvedValue(endpoint);
  fakeLocale.value = null;
  fakeLocale.writes = [];
  i18n.global.locale.value = "en";
  installFakeWebSocket();
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  installFakeDaemon({
    ...localeRoutes,
    "GET /health": () => ({ status: 200, body: endpoint }),
    "GET /desks": () => ({ status: 200, body: { desks: [] } }),
    "GET /approvals": () => ({ status: 200, body: { approvals: [] } }),
    // One AVAILABLE runtime keeps the right panel off its Settings onboarding.
    "GET /runtimes": () => ({
      status: 200,
      body: { runtimes: [{ runtime: "codex", state: "AVAILABLE" }] },
    }),
  });
});

it("detects the system language once, writes it, and renders it", async () => {
  systemLanguages(["zh-CN", "en-US"]);
  const wrapper = mountWithI18n(App);
  await flushPromises();
  await flushPromises();

  expect(fakeLocale.writes).toEqual(["zh-Hans"]);
  expect(i18n.global.locale.value).toBe("zh-Hans");
  expect(wrapper.text()).toContain(zhHans.desks.empty);
});

it("writes nothing when the daemon already holds a language", async () => {
  fakeLocale.value = "en";
  systemLanguages(["zh-CN"]);
  mountWithI18n(App);
  await flushPromises();
  await flushPromises();

  expect(fakeLocale.writes).toEqual([]);
  expect(i18n.global.locale.value).toBe("en");
});
