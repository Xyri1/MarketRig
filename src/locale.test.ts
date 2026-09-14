import { afterEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("./test/fakeDaemon")).fakeInvoke,
}));

import { fakeInvoke } from "./test/fakeDaemon";
import i18n from "./i18n";
import { applyLocale, detectLocale } from "./locale";

afterEach(async () => {
  fakeInvoke.mockReset();
  await applyLocale("en");
});

it("resolves a language list by BCP 47 lookup over likely subtags", () => {
  const table: [string[], string][] = [
    [["zh-CN"], "zh-Hans"],
    [["zh"], "zh-Hans"],
    [["zh-SG"], "zh-Hans"],
    [["zh-Hans-CN"], "zh-Hans"],
    [["zh-TW"], "en"],
    [["zh-HK"], "en"],
    [["zh-Hant"], "en"],
    [["ja", "zh-CN"], "zh-Hans"],
    [["en-US"], "en"],
    [[""], "en"],
    [[], "en"],
  ];
  for (const [preferred, want] of table) {
    expect(detectLocale(preferred), preferred.join(",")).toBe(want);
  }
});

it("applies a locale to vue-i18n, the document, and the shell", async () => {
  await applyLocale("zh-Hans");

  expect(i18n.global.locale.value).toBe("zh-Hans");
  expect(document.documentElement.lang).toBe("zh-Hans");
  expect(fakeInvoke).toHaveBeenCalledWith("set_locale", {
    locale: "zh-Hans",
  });
});

it("applies a locale with no shell to invoke", async () => {
  fakeInvoke.mockRejectedValue(new Error("not a Tauri window"));
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

  await applyLocale("zh-Hans");

  expect(i18n.global.locale.value).toBe("zh-Hans");
  expect(document.documentElement.lang).toBe("zh-Hans");
  expect(warn).toHaveBeenCalled();
  warn.mockRestore();
});
