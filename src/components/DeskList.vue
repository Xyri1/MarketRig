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
  <nav class="flex flex-col gap-1 border-r border-line py-2">
    <p v-if="!desks.length" class="px-2 text-ink-muted">
      {{ t("desks.empty") }}
    </p>
    <DeskRow
      v-for="desk in desks"
      :key="desk.id"
      :desk="desk"
      :selected="desk.id === selected"
      @select="$emit('select', desk.id)"
    />
    <div class="mt-auto px-2">
      <NewDeskForm v-if="creating" @close="creating = false" />
      <button v-else :class="button" @click="creating = true">
        {{ t("desks.new") }}
      </button>
    </div>
  </nav>
</template>
