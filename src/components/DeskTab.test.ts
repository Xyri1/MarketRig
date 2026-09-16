import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", async () => ({
  invoke: (await import("../test/fakeDaemon")).fakeInvoke,
}));

import { flushPromises } from "@vue/test-utils";
import { client } from "../client/client.gen";
import { installFakeDaemon } from "../test/fakeDaemon";
import { mountWithI18n } from "../test/mountWithI18n";
import DeskTab from "./DeskTab.vue";

const cycle = (id: string, pnl: string, closed: number) => ({
  id,
  position_id: `p-${id}`,
  instrument_id: "AAPL.XNAS",
  opened_at_ns: closed - 1,
  closed_at_ns: closed,
  realized_pnl: pnl,
  currency: "USD",
});

beforeEach(() => {
  client.setConfig({ baseUrl: "http://127.0.0.1:7100" });
  installFakeDaemon({
    "GET /desks/d-1/market/quotes": () => ({
      status: 200,
      body: {
        quotes: [
          { instrument_id: "AAPL.XNAS", last: "150.10", health: "AVAILABLE" },
        ],
      },
    }),
    "GET /desks/d-1/market/book": () => ({
      status: 200,
      body: {
        book: [
          {
            observation: { instrument_id: "AAPL.XNAS" },
            bid_price: "150.00",
            ask_price: "150.20",
          },
        ],
      },
    }),
    "GET /desks/d-1/positions": () => ({
      status: 200,
      body: {
        positions: [
          {
            position_id: "p-1",
            instrument_id: "AAPL.XNAS",
            side: "LONG",
            quantity: "10",
            average_open_price: "150.00",
            currency: "USD",
          },
        ],
      },
    }),
    "GET /desks/d-1/orders": () => ({ status: 200, body: { orders: [] } }),
    "GET /desks/d-1/history/fills": () => ({
      status: 200,
      body: { fills: [] },
    }),
    // Newest first, as the daemon lists them; 0.1 + 0.2 must not print as a float.
    "GET /desks/d-1/history/cycles": () => ({
      status: 200,
      body: {
        cycles: [
          cycle("c-3", "-0.50", 3),
          cycle("c-2", "0.2", 2),
          cycle("c-1", "0.1", 1),
        ],
      },
    }),
  });
});

it("totals realized cycles exactly, draws one bar per cycle, and merges the book top", async () => {
  const wrapper = mountWithI18n(DeskTab, { deskId: "d-1" });
  await flushPromises();
  const cycles = wrapper.get('[data-testid="desk-cycles"]');
  expect(cycles.text()).toContain("-0.20");
  expect(cycles.text()).toContain("USD");
  expect(cycles.findAll("rect")).toHaveLength(3);
  expect(cycles.findAll("rect.fill-state-failure")).toHaveLength(1);
  expect(wrapper.get('[data-testid="desk-positions"]').text()).toContain(
    "AAPL.XNAS",
  );
  const quotes = wrapper.get('[data-testid="desk-quotes"]').text();
  expect(quotes).toContain("150.10");
  expect(quotes).toContain("150.20");
});
