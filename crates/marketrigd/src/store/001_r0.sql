-- Migration 1 — the R0 schema (feature SPEC §3.3). Forward-only; never edited.
CREATE TABLE desks (
  id              TEXT NOT NULL PRIMARY KEY,          -- lowercase UUIDv7
  name            TEXT NOT NULL UNIQUE,               -- immutable kebab name
  state           TEXT NOT NULL CHECK (state IN ('CREATING','READY','FAILED')),
  workspace_path  TEXT NOT NULL,
  created_at_ns   INTEGER NOT NULL,
  ready_at_ns     INTEGER,
  failure_code    TEXT,
  failure_message TEXT,
  CHECK ((state = 'READY')  = (ready_at_ns  IS NOT NULL)),
  CHECK ((state = 'FAILED') = (failure_code IS NOT NULL))
) STRICT;

CREATE TABLE operational_events (
  id             TEXT NOT NULL PRIMARY KEY,           -- lowercase UUIDv7
  kind           TEXT NOT NULL CHECK (kind IN
                   ('RECOVERY','DESK_CREATED','DESK_READY','DESK_FAILED','DESK_RETRIED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
