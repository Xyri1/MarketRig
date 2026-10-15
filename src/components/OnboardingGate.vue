<script setup lang="ts">
import { ref } from "vue";
import { useI18n } from "vue-i18n";
import { disable, enable } from "@tauri-apps/plugin-autostart";
import { buttonPrimary, eyebrow, failureText, input } from "../deskState";
import { chooseLocale, LOCALES, type Locale } from "../locale";

const emit = defineEmits<{ done: [] }>();
const { t, locale } = useI18n();
// Autostart is offered on by default (SPEC §6.3, per D52); the plugin's own
// state is the setting, written once on Continue.
const autostart = ref(true);
const failure = ref("");

async function setLanguage(event: Event): Promise<void> {
  const select = event.target as HTMLSelectElement;
  failure.value = (await chooseLocale(select.value as Locale)) ?? "";
  if (failure.value) select.value = locale.value;
}

async function finish(): Promise<void> {
  try {
    await (autostart.value ? enable() : disable());
  } catch (problem) {
    // Outside the shell — a browser during `pnpm dev` — there is no plugin.
    console.warn("autostart", problem);
  }
  emit("done");
}
</script>

<template>
  <main class="flex h-full items-center justify-center p-8">
    <div class="flex w-96 flex-col gap-4">
      <p class="terminal text-lg font-semibold">{{ t("app.name") }}</p>
      <p class="text-ink-muted">{{ t("onboarding.intro") }}</p>
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
      <label class="flex items-center gap-2">
        <input
          v-model="autostart"
          type="checkbox"
          class="accent-accent"
          data-testid="onboarding-autostart"
        />
        <span>{{ t("settings.autostart.label") }}</span>
      </label>
      <p class="text-ink-muted">{{ t("onboarding.next") }}</p>
      <p v-if="failure" :class="failureText">{{ failure }}</p>
      <button
        type="button"
        :class="buttonPrimary"
        class="self-start"
        data-testid="onboarding-continue"
        @click="finish()"
      >
        {{ t("onboarding.continue") }}
      </button>
    </div>
  </main>
</template>
