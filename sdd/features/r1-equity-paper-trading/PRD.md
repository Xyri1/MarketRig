# R1 — Equity paper trading and the agent's market surface: Feature PRD

**Milestone:** [R1](../../ROADMAP.md#milestone-r1--equity-paper-trading-and-the-agents-market-surface)
**Status:** Design complete — PRD, DECISIONS, and SPEC accepted 2026-09-01

This feature designs Milestone R1: the first authoritative trading fact and both halves of the agent's market surface. It refines `sdd/SPEC.md` §3, §12, §13, §15, and §17 and invents nothing beyond them.

## 1. Motivation

*Decision basis: per D4, D38, D76.*

The trading plane is the product's reason to exist and the one capability the 2026-09-01 spikes proved outright. Every later milestone consumes what R1 founds: triggers act through its order path, delivery carries its evaluation prompts, memory judges its realized P&L, and the desktop renders its history. Running both real runtimes against the MCP surface now settles the surface-split risk (per D4) before four milestones depend on it — and the feed risk D76 accepts is cheapest to confront while the harness is small.

## 2. Outcome

A desk that buys and sells real equities on paper across three markets: live Yahoo quotes feed a per-desk NautilusTrader sandbox book on a multi-currency account; every sandbox-produced fact — orders, fills, position cycles, fees, net-of-fees realized P&L — lands verbatim in durable history and survives a daemon restart together with the book itself; each closed cycle durably queues one evaluation prompt; and a real Codex session and a real Claude Code session each read a fresh quote through the desk's MCP resources and submit and cancel a paper order through its typed tools.

## 3. Scope

R1 delivers seven things, each a thin vertical of a contract `sdd/SPEC.md` already states:

1. **The equity feed** (per D39, D76): one out-of-tree Yahoo `DataClient` in MarketRig's own crates, polled, keyless, covering US, Hong Kong, and China A-share, with the market-hours calendars, staleness exposure, synthesized zero-spread book, and 429 retry policy this feature's SPEC settles — verified against the live endpoint at spec time, not assumed.
2. **The instrument catalog** (per D76): a curated set of tradable equities per market carrying the metadata the sandbox needs to be correct — currency, tick, lot — because the feed itself declares none of it.
3. **Per-desk paper trading** (per D38, D64, D76): one lazy `LiveNode` per trading desk hosting the sandbox execution client and one multi-currency cash account (USD, HKD, CNY); market and limit orders, cancel, fees charged from each instrument's declared per-market rate; realized P&L in the instrument's own currency.
4. **Durable trading history and restoration** (per D38, D64): sandbox event payloads stored verbatim beside normalized query fields; the position cycle as the realized-P&L unit; book restoration across a daemon restart from serialization snapshots, never from history replay.
5. **The realized-P&L signal** (per D22, D38): a closed cycle and its queued evaluation prompt commit in one transaction; delivery arrives with the runtimes in R3, so R1's prompts are born and stay `QUEUED`.
6. **The MCP market plane** (per D4, D63): the `marketrig-mcp` crate joins the workspace — five concretely enumerated desk-scoped resources (quotes, book, positions, open orders, instruments) and two typed tools (submit, cancel), every argument validated by the daemon, no subscriptions, no completion.
7. **The CLI's trading read surface** (per D4): `marketrig history` over durable records — orders, individual fills, closed position cycles — exactly as the sandbox produced them. Live state stays on the MCP plane.

The acceptance chain grows accordingly: gate scenarios on a deterministic stand-in quote source, and the experiment target's first attended scenarios on real Yahoo and the real runtime CLIs (per D75, D76).

## 4. Non-goals

Everything later milestones own, plus the physics D76 states rather than approximates:

- no T+1 settlement, daily price limits, trading halts, opening or closing auctions, or holiday calendars — gaps are stated to the agent, not simulated;
- no short selling, margin, leverage, or FX conversion between the account's currencies;
- no order types beyond market and limit GTC, no order lists, no execution algorithms;
- no real bid/ask — the synthesized zero-spread book is a recorded reward-signal ceiling (per D76);
- no Kraken crypto (R6, per D74); no triggers, scheduler, or prompt delivery (R2/R3); no approval surfaces — paper orders are fixed at **Always allow** (per D70); no desktop, no OpenAPI emission wiring (R5);
- no quote persistence: observations die with the daemon (root §15);
- no pagination, attribution headers, or idempotency mechanics beyond the order path's own (root §18).

## 5. Success criteria

1. The roadmap's evidence line passes: a desk buys and sells one equity on a real market quote → the sandbox reports the fill, the close, and net-of-fees realized P&L → MarketRig stores every fact unmodified and one evaluation is queued → a second round trip in a non-USD market closes with realized P&L in its own currency → `marketrigd` restarts and the book and the history are still there.
2. A real Codex session and a real Claude Code session each read the desk's quote resource twice, see a fresh value, and submit and cancel a paper order through the typed tools (attended experiment).
3. Restoration is proven, not assumed: a resting limit order survives a restart under its original client order identifier — the check D64 names as open verification.
4. Every mutating trading command is idempotent and attributed: replaying an action identity returns the original record and creates nothing.
5. Observation provenance is honest: reads expose source time, receipt time, age, market phase, and the synthesized-book flag; a stopped feed degrades explicitly and no read silently substitutes another provider.
6. The gate runs unattended and deterministic on the stand-in quote source; static checks and `cargo test` stay green on both MVP platforms in CI.

R1 is done when this evidence exists — produced by the checks this feature's SPEC names — not when the deliverable list is exhausted.
