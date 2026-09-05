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
import { button, buttonPrimary } from "../deskState";

const props = defineProps<{ desk: Desk }>();
const { t } = useI18n();
const { on } = useEvents();

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
  <header class="flex flex-col gap-1 border-b border-line px-2 py-2">
    <div class="flex items-center gap-2">
      <span class="terminal">{{ desk.name }}</span>
      <span class="terminal text-ink-muted">{{ desk.selected_runtime }}</span>
      <span class="text-ink-muted">{{
        live ? t("session.live") : t("session.none")
      }}</span>
      <span class="flex-1" />
      <button
        v-if="!live"
        :class="buttonPrimary"
        :disabled="busy"
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
        :disabled="busy || desk.selected_runtime === 'claude'"
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
        :disabled="busy"
        @click="act(() => sessionExit({ path: { desk_id: desk.id } }))"
      >
        {{ t("session.exit") }}
      </button>
      <SelectRoot v-model="switchTo" :disabled="busy || !others.length">
        <SelectTrigger :class="button" :aria-label="t('session.switch')">
          <SelectValue :placeholder="t('session.switch')" />
          <span aria-hidden="true">▾</span>
        </SelectTrigger>
        <SelectPortal>
          <SelectContent class="rounded-control border border-line bg-panel">
            <SelectViewport>
              <SelectItem
                v-for="row in others"
                :key="row.runtime"
                class="terminal px-2 py-1"
                :value="row.runtime"
              >
                <SelectItemText>{{ row.runtime }}</SelectItemText>
              </SelectItem>
            </SelectViewport>
          </SelectContent>
        </SelectPortal>
      </SelectRoot>
    </div>
    <pre
      v-if="failure"
      class="terminal text-xs wrap-anywhere whitespace-pre-wrap"
      >{{ failure }}</pre>
  </header>
</template>
