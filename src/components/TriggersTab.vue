<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { listTriggers, patchTrigger, triggerFirings } from "../client";
import { useEvents } from "../composables/useEvents";
import { button, chip } from "../deskState";

type Trigger = {
  id: string;
  name: string;
  recurrence: string;
  enabled: boolean;
  next_occurrence_ns?: number | null;
  code?: { approval?: string } | null;
};

const props = defineProps<{ deskId: string }>();
const { t } = useI18n();
const { on } = useEvents();

const triggers = ref<Trigger[]>([]);
const open = ref<string | null>(null);
const firings = ref<unknown[]>([]);

async function refresh(): Promise<void> {
  const answer = await listTriggers({ path: { desk_id: props.deskId } });
  triggers.value =
    (answer.data as { triggers?: Trigger[] } | undefined)?.triggers ?? [];
}

async function toggle(trigger: Trigger): Promise<void> {
  await patchTrigger({
    path: { desk_id: props.deskId, trigger_id: trigger.id },
    body: { enabled: !trigger.enabled },
  });
  await refresh();
}

async function drawer(trigger: Trigger): Promise<void> {
  if (open.value === trigger.id) {
    open.value = null;
    return;
  }
  const answer = await triggerFirings({
    path: { desk_id: props.deskId, trigger_id: trigger.id },
  });
  firings.value =
    (answer.data as { firings?: unknown[] } | undefined)?.firings ?? [];
  open.value = trigger.id;
}

function at(ns: number): string {
  return new Date(ns / 1e6).toLocaleString();
}

on(
  ["TRIGGER_MISSED", "APPROVAL_REQUESTED", "APPROVAL_DECIDED"],
  () => void refresh(),
);
watch(() => props.deskId, refresh);
onMounted(() => void refresh());
</script>

<template>
  <div class="flex flex-col gap-2">
    <p v-if="!triggers.length" class="text-ink-muted">
      {{ t("triggers.empty") }}
    </p>
    <div
      v-for="trigger in triggers"
      :key="trigger.id"
      class="flex flex-col gap-2 rounded-control border border-line bg-panel-raised p-2"
      :class="trigger.enabled ? '' : 'opacity-60'"
    >
      <div class="flex items-center gap-2">
        <button
          class="terminal min-w-0 flex-1 truncate rounded-control text-left focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent"
          :aria-expanded="open === trigger.id"
          @click="drawer(trigger)"
        >
          <span aria-hidden="true" class="mr-1 inline-block w-3 text-ink-muted">
            {{ open === trigger.id ? "▾" : "▸" }}
          </span>
          {{ trigger.name }}
        </button>
        <button :class="button" class="h-6 px-2" @click="toggle(trigger)">
          {{ trigger.enabled ? t("triggers.disable") : t("triggers.enable") }}
        </button>
      </div>
      <div class="flex flex-wrap items-center gap-1.5 pl-4">
        <span :class="chip">{{ trigger.recurrence }}</span>
        <span v-if="trigger.code" :class="chip">{{
          trigger.code.approval
        }}</span>
        <span
          v-if="trigger.next_occurrence_ns"
          class="terminal text-sm text-ink-muted"
        >
          {{ t("triggers.next") }} {{ at(trigger.next_occurrence_ns) }}
        </span>
      </div>
      <div
        v-if="open === trigger.id"
        class="flex flex-col gap-1 border-t border-line pt-2 pl-4 select-text"
      >
        <p v-if="!firings.length" class="text-sm text-ink-muted">
          {{ t("triggers.noFirings") }}
        </p>
        <pre
          v-for="(firing, at) in firings"
          :key="at"
          class="terminal text-sm wrap-anywhere whitespace-pre-wrap text-ink-muted"
          >{{ JSON.stringify(firing) }}</pre>
      </div>
    </div>
  </div>
</template>
