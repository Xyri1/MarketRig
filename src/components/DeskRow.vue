<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { retry } from "../client";
import type { Desk } from "../client";
import { useApprovals } from "../composables/useApprovals";
import { useEvents } from "../composables/useEvents";
import { useTerminal } from "../composables/useTerminal";
import { button, deskState, gutterClass } from "../deskState";

const props = defineProps<{ desk: Desk; selected: boolean }>();
const { t } = useI18n();
const { byDesk } = useApprovals();
const { attention } = useEvents();
const { panes } = useTerminal();

const pending = computed(() => byDesk.get(props.desk.id) ?? 0);
const state = computed(() =>
  deskState(
    props.desk,
    panes.has(props.desk.id),
    pending.value,
    attention.has(props.desk.id),
  ),
);
</script>

<template>
  <div
    class="flex items-center gap-2 pr-2"
    :class="selected ? 'bg-panel' : ''"
    :data-state="state"
  >
    <span class="h-7 w-[2px] shrink-0" :class="gutterClass[state]" />
    <button
      class="terminal flex-1 truncate py-1 text-left focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent"
      :class="selected ? 'text-accent' : ''"
      @click="$emit('select')"
    >
      {{ desk.name }}
    </button>
    <span v-if="pending" class="text-xs text-ink-muted">{{ pending }}</span>
    <span
      v-if="attention.has(desk.id)"
      class="h-2 w-2 rounded-pill bg-state-attention"
      :aria-label="t('desks.attention')"
    />
    <button
      v-if="desk.state === 'FAILED'"
      :class="button"
      @click="retry({ path: { desk_id: desk.id } })"
    >
      {{ t("desks.retry") }}
    </button>
  </div>
</template>
