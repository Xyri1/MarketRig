# A-share engine feasibility check — handoff for Claude

**Status:** Initial spike DONE; follow-up NOT STARTED — see Follow-up handoff below. Initial run 2026-09-09 (macOS arm64).  
**Owner:** Claude, when the user starts this work  
**Scope:** bounded evidence spike; no product implementation slice opened  
**Blocks:** declaring the revised feature design complete and starting implementation

## Task

Determine whether the pinned Rust NautilusTrader sandbox can enforce the revised SPEC's CN session, day lifetime, readiness, and price-band restrictions through supported interfaces, including already-resting orders and restart. Resolve whether HiThink provides enough authoritative evidence for a usable current-day reference. Read PRD → DECISIONS → SPEC → ROADMAP at the root, then this feature's PRD, DECISIONS, SPEC, and RESEARCH. Read the current AGENTS.md.

Do not assume quote suppression disables cached matching, DAY configuration schedules expiry, a closed upstream issue means support shipped, or a source-level method proves the full execution path. Preserve the distinction between official exchange rules and our deliberate conservative fill policy.

## Isolation and bounds

- Use a worktree under `.worktrees/a-share-feasibility/` with a `codex/` branch, or the user-requested branch. Preserve unrelated changes. The feature folder may be untracked in the source checkout: deliberately copy these five feature documents into the isolated worktree if absent, without adding unrelated files.
- Reuse the existing node tests and stand-in HTTP feed. Every MarketRig binary invocation requires MARKETRIG_TEST_DATA_ROOT pointing to scratch inside the worktree; disable public trading feeds and use explicit local stand-ins. Do not touch the real data root, credentials, desks, or smoke wipe path.
- Keep the spike focused on public seams in the exact pinned crate version. No crate fork, pin bump, private-field mutation, second execution engine, daemon fee/P&L arithmetic, or edited fill events. Record a blocker if these become necessary.
- No real orders, no attended experiment, no full gate required for this spike. Real-provider reads, if already authorized and configured, are read-only and must never expose a key. If unavailable, record the provider question as unresolved rather than synthesizing evidence.
- Follow the repository Context7 workflow for library questions, then inspect pinned source. Development docs and Python/backtest APIs are not evidence for this Rust sandbox version.

## Inspect first

Find the current trading-node construction, data client publish path, shared observation state, submit/cancel/approval handlers, event persistence, snapshots, and restart replay. Trace all callers of any seam tested. Inspect pinned sandbox config/execution, matching engine, order book, clock/timer ownership, and native order lifecycle. Reuse existing test helpers instead of creating another harness.

## Questions and minimum runnable evidence

| ID  | Question                                                       | Smallest decisive experiment                                                                                                                                                                                                                                                                                                                                                                                          |
| --- | -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F1  | Can CN matching pause without disabling US/HK?                 | Rest a CN order, retain a crossing cached quote, cross 11:30 with a controlled clock, then deliver a crossing quote and attempt a new order while closed. No CN fill. Reopen at 13:00 with ready data; eligible order fills. Exercise both directions and unchanged US/HK execution.                                                                                                                                  |
| F2  | Can session-end termination occur without a tick?              | Rest an order before 14:57; advance time with no market data. Observe native terminal event, released reservation, no later fill. Record whether native DAY works or a supported scheduled cancellation is needed, and its exposed time-in-force. Test event ordering exactly at the boundary.                                                                                                                        |
| F3  | Can recovery terminate expired orders before matching?         | Persist a resting order, stop before its deadline, restart after the deadline/next trading day with immediately crossing data. Observe no fill, exactly one terminal outcome under the original ID, and correct reservation release. Also test a same-day lunch restart. Do not fake prior-day state without documenting how it was produced.                                                                         |
| F4  | Does conservative book shaping constrain every fill?           | On the real sandbox, zero the ask at upper limit and bid at lower limit; prove quote→ladder→matching-core propagation for new MARKET and already-resting LIMIT orders. Restore the side and observe eligible fills. Inspect every fill near both bands for oversized market remainders/slippage. Never clip a produced fill.                                                                                          |
| F5  | Can missing data suspend cached execution coherently?          | With a resting order, lose feed/band readiness, attempt crossing updates and submissions, then recover with a new band. No fill while blocked; no fill against the old book on reopening. Repeat provider transition and differing shared/local observation order.                                                                                                                                                    |
| F6  | Can checks and reservations be serialized with persistence?    | Two sells competing for the same sellable shares, partial fill, confirmed cancel, and pending approval followed by changed holdings/band/session. Only currently eligible execution occurs; no pending-approval reservation or duplicate action. Identify the native quantity increment needed for odd-lot sells.                                                                                                     |
| F7  | Is the reference actually current-day and ex-dividend-correct? | Trace the official HiThink contract for date and reference semantics. Verify a documented/live ex-dividend example against an authoritative exchange reference if available. Identify the minimal reliable evidence for date, calendar, freshness, and reference. Null snapshot timestamp plus receipt time is insufficient. If evidence is absent, mark blocked and propose the smallest alternative for discussion. |

F1–F6 must exercise the actual sandbox, not only pure predicates. Identify a controlled-clock seam that drives node timers, fill timestamps, readiness, persistence, and recovery consistently. If the real integration cannot use controlled time, record that as a deterministic-acceptance blocker; do not replace it with a long sleep or wall-clock-dependent passing claim.

## Deliverable and completion rule

Update this file with a result section containing:

- checked commit, crate/runtime versions, exact commands and exit codes;
- F1–F7 status: PASS / FAIL / BLOCKED / NOT RUN, with evidence paths and relevant native events;
- the smallest supported mechanism for gating, expiry, recovery ordering, and coherent validation; relevant source paths/symbols;
- actual public time-in-force/terminal behavior, proposed readiness fields/reasons/freshness rule, and any required test-only clock seam;
- provider-date/reference evidence and remaining limitations;
- recommendation: proceed, revise this design, or stop on a named blocker.

Keep runnable spike code and redacted evidence reproducible on the isolated branch; report that branch and worktree path. Do not remove it before the user can review the result. Record which platform was run; do not imply cross-platform runtime acceptance from a single platform.

Passing F1–F6 does not close unresolved F7. Source inspection alone is not PASS for execution scenarios. Do not mark the feature design complete, open an implementation slice, change root decisions, or implement the full feature automatically. Bring the evidence and any required design changes back for review.

## Result

**Status:** DONE 2026-09-09 — evidence spike complete; recommendation: **revise this design**, then proceed. No product code changed except two `#[cfg(test)]` seams. Nothing here marks the feature design complete or opens a slice.

### Run record

- Checked commit: `cfd2464` (master); spike branch `codex/a-share-feasibility` at `0274310` plus this file, worktree `.worktrees/a-share-feasibility/` (kept for review).
- Platform: macOS 26.3.1 arm64 (Darwin 25.3.0), rustc 1.98.0. No Windows run; no cross-platform claim.
- Crates: `nautilus-* =0.62.0` from the registry, unmodified. No pin bump, fork, private-field mutation, second engine, daemon P&L arithmetic, or edited fill event.
- Spike code: `crates/marketrigd/src/feasibility/{clock,f1_f5,f2,f3,f4,f6}.rs` (40 tests, `#![cfg(test)]`, all on the real `LiveNode` + `SandboxExecutionClient`, `feed_base: None`, every quote published by hand at a chosen instant). F7 evidence: `F7-EVIDENCE.md` and redacted `f7/`.
- Test-only seams (production byte-identical): `node.rs` (+22, `#[cfg(test)]` clock lookup + `ControlledSandboxFactory`), `trade.rs` (+27, `#[cfg(test)] place_form`).
- Commands, all exit 0, `CARGO_TARGET_DIR=<repo>/target`:

```text
cargo fmt --check                                              # 0
cargo clippy -p marketrigd --all-targets -- -D warnings        # 0
cargo test -p marketrigd --lib feasibility                     # 0 — 40 passed, 1.8 s
cargo test -p marketrigd --lib                                 # 0 — 199 passed (no regression)
```

F7 live reads: GET-only against the real HiThink service with the key from the operator's `.env`, passed only as a header, never written; responses redacted under `f7/`.

### F0 — controlled clock seam: PASS (with one required replacement)

`LiveNode::builder().with_clock_factory(|| clock.clone())` builds and runs the daemon's exact node (Sandbox environment, data client, four sandbox exec clients, `NodeRunMode::Hosted`) on one shared `TestClock`; the factory memoizes the kernel clock, so kernel and component clocks are the same instance. **But `SandboxExecutionClientFactory::create` hard-codes `LiveClock::default()`** (`nautilus-sandbox-0.62.0/src/factory.rs:80`), so every order/fill stamp stays wall-clock unless the daemon builds the client through the public `SandboxExecutionClient::new` with its own clock (`feasibility/clock.rs::ControlledSandboxFactory`, public API only). With that, every `order_events.occurred_at_ns` and `fills.occurred_at_ns` equals the injected instant. Time events must be dispatched by the test (`advance_time` → `match_handlers` → `TimeEventHandler::run`); a live node has no runner draining a `TestClock`. Limits: `store::now_ns()` rows (`book_snapshots.written_at_ns`, `trading_actions`, `operational_events`), `hand_to_node`'s `SubmitOrder.ts_init`, `trade::restore`'s own "now", and the poller's quote stamps remain wall-clock — the daemon has no session clock of its own, which is the deterministic-acceptance blocker below.

### F1–F7 status

| ID  | Status                                                                                                                     | Evidence                | Native events / facts                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| --- | -------------------------------------------------------------------------------------------------------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F1  | **PASS**                                                                                                                   | `f1_f5.rs` (8 tests)    | Withholding quotes freezes resting orders (no non-quote path matches: expiry-sweep timer, portfolio timer, another instrument's tick all proven inert) but **new orders fill against the cached book** out of session. The supported per-instrument gate is `InstrumentStatus` on the data path: `Pause`/`Close` → `process_status` → `iterate` skips (`matching_engine/mod.rs:3702`) and `process_order` rejects `Market 600519.XSHG is PAUSED                                                                                                                                                                                                                                                                                                                                                                                                                                                      | CLOSED, cannot accept order …` (`OrderInitialized, OrderSubmitted, OrderRejected`). `Trading` reopens; the reopen itself never matches, the first quote after it does. AAPL kept filling during the CN closure. |
| F2  | **PASS** (question) / **FAIL** (native DAY, GTD)                                                                           | `f2.rs` (4 tests)       | `TimeInForce::Day` is inert in the pinned stack. GTD+`expire_time` is accepted (`support_gtd_orders` defaults true) but expires only in post-match maintenance of a tick, and a crossing tick fills first, even stamped past the boundary. A kernel-clock `set_time_alert_ns` whose node-thread callback sends the same `CancelOrder` as `trade::cancel` terminates with no data: `OrderCanceled` at exactly the alert instant, locked CNY → 0, no later fill, `history_orders` shows `CANCELED`. Exposed TIF stays the plain string (`"GTC"` today, `"GTD"` if used).                                                                                                                                                                                                                                                                                                                               |
| F3  | **PASS** (ordering) / **FAIL as shipped** (race, reservation)                                                              | `f3.rs` (11 tests)      | A restored prior-day order fills on the first crossing quote; **the first feed poll beats `trade::restore` 3/3** (`flush_pending_data` runs before `is_running()`). Runner drains `exec_cmd_rx` before `data_evt_rx` (`biased` select, `nautilus-live-0.62.0/src/runner.rs:579-606`), so a `CancelOrder` queued in the restore job beats a queued quote: one `OrderCanceled`, chain replayed once, `history_orders` = `CANCELED`. Holding the data client's first publish until restore ran fixes the race (proven test-only). `MarketStatus` does **not** survive a restart (engine rebuilt `Open`). **Pinned-crate defect:** `CashAccount.balances_locked` is `#[serde(skip)]`, so a restored order that cancels without a fill leaves `locked` stranded (160000.00 before and after); fresh orders on the same node release correctly. Owning-day key: `ts_accepted` in the snapshot payload.     |
| F4  | **PASS** (mechanism) with one invariant                                                                                    | `f4.rs` (9 tests)       | A zero-size L1 side clears the ladder (`ladder.rs:238-244`), `core.ask = None`; new MARKET → `OrderRejected` reason `No market for 600519.XSHG` (`process_market_order`, `mod.rs:3234`); resting and new LIMIT rest untouched across repeats; opposite side still fills. Restore → resting LIMIT fills **as MAKER at its own limit price**. MARKET remainder slips exactly one tick once (`mod.rs:4790-4846`): from 1319.99 → 1320.00 in band, **from a sized ask at 1320.00 → 1320.01 out of band**; LIMIT remainders rest instead. `SandboxExecutionClientConfig` exposes no `fill_model`/`price_protection_points`. No public book accessor from `Node::call`.                                                                                                                                                                                                                                    |
| F5  | **PASS** with one defect                                                                                                   | `f1_f5.rs`              | `MarketState` health has no effect on matching. Recovery matches only against the new book. Zero-size book is not a submission gate (crossing LIMIT still accepted and rests). **Defect:** a quote with `ts_event < book.ts_last` skips the book update but still `iterate`s (`mod.rs:1584-1592`), so a HiThink→Yahoo switch (receipt stamp → `regularMarketTime`) **filled off the discarded old book**. `MarketState::accept` runs before the async `send`, so a validation reading shared state reads a newer book than the node matches against (measured).                                                                                                                                                                                                                                                                                                                                      |
| F6  | **PASS**                                                                                                                   | `f6.rs` (6 tests)       | All three §1.1 inputs live in the node cache (`positions_open`, `Position::events` `OrderFilled` with side/qty/`ts_event`, `orders(..Sell)` filtered `!is_closed()` — not `orders_open`, which misses the `INITIALIZED` order a concurrent job just placed). Store lags cache inside dispatch (cache 300 filled, `fills` count 0). Today two `submit` SELL 200 on 300 shares are both accepted; check+`place` in one `Node::call` refused exactly one in 20/20 barrier races. Partial fill and confirmed cancel adjust as specified; cancel is deferred (reservation held until `OrderCanceled`). Pending approvals reserve nothing; `decide` reruns unchecked (600 reserved on 300); the only native SELL guard is the cash-account short-sell rejection. `Equity::size_increment()` is hard-coded 1, `lot_size` advisory: SELL 150 handed to the sandbox fills; only `trade::validate` refuses it. |
| F7  | **PASS** (reference) / **BLOCKED→PASS with one extra request** (date) / **FAIL** (freshness) / **NOT RUN** (exchange page) | `F7-EVIDENCE.md`, `f7/` | Snapshot has no date field; its `timestamp` is the response clock (advanced 13 s with an unchanged price at 17:12). Date attribution: `prices/historical?adjust=none` last bar `date_ms` == today and `close_price == last_price` (16/16). Calendar: `max(item.date) == Shanghai date` proves a trading day; the response's `timestamp` separates holiday from stale list. `prev_price` == exchange ex-dividend reference 15/16 on the 16 real 2026-09-09 ex-dates, 1/16 one cent high (rounded `dividend_per_share`); == raw prior close 0/16. No per-observation freshness signal exists; only monotonic `volume` inside a proven date. Rights issues not reconstructible from the per-code endpoint. Exchange pages unreachable (400 / TLS / empty). Intraday samples not taken. Rate limit envelope `code: 429` is not in `hithink.rs`'s retryable set.                                          |

### Smallest supported mechanisms

- **Gating (session, readiness, lunch):** publish `InstrumentStatus` `Pause` (lunch, data loss) / `Close` (14:57, night) / `Trading` (open) per CN instrument on the data path. Gates matching and submission natively, per instrument, US/HK untouched. Plus a daemon check before `trade::place` for the `ORDER_INVALID` vocabulary. Quotes may keep flowing while paused (awareness stays live, no fill risk) — but see §5.3 monotonic stamp.
- **Expiry (14:57):** kernel-clock `set_time_alert_ns` → node-thread callback → `CancelOrder` per open CN order. Terminal event is **`OrderCanceled`, never `OrderExpired`**. Native DAY/GTD are not the mechanism.
- **Recovery ordering:** hold the data client's first publish per instrument → `restore` re-hands → `CancelOrder` prior-day/expired orders (decided from `ts_accepted` in the snapshot payload) → publish current status → release quotes. Never status-before-restore (`process_order` would reject the re-handed orders that must survive lunch).
- **Coherent validation and reservation:** the §1.1 triple and `place` in **one `Node::call` closure** reading only the node cache; release only on the terminal event; approval rerun uses the same closure; a refused approval needs a terminal outcome shape (`{"failure_code":"ORDER_INVALID","reason":…}`, precedent `DENIED`).
- **Band policy:** zero the opposite side at a limit (`ask_size=0` at `limit_up`, `bid_size=0` at `limit_down`) **and never publish a sized side at a boundary price** (the one-tick slip). The MARKET refusal is `ORDER_REJECTED` / `No market for …`; the price-condition field is the only agent-visible signal (`BookTop` synthesizes lot sizes regardless).

### Time-in-force, readiness, clock seam

- Public TIF today: `"GTC"`, implicit. Recommendation: keep GTC on the sandbox order and label the CN lifetime as daemon-scheduled cancellation; do not claim DAY.
- Readiness fields (proposal, from F7): `trading_date` (from the calendar), `reference_date` + `prev_price` (proven by the dated bar), `execution: OPEN | PAUSED | CLOSED | UNAVAILABLE` with reason (`NO_CALENDAR`, `DATE_UNPROVEN`, `REFERENCE_DIVERGENT`, `FEED_LOST`, `OUT_OF_SESSION`), separate from `health`/phase. Freshness rule: volume strictly increased since a read inside the proven date; **no `age_ms` bound** — there is no source time.
- Required clock seam for deterministic acceptance: the daemon needs an injectable CN session clock (today `store::now_ns()` and `feed::phase(.., now_ns())` are wall-clock with no seam) plus the sandbox client built with the shared clock (F0). Without both, restart/expiry gate scenarios stay wall-clock dependent.

### Provider evidence and limitations

`prev_price` is the ex-rights/ex-dividend reference (exchange arithmetic subtraction confirmed on 600519's 2026-06-26 ex-date), accurate to the exchange's cent; a locally derived cross-check is ±0.01. Date needs one `historical` request per instrument per day; calendar needs one request per day and must replace the `WEEKDAY` fallback for execution. Nothing is persisted that proves a trading date today. Limits: after-hours samples only; cash dividends only; no exchange page obtained; Windows not run.

### Required design revisions before an implementation slice

1. §5.1: mechanism is `InstrumentStatus` + scheduled `CancelOrder`; terminal event `OrderCanceled`; drop "native DAY"; reopen never settles a resting order.
2. §2.3: promote zero-size shaping to contract with the "no sized side at a boundary price" invariant; resting LIMITs fill at the limit price (word the policy as a condition, not a fill-price ban); name the one-tick slip in §4.
3. §5.3: hold first publish until restore; republish status on every start; key on `ts_accepted`; **monotonic `ts_event` per CN instrument across providers** (receipt stamp for both, or clamp); record the stranded `balances_locked` ceiling or verify a re-seed through `calculate_balance_locked`/`update_balance_locked`.
4. §1.1/§5.2: one node job for check+place from cache state; `!is_closed()`; refused-approval outcome shape; duplicate-approval case.
5. §2.1: readiness fields above; `MarketState` is awareness-only, never a matching gate; calendar is execution-mandatory; persist proven date + reference.
6. §3.1: odd-lot rule lives in `trade::validate` only; no catalog change.
7. §6: acceptance needs the daemon session-clock seam and the sandbox clock factory replacement from F0.
8. Root: D76/D78 amendment and SPEC §12.3 approval boundary per §5.2 above; the `balances_locked` restart defect and the stale-`ts_event` fill are not CN-specific and deserve their own corrective entries.

### Recommendation

**Revise this design** along the eight points, then proceed. No named blocker stops the feature: every restriction has a proven supported mechanism on the pinned sandbox. Two items need one more check each before the SPEC can rely on them: the `balances_locked` re-seed after restore, and an intraday HiThink sample for the dated bar and `auction/snapshot` `data_status`. Evidence and runnable spikes stay on `codex/a-share-feasibility` in `.worktrees/a-share-feasibility/` for review.

## Follow-up handoff — cash recovery and intraday provider evidence

**Status:** NOT STARTED — requested 2026-09-09; prepared for Claude, not dispatched.  
**Scope:** Run the two bounded follow-ups below on the existing feasibility branch/worktree. Do not implement the full feature or open an implementation slice.

### Review correction and intended outcome

The initial Result above is preserved as recorded evidence. Its “no named blocker” recommendation is superseded by this follow-up: restored cash release remains a correctness blocker, and F7 has not established intraday readiness or a source-delay guarantee. After-hours matching prices corroborate a date; they do not uniquely date a snapshot. Increasing volume may be delayed. A response timestamp does not certify the freshness of the underlying calendar list. Do not upgrade those inferences to proof.

The user has requested these tests/experiments. Reuse the original isolation rules and the existing `codex/a-share-feasibility` worktree. Check its current state before editing; preserve unrelated work. If documents differ between the source checkout and worktree, bring this handoff into the worktree without overwriting newer results. No real trading; provider calls are read-only, using already configured credentials without printing or copying secrets into artifacts.

### R1 — restored reservations release correctly

Extend the existing F3 real-node tests. First retain a failing reproduction of stranded cash. Evaluate only supported native account/risk restoration APIs, including the proposed `calculate_balance_locked` / `update_balance_locked` route after verifying their intended use and all relevant callers. Let NautilusTrader compute the reservation and balance; do not set guessed balances, edit fills, patch crates, or introduce an alternate accounting engine.

Required scenarios:

1. Rest an unfilled buy and capture native total/locked/free balances and order ID. Restart, cancel the restored order, and observe its native terminal event. The associated cash becomes free; cancellation does not alter total cash. Restart again and verify the state remains correct.
2. Partially fill a buy, persist, restart, and cancel its remainder. Release only the unfilled reservation. Retain the position, executed cost, and fees exactly as produced. Compare total cash immediately before and after cancellation, not against the pre-fill total.
3. Keep a second resting buy in the same account while cancelling the first. Release only the cancelled order's reservation. Repeating cancellation or restarting must not release the other order's cash or duplicate a terminal event.
4. Exercise the actual session-end cancellation/recovery ordering from F2/F3: no new quote required, and no crossing quote may fill the expired order during restoration.

PASS requires native event, balance, reservation, and repeat-restart assertions on the actual sandbox. A source signature or manual balance correction is not PASS. If no supported fix exists, mark BLOCKED and name the required upstream change; do not change the dependency pin in this task.

Keep the smallest runnable regression tests and any narrowly scoped experimental fix on the isolated branch. Record exact commands, exits, commit, platform, and evidence paths. Run focused tests and relevant existing checks; broaden only if the change warrants it.

### R2 — intraday readiness, with source delay explicitly unknown

Collect small paired samples during real confirmed exchange trading sessions, including morning opening and lunch reopening. Record the actual window sampled; if it is currently closed, prepare a bounded capture script and list the next required windows. Do not substitute after-hours samples, long blocking sleeps, or fabricated data. Do not claim the experiment completed until the session samples exist.

Use a small set of already supported CN instruments and respect rate limits. For each sample record local request start/end, HTTP status, provider envelope code, and redacted response fields. Read batched snapshot, unadjusted dated daily bars, and calendar; inspect auction `data_status` only to test its documented meaning, not to assume it is a continuous-session freshness/halt guarantee. Repeat over a short interval to show behavior; capture rate-limit HTTP status if encountered and stop rather than hammering the service.

Answer separately:

- Does today's daily bar exist at opening and lunch reopening? Does it update intraday? If snapshot and bar differ because requests race, retain the discrepancy instead of forcing equality.
- What actually attributes the reference and observation to a date? Distinguish direct provider assertions from price/volume correlation.
- Is `prev_price` documented to be the exchange-adjusted reference? Seek official documentation or an independent published reference for a sampled ex-dividend case, especially the prior one-cent discrepancy. Two HiThink endpoints are not independent exchange verification. Mark unavailable evidence honestly; do not send a support message without user authorization.
- Does any trustworthy source-observation timestamp or documented maximum delay exist? If not, record that there is no source-delay guarantee. One intraday sample or increasing volume cannot create one.
- Can calendar readiness be re-established conservatively after startup? Consider starting unavailable and revalidating before proposing persisted reference state. Do not add storage merely to make a read look proven.

The proposed product ceiling to bring back for review is: confirmed trading day and adequately supported current-day reference; execution pauses on feed failure or missing reference; receipt age is visible; source delay remains unknown. Volume changes are supporting evidence, not a freshness certificate. This is a weaker snapshot-simulation promise than a maximum-delay guarantee. Present the precise remaining assumptions for user acceptance; do not silently declare the stronger existing SPEC satisfied.

### Follow-up deliverable

Append a new dated result here and update F7-EVIDENCE with the additional samples. Preserve the initial report and distinguish corrections from new observations. Include:

- R1 and R2 status independently: PASS / FAIL / BLOCKED / NOT RUN;
- reproducible branch/commit, test commands, exit codes, native events/balances, and redacted sample paths;
- sampled market windows and missing windows;
- confirmed provider facts versus assumptions and unknown delay;
- smallest proposed design corrections, including conservative limit policy stated against the observed market condition rather than an absolute ban on boundary-priced fills;
- a recommendation on whether the blockers are closed under the proposed weaker data contract.

No automatic product implementation, root decision changes, merge, or declaration of design completion. Return the evidence and explicit data-quality tradeoff for review.

### Follow-up result

NOT STARTED. This section is a handoff, not execution evidence.
