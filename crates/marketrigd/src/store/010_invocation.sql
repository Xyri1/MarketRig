-- Migration 10 — trigger invocation (feature SPEC `event-triggers` §4, per
-- ET-1, ET-2, ET-7). Forward-only; never edited.
--
-- Foreign keys are off for the migration window (`store.rs` applies SQLite's
-- documented ALTER TABLE procedure), which is what lets `triggers` and
-- `firings` be rebuilt although `firings`, `executions`, and `trading_actions`
-- reference them: with enforcement off `DROP TABLE` performs no implicit delete
-- and each rename leaves those REFERENCES clauses naming the rebuilt table
-- (migration 6's note).

-- `triggers` loses `source` — every trigger is invocable, and a scheduled one
-- is no longer a different kind — and relaxes migration 3's two schedule CHECKs
-- so that a one-off carries `at_ns` or nothing and a recurring trigger carries
-- the rule triple or nothing. Every other column is migration 3's definition
-- byte for byte, and both partial indexes are recreated unchanged.
CREATE TABLE triggers_10 (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  name TEXT NOT NULL,                              -- desk-name grammar
  recurrence TEXT NOT NULL CHECK (recurrence IN ('ONE_OFF','RECURRING')),
  brief TEXT NOT NULL, context TEXT,
  at_ns INTEGER, rrule TEXT, dtstart TEXT, tz TEXT,
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  revision INTEGER NOT NULL,
  code_snapshot_id TEXT REFERENCES code_snapshots(id),
  next_occurrence_ns INTEGER,                      -- the projection; NULL = never due
  created_at_ns INTEGER NOT NULL, updated_at_ns INTEGER NOT NULL, deleted_at_ns INTEGER,
  CHECK (at_ns IS NULL OR recurrence = 'ONE_OFF'),
  CHECK (rrule IS NULL OR recurrence = 'RECURRING'),
  CHECK ((rrule IS NULL) = (dtstart IS NULL) AND (rrule IS NULL) = (tz IS NULL)),
  CHECK (at_ns IS NULL OR rrule IS NULL)
) STRICT;

INSERT INTO triggers_10 (id, desk_id, name, recurrence, brief, context, at_ns, rrule,
                         dtstart, tz, enabled, revision, code_snapshot_id,
                         next_occurrence_ns, created_at_ns, updated_at_ns, deleted_at_ns)
  SELECT id, desk_id, name, recurrence, brief, context, at_ns, rrule,
         dtstart, tz, enabled, revision, code_snapshot_id,
         next_occurrence_ns, created_at_ns, updated_at_ns, deleted_at_ns
    FROM triggers;
DROP TABLE triggers;
ALTER TABLE triggers_10 RENAME TO triggers;

CREATE UNIQUE INDEX triggers_live_name ON triggers (desk_id, name) WHERE deleted_at_ns IS NULL;
CREATE INDEX triggers_due ON triggers (next_occurrence_ns)
  WHERE deleted_at_ns IS NULL AND enabled = 1 AND next_occurrence_ns IS NOT NULL;

-- `firings` gains the caller's request identity and its verbatim input, and
-- migration 3's table-level UNIQUE becomes two partial unique indexes so each
-- entry path keeps its own guard: the scheduled occurrence, and the invoked
-- request id (§2.2, §4).
CREATE TABLE firings_10 (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  trigger_id TEXT NOT NULL REFERENCES triggers(id),
  occurrence_ns INTEGER NOT NULL, accepted_at_ns INTEGER NOT NULL,
  trigger_revision INTEGER NOT NULL, brief TEXT NOT NULL, context TEXT,
  code_snapshot_id TEXT REFERENCES code_snapshots(id),
  request_id TEXT,                                 -- the caller's identity (§2.1)
  input TEXT,                                      -- stored verbatim, never parsed (§2.1)
  CHECK (input IS NULL OR request_id IS NOT NULL)
) STRICT;

INSERT INTO firings_10 (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns,
                        trigger_revision, brief, context, code_snapshot_id)
  SELECT id, desk_id, trigger_id, occurrence_ns, accepted_at_ns,
         trigger_revision, brief, context, code_snapshot_id
    FROM firings;
DROP TABLE firings;
ALTER TABLE firings_10 RENAME TO firings;

CREATE UNIQUE INDEX firings_scheduled ON firings (desk_id, trigger_id, occurrence_ns) WHERE request_id IS NULL;
CREATE UNIQUE INDEX firings_invoked   ON firings (desk_id, trigger_id, request_id)    WHERE request_id IS NOT NULL;
CREATE INDEX firings_by_trigger ON firings (desk_id, trigger_id, accepted_at_ns, id);
