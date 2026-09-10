# Slice 014 — A-share paper-trading engine

**Status:** Implemented on macOS (2026-09-10) on branch `codex/a-share-engine`; steps 1–5 landed, the extended gate G1–A6 passes on macOS. Not frozen: the Windows checks and the attended E7 cells (macOS/Windows × Codex/Claude, with the later-session close) remain outstanding, so root SDD is not merged yet.

The implementation plan for [`features/a-share-engine/`](../features/a-share-engine/PRD.md), decisions AE-1–AE-9 and SPEC §1–§6. The feature folder is canonical. This extends the R6 CN data plane with enforceable paper-trading restrictions; slice 013's delivery status is unchanged. Root D76/D78/D84 and SPEC §12 describe delivered behavior until this slice's exit checks pass. The feature decisions explicitly record their intended amendments.

## Outcome

On the supported CN catalog, enforce T+1, quantity rules, confirmed trading-day sessions, and clock-driven cancellation at 14:57. HiThink LIMITs normally wait for a later compatible, volume-increasing observation and fill fully at their limit. A MARKET publication may fill compatible resting LIMITs first regardless of volume, then the MARKET fills fully at last. The agent sees this exception, inferred reference/date attribution, unknown source delay, synthetic liquidity, and actual execution availability. NautilusTrader alone owns fills, fees, balances and history.

Yahoo CN remains explicitly simplified, with the common eligibility/session/lifetime rules but without the HiThink band or sampled-fill policy. US/HK keep their current behavior.

## Starting evidence and boundaries

- Review branch `codex/a-share-feasibility` through `61cb178`. F0–F8, R1 and R2 are recorded in [FEASIBILITY](../features/a-share-engine/FEASIBILITY.md). F8's 35 tests were independently rerun on macOS. Windows and production integration are not established.
- Start an implementation worktree under `.worktrees/` with a `codex/` branch; deliberately port the small supported changes and useful regressions. Do not merge the entire experimental harness into production or overwrite unrelated slice work. Read current AGENTS.md and root SDD first.
- The existing slice-013 HiThink provider, feed and stand-in are dependencies. The partial-fill restart/history defect belongs to the same capture/restore area as slice 012; this slice owns its regression and correction unless already fixed there. Reuse and verify any landed correction rather than duplicating it. Do not alter frozen slices.
- Keep `nautilus-* =0.62.0`. No fork, private mutation, second matcher, fabricated events, daemon fee/P&L calculation, or post-fill correction. No provider load test or further ordinary live sampling is needed for design feasibility.
- Every spawned MarketRig binary requires a scratch `MARKETRIG_TEST_DATA_ROOT`. Use the existing quote/HiThink stand-in seams; never touch real desks, credentials or smoke-wipe paths. No real-market trading is in scope.

## Implementation sequence

Each step leaves its focused checks green before proceeding. Keep feature docs current when implementation reveals drift. Names below identify required assertions, not instructions to retain every exploratory spike test.

### 1. Native recovery and shared execution boundary

Touch `crates/marketrigd/src/node.rs`, `trade.rs`, and the smallest necessary internal helper.

- Port the public `Portfolio::initialize_orders` restore call and needed portfolio handle from R1; keep dependency pins unchanged. Verify BUY and SELL reservations, cancellation, multiple orders, and repeated restart from native payloads.
- Correct partial-fill restoration so native order history remains replayable without duplicate acceptance or missing fills. Do not fabricate a chain or silently discard conflicting native events. Convert the defect-reproduction assertion to the required correct outcome.
- Add the per-node publication/admission critical section across submit and approval execution. Establish crossing-publish confirmation and idle-restore confirmation before admitting another order. Avoid a mutex that blocks the runner needed to complete publication. Failure, timeout, cancellation and node shutdown must leave no executable crossing book exposed.
- Reuse F0's controlled native clock and provide the same controllable time source for CN session, owning-day and restart decisions. Time passing must drive native handlers and daemon decisions together. Do not change US/HK policy.

Checks: restored reservation release and history continuity; delayed data publication versus submission; concurrent approval/submit; failure during crossing/idle restoration; session timer progress while a publish is outstanding. Exercise the real node and queued runner path, not only helpers.

### 2. Readiness and provider failures

Touch `hithink.rs`, `feed.rs`, and the existing per-node data client.

- Add the smallest dated in-memory calendar/bar evidence and readiness state required by SPEC §2.1. Successful same-day evidence survives a refused redundant refresh; rollover invalidates it; contradictory successful evidence blocks. Weekday fallback remains awareness only and cannot authorize CN execution.
- Fetch unadjusted current-day bars only as needed to establish the per-instrument evidence; reuse the existing provider client. No new research command or duplicate credential path. Missing data pauses affected execution, while a successful unchanged snapshot is not itself a failure.
- Recognize envelope `429` alongside `4001` and HTTP 429 in the existing bounded retry path. No automatic order replay. Retain the 10/30-second batched per-node cadence and disclose that it is not a provider quota.
- Install each desk's coherent accepted observation/reference on its own execution boundary. Use monotonic execution-event timestamps across provider changes; keep source time unknown.

Checks: envelope-only/HTTP rate limits, exhaustion/recovery, first-day unavailable state, retained calendar after refusal, rollover and contradictory evidence, missing/invalid bar or reference, reference change, missing instrument, two desks with different observation progress, provider switching without old-book fills. Use stand-ins and controlled time rather than real sleeps where supported.

### 3. Eligibility, session and sampled execution

Touch catalog metadata, `trade.rs`, the node data client, and the new execution state only where needed.

- Read T+1 BUY fills, positions and all nonterminal SELL leaves from the native cache; validate and place in one serialized action. Pending approvals reserve nothing; execution-time approval reruns dynamic checks. Enforce AE-4 odd-remainder rules and board/type caps with exact arithmetic.
- Apply SPEC's band derivation and direction suppression. Outside supported sessions, reject new orders and block resting fills. At lunch pause; at 14:57 issue native CancelOrder without a quote. Expose GTC/OrderCanceled honestly.
- Implement the idle-book / temporary crossing publication mechanism from F8. Ordinary feed triggers require volume growth and compatible price. Native matching sets each LIMIT's price and quantity. Reset temporal baselines on restart, feed recovery, volume decrease and provider change; never persist them as source-time evidence.
- Apply the accepted MARKET exception on both unsuppressed sides. Size publications for full eligible remainders and the MARKET to avoid native one-tick remainder slippage. Confirm restoration to idle after the action. A native MARKET denial may follow resting-order fills; preserve each authoritative outcome and never roll back or retry them.
- Recover in order: hold first publish → restore → rebuild native reservations → cancel expired orders → establish status/readiness and baseline → release. Cancellation stays available while data/session execution is blocked.

Checks: feature A1–A6, including main board and ChiNext, both sides at both band boundaries, two-sided resting orders, multiple limits at different prices, MARKET after reset, MARKET denial after compatible resting orders, no own-admission LIMIT fill, missed/equal-volume crossings, cutoff interleavings, prior-day cancellation, and approval-time baseline. Repeated/concurrent interleavings must test the lock, not simply serialize calls manually as the spike did.

### 4. Agent surfaces and seed

Update the existing quote/book/position/instrument resources and new-desk seed; regenerate OpenAPI/client only where affected.

- Surface CN sellable/locked/reserved share quantities only on current positions; leave historical records historical.
- Expose availability/reason separately from quote health and phase, inferred band date, receipt age, unknown source delay, and active fill policy, including the MARKET exception and Yahoo simplification. Do not display transient synthetic execution depth as real market liquidity.
- Native SELL reservation slots can contain share counts despite their currency label. Do not present those values as reserved cash or derive cash availability from them; preserve native payloads and use the named share projections.
- Keep English agent output byte-identical under UI locales. Seed new desks only; never rewrite existing agent-owned AGENTS.md. Do not add a CLI awareness command or leak native APIs.

Checks: resource/serialization and affected frontend tests, old/new seed ownership, exact policy disclosure, US/HK unchanged, history still replayable, and idempotent action results following multi-order native outcomes.

### 5. Gate and attended acceptance

Integrate A1–A6 after H1–H4 using the existing acceptance stand-in and real binaries. Update affected CN gate assumptions to controlled exchange sessions and next-day sell eligibility; do not retain immediate same-day CN round trips or depend on wall-clock market hours. Keep unrelated scenarios intact.

Update `crates/marketrig-acceptance/EXPERIMENT.md` and E7 for a real-provider buy, same-day T+1 refusal, and later supported-session sell producing the native cycle and queued evaluation. The same-day sitting is partial; full completion requires the later close. Preserve unknown-delay and corporate-action limitations in the evidence. Run attended cells only with the operator and prerequisites present; do not claim success from a skipped or inconclusive cell.

## Exit checks

- Feature SPEC §6 A1–A6 and surface/seed checks pass in production integration on macOS and Windows. The new defect regressions assert correct behavior, not the existence of the old bugs.
- `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `pnpm check`. Run focused checks while implementing and the full required suite at integration. If generated contracts changed, regenerate with `pnpm generate` and verify their committed consistency.
- The extended deterministic gate passes on both platforms with normal evidence bundles, isolated data roots and no credential leakage. There are no source-time claims or real-market prerequisites in the gate.
- Revised attended E7 closes on all four platform/runtime cells (macOS/Windows × Codex/Claude), with later-session close and one queued evaluation. Record bundle paths and actual status here; no auto-created schedules or unattended substitutes.
- Every integration hazard above has an explicit result, especially publication/admission races, deadline interruption, MARKET side effects, partial-fill history and reservation restoration.

After these checks, freeze this slice, merge durable amendments into root SPEC §12, §13, §16 and §17 and the appropriate decisions, then refresh ROADMAP and AGENTS.md. Update Commands only when a command actually changes. Do not promote experimental success or this plan into delivered root behavior.

## Verification record

Planning (2026-09-10, morning): feature docs and local links checked; prior F8 rerun 35/35 macOS, recorded in FEASIBILITY.

Implementation (2026-09-10, macOS 26.3 arm64, rustc 1.98.0, branch `codex/a-share-engine` in `.worktrees/a-share-engine/`), steps 1–5 in order, each committed after its focused checks:

- Step 1 (`da6b9b8`, with step 2): controlled clock seam in production (`MARKETRIG_TEST_CLOCK_NS` under the data-root seam, `PUT /test/clock`), first publish held until restore and CN reconciliation, prior-day/expired CN orders canceled before any quote, the publish/confirm/idle critical section with admission waiting on it (the F8 hazard now asserted as the correct outcome, negative-controlled), partial-fill restore stores one acceptance so history replays. R1's `initialize_orders` had already landed with the spike merge.
- Step 2 (`da6b9b8`): envelope `429` retryable, backoff a field (stand-in ~1 ms), dated calendar retention with `CalendarRefresh` outcomes and `trading_day`, per-instrument current-day bar evidence, `Observed` per snapshot item, `catalog::Board` with caps and the §2.2 band arithmetic, awareness band fields.
- Step 3 (`8596891`): sellability from the node cache, execution-time checks inside the admission closure, the AE-9 observation → publication rule with per-order baselines and resets, two-phase MARKET sizing with idle restore on every path, the self-rearming session boundary alert (11:30 Pause, 13:00 re-gate, 14:57 Close + CancelOrder without a quote). 25 feasibility assertions flipped to the enforced behavior.
- Step 4 (`7a69cde`): CN positions carry sellable/locked/reserved; CN quotes and books carry this desk's `execution` object; instruments carry board, band percent and caps; Yahoo CN requires the confirmed calendar (AE-7); the seed's CN section rewritten from feature SPEC §4.1 with the byte-for-byte test re-pointed. `pnpm generate` produced no diff (the market-plane routes are untyped `Value` bodies).
- Step 5: `Harness::standin_clock`/`advance_clock`, stand-in scripting for `prev_price`, cumulative volume, bar date and a hidden instrument; H2's trading half moved to the controlled clock with the same-day refusal and the next-day sell; G16 and O9's sandbox refusals moved to AAPL; A1–A6 appended after H4; E7 procedure and driver revised to a partial first sitting and a later-session close.
- Review fixes (after the two-axis code review): the poller skips an instrument whose book a MARKET admission holds (`cn::cycle_observation`), so the critical section binds both directions; every boundary alert sweeps and cancels every non-closed CN order that `expired` names, so a clock step or a suspended daemon that skips 14:57 still terminates the day; the admission pre-check reads dated readiness inside a session, so an approval after rollover records no decision; dead `Reason` variants removed, one band derivation on `Entry::band`, one `shanghai_date`, `Board::share_cap`. A `#[cfg(test)]` `CnExec::sweep` knob (production always sweeps) keeps 13 frozen feasibility day-jump checks green. Gate rerun passed (`target/acceptance/gate-1789053227`, 943 s); `cargo test -p marketrigd --lib` 278 passed.
- Post-step fix: readiness carries the Shanghai date it was established on and reads `NO_CALENDAR` on a later date until a poll re-establishes it (`cn::Inst::ready_at`), closing the one-poll window in which a boundary alert could reopen an instrument on yesterday's evidence.

Checks (all exit 0 unless noted): `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test -p marketrigd --lib` 275 passed; `cargo test --workspace --exclude marketrig-acceptance` green (step 5 run); `pnpm check` green; the gate G1–G32, O1–O10, H1–H4, A1–A6 passed on macOS in step 5 (bundle `target/acceptance/gate-1789047661`, 374 observations, 935 s) — the post-fix rerun passed likewise (bundle `target/acceptance/gate-1789050355`, 979 s). E7 was not run.

Outstanding before freeze: the module checks and the extended gate on Windows; the attended E7 cells on all four platform/runtime pairs with the later-session close and one queued evaluation; root merge of the AE amendments into SPEC §12, §13, §17 and DECISIONS, and the ROADMAP/AGENTS.md refresh, which wait for those checks.
