-- Migration 6 — the R5 installation policies and the approval vocabulary on the
-- two gated rows (feature SPEC `r5-desktop-approval-controls` §2 and §3, per
-- R5-1 and R5-2). Forward-only; never edited.
--
-- Foreign keys are off for the migration window (`store.rs` applies SQLite's
-- documented ALTER TABLE procedure), which is what lets `code_snapshots` be
-- rebuilt although `triggers` and `firings` reference it: with enforcement off
-- `DROP TABLE` performs no implicit delete and the rename leaves their
-- REFERENCES clauses naming `code_snapshots`, which is the rebuilt table.

CREATE TABLE installation_settings (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  trigger_code_policy TEXT NOT NULL
    CHECK (trigger_code_policy IN ('ALWAYS_ALLOW','REQUIRE_APPROVAL')),
  paper_order_policy TEXT NOT NULL
    CHECK (paper_order_policy IN ('ALWAYS_ALLOW','REQUIRE_APPROVAL')),
  delivery_mode TEXT NOT NULL CHECK (delivery_mode = 'QUEUE'),  -- STEER stays reserved (root §11.2)
  updated_at_ns INTEGER NOT NULL
) STRICT;
INSERT INTO installation_settings VALUES (1, 'REQUIRE_APPROVAL', 'ALWAYS_ALLOW', 'QUEUE', 0);

-- `code_snapshots` gains `approval` and `decided_at_ns` and loses R2's
-- `approved_at_ns`, which the Trigger resource now derives. SQLite cannot alter
-- a CHECK, so the table is rebuilt by migration 3's pattern; every other column
-- is migration 3's definition byte for byte. R2's rows were all Always allow.
CREATE TABLE code_snapshots_6 (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  source TEXT NOT NULL, suffix TEXT NOT NULL,
  argv TEXT NOT NULL,                              -- JSON array of strings
  timeout_secs INTEGER NOT NULL,
  fingerprint TEXT NOT NULL,                       -- lowercase hex SHA-256
  approval TEXT NOT NULL CHECK (approval IN ('ALWAYS_ALLOW','PENDING','APPROVED','DENIED')),
  decided_at_ns INTEGER,                           -- null exactly while PENDING
  created_at_ns INTEGER NOT NULL,
  CHECK ((approval = 'PENDING') = (decided_at_ns IS NULL))
) STRICT;

INSERT INTO code_snapshots_6 (id, desk_id, source, suffix, argv, timeout_secs,
                              fingerprint, approval, decided_at_ns, created_at_ns)
  SELECT id, desk_id, source, suffix, argv, timeout_secs,
         fingerprint, 'ALWAYS_ALLOW', created_at_ns, created_at_ns
    FROM code_snapshots;
DROP TABLE code_snapshots;
ALTER TABLE code_snapshots_6 RENAME TO code_snapshots;

-- `trading_actions` gains the same two columns, backfilled the same way; the
-- primary key, the `id` uniqueness, and the TRIGGER check are migration 3's.
CREATE TABLE trading_actions_6 (
  desk_id       TEXT NOT NULL REFERENCES desks(id),
  action_id     TEXT NOT NULL,                    -- caller-supplied (R1-8)
  id            TEXT NOT NULL UNIQUE,             -- lowercase UUIDv7
  kind          TEXT NOT NULL CHECK (kind IN ('SUBMIT','CANCEL')),
  source        TEXT NOT NULL CHECK (source IN ('SESSION','TRIGGER')),
  trigger_id    TEXT REFERENCES triggers(id),
  firing_id     TEXT REFERENCES firings(id),
  request       TEXT NOT NULL,
  outcome       TEXT,                             -- response record, set when answered
  approval      TEXT NOT NULL CHECK (approval IN ('ALWAYS_ALLOW','PENDING','APPROVED','DENIED')),
  decided_at_ns INTEGER,                          -- null exactly while PENDING
  created_at_ns INTEGER NOT NULL,
  PRIMARY KEY (desk_id, action_id),
  CHECK ((source = 'TRIGGER') = (firing_id IS NOT NULL)),
  CHECK ((approval = 'PENDING') = (decided_at_ns IS NULL))
) STRICT;

INSERT INTO trading_actions_6
    (desk_id, action_id, id, kind, source, trigger_id, firing_id, request, outcome,
     approval, decided_at_ns, created_at_ns)
  SELECT desk_id, action_id, id, kind, source, trigger_id, firing_id, request, outcome,
         'ALWAYS_ALLOW', created_at_ns, created_at_ns
    FROM trading_actions;
DROP TABLE trading_actions;
ALTER TABLE trading_actions_6 RENAME TO trading_actions;

-- `operational_events.kind` gains the three R5 kinds: migration 5's block with
-- the new words appended, every row carried over, and the tail index recreated.
CREATE TABLE operational_events_6 (
  id             TEXT NOT NULL PRIMARY KEY,           -- lowercase UUIDv7
  kind           TEXT NOT NULL CHECK (kind IN
                   ('RECOVERY','DESK_CREATED','DESK_READY','DESK_FAILED','DESK_RETRIED',
                    'TRADING_NODE_STARTED','TRADING_NODE_FAILED','TRIGGER_MISSED',
                    'RUNTIME_DISCOVERED','RUNTIME_UNAVAILABLE','CONTROL_PLANE_STARTED',
                    'CONTROL_PLANE_LOST','SESSION_STARTED','SESSION_READY',
                    'SESSION_POINTER_CHANGED','SESSION_ATTENTION','SESSION_TURN_ENDED',
                    'SESSION_INTERRUPTED','SESSION_EXITED','PROMPT_DELIVERED',
                    'PROMPT_FAILED','RUNTIME_SWITCHED',
                    'MEMORY_CONFIGURED','MEMORY_STARTED','MEMORY_LOST','MEMORY_UNAVAILABLE',
                    'MEMORY_RETAINED','MEMORY_RECALLED',
                    'POLICY_CHANGED','APPROVAL_REQUESTED','APPROVAL_DECIDED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_6 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_6 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
