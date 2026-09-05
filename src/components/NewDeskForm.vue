<script setup lang="ts">
import { onMounted, ref } from "vue";
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
import { create, runtimes } from "../client";
import type { Envelope, Runtime } from "../client";
import { button, buttonPrimary } from "../deskState";

const emit = defineEmits<{ close: [] }>();
const { t } = useI18n();

const name = ref("");
const runtime = ref("");
const available = ref<Runtime[]>([]);
const failure = ref("");
const busy = ref(false);

onMounted(async () => {
  const answer = await runtimes();
  available.value = (
    (answer.data as { runtimes?: Runtime[] } | undefined)?.runtimes ?? []
  ).filter((row) => row.state === "AVAILABLE");
  runtime.value = available.value[0]?.runtime ?? "";
});

async function submit(): Promise<void> {
  busy.value = true;
  const answer = await create({
    body: runtime.value
      ? { name: name.value, runtime: runtime.value }
      : { name: name.value },
  });
  busy.value = false;
  const envelope = answer.error as Envelope | undefined;
  if (envelope) {
    failure.value = envelope.message;
    return;
  }
  emit("close");
}
</script>

<template>
  <form class="flex flex-col gap-1" @submit.prevent="submit()">
    <input
      v-model="name"
      class="terminal rounded-control border border-line px-1 py-1"
      :placeholder="t('desks.namePlaceholder')"
      :aria-label="t('desks.name')"
    />
    <SelectRoot v-model="runtime">
      <SelectTrigger :class="button" :aria-label="t('desks.runtime')">
        <SelectValue class="terminal" />
      </SelectTrigger>
      <SelectPortal>
        <SelectContent class="rounded-control border border-line bg-panel">
          <SelectViewport>
            <SelectItem
              v-for="row in available"
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
    <div class="flex gap-1">
      <button :class="buttonPrimary" :disabled="busy" type="submit">
        {{ t("desks.create") }}
      </button>
      <button :class="button" type="button" @click="emit('close')">
        {{ t("common.cancel") }}
      </button>
    </div>
    <pre v-if="failure" class="terminal text-xs wrap-anywhere">{{
      failure
    }}</pre>
  </form>
</template>
