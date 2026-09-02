-- Migration 3 — the R2 scheduled-trigger schema (feature SPEC
-- `r2-scheduled-triggers` §7). Forward-only; never edited.
--
-- `PRAGMA foreign_keys` is on for the whole unit, so the new tables come first:
-- the rebuilt `trading_actions` references `triggers` and `firings`, and nothing
-- references the three tables this migration drops.
CREATE TABLE code_snapshots (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  source TEXT NOT NULL, suffix TEXT NOT NULL,
  argv TEXT NOT NULL,                              -- JSON array of strings
  timeout_secs INTEGER NOT NULL,
  fingerprint TEXT NOT NULL,                       -- lowercase hex SHA-256 (§4.1)
  approved_at_ns INTEGER,                          -- Always allow in R2: = created_at_ns
  created_at_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE triggers (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  name TEXT NOT NULL,                              -- desk-name grammar
  source TEXT NOT NULL CHECK (source IN ('SCHEDULED')),        -- EVENT widened later
  recurrence TEXT NOT NULL CHECK (recurrence IN ('ONE_OFF','RECURRING')),
  brief TEXT NOT NULL, context TEXT,
  at_ns INTEGER, rrule TEXT, dtstart TEXT, tz TEXT,
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  revision INTEGER NOT NULL,
  code_snapshot_id TEXT REFERENCES code_snapshots(id),
  next_occurrence_ns INTEGER,                      -- the projection; NULL = never due
  created_at_ns INTEGER NOT NULL, updated_at_ns INTEGER NOT NULL, deleted_at_ns INTEGER,
  CHECK ((recurrence = 'ONE_OFF') = (at_ns IS NOT NULL)),
  CHECK ((recurrence = 'RECURRING') = (rrule IS NOT NULL AND dtstart IS NOT NULL AND tz IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX triggers_live_name ON triggers (desk_id, name) WHERE deleted_at_ns IS NULL;
CREATE INDEX triggers_due ON triggers (next_occurrence_ns)
  WHERE deleted_at_ns IS NULL AND enabled = 1 AND next_occurrence_ns IS NOT NULL;

CREATE TABLE firings (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  trigger_id TEXT NOT NULL REFERENCES triggers(id),
  occurrence_ns INTEGER NOT NULL, accepted_at_ns INTEGER NOT NULL,
  trigger_revision INTEGER NOT NULL, brief TEXT NOT NULL, context TEXT,
  code_snapshot_id TEXT REFERENCES code_snapshots(id),
  UNIQUE (desk_id, trigger_id, occurrence_ns)      -- the duplicate-wake guard (root §10)
) STRICT;
CREATE INDEX firings_by_trigger ON firings (desk_id, trigger_id, accepted_at_ns, id);

CREATE TABLE executions (
  firing_id TEXT NOT NULL PRIMARY KEY REFERENCES firings(id),
  desk_id TEXT NOT NULL REFERENCES desks(id),
  daemon_uuid TEXT NOT NULL,                       -- the daemon that claimed it
  state TEXT NOT NULL CHECK (state IN ('RUNNING','COMPLETE')),
  outcome TEXT CHECK (outcome IN ('EXITED','TIMED_OUT','OUTPUT_LIMIT','SPAWN_FAILED','QUIT','DAEMON_LOST')),
  exit_code INTEGER, executable TEXT, error TEXT,
  stdout BLOB, stderr BLOB,                        -- the raw result, capped (§4.3)
  stdout_truncated INTEGER CHECK (stdout_truncated IN (0, 1)),
  stderr_truncated INTEGER CHECK (stderr_truncated IN (0, 1)),
  started_at_ns INTEGER NOT NULL, finished_at_ns INTEGER,
  CHECK ((state = 'COMPLETE') = (outcome IS NOT NULL)),
  CHECK ((state = 'COMPLETE') = (finished_at_ns IS NOT NULL))
) STRICT;
CREATE INDEX executions_running ON executions (desk_id) WHERE state = 'RUNNING';

-- `prompts.kind` gains TRIGGER_RESULT. SQLite cannot alter a CHECK, so the
-- table is rebuilt by migration 2's pattern; everything else is byte-for-byte
-- migration 2's definition.
CREATE TABLE prompts_3 (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  kind TEXT NOT NULL CHECK (kind IN ('EVALUATION','TRIGGER_RESULT')),
  state TEXT NOT NULL CHECK (state IN ('QUEUED')),        -- delivery states arrive in R3
  payload TEXT NOT NULL, created_at_ns INTEGER NOT NULL
) STRICT;

INSERT INTO prompts_3 (id, desk_id, kind, state, payload, created_at_ns)
  SELECT id, desk_id, kind, state, payload, created_at_ns FROM prompts;
DROP TABLE prompts;
ALTER TABLE prompts_3 RENAME TO prompts;

-- `trading_actions.source` gains TRIGGER, and a TRIGGER row names the firing
-- that placed it (§6). Same rebuild; the primary key and the `id` uniqueness
-- are migration 2's.
CREATE TABLE trading_actions_3 (
  desk_id       TEXT NOT NULL REFERENCES desks(id),
  action_id     TEXT NOT NULL,                    -- caller-supplied (R1-8)
  id            TEXT NOT NULL UNIQUE,             -- lowercase UUIDv7
  kind          TEXT NOT NULL CHECK (kind IN ('SUBMIT','CANCEL')),
  source        TEXT NOT NULL CHECK (source IN ('SESSION','TRIGGER')),
  trigger_id    TEXT REFERENCES triggers(id),
  firing_id     TEXT REFERENCES firings(id),
  request       TEXT NOT NULL,
  outcome       TEXT,                             -- response record, set when answered
  created_at_ns INTEGER NOT NULL,
  PRIMARY KEY (desk_id, action_id),
  CHECK ((source = 'TRIGGER') = (firing_id IS NOT NULL))
) STRICT;

INSERT INTO trading_actions_3
    (desk_id, action_id, id, kind, source, request, outcome, created_at_ns)
  SELECT desk_id, action_id, id, kind, source, request, outcome, created_at_ns
    FROM trading_actions;
DROP TABLE trading_actions;
ALTER TABLE trading_actions_3 RENAME TO trading_actions;

-- `operational_events.kind` gains TRIGGER_MISSED; the prior vocabulary is
-- carried over exactly, and the tail index is recreated.
CREATE TABLE operational_events_3 (
  id             TEXT NOT NULL PRIMARY KEY,           -- lowercase UUIDv7
  kind           TEXT NOT NULL CHECK (kind IN
                   ('RECOVERY','DESK_CREATED','DESK_READY','DESK_FAILED','DESK_RETRIED',
                    'TRADING_NODE_STARTED','TRADING_NODE_FAILED','TRIGGER_MISSED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_3 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_3 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
