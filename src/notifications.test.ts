import { beforeEach, expect, it, vi } from "vitest";

const sendNotification = vi.fn();

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("./test/fakeDaemon")).fakeInvoke,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => true),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: (...args: unknown[]) => sendNotification(...args),
}));

import { installFakeWebSocket, FakeWebSocket } from "./test/fakeDaemon";
import { useEvents } from "./composables/useEvents";
import { applyLocale } from "./locale";
import zhHans from "./locales/zh-Hans.json";
import { installNotifications } from "./notifications";

const { connect, disconnect } = useEvents();

function emit(kind: string, payload: unknown = {}): void {
  FakeWebSocket.instances[0].message(
    JSON.stringify({
      id: "e-1",
      kind,
      desk_id: "d-1",
      occurred_at_ns: 1,
      payload,
    }),
  );
}

let hidden = true;

beforeEach(() => {
  sendNotification.mockReset();
  hidden = true;
  vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
  vi.spyOn(document, "hasFocus").mockImplementation(() => !hidden);
  installFakeWebSocket();
  connect(7100, "b");
  FakeWebSocket.instances[0].open();
});

it("notifies an approval request only while the window is away", async () => {
  const stop = installNotifications(new Map([["d-1", "alpha"]]));

  emit("APPROVAL_REQUESTED", {
    kind: "PAPER_ORDER",
    side: "BUY",
    quantity: "100",
    instrument_id: "AAPL.XNAS",
  });
  await vi.waitFor(() => expect(sendNotification).toHaveBeenCalledTimes(1));
  expect(sendNotification.mock.calls[0][0]).toEqual({
    title: "alpha: approval needed",
    body: "BUY 100 AAPL.XNAS",
  });

  hidden = false;
  emit("APPROVAL_REQUESTED", { kind: "PAPER_ORDER" });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(sendNotification).toHaveBeenCalledTimes(1);

  stop();
  disconnect();
});

it("sends the title in the language applied at the time", async () => {
  await applyLocale("zh-Hans");
  const stop = installNotifications(new Map([["d-1", "alpha"]]));

  emit("DESK_FAILED", { failure_code: "WORKSPACE_UNWRITABLE" });
  await vi.waitFor(() => expect(sendNotification).toHaveBeenCalledTimes(1));
  expect(sendNotification.mock.calls[0][0].title).toBe(
    zhHans.notify.DESK_FAILED.title.replace("{desk}", "alpha"),
  );

  stop();
  disconnect();
  await applyLocale("en");
});

it("never notifies a routine result", async () => {
  const stop = installNotifications();

  emit("PROMPT_DELIVERED", { kind: "TRIGGER_RESULT" });
  emit("SKILLS_PROJECTED", {});
  emit("SESSION_ATTENTION", { kind: "session_start" });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(sendNotification).not.toHaveBeenCalled();

  stop();
  disconnect();
});
