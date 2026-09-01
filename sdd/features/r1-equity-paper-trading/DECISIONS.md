# R1 — Equity paper trading and the agent's market surface: Feature Decisions

Local decisions for [Milestone R1](../../ROADMAP.md#milestone-r1--equity-paper-trading-and-the-agents-market-surface), prefixed `R1-<n>`. They resolve mechanics the root SPEC defers (§18) for R1's scope and contradict nothing settled; on merge they are summarized as one product `D<n>` in `sdd/DECISIONS.md`.

### R1-1 — The feed is one polled Yahoo chart client whose delay is evidenced, never declared

**Decision:** The equity `DataClient` polls Yahoo's chart endpoint per catalog instrument: once at subscription regardless of market phase, then — only while that instrument's calendar phase is `OPEN` — every **30 seconds**, tightened to every **10 seconds** while the desk has an open order or a nonflat position in it, because freshness only matters where the book has exposure. HTTP 429 is retried up to 8 attempts 400 ms apart (the counts the data spike verified, per D76); exhaustion leaves the last accepted observation standing with degraded health and is never a silent provider substitution. The endpoint layer is MarketRig's own thin chart client — URL construction and response parsing on the workspace's reqwest line — because D76's candidate, the community `yahoo_finance_api` crate, was replaced at plan time: its 5.0.0 release compiles the chart URL in privately and exposes no override, which the acceptance seam (R1-9) requires (crate source verified 2026-09-01; slice 002). The base URL is compiled in and overridable only through that seam, never by configuration.

Verified against the live endpoint on 2026-09-01: the chart metadata's `exchangeDataDelayedBy` field is **null on all three exchanges**, so the feed declares no delay figure. The contract therefore promises no delay constant: an observation's freshness is evidenced solely by its source timestamp against MarketRig receipt time, both exposed on every read (root §12.2). The same verification confirmed per-market currencies (USD/HKD/CNY), IANA zones, and session envelopes (US 09:30–16:00 America/New_York; Hong Kong envelope 09:30–16:10 Asia/Hong_Kong; Shanghai/Shenzhen envelope 09:30–15:00 Asia/Shanghai).

**Rationale:** A flat 10-second sweep of the whole catalog is 1.5 requests per second sustained — exactly the shape that provokes the endpoint's probabilistic 429s — while the two-tier cadence keeps the steady state near 0.5 and still gives an exposed instrument the freshness a fill depends on. Polling only during `OPEN` is the cheapest 429 hygiene there is.

`ponytail:` each desk's node polls independently, so two desks duplicate the same public reads; the upgrade path is one installation-wide fetch layer behind the per-node clients (root §12.2 already permits installation-wide provider connections) when desk count makes the duplication matter. Promising a delay we cannot observe would be an assumption dressed as a contract — D76 ordered the delay characteristics verified, and what verification found is that only timestamps are honest.

**Contract:** root [SPEC §12.2](../../SPEC.md#122-shared-reads-and-isolated-books); this feature's [SPEC](SPEC.md) §2.

### R1-2 — Tradable instruments are a curated catalog shipped as product data

**Decision:** MarketRig ships a small curated equity catalog — the starter set is pinned in this feature's SPEC §3 — with each entry carrying the NautilusTrader instrument identifier, the Yahoo symbol, the market key, the currency, the price increment, and the lot size. The catalog is the whole tradable universe: an order or quote read for anything else answers `INSTRUMENT_UNKNOWN`. Hong Kong entries fix one price increment chosen from the exchange's price-band ladder at the stock's prevailing band, and every entry's tick and lot are verified against the venue when the implementing slice lands the data.

`ponytail:` a static catalog with per-entry fixed ticks is a deliberate ceiling — the Hong Kong ladder is price-banded and board lots change by corporate action; the upgrade path is a maintained catalog source or per-band tick logic behind the same catalog seam when a desk needs breadth.

**Rationale:** Yahoo's chart endpoint declares currency but neither tick nor lot, and the sandbox silently drops a quote whose precision disagrees with its instrument (per D76) — so instrument metadata must be authored somewhere, and a reviewed static table is the smallest correct somewhere. A catalog also bounds the polling surface, which is 429 hygiene again.

**Contract:** root [SPEC §12.2](../../SPEC.md#122-shared-reads-and-isolated-books); this feature's [SPEC](SPEC.md) §3.

### R1-3 — Market calendars are static weekly sessions with no holiday calendar

**Decision:** Each market key carries a fixed weekly session table in its IANA zone: US 09:30–16:00 America/New_York; Hong Kong 09:30–12:00 and 13:00–16:00 Asia/Hong_Kong; China A-share 09:30–11:30 and 13:00–15:00 Asia/Shanghai; Monday through Friday. The calendar yields exactly one derived fact, the market phase `OPEN | CLOSED`, which gates polling (R1-1) and labels observations — it never gates an order, because the sandbox matches against whatever book it has and the agent decides with the provenance in front of it.

`ponytail:` no holiday calendar — on an exchange holiday the phase reads `OPEN`, polling proceeds, and the observation simply stops advancing, which age and source time expose; the upgrade path is a per-market holiday table behind the same calendar seam if evaluation evidence shows the mislabel misleads the agent.

**Rationale:** The lunch breaks are load-bearing (D76 names them) because half of every Asian trading day would otherwise read as a dead feed; holidays are not, because staleness exposure already tells the truth about them and a correct holiday table is a maintenance treadmill.

**Contract:** this feature's [SPEC](SPEC.md) §2.

### R1-4 — One cash multi-currency account, long-only market and limit orders, and a declared per-market fee schedule

**Decision:** Each desk's paper book is one NautilusTrader **cash** account with netting positions, seeded once at book creation with 100,000 USD, 1,000,000 HKD, and 1,000,000 CNY, and no conversion between them: an instrument trades against its own currency's balance. R1 accepts exactly `MARKET` and `LIMIT` orders, time in force GTC, plus cancel; quantity must be a positive multiple of the instrument's lot and a limit price a positive multiple of its tick. Long-only follows from the cash account, and the trading node — its risk engine and sandbox, never the daemon — is the judge of sufficiency: the daemon validates form (catalog membership, side, type, lot, tick, price presence) and forwards, and a node refusal of any kind answers one code, `ORDER_REJECTED`, carrying the node's own reason, because a daemon that predicts balance or position sufficiency is a daemon computing a trading fact (per D38). Fees are charged by the sandbox at each market's declared per-side rate — US 0 bp, Hong Kong 11 bp, China A-share 3 bp — through an explicitly configured fee model (per D64); whether the rate rides the instrument type's own fee fields or an explicit MarketRig fee model is settled at slice time against the pinned crates, and it is never implicit. The workspace's numeric precision feature stays the default 64-bit mode, set explicitly and asserted at startup (per D39).

`ponytail:` the fee schedule folds each market's statutory round-trip charges (Hong Kong stamp duty and levies; the A-share sell-side stamp duty) into one symmetric per-side rate, and simulates no US regulatory fees — a modeling choice, not a claim of regulatory completeness; the upgrade path is a side-aware custom fee model behind the same explicit-configuration seam. The synthesized zero-spread book ceiling is already recorded in D76.

**Rationale:** A cash account is the only account type whose refusals match what R1 wants to forbid — shorting and leverage — so the physics do the policing. Round seed balances of comparable purchasing power keep three-market evidence legible. Two order types are the smallest set with both an immediate and a resting path, which is exactly what fills, cancels, and restoration need to be proven against.

**Contract:** root [SPEC §12.3](../../SPEC.md#123-trading-actions-and-approvals); this feature's [SPEC](SPEC.md) §4.

### R1-5 — History is append-only verbatim events beside normalized fields, with one restoration snapshot per desk

**Decision:** Migration 2 adds the trading tables: `trading_actions` (the idempotent, attributed command record per D64), `order_events`, `fills`, `position_events`, `position_cycles`, `prompts`, and `book_snapshots` — every row desk-scoped by a `desk_id` reference to `desks`, the caller-facing identity `(desk_id, action_id)` the `trading_actions` primary key, all money as decimal text with its currency. Sandbox payloads are stored verbatim as versioned JSON beside the normalized columns MarketRig queries; MarketRig recalculates nothing. Order and position listings are query projections over the event tables, never stored projections. `position_cycles` gains its row when a fill carries the netting position through zero; that insert and the cycle's `prompts` row (kind `EVALUATION`, state `QUEUED`) commit in one unit (per D22, D38). `book_snapshots` holds exactly one current serialization snapshot per desk — account state, open positions, open orders — rewritten in the same unit as every event that changes the book; restoration reads it and never replays history (per D64). MarketRig holds no `Position` objects outside the node: the node's own cache is the live authority and SQLite the durable one, settling the question root §12.4 left open.

**Rationale:** Append-only events plus verbatim payloads is the D38 capture rule made literal; a single rewritten snapshot is the cheapest thing restoration can read that history replay could never be. Skipping stored projections and an external position ledger removes two synchronization problems R1's volume cannot justify.

**Contract:** root [SPEC §12.4](../../SPEC.md#124-ledger-and-provenance), [§15](../../SPEC.md#15-persistence-crash-recovery-and-history); this feature's [SPEC](SPEC.md) §5, §6.

### R1-6 — Trading nodes start lazily and restore at start

**Decision:** A desk's `LiveNode` starts on the desk's first market-plane operation after daemon start — a quote read, a book read, or an order — never eagerly at daemon startup. Node start runs restoration from the desk's `book_snapshots` row: rebuild account and positions, re-place resting limit orders under their original client order identifiers. A node that fails to start or lose its feed makes that desk's market plane explicitly unavailable (`MARKET_UNAVAILABLE`) and appends a `TRADING_NODE_FAILED` event; it never fails daemon startup and never touches another desk (root §12.1). Stopping the daemon stops every node inside the §4.2 shutdown bound.

**Rationale:** Root §5.2 already makes trading setup lazy; restoration-at-start is the only ordering in which the snapshot is both complete (written transactionally with the last event) and needed (the node is about to serve). Eager startup restoration would put three markets' worth of node spin-up in front of `endpoint.json` for desks nobody may touch today.

**Contract:** root [SPEC §12.1](../../SPEC.md#121-trading-authority-and-node-topology); this feature's [SPEC](SPEC.md) §4.3.

### R1-7 — The MCP surface is five aggregate resources and two tools from one desk-bound adapter

**Decision:** `marketrig-mcp` (stdio, `rmcp =3.2.0` per D4's wire evidence) binds to exactly one desk, named by a required `--desk <name-or-id>` argument, and discovers the daemon exactly as the CLI does — `runtime/endpoint.json` under the resolved data root, authenticated health, UUID match (root §4.3). It enumerates five concrete resources — `marketrig://desk/<name>/quotes`, `.../book`, `.../positions`, `.../orders`, `.../instruments` — each an aggregate over the whole catalog or book, so the enumeration stays at five entries no matter how the catalog grows (the client that lists eagerly pays five, per D4). It exposes two tools, `submit_order` and `cancel_order`, forwarding every call to the daemon, which validates all arguments server-side; a daemon refusal surfaces as a structured tool error carrying the §4.3 envelope. Registration is by hand in R1 (the operator configures the runtime's MCP settings for the experiment); the runtime adapters take it over in R3 (root §6.4).

**Rationale:** Aggregates are what keeps "kept small in count" true beside a growing catalog; per-instrument resources would make the eager lister pay the catalog size in every session's context. One desk per adapter process matches how runtimes attach MCP servers and keeps the desk UUID out of every tool argument.

**Contract:** root [SPEC §13.1](../../SPEC.md#131-mcp-trading-plane); this feature's [SPEC](SPEC.md) §8.

### R1-8 — Order identity is caller-supplied; new REST routes and the `history` group carry the rest

**Decision:** Every mutating trading command carries a caller-supplied `action_id` (1–64 characters, `[a-z0-9-]`), unique per desk: the daemon records it in `trading_actions` before touching the sandbox, and a repeat of the same `action_id` returns the original action's record without acting (per D64, root §12.3). The daemon gains the §7 REST routes; the CLI gains exactly `marketrig [--json] history <orders|fills|cycles> <desk-name-or-id>`, newest first, over durable records only. New error codes, append-only per D68: `INSTRUMENT_UNKNOWN` 404, `ORDER_NOT_FOUND` 404, `ORDER_INVALID` 400 (form), `ORDER_REJECTED` 409 (the sandbox's refusal, reason verbatim), `MARKET_UNAVAILABLE` 503, `DESK_NOT_READY` 409.

**Rationale:** Only the caller knows whether two submits are one intent retried or two intents, so idempotency identity must come from the caller — and recording it before the sandbox sees the order is what makes an uncertain client response safely retryable. The history grammar mirrors the R0 `desk` group so the CLI grows a sibling, not a dialect.

**Contract:** root [SPEC §13.2](../../SPEC.md#132-cli-continuity-plane); this feature's [SPEC](SPEC.md) §7, §9.

### R1-9 — The gate's market is a harness-owned Yahoo-shaped server behind a third test-seam variable

**Decision:** The acceptance seam grows one variable: `MARKETRIG_TEST_QUOTE_URL`, honored only alongside `MARKETRIG_TEST_DATA_ROOT`, pointing the daemon's Yahoo client at a harness-owned local HTTP server that speaks the same chart-endpoint shape with scripted deterministic prices. The gate drives its scenarios against that stand-in; real Yahoo rides only the live and attended legs (per D75, D76). The daemon otherwise never reads the variable, and no configuration surface can change the feed URL.

**Rationale:** Faking the provider at the HTTP boundary exercises every line of the real client — parsing, retry, staleness, precision — which a stubbed `DataClient` would bypass, and it answers "is it my harness or is it Yahoo?" by construction. A third seam variable is a smaller cost than a test-only client factory, and it stays inert without the root-relocation variable that already marks a test run.

**Contract:** root [SPEC §17](../../SPEC.md#17-verification); this feature's [SPEC](SPEC.md) §10.
