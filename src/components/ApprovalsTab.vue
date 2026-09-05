<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogRoot,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "reka-ui";
import { approval } from "../client";
import type { Approval, Envelope } from "../client";
import { useApprovals } from "../composables/useApprovals";

const { t } = useI18n();
const { pending, refetch, decide } = useApprovals();

// ponytail: three plain maps keyed by approval id; a store buys nothing here.
const deciding = ref("");
const failures = reactive(new Map<string, string>());
const sources = reactive(new Map<string, string>());

onMounted(() => void refetch());

function requestedAt(item: Approval): string {
  return new Date(item.requested_at_ns / 1_000_000).toLocaleString();
}

/** The order's terms, one line, as the daemon stored them. */
function request(item: Approval): string {
  return JSON.stringify(item.detail);
}

/** The snapshot's source, fetched once, on the first expand (feature §3.1). */
async function toggle(item: Approval): Promise<void> {
  if (sources.has(item.id)) {
    sources.delete(item.id);
    return;
  }
  const answer = await approval({ path: { id: item.id } });
  if (answer.error) {
    failures.set(item.id, (answer.error as Envelope).message);
    return;
  }
  const detail = answer.data?.detail as { source?: string } | undefined;
  sources.set(item.id, detail?.source ?? "");
}

async function settle(
  item: Approval,
  decision: "APPROVE" | "DENY",
): Promise<void> {
  deciding.value = item.id;
  failures.delete(item.id);
  const refused = await decide(item.desk_id, item.id, decision);
  if (refused) failures.set(item.id, refused.message);
  deciding.value = "";
}
</script>

<template>
  <div class="flex flex-col gap-4 p-4">
    <p v-if="pending.length === 0" class="text-ink-muted">
      {{ t("approvals.empty") }}
    </p>
    <div
      v-for="item in pending"
      :key="item.id"
      class="flex flex-col gap-2 border-b border-line pb-4"
    >
      <div class="flex items-baseline gap-2">
        <span>{{ t(`approvals.kind.${item.kind}`) }}</span>
        <span class="terminal text-sm">{{ item.desk_name }}</span>
        <span class="terminal text-xs text-ink-muted">{{
          requestedAt(item)
        }}</span>
      </div>

      <button
        v-if="item.kind === 'TRIGGER_CODE'"
        type="button"
        class="self-start rounded-control border border-line px-2 py-1"
        @click="toggle(item)"
      >
        {{
          t(sources.has(item.id) ? "approvals.hideCode" : "approvals.showCode")
        }}
      </button>
      <pre
        v-if="sources.has(item.id)"
        class="terminal text-xs wrap-anywhere whitespace-pre-wrap"
        >{{ sources.get(item.id) }}</pre>
      <pre
        v-else-if="item.kind !== 'TRIGGER_CODE'"
        class="terminal text-xs wrap-anywhere whitespace-pre-wrap"
        >{{ request(item) }}</pre>

      <div class="flex gap-2">
        <button
          type="button"
          class="rounded-control bg-accent px-2 py-1 text-accent-ink"
          :disabled="deciding === item.id"
          @click="settle(item, 'APPROVE')"
        >
          {{ t("approvals.approve") }}
        </button>
        <AlertDialogRoot>
          <AlertDialogTrigger
            class="rounded-control border border-line px-2 py-1"
            :disabled="deciding === item.id"
          >
            {{ t("approvals.deny") }}
          </AlertDialogTrigger>
          <AlertDialogPortal>
            <AlertDialogOverlay class="fixed inset-0" />
            <AlertDialogContent
              class="fixed top-1/2 left-1/2 flex -translate-x-1/2 -translate-y-1/2 flex-col gap-3 rounded-panel border border-line bg-panel p-4"
            >
              <AlertDialogTitle>{{ t("dialog.denyTitle") }}</AlertDialogTitle>
              <div class="flex gap-2">
                <AlertDialogAction
                  class="rounded-control border border-line px-2 py-1"
                  @click="settle(item, 'DENY')"
                >
                  {{ t("dialog.deny") }}
                </AlertDialogAction>
                <AlertDialogCancel
                  class="rounded-control border border-line px-2 py-1"
                >
                  {{ t("dialog.cancel") }}
                </AlertDialogCancel>
              </div>
            </AlertDialogContent>
          </AlertDialogPortal>
        </AlertDialogRoot>
      </div>

      <p v-if="failures.has(item.id)" class="terminal text-xs">
        {{ failures.get(item.id) }}
      </p>
    </div>
  </div>
</template>
