-- Migration 5 — the R4 memory child, the provider settings, and the six memory
-- event kinds (feature SPEC `r4-memory-skills-loop` §6, per R4-5).
-- Forward-only; never edited.

CREATE TABLE memory_child (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  state TEXT NOT NULL CHECK (state IN ('UNCONFIGURED','AVAILABLE','UNAVAILABLE')),
  executable_path TEXT, validated_at_ns INTEGER, failure_code TEXT, failure_message TEXT,
  CHECK ((state = 'UNAVAILABLE') = (failure_code IS NOT NULL))
) STRICT;
INSERT INTO memory_child (id, state) VALUES (1, 'UNCONFIGURED');

CREATE TABLE memory_provider (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  base_url TEXT, llm_model TEXT, embedding_model TEXT,
  key_ref TEXT,                       -- opaque; 'marketrig/hindsight-provider' once a key is stored
  embedding_locked_at_ns INTEGER, updated_at_ns INTEGER NOT NULL
) STRICT;
INSERT INTO memory_provider (id, updated_at_ns) VALUES (1, 0);

-- `operational_events.kind` gains the six R4 kinds (R4-5): migration 4's block
-- with the new words appended, every row carried over, and the tail index
-- recreated.
CREATE TABLE operational_events_5 (
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
                    'MEMORY_RETAINED','MEMORY_RECALLED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_5 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_5 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
