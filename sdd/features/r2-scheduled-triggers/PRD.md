# R2 — Scheduled triggers: Feature PRD

**Milestone:** [R2](../../ROADMAP.md#milestone-r2--scheduled-triggers)
**Status:** Design complete — PRD, DECISIONS, and SPEC accepted 2026-09-02

This feature designs Milestone R2: the half of the loop R1 cannot produce — work a desk defines now that happens later, with or without an agent alive. It refines `sdd/SPEC.md` §8, §9, §10, §11.1, §12.3, §13.2, §15, and §17 and invents nothing beyond them.

## 1. Motivation

*Decision basis: per D34, D35, D37, D40, D41.*

R1 proved the market plane: a session observes and acts, and every sandbox fact lands in durable history. But a session is a disposable conversation, and the product's claim is persistence — a trader identity that keeps working while no conversation exists. The scheduler is the first mechanism that acts on a desk's behalf from durable state alone, and every later milestone leans on it: R3 delivers what R2 persists, R4 evaluates what R2's code traded, R5 approves what R2 stores. It is also where MarketRig's containment primitive is born, which R3's app-server and R4's memory child reuse (root §9, per D73).

## 2. Outcome

A desk defines a scheduled trigger — a brief for its future self, a one-off instant or a recurrence rule in a named time zone, and optionally an immutable code snapshot — through the `marketrig` CLI. `marketrigd` alone computes the next occurrence, accepts it atomically as a firing while no agent process exists, runs the approved code without a shell under native process-group containment in the desk workspace, and persists the raw result and one queued result prompt in a single transaction. An order the code places through the desk's MCP adapter is attributed to the firing in `trading_actions`. A schedule that elapses while the daemon is down becomes one miss record and never a firing; a daemon killed mid-execution leaves the firing and a terminal `DAEMON_LOST` result behind.

## 3. Scope

R2 delivers seven things, each a thin vertical of a contract `sdd/SPEC.md` already states:

1. **Trigger definitions** (per D34): desk-bound `SCHEDULED` triggers, `ONE_OFF` or `RECURRING`, with a unique per-desk kebab name, an agent-authored brief and optional context, enablement, a revision counter, and soft deletion that preserves history.
2. **The scheduler** (per D37, D40): one daemon-owned coordinator task over SQLite `next_occurrence_ns` projections — RFC 5545 recurrence in the desk's named IANA zone with explicit DST behavior, a 60-second recheck, an in-memory wake after every mutation, compare-and-accept firing transactions, the 60-second lateness tolerance, and miss evidence as one reconciliation record per missed range.
3. **Firings as immutable provenance** (per D34): occurrence identity, firing-time brief and context, trigger revision, and code-snapshot identity captured in the acceptance transaction, which also consumes a one-off or advances a recurring definition.
4. **Code snapshots and execution** (per D35, D41): immutable UTF-8 source with suffix, direct argument vector, and timeout under one SHA-256 fingerprint; executed at most once per firing as the current user in the desk workspace, with the firing document on standard input, concurrent capped stream capture, and one containment primitive — a POSIX session on macOS, a kill-on-close Job Object on Windows — behind which every managed child MarketRig ever owns will run.
5. **Results and the queued prompt**: every terminal outcome — normal exit, nonzero exit, timeout, output limit, spawn failure, Quit, daemon loss — persists exactly one execution record and one `TRIGGER_RESULT` prompt, born `QUEUED`; delivery arrives with R3.
6. **Attribution**: the trigger environment reaches the market plane through the shared daemon client, so an order placed by trigger code through `marketrig-mcp` is recorded with `source: TRIGGER` and its firing identity.
7. **The CLI's `trigger` and `prompt` groups** (per D4): definitions, firings, and results as durable records; queue status as §11.1 states it.

The acceptance chain grows accordingly: gate scenarios on the stand-in feed with a harness-owned trigger executable, and one attended scenario where a real session defines a trigger whose code trades.

## 4. Non-goals

Everything later milestones own, plus what the root contract defers:

- no delivery — prompts are born and stay `QUEUED` until R3; no activation, no runtime adapters;
- no `EVENT` triggers, connectors, or ingress (R6); the `source` vocabulary is widened by migration then;
- no approval workflow — trigger-code approval is fixed at **Always allow** and every snapshot is approved at creation; the policy resource and its surfaces arrive in R5 (per D70);
- no artifact files: the raw result is the captured standard output, bounded, stored beside its diagnostics; artifact storage beyond that stays deferred (root §18);
- no automatic retry, circuit breaker, expiry, catch-up firing, or manual cancellation of a queued result (root §8.3, §10);
- no change to the seeded `AGENTS.md`: its trigger wording arrives with the constitution milestone (root §18);
- no sub-minute recurrence, `COUNT`, `UNTIL`, multiple rules, or RDATE/EXDATE sets (per D40).

## 5. Success criteria

1. The roadmap's evidence line passes: a scheduled trigger with approved code fires while no agent process is alive → the code places an attributable, idempotent paper action → the raw result is persisted before anything is delivered → a schedule missed across a daemon downtime becomes miss evidence, not a firing → a restart mid-flight loses neither the firing nor the result.
2. Every terminal outcome yields exactly one execution record and one queued result prompt in one transaction, and none reruns the code.
3. A duplicate wake, a duplicate acceptance, or a restart cannot fire the same occurrence twice: the firing table's uniqueness is the guard.
4. Recurrence is honest about time: a candidate that does not exist in its zone is skipped, an ambiguous one resolves to its first occurrence, and the projection is recomputed only from the definition's own anchor.
5. Containment is proven on both platforms: a timed-out script's descendants are gone when the outcome is recorded.
6. A real Codex or Claude Code session defines a trigger whose code trades through the MCP adapter, and the resulting action carries the firing's identity (attended experiment).
7. The gate runs unattended and deterministic; static checks and `cargo test` stay green on both MVP platforms in CI.

R2 is done when this evidence exists — produced by the checks this feature's SPEC names — not when the deliverable list is exhausted.
