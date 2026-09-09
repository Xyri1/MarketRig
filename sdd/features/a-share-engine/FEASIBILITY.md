# A-share engine feasibility check — handoff for Claude

**Status:** NOT STARTED — logged 2026-09-09  
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

NOT STARTED. No commands, runtime checks, provider verification, or feasibility outcomes have been produced for this handoff.
