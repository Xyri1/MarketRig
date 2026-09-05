import { listen } from "@tauri-apps/api/event";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import i18n from "./i18n";
import { useDaemon } from "./composables/useDaemon";
import { useEvents, type DaemonEvent } from "./composables/useEvents";

/**
 * The kinds worth interrupting the operator for (feature SPEC §6.4). A routine
 * result — a fill, a delivery, a retain — never appears here (per D52).
 */
const KINDS = [
  "APPROVAL_REQUESTED",
  "SESSION_ATTENTION",
  "PROMPT_FAILED",
  "DESK_FAILED",
  "TRADING_NODE_FAILED",
  "RUNTIME_UNAVAILABLE",
  "CONTROL_PLANE_LOST",
  "MEMORY_UNAVAILABLE",
  "TRIGGER_MISSED",
];

// The catalog keys are built from the kind, so `t` is used untyped here.
const t = i18n.global.t as unknown as (
  key: string,
  named: Record<string, string>,
) => string;

let asked = false;

/** Asked once, at first need (feature SPEC §6.4). */
async function allowed(): Promise<boolean> {
  if (await isPermissionGranted()) return true;
  if (asked) return false;
  asked = true;
  return (await requestPermission()) === "granted";
}

/** One line of the payload the operator can act on, English as it is stored. */
function detail(payload: unknown): string {
  const object = (payload ?? {}) as Record<string, unknown>;
  for (const field of ["instrument_id", "failure_code", "title", "kind"]) {
    if (typeof object[field] === "string") {
      return field === "instrument_id"
        ? `${object.side} ${object.quantity} ${object.instrument_id}`
        : (object[field] as string);
    }
  }
  return JSON.stringify(object);
}

async function notify(
  event: DaemonEvent,
  deskNames: Map<string, string>,
): Promise<void> {
  // The window has the operator's eyes already: nothing to interrupt.
  if (!document.hidden && document.hasFocus()) return;
  // `session_start` is the runtime saying hello, not the session asking (§6.4).
  if (
    event.kind === "SESSION_ATTENTION" &&
    (event.payload as { kind?: string } | undefined)?.kind === "session_start"
  ) {
    return;
  }
  if (!(await allowed())) return;
  const named = {
    desk:
      (event.desk_id && deskNames.get(event.desk_id)) ?? event.desk_id ?? "",
    detail: detail(event.payload),
  };
  sendNotification({
    title: t(`notify.${event.kind}.title`, named),
    body: t(`notify.${event.kind}.body`, named),
  });
}

/**
 * Subscribes the §6.4 kinds; `deskNames` is the caller's desk id → name map,
 * read at send time, and a desk missing from it notifies by id.
 *
 * ponytail: a `TRIGGER_RESULT` prompt whose execution failed is not among them
 * — `PROMPT_DELIVERED` carries `prompt_id`, `kind`, `runtime`, and
 * `native_session_id` and no firing id, so the firing route cannot be reached
 * from the event. Restore it by adding `firing_id` to that payload.
 */
export function installNotifications(
  deskNames: Map<string, string> = new Map(),
): () => void {
  return useEvents().on(KINDS, (event) => void notify(event, deskNames));
}

/** The tray's *Quit MarketRig*, which the webview runs (feature SPEC §5). */
export function installQuitListener(): Promise<() => void> {
  return listen("marketrig://quit", () => void useDaemon().quit());
}
