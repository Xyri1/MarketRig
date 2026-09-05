import { mount } from "@vue/test-utils";
import type { Component } from "vue";
import i18n from "../i18n";

export function mountWithI18n(
  component: Component,
  props?: Record<string, unknown>,
) {
  return mount(component, { props, global: { plugins: [i18n] } });
}
