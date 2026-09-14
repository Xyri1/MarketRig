import { invoke } from "@tauri-apps/api/core";
import i18n from "./i18n";

export const LOCALES = ["en", "zh-Hans"] as const;
export type Locale = (typeof LOCALES)[number];

/**
 * BCP 47 lookup over CLDR likely subtags against the shipped catalogs
 * (feature SPEC §2.1): `zh-CN` maximizes to `zh-Hans-CN` and truncates to
 * `zh-Hans`; `zh-TW` maximizes to `zh-Hant-TW`, which ships no catalog, so it
 * ends at `en` like any other unshipped language.
 */
export function detectLocale(preferred: readonly string[]): Locale {
  for (const tag of preferred) {
    let full: string;
    try {
      full = new Intl.Locale(tag).maximize().baseName; // zh-CN → zh-Hans-CN
    } catch {
      continue; // "" or a malformed tag
    }
    const parts = full.split("-");
    while (parts.length) {
      const candidate = parts.join("-");
      if ((LOCALES as readonly string[]).includes(candidate))
        return candidate as Locale;
      parts.pop();
    }
  }
  return "en";
}

/**
 * The one application path, called by startup and by the Settings control
 * (feature SPEC §2.3). Outside Tauri — Vitest, or a browser during `pnpm dev`
 * without the shell — `invoke` rejects and the first two lines still hold.
 */
export async function applyLocale(locale: Locale): Promise<void> {
  i18n.global.locale.value = locale;
  document.documentElement.lang = locale;
  try {
    await invoke("set_locale", { locale });
  } catch (failure) {
    console.warn("set_locale", failure);
  }
}
