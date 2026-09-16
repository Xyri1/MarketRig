<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogRoot,
  AlertDialogTitle,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuPortal,
  ContextMenuRoot,
  ContextMenuTrigger,
} from "reka-ui";
import { invoke } from "@tauri-apps/api/core";
import { deleteDesk, retry } from "../client";
import type { Desk, Envelope } from "../client";
import { useApprovals } from "../composables/useApprovals";
import { useEvents } from "../composables/useEvents";
import { useTerminal } from "../composables/useTerminal";
import {
  button,
  buttonPrimary,
  deskState,
  dialogContent,
  dialogOverlay,
  failureText,
  gutterClass,
  isWindows,
  selectContent,
  selectItem,
} from "../deskState";

const props = defineProps<{ desk: Desk; selected: boolean }>();
const emit = defineEmits<{ select: []; deleted: [] }>();
const { t } = useI18n();
const { byDesk } = useApprovals();
const { attention } = useEvents();
const { panes } = useTerminal();

const pending = computed(() => byDesk.get(props.desk.id) ?? 0);
const state = computed(() =>
  deskState(
    props.desk,
    panes.has(props.desk.id),
    pending.value,
    attention.has(props.desk.id),
  ),
);
const revealLabel = computed(() =>
  isWindows() ? t("desks.reveal.windows") : t("desks.reveal.mac"),
);

const confirming = ref(false);
const failure = ref("");

/** Outside Tauri — Vitest, or a browser during `pnpm dev` — this rejects. */
async function reveal(): Promise<void> {
  try {
    await invoke("reveal_path", { path: props.desk.workspace_path });
  } catch (error) {
    console.warn("reveal_path", error);
  }
}

/**
 * A refusal keeps the dialog open with the daemon's own sentence; the list
 * refreshes on the parent's `deleted`, not on an event this row waits for.
 */
async function remove(): Promise<void> {
  failure.value = "";
  const answer = await deleteDesk({ path: { desk_id: props.desk.id } });
  const envelope = answer.error as Envelope | undefined;
  if (envelope) {
    failure.value = envelope.message;
    return;
  }
  confirming.value = false;
  emit("deleted");
}
</script>

<template>
  <ContextMenuRoot>
    <ContextMenuTrigger as-child>
      <div
        class="flex h-9 items-center gap-2 pr-2"
        :class="selected ? 'bg-ground' : 'hover:bg-ground/60'"
        :data-state="state"
        :title="desk.selected_runtime"
      >
        <span class="h-full w-[2px] shrink-0" :class="gutterClass[state]" />
        <button
          class="terminal h-full min-w-0 flex-1 truncate rounded-control pl-1 text-left focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
          :class="selected ? 'text-accent' : ''"
          @click="emit('select')"
        >
          {{ desk.name }}
        </button>
        <span
          v-if="pending"
          class="terminal inline-flex h-5 min-w-5 items-center justify-center rounded-pill bg-state-pending px-1.5 text-xs text-well"
          :data-testid="`desk-pending-${desk.id}`"
          >{{ pending }}</span
        >
        <span
          v-if="attention.has(desk.id)"
          class="h-2 w-2 shrink-0 rounded-pill bg-state-attention"
          :aria-label="t('desks.attention')"
          :title="t('desks.attention')"
        />
        <button
          v-if="desk.state === 'FAILED'"
          :class="button"
          class="h-6 px-2"
          @click="retry({ path: { desk_id: desk.id } })"
        >
          {{ t("desks.retry") }}
        </button>
      </div>
    </ContextMenuTrigger>
    <ContextMenuPortal>
      <ContextMenuContent
        :class="selectContent"
        :data-testid="`menu-${desk.id}`"
      >
        <ContextMenuItem :class="selectItem" @select="reveal()">
          {{ revealLabel }}
        </ContextMenuItem>
        <ContextMenuItem
          :class="selectItem"
          class="text-state-failure"
          data-testid="desk-delete"
          @select="confirming = true"
        >
          {{ t("desks.delete") }}
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenuPortal>
  </ContextMenuRoot>

  <AlertDialogRoot v-model:open="confirming">
    <AlertDialogPortal>
      <AlertDialogOverlay :class="dialogOverlay" />
      <AlertDialogContent :class="dialogContent">
        <div class="flex flex-col gap-1">
          <AlertDialogTitle class="text-lg font-semibold">
            {{ t("desks.deleteTitle", { name: desk.name }) }}
          </AlertDialogTitle>
          <AlertDialogDescription class="text-ink-muted">
            {{ t("desks.deleteBody") }}
          </AlertDialogDescription>
        </div>
        <p v-if="failure" :class="failureText">{{ failure }}</p>
        <div class="flex justify-end gap-2">
          <AlertDialogCancel :class="button">
            {{ t("common.cancel") }}
          </AlertDialogCancel>
          <!--
            A plain button, not AlertDialogAction: the Action closes the dialog
            on click, and a refusal has to stay on screen with its message.
          -->
          <button
            type="button"
            :class="buttonPrimary"
            data-testid="desk-delete-confirm"
            @click="remove()"
          >
            {{ t("desks.deleteConfirm") }}
          </button>
        </div>
      </AlertDialogContent>
    </AlertDialogPortal>
  </AlertDialogRoot>
</template>
