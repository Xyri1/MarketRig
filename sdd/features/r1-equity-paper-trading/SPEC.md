# R1 — Equity paper trading and the agent's market surface: Feature SPEC

*Decision basis: per D4, D5, D38, D39, D63, D64, D75, D76, D77, and R1-1…R1-9.*

This specification settles the mechanics Milestone R1 implements. It refines root [SPEC](../../SPEC.md) §3, §12, §13.1, §13.2, §15, and §17 and contradicts nothing settled there. Mechanics R1 does not need — prompt delivery, approval workflows, pagination, OpenAPI wiring, holiday calendars — stay **deferred** (root §18) and are not invented here.

## 1. Workspace additions

`crates/marketrig-mcp` joins the workspace (root §3): a thin stdio binary on `rmcp =3.2.0` (the line D4's wire evidence was gathered on; re-verified at plan time). `crates/marketrigd` gains the `nautilus-*` crates, every one pinned `=0.62.0` in lockstep (per D39), and the equity feed's endpoint layer — MarketRig's own thin chart client on the workspace's reqwest line (`=0.13.4`, the line `nautilus-network` pins; D76's `yahoo_finance_api` candidate was replaced at plan time for lacking a chart-URL override, per R1-1). The workspace does **not** enable NautilusTrader's high-precision feature: 64-bit precision is declared explicitly, and the daemon asserts at node start that the precision the crates report matches (per D39, R1-4). Exact new pins land in the implementing slice per the house rule.

New `marketrigd` modules mirror the check prefixes: `feed`, `catalog`, `node`, `trade`; the REST additions extend `api`, the CLI additions `crates/marketrig`.

## 2. Feed and observations

### 2.1 Client behavior (R1-1)

One out-of-tree `DataClient` per node polls Yahoo's chart endpoint per catalog instrument:

- once at subscription, whatever the phase — so a desk always has a last close to look at;
- then, only while the instrument's market phase is `OPEN`: every **30 seconds**, tightened to every **10 seconds** while the desk holds an open order or a nonflat position in that instrument (R1-1);
- HTTP 429: up to **8 attempts, 400 ms apart** (the spike-verified counts, per D76); exhaustion or any other failure leaves the last accepted observation standing and marks health `DEGRADED` — never a silent provider substitution (root §12.2);
- the endpoint base URL is compiled in; `MARKETRIG_TEST_QUOTE_URL` (§10.1) is its only override.

Each accepted response advances that instrument's observation in the daemon's installation-wide market-state module: last price as canonical decimal text at the **instrument's** precision (never a formatting choice — the sandbox drops mismatched precision silently, per D76), the source timestamp, and the receipt instant. A response whose source timestamp does not advance replaces nothing but refreshes health. Observations are never persisted (root §15).

### 2.2 Calendars and phase (R1-3)

| Market key | Zone | Sessions (Mon–Fri) |
| --- | --- | --- |
| `US` | America/New_York | 09:30–16:00 |
| `HK` | Asia/Hong_Kong | 09:30–12:00, 13:00–16:00 |
| `CN` | Asia/Shanghai | 09:30–11:30, 13:00–15:00 |

The derived phase is `OPEN` inside a session and `CLOSED` otherwise. Phase gates polling and labels observations; it never gates an order. There is no holiday calendar (R1-3, `ponytail:` recorded there).

### 2.3 Observation shape and health

Every read (REST §7, MCP §8) returns, per instrument:

```json
{ "instrument_id": "0700.XHKG", "provider": "yahoo", "venue": "XHKG",
  "last": "441.40", "currency": "HKD",
  "source_time_ns": 0, "received_at_ns": 0, "read_at_ns": 0, "age_ms": 0,
  "sequence": 7, "market_phase": "OPEN", "health": "LIVE",
  "book_synthesized": true }
```

Health vocabulary: `LIVE` (the most recent poll cycle succeeded), `DEGRADED` (a failure since the last success; the shown observation is the last accepted one, aging), `UNAVAILABLE` (no observation ever accepted, or the desk's node is down — price fields are omitted). The feed declares no delay figure — verified 2026-09-01: `exchangeDataDelayedBy` is null on all three exchanges (R1-1) — so `source_time_ns` versus `received_at_ns` is the delay evidence, exposed rather than summarized.

## 3. Instrument catalog (R1-2)

The catalog is compiled into the daemon; each entry:

```text
instrument_id      Nautilus identifier, SYMBOL.VENUE     e.g. 0700.XHKG
yahoo_symbol       the feed's symbol                     e.g. 0700.HK
market             US | HK | CN                          calendar and fee key
currency           USD | HKD | CNY
price_increment    decimal text                          the fixed tick (R1-2)
lot_size           integer                               order-quantity multiple
```

Starter set (fifteen; tick and lot venue-verified when the implementing slice lands the data, per R1-2): US — AAPL.XNAS, MSFT.XNAS, NVDA.XNAS, AMZN.XNAS, TSLA.XNAS (tick 0.01, lot 1); HK — 0700.XHKG (0.20/100), 9988.XHKG (0.10/100), 0005.XHKG (0.10/400), 1299.XHKG (0.05/200), 3690.XHKG (0.05/100); CN — 600519.XSHG, 601318.XSHG (tick 0.01, lot 100), 000001.XSHE, 000858.XSHE, 300750.XSHE (tick 0.01, lot 100).

Any operation naming an instrument outside the catalog answers `INSTRUMENT_UNKNOWN`. The catalog is the polling universe and the `instruments` resource's content. Extending it is a data change verified by `catalog::entries_valid`, not a new decision.

## 4. Paper account, book, and orders

### 4.1 Account and sandbox wiring (R1-4)

One NautilusTrader **cash** account with netting positions per desk, created with the desk's node and seeded exactly once: **100,000 USD, 1,000,000 HKD, 1,000,000 CNY**. No conversion between balances: an instrument trades against its own currency. Wiring follows root §12.1's normative rules verbatim: sandbox execution client through the builder's simulated-client entry point; the fee model configured **explicitly** — never implicit — to charge each market's declared per-side rate, whether that rides the instrument type's own fee fields or an explicit MarketRig fee model (settled at slice time against the pinned crates, per R1-4); the data-event sender taken on the node thread; one venue routed to one execution client per node.

Fee rates by market (R1-4, `ponytail:` ceiling recorded there): `US` 0 bp, `HK` 11 bp, `CN` 3 bp per side.

The synthesized book (per D76): both sides equal the last observation at the instrument's precision, both sizes one lot. Everything that exposes the book says `book_synthesized: true`. A market order therefore fills at last; a limit order rests until last crosses it; round trips cost only fees.

### 4.2 Order validation

`POST /desks/{desk_id}/orders` (and the `submit_order` tool behind it) accepts:

```json
{ "action_id": "buy-tencent-1", "instrument_id": "0700.XHKG",
  "side": "BUY", "type": "MARKET", "quantity": "100", "price": null }
```

The daemon validates **form**, and nowhere else validates anything (per D4): `action_id` matches `[a-z0-9-]{1,64}` and is new for the desk (a repeat replays, §6); the instrument is cataloged; `side` is `BUY | SELL`, `type` is `MARKET | LIMIT`; `quantity` is a positive multiple of the lot; `price` is required for `LIMIT`, forbidden for `MARKET`, and a positive multiple of the tick — each failure `ORDER_INVALID`. Sufficiency is the sandbox's judgment, not the daemon's (per D38, R1-4): a sandbox refusal of any kind — insufficient balance, a sell beyond the held quantity, or any other denial — answers `ORDER_REJECTED` with the sandbox's own reason in the message. Time in force is GTC and implicit. Cancel takes the resting order's `client_order_id` plus its own `action_id`; an unknown or already-terminal order answers `ORDER_NOT_FOUND`.

Submission is synchronous through the sandbox: the route answers after the sandbox accepts (and, for a marketable order, fills) or refuses. Orders on a desk that is not `READY` answer `DESK_NOT_READY`.

### 4.3 Node lifecycle and restoration (R1-6)

A desk's node starts on its first market-plane operation after daemon start, on its own OS thread with a current-thread Tokio runtime (root §12.1). Start order: build node → assert precision → load catalog instruments into the cache → run restoration from `book_snapshots` (rebuild account and positions; re-place resting limit orders under their **original client order identifiers**) → register the data client → subscribe the catalog → append `TRADING_NODE_STARTED`. A start failure appends `TRADING_NODE_FAILED`, answers `MARKET_UNAVAILABLE` to market-plane operations for that desk, blocks nothing else, and the next operation may retry the start. Daemon shutdown stops every node inside root §4.6's bound.

## 5. Durable schema (R1-5)

Migration 2. All rows desk-scoped (`desk_id` referencing `desks`), money as decimal text with currency, payloads verbatim versioned JSON, conventions of root §15 throughout:

```sql
CREATE TABLE trading_actions (
  desk_id       TEXT NOT NULL REFERENCES desks(id),
  action_id     TEXT NOT NULL,                    -- caller-supplied (R1-8)
  id            TEXT NOT NULL UNIQUE,             -- lowercase UUIDv7
  kind          TEXT NOT NULL CHECK (kind IN ('SUBMIT','CANCEL')),
  source        TEXT NOT NULL CHECK (source IN ('SESSION')),  -- widened by later milestones
  request       TEXT NOT NULL,
  outcome       TEXT,                             -- response record, set when answered
  created_at_ns INTEGER NOT NULL,
  PRIMARY KEY (desk_id, action_id)
) STRICT;

CREATE TABLE order_events (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  client_order_id TEXT NOT NULL, instrument_id TEXT NOT NULL,
  kind TEXT NOT NULL,                             -- the sandbox event's own type name
  payload_version INTEGER NOT NULL, payload TEXT NOT NULL,
  occurred_at_ns INTEGER NOT NULL
) STRICT;
CREATE INDEX order_events_by_order ON order_events (desk_id, client_order_id, occurred_at_ns, id);

CREATE TABLE fills (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  client_order_id TEXT NOT NULL, trade_id TEXT NOT NULL, instrument_id TEXT NOT NULL,
  side TEXT NOT NULL CHECK (side IN ('BUY','SELL')),
  quantity TEXT NOT NULL, price TEXT NOT NULL,
  commission TEXT NOT NULL, currency TEXT NOT NULL,
  payload_version INTEGER NOT NULL, payload TEXT NOT NULL,
  occurred_at_ns INTEGER NOT NULL
) STRICT;
CREATE INDEX fills_by_desk ON fills (desk_id, occurred_at_ns, id);

CREATE TABLE position_events (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  position_id TEXT NOT NULL, instrument_id TEXT NOT NULL, kind TEXT NOT NULL,
  payload_version INTEGER NOT NULL, payload TEXT NOT NULL,
  occurred_at_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE position_cycles (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  position_id TEXT NOT NULL, instrument_id TEXT NOT NULL,
  opened_at_ns INTEGER NOT NULL, closed_at_ns INTEGER NOT NULL,
  realized_pnl TEXT NOT NULL,                     -- net of fees (root §12.4)
  currency TEXT NOT NULL,
  payload_version INTEGER NOT NULL, payload TEXT NOT NULL,
  UNIQUE (desk_id, position_id)
) STRICT;

CREATE TABLE prompts (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  kind TEXT NOT NULL CHECK (kind IN ('EVALUATION')),      -- widened in R2
  state TEXT NOT NULL CHECK (state IN ('QUEUED')),        -- delivery states arrive in R3
  payload TEXT NOT NULL, created_at_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE book_snapshots (
  desk_id TEXT NOT NULL PRIMARY KEY REFERENCES desks(id),
  payload_version INTEGER NOT NULL, payload TEXT NOT NULL, -- account, open positions, open orders
  written_at_ns INTEGER NOT NULL
) STRICT;
```

Migration 2 also widens `operational_events.kind` with `TRADING_NODE_STARTED` and `TRADING_NODE_FAILED` — as a table rebuild (create the widened `STRICT` table, copy, drop, rename, recreate the index), since SQLite cannot alter a `CHECK`; the pattern every later kind widening repeats. Every sandbox event lands in its event table, and the `book_snapshots` row is rewritten, inside one unit with whatever else that event implies; order and position listings are query projections over these tables, never stored ones (R1-5). MarketRig holds no `Position` objects outside the node (R1-5, settling root §12.4's open question).

## 6. Actions, replay, and the evaluation queue

An accepted mutating command inserts its `trading_actions` row **before** the sandbox sees the order, in its own unit; the outcome lands on the row when the command answers. A repeated `(desk_id, action_id)` returns the stored outcome with `200` and acts on nothing — that is the whole idempotency contract (R1-8, root §12.3). Every action carries `source: SESSION` in R1; trigger attribution widens the vocabulary in R2 by migration.

A fill that carries the netting position through zero closes one cycle (root §12.4): one unit inserts the `position_cycles` row and its `prompts` row and rewrites the snapshot. The prompt payload:

```json
{ "kind": "EVALUATION", "cycle_id": "…", "instrument_id": "0700.XHKG",
  "realized_pnl": "-93.71", "currency": "HKD", "closed_at_ns": 0,
  "client_order_ids": ["…"], "fill_ids": ["…"] }
```

— a stable history reference with enough identity to query supporting rows (root §11.1). R1 delivers nothing: prompts are born `QUEUED` and stay there until R3.

## 7. REST surface additions

All routes behind the R0 bearer and envelope. Live-state routes (node-backed) start the node lazily (§4.3):

| Route | Success | Purpose |
| --- | --- | --- |
| `GET /desks/{desk_id}/market/instruments` | `200` `{"instruments":[…]}` | the catalog (§3) |
| `GET /desks/{desk_id}/market/quotes` | `200` `{"quotes":[Observation…]}` | §2.3, whole catalog |
| `GET /desks/{desk_id}/market/book` | `200` `{"book":[…]}` | top-of-book, synthesized flag |
| `GET /desks/{desk_id}/positions` | `200` `{"positions":[…]}` | live, from the node |
| `GET /desks/{desk_id}/orders` | `200` `{"orders":[…]}` | open orders, from the node |
| `POST /desks/{desk_id}/orders` | `201` ActionRecord (`200` on replay) | submit (§4.2) |
| `POST /desks/{desk_id}/orders/{client_order_id}/cancel` | `200` ActionRecord | cancel |
| `GET /desks/{desk_id}/history/orders` | `200` `{"orders":[…]}` | projection, newest first |
| `GET /desks/{desk_id}/history/fills` | `200` `{"fills":[…]}` | newest first |
| `GET /desks/{desk_id}/history/cycles` | `200` `{"cycles":[…]}` | newest first |

The ActionRecord is the `trading_actions` row's JSON: `action_id`, `id`, `kind`, `created_at_ns`, and the outcome (for a submit, the order's current projection including `client_order_id`). History routes return complete newest-first lists; pagination stays deferred (root §18).

New codes (append-only per D68): `INSTRUMENT_UNKNOWN` 404, `ORDER_NOT_FOUND` 404, `ORDER_INVALID` 400 (form), `ORDER_REJECTED` 409 (sandbox refusal, reason verbatim), `DESK_NOT_READY` 409, `MARKET_UNAVAILABLE` 503.

## 8. The MCP adapter (R1-7)

`marketrig-mcp --desk <name-or-id>` discovers and verifies the daemon exactly as the CLI does (root §4.3, R0 feature SPEC §5.2), resolving a name through `GET /desks`. It serves stdio MCP on `rmcp =3.2.0`:

- **Resources**, enumerated concretely in `resources/list`, five entries, JSON bodies identical to the §7 routes they proxy: `marketrig://desk/<name>/quotes`, `…/book`, `…/positions`, `…/orders`, `…/instruments`. A read calls the daemon at read time; the adapter caches nothing.
- **Tools**: `submit_order` (the §4.2 body, minus desk) and `cancel_order` (`client_order_id`, `action_id`). The daemon validates everything; the advertised schema is documentation (per D4). A daemon refusal, an unreachable daemon, or a failed verification surfaces as a structured tool error whose text carries the §4.3 envelope's code and message — explicit, never retried.
- No subscriptions, no completion, no other capability advertised.

Registration is operator-performed in R1 (the experiment configures each runtime's MCP settings by hand); runtime adapters take it over in R3 (root §6.4).

## 9. CLI additions (R1-8)

`marketrig [--json] history <orders|fills|cycles> <desk-name-or-id>` — the R0 grammar's sibling: name-or-id resolved through the daemon's listing, human output plain rows newest first, `--json` the route's body verbatim, the R0 exit codes unchanged. Live positions, open orders, quotes, and instrument discovery are deliberately absent: they belong to the MCP plane (per D4).

## 10. Acceptance

### 10.1 The stand-in feed (R1-9)

The gate serves a harness-owned local HTTP server speaking the chart-endpoint shape with a scripted deterministic price sequence, and sets `MARKETRIG_TEST_QUOTE_URL` to it. The variable is honored only alongside `MARKETRIG_TEST_DATA_ROOT`. `MARKETRIG_TEST_NO_TRADING` (root §17) keeps the daemon off the compiled-in public URL; it does not suppress polling a stand-in named by `MARKETRIG_TEST_QUOTE_URL`. Scripts can advance prices, return 429 bursts, and go dark, which is what G18 drives. Real Yahoo appears only in the live and attended legs (per D75, D76).

### 10.2 Gate scenarios (continuing R0's chain)

- **G12 — catalog and first observation.** `market/instruments` lists the catalog; the first `market/quotes` read lazily starts the node (`TRADING_NODE_STARTED` appended) and, after the stand-in ticks, shows an advancing `sequence` with full provenance fields and `book_synthesized: true`.
- **G13 — USD round trip and the queued evaluation.** Market buy of one AAPL lot → fill row, order events verbatim, live position, balance moved; market sell to flat → `position_cycles` row with net realized P&L, and — asserted via read-only SQLite in one query — its `prompts` row born in the same transaction.
- **G14 — resting, cancel, and replay.** A limit buy below market rests; the same `action_id` resubmitted answers `200` with the original record and creates no second order; cancel with a fresh `action_id` removes it; cancel of an unknown id answers `ORDER_NOT_FOUND`.
- **G15 — non-USD cycle.** A 0700.XHKG round trip closes with realized P&L in `HKD` and nonzero commissions at the HK rate on both fills.
- **G16 — refusals.** Unknown instrument (`INSTRUMENT_UNKNOWN`); quantity off-lot (`ORDER_INVALID`); a buy exceeding the CNY balance and a sell exceeding the position (each `ORDER_REJECTED`, the sandbox's reason in the message); each the envelope with its documented code, and no `trading_actions` outcome recording an accepted order.
- **G17 — restoration.** With a resting limit order standing: `POST /quit`, restart, first market-plane read restores the book — the order is open under its **original** `client_order_id`, positions and balances match, history rows are untouched, and quotes are `UNAVAILABLE` until the stand-in is polled again.
- **G18 — feed honesty.** The stand-in answers 429 on the first seven attempts and succeeds on the eighth → observation accepted (the retry bound's last attempt); a poll of nine straight 429s → no new observation, health `DEGRADED`; the stand-in goes dark → health `DEGRADED`, last observation standing with growing age; a never-observed instrument reads `UNAVAILABLE` with no price fields.
- **G19 — the MCP plane.** The harness's own MCP client (per D4) spawns `marketrig-mcp --desk …`: `resources/list` names exactly five; the quote resource read twice straddling a stand-in tick yields different observations; `submit_order` and `cancel_order` round-trip; a two-field call against the four-field schema answers a structured tool error carrying `ORDER_INVALID`.
- **G20 — the history group.** `marketrig --json history` for orders, fills, and cycles matches the SQLite rows; unknown desk exits 1; no daemon exits 3.

G13 + G15 + G17 together are the roadmap's R1 evidence line.

### 10.3 Experiment scenarios (the first attended ones)

- **E1 / E2 — real runtimes on the market plane.** For Codex CLI and Claude Code in turn, operator-attended, real Yahoo, MCP registration by hand: the session reads the desk's quote resource twice and sees a fresh value, then submits and cancels a paper order through the typed tools; the harness verifies by side effects (the `trading_actions` rows and the resting order's lifecycle). Agent-behavior aspects end inconclusive rather than failed (root §17).

## 11. Required checks

**Module checks** (`cargo test`, fakes allowed):

- `feed::retry_on_429_bounded` — 8 attempts 400 ms apart against a scripted server; exhaustion leaves the prior observation and marks `DEGRADED`.
- `feed::cadence_two_tier` — an idle instrument polls on the 30-second tier; an open order or nonflat position moves it to the 10-second tier and flat-and-orderless moves it back.
- `feed::phase_from_calendar` — the §2.2 table incl. both lunch breaks and a US DST boundary.
- `feed::observation_provenance` — the §2.3 fields, sequence advance on accepted updates only, precision from the instrument.
- `feed::base_url_seam_only` — `MARKETRIG_TEST_QUOTE_URL` inert without `MARKETRIG_TEST_DATA_ROOT`.
- `catalog::entries_valid` — unique ids, positive tick and lot, currency matching market, calendar key present, for every entry.
- `node::precision_asserted` — the daemon's startup assertion against the crates' reported precision mode.
- `node::sender_on_node_thread` — the data-event sender is obtained on the node thread and a clone moved into the polling task (the D76 landmine).
- `store::trading_migration_applies` — empty database reaches `user_version` 2 with the §5 schema; migration 1 databases upgrade.
- `trade::cycle_and_prompt_atomic` — a through-zero fill inserts cycle and prompt in one transaction; a forced failure between them leaves neither.
- `trade::snapshot_restores_book` — snapshot round trip: account, positions, and a resting order re-placed under its original client order id.
- `api::action_replay` — repeated `action_id` returns the original record, acts once.
- `api::market_codes` — every §7 error path answers the envelope with its documented code.
- `mcp::resources_and_freshness` — five concrete resources; re-read returns the daemon's current observation.
- `mcp::server_side_validation` — malformed tool calls answer structured tool errors from the daemon's validation, not the client's.
- `cli::history_exit_codes` — the group's 0/1/3 mapping against a fake endpoint.

**Gate** (the same Cargo test target as R0's, extended): scenarios G12–G20 in order after G11, on the stand-in feed, producing the evidence bundle.

**Experiment** (attended target, first content): E1 and E2, once per platform-and-runtime cell, real Yahoo and real runtime CLIs.

**Static checks:** rustfmt, Clippy `-D warnings`, `cargo test` across the workspace — now including `marketrig-mcp` — on both MVP platforms in CI.
