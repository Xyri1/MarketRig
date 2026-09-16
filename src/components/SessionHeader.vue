<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import {
  SelectContent,
  SelectItem,
  SelectItemText,
  SelectPortal,
  SelectRoot,
  SelectTrigger,
  SelectValue,
  SelectViewport,
} from "reka-ui";
import {
  runtimes,
  session,
  sessionActivate,
  sessionExit,
  sessionInterrupt,
  sessionSwitch,
  show,
} from "../client";
import type { Desk, Envelope, Runtime } from "../client";
import { useEvents } from "../composables/useEvents";
import { useTerminal } from "../composables/useTerminal";
import {
  button,
  buttonPrimary,
  chip,
  failureText,
  selectContent,
  selectItem,
  selectTrigger,
} from "../deskState";

const props = defineProps<{ desk: Desk }>();
const { t } = useI18n();
const { on } = useEvents();
const { ensure } = useTerminal();

const process = ref<{ runtime: string } | null>(null);
const pointers = ref<Record<string, string>>({});
const others = ref<Runtime[]>([]);
const failure = ref("");
const busy = ref(false);
const switchTo = ref("");

const live = computed(() => process.value !== null);
const canContinue = computed(
  () => props.desk.selected_runtime in pointers.value,
);

async function refresh(): Promise<void> {
  const answer = await session({ path: { desk_id: props.desk.id } });
  process.value =
    (answer.data as { process?: { runtime: string } | null } | undefined)
      ?.process ?? null;
  // A reload finds the process already running: no SESSION_STARTED will
  // come, so a live answer attaches the pane here and the ring replays.
  if (process.value) ensure(props.desk.id);
  const desk = await show({ path: { desk_id: props.desk.id } });
  pointers.value =
    ((desk.data as Desk | undefined)?.native_sessions as Record<
      string,
      string
    >) ?? {};
  const all = await runtimes();
  others.value = (
    (all.data as { runtimes?: Runtime[] } | undefined)?.runtimes ?? []
  ).filter(
    (row) =>
      row.state === "AVAILABLE" && row.runtime !== props.desk.selected_runtime,
  );
}

/** Every control awaits the daemon and lets the refetch redraw (per D72). */
async function act(call: () => Promise<{ error?: unknown }>): Promise<void> {
  busy.value = true;
  failure.value = "";
  const answer = await call();
  busy.value = false;
  const envelope = answer.error as Envelope | undefined;
  if (envelope) failure.value = envelope.message;
  await refresh();
}

on(
  [
    "SESSION_STARTED",
    "SESSION_READY",
    "SESSION_POINTER_CHANGED",
    "SESSION_INTERRUPTED",
    "SESSION_EXITED",
    "RUNTIME_SWITCHED",
  ],
  () => void refresh(),
);

watch(
  () => props.desk.id,
  () => void refresh(),
);
watch(switchTo, (runtime) => {
  if (!runtime) return;
  void act(() =>
    sessionSwitch({ path: { desk_id: props.desk.id }, body: { runtime } }),
  );
  switchTo.value = "";
});
onMounted(() => void refresh());
</script>

<template>
  <header class="flex shrink-0 flex-col border-b border-line">
    <div class="flex h-10 items-center gap-2 px-3">
      <span class="terminal truncate font-semibold">{{ desk.name }}</span>
      <span :class="chip">{{ desk.selected_runtime }}</span>
      <span class="flex items-center gap-1.5 text-ink-muted">
        <span
          class="h-1.5 w-1.5 rounded-pill"
          :class="
            live
              ? 'bg-state-live shadow-[0_0_6px_var(--color-state-live)]'
              : 'bg-state-idle'
          "
        />
        {{ live ? t("session.live") : t("session.none") }}
      </span>
      <span class="flex-1" />
      <button
        v-if="!live"
        :class="buttonPrimary"
        :disabled="busy"
        data-testid="session-start"
        @click="
          act(() =>
            sessionActivate({
              path: { desk_id: desk.id },
              body: { mode: 'NEW' },
            }),
          )
        "
      >
        {{ t("session.start") }}
      </button>
      <button
        v-if="!live && canContinue"
        :class="button"
        :disabled="busy"
        @click="
          act(() =>
            sessionActivate({
              path: { desk_id: desk.id },
              body: { mode: 'CONTINUE' },
            }),
          )
        "
      >
        {{ t("session.continue") }}
      </button>
      <button
        :class="button"
        :disabled="busy || !live || desk.selected_runtime === 'claude'"
        :title="
          desk.selected_runtime === 'claude'
            ? t('session.interruptUnsupported')
            : t('session.interrupt')
        "
        @click="act(() => sessionInterrupt({ path: { desk_id: desk.id } }))"
      >
        {{ t("session.interrupt") }}
      </button>
      <button
        :class="button"
        :disabled="busy || !live"
        @click="act(() => sessionExit({ path: { desk_id: desk.id } }))"
      >
        {{ t("session.exit") }}
      </button>
      <SelectRoot v-model="switchTo" :disabled="busy || !others.length">
        <SelectTrigger :class="selectTrigger" :aria-label="t('session.switch')">
          <SelectValue :placeholder="t('session.switch')" />
          <span aria-hidden="true" class="text-ink-muted">▾</span>
        </SelectTrigger>
        <SelectPortal>
          <SelectContent :class="selectContent" position="popper" align="end">
            <SelectViewport>
              <SelectItem
                v-for="row in others"
                :key="row.runtime"
                :class="selectItem"
                :value="row.runtime"
              >
                <SelectItemText>{{ row.runtime }}</SelectItemText>
              </SelectItem>
            </SelectViewport>
          </SelectContent>
        </SelectPortal>
      </SelectRoot>
    </div>
    <p v-if="failure" :class="failureText" class="px-3 pb-2">{{ failure }}</p>
  </header>
</template>
