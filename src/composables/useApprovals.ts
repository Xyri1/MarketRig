import { computed, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { approvals, decideApproval } from "../client";
import type { Approval } from "../client";
import { useEvents } from "./useEvents";

const pending = ref<Approval[]>([]);
const byDesk = reactive(new Map<string, number>());
const total = computed(() => pending.value.length);

async function refetch(): Promise<void> {
  const answer = await approvals({ query: { state: "PENDING" } });
  const rows =
    (answer.data as { approvals?: Approval[] } | undefined)?.approvals ?? [];
  pending.value = rows;
  byDesk.clear();
  for (const row of rows) {
    byDesk.set(row.desk_id, (byDesk.get(row.desk_id) ?? 0) + 1);
  }
  await invoke("set_tray_pending", { n: rows.length });
}

/** Awaits the daemon and lets the APPROVAL_DECIDED refetch redraw (per D72). */
async function decide(
  deskId: string,
  id: string,
  decision: "APPROVE" | "DENY",
): Promise<void> {
  await decideApproval({ path: { desk_id: deskId, id }, body: { decision } });
}

let wired = false;

export function useApprovals() {
  if (!wired) {
    wired = true;
    useEvents().on(["APPROVAL_REQUESTED", "APPROVAL_DECIDED"], () => {
      void refetch();
    });
  }
  return { pending, byDesk, total, refetch, decide };
}
