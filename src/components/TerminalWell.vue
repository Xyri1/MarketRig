<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useTerminal } from "../composables/useTerminal";
import { gutterClass, type DeskState } from "../deskState";

const props = defineProps<{ deskId: string | null; state: DeskState }>();
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
  <div class="flex overflow-hidden bg-well">
    <span class="w-[2px] shrink-0" :class="gutterClass[state]" />
    <div ref="slot" class="min-w-0 flex-1 overflow-hidden">
      <p v-if="!hasPane" class="terminal p-2 text-state-idle">
        {{ t("well.noSession") }}
      </p>
    </div>
  </div>
</template>
