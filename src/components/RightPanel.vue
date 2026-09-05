<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from "reka-ui";
import { runtimes } from "../client";
import type { Runtime } from "../client";
import { button } from "../deskState";
import ActivityTab from "./ActivityTab.vue";
import DeskTab from "./DeskTab.vue";
import TriggersTab from "./TriggersTab.vue";

defineProps<{ deskId: string | null }>();
const tab = defineModel<string>("tab", { default: "desk" });
const { t } = useI18n();

const collapsed = ref(false);
const TABS = ["desk", "triggers", "approvals", "activity", "settings"] as const;
const label: Record<string, string> = {
  desk: "panel.desk",
  triggers: "panel.triggers",
  approvals: "panel.approvals",
  activity: "panel.activity",
  settings: "panel.settings",
};
const short: Record<string, string> = {
  desk: "panel.deskShort",
  triggers: "panel.triggersShort",
  approvals: "panel.approvalsShort",
  activity: "panel.activityShort",
  settings: "panel.settingsShort",
};

// First-launch onboarding is the Settings tab and nothing else (§6.3).
onMounted(async () => {
  const answer = await runtimes();
  const rows =
    (answer.data as { runtimes?: Runtime[] } | undefined)?.runtimes ?? [];
  if (!rows.some((row) => row.state === "AVAILABLE")) tab.value = "settings";
});
</script>

<template>
  <TabsRoot
    v-model="tab"
    class="flex shrink-0 flex-col border-l border-line"
    :class="collapsed ? 'w-10' : 'w-90'"
  >
    <div class="flex items-center gap-1 border-b border-line px-1 py-1">
      <TabsList
        class="flex flex-1 gap-1"
        :class="collapsed ? 'flex-col' : ''"
        :aria-label="t('panel.tabs')"
      >
        <TabsTrigger
          v-for="name in TABS"
          :key="name"
          class="px-1 py-1 data-[state=active]:text-accent"
          :value="name"
        >
          {{ collapsed ? t(short[name]) : t(label[name]) }}
        </TabsTrigger>
      </TabsList>
      <button
        :class="button"
        :aria-label="t('panel.collapse')"
        @click="collapsed = !collapsed"
      >
        {{ collapsed ? t("panel.expand") : t("panel.collapse") }}
      </button>
    </div>
    <template v-if="!collapsed">
      <TabsContent
        v-for="name in TABS"
        :key="name"
        class="min-w-0 flex-1 overflow-x-hidden overflow-y-auto p-2 transition-opacity duration-[120ms] data-[state=inactive]:opacity-0 motion-reduce:transition-none"
        :value="name"
      >
        <DeskTab v-if="name === 'desk' && deskId" :desk-id="deskId" />
        <TriggersTab
          v-else-if="name === 'triggers' && deskId"
          :desk-id="deskId"
        />
        <ActivityTab
          v-else-if="name === 'activity' && deskId"
          :desk-id="deskId"
        />
        <slot v-else-if="name === 'approvals'" name="approvals" />
        <slot v-else-if="name === 'settings'" name="settings" />
      </TabsContent>
    </template>
  </TabsRoot>
</template>
