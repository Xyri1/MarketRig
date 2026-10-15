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
import { disable, enable } from "@tauri-apps/plugin-autostart";
import {
  fakeLocale,
  installFakeDaemon,
  localeRoutes,
} from "../test/fakeDaemon";
import { client } from "../client/client.gen";
import i18n from "../i18n";
import { mountWithI18n } from "../test/mountWithI18n";
import OnboardingGate from "./OnboardingGate.vue";

beforeEach(() => {
  vi.mocked(enable).mockClear();
  vi.mocked(disable).mockClear();
  fakeLocale.value = null;
  fakeLocale.writes = [];
  i18n.global.locale.value = "en";
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  installFakeDaemon(localeRoutes);
});

it("offers autostart on, and Continue enables it and emits done", async () => {
  const wrapper = mountWithI18n(OnboardingGate);
  await wrapper.get('[data-testid="onboarding-continue"]').trigger("click");
  await flushPromises();

  expect(enable).toHaveBeenCalledTimes(1);
  expect(disable).not.toHaveBeenCalled();
  expect(wrapper.emitted("done")).toHaveLength(1);
});

it("stores the chosen language before applying it, and unticked autostart is disabled", async () => {
  const wrapper = mountWithI18n(OnboardingGate);
  await wrapper.get('[data-testid="language"]').setValue("zh-Hans");
  await flushPromises();
  expect(fakeLocale.writes).toEqual(["zh-Hans"]);
  expect(i18n.global.locale.value).toBe("zh-Hans");

  await wrapper.get('[data-testid="onboarding-autostart"]').setValue(false);
  await wrapper.get('[data-testid="onboarding-continue"]').trigger("click");
  await flushPromises();
  expect(disable).toHaveBeenCalledTimes(1);
  expect(enable).not.toHaveBeenCalled();
});
