<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useTerminal } from "../composables/useTerminal";
import { gutterClass, type DeskState } from "../deskState";

const props = defineProps<{
  deskId: string | null;
  deskName?: string;
  state: DeskState;
}>();
const { t } = useI18n();
const { mount, evict, panes } = useTerminal();

const slot = ref<HTMLDivElement | null>(null);
const hasPane = computed(
  () => props.deskId !== null && panes.has(props.deskId),
);

/** The Terminal is never recreated: only its element moves into the slot. */
async function attach(): Promise<void> {
  await nextTick();
  if (!slot.value) return;
  if (props.deskId && panes.has(props.deskId)) {
    mount(props.deskId, slot.value);
  } else {
    evict(slot.value);
  }
}

// A pane appears on SESSION_STARTED or when a reload finds the session
// live, and goes on SESSION_EXITED; the map is reactive, so this covers all.
watch([() => props.deskId, hasPane], attach);
onMounted(attach);
</script>

<template>
  <div class="relative flex overflow-hidden bg-well">
    <span class="w-[2px] shrink-0" :class="gutterClass[state]" />
    <div
      ref="slot"
      class="min-w-0 flex-1 overflow-hidden"
      :class="hasPane ? 'p-1' : ''"
      data-testid="well"
    >
      <div
        v-if="!hasPane"
        class="terminal relative flex h-full flex-col items-center justify-center gap-2 p-4 text-center text-state-idle"
      >
        <span
          aria-hidden="true"
          class="absolute inset-0 bg-[radial-gradient(var(--color-state-idle)_0.6px,transparent_0.7px)] bg-[size:16px_16px] bg-center opacity-15"
        />
        <p v-if="deskName" class="relative flex items-center gap-2 text-lg">
          <span>{{ deskName }}</span>
          <span aria-hidden="true">❯</span>
          <span aria-hidden="true" class="h-4 w-2 bg-accent" />
        </p>
        <p class="relative text-sm">
          {{ deskId ? t("well.noSession") : t("well.noDesk") }}
        </p>
      </div>
    </div>
  </div>
</template>
