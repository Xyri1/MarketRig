<script setup lang="ts">
import { onMounted, onUnmounted, reactive, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogRoot,
  AlertDialogTitle,
  AlertDialogTrigger,
  SelectContent,
  SelectItem,
  SelectItemText,
  SelectRoot,
  SelectTrigger,
  SelectValue,
  SelectViewport,
} from "reka-ui";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import {
  memoryDiscover,
  memoryModels,
  memoryProvider,
  memoryRetry,
  memoryStatus,
  policies,
  putPolicies,
  runtimeDiscover,
  runtimeRetry,
  runtimes as listRuntimes,
} from "../client";
import type { Envelope, Resource, Runtime, Status } from "../client";
import { useDaemon } from "../composables/useDaemon";
import { useEvents } from "../composables/useEvents";
import { selectTrigger } from "../deskState";

const { t } = useI18n();
const { on } = useEvents();
const { quit } = useDaemon();

const rows = ref<Runtime[]>([]);
const explicit = reactive(new Map<string, string>());
const memory = ref<Status | null>(null);
const memoryPath = ref("");
const policy = ref<Resource | null>(null);
const models = ref<string[]>([]);
const autostart = ref(false);
const failure = ref("");
// The key is write-only: the daemon never returns it (R4 §3).
const form = reactive({ base_url: "", api_key: "", llm: "", embedding: "" });

/** Every route answers `{data, error}`; one place turns a refusal into text. */
function refused(error: unknown): boolean {
  if (!error) return false;
  failure.value = (error as Envelope).message;
  return true;
}

async function loadRuntimes(): Promise<void> {
  const answer = await listRuntimes();
  if (refused(answer.error)) return;
  rows.value = (answer.data as { runtimes?: Runtime[] })?.runtimes ?? [];
}

async function loadMemory(): Promise<void> {
  const answer = await memoryStatus();
  if (refused(answer.error)) return;
  memory.value = answer.data ?? null;
  form.base_url = answer.data?.provider.base_url ?? "";
  form.llm = answer.data?.provider.llm_model ?? "";
  form.embedding = answer.data?.provider.embedding_model ?? "";
}

async function loadPolicies(): Promise<void> {
  const answer = await policies();
  if (refused(answer.error)) return;
  policy.value = answer.data ?? null;
}

async function discover(runtime: string, path?: string): Promise<void> {
  const body = path ? { executable: path } : {};
  refused((await runtimeDiscover({ path: { runtime }, body })).error);
  await loadRuntimes();
}

async function retry(runtime: string): Promise<void> {
  refused((await runtimeRetry({ path: { runtime } })).error);
  await loadRuntimes();
}

async function discoverMemory(): Promise<void> {
  refused(
    (await memoryDiscover({ body: { executable: memoryPath.value } })).error,
  );
  await loadMemory();
}

async function retryMemory(): Promise<void> {
  refused((await memoryRetry()).error);
  await loadMemory();
}

/** Live on open, never cached (R4 §3). */
async function openModels(open: boolean): Promise<void> {
  if (!open) return;
  const answer = await memoryModels();
  if (refused(answer.error)) return;
  models.value = (answer.data as { models?: string[] })?.models ?? [];
}

async function saveProvider(): Promise<void> {
  const body: Record<string, string> = {
    base_url: form.base_url,
    llm_model: form.llm,
    embedding_model: form.embedding,
  };
  if (form.api_key) body.api_key = form.api_key;
  if (!refused((await memoryProvider({ body })).error)) form.api_key = "";
  await loadMemory();
}

/** Only the changed field is sent; the refetch redraws (per D72). */
async function setPolicy(field: string, value: string): Promise<void> {
  if (policy.value?.[field as keyof Resource] === value) return;
  refused((await putPolicies({ body: { [field]: value } })).error);
  await loadPolicies();
}

async function toggleAutostart(on: boolean): Promise<void> {
  await (on ? enable() : disable());
  autostart.value = await isEnabled();
}

const off = [
  on(["RUNTIME_DISCOVERED", "RUNTIME_UNAVAILABLE"], () => void loadRuntimes()),
  on(
    [
      "MEMORY_CONFIGURED",
      "MEMORY_STARTED",
      "MEMORY_LOST",
      "MEMORY_UNAVAILABLE",
    ],
    () => void loadMemory(),
  ),
  on("POLICY_CHANGED", () => void loadPolicies()),
];
onUnmounted(() => off.forEach((stop) => stop()));

onMounted(async () => {
  await Promise.all([loadRuntimes(), loadMemory(), loadPolicies()]);
  autostart.value = await isEnabled();
});
</script>

<template>
  <div class="flex flex-col gap-6 p-4">
    <section class="flex flex-col gap-2">
      <p class="text-xs text-ink-muted">{{ t("settings.runtimes.title") }}</p>
      <div
        v-for="row in rows"
        :key="row.runtime"
        class="flex flex-col gap-1 border-b border-line pb-2"
      >
        <p class="terminal text-sm wrap-anywhere">
          {{ row.runtime }} {{ row.executable_path }} {{ row.version }}
          {{ row.state }}
        </p>
        <div class="flex gap-2">
          <button
            type="button"
            class="rounded-control border border-line px-2 py-1"
            @click="discover(row.runtime)"
          >
            {{ t("settings.runtimes.discover") }}
          </button>
          <button
            v-if="row.state === 'UNAVAILABLE'"
            type="button"
            class="rounded-control border border-line px-2 py-1"
            @click="retry(row.runtime)"
          >
            {{ t("settings.runtimes.retry") }}
          </button>
        </div>
        <form
          class="flex gap-2"
          @submit.prevent="discover(row.runtime, explicit.get(row.runtime))"
        >
          <input
            class="terminal flex-1 rounded-control border border-line px-2 py-1"
            :aria-label="t('settings.runtimes.path')"
            :placeholder="t('settings.runtimes.path')"
            :value="explicit.get(row.runtime) ?? ''"
            @input="
              explicit.set(
                row.runtime,
                ($event.target as HTMLInputElement).value,
              )
            "
          />
          <button
            type="submit"
            class="rounded-control border border-line px-2 py-1"
          >
            {{ t("settings.runtimes.submit") }}
          </button>
        </form>
      </div>
    </section>

    <section v-if="memory" class="flex flex-col gap-2">
      <p class="text-xs text-ink-muted">{{ t("settings.memory.title") }}</p>
      <p class="terminal text-sm wrap-anywhere">
        {{ memory.child.state }} {{ memory.child.live }}
        {{ memory.child.executable_path }}
      </p>
      <p class="terminal text-sm wrap-anywhere">
        {{ memory.provider.base_url }} {{ memory.provider.llm_model }}
        {{ memory.provider.embedding_model }}
      </p>
      <p class="text-xs text-ink-muted">
        {{
          t(
            memory.provider.api_key_present
              ? "settings.memory.keySet"
              : "settings.memory.keyUnset",
          )
        }}
      </p>
      <form class="flex gap-2" @submit.prevent="discoverMemory()">
        <input
          v-model="memoryPath"
          class="terminal flex-1 rounded-control border border-line px-2 py-1"
          :aria-label="t('settings.memory.path')"
          :placeholder="t('settings.memory.path')"
        />
        <button
          type="submit"
          class="rounded-control border border-line px-2 py-1"
        >
          {{ t("settings.memory.discover") }}
        </button>
        <button
          type="button"
          class="rounded-control border border-line px-2 py-1"
          @click="retryMemory()"
        >
          {{ t("settings.memory.retry") }}
        </button>
      </form>
      <form class="flex flex-col gap-2" @submit.prevent="saveProvider()">
        <input
          v-model="form.base_url"
          class="terminal rounded-control border border-line px-2 py-1"
          :aria-label="t('settings.memory.baseUrl')"
          :placeholder="t('settings.memory.baseUrl')"
        />
        <input
          v-model="form.api_key"
          type="password"
          class="terminal rounded-control border border-line px-2 py-1"
          :aria-label="t('settings.memory.apiKey')"
          :placeholder="t('settings.memory.apiKey')"
        />
        <label
          v-for="field in [
            { key: 'llm', label: 'settings.memory.llmModel' },
            { key: 'embedding', label: 'settings.memory.embeddingModel' },
          ]"
          :key="field.key"
          class="flex items-center gap-2"
        >
          <span class="text-xs text-ink-muted">{{ t(field.label) }}</span>
          <SelectRoot
            :model-value="form[field.key as 'llm' | 'embedding']"
            @update:model-value="
              form[field.key as 'llm' | 'embedding'] = $event as string
            "
            @update:open="openModels($event)"
          >
            <SelectTrigger :class="`terminal ${selectTrigger}`">
              <SelectValue />
              <span aria-hidden="true">▾</span>
            </SelectTrigger>
            <SelectContent
              class="rounded-panel border border-line bg-panel p-1"
            >
              <SelectViewport>
                <SelectItem
                  v-for="model in models"
                  :key="model"
                  :value="model"
                  class="terminal px-2 py-1"
                >
                  <SelectItemText>{{ model }}</SelectItemText>
                </SelectItem>
              </SelectViewport>
            </SelectContent>
          </SelectRoot>
        </label>
        <button
          type="submit"
          class="self-start rounded-control border border-line px-2 py-1"
        >
          {{ t("settings.memory.save") }}
        </button>
      </form>
    </section>

    <section v-if="policy" class="flex flex-col gap-2">
      <p class="text-xs text-ink-muted">{{ t("settings.policies.title") }}</p>
      <label
        v-for="row in [
          {
            field: 'trigger_code_policy' as const,
            label: 'settings.policies.triggerCode',
          },
          {
            field: 'paper_order_policy' as const,
            label: 'settings.policies.paperOrder',
          },
        ]"
        :key="row.field"
        class="flex items-center gap-2"
      >
        <span class="text-xs text-ink-muted">{{ t(row.label) }}</span>
        <SelectRoot
          :model-value="policy[row.field]"
          @update:model-value="setPolicy(row.field, $event as string)"
        >
          <SelectTrigger :class="selectTrigger">
            <SelectValue />
            <span aria-hidden="true">▾</span>
          </SelectTrigger>
          <SelectContent class="rounded-panel border border-line bg-panel p-1">
            <SelectViewport>
              <SelectItem
                v-for="value in ['ALWAYS_ALLOW', 'REQUIRE_APPROVAL']"
                :key="value"
                :value="value"
                class="px-2 py-1"
              >
                <SelectItemText>{{
                  t(`settings.policy.${value}`)
                }}</SelectItemText>
              </SelectItem>
            </SelectViewport>
          </SelectContent>
        </SelectRoot>
      </label>
      <!--
        Delivery admits only QUEUE, so it is a plain disabled select: it never
        opens, and Steer stays visibly refused (feature SPEC §6.3, per D70).
      -->
      <label class="flex items-center gap-2">
        <span class="text-xs text-ink-muted">{{
          t("settings.policies.delivery")
        }}</span>
        <select
          disabled
          class="rounded-control border border-line px-2 py-1"
          :value="policy.delivery_mode"
          :aria-label="t('settings.policies.delivery')"
        >
          <option value="QUEUE">{{ t("settings.delivery.QUEUE") }}</option>
          <option value="STEER" disabled>
            {{ t("settings.delivery.STEER") }}
          </option>
        </select>
      </label>
    </section>

    <section class="flex flex-col gap-2">
      <p class="text-xs text-ink-muted">{{ t("settings.autostart.title") }}</p>
      <label class="flex items-center gap-2">
        <input
          type="checkbox"
          :checked="autostart"
          @change="toggleAutostart(($event.target as HTMLInputElement).checked)"
        />
        <span>{{ t("settings.autostart.label") }}</span>
      </label>
    </section>

    <AlertDialogRoot>
      <AlertDialogTrigger
        class="self-start rounded-control border border-line px-2 py-1"
      >
        {{ t("settings.quit") }}
      </AlertDialogTrigger>
      <AlertDialogPortal>
        <AlertDialogOverlay class="fixed inset-0" />
        <AlertDialogContent
          class="fixed top-1/2 left-1/2 flex -translate-x-1/2 -translate-y-1/2 flex-col gap-3 rounded-panel border border-line bg-panel p-4"
        >
          <AlertDialogTitle>{{ t("dialog.quitTitle") }}</AlertDialogTitle>
          <div class="flex gap-2">
            <AlertDialogAction
              class="rounded-control border border-line px-2 py-1"
              @click="quit()"
            >
              {{ t("dialog.quit") }}
            </AlertDialogAction>
            <AlertDialogCancel
              class="rounded-control border border-line px-2 py-1"
            >
              {{ t("dialog.cancel") }}
            </AlertDialogCancel>
          </div>
        </AlertDialogContent>
      </AlertDialogPortal>
    </AlertDialogRoot>

    <p v-if="failure" class="terminal text-xs">{{ failure }}</p>
  </div>
</template>
