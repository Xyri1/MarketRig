<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import {
  book,
  historyCycles,
  historyFills,
  openOrders,
  positions,
  quotes,
} from "../client";
import { useEvents } from "../composables/useEvents";
import { chip, eyebrow, failureText } from "../deskState";

type Row = Record<string, unknown>;

const props = defineProps<{ deskId: string }>();
const { t } = useI18n();
const { on } = useEvents();

const rows = ref<Record<string, Row[]>>({});
const failure = ref("");

async function refresh(): Promise<void> {
  const path = { desk_id: props.deskId };
  const answers = await Promise.all([
    quotes({ path }),
    book({ path }),
    positions({ path }),
    openOrders({ path }),
    historyCycles({ path }),
    historyFills({ path }),
  ]);
  const first = answers.find((answer) => answer.error);
  failure.value = first ? (first.error as { message: string }).message : "";
  const read = (at: number, key: string) =>
    (answers[at].data as Record<string, Row[]> | undefined)?.[key] ?? [];
  rows.value = {
    quotes: read(0, "quotes"),
    book: read(1, "book"),
    positions: read(2, "positions"),
    orders: read(3, "orders"),
    cycles: read(4, "cycles"),
    fills: read(5, "fills"),
  };
}

/** Verbatim: money and quantities are decimal text and are never reformatted. */
function text(value: unknown): string {
  if (value === null || value === undefined) return "";
  return typeof value === "string" ? value : JSON.stringify(value);
}

function at(ns: unknown): string {
  if (typeof ns !== "number") return "";
  return new Date(ns / 1e6).toLocaleString(undefined, {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Exact decimal-text addition on scaled integers, so a display total never
 * carries a float artefact. The addends are the node's own realized figures
 * (per D38); the total is a reading of them, not a new trading fact.
 */
function sum(values: string[]): string {
  const scale = Math.max(0, ...values.map((v) => v.split(".")[1]?.length ?? 0));
  let total = 0n;
  for (const value of values) {
    const negative = value.startsWith("-");
    const [whole, fraction = ""] = value.replace("-", "").split(".");
    const scaled = BigInt(whole + fraction.padEnd(scale, "0"));
    total += negative ? -scaled : scaled;
  }
  const sign = total < 0n ? "-" : "";
  const digits = (total < 0n ? -total : total)
    .toString()
    .padStart(scale + 1, "0");
  return scale
    ? `${sign}${digits.slice(0, -scale)}.${digits.slice(-scale)}`
    : `${sign}${digits}`;
}

function signClass(value: string): string {
  if (value.startsWith("-")) return "text-state-failure";
  return Number(value) > 0 ? "text-state-live" : "";
}

/** Closed cycles grouped by currency, oldest first, with their exact total. */
const realized = computed(() => {
  const groups = new Map<string, Row[]>();
  for (const cycle of rows.value.cycles ?? []) {
    const currency = text(cycle.currency);
    groups.set(currency, [...(groups.get(currency) ?? []), cycle]);
  }
  return [...groups].map(([currency, cycles]) => {
    const ordered = [...cycles].reverse();
    return {
      currency,
      cycles: ordered,
      total: sum(ordered.map((cycle) => text(cycle.realized_pnl))),
    };
  });
});

/**
 * Bars for one currency: each cycle's realized P&L against a zero baseline,
 * with the running total over them as a step line, both scaled to one axis.
 */
const CHART = { width: 320, height: 72, gap: 2 };
function chart(cycles: Row[]) {
  const values = cycles.map((cycle) => Number(text(cycle.realized_pnl)));
  const running: number[] = [];
  values.reduce((total, value) => {
    running.push(total + value);
    return total + value;
  }, 0);
  const extent =
    Math.max(...values.map(Math.abs), ...running.map(Math.abs), 0) || 1;
  const half = CHART.height / 2;
  const y = (value: number) => half - (value / extent) * (half - 2);
  const width = (CHART.width - CHART.gap * (cycles.length - 1)) / cycles.length;
  const bars = cycles.map((cycle, index) => {
    const value = values[index];
    return {
      key: text(cycle.id),
      x: index * (width + CHART.gap),
      y: Math.min(y(value), half),
      width,
      height: Math.max(1, Math.abs(y(value) - half)),
      fill: value < 0 ? "fill-state-failure" : "fill-state-live",
      title: `${text(cycle.instrument_id)} ${text(cycle.realized_pnl)} ${at(cycle.closed_at_ns)}`,
    };
  });
  const line = running
    .map(
      (total, index) =>
        `${index * (width + CHART.gap) + width / 2},${y(total)}`,
    )
    .join(" ");
  return { bars, line };
}

/** Each quote with its book top beside it, keyed by instrument. */
const watchlist = computed(() => {
  const tops = new Map<string, Row>();
  for (const top of rows.value.book ?? []) {
    const observation = (top.observation as Row | undefined) ?? top;
    tops.set(text(observation.instrument_id), top);
  }
  return (rows.value.quotes ?? []).map((quote) => {
    const id = text(quote.instrument_id);
    const top = tops.get(id) ?? {};
    return {
      id,
      last: text(quote.last),
      bid: text(top.bid_price),
      ask: text(top.ask_price),
      phase: text(quote.market_phase),
      live: quote.health === "AVAILABLE",
    };
  });
});

const recentFills = computed(() => (rows.value.fills ?? []).slice(0, 8));

// No operational event kind reports a fill or a position, so the tab polls
// while it is visible and refetches on a decision (feature SPEC §6.3).
let timer: ReturnType<typeof setInterval> | null = null;
const off = on(["APPROVAL_DECIDED"], () => void refresh());

watch(() => props.deskId, refresh);
onMounted(() => {
  void refresh();
  timer = setInterval(() => void refresh(), 15_000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
  off();
});
</script>

<template>
  <div class="flex min-w-0 flex-col gap-5">
    <section class="flex flex-col gap-2" data-testid="desk-cycles">
      <p :class="eyebrow">{{ t("desk.realized") }}</p>
      <p v-if="!realized.length" class="text-ink-muted">
        {{ t("desk.empty.cycles") }}
      </p>
      <div
        v-for="group in realized"
        :key="group.currency"
        class="flex flex-col gap-2 rounded-control border border-line bg-panel-raised p-3"
      >
        <div class="flex items-baseline gap-2">
          <span
            class="terminal text-xl font-semibold"
            :class="signClass(group.total)"
            >{{ group.total }}</span
          >
          <span class="terminal text-ink-muted">{{ group.currency }}</span>
          <span class="flex-1" />
          <span class="text-sm text-ink-muted">
            {{ t("desk.cycleCount", { count: group.cycles.length }) }}
          </span>
        </div>
        <svg
          class="w-full"
          :viewBox="`0 0 ${CHART.width} ${CHART.height}`"
          :height="CHART.height"
          preserveAspectRatio="none"
          role="img"
          :aria-label="t('desk.realized')"
        >
          <line
            :x1="0"
            :x2="CHART.width"
            :y1="CHART.height / 2"
            :y2="CHART.height / 2"
            class="stroke-line"
          />
          <rect
            v-for="bar in chart(group.cycles).bars"
            :key="bar.key"
            :x="bar.x"
            :y="bar.y"
            :width="bar.width"
            :height="bar.height"
            :class="bar.fill"
            rx="1"
          >
            <title>{{ bar.title }}</title>
          </rect>
          <polyline
            :points="chart(group.cycles).line"
            fill="none"
            class="stroke-ink-muted"
            stroke-width="1.5"
            stroke-linejoin="round"
            vector-effect="non-scaling-stroke"
          />
        </svg>
      </div>
    </section>

    <section class="flex flex-col gap-2" data-testid="desk-positions">
      <div class="flex items-baseline gap-2">
        <p :class="eyebrow">{{ t("desk.positions") }}</p>
        <span
          v-if="(rows.positions ?? []).length"
          class="terminal text-sm text-ink-muted"
          >{{ (rows.positions ?? []).length }}</span
        >
      </div>
      <p v-if="!(rows.positions ?? []).length" class="text-ink-muted">
        {{ t("desk.empty.positions") }}
      </p>
      <table v-else class="terminal w-full text-sm select-text">
        <thead class="text-left font-sans text-ink-muted">
          <tr>
            <th class="pr-2 font-normal">{{ t("desk.col.instrument") }}</th>
            <th class="pr-2 font-normal">{{ t("desk.col.side") }}</th>
            <th class="pr-2 text-right font-normal">
              {{ t("desk.col.quantity") }}
            </th>
            <th class="text-right font-normal">{{ t("desk.col.avgOpen") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in rows.positions"
            :key="text(row.position_id)"
            class="border-t border-line"
          >
            <td class="py-1 pr-2 wrap-anywhere">
              {{ text(row.instrument_id) }}
            </td>
            <td class="py-1 pr-2">{{ text(row.side) }}</td>
            <td class="py-1 pr-2 text-right">
              {{ text(row.quantity) }}
              <span
                v-if="row.sellable_quantity !== undefined"
                class="block text-xs text-ink-muted"
                >{{ text(row.sellable_quantity) }}
                {{ t("desk.sellable") }}</span
              >
            </td>
            <td class="py-1 text-right">
              {{ text(row.average_open_price) }}
              <span class="text-ink-muted">{{ text(row.currency) }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </section>

    <section class="flex flex-col gap-2" data-testid="desk-orders">
      <div class="flex items-baseline gap-2">
        <p :class="eyebrow">{{ t("desk.orders") }}</p>
        <span
          v-if="(rows.orders ?? []).length"
          class="terminal text-sm text-ink-muted"
          >{{ (rows.orders ?? []).length }}</span
        >
      </div>
      <p v-if="!(rows.orders ?? []).length" class="text-ink-muted">
        {{ t("desk.empty.orders") }}
      </p>
      <table v-else class="terminal w-full text-sm select-text">
        <thead class="text-left font-sans text-ink-muted">
          <tr>
            <th class="pr-2 font-normal">{{ t("desk.col.instrument") }}</th>
            <th class="pr-2 font-normal">{{ t("desk.col.side") }}</th>
            <th class="pr-2 text-right font-normal">
              {{ t("desk.col.quantity") }}
            </th>
            <th class="pr-2 text-right font-normal">
              {{ t("desk.col.price") }}
            </th>
            <th class="font-normal">{{ t("desk.col.status") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in rows.orders"
            :key="text(row.client_order_id)"
            class="border-t border-line"
          >
            <td class="py-1 pr-2 wrap-anywhere">
              {{ text(row.instrument_id) }}
            </td>
            <td class="py-1 pr-2">
              {{ text(row.side) }}
              <span class="text-ink-muted">{{ text(row.type) }}</span>
            </td>
            <td class="py-1 pr-2 text-right">
              <span
                v-if="text(row.filled_quantity) !== '0'"
                class="text-ink-muted"
                >{{ text(row.filled_quantity) }} /</span
              >
              {{ text(row.quantity) }}
            </td>
            <td class="py-1 pr-2 text-right">{{ text(row.price) }}</td>
            <td class="py-1">
              <span :class="chip">{{ text(row.status) }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </section>

    <section class="flex flex-col gap-2" data-testid="desk-fills">
      <p :class="eyebrow">{{ t("desk.fills") }}</p>
      <p v-if="!recentFills.length" class="text-ink-muted">
        {{ t("desk.empty.fills") }}
      </p>
      <ol v-else class="terminal flex flex-col text-sm select-text">
        <li
          v-for="row in recentFills"
          :key="text(row.id)"
          class="flex gap-2 border-t border-line py-1 first:border-t-0"
        >
          <span class="text-ink-muted">{{ at(row.occurred_at_ns) }}</span>
          <span>{{ text(row.side) }} {{ text(row.quantity) }}</span>
          <span class="min-w-0 flex-1 truncate">{{
            text(row.instrument_id)
          }}</span>
          <span>{{ text(row.price) }}</span>
        </li>
      </ol>
    </section>

    <section class="flex flex-col gap-2" data-testid="desk-quotes">
      <div class="flex items-baseline gap-2">
        <p :class="eyebrow">{{ t("desk.quotes") }}</p>
        <span v-if="watchlist.length" class="terminal text-sm text-ink-muted">{{
          watchlist.length
        }}</span>
      </div>
      <p v-if="!watchlist.length" class="text-ink-muted">
        {{ t("desk.empty.quotes") }}
      </p>
      <table v-else class="terminal w-full text-sm select-text">
        <thead class="text-left font-sans text-ink-muted">
          <tr>
            <th class="pr-2 font-normal">{{ t("desk.col.instrument") }}</th>
            <th class="pr-2 text-right font-normal">
              {{ t("desk.col.last") }}
            </th>
            <th class="pr-2 text-right font-normal">{{ t("desk.col.bid") }}</th>
            <th class="pr-2 text-right font-normal">{{ t("desk.col.ask") }}</th>
            <th class="text-right font-normal">{{ t("desk.col.phase") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in watchlist"
            :key="row.id"
            class="border-t border-line"
          >
            <td class="flex items-center gap-1.5 py-1 pr-2">
              <span
                class="h-1.5 w-1.5 shrink-0 rounded-pill"
                :class="row.live ? 'bg-state-live' : 'bg-state-idle'"
              />
              <span class="truncate">{{ row.id }}</span>
            </td>
            <td class="py-1 pr-2 text-right">{{ row.last }}</td>
            <td class="py-1 pr-2 text-right text-ink-muted">{{ row.bid }}</td>
            <td class="py-1 pr-2 text-right text-ink-muted">{{ row.ask }}</td>
            <td class="py-1 text-right text-ink-muted">{{ row.phase }}</td>
          </tr>
        </tbody>
      </table>
    </section>

    <p v-if="failure" :class="failureText">{{ failure }}</p>
  </div>
</template>
