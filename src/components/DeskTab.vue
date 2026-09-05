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
  <div class="flex flex-col gap-3">
    <section
      v-for="key in ['quotes', 'book', 'positions', 'orders']"
      :key="key"
    >
      <p class="text-xs text-ink-muted">{{ t(`desk.${key}`) }}</p>
      <pre
        v-for="(row, at) in rows[key] ?? []"
        :key="at"
        class="terminal text-xs wrap-anywhere"
        >{{ JSON.stringify(row) }}</pre>
    </section>
    <pre v-if="failure" class="terminal text-xs wrap-anywhere">{{
      failure
    }}</pre>
  </div>
</template>
