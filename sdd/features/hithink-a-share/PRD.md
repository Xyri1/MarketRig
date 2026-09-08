# HiThink A-share data — Feature PRD

**Slice:** [013 — HiThink A-share data](../../slices/013-r6-hithink-a-share.md)
**Status:** Active — PRD, DECISIONS, and SPEC written 2026-09-08; implementation under slice 013, opened the same day

This feature makes the official 同花顺 (HiThink) financial data service the daemon's source for everything China A-share: the quotes the A-share leg of the paper book trades on, the exchange's trading-day calendar, and the research reads an agent asks for through `marketrig`. It changes `sdd/SPEC.md` §4.4, §12.1, §12.2, §13.2, §16, and §17, refines D9 and D76, and is the whole of Milestone R6 (per D84). Yahoo stays the feed for the US and Hong Kong markets; HiThink carries no data for either (verified 2026-09-08: its exchange set is `SH`, `SZ`, `BJ`, and every capability map lists 港股 and 美股 as out of scope).

## 1. Motivation

*Decision basis: per D4, D9, D38, D49, D75, D76, D78, D83, D84.*

R1 chose Yahoo's undocumented chart endpoint because it was the only keyless source covering all three equity markets, and D76 recorded the cost: an unsupported endpoint that rate-limits unpredictably, no holiday calendar, and a desk whose research is whatever the agent can scrape with its own tools. For A-share the trade no longer holds. HiThink publishes an official, keyed REST contract for the whole A-share market — snapshot quotes, daily bars with adjustment, statements, valuations, the trading calendar, index and sector constituents, the venue's own limit-up and dragon-tiger lists — with a machine-readable envelope, documented error codes, and an agent skill whose reference tree mirrors that contract. An A-share desk whose evidence is supposed to show an agent learning from outcomes should reason over the data a real A-share trader reads, from the source the venue's own data vendor maintains, rather than over a chart endpoint's last price alone. D9 left a slot for exactly this — informational data, never a writer of trading state, reached only through `marketrig` — and reserved it for OpenBB; HiThink fills the slot for A-share without a supervised interpreter child, because it is a hosted HTTPS service the daemon calls with the HTTP client it already has.

Crypto is deferred past MVP in the same decision (D84): a second venue on the same trading topology buys no loop evidence the equity book has not already produced, while an A-share desk with honest research and a real calendar does change what a session can learn.

## 2. Outcome

An operator pastes one HiThink API key into Settings and the "Use HiThink for A-share" tick box lights up, ticked. From then on every A-share catalog instrument polls HiThink's snapshot in one batched request per cycle under the same cadence, the same synthesized book, and the same provenance fields the Yahoo leg exposes, and its market phase respects the exchange's real holiday calendar. The agent reads any of HiThink's endpoints as `marketrig research hithink <path> [--param k=v]…`, receives the upstream envelope untouched, and finds a large result written to a file rather than flooding its context. A new desk is seeded with the vendored `hithink-finance` skill, its routing rewritten so the only path it names is `marketrig`, so an agent that has never seen HiThink knows what the endpoints are and how to disambiguate a stock name into a `thscode`. The key lives only in the credential store; no runtime, workspace file, or prompt ever carries it. The A-share leg is a tick box in Settings, greyed out until a key is saved; without one the leg stays on Yahoo exactly as R1 ships it, and the research command refuses with one code. The US and HK legs, triggers, sessions, and memory are untouched either way.

## 3. Scope

1. **Provider setup** (per D42, D49): one installation resource holding the key reference, its validation state, and the A-share feed toggle (`YAHOO | HITHINK`, off and disabled without a key), set through REST and Settings, validated by one minimal live request, never returned.
2. **The A-share feed** (per D76, D78): the `CN` catalog entries move from the Yahoo chart client to HiThink's snapshot endpoint, batched, with `provider: "hithink"`, the retry bound HiThink documents, and honest handling of a batch reply that carries no source timestamp.
3. **The A-share calendar** (per D78's `ponytail`): the trading-days endpoint fetched once per Shanghai day gates polling and labels phase for `CN`; the weekday rule stays the fallback and the observation says which one applied.
4. **The research passthrough** (per D4, D9): one daemon route and one CLI command proxying HiThink's endpoint catalog with the key attached, an allowlist, bounded retry, rate spacing, and file spill for large bodies. Informational only; it writes no trading state and records no trading fact.
5. **The seeded skill** (per D20, D83): the upstream `hithink-finance` skill vendored at a pinned commit and rewritten by a committed script into a MarketRig-routed form, seeded into every new desk's OpenViking user beside `desk-improvement`, projected read-only.
6. **Acceptance** (per D75): a loopback HiThink stand-in in the gate, scenarios H1–H4 after O10, and one attended scenario E7 per cell on the real service.

## 4. Non-goals

- No Hong Kong or US data from HiThink; Yahoo keeps both legs unchanged.
- No bundling or execution of HiThink's Node CLI, Python SDK, `marketdb`, DuckDB, or the parquet market-dump importer; no local mirror of history.
- No typed research commands: the daemon types the two responses it acts on (snapshot, trading days) and the envelope, nothing else.
- No hosted-MCP registration in the desk: the four `fuyao.aicubes.cn/mcp/*` servers are not seeded, because the key would have to reach the runtime (per D49) and research would appear on two planes (per D4).
- No minute bars, tick, auction-driven fills, price limits, halts, or T+1 — the sandbox physics stay as D76 states them.
- No key on any runtime process environment, workspace file, prompt, log, event, or CLI output; no second copy in HiThink's own keychain entry.
- No research reads recorded as trading actions or events; a read leaves no durable row.
- No rewriting of the upstream skill's data contract prose; the script edits routing, credentials, installation, and update sections only.
- No Kraken crypto: deferred past MVP (per D84).

## 5. Success criteria

- A desk trades a `CN` catalog instrument on HiThink quotes: the observation names `provider: "hithink"`, a switch of the toggle changes the next observation's provider without a node restart, one poll cycle issues one snapshot request for every `CN` instrument together, and a scripted non-trading weekday reads `market_phase: "CLOSED"` with the calendar source named.
- `marketrig research hithink <path>` returns the upstream envelope verbatim for an allowlisted path, refuses an unknown path and an unconfigured provider with documented codes, retries `4001` within its bound, and spills a large body to a file naming the path.
- The key is absent from SQLite, the log root, every event, every launch file, every workspace file, and every CLI output in the gate bundle.
- A desk created after this feature lists `hithink-finance` in its projection and the skill names no path but `marketrig`.
- One attended cell on the real service: a session reads an A-share instrument's financials through `marketrig`, trades it on HiThink quotes, and the cycle closes with its evaluation queued.
