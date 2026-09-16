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

function payload(row: DaemonEvent): string {
  return JSON.stringify(row.payload ?? {});
}

on("*", (event) => {
  if (event.desk_id === props.deskId) void refresh();
});
watch(() => props.deskId, refresh);
onMounted(() => void refresh());
</script>

<template>
  <div class="flex flex-col gap-3">
    <p v-if="!rows.length" class="text-ink-muted">{{ t("activity.empty") }}</p>
    <ol class="flex flex-col select-text">
      <li
        v-for="row in rows"
        :key="row.id"
        class="flex flex-col gap-0.5 border-l border-line py-1.5 pl-3"
      >
        <p class="terminal flex gap-2 text-sm">
          <span class="text-ink-muted">{{ at(row.occurred_at_ns) }}</span>
          <span>{{ row.kind }}</span>
        </p>
        <pre
          class="terminal text-sm wrap-anywhere whitespace-pre-wrap text-ink-muted"
          >{{ payload(row) }}</pre>
      </li>
    </ol>
    <button v-if="before" :class="button" class="self-start" @click="more()">
      {{ t("activity.more") }}
    </button>
  </div>
</template>
