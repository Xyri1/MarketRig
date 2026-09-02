-- Migration 4 — the R3 runtime, pointer, process, and delivery schema
-- (feature SPEC `r3-runtime-delivery` §8, per R3-7). Forward-only; never edited.

ALTER TABLE desks ADD COLUMN selected_runtime TEXT NOT NULL DEFAULT 'codex'
  CHECK (selected_runtime IN ('codex','claude'));

CREATE TABLE runtimes (
  runtime TEXT NOT NULL PRIMARY KEY CHECK (runtime IN ('codex','claude')),
  state TEXT NOT NULL CHECK (state IN ('UNDISCOVERED','AVAILABLE','UNAVAILABLE')),
  executable_path TEXT, version TEXT, validated_at_ns INTEGER,
  failure_code TEXT, failure_message TEXT
) STRICT;
INSERT INTO runtimes (runtime, state) VALUES ('codex','UNDISCOVERED'), ('claude','UNDISCOVERED');

CREATE TABLE native_sessions (
  desk_id TEXT NOT NULL REFERENCES desks(id),
  runtime TEXT NOT NULL CHECK (runtime IN ('codex','claude')),
  native_session_id TEXT NOT NULL, updated_at_ns INTEGER NOT NULL,
  PRIMARY KEY (desk_id, runtime)
) STRICT;

CREATE TABLE agent_processes (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  runtime TEXT NOT NULL CHECK (runtime IN ('codex','claude')),
  native_session_id TEXT, pid INTEGER NOT NULL, daemon_uuid TEXT NOT NULL,
  started_at_ns INTEGER NOT NULL, ready_at_ns INTEGER, ended_at_ns INTEGER,
  exit_reason TEXT CHECK (exit_reason IN ('EXITED','INTERRUPTED','QUIT','CONTROL_PLANE_LOST','DAEMON_LOST')),
  exit_code INTEGER,
  CHECK ((ended_at_ns IS NULL) = (exit_reason IS NULL))
) STRICT;
CREATE UNIQUE INDEX agent_processes_live ON agent_processes (desk_id) WHERE ended_at_ns IS NULL;

-- `prompts` gains the ORIENTATION and DISCLOSURE kinds, the delivery states,
-- and the attempt columns. SQLite cannot alter a CHECK, so the table is rebuilt
-- by migration 3's pattern. Every existing row is carried over; migration 3's
-- only state was `QUEUED`, which is the new vocabulary's `QUEUED` unchanged,
-- and the attempt columns arrive NULL.
CREATE TABLE prompts_4 (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  kind TEXT NOT NULL CHECK (kind IN ('EVALUATION','TRIGGER_RESULT','ORIENTATION','DISCLOSURE')),
  state TEXT NOT NULL CHECK (state IN ('QUEUED','DELIVERED','FAILED')),
  payload TEXT NOT NULL, created_at_ns INTEGER NOT NULL,
  attempted_at_ns INTEGER, resolved_at_ns INTEGER,
  runtime TEXT CHECK (runtime IN ('codex','claude')),
  native_session_id TEXT, failure_code TEXT, disclosed_at_ns INTEGER,
  CHECK ((state <> 'QUEUED') = (resolved_at_ns IS NOT NULL)),
  CHECK ((state = 'FAILED') = (failure_code IS NOT NULL))
) STRICT;

INSERT INTO prompts_4 (id, desk_id, kind, state, payload, created_at_ns)
  SELECT id, desk_id, kind, state, payload, created_at_ns FROM prompts;
DROP TABLE prompts;
ALTER TABLE prompts_4 RENAME TO prompts;

CREATE INDEX prompts_queue ON prompts (desk_id, created_at_ns) WHERE state = 'QUEUED';

-- `operational_events.kind` gains the fourteen R3 kinds (R3-7); the prior
-- vocabulary is carried over exactly, and the tail index is recreated.
CREATE TABLE operational_events_4 (
  id             TEXT NOT NULL PRIMARY KEY,           -- lowercase UUIDv7
  kind           TEXT NOT NULL CHECK (kind IN
                   ('RECOVERY','DESK_CREATED','DESK_READY','DESK_FAILED','DESK_RETRIED',
                    'TRADING_NODE_STARTED','TRADING_NODE_FAILED','TRIGGER_MISSED',
                    'RUNTIME_DISCOVERED','RUNTIME_UNAVAILABLE','CONTROL_PLANE_STARTED',
                    'CONTROL_PLANE_LOST','SESSION_STARTED','SESSION_READY',
                    'SESSION_POINTER_CHANGED','SESSION_ATTENTION','SESSION_TURN_ENDED',
                    'SESSION_INTERRUPTED','SESSION_EXITED','PROMPT_DELIVERED',
                    'PROMPT_FAILED','RUNTIME_SWITCHED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_4 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_4 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
