import { mount } from "@vue/test-utils";
import i18n from "../i18n";

/** `mount` with the one plugin every component needs. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function mountWithI18n(component: any, props?: Record<string, unknown>) {
  return mount(component, { global: { plugins: [i18n] }, props });
}
