<script setup lang="ts">
import { onMounted, onUnmounted, reactive, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
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
  hithinkDelete,
  hithinkPatch,
  hithinkProvider,
  hithinkPut,
  memoryProvider,
  memoryProviderRow,
  openviking,
  openvikingCandidates,
  openvikingRetry,
  openvikingSetup,
  policies,
  putLocale,
  putPolicies,
  runtimeDiscover,
  runtimeRetry,
  runtimes as listRuntimes,
} from "../client";
import type {
  Envelope,
  HithinkProvider,
  OpenVikingStatus,
  Provider,
  Resource,
  Runtime,
} from "../client";
import { useDaemon } from "../composables/useDaemon";
import { useEvents } from "../composables/useEvents";
import {
  button,
  buttonPrimary,
  chip,
  dialogContent,
  dialogOverlay,
  eyebrow,
  failureText,
  input,
  selectContent,
  selectItem,
  selectTrigger,
} from "../deskState";
import { applyLocale, LOCALES, type Locale } from "../locale";

const { t, locale } = useI18n();
const { on } = useEvents();
const { quit } = useDaemon();

const rows = ref<Runtime[]>([]);
const explicit = reactive(new Map<string, string>());
const memory = ref<Provider | null>(null);
const hithink = ref<HithinkProvider | null>(null);
// Write-only like the memory key: the row answers `api_key_present`, never a
// key (feature SPEC §1.2).
const hithinkKey = ref("");
const ov = ref<OpenVikingStatus | null>(null);
const paths = reactive({ python: "", node: "" });
// What the daemon's fixed-list search found, kept so a refetch of an
// unconfigured row keeps showing it (§8). Searched at most once per mount.
const found = reactive({ python: "", node: "" });
let searched = false;
const setupFailure = ref("");
let provisioning: ReturnType<typeof setInterval> | null = null;
const policy = ref<Resource | null>(null);
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
  const answer = await memoryProviderRow();
  if (refused(answer.error)) return;
  memory.value = answer.data ?? null;
  form.base_url = answer.data?.base_url ?? "";
  form.llm = answer.data?.llm_model ?? "";
  form.embedding = answer.data?.embedding_model ?? "";
}

async function loadHithink(): Promise<void> {
  const answer = await hithinkProvider();
  if (refused(answer.error)) return;
  hithink.value = answer.data ?? null;
}

async function saveHithink(): Promise<void> {
  const answer = await hithinkPut({ body: { api_key: hithinkKey.value } });
  if (!refused(answer.error)) hithinkKey.value = "";
  await loadHithink();
}

async function removeHithink(): Promise<void> {
  refused((await hithinkDelete()).error);
  await loadHithink();
}

/** The tick box is the whole choice of which client serves `CN` (§1.4). */
async function setAShareFeed(hithinkFeed: boolean): Promise<void> {
  const body = { a_share_feed: hithinkFeed ? "HITHINK" : "YAHOO" };
  refused((await hithinkPatch({ body })).error);
  await loadHithink();
}

/** The row and the child's live state; `PROVISIONING` polls itself (§8). */
async function loadOpenViking(): Promise<void> {
  const answer = await openviking();
  if (refused(answer.error)) return;
  ov.value = answer.data ?? null;
  const setup = answer.data?.setup;
  // A row naming neither path is prefilled from discovery, so the operator
  // confirms two paths instead of typing them (§8).
  if (!searched && !setup?.python_path && !setup?.node_path) {
    searched = true;
    const candidates = await openvikingCandidates();
    found.python = candidates.data?.python ?? "";
    found.node = candidates.data?.node ?? "";
  }
  paths.python = setup?.python_path || found.python;
  paths.node = setup?.node_path || found.node;
  if (ov.value?.setup.state === "PROVISIONING") {
    provisioning ??= setInterval(() => void loadOpenViking(), 2_000);
  } else if (provisioning) {
    clearInterval(provisioning);
    provisioning = null;
  }
}

async function provision(): Promise<void> {
  setupFailure.value = "";
  const answer = await openvikingSetup({
    body: { python: paths.python, node: paths.node },
  });
  // A setup refusal names the field the operator got wrong, so it shows there
  // instead of in the tab's one failure line (§8).
  if (answer.error) {
    setupFailure.value = (answer.error as Envelope).message;
    return;
  }
  await loadOpenViking();
}

async function retryChild(): Promise<void> {
  refused((await openvikingRetry()).error);
  await loadOpenViking();
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

/**
 * The daemon holds the choice and `applyLocale` shows it (feature SPEC §2.5).
 * A refusal snaps the native select back to the language still in force.
 */
async function setLanguage(event: Event): Promise<void> {
  const select = event.target as HTMLSelectElement;
  const next = select.value as Locale;
  if (refused((await putLocale({ body: { locale: next } })).error)) {
    select.value = locale.value;
    return;
  }
  await applyLocale(next);
}

async function toggleAutostart(on: boolean): Promise<void> {
  await (on ? enable() : disable());
  autostart.value = await isEnabled();
}

const off = [
  on(["RUNTIME_DISCOVERED", "RUNTIME_UNAVAILABLE"], () => void loadRuntimes()),
  on("OPENVIKING_CONFIGURED", () => void loadMemory()),
  on("HITHINK_PROVIDER_CHANGED", () => void loadHithink()),
  on(
    [
      "OPENVIKING_PROVISIONED",
      "OPENVIKING_STARTED",
      "OPENVIKING_LOST",
      "OPENVIKING_UNAVAILABLE",
    ],
    () => void loadOpenViking(),
  ),
  on("POLICY_CHANGED", () => void loadPolicies()),
];
onUnmounted(() => {
  off.forEach((stop) => stop());
  if (provisioning) clearInterval(provisioning);
});

onMounted(async () => {
  await Promise.all([
    loadRuntimes(),
    loadMemory(),
    loadHithink(),
    loadOpenViking(),
    loadPolicies(),
  ]);
  // First launch — no runtime discovered yet — turns autostart on once.
  // ponytail: "first launch" is read as "no AVAILABLE runtime", so an operator
  // who disables autostart before discovering one gets it re-enabled next
  // launch; the plugin's own state is the setting (slice 008 §2).
  if (
    !rows.value.some((row) => row.state === "AVAILABLE") &&
    !(await isEnabled())
  ) {
    await enable();
  }
  autostart.value = await isEnabled();
});
</script>

<template>
  <div class="flex flex-col gap-6">
    <section class="flex flex-col gap-2">
      <p :class="eyebrow">{{ t("settings.language.title") }}</p>
      <select
        :class="input"
        class="self-start font-sans"
        data-testid="language"
        :value="locale"
        :aria-label="t('settings.language.title')"
        @change="setLanguage($event)"
      >
        <option v-for="value in LOCALES" :key="value" :value="value">
          {{
            t(
              value === "en"
                ? "settings.language.en"
                : "settings.language.zhHans",
            )
          }}
        </option>
      </select>
    </section>

    <section class="flex flex-col gap-2">
      <p :class="eyebrow" data-testid="runtimes-title">
        {{ t("settings.runtimes.title") }}
      </p>
      <div
        v-for="row in rows"
        :key="row.runtime"
        class="flex flex-col gap-2 rounded-control border border-line bg-panel-raised p-2"
      >
        <div class="flex items-center gap-2">
          <span class="terminal font-semibold">{{ row.runtime }}</span>
          <span v-if="row.version" :class="chip">{{ row.version }}</span>
          <span class="flex-1" />
          <span
            class="terminal flex items-center gap-1.5 text-sm text-ink-muted"
          >
            <span
              class="h-1.5 w-1.5 rounded-pill"
              :class="
                row.state === 'AVAILABLE'
                  ? 'bg-state-live'
                  : row.state === 'UNAVAILABLE'
                    ? 'bg-state-failure'
                    : 'bg-state-idle'
              "
            />
            {{ row.state }}
          </span>
        </div>
        <p
          v-if="row.executable_path"
          class="terminal text-sm wrap-anywhere text-ink-muted select-text"
        >
          {{ row.executable_path }}
        </p>
        <form
          class="flex gap-2"
          @submit.prevent="discover(row.runtime, explicit.get(row.runtime))"
        >
          <input
            :class="input"
            class="flex-1"
            :data-testid="`runtime-path-${row.runtime}`"
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
            :class="button"
            :data-testid="`runtime-submit-${row.runtime}`"
          >
            {{ t("settings.runtimes.submit") }}
          </button>
        </form>
        <div class="flex gap-2">
          <button type="button" :class="button" @click="discover(row.runtime)">
            {{ t("settings.runtimes.discover") }}
          </button>
          <button
            v-if="row.state === 'UNAVAILABLE'"
            type="button"
            :class="button"
            @click="retry(row.runtime)"
          >
            {{ t("settings.runtimes.retry") }}
          </button>
        </div>
      </div>
    </section>

    <section v-if="ov" class="flex flex-col gap-2" data-testid="openviking">
      <p :class="eyebrow">{{ t("settings.openviking.title") }}</p>
      <div class="flex items-center gap-2">
        <span :class="chip">{{ ov.setup.state }}</span>
        <span :class="chip">{{ ov.child }}</span>
      </div>
      <p
        v-if="ov.setup.failure_message"
        :class="failureText"
        data-testid="openviking-failure"
      >
        {{ ov.setup.failure_code }} {{ ov.setup.failure_message }}
      </p>
      <form class="flex flex-col gap-2" @submit.prevent="provision()">
        <label class="flex flex-col gap-1">
          <span class="text-ink-muted">{{
            t("settings.openviking.python")
          }}</span>
          <input
            v-model="paths.python"
            :class="input"
            data-testid="openviking-python"
            :placeholder="t('settings.openviking.python')"
          />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-ink-muted">{{
            t("settings.openviking.node")
          }}</span>
          <input
            v-model="paths.node"
            :class="input"
            data-testid="openviking-node"
            :placeholder="t('settings.openviking.node')"
          />
        </label>
        <p
          v-if="setupFailure"
          :class="failureText"
          data-testid="openviking-error"
        >
          {{ setupFailure }}
        </p>
        <div class="flex gap-2">
          <button
            type="submit"
            :class="buttonPrimary"
            data-testid="openviking-setup"
            :disabled="ov.setup.state === 'PROVISIONING'"
          >
            {{ t("settings.openviking.setup") }}
          </button>
          <button
            v-if="ov.setup.state === 'UNAVAILABLE'"
            type="button"
            :class="button"
            data-testid="openviking-retry"
            @click="retryChild()"
          >
            {{ t("settings.openviking.retry") }}
          </button>
        </div>
      </form>
    </section>

    <section v-if="memory" class="flex flex-col gap-2">
      <p :class="eyebrow">{{ t("settings.memory.title") }}</p>
      <p class="text-ink-muted">
        {{
          t(
            memory.api_key_present
              ? "settings.memory.keySet"
              : "settings.memory.keyUnset",
          )
        }}
      </p>
      <form class="flex flex-col gap-2" @submit.prevent="saveProvider()">
        <label class="flex flex-col gap-1">
          <span class="text-ink-muted">{{ t("settings.memory.baseUrl") }}</span>
          <input
            v-model="form.base_url"
            :class="input"
            :aria-label="t('settings.memory.baseUrl')"
          />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-ink-muted">{{ t("settings.memory.apiKey") }}</span>
          <input
            v-model="form.api_key"
            type="password"
            :class="input"
            :aria-label="t('settings.memory.apiKey')"
          />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-ink-muted">{{
            t("settings.memory.llmModel")
          }}</span>
          <input
            v-model="form.llm"
            :class="input"
            :aria-label="t('settings.memory.llmModel')"
          />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-ink-muted">{{
            t("settings.memory.embeddingModel")
          }}</span>
          <input
            v-model="form.embedding"
            :class="input"
            :aria-label="t('settings.memory.embeddingModel')"
          />
        </label>
        <button type="submit" :class="button" class="self-start">
          {{ t("settings.memory.save") }}
        </button>
      </form>
    </section>

    <section v-if="hithink" class="flex flex-col gap-2" data-testid="hithink">
      <p :class="eyebrow">{{ t("settings.hithink.title") }}</p>
      <div class="flex items-center gap-2">
        <span :class="chip">{{ hithink.state }}</span>
        <span
          v-if="hithink.validated_at_ns"
          class="terminal text-sm text-ink-muted"
        >
          {{ new Date(hithink.validated_at_ns / 1_000_000).toLocaleString() }}
        </span>
      </div>
      <p
        v-if="hithink.failure_message"
        :class="failureText"
        data-testid="hithink-failure"
      >
        {{ hithink.failure_code }} {{ hithink.failure_message }}
      </p>
      <p class="text-ink-muted">
        {{
          t(
            hithink.api_key_present
              ? "settings.hithink.keySet"
              : "settings.hithink.keyUnset",
          )
        }}
      </p>
      <form class="flex gap-2" @submit.prevent="saveHithink()">
        <input
          v-model="hithinkKey"
          type="password"
          :class="input"
          class="flex-1"
          data-testid="hithink-key"
          :aria-label="t('settings.hithink.apiKey')"
          :placeholder="t('settings.hithink.apiKey')"
        />
        <button type="submit" :class="button" data-testid="hithink-save">
          {{ t("settings.hithink.save") }}
        </button>
        <button
          v-if="hithink.api_key_present"
          type="button"
          :class="button"
          data-testid="hithink-remove"
          @click="removeHithink()"
        >
          {{ t("settings.hithink.remove") }}
        </button>
      </form>
      <label class="flex items-center gap-2">
        <input
          type="checkbox"
          class="accent-accent"
          data-testid="hithink-feed"
          :checked="hithink.a_share_feed === 'HITHINK'"
          :disabled="!hithink.api_key_present"
          @change="setAShareFeed(($event.target as HTMLInputElement).checked)"
        />
        <span>{{ t("settings.hithink.aShareFeed") }}</span>
      </label>
    </section>

    <section v-if="policy" class="flex flex-col gap-2">
      <p :class="eyebrow">{{ t("settings.policies.title") }}</p>
      <div class="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
        <template
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
        >
          <span class="text-ink-muted">{{ t(row.label) }}</span>
          <SelectRoot
            :model-value="policy[row.field]"
            @update:model-value="setPolicy(row.field, $event as string)"
          >
            <SelectTrigger :class="selectTrigger" :aria-label="t(row.label)">
              <SelectValue />
              <span aria-hidden="true" class="text-ink-muted">▾</span>
            </SelectTrigger>
            <SelectContent :class="selectContent" position="popper">
              <SelectViewport>
                <SelectItem
                  v-for="value in ['ALWAYS_ALLOW', 'REQUIRE_APPROVAL']"
                  :key="value"
                  :value="value"
                  :class="selectItem"
                  class="font-sans"
                >
                  <SelectItemText>{{
                    t(`settings.policy.${value}`)
                  }}</SelectItemText>
                </SelectItem>
              </SelectViewport>
            </SelectContent>
          </SelectRoot>
        </template>
        <!--
          Delivery admits only QUEUE, so it is a plain disabled select: it never
          opens, and Steer stays visibly refused (feature SPEC §6.3, per D70).
        -->
        <span class="text-ink-muted">{{
          t("settings.policies.delivery")
        }}</span>
        <select
          disabled
          :class="input"
          class="font-sans opacity-50"
          :value="policy.delivery_mode"
          :aria-label="t('settings.policies.delivery')"
        >
          <option value="QUEUE">{{ t("settings.delivery.QUEUE") }}</option>
          <option value="STEER" disabled>
            {{ t("settings.delivery.STEER") }}
          </option>
        </select>
      </div>
    </section>

    <section class="flex flex-col gap-2">
      <p :class="eyebrow">{{ t("settings.autostart.title") }}</p>
      <label class="flex items-center gap-2">
        <input
          type="checkbox"
          class="accent-accent"
          :checked="autostart"
          @change="toggleAutostart(($event.target as HTMLInputElement).checked)"
        />
        <span>{{ t("settings.autostart.label") }}</span>
      </label>
    </section>

    <AlertDialogRoot>
      <AlertDialogTrigger :class="button" class="self-start" data-testid="quit">
        {{ t("settings.quit") }}
      </AlertDialogTrigger>
      <AlertDialogPortal>
        <AlertDialogOverlay :class="dialogOverlay" />
        <AlertDialogContent :class="dialogContent">
          <div class="flex flex-col gap-1">
            <AlertDialogTitle class="text-lg font-semibold">{{
              t("dialog.quitTitle")
            }}</AlertDialogTitle>
            <AlertDialogDescription class="text-ink-muted">
              {{ t("dialog.quitBody") }}
            </AlertDialogDescription>
          </div>
          <div class="flex justify-end gap-2">
            <AlertDialogCancel :class="button">
              {{ t("dialog.cancel") }}
            </AlertDialogCancel>
            <AlertDialogAction
              :class="buttonPrimary"
              data-testid="dialog-quit"
              @click="quit()"
            >
              {{ t("dialog.quit") }}
            </AlertDialogAction>
          </div>
        </AlertDialogContent>
      </AlertDialogPortal>
    </AlertDialogRoot>

    <p v-if="failure" :class="failureText">{{ failure }}</p>
  </div>
</template>
