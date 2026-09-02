# R2 — Scheduled triggers: Feature SPEC

Refines root [`SPEC.md`](../../SPEC.md) §8, §9, §10, §11.1, §12.3, §13.2, §15, and §17 for [Milestone R2](../../ROADMAP.md#milestone-r2--scheduled-triggers); decisions in [`DECISIONS.md`](DECISIONS.md) (R2-1…R2-8). Everything here is desk-scoped by the desk UUID, English-only, and byte-identical under both locales (root §4.5).

## 1. Workspace additions

New exact pins in the workspace manifest (verified 2026-09-02): `rrule =0.14.0` (default features off; requires `chrono ^0.4.39` and `chrono-tz ^0.10.1`, both already pinned), `process-wrap =9.1.0` (already in the graph under `rmcp =3.2.0`; features `tokio1`, `process-session`, `job-object`), and `ring =0.17.14` (already in the graph under rustls; the fingerprint's SHA-256). The `tokio` pin gains the `process` feature. `rrule` is the only crate new to the graph; the other two are already compiled into it.

New daemon modules: `schedule` (§2, §3), `trigger` (§7, §8's handlers' logic), `exec` (§4, §5). The `marketrig` library gains the attribution headers (§6) and the CLI two groups (§9); `marketrig-mcp` changes nothing and inherits §6 through the shared client. `marketrig-acceptance` gains the `trigger-code` binary (§10.1).

## 2. Schedules and recurrence (R2-1)

A schedule is one of two shapes, given on creation and on reschedule:

```json
{ "at": "2026-09-03T14:00:00Z" }
{ "rrule": "FREQ=DAILY;BYHOUR=9;BYMINUTE=30", "dtstart": "2026-09-03T09:30:00", "tz": "America/New_York" }
```

- `at` is RFC 3339 with an offset; the daemon stores its UTC instant as `at_ns` and reports only that. It must be strictly after now at creation and reschedule.
- `rrule` is the value after `RRULE:` — one rule. `dtstart` is naive local wall clock `YYYY-MM-DDTHH:MM:SS`. `tz` is an IANA name `chrono-tz` knows.
- Rejected with `TRIGGER_INVALID`: `FREQ=SECONDLY`; `COUNT`; `UNTIL`; `BYSECOND`; a line break or any of `RRULE:`, `DTSTART`, `RDATE`, `EXDATE`, `EXRULE` in the rule text; a rule `rrule` cannot parse or validate against `dtstart`; an unknown `tz`; an unparseable `dtstart`; a schedule with neither shape, both, or extra keys.

**Candidates.** The daemon builds an `RRuleSet` whose `DTSTART` is `dtstart`'s naive fields stamped `Tz::UTC`, iterates it, and treats every produced instant's naive fields as a wall-clock candidate in `tz`, resolved with `Tz::from_local_datetime`: `None` → skipped; `Ambiguous(earlier, later)` → `earlier`; `Single(t)` → `t`. The candidate instant is `t` as Unix nanoseconds. The recurrence crate never sees the zone (R2-1).

**Projection.** `next_after(reference_ns)` is the first candidate whose instant is strictly greater than `reference_ns` — for a one-off, `at_ns` if greater, else none. Creation, enable, reschedule, and a miss use `now`; acceptance uses the accepted occurrence (§3). Iteration is bounded at 100,000 candidates per projection; a rule that yields none within that bound projects `NULL` and is treated as consumed. Candidates are compared as instants, so two wall-clock candidates resolving to the same instant (impossible under the earlier-of-two rule) cannot double-fire.

**DST, stated as scenarios** (all `America/New_York`):

| `dtstart` | rule | candidate wall clock | result |
| --- | --- | --- | --- |
| `2026-03-07T02:30:00` | `FREQ=DAILY` | `2026-03-08T02:30:00` (gap) | skipped; next is `2026-03-09T02:30:00 EDT` |
| `2026-10-31T01:30:00` | `FREQ=DAILY` | `2026-11-01T01:30:00` (twice) | `2026-11-01T01:30:00 EDT`, the earlier |
| `2026-09-03T09:30:00` | `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR` | weekdays 09:30 local | UTC offset shifts across 2026-11-01 and the wall clock does not |

## 3. The scheduler (R2-2)

### 3.1 Eligibility and the task

A trigger is **eligible** when it is undeleted, enabled, and carries a non-null `next_occurrence_ns`; the partial index `triggers_due` (§7) is exactly that set, and everything ineligible carries `NULL` by construction: disabled, deleted, consumed one-offs, elapsed one-offs after enable, and rules with no further candidate.

One tokio task, started after the daemon becomes discoverable (root §4.6 step 8) and stopped at shutdown:

```text
loop:
  deadline = min(earliest eligible next_occurrence_ns, now + 60 s)
  wait until deadline, or until the wake Notify fires
  run the acceptance unit (§3.2)
  if any accepted firing carries code: wake the executor (§4.3)
```

The `Notify` lives in the API state; every trigger create, patch, enable, disable, and delete signals it after its unit commits. The task holds no armed occurrence in memory (root §10).

### 3.2 The acceptance unit

One `BEGIN IMMEDIATE` unit reads every eligible trigger with `next_occurrence_ns <= now` and, for each, in `(next_occurrence_ns, id)` order:

- **accept** when `next_occurrence_ns >= daemon.started_at_ns` and `now - next_occurrence_ns <= 60 s`: insert the firing (`id`, `desk_id`, `trigger_id`, `occurrence_ns = next_occurrence_ns`, `accepted_at_ns = now`, `trigger_revision`, `brief`, `context`, `code_snapshot_id`); when `code_snapshot_id` is `NULL`, insert the `TRIGGER_RESULT` prompt (§5) in the same statement group; then set the projection to `NULL` for a one-off or to `next_after(occurrence_ns)` for a recurring rule;
- **miss** otherwise: append `TRIGGER_MISSED` (§7) with `missed_from_ns = next_occurrence_ns`, `missed_through_ns = now`, `count` = the number of candidates in `[missed_from_ns, now]` (one for a one-off; capped at 10,000 with `count_capped: true`), and `next_occurrence_ns` = the new projection; then set the projection to `NULL` for a one-off or to `next_after(now)` for a recurring rule.

A firing insert that violates `UNIQUE (desk_id, trigger_id, occurrence_ns)` means a concurrent or repeated wake already accepted the occurrence: the unit skips that trigger untouched and continues. Nothing in the unit reads wall time twice — `now` is taken once at entry.

### 3.3 Transaction scenarios

| Scenario | Outcome |
| --- | --- |
| one-off due 2 s ago, daemon started 1 min ago | accepted; projection `NULL`; one firing |
| one-off due during downtime, daemon started after it | missed; one `TRIGGER_MISSED` with `count 1`; projection `NULL`; no firing; `enable` afterwards leaves it `NULL` |
| recurring every minute, daemon down 3 minutes | one `TRIGGER_MISSED` with `count 3` and the range; projection = first candidate after now; anchor unchanged |
| recurring, clock jumps forward 2 hours | one `TRIGGER_MISSED` for the range; no catch-up firing |
| recurring, accepted 30 s late | accepted; projection = next candidate after the occurrence even when that is already past — the next pass misses or accepts it by the same rules |
| disabled for a day, then enabled | no miss record; projection = first candidate after now |
| two wakes race on the same occurrence | one firing; the loser's insert violates the unique index and skips |
| trigger deleted while due | ineligible; nothing |

## 4. Code snapshots and execution (R2-3, R2-4)

### 4.1 Snapshot

Given as `code` on trigger creation or patch:

```json
{ "source": "…UTF-8…", "suffix": ".py", "argv": ["python3", "{script}"], "timeout_secs": 300 }
```

- `source`: 1–262,144 bytes of UTF-8.
- `suffix`: empty, or `.` followed by 1–15 characters of `[A-Za-z0-9]`. Default empty.
- `argv`: 1–64 strings, each 1–4,096 bytes, exactly one equal to `{script}` as a whole argument. Default `["{script}"]`.
- `timeout_secs`: integer 1–3,600. Default 300.
- `fingerprint`: lowercase hex SHA-256 of `source`, `\0`, `suffix`, `\0`, the argv as a JSON array, `\0`, the timeout in decimal.

Violations answer `TRIGGER_INVALID`. A snapshot row is immutable; a patch with `code` inserts a new row and repoints the trigger, and `"code": null` detaches it. Under R2's fixed **Always allow**, `approved_at_ns = created_at_ns`. `GET` of a trigger returns the snapshot including `source`; the listing omits `source` and reports `source_bytes`.

### 4.2 The firing document

Version 1, one UTF-8 JSON object on the child's standard input, then EOF:

```json
{ "version": 1,
  "firing": { "id": "…", "occurrence_ns": 0, "accepted_at_ns": 0 },
  "trigger": { "id": "…", "name": "…", "revision": 1, "recurrence": "ONE_OFF|RECURRING" },
  "desk": { "id": "…", "name": "…", "workspace_path": "…" },
  "brief": "…", "context": "…" | null,
  "code_snapshot_id": "…" }
```

Environment: the daemon's own environment as inherited from the operator, plus `MARKETRIG_DESK_ID`, `MARKETRIG_DESK_NAME`, `MARKETRIG_TRIGGER_ID`, `MARKETRIG_FIRING_ID`. The `MARKETRIG_TEST_*` seam variables pass through with the rest, which is how the desk-scoped CLI and adapter a script spawns discover the same daemon under the harness (root §17). The daemon holds no private entries in R2; when a later milestone adds one it is removed here.

### 4.3 The executor

One task, woken by the scheduler after an accepted code-bearing firing, by every completion, and at start. Its claim unit: for each desk that has no `RUNNING` execution, take the oldest firing by `(accepted_at_ns, id)` that carries a `code_snapshot_id` and has no `executions` row, and insert `executions (firing_id, desk_id, daemon_uuid, state = 'RUNNING', started_at_ns)`. Each claim spawns one run:

```text
1. write source to <data root>/runtime/scripts/<firing_id><suffix>   (0700 on macOS)
2. argv with {script} replaced by that absolute path; cwd = desk workspace_path
3. spawn through the containment primitive (§4.5); failure -> SPAWN_FAILED
4. write the firing document to stdin, close stdin
5. read stdout and stderr concurrently; stdout > 1 MiB or stderr > 256 KiB -> terminate, OUTPUT_LIMIT
6. wait, bounded by timeout_secs -> terminate, TIMED_OUT
7. persist (§4.4), remove the script file best effort, re-run the claim unit
```

`executable` records `argv[0]` as resolved by the platform launch, or as given when resolution is opaque. The first 1 MiB / 256 KiB captured are kept on every outcome, including the terminated ones, with `stdout_truncated` / `stderr_truncated` marking a cut.

### 4.4 Outcomes, recovery, and shutdown

Completion is one unit: `UPDATE executions` to `state = 'COMPLETE'` with `outcome`, `exit_code`, `error`, `executable`, `stdout`, `stderr`, the truncation flags, and `finished_at_ns`, plus `INSERT prompts` (§5). Exactly one of these happens per execution row:

| Outcome | When | `exit_code` | `error` |
| --- | --- | --- | --- |
| `EXITED` | the process ended by itself | the code, or `NULL` on a signal | the signal name when signalled |
| `TIMED_OUT` | `timeout_secs` elapsed | `NULL` | — |
| `OUTPUT_LIMIT` | a stream cap was exceeded | `NULL` | which stream |
| `SPAWN_FAILED` | the launch failed | `NULL` | the OS error |
| `QUIT` | the daemon stopped while the code ran | `NULL` | — |
| `DAEMON_LOST` | recovery found it `RUNNING` under a dead daemon | `NULL` | the dead daemon's UUID |

Recovery (root §15) registers one step after reaping: every `executions` row `RUNNING` with `daemon_uuid <> this daemon` completes as `DAEMON_LOST` with its prompt, inside the recovery transaction, and the `RECOVERY` payload lists them under `executions_lost` (`firing_id`, `desk_id`, `daemon_uuid`). Firings with a snapshot and no execution row stay pending and are claimed by the new daemon in FIFO order (R2-4). Shutdown (root §4.6): the executor terminates every running group, persists `QUIT` for each, and both tasks stop inside the 5-second bound; the scheduler's in-flight unit, if any, finishes first because store units are synchronous.

### 4.5 Containment

`exec::spawn(command) -> Contained` is the one primitive: on macOS it wraps the tokio command in `process-wrap`'s `ProcessSession` — `setsid` before exec, so the child leads a new session and process group — and `terminate` sends `SIGKILL` to the group; on Windows it wraps in `JobObject` with kill-on-close, and `terminate` ends the job. Both platforms expose the same handle (`id`, piped stdin/stdout/stderr, `wait`, `terminate`) and nothing else, so R3's app-server and R4's memory child reuse it unchanged (per D73). Trigger scripts are not recorded in `runtime/children.json` (root §15); on macOS a script outlives a hard-killed daemon in its own session and is resolved by recovery as `DAEMON_LOST`, never resumed.

## 5. Results and the queued prompt (R2-5)

`prompts.kind` gains `TRIGGER_RESULT`, born `QUEUED` like `EVALUATION` and left there until R3. Payload:

```json
{ "kind": "TRIGGER_RESULT",
  "trigger_id": "…", "trigger_name": "…",
  "firing_id": "…", "occurrence_ns": 0, "accepted_at_ns": 0,
  "brief": "…", "context": "…" | null,
  "execution": null | { "outcome": "EXITED", "exit_code": 0, "error": null,
                        "stdout_bytes": 0, "stderr_bytes": 0,
                        "stdout_truncated": false, "stderr_truncated": false,
                        "started_at_ns": 0, "finished_at_ns": 0 } }
```

`execution: null` is a code-free firing, whose prompt commits in the acceptance unit (§3.2). The captured streams are read from the firing route (§8), rendered as lossy UTF-8 with the byte counts beside them.

## 6. Attribution (R2-6)

`marketrig::client::Endpoint` sends `X-MarketRig-Trigger-Id` and `X-MarketRig-Firing-Id` on every request when both `MARKETRIG_TRIGGER_ID` and `MARKETRIG_FIRING_ID` are set, and neither header otherwise. `POST /desks/{desk_id}/orders` and the cancel route:

- no headers → `source: SESSION`;
- both headers, and a `firings` row with that id, that `desk_id`, and that `trigger_id` exists → `source: TRIGGER`, `trigger_id`, `firing_id` on the `trading_actions` row, before the sandbox sees the order (root §12.3);
- otherwise → `ATTRIBUTION_INVALID` (400), no row.

The ActionRecord (R1 §7) gains `source` always and `trigger_id`, `firing_id` when `TRIGGER`; the history orders projection is unchanged. A replayed `action_id` returns the stored record whatever the caller's current attribution.

## 7. Durable schema (migration 3)

Conventions of root §15 throughout; every row desk-scoped:

```sql
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
```

Rebuilds, each by the migration-2 pattern (create, copy, drop, rename, recreate indexes):

- `prompts.kind` → `('EVALUATION','TRIGGER_RESULT')`;
- `trading_actions.source` → `('SESSION','TRIGGER')`, plus `trigger_id TEXT REFERENCES triggers(id)` and `firing_id TEXT REFERENCES firings(id)` with `CHECK ((source = 'TRIGGER') = (firing_id IS NOT NULL))`;
- `operational_events.kind` gains `TRIGGER_MISSED`, desk-scoped, payload `{ "trigger_id", "name", "recurrence", "missed_from_ns", "missed_through_ns", "count", "count_capped", "next_occurrence_ns" }`; the `RECOVERY` payload gains `executions_lost` (§4.4).

## 8. REST surface

All routes behind the R0 bearer and envelope. Trigger and firing routes are daemon-local — SQLite only, no node — and answer in any desk state except that creation requires `READY` (`DESK_NOT_READY`).

| Route | Success | Purpose |
| --- | --- | --- |
| `POST /desks/{desk_id}/triggers` | `201` Trigger | create (§2, §4.1) |
| `GET /desks/{desk_id}/triggers` | `200` `{"triggers":[…]}` | undeleted, creation order, snapshot without `source` |
| `GET /desks/{desk_id}/triggers/{trigger_id}` | `200` Trigger | one, deleted included, snapshot with `source` |
| `PATCH /desks/{desk_id}/triggers/{trigger_id}` | `200` Trigger | partial update |
| `DELETE /desks/{desk_id}/triggers/{trigger_id}` | `200` Trigger | soft delete |
| `GET /desks/{desk_id}/triggers/{trigger_id}/firings` | `200` `{"firings":[…]}` | newest first, execution summary |
| `GET /desks/{desk_id}/firings/{firing_id}` | `200` Firing | one, execution with streams |
| `GET /desks/{desk_id}/prompts` | `200` `{"prompts":[…]}` | newest first, no payload |
| `GET /desks/{desk_id}/prompts/{prompt_id}` | `200` Prompt | one, with payload |

**Create body:** `{ "name", "brief", "context"?, "schedule", "code"? }`. `name` follows the desk-name grammar; `brief` is 1–16,384 bytes; `context` up to 65,536 bytes or absent. **Patch body:** any subset of `brief`, `context` (`null` clears), `schedule`, `enabled`, `code` (`null` detaches); an empty object answers `TRIGGER_INVALID`. Every patch bumps `revision` and `updated_at_ns`, recomputes the projection (§2, and `enabled: false` sets it `NULL`), and signals the scheduler. The Trigger resource:

```json
{ "id": "…", "desk_id": "…", "name": "…", "source": "SCHEDULED", "recurrence": "ONE_OFF",
  "brief": "…", "context": "…",
  "schedule": { "at_ns": 0 } | { "rrule": "…", "dtstart": "…", "tz": "…" },
  "enabled": true, "revision": 1, "next_occurrence_ns": 0,
  "code": null | { "snapshot_id": "…", "suffix": "…", "argv": ["…"], "timeout_secs": 300,
                   "fingerprint": "…", "approved_at_ns": 0, "source_bytes": 0, "source": "…" },
  "created_at_ns": 0, "updated_at_ns": 0, "deleted_at_ns": 0 }
```

Nullable fields are omitted when null, as R0's resources do. A **Firing** is its row plus `execution: null | { "state", "daemon_uuid", "outcome", "exit_code", "error", "executable", "stdout", "stderr", "stdout_bytes", "stderr_bytes", "stdout_truncated", "stderr_truncated", "started_at_ns", "finished_at_ns" }`; the per-trigger listing carries the same object without `stdout` and `stderr`. A **Prompt** is `{ "id", "desk_id", "kind", "state", "created_at_ns" }` in the listing and adds `payload` on the single read (root §11.1).

New codes (append-only): `TRIGGER_INVALID` 400, `TRIGGER_NAME_TAKEN` 409, `TRIGGER_NOT_FOUND` 404, `FIRING_NOT_FOUND` 404, `PROMPT_NOT_FOUND` 404, `ATTRIBUTION_INVALID` 400. `DESK_NOT_FOUND` and `DESK_NOT_READY` keep their R0/R1 meanings.

## 9. CLI (R2-7)

Two groups, the R0 grammar's siblings — global flags first, desk name-or-id resolved through `GET /desks`, trigger name-or-id resolved through the desk's trigger listing, exit codes unchanged:

```text
marketrig [--json] trigger create <desk> --name <name> --brief <text> [--context <text>]
    (--at <rfc3339> | --rrule <rule> --dtstart <local> --tz <iana>)
    [--code <file> [--suffix <s>] [--arg <a>]... [--timeout <secs>]]
marketrig [--json] trigger list <desk>
marketrig [--json] trigger show <desk> <trigger>
marketrig [--json] trigger update <desk> <trigger> [--brief <text>] [--context <text>|--no-context]
    [--at … | --rrule … --dtstart … --tz …] [--code <file> [--suffix] [--arg]... [--timeout] | --no-code]
marketrig [--json] trigger enable|disable|delete <desk> <trigger>
marketrig [--json] trigger firings <desk> <trigger>
marketrig [--json] trigger firing <desk> <firing-id>
marketrig [--json] prompt list <desk>
marketrig [--json] prompt show <desk> <prompt-id>
```

`--code` reads the file before the daemon is contacted; a file that is unreadable or not UTF-8 is a usage error (exit 2) because the CLI cannot carry it, and it outranks `DAEMON_UNREACHABLE`. `--suffix` defaults to the file's own extension (`.py` for `job.py`, empty when there is none); `--arg` repeats to form `argv` and defaults to `{script}` alone; `--timeout` defaults to the daemon's default by omission. Every other value passes through untouched; the daemon validates. Human output: listings as tab-separated rows (`triggers`: name, recurrence, enabled, next occurrence, id; `firings`: id, occurrence, accepted, outcome; `prompts`: id, kind, state, created), single resources as `field: value` lines in the resource's §8 key order (nulls omitted, nested objects and arrays as compact JSON, keys §8 does not name printed last); `--json` the route's body verbatim.

## 10. Acceptance

### 10.1 The gate's trigger code (R2-8)

`marketrig-acceptance` has a binary target `trigger-code`, built with the other binaries. The gate creates every code-bearing trigger with `argv = [<absolute path to trigger-code>, "{script}"]`, and the snapshot's `source` is one line the binary reads from the script file:

- `env` — print, as one JSON object, the four `MARKETRIG_*` identifiers, the current directory, and the firing document read from standard input; exit 0;
- `order <instrument_id> <side> <quantity>` — spawn `marketrig-mcp --desk $MARKETRIG_DESK_NAME` from beside its own executable, initialize as an MCP client, call `submit_order` twice with `action_id = $MARKETRIG_FIRING_ID`, print both results as JSON lines, exit 0;
- `exit <code>` — exit with that code after writing one line to standard error;
- `sleep <secs>` — sleep, then exit 0;
- `flood <bytes>` — write that many bytes to standard output, exit 0.

### 10.2 Gate scenarios (continuing R1's chain)

- **G21 — definitions.** `marketrig trigger create` a code-free one-off 2 s ahead and a recurring rule (`FREQ=MINUTELY`, `dtstart` chosen so the next candidate is ~2 s ahead, `tz` `UTC`); `list` and `show` carry the resources; the one-off fires: its firing row and its `TRIGGER_RESULT` prompt exist — asserted in one read-only SQLite query — and the projection is `NULL`; the recurring fires once and its projection advances by 60 s, then `delete` hides it from `list` while `show` by id still answers with `deleted_at_ns` and its firing stays readable; a second recurring rule whose candidate is 2 s ahead is `disable`d at once and after 4 s has neither a firing nor a miss; refusals: `FREQ=SECONDLY`, `COUNT=3`, a past `--at`, an unknown zone (`TRIGGER_INVALID`), the same name again (`TRIGGER_NAME_TAKEN`), an unknown trigger (`TRIGGER_NOT_FOUND`).
- **G22 — code fires with no agent alive and places an attributable, idempotent paper action.** An `order AAPL.XNAS BUY 1` one-off 2 s ahead: within bounds the execution completes `EXITED 0`; `trading_actions` holds exactly one row for the firing's `action_id` with `source TRIGGER`, `trigger_id`, and `firing_id`; the script's two calls answered `201` then `200` with the same record (its standard output); the live position is one lot; the `TRIGGER_RESULT` prompt references the firing and summarizes the execution; `marketrig trigger firing` shows the streams. No session existed at any point.
- **G23 — the environment and the document.** An `env` one-off: standard output carries the four identifiers matching the rows, the desk workspace as the current directory, and a version-1 document whose `brief` and `context` equal the trigger's at creation; a patch of the brief before the next firing of a recurring `env` trigger shows the new brief in the next document and the old one in the earlier firing row.
- **G24 — every outcome is one record and one prompt, and nothing reruns.** Four one-offs: `exit 3` → `EXITED 3` with the stderr line; `sleep 30` under `timeout_secs 1` → `TIMED_OUT` within a few seconds; `flood 2000000` → `OUTPUT_LIMIT` with `stdout_truncated`; an `argv[0]` that does not exist → `SPAWN_FAILED`. Each: exactly one `executions` row, exactly one prompt, projection `NULL`, and — after a 5 s wait — still one row.
- **G25 — misses across downtime.** A one-off 2 s ahead and a recurring every-minute rule whose next candidate is 2 s ahead; `POST /quit` at once; wait 4 s; restart: no new firing for either, one `TRIGGER_MISSED` each (`count 1`), the one-off's projection `NULL` and `enable` leaves it so, the recurring projection in the future and its anchor unchanged.
- **G26 — a restart mid-flight loses neither the firing nor the result.** A `sleep 30` one-off with `timeout_secs 60` fires and its execution is `RUNNING`; hard-kill the daemon; restart: the `RECOVERY` payload lists the execution under `executions_lost`, the row is `COMPLETE` / `DAEMON_LOST` naming the dead daemon, its prompt is queued, the firing row is intact, and no second execution appears.

G22 + G25 + G26 together are the roadmap's R2 evidence line.

### 10.3 Experiment scenario

- **E3 — a real session defines a trigger whose code trades.** Attended, real Yahoo, the selected runtime's session with the adapter registered as in E1/E2. The operator asks the agent to create, through `marketrig trigger create`, a one-off trigger due in about two minutes whose code places a paper order for one lot of a named instrument through `marketrig-mcp` (the printed adapter path) with the firing id as `action_id`. The harness waits for the firing, the execution, and a `trading_actions` row with `source TRIGGER` naming it, and reads the queued `TRIGGER_RESULT` prompt; the legs that wait on the agent end inconclusive (root §17).

## 11. Required checks

**Module checks** (`cargo test`, fakes allowed):

- `store::trigger_migration_applies` — empty database reaches `user_version` 3 with the §7 schema; migration-2 databases upgrade with their rows intact.
- `schedule::form_rejected` — every §2 rejection answers `TRIGGER_INVALID`; the accepted shapes parse.
- `schedule::dst_gap_skipped_overlap_earlier` — the §2 table.
- `schedule::projection_from_anchor` — `next_after` after acceptance, miss, enable, and creation; a one-off past its instant projects `NULL`.
- `schedule::accept_or_miss` — the §3.3 table against a fake clock and daemon start: firings, prompts, `TRIGGER_MISSED` payloads, projections.
- `schedule::duplicate_wake_no_second_firing` — the unique index guards a repeated unit.
- `schedule::wake_and_recheck` — a mutation wakes the task; an idle task sleeps at most 60 s.
- `trigger::fingerprint_stable` — the §4.1 hash over a fixed snapshot; any field change changes it.
- `exec::document_and_environment` — the child receives the version-1 document and the four identifiers, in the workspace.
- `exec::outcomes_one_record_one_prompt` — exit 0, nonzero, timeout, output limit, spawn failure: one `executions` row and one prompt each, in one unit, no rerun.
- `exec::group_terminated_on_timeout` — a script (`sh` on macOS, `powershell` on Windows) that spawns a grandchild and prints its pid; after `TIMED_OUT` the grandchild is gone.
- `exec::fifo_per_desk_concurrent_across_desks` — two firings on one desk run in acceptance order, one at a time; two desks run at once.
- `exec::recovery_marks_daemon_lost` — a `RUNNING` row under another daemon's UUID completes `DAEMON_LOST` with its prompt in the recovery transaction; a pending firing is then claimed.
- `api::trigger_codes` — every §8 error path answers the envelope with its documented code.
- `api::action_attribution` — headers → `source TRIGGER` and the ids on the row and record; a foreign or unknown firing, or one header alone, → `ATTRIBUTION_INVALID`; no headers → `SESSION`.
- `client::attribution_headers_from_env` — both variables set → both headers; either missing → neither.
- `cli::trigger_exit_codes` — the group's 0/1/2/3 mapping against a fake endpoint, including the non-UTF-8 code file.
- `cli::prompt_exit_codes` — the group's 0/1/3 mapping.

**Gate** (the same target, extended): G21–G26 in order after G20, on the stand-in feed, with the `trigger-code` binary, producing the evidence bundle.

**Experiment** (attended target): E3, once per platform-and-runtime cell.

**Static checks:** rustfmt, Clippy `-D warnings`, `cargo test` across the workspace on both MVP platforms in CI.
