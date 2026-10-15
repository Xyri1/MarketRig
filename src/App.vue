<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { list, locale, putLocale, runtimes } from "./client";
import type { Desk, Runtime } from "./client";
import { applyLocale, detectLocale, type Locale } from "./locale";
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
import OnboardingGate from "./components/OnboardingGate.vue";
import SettingsTab from "./components/SettingsTab.vue";
import { installNotifications, installQuitListener } from "./notifications";

const { t, locale: active } = useI18n();
const { status, endpoint, error, boot, retry } = useDaemon();
const { connect, on, attention } = useEvents();
const { byDesk, refetch: refetchApprovals } = useApprovals();
const { panes } = useTerminal();

const desks = ref<Desk[]>([]);
// First launch gates the window until Continue: no runtime is AVAILABLE yet
// (SPEC §6.3). The runtime state is the whole test; nothing is stored.
const onboarding = ref(false);
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

/**
 * The stored language, or the one detected from the system and written once
 * (feature SPEC §2.2). A failed write applies the detected value anyway for
 * this run and leaves the column null for the next launch to try again.
 */
async function startLocale(): Promise<void> {
  // A Language change made while these requests are in flight wins: the
  // shell starts in `en`, so anything else is the operator's own choice, and
  // startup neither overwrites it nor stores its detection over it.
  const untouched = () => active.value === "en";
  const stored = (await locale()).data?.locale as Locale | null | undefined;
  if (!untouched()) return;
  if (stored) return applyLocale(stored);
  const detected = detectLocale(navigator.languages);
  try {
    await putLocale({ body: { locale: detected } });
  } catch {
    // The daemon is the one that remembers; this run renders either way.
  }
  if (untouched()) await applyLocale(detected);
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
  void runtimes().then((answer) => {
    const rows = (answer.data as { runtimes?: Runtime[] })?.runtimes ?? [];
    onboarding.value = !rows.some((row) => row.state === "AVAILABLE");
  });
  void refresh();
  void refetchApprovals();
  void startLocale();
});

onMounted(() => {
  installNotifications(deskNames);
  void installQuitListener();
  void boot();
});
</script>

<template>
  <main
    v-if="status !== 'READY'"
    class="flex h-full items-center justify-center p-8"
  >
    <div class="flex w-96 flex-col gap-3">
      <p class="terminal text-lg font-semibold">{{ t("app.name") }}</p>
      <p v-if="status === 'STARTING'" class="text-ink-muted">
        {{ t("daemon.starting") }}
      </p>
      <template v-else>
        <p>{{ t("daemon.unavailable") }}</p>
        <pre
          v-if="error"
          class="terminal max-h-48 overflow-y-auto rounded-control border border-line bg-panel p-2 text-sm wrap-anywhere whitespace-pre-wrap text-ink-muted"
          >{{ error }}</pre>
        <button :class="button" class="self-start" @click="retry()">
          {{ t("daemon.retry") }}
        </button>
      </template>
    </div>
  </main>
  <OnboardingGate v-else-if="onboarding" @done="onboarding = false" />
  <main v-else class="flex h-full overflow-x-hidden">
    <DeskList
      class="w-60 shrink-0"
      :desks="desks"
      :selected="selected"
      @select="selected = $event"
      @deleted="refresh()"
    />
    <section class="flex min-w-[480px] flex-1 flex-col overflow-hidden">
      <SessionHeader v-if="selectedDesk" :desk="selectedDesk" />
      <TerminalWell
        class="mx-2 mb-2 flex-1 rounded-panel border border-line-strong/60 shadow-inner"
        :desk-id="selected"
        :desk-name="selectedDesk?.name"
        :state="selectedState"
      />
    </section>
    <RightPanel :desk-id="selected">
      <template #approvals><ApprovalsTab /></template>
      <template #settings><SettingsTab /></template>
    </RightPanel>
  </main>
</template>
