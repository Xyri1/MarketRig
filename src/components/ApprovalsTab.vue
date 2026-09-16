<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogRoot,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "reka-ui";
import { approval } from "../client";
import type { Approval, Envelope } from "../client";
import { useApprovals } from "../composables/useApprovals";
import {
  button,
  buttonPrimary,
  chip,
  dialogContent,
  dialogOverlay,
  eyebrow,
  failureText,
} from "../deskState";

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

/** The order's terms as the daemon stored them, one flat key per line. */
function request(item: Approval): [string, string][] {
  const out: [string, string][] = [];
  const walk = (value: unknown, at: string) => {
    if (value && typeof value === "object" && !Array.isArray(value)) {
      for (const [key, inner] of Object.entries(value))
        walk(inner, at ? `${at}.${key}` : key);
    } else {
      out.push([at, typeof value === "string" ? value : JSON.stringify(value)]);
    }
  };
  walk(item.detail, "");
  return out;
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
  <div class="flex flex-col gap-3">
    <p v-if="pending.length === 0" class="text-ink-muted">
      {{ t("approvals.empty") }}
    </p>
    <div
      v-for="item in pending"
      :key="item.id"
      class="flex flex-col gap-2 rounded-control border border-line bg-panel-raised p-3"
      data-testid="approval-row"
    >
      <div class="flex items-center gap-2">
        <span :class="eyebrow">{{ t(`approvals.kind.${item.kind}`) }}</span>
        <span :class="chip">{{ item.desk_name }}</span>
        <span class="flex-1" />
        <span class="terminal text-sm text-ink-muted">{{
          requestedAt(item)
        }}</span>
      </div>

      <button
        v-if="item.kind === 'TRIGGER_CODE'"
        type="button"
        :class="button"
        class="self-start"
        @click="toggle(item)"
      >
        {{
          t(sources.has(item.id) ? "approvals.hideCode" : "approvals.showCode")
        }}
      </button>
      <pre
        v-if="sources.has(item.id)"
        class="terminal max-h-80 overflow-y-auto rounded-control bg-well p-2 text-sm wrap-anywhere whitespace-pre-wrap text-state-idle select-text"
        >{{ sources.get(item.id) }}</pre>
      <dl
        v-else-if="item.kind !== 'TRIGGER_CODE'"
        class="terminal grid grid-cols-[auto_1fr] gap-x-3 text-sm select-text"
      >
        <template v-for="[key, value] in request(item)" :key="key">
          <dt class="text-ink-muted">{{ key }}</dt>
          <dd class="wrap-anywhere">{{ value }}</dd>
        </template>
      </dl>

      <div class="flex gap-2">
        <button
          type="button"
          :class="buttonPrimary"
          data-testid="approval-approve"
          :disabled="deciding === item.id"
          @click="settle(item, 'APPROVE')"
        >
          {{ t("approvals.approve") }}
        </button>
        <AlertDialogRoot>
          <AlertDialogTrigger
            :class="button"
            data-testid="approval-deny"
            :disabled="deciding === item.id"
          >
            {{ t("approvals.deny") }}
          </AlertDialogTrigger>
          <AlertDialogPortal>
            <AlertDialogOverlay :class="dialogOverlay" />
            <AlertDialogContent :class="dialogContent">
              <div class="flex flex-col gap-1">
                <AlertDialogTitle class="text-lg font-semibold">{{
                  t("dialog.denyTitle")
                }}</AlertDialogTitle>
                <AlertDialogDescription class="text-ink-muted">
                  {{ t("dialog.denyBody") }}
                </AlertDialogDescription>
              </div>
              <div class="flex justify-end gap-2">
                <AlertDialogCancel :class="button">
                  {{ t("dialog.cancel") }}
                </AlertDialogCancel>
                <AlertDialogAction
                  :class="buttonPrimary"
                  data-testid="dialog-deny"
                  @click="settle(item, 'DENY')"
                >
                  {{ t("dialog.deny") }}
                </AlertDialogAction>
              </div>
            </AlertDialogContent>
          </AlertDialogPortal>
        </AlertDialogRoot>
      </div>

      <p v-if="failures.has(item.id)" :class="failureText">
        {{ failures.get(item.id) }}
      </p>
    </div>
  </div>
</template>
