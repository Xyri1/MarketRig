<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useEvents } from "../composables/useEvents";
import { useTerminal } from "../composables/useTerminal";
import { gutterClass, type DeskState } from "../deskState";

const props = defineProps<{ deskId: string | null; state: DeskState }>();
const { t } = useI18n();
const { on } = useEvents();
const { mount, panes } = useTerminal();

const slot = ref<HTMLDivElement | null>(null);
// `panes` is a plain Map; the two kinds that change it are the notification.
const tick = ref(0);
const hasPane = computed(() => {
  void tick.value;
  return props.deskId !== null && panes.has(props.deskId);
});

/** The Terminal is never recreated: only its element moves into the slot. */
async function attach(): Promise<void> {
  await nextTick();
  if (props.deskId && slot.value && panes.has(props.deskId)) {
    mount(props.deskId, slot.value);
  }
}

on(["SESSION_STARTED", "SESSION_EXITED"], () => {
  tick.value += 1;
  void attach();
});
watch(() => props.deskId, attach);
onMounted(attach);
</script>

<template>
  <div class="flex bg-well">
    <span class="w-[2px] shrink-0" :class="gutterClass[state]" />
    <div ref="slot" class="flex-1">
      <p v-if="!hasPane" class="terminal p-2 text-state-idle">
        {{ t("well.noSession") }}
      </p>
    </div>
  </div>
</template>
