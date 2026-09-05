<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { list } from "./client";
import type { Desk } from "./client";
import { useApprovals } from "./composables/useApprovals";
import { useDaemon } from "./composables/useDaemon";
import { useEvents } from "./composables/useEvents";
import { useTerminal } from "./composables/useTerminal";
import { button, deskState } from "./deskState";
import DeskList from "./components/DeskList.vue";
import RightPanel from "./components/RightPanel.vue";
import SessionHeader from "./components/SessionHeader.vue";
import TerminalWell from "./components/TerminalWell.vue";
import ApprovalsTab from "./components/ApprovalsTab.vue";
import SettingsTab from "./components/SettingsTab.vue";
import { installNotifications, installQuitListener } from "./notifications";

const { t } = useI18n();
const { status, endpoint, error, boot, retry } = useDaemon();
const { connect, on, attention } = useEvents();
const { byDesk, refetch: refetchApprovals } = useApprovals();
const { panes } = useTerminal();

const desks = ref<Desk[]>([]);
const deskNames = new Map<string, string>();
const selected = ref<string | null>(null);
const selectedDesk = computed(
  () => desks.value.find((desk) => desk.id === selected.value) ?? null,
);
const selectedState = computed(() =>
  selectedDesk.value
    ? deskState(
        selectedDesk.value,
        panes.has(selectedDesk.value.id),
        byDesk.get(selectedDesk.value.id) ?? 0,
        attention.has(selectedDesk.value.id),
      )
    : "idle",
);

async function refresh(): Promise<void> {
  const answer = await list();
  desks.value = (answer.data as { desks?: Desk[] } | undefined)?.desks ?? [];
  for (const desk of desks.value) deskNames.set(desk.id, desk.name);
  if (!desks.value.some((desk) => desk.id === selected.value)) {
    selected.value = desks.value[0]?.id ?? null;
  }
}

on(
  [
    "DESK_CREATED",
    "DESK_READY",
    "DESK_FAILED",
    "DESK_RETRIED",
    "RUNTIME_SWITCHED",
    "SESSION_STARTED",
    "SESSION_EXITED",
  ],
  () => void refresh(),
);

watch(status, (now) => {
  if (now !== "READY" || !endpoint.value) return;
  connect(endpoint.value.port, endpoint.value.bearer);
  void refresh();
  void refetchApprovals();
});

onMounted(() => {
  installNotifications(deskNames);
  void installQuitListener();
  void boot();
});
</script>

<template>
  <main v-if="status === 'STARTING'" class="p-4">
    {{ t("daemon.starting") }}
  </main>
  <main v-else-if="status === 'UNAVAILABLE'" class="p-4">
    <p>{{ t("daemon.unavailable") }}</p>
    <pre
      class="terminal text-xs wrap-anywhere whitespace-pre-wrap text-ink-muted"
      >{{ error }}</pre>
    <button :class="button" @click="retry()">{{ t("daemon.retry") }}</button>
  </main>
  <main v-else class="flex h-full overflow-x-hidden">
    <DeskList
      class="w-60 shrink-0"
      :desks="desks"
      :selected="selected"
      @select="selected = $event"
    />
    <section class="flex min-w-[480px] flex-1 flex-col overflow-hidden">
      <SessionHeader v-if="selectedDesk" :desk="selectedDesk" />
      <TerminalWell class="flex-1" :desk-id="selected" :state="selectedState" />
    </section>
    <RightPanel :desk-id="selected">
      <template #approvals><ApprovalsTab /></template>
      <template #settings><SettingsTab /></template>
    </RightPanel>
  </main>
</template>
