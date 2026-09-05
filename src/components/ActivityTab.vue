<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { events } from "../client";
import type { DaemonEvent } from "../composables/useEvents";
import { useEvents } from "../composables/useEvents";
import { button } from "../deskState";

const props = defineProps<{ deskId: string }>();
const { t } = useI18n();
const { on } = useEvents();

const rows = ref<DaemonEvent[]>([]);
const before = ref<string | null>(null);

/** A live row prepends by refetching the first page, never by insertion. */
async function refresh(): Promise<void> {
  const answer = await events({
    query: { desk_id: props.deskId, limit: 100 },
  });
  const body = answer.data as
    { events?: DaemonEvent[]; next_before?: string } | undefined;
  rows.value = body?.events ?? [];
  before.value = body?.next_before ?? null;
}

async function more(): Promise<void> {
  if (!before.value) return;
  const answer = await events({
    query: { desk_id: props.deskId, limit: 100, before: before.value },
  });
  const body = answer.data as
    { events?: DaemonEvent[]; next_before?: string } | undefined;
  rows.value = [...rows.value, ...(body?.events ?? [])];
  before.value = body?.next_before ?? null;
}

function at(ns: number): string {
  return new Date(ns / 1e6).toLocaleTimeString();
}

on("*", (event) => {
  if (event.desk_id === props.deskId) void refresh();
});
watch(() => props.deskId, refresh);
onMounted(() => void refresh());
</script>

<template>
  <div class="flex flex-col gap-1">
    <div v-for="row in rows" :key="row.id" class="flex flex-col">
      <p class="terminal text-xs text-ink-muted">
        {{ at(row.occurred_at_ns) }} {{ row.kind }}
      </p>
      <pre class="terminal text-xs wrap-anywhere whitespace-pre-wrap">{{
        JSON.stringify(row.payload ?? {})
      }}</pre>
    </div>
    <button v-if="before" :class="button" @click="more()">
      {{ t("activity.more") }}
    </button>
  </div>
</template>
