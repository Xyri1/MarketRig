-- Migration 7 — OpenViking replaces Hindsight (feature SPEC
-- `openviking-continuity` §6, per OV-6): the child row goes, the setup row
-- arrives, and the event vocabulary loses the six memory kinds and gains the
-- eight OpenViking ones. Nothing reads, migrates, or preserves Hindsight data.
-- Forward-only; never edited.

DROP TABLE memory_child;

-- The one installation row (§1.1). `UNAVAILABLE` is a child failure on a
-- provisioned environment; `UNCONFIGURED` is no environment, and it carries
-- PROVISION_FAILED when provisioning is what failed, so a state/failure_code
-- invariant would be wrong here.
CREATE TABLE openviking_setup (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  state TEXT NOT NULL
    CHECK (state IN ('UNCONFIGURED','PROVISIONING','AVAILABLE','UNAVAILABLE')),
  python_path TEXT, python_version TEXT, node_path TEXT, node_version TEXT,
  venv_path TEXT, provisioned_at_ns INTEGER, failure_code TEXT, failure_message TEXT
) STRICT;
INSERT INTO openviking_setup (id, state) VALUES (1, 'UNCONFIGURED');

-- `memory_provider` keeps its columns; `embedding_locked_at_ns` is ignored from
-- here on and dropped by a later rebuild (§6).

-- Hindsight's own history goes with it: no compatibility is owed to those rows,
-- and the rebuilt vocabulary below would refuse them.
DELETE FROM operational_events WHERE kind LIKE 'MEMORY_%';

-- `operational_events.kind` is migration 6's block without the six MEMORY_
-- words and with the eight OpenViking ones, every row carried over, and the
-- tail index recreated.
CREATE TABLE operational_events_7 (
  id             TEXT NOT NULL PRIMARY KEY,           -- lowercase UUIDv7
  kind           TEXT NOT NULL CHECK (kind IN
                   ('RECOVERY','DESK_CREATED','DESK_READY','DESK_FAILED','DESK_RETRIED',
                    'TRADING_NODE_STARTED','TRADING_NODE_FAILED','TRIGGER_MISSED',
                    'RUNTIME_DISCOVERED','RUNTIME_UNAVAILABLE','CONTROL_PLANE_STARTED',
                    'CONTROL_PLANE_LOST','SESSION_STARTED','SESSION_READY',
                    'SESSION_POINTER_CHANGED','SESSION_ATTENTION','SESSION_TURN_ENDED',
                    'SESSION_INTERRUPTED','SESSION_EXITED','PROMPT_DELIVERED',
                    'PROMPT_FAILED','RUNTIME_SWITCHED',
                    'POLICY_CHANGED','APPROVAL_REQUESTED','APPROVAL_DECIDED',
                    'OPENVIKING_CONFIGURED','OPENVIKING_PROVISIONED','OPENVIKING_STARTED',
                    'OPENVIKING_LOST','OPENVIKING_UNAVAILABLE','DESK_MEMORY_PROVISIONED',
                    'SKILLS_PROJECTED','SKILLS_PROJECTION_FAILED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_7 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_7 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
