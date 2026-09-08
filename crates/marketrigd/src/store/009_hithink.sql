-- Migration 9 — the HiThink provider (feature SPEC `hithink-a-share` §1.1, per
-- HT-1): one installation row holding the provider's state and the operator's
-- A-share feed choice, and one more event kind for its changes. The key itself
-- is in the credential store, never here.
-- Forward-only; never edited.

CREATE TABLE hithink_provider (
  id              INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  state           TEXT NOT NULL
                    CHECK (state IN ('UNCONFIGURED','AVAILABLE','UNAVAILABLE')),
  -- Which client serves the CN catalog entries. Without a key there is nothing
  -- to serve them with, so `UNCONFIGURED` pins it to Yahoo.
  a_share_feed    TEXT NOT NULL CHECK (a_share_feed IN ('YAHOO','HITHINK')),
  validated_at_ns INTEGER,
  failure_code    TEXT,
  failure_message TEXT,
  updated_at_ns   INTEGER NOT NULL,
  CHECK (state <> 'UNCONFIGURED' OR a_share_feed = 'YAHOO')
) STRICT;
INSERT INTO hithink_provider (id, state, a_share_feed, updated_at_ns)
  VALUES (1, 'UNCONFIGURED', 'YAHOO', 0);

-- `operational_events.kind` is migration 7's block plus HITHINK_PROVIDER_CHANGED,
-- every row carried over, and the tail index recreated.
CREATE TABLE operational_events_9 (
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
                    'SKILLS_PROJECTED','SKILLS_PROJECTION_FAILED',
                    'HITHINK_PROVIDER_CHANGED')),
  desk_id        TEXT REFERENCES desks(id),           -- NULL for installation-wide kinds
  occurred_at_ns INTEGER NOT NULL,
  payload        TEXT NOT NULL DEFAULT '{}'           -- English-only JSON object
) STRICT;

INSERT INTO operational_events_9 (id, kind, desk_id, occurred_at_ns, payload)
  SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events;
DROP TABLE operational_events;
ALTER TABLE operational_events_9 RENAME TO operational_events;

CREATE INDEX operational_events_tail ON operational_events (occurred_at_ns, id);
