<script setup lang="ts">
import { ref } from "vue";
import { useI18n } from "vue-i18n";
import type { Desk } from "../client";
import { button } from "../deskState";
import DeskRow from "./DeskRow.vue";
import NewDeskForm from "./NewDeskForm.vue";

defineProps<{ desks: Desk[]; selected: string | null }>();
defineEmits<{ select: [id: string] }>();

const { t } = useI18n();
const creating = ref(false);
</script>

<template>
  <nav class="flex flex-col border-r border-line bg-panel">
    <div class="flex h-10 shrink-0 items-center gap-2 px-3">
      <span class="terminal flex items-center gap-1 font-semibold">
        {{ t("app.name") }}
        <span aria-hidden="true" class="h-3.5 w-1.5 bg-accent" />
      </span>
      <span v-if="desks.length" class="terminal text-sm text-ink-muted">{{
        desks.length
      }}</span>
    </div>
    <div class="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <p v-if="!desks.length" class="px-3 py-2 text-ink-muted">
        {{ t("desks.empty") }}
      </p>
      <DeskRow
        v-for="desk in desks"
        :key="desk.id"
        :desk="desk"
        :selected="desk.id === selected"
        @select="$emit('select', desk.id)"
      />
    </div>
    <div class="shrink-0 border-t border-line p-2">
      <NewDeskForm v-if="creating" @close="creating = false" />
      <button
        v-else
        :class="button"
        class="w-full"
        data-testid="new-desk"
        @click="creating = true"
      >
        <span aria-hidden="true">+</span>
        {{ t("desks.new") }}
      </button>
    </div>
  </nav>
</template>
