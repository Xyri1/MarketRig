# A-share paper-trading engine — Feature SPEC

**Status:** Implemented on macOS 2026-09-10 by [slice 014](../../slices/014-a-share-engine.md) (the "as built" paragraphs below record the integration); Windows checks and the attended E7 close remain before root merge.

_Decision basis: per D4, D20, D38, D75, D76, D78, D84; proposed amendments AE-1–AE-9._

CN only; US and HK retain their existing contracts. This proposal changes CN's phase-independent execution, GTC lifetime, and form-only validation boundary. AE-1–AE-9 record the slice’s amendments; root SDD remains the delivered contract until exit checks pass and changes are merged back. [FEASIBILITY.md](FEASIBILITY.md) records the completed spike and known implementation defects; [RESEARCH.md](RESEARCH.md) holds supporting evidence.

## 1. T+1 and sellable quantity (AE-2)

### 1.1 Derived quantities

For desk `d`, instrument `i`, and the current Asia/Shanghai date:

```text
locked_quantity   = sum of today's BUY fill quantities for d/i
reserved_quantity = sum of remaining quantities of outstanding SELL orders for d/i
sellable_quantity = max(0, current position quantity - locked_quantity - reserved_quantity)
```

Read fills in the day's half-open nanosecond interval, parse their decimal text, and sum exactly; never use SQLite floating-point SUM over decimal text. Quantities and money stay decimal text externally. No new settlement ledger is required. Pending approvals are not outstanding sandbox orders and reserve nothing. A cancel request does not release shares until its terminal outcome is authoritative; partial fills reduce the reservation by filled quantity.

Compute from the node cache’s positions, native BUY fill events, and all nonterminal SELL orders, including INITIALIZED orders. F6 proves that storage can lag this cache within dispatch. Read eligibility and place the order in the same Node::call so two competing sells cannot reserve the same shares; do not base admission on a lagging SQL projection.

### 1.2 Validation

Structural checks run first: catalog, positive quantity, integral shares, type/side, tick-aligned LIMIT price, BUY lot, and board/type cap. CN SELL does not run the old blanket lot-multiple check.

Execution-time checks then require §5's session and §2's data readiness; SELL must not exceed sellability before applying §3's odd-remainder rule. A failed eligibility or quantity check answers `ORDER_INVALID` with requested, sellable, locked, and reserved quantities and the reason. For example:

```text
quantity 200 exceeds sellable 100 for 600519.XSHG: 100 bought today are locked by T+1; 100 reserved by outstanding sells
```

The sandbox still decides cash sufficiency and other native denials; MarketRig records every resulting event verbatim. Approval-time behavior is §5.2.

### 1.3 Projection

Native cash-account SELL reservations may place a share count in the account’s currency-labeled locked slot (F8). Do not label that value reserved cash or derive spendable cash from it. Expose the named share-eligibility projections below; retain native account payloads without reinterpretation.

Current CN positions gain decimal-text `sellable_quantity`, `locked_quantity`, and `reserved_quantity`. US/HK positions omit them. These are read-time eligibility projections, not new stored trading facts. Sellability does not promise the session is open or data is ready. Do not attach today's eligibility to historical fills, closed cycles, or historical position snapshots. The CLI's existing historical record surface remains historical; no new CLI awareness command is added.

## 2. Price bands and execution data (AE-3, AE-8)

### 2.1 Readiness and provenance

For HiThink CN, readiness requires: a successful calendar response containing today (Asia/Shanghai); a successful unadjusted daily bar with date equal to today for the instrument; and a valid snapshot carrying positive `prev_price`, positive tick-aligned last price within the derived band, and nonnegative cumulative volume. No snapshot/bar price equality or tolerance is required. These establish a simulation assumption: the snapshot and `prev_price` describe that day. They do not independently certify it. `prev_price` reference behavior is observed, not a documented ex-rights guarantee. A local dividend-derived price must not override or gate it.

At startup and day rollover, readiness starts unavailable. Obtain calendar and per-instrument bar evidence once for that day; retain successful evidence in memory during the day. A subsequent failed redundant calendar/bar refresh does not revoke successful same-day evidence; contradictory successful evidence does block readiness. Missing evidence is retried on the existing poll cadence with bounded backoff, not a tight loop. A changed `prev_price` during the day blocks that instrument until the next day; no automatic intraday rebasing. Never carry date evidence across rollover or persist it as permanently verified provenance.

A failed snapshot request, malformed/missing instrument data, or node failure blocks affected submissions and resting fills with MARKET_UNAVAILABLE and a specific reason. Successful recovery installs coherent evidence and resets §2.5’s observation baseline before matching. A successful but unchanged snapshot is not itself an error. A stale snapshot that continues to return successfully may escape detection: source delay remains unknown, and no bounded source freshness is promised. Any displayed age is age since receipt, never market-data age.

Keep the existing batched per-node cadence: 10 seconds while any CN order/position is open, 30 seconds idle, with existing phase suppression and initial subscription read. This is not a provider quota. Back off on HTTP 429 or envelope 429/4001 using the existing three-attempt bound (500 ms then 1 second), and on existing retryable failures. After exhaustion, execution stays unavailable until a successful poll; no automatic order resubmission. Research traffic and other desk nodes share the key's unknown service limits.

Yahoo CN retains its explicitly simplified native quote-matching model, with null bands and no AE-9 claim. T+1, quantities, confirmed calendar, sessions and scheduled cancellation still apply. Missing confirmed calendar blocks Yahoo CN execution as well. Never automatically fall back from HiThink to Yahoo.

Expose execution availability/reason independently from health and phase, inferred band date, receipt age, source delay unknown, and the active fill policy. Auction fields do not authorize execution or detect halts. No client may infer eligibility from health: LIVE alone.

Concretely (slice 014): every CN entry of the desk-scoped quote and book resources carries `execution: {availability: OPEN | PAUSED | CLOSED | UNAVAILABLE, reason, band_date, band_date_inferred: true, receipt_age_ms, source_delay: "UNKNOWN", fill_policy: HITHINK_SAMPLED | YAHOO_SIMPLIFIED}`. `reason` is one of `NO_CALENDAR`, `CALENDAR_REFUSED`, `NOT_TRADING_DAY`, `DATE_UNPROVEN`, `NO_REFERENCE`, `REFERENCE_CHANGED`, `PRICE_OUT_OF_BAND`, `FEED_LOST`, `RATE_LIMITED`, `PUBLICATION_FAILED`, `PUBLICATION_PENDING`, `NODE_NOT_STARTED`, and null while available. `fill_policy` is the desk's A-share feed, so it is uniform across the desk's CN entries. `PAUSED` is the lunch break, a volume-decrease reset, or a failed publication; `CLOSED` is every other out-of-session instant. Readiness is held per desk node, in memory, carries the Shanghai date it was established on, and reads `NO_CALENDAR` on any later date until a poll on that day re-establishes it, so a boundary alert or an admission never trusts the previous day's evidence; it starts `UNAVAILABLE`/`NO_CALENDAR` on every node start for every A-share feed; the confirmed calendar is fetched whenever a provider key is present, whichever feed is selected, so a desk with no provider never executes CN. US/HK entries carry no `execution` object. The instrument resource carries `board`, `band_percent`, and the LIMIT/MARKET caps on CN entries; availability is per desk and lives only on quote/book.

### 2.2 Calculation and catalog

Catalog entries must identify the supported board sufficiently to derive band and order caps. Main board: 10%; ChiNext: 20%. STAR/Beijing and exceptional listing regimes are not admitted by adding a band field alone.

With provider reference `prev` accepted under §2.1, tick `tick`, and percentage `band`:

```text
limit_up   = round_half_up(prev * (1 + band/100), tick)
limit_down = round_half_up(prev * (1 - band/100), tick)
if limit_up - prev < tick: limit_up = prev + tick
if prev - limit_down < tick: limit_down = prev - tick
limit_down = max(tick, limit_down)
```

Use exact decimal arithmetic. A last price outside the calculated band is inconsistent input and blocks execution; it is not clamped into a valid fill price.

### 2.3 Observation and conservative policy

CN quote/book resources carry `prev_close`, `limit_up`, `limit_down`, and the inferred band date when available. Replace the draft `limit_struck` vocabulary with a price-condition field whose values are `AT_UPPER_LIMIT`, `AT_LOWER_LIMIT`, or null. Prices equal to a boundary describe a price condition, not counterparties. No fabricated source timestamp.

When the observed last price equals the upper limit, suppress BUY fills; at the lower limit, suppress SELL fills. The opposite direction may fill subject to every other check. A LIMIT fill at a boundary price is allowed when its triggering observation is inside the band. All fill prices remain within the band. This is a policy against the observed condition, not an assertion about the exchange queue.

F4 proves zero-size side propagation; F8 proves the temporal publication model in §2.5 with serialized admission. At a suppressed side, MARKET receives native no-market rejection; LIMIT remains open until a qualifying observation or cancellation. Preserve actual events/reasons. Production integration must preserve the F8 mechanism without fabricated fills or post-fill correction.

### 2.4 Limit-price validation

A LIMIT price outside the inclusive band answers `ORDER_INVALID`, naming the instrument, reference, date, and bounds. Even a compatible LIMIT rests at its own submission. Section 2.5 defines ordinary subsequent-volume triggers and the separate MARKET publication exception.

### 2.5 HiThink order-trigger and fill policy (AE-9)

For ordinary feed observations, the following temporal rule applies; MARKET publications follow the explicit exception below. On execution-time admission (approval time for a pending action), record the node's latest accepted observation sequence and cumulative volume as the LIMIT baseline. Read that baseline and submit atomically with the eligibility checks, inside the same per-node critical section that publishes an observation to the node, confirms it landed, and restores the idle book (F8 item 2: a compatible LIMIT admitted while a crossing book stands fills at submission). A snapshot already received before admission, including one still queued for delivery, cannot trigger this order. A subsequent accepted observation qualifies only when its volume exceeds both the order baseline and the preceding accepted observation's volume, and its last price is <= the BUY limit or >= the SELL limit. All session, readiness and direction gates must also hold. Increasing volume with an incompatible price advances the preceding-observation baseline; a later price-only change without more volume does not qualify. Equal-volume feed snapshots never trigger LIMIT fills; MARKET publications are exempt from this temporal/volume rule.

The first qualifying observation permits the full remaining quantity at the limit price, with native sufficiency checks. The observed volume increase is a trigger, not fillable quantity: it is not allocated among orders or desks, and queue priority, partial liquidity, price improvement and missed intrapoll trades are not modeled. No fill is backdated to an inferred transaction time. BUY 100 @ 10 submitted after a snapshot at 9.90/volume 1000 waits; 9.95/1001 may fill 100 @ 10. A SELL limit uses the reverse price comparison. Compatible price without increased volume does not fill through the ordinary feed path.

MARKET means immediate synthetic execution against the latest usable snapshot: full requested quantity at last price, no artificial spread or slippage, subject to native cash/holdings checks and all other gates. It does not assert that a real trade occurred after submission. At a suppressed direction it receives native no-market rejection. A MARKET is not parked awaiting future volume. The observation a MARKET executes against is itself a qualifying observation for every compatible resting LIMIT on that instrument, regardless of volume: those LIMITs fill first at their own limit price, then the MARKET fills at last (per AE-9, settled from F8 item 3). The snapshot may have unknown source delay, as disclosed in §2.1.

On restart, feed recovery, or provider switch, clear executable stale state and establish a first usable observation as baseline without filling restored LIMITs. They require a subsequent qualifying feed observation or an otherwise eligible MARKET publication; re-baselining itself never fills. On a same-day cumulative-volume decrease, pause, discard temporal baselines, and use the next valid observation only to re-establish them; no fill on reset. Day rollover cancels the previous day's orders. Ordinary lunch reopening retains the same-day baseline; reopening status alone never fills. A qualifying feed observation or eligible MARKET publication is required.

Keep one per-node critical section around observation publication, confirmation, idle-book restoration, and every admission/approval. Confirm both the crossing publication and the restored idle book before releasing admission; failure or interruption leaves the instrument non-executable until recovery clears and re-baselines it. Boundary timers and cancellation must remain able to stop execution without waiting indefinitely for a failed publication. Test interleavings rather than assuming queue insertion orders delivery.

The idle book has zero-sized sides. Qualifying feed publications size the relevant side for the eligible remaining quantities; MARKET publications size each unsuppressed side sufficiently for the compatible resting quantities and the MARKET request, preserving full fills without remainder slippage. After confirmation restore the idle book. Native matching selects compatible prices; MarketRig supplies the simulation observation and exact quantities, never creates fill events. Observation baselines are in memory only and reset after restart/recovery; confirm-before-admission permits one preceding-volume baseline per node/instrument rather than redundant durable per-order metadata.

MARKET publication can execute previously authorized resting orders before the new MARKET is accepted by native risk checks. Therefore a native MARKET denial does not imply that those other orders remained unchanged. Read authoritative order/fill outcomes; do not roll back their fills or automatically retry the MARKET. Structural/session/readiness failures detected before publication publish nothing. Direction suppression applies independently to both sides.

These are required native-engine outcomes, not permission to implement a daemon matcher, delay orders in an unmodeled daemon queue, or fabricate fills. F8 proved per-order timing, full-quantity limit-price fills and the MARKET model on the pinned sandbox with the idle-book / one-sided crossing publish mechanism recorded in [FEASIBILITY](FEASIBILITY.md) "F8 result"; MARKET/LIMIT isolation on one book is not available and is replaced by the rule above.

## 3. Quantities and fees (AE-4, AE-5)

### 3.1 Lots

BUY: positive multiple of 100 for supported CN boards. SELL, after checking `q <= sellable_quantity`: valid when `q % 100 == 0` or `q % 100 == sellable_quantity % 100`.

Unreserved sellable 250 permits 50, 100, 150, 200, 250; 125 is refused. Sellable 150 permits 50, 100, 150. Sellable 200 permits 100, 200, not 50 or 150. Open-order reservations and partial fills must not allow splitting one odd remainder across multiple submissions; verify this against actual node state before finalizing the implementation.

### 3.2 Caps

| Supported board | LIMIT shares | MARKET shares |
| --------------- | -----------: | ------------: |
| Main board      |    1,000,000 |     1,000,000 |
| ChiNext         |      300,000 |       150,000 |

At-cap quantities pass this structural check; over-cap quantities fail with `ORDER_INVALID`. Passing form does not imply sufficient funds or a fill.

### 3.3 Fees

3 bp each side, computed by the sandbox. This approximates costs and does not model actual stamp duty, commission minimums, or side-aware charges. The pinned sandbox configuration, rather than the underlying FeeModelHandle, is the custom-model limitation.

## 4. Agent-visible contract (AE-6)

New-desk constitution text must explain CN T+1, the three quantity projections, supported continuous sessions, expiry at 14:57, data-readiness blocks, HiThink bands, conservative direction suppression, sampled LIMIT triggering, and immediate synthetic MARKET pricing, and simplified Yahoo mode. Also name inferred snapshot/reference date, unknown source delay, missed crossings, full-quantity synthetic liquidity, unsupported auctions/halts/after-hours, approximate fees, and missing corporate-action accounting. US/HK behavior stays as declared today. Existing AGENTS.md is never rewritten.

Keep CLI, MCP, errors, and seed text English under both desktop locales. Quote/book/instrument resources expose the active execution restrictions and readiness for all desks, including those with older constitutions. The exact seeded paragraph follows the finalized contract after feasibility; do not claim an unproven capability in a byte-for-byte seed test now.

### 4.1 The seeded constitution

The `## The paper environment` section of `crates/marketrigd/seed/AGENTS.md` is this block, byte for byte — it is the one section this feature rewrites, and `marketrigd`'s `desk::seeds_are_the_spec_blocks` compares the shipped file against it. Its last paragraph is `r5-desktop-approval-controls` §3.3's, unchanged. The `## Surfaces` section stays `hithink-a-share` §5.3's.

```markdown
## The paper environment

Paper only, on a NautilusTrader sandbox with a cash account per desk: no shorting, no leverage,
`MARKET` and `LIMIT` orders good till cancelled, realized P&L in each instrument's own currency, fees
at each market's declared per-side rate. A position cycle (open to flat in one instrument) is the
unit of realized P&L and of evaluation. For `US` and `HK`, not simulated: T+1 settlement, daily price
limits, trading halts, opening and closing auctions, and holiday calendars — a quote may read stale
on a holiday.

`CN` is modelled further. Every `CN` quote and book entry carries an `execution` object —
`availability` of `OPEN`, `PAUSED`, `CLOSED` or `UNAVAILABLE`, a `reason` when it is unavailable, the
inferred `band_date`, `receipt_age_ms`, `source_delay: "UNKNOWN"`, and the active `fill_policy`.
Read it before you order: `health` and the market phase authorize nothing.

- **T+1.** Shares bought today cannot be sold today. A current `CN` position carries
  `sellable_quantity`, `locked_quantity` (bought today) and `reserved_quantity` (held by your own
  outstanding sells). A `BUY` is a multiple of 100; a `SELL` is a multiple of 100 or exactly the odd
  remainder of `sellable_quantity`. Per-order caps are 1,000,000 shares on the main board and
  300,000 (`LIMIT`) or 150,000 (`MARKET`) on ChiNext.
- **Sessions.** Execution is supported only in [09:30,11:30) and [13:00,14:57) Asia/Shanghai on a
  confirmed exchange trading day. Lunch suspends fills without ending an order. Orders are `GTC`;
  at 14:57 every remaining `CN` order is canceled and reports `OrderCanceled`, deliberately before
  the closing auction, which is not modelled. Nothing is queued for the next session.
- **Readiness.** Execution needs a confirmed trading day for today, a current-day daily bar for the
  instrument, and a valid snapshot. Until all three hold, `availability` is `UNAVAILABLE` with a
  `reason` — `NO_CALENDAR`, `NOT_TRADING_DAY`, `DATE_UNPROVEN`, `NO_REFERENCE`, `REFERENCE_CHANGED`,
  `FEED_LOST` and the like — and an order is refused rather than parked.
- **Bands (HiThink feed).** The daily band is 10% of the provider reference on the main board and 20%
  on ChiNext; a quote carries `prev_close`, `limit_up`, `limit_down` and `band_date`. A `LIMIT` price
  outside the inclusive band is refused. At the upper limit no `BUY` fills, at the lower limit no
  `SELL` fills; the other direction still may. `price_condition` names that condition, not a
  counterparty.
- **Fills (HiThink feed).** A `LIMIT` rests at submission, even at a compatible price. It triggers on
  a later snapshot whose cumulative volume has risen and whose last price is at or through your
  limit, and the whole remainder then fills at your limit price; equal volume never triggers a fill.
  A `MARKET` executes immediately against the latest snapshot — the full quantity at the last price,
  no spread and no slippage. That same publication first fills every compatible resting `LIMIT` at
  its own limit price whatever the volume, so a `MARKET` can move your other orders, and a `MARKET`
  the sandbox then denies does not undo them.
- **Yahoo `CN` feed.** Explicitly simplified: no bands, no volume rule, native quote matching alone.
  T+1, the sessions, the quantity rules, `GTC` and the 14:57 cancel still apply, and it still needs
  the confirmed calendar.

What `CN` does not model: the snapshot and its reference date are inferred from receipt, never
certified; source delay is unknown and no freshness is promised; trades between polls are missed;
the liquidity an order fills against is synthetic and always sufficient; opening, closing and
volatility auctions, halts and after-hours trading are unsupported; fees are a flat 3 bp per side
rather than real stamp duty and commission; and dividends, splits and other corporate actions are
not accounted for anywhere.

The user may require approval of paper orders and of trigger code. A gated order answers
`approval: PENDING` with no order and reaches the sandbox only once approved in the MarketRig
desktop; read its state with `marketrig history actions <name>`. A gated trigger is never due
until approved (`marketrig trigger show`). You cannot approve, deny, or change the policy.
```

## 5. Session, approvals, and recovery (AE-1, AE-7, AE-8)

### 5.1 Session and day lifetime

On a confirmed exchange trading day, permit execution only in [09:30,11:30) and [13:00,14:57) Asia/Shanghai. Outside those intervals new submissions fail `ORDER_INVALID` naming the supported session; resting orders cannot fill. No order is queued for the next session implicitly.

Lunch suspends fills without expiring orders. Native TIF is GTC. At 14:57, a kernel-clock alert dispatches CancelOrder for every remaining CN order; OrderCanceled releases reservations. This deliberately precedes the unmodeled closing auction. Expose GTC and OrderCanceled; never report DAY or OrderExpired. F2 establishes that native DAY/GTD does not provide the required pre-match deadline.

Boundary transitions must run without a quote. Withholding the next poll is insufficient. At a boundary, timer, submission, data delivery, and matching must be ordered so no fill with an out-of-session instant can occur. Cancellation remains available while the session/data gate is closed, except existing pending-approval cancellation semantics remain unchanged.

### 5.2 Approvals and action integrity

Under Require approval, structural checks and attribution/readiness of the desk retain the existing pending-action path without consulting the trading node. No inventory is reserved and no dynamic eligibility promise is made. On approval, start/restore the node if needed, then rerun session, band, sellability, quantity, and native checks from the stored request.

Temporary execution unavailability leaves the action pending under `MARKET_UNAVAILABLE`, without recording an approval decision. An available-node eligibility refusal resolves an approved attempt as a terminal refused action with the current reason and no accepted sandbox order: the stored outcome is `{"failure_code": "ORDER_INVALID", "reason": …}`, written where a sandbox refusal's outcome is written. Persist approval/outcome transitions atomically according to the finalized path. Inspect existing accept/approval behavior in feasibility before assigning exact transaction mechanics. No automatic retry or resubmission; idempotent replay returns the stored record. This explicitly amends the root approval contract for CN.

### 5.3 Recovery and provider transitions

After restart, reconcile the order's owning trading date and terminal deadline before making the node executable. Prior-day or expired orders terminate exactly once through supported native events before any replay can match them. Current-day restoration waits for session/readiness gates; retain original identifiers and do not duplicate fills or reservations.

Feed failure or loss of current-day reference suspends all affected resting fills, including those otherwise possible against cached quotes. Recovery establishes a coherent band/book for that desk before allowing matching. Provider switching first blocks CN execution and clears/reconciles stale executable state, then enables the explicitly selected mode. It must not create a transient unchecked fill. Both awareness and matching must use the same accepted reference; another desk's newer installation-wide observation cannot validate an order against a different local book.

Use F1’s per-instrument InstrumentStatus Pause/Close/Trading for session/data gating and F2’s clock-driven cancellation. Hold the first feed publish, restore orders, call Portfolio::initialize_orders to rebuild native reservations (R1), cancel expired orders, establish current readiness/status, then release observations. Execution event times must be monotonic across provider switches (F5), without relabeling them as source times. Partial-fill restart history must preserve the native chain; R1’s duplicate-accept/history-loss regression is required before delivery. Production integration must preserve F8’s publication/admission ordering, including interruption and error paths.

## 6. Required checks

Slice 014 follows the resolved macOS feasibility record. Required tests use the real pinned sandbox at the integration boundary; pure arithmetic tests supplement them. A shared controlled clock must drive the actual node/lifecycle path for deterministic day transitions, not only a standalone sellability function. Reuse F0’s proven node clock seam and extend it through daemon session and restart decisions; implementation checks must cover both macOS and Windows.

The seam as built (slice 014): the node clock is the single time source for every CN decision and CN stamp (session, trading date, owning day, the 14:57 deadline, HiThink receipt `ts_event`, CN `SubmitOrder`/`CancelOrder` `ts_init`); `store::now_ns()` stays wall clock for row bookkeeping. With `MARKETRIG_TEST_CLOCK_NS=<start_ns>` set alongside `MARKETRIG_TEST_DATA_ROOT`, every node and its sandbox execution client run on one controlled clock starting at that instant, and `PUT /test/clock {"now_ns"}` advances every started node, firing its due timers; the route is not served and not documented without the seam. The 11:30, 13:00, 14:57 and 09:30 boundaries are one self-rearming kernel-clock alert per node; the 14:57 cancellation is stamped at the alert instant.

| Check                      | Required evidence                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1 — T+1 and reservation   | Same-day buy/sell refusal; competing sells, partial fills, cancel confirmation, and approval revalidation; next-session sell succeeds. Exact decimal summation and Shanghai midnight boundaries.                                                                                                                                                                                                                                                                                                                    |
| A2 — bands and quantities  | Main/ChiNext ratios, rounding corners, invalid references, each cap boundary, odd-lot accepted/refused cases. A buy limit 1856.80 does not fill on submission at last 1700; a later volume-increasing compatible observation triggers at 1856.80, provided the active band permits it.                                                                                                                                                                                                                              |
| A3 — limit-fill policy     | Upper and lower limits, both sides, MARKET and resting LIMIT; actual native rejection where applicable; resumed fills only when eligible; every fill within band, including quantities larger than displayed liquidity.                                                                                                                                                                                                                                                                                             |
| A4 — session and expiry    | Lunch pause/resume; 14:57 terminal lifecycle without a quote; night/weekend/holiday blocked; no transient fills at exact boundaries.                                                                                                                                                                                                                                                                                                                                                                                |
| A5 — restart and data loss | Before/after deadline restarts, original IDs, terminal exactly once, no prior-day replay fill; missing/day-stale bands, feed failure/recovery, provider switch, different desk observation timing.                                                                                                                                                                                                                                                                                                                  |
| A6 — sampled execution     | Pre-admission queued observations excluded; compatible price with and without volume growth; incompatible-volume advance then equal-volume price crossing; multiple orders with different admission baselines; exact limit/full-quantity fills; MARKET publication fills compatible resting LIMITs first regardless of volume, leaves price-incompatible/suppressed directions untouched, then fills MARKET at last; denied MARKET and both-side coexistence; restart/reset baselines; native balances and history. |
| Surface and seed           | CN-only eligibility fields; historical records unchanged; current execution availability distinct from phase/health; new seed describes final supported behavior, old seed unchanged; US/HK unaffected.                                                                                                                                                                                                                                                                                                             |

Integrate A1–A6 after the existing HiThink gate scenarios once implementation begins. Existing CN scenarios that assume phase-independent trading or immediate round trips must be updated in that implementation slice; do not leave the gate dependent on wall-clock market hours.

As integrated (slice 014): A1–A6 run on real binaries over the HiThink stand-in and the controlled clock across scripted Shanghai days; G16 and O9's sandbox-refusal cases moved to a US instrument, because a CN order on a desk with no provider is refused by the execution boundary before the sandbox is consulted; H2's trading half runs on the controlled clock and its awareness half on the wall clock, because the awareness `calendar` label and `market_phase` are installation-wide and read wall time while every execution decision reads the node clock. Two A2 cases have no positive half at the gate layer: an odd sellable remainder cannot arise (every CN BUY is a lot multiple and AE-9 fills are full quantity), so only the odd-lot refusal is asserted, and an at-cap order exceeds the per-venue CNY seed at any in-band price, so the cap boundary is asserted as sandbox `ORDER_REJECTED` at the cap versus `ORDER_INVALID` naming the cap above it. Daemon refusals (`ORDER_INVALID`) answer HTTP 400; sandbox refusals (`ORDER_REJECTED`) answer 409.

E7: attended session reads real research and a band under the disclosed provider-reference assumptions, buys, observes the lock, and receives the same-day sell refusal. A later supported trading session sells, closes the native cycle, and queues evaluation. The first sitting alone is partial/inconclusive, never full completion. Hold over a corporate-action date only with the accounting limitation explicitly recorded; do not attribute omitted dividend effects to strategy performance.

Repository static/module checks apply to implementing changes. For this design-only revision, check local links, document consistency, and completeness of the feasibility handoff; no runtime pass is claimed.
