import { createI18n } from "vue-i18n";
import en from "./locales/en.json";
import zhHans from "./locales/zh-Hans.json";

export type MessageSchema = typeof en;

// The third parameter is `Legacy`, which defaults to `true`: without it
// `i18n.global` is the legacy instance, whose `locale` is a plain string and
// not the writable ref `applyLocale` sets (feature SPEC §2.3).
export default createI18n<[MessageSchema], "en" | "zh-Hans", false>({
  legacy: false,
  locale: "en",
  fallbackLocale: "en",
  messages: { en, "zh-Hans": zhHans },
});
