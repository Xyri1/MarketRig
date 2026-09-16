import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));

import { flushPromises, type VueWrapper } from "@vue/test-utils";
import { client } from "../client/client.gen";
import { fakeInvoke, installFakeDaemon } from "../test/fakeDaemon";
import { mountWithI18n } from "../test/mountWithI18n";
import DeskRow from "./DeskRow.vue";

// Reka positions the menu with floating-ui, which observes its size; jsdom has
// no ResizeObserver, so the menu would throw on open without this.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

const desk = {
  id: "d-1",
  name: "alpha",
  state: "READY",
  selected_runtime: "codex",
  workspace_path: "/tmp/alpha",
  created_at_ns: 1,
};

const deletes: string[] = [];

beforeEach(() => {
  document.body.innerHTML = "";
  deletes.length = 0;
  fakeInvoke.mockReset();
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  installFakeDaemon({
    "DELETE /desks/d-1": (request) => {
      deletes.push(request.path);
      return { status: 204 };
    },
  });
});

/** Right-click the row; Reka's trigger opens on `contextmenu` alone. */
async function openMenu(wrapper: VueWrapper): Promise<HTMLElement[]> {
  await wrapper.get("[data-state]").trigger("contextmenu");
  await flushPromises();
  return [...document.body.querySelectorAll<HTMLElement>('[role="menuitem"]')];
}

it("offers the platform's file manager and calls the shell with the workspace", async () => {
  const wrapper = mountWithI18n(DeskRow, { desk, selected: false });
  const items = await openMenu(wrapper);
  expect(items.map((item) => item.textContent?.trim())).toEqual([
    // jsdom is not Windows, so the mac label is the one that renders.
    "Open in Finder",
    "Delete desk…",
  ]);
  items[0].click();
  await flushPromises();
  expect(fakeInvoke).toHaveBeenCalledWith("reveal_path", {
    path: "/tmp/alpha",
  });
});

it("deletes the desk once the dialog is confirmed and tells the list", async () => {
  const wrapper = mountWithI18n(DeskRow, { desk, selected: false });
  const items = await openMenu(wrapper);
  items[1].click();
  await flushPromises();
  expect(document.body.textContent).toContain("Delete alpha?");
  expect(wrapper.emitted("deleted")).toBeUndefined();
  document.body
    .querySelector<HTMLElement>('[data-testid="desk-delete-confirm"]')!
    .click();
  await flushPromises();
  expect(deletes).toEqual(["/desks/d-1"]);
  expect(wrapper.emitted("deleted")).toHaveLength(1);
});

it("keeps the dialog open on a refusal and shows the daemon's sentence", async () => {
  installFakeDaemon({
    "DELETE /desks/d-1": () => ({
      status: 409,
      body: { code: "DESK_BUSY", message: "The desk has a live session." },
    }),
  });
  const wrapper = mountWithI18n(DeskRow, { desk, selected: false });
  const items = await openMenu(wrapper);
  items[1].click();
  await flushPromises();
  document.body
    .querySelector<HTMLElement>('[data-testid="desk-delete-confirm"]')!
    .click();
  await flushPromises();
  expect(document.body.textContent).toContain("The desk has a live session.");
  expect(wrapper.emitted("deleted")).toBeUndefined();
});
