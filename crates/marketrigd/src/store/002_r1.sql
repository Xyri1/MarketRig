-- Migration 2 — the R1 trading schema (feature SPEC `r1-equity-paper-trading`
-- §5). Forward-only; never edited.
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
  -- NautilusTrader's netting position id is `{instrument_id}-{strategy_id}` and
  -- is reused after a close, so one id spans every round trip on an instrument;
  -- the close instant is what makes a cycle unique, and the pair still refuses
  -- capturing the same close twice.
  UNIQUE (desk_id, position_id, closed_at_ns)
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

-- `operational_events.kind` gains TRADING_NODE_STARTED and TRADING_NODE_FAILED.
-- SQLite cannot alter a CHECK, so the table is rebuilt with the widened
-- vocabulary and its rows carried over verbatim; everything else is byte-for-byte
-- migration 1's definition.
CREATE TABLE operational_events_2 (
  id             TEXT NOT NULL PRIMARY KEY,           -- lowercase UUIDv7
  kind           TEXT NOT NULL CHECK (kind IN
                   ('RECOVERY','DESK_CREATED','DESK_READY','DESK_FAILED','DESK_RETRIED',
                    'TRADING_NODE_STARTED','TRADING_NODE_FAILED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_2 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_2 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
