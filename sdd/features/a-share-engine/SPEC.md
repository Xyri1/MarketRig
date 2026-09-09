# A-share paper-trading engine — Feature SPEC

**Status:** Revised proposal, 2026-09-09; feasibility pending, no implementation slice.

_Decision basis: per D4, D20, D38, D75, D76, D78, D84; proposed amendments AE-1–AE-8._

CN only; US and HK retain their existing contracts. This proposal changes CN's phase-independent execution, GTC lifetime, and form-only validation boundary. Root SDD remains the delivered contract until the feasibility result is resolved and these amendments are reconciled before implementation. [FEASIBILITY.md](FEASIBILITY.md) owns the outstanding check; [RESEARCH.md](RESEARCH.md) holds supporting evidence.

## 1. T+1 and sellable quantity (AE-2)

### 1.1 Derived quantities

For desk `d`, instrument `i`, and the current Asia/Shanghai date:

```text
locked_quantity   = sum of today's BUY fill quantities for d/i
reserved_quantity = sum of remaining quantities of outstanding SELL orders for d/i
sellable_quantity = max(0, current position quantity - locked_quantity - reserved_quantity)
```

Read fills in the day's half-open nanosecond interval, parse their decimal text, and sum exactly; never use SQLite floating-point SUM over decimal text. Quantities and money stay decimal text externally. No new settlement ledger is required. Pending approvals are not outstanding sandbox orders and reserve nothing. A cancel request does not release shares until its terminal outcome is authoritative; partial fills reduce the reservation by filled quantity.

Compute from a consistent node position/order state and its committed fill history. Serialize eligibility validation and submission on the same desk action boundary so two competing sells cannot pass against the same shares. A new request cannot overtake persistence of an earlier fill. The feasibility check must establish the existing ordering mechanism before specifying an implementation.

### 1.2 Validation

Structural checks run first: catalog, positive quantity, integral shares, type/side, tick-aligned LIMIT price, BUY lot, and board/type cap. CN SELL does not run the old blanket lot-multiple check.

Execution-time checks then require §5's session and §2's data readiness; SELL must not exceed sellability before applying §3's odd-remainder rule. A failed eligibility or quantity check answers `ORDER_INVALID` with requested, sellable, locked, and reserved quantities and the reason. For example:

```text
quantity 200 exceeds sellable 100 for 600519.XSHG: 100 bought today are locked by T+1; 100 reserved by outstanding sells
```

The sandbox still decides cash sufficiency and other native denials; MarketRig records every resulting event verbatim. Approval-time behavior is §5.2.

### 1.3 Projection

Current CN positions gain decimal-text `sellable_quantity`, `locked_quantity`, and `reserved_quantity`. US/HK positions omit them. These are read-time eligibility projections, not new stored trading facts. Sellability does not promise the session is open or data is ready. Do not attach today's eligibility to historical fills, closed cycles, or historical position snapshots. The CLI's existing historical record surface remains historical; no new CLI awareness command is added.

## 2. Price bands and execution data (AE-3, AE-8)

### 2.1 Readiness and provenance

For HiThink CN, matching and submission require a usable current-trading-day band: positive valid reference price, verified date attribution, verified ex-rights/ex-dividend reference semantics, confirmed calendar, and a healthy current feed observation. Missing/invalid reference, inconsistent price, uncertain date, feed failure, or node failure blocks execution with `MARKET_UNAVAILABLE` and a specific reason. No silent disabling of the band.

How HiThink establishes the date and reference semantics is an explicit feasibility blocker. A successful HTTP request after midnight, receipt time, or a changed price triple is not proof. Determine the least additional authoritative evidence needed; do not invent provider fields. A valid reference may be retained within its proven trading date, but matching resumes only with a healthy usable observation. Day rollover invalidates readiness until the new day's evidence is established.

Yahoo CN remains explicitly simplified: band fields are null and the band/limit-fill policy is unavailable. T+1, quantity rules, confirmed-calendar requirement, session restriction, and day lifetime still apply. An unavailable confirmed calendar also blocks Yahoo CN execution. Never automatically fall back from HiThink to Yahoo.

Expose execution availability and its reason separately from quote health/market phase, including when auction hours fall inside the existing phase envelope. Exact field/error-reason vocabulary and freshness bound are to be settled from feasibility evidence before an implementation slice; no client may infer eligibility from `health: LIVE` alone.

### 2.2 Calculation and catalog

Catalog entries must identify the supported board sufficiently to derive band and order caps. Main board: 10%; ChiNext: 20%. STAR/Beijing and exceptional listing regimes are not admitted by adding a band field alone.

With verified reference `prev`, tick `tick`, and percentage `band`:

```text
limit_up   = round_half_up(prev * (1 + band/100), tick)
limit_down = round_half_up(prev * (1 - band/100), tick)
if limit_up - prev < tick: limit_up = prev + tick
if prev - limit_down < tick: limit_down = prev - tick
limit_down = max(tick, limit_down)
```

Use exact decimal arithmetic. A last price outside the calculated band is inconsistent input and blocks execution; it is not clamped into a valid fill price.

### 2.3 Observation and conservative policy

CN quote/book resources carry `prev_close`, `limit_up`, `limit_down`, and the proven band date when available. Replace the draft `limit_struck` vocabulary with a price-condition field whose values are `AT_UPPER_LIMIT`, `AT_LOWER_LIMIT`, or null. Prices equal to a boundary describe a price condition, not counterparties. No fabricated source timestamp.

The active policy is explicit: with HiThink bands, no BUY fill at the upper limit and no SELL fill at the lower limit. The opposite direction may fill, subject to every other check. Inside the band, ordinary synthesized matching applies. This includes resting orders and partial fills. All resulting fill prices must be within the band.

Zero-size opposite-side quotes are a candidate mechanism, not the contract. Prove book-to-core propagation and band-safe remainder behavior in FEASIBILITY. At a suppressed side, a new MARKET order is expected to receive the sandbox's own no-market rejection; preserve the actual event/reason and do not rename it exchange cancellation. LIMIT orders may rest until eligible liquidity returns or they expire. If the pinned integration cannot produce these outcomes, revise the proposal rather than implement post-fill correction.

### 2.4 Limit-price validation

A LIMIT price outside the inclusive band answers `ORDER_INVALID`, naming the instrument, reference, date, and bounds. A valid crossing limit is eligible to fill, not guaranteed to rest. A valid non-crossing limit rests subject to lifecycle rules.

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

New-desk constitution text must explain CN T+1, the three quantity projections, supported continuous sessions, expiry at 14:57, data-readiness blocks, HiThink bands, conservative no-fill-at-limit behavior, and simplified Yahoo mode. Also name synthetic spread/depth, unsupported auctions/halts/after-hours, approximate fees, and missing corporate-action accounting. US/HK behavior stays as declared today. Existing AGENTS.md is never rewritten.

Keep CLI, MCP, errors, and seed text English under both desktop locales. Quote/book/instrument resources expose the active execution restrictions and readiness for all desks, including those with older constitutions. The exact seeded paragraph follows the finalized contract after feasibility; do not claim an unproven capability in a byte-for-byte seed test now.

## 5. Session, approvals, and recovery (AE-1, AE-7, AE-8)

### 5.1 Session and day lifetime

On a confirmed exchange trading day, permit execution only in [09:30,11:30) and [13:00,14:57) Asia/Shanghai. Outside those intervals new submissions fail `ORDER_INVALID` naming the supported session; resting orders cannot fill. No order is queued for the next session implicitly.

Lunch suspends fills without expiring orders. At 14:57, all remaining CN orders terminate through a supported sandbox lifecycle path and release reservations. This simulator-day expiry deliberately precedes the unmodeled closing auction. Expose the actual native time-in-force and terminal events; do not merely label a GTC order DAY. Determine native DAY support versus supported scheduled cancellation in feasibility and record the chosen mapping before implementation.

Boundary transitions must run without a quote. Withholding the next poll is insufficient. At a boundary, timer, submission, data delivery, and matching must be ordered so no fill with an out-of-session instant can occur. Cancellation remains available while the session/data gate is closed, except existing pending-approval cancellation semantics remain unchanged.

### 5.2 Approvals and action integrity

Under Require approval, structural checks and attribution/readiness of the desk retain the existing pending-action path without consulting the trading node. No inventory is reserved and no dynamic eligibility promise is made. On approval, start/restore the node if needed, then rerun session, band, sellability, quantity, and native checks from the stored request.

Temporary execution unavailability leaves the action pending under `MARKET_UNAVAILABLE`, without recording an approval decision. An available-node eligibility refusal resolves an approved attempt as a terminal refused action with the current reason and no accepted sandbox order. Persist approval/outcome transitions atomically according to the finalized path. Inspect existing accept/approval behavior in feasibility before assigning exact transaction mechanics. No automatic retry or resubmission; idempotent replay returns the stored record. This explicitly amends the root approval contract for CN.

### 5.3 Recovery and provider transitions

After restart, reconcile the order's owning trading date and terminal deadline before making the node executable. Prior-day or expired orders terminate exactly once through supported native events before any replay can match them. Current-day restoration waits for session/readiness gates; retain original identifiers and do not duplicate fills or reservations.

Feed failure or loss of current-day reference suspends all affected resting fills, including those otherwise possible against cached quotes. Recovery establishes a coherent band/book for that desk before allowing matching. Provider switching first blocks CN execution and clears/reconciles stale executable state, then enables the explicitly selected mode. It must not create a transient unchecked fill. Both awareness and matching must use the same accepted reference; another desk's newer installation-wide observation cannot validate an order against a different local book.

The mechanism for gating, expiry, and restoration is unresolved until FEASIBILITY passes. A saved snapshot or source-level API signature is not execution evidence.

## 6. Required checks

The implementation slice is created only after the feasibility record is resolved. Required tests use the real pinned sandbox at the integration boundary; pure arithmetic tests supplement them. A shared controlled clock must drive the actual node/lifecycle path for deterministic day transitions, not only a standalone sellability function. Its test-only mechanism must be proven first.

| Check                      | Required evidence                                                                                                                                                                                                       |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1 — T+1 and reservation   | Same-day buy/sell refusal; competing sells, partial fills, cancel confirmation, and approval revalidation; next-session sell succeeds. Exact decimal summation and Shanghai midnight boundaries.                        |
| A2 — bands and quantities  | Main/ChiNext ratios, rounding corners, invalid references, each cap boundary, odd-lot accepted/refused cases. Buy limit 1856.80 against ask 1700 crosses; use a buy below 1700 for the resting case.                    |
| A3 — limit-fill policy     | Upper and lower limits, both sides, MARKET and resting LIMIT; actual native rejection where applicable; resumed fills only when eligible; every fill within band, including quantities larger than displayed liquidity. |
| A4 — session and expiry    | Lunch pause/resume; 14:57 terminal lifecycle without a quote; night/weekend/holiday blocked; no transient fills at exact boundaries.                                                                                    |
| A5 — restart and data loss | Before/after deadline restarts, original IDs, terminal exactly once, no prior-day replay fill; missing/day-stale bands, feed failure/recovery, provider switch, different desk observation timing.                      |
| Surface and seed           | CN-only eligibility fields; historical records unchanged; current execution availability distinct from phase/health; new seed describes final supported behavior, old seed unchanged; US/HK unaffected.                 |

Integrate A1–A5 after the existing HiThink gate scenarios once implementation begins. Existing CN scenarios that assume phase-independent trading or immediate round trips must be updated in that implementation slice; do not leave the gate dependent on wall-clock market hours.

E7: attended session reads real research and a verified current-day band, buys, observes the lock, and receives the same-day sell refusal. A later supported trading session sells, closes the native cycle, and queues evaluation. The first sitting alone is partial/inconclusive, never full completion. Hold over a corporate-action date only with the accounting limitation explicitly recorded; do not attribute omitted dividend effects to strategy performance.

Repository static/module checks apply to implementing changes. For this design-only revision, check local links, document consistency, and completeness of the feasibility handoff; no runtime pass is claimed.
