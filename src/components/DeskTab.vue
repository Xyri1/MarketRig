<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { book, openOrders, positions, quotes } from "../client";
import { useEvents } from "../composables/useEvents";

const props = defineProps<{ deskId: string }>();
const { t } = useI18n();
const { on } = useEvents();

const rows = ref<Record<string, unknown[]>>({});
const failure = ref("");

async function refresh(): Promise<void> {
  const path = { desk_id: props.deskId };
  const [q, b, p, o] = await Promise.all([
    quotes({ path }),
    book({ path }),
    positions({ path }),
    openOrders({ path }),
  ]);
  const first = [q, b, p, o].find((answer) => answer.error);
  failure.value = first ? (first.error as { message: string }).message : "";
  const read = (answer: { data?: unknown }, key: string) =>
    (answer.data as Record<string, unknown[]> | undefined)?.[key] ?? [];
  rows.value = {
    quotes: read(q, "quotes"),
    book: read(b, "book"),
    positions: read(p, "positions"),
    orders: read(o, "orders"),
  };
}

/** The four blocks, each with the fields that identify one of its rows. */
const BLOCKS = [
  { key: "quotes", ids: ["instrument_id"] },
  { key: "book", ids: ["instrument_id"] },
  { key: "positions", ids: ["position_id", "instrument_id"] },
  { key: "orders", ids: ["client_order_id"] },
] as const;

/** Verbatim: money and quantities are decimal text and are never reformatted. */
function text(value: unknown): string {
  return typeof value === "string" ? value : JSON.stringify(value);
}

function head(row: unknown, ids: readonly string[]): string {
  const item = row as Record<string, unknown>;
  return ids
    .map((key) => (key in item ? text(item[key]) : ""))
    .filter(Boolean)
    .join(" ");
}

function fields(row: unknown, ids: readonly string[]): string {
  return Object.entries(row as Record<string, unknown>)
    .filter(([key]) => !ids.includes(key))
    .map(([key, value]) => `${key} ${text(value)}`)
    .join("  ");
}

// No operational event kind reports a fill or a position, so the tab polls
// while it is visible and refetches on a decision (feature SPEC §6.3).
let timer: ReturnType<typeof setInterval> | null = null;
const off = on(["APPROVAL_DECIDED"], () => void refresh());

watch(() => props.deskId, refresh);
onMounted(() => {
  void refresh();
  timer = setInterval(() => void refresh(), 15_000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
  off();
});
</script>

<template>
  <div class="flex min-w-0 flex-col gap-3">
    <section v-for="block in BLOCKS" :key="block.key" class="flex flex-col">
      <p class="text-xs text-ink-muted">{{ t(`desk.${block.key}`) }}</p>
      <p v-if="!(rows[block.key] ?? []).length" class="text-xs text-ink-muted">
        {{ t(`desk.empty.${block.key}`) }}
      </p>
      <div
        v-for="(row, at) in rows[block.key] ?? []"
        :key="at"
        class="flex min-w-0 flex-col pt-1"
      >
        <p class="terminal text-sm wrap-anywhere">{{ head(row, block.ids) }}</p>
        <p class="terminal text-xs wrap-anywhere text-ink-muted">
          {{ fields(row, block.ids) }}
        </p>
      </div>
    </section>
    <pre
      v-if="failure"
      class="terminal text-xs wrap-anywhere whitespace-pre-wrap"
      >{{ failure }}</pre>
  </div>
</template>
