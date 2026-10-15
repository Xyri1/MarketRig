<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from "reka-ui";
import { runtimes } from "../client";
import type { Runtime } from "../client";
import { useApprovals } from "../composables/useApprovals";
import { buttonQuiet } from "../deskState";
import ActivityTab from "./ActivityTab.vue";
import DeskTab from "./DeskTab.vue";
import TriggersTab from "./TriggersTab.vue";

defineProps<{ deskId: string | null }>();
const tab = defineModel<string>("tab", { default: "desk" });
const { t } = useI18n();
const { pending } = useApprovals();

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

// After the onboarding gate, first launch lands on the Settings tab (§6.3).
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
    class="flex shrink-0 flex-col border-l border-line bg-panel"
    :class="collapsed ? 'w-10' : 'w-90'"
  >
    <div
      class="flex shrink-0 border-b border-line"
      :class="collapsed ? 'flex-col items-center py-1' : 'h-10 items-stretch'"
    >
      <TabsList
        class="flex min-w-0 flex-1"
        :class="collapsed ? 'flex-col items-center gap-1' : 'px-1'"
        :aria-label="t('panel.tabs')"
      >
        <TabsTrigger
          v-for="name in TABS"
          :key="name"
          class="relative flex items-center gap-1 text-ink-muted hover:text-ink focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent data-[state=active]:text-ink"
          :class="
            collapsed
              ? 'terminal h-7 w-7 justify-center rounded-control data-[state=active]:bg-ground'
              : 'px-2 after:absolute after:inset-x-2 after:bottom-0 after:h-[2px] after:rounded-pill data-[state=active]:after:bg-accent'
          "
          :data-testid="`tab-${name}`"
          :title="t(label[name])"
          :value="name"
        >
          {{ collapsed ? t(short[name]) : t(label[name]) }}
          <span
            v-if="name === 'approvals' && pending.length"
            class="terminal inline-flex h-4 min-w-4 items-center justify-center rounded-pill bg-state-pending px-1 text-xs text-well"
            :class="collapsed ? 'absolute -top-1 -right-1' : ''"
            >{{ pending.length }}</span
          >
        </TabsTrigger>
      </TabsList>
      <button
        :class="buttonQuiet"
        class="my-auto mr-1 w-7 px-0"
        :aria-label="collapsed ? t('panel.expand') : t('panel.collapse')"
        :title="collapsed ? t('panel.expand') : t('panel.collapse')"
        @click="collapsed = !collapsed"
      >
        <span aria-hidden="true">{{ collapsed ? "‹" : "›" }}</span>
      </button>
    </div>
    <template v-if="!collapsed">
      <TabsContent
        v-for="name in TABS"
        :key="name"
        class="min-w-0 flex-1 overflow-x-hidden overflow-y-auto p-3 transition-opacity duration-[120ms] data-[state=inactive]:opacity-0 motion-reduce:transition-none"
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
        <p v-else class="text-ink-muted">{{ t("panel.noDesk") }}</p>
      </TabsContent>
    </template>
  </TabsRoot>
</template>
