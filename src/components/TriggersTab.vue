<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { listTriggers, patchTrigger, triggerFirings } from "../client";
import { useEvents } from "../composables/useEvents";
import { button } from "../deskState";

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
      class="flex flex-col gap-1"
    >
      <div class="flex items-center gap-2">
        <button
          class="terminal flex-1 truncate text-left"
          @click="drawer(trigger)"
        >
          {{ trigger.name }}
        </button>
        <span class="terminal text-xs text-ink-muted">{{
          trigger.recurrence
        }}</span>
        <span v-if="trigger.code" class="terminal text-xs text-ink-muted">{{
          trigger.code.approval
        }}</span>
        <button :class="button" @click="toggle(trigger)">
          {{ trigger.enabled ? t("triggers.disable") : t("triggers.enable") }}
        </button>
      </div>
      <p class="terminal text-xs text-ink-muted">
        {{ trigger.next_occurrence_ns ?? "" }}
      </p>
      <pre
        v-for="(firing, at) in open === trigger.id ? firings : []"
        :key="at"
        class="terminal text-xs wrap-anywhere whitespace-pre-wrap"
        >{{ JSON.stringify(firing) }}</pre>
    </div>
  </div>
</template>
