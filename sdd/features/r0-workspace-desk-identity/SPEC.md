# R0 — Workspace, daemon, and desk identity: Feature SPEC

*Decision basis: per D7, D13, D15, D20, D42–D46, D48–D51, D53, D54, D60, D61, D73, D75, and R0-1…R0-8.*

This specification settles the mechanics Milestone R0 implements. It refines root [SPEC](../../SPEC.md) §3, §4.1, §4.3, §5, §13.2, §15, and §17 and contradicts nothing settled there. Mechanics R0 does not need — pagination, idempotency keys, attribution headers, WebSocket framing, OpenAPI emission wiring — stay **deferred** (root §18) and are not invented here.

## 1. Workspace and release boundary

The repository gains the root Cargo workspace manifest and `Cargo.lock`, with members:

- `crates/marketrigd` — the daemon: a library crate plus a thin `main.rs` binary;
- `crates/marketrig` — the CLI;
- the acceptance harness crate (per R0-7; exact name at plan time).

Toolchain rustc ≥ 1.98, edition 2024, one workspace version shared by every member (per D43, D53). `marketrig-mcp` and `src-tauri` join in their own milestones. rustfmt, Clippy `-D warnings`, and `cargo test` run over the whole workspace from day one (per D60, D61).

## 2. Data roots and the test seam

Two per-user roots exist (root §15, §4.4):

- **application-data root**: `%LOCALAPPDATA%\MarketRig` (Windows) / `~/Library/Application Support/MarketRig` (macOS), holding `marketrig.sqlite3` and `runtime/` (`daemon.lock`, `endpoint.json`, `children.json`);
- **desks home**: `~/.marketrig/desks/`, holding one workspace directory per desk;
- **log root**: the OS application-log directory (`~/Library/Logs/MarketRig`; `%LOCALAPPDATA%\MarketRig\logs`).

`MARKETRIG_TEST_DATA_ROOT=<dir>` relocates all three for both binaries: application data to `<dir>/data`, desks home to `<dir>/desks`, logs to `<dir>/logs`. Nothing outside `<dir>` is touched when the variable is set, which is what lands the whole root in an evidence bundle by construction (per D75). `MARKETRIG_TEST_NO_TRADING` is accepted and inert in R0 (no feed exists yet).

## 3. Durable store

### 3.1 Conventions in force

As root §15 states, from the first table: sole-writer `marketrigd`; one database thread owning the one connection, accepting submitted units and read functions, with no handle escaping it; explicit `BEGIN IMMEDIATE`; WAL; `STRICT` tables; enforced foreign keys; lowercase UUIDv7 text IDs; `*_ns` UTC Unix-nanosecond instants. No money column exists in R0.

### 3.2 Migrations

Per R0-3: the daemon embeds its ordered migration list; startup applies pending migrations before recovery and records the count in `PRAGMA user_version`. A database whose `user_version` exceeds the newest embedded migration stops startup with `DATABASE_NEWER`. R0 ships migration `1`.

### 3.3 R0 schema

```sql
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
```

Per R0-4 this is the one append-only evidence table; later milestones extend the `kind` CHECK by migration. Rows commit inside the transaction of the change they evidence. The `(occurred_at_ns, id)` index is the cursor order D71 tails in R5.

The `RECOVERY` payload names `previous_daemon_uuid` (the newest earlier `RECOVERY` row's `daemon_uuid`, `null` on first start), `daemon_uuid`, and a `children` array of reap outcomes (§4.4). Desk-lifecycle payloads carry at least `name`; `DESK_FAILED` adds `failure_code`.

## 4. Daemon lifecycle

### 4.1 Startup order

```text
1. resolve roots (test seam), create missing directories
2. acquire runtime/daemon.lock exclusively (R0-2); on failure exit nonzero: ALREADY_RUNNING
3. open SQLite, set WAL and foreign keys, apply migrations (§3.2)
4. mint the daemon UUID — recovery's own event names it (§3.3)
5. recovery transaction (§4.3)
6. complete every interrupted CREATING desk (§7.3)
7. bind 127.0.0.1:0, mint the per-start bearer credential
8. write runtime/endpoint.json atomically (§5.1) — the daemon is now discoverable
```

The bearer credential is 32 bytes from the OS CSPRNG, lowercase hex, and is minted at bind time because nothing before the listener needs it. The daemon UUID is a lowercase UUIDv7 and is minted before recovery because the `RECOVERY` payload carries it.

### 4.2 Shutdown

Per R0-6, `POST /quit` (and Ctrl+C on a terminal-attached daemon): stop accepting requests, drain the database thread, remove `endpoint.json`, exit 0; the lock releases with the process. Bounded end to end at 5 seconds, after which the daemon exits anyway. A hard kill leaves `endpoint.json` and the lock file behind; the lock is released by the OS and the stale file fails client verification (§5.2).

### 4.3 Recovery

One pre-service transaction on every start, clean or not (root §15): each module registers its step, steps run in one fixed order — child reaping first — and the transaction appends exactly one `RECOVERY` event. R0 registers two steps: reap recorded children (§4.4) and no-op placeholders for nothing else, since R0 has no in-flight prompts, firings, or actions.

### 4.4 `runtime/children.json` and reaping

Format (written by later milestones' launches; R0 defines and consumes it):

```json
{ "children": [ { "pid": 123, "kind": "…", "args": ["…"], "daemon_uuid": "…", "launched_at_ns": 0 } ] }
```

On macOS, recovery terminates a recorded child whose pid is alive **and** whose current command line still carries the recorded `args` (a shim may have replaced the executable path, per D73); a pid that is dead or whose command line no longer matches is left untouched. On Windows, records are discarded without a check. Either way every record is dropped and each outcome — `TERMINATED`, `NOT_RUNNING`, `PID_RECYCLED`, `DISCARDED` — lands in the `RECOVERY` payload.

## 5. Endpoint discovery and authentication

### 5.1 `runtime/endpoint.json`

```json
{ "port": 49152, "credential": "<64 hex>", "daemon_uuid": "<uuidv7>", "pid": 123, "started_at_ns": 0 }
```

Written atomically (temp file plus rename) after the listener is live; mode `0600` on macOS, per-user directory ACLs on Windows (root §4.3).

### 5.2 Verification

The file is a pointer, never proof of liveness (per R0-2). A client reads it, calls `GET /health` with the credential, and requires the response's `daemon_uuid` to equal the file's. Connection failure, `401`, or a UUID mismatch means no usable daemon; the CLI reports it and never spawns one (per R0-1).

## 6. REST surface

All routes require `Authorization: Bearer <credential>`; a missing or wrong credential answers `401 UNAUTHORIZED` with the envelope. R0 serves REST only; WebSocket arrives with the milestones that need it.

| Route | Success | Purpose |
| --- | --- | --- |
| `GET /health` | `200` `{"daemon_uuid","version","started_at_ns"}` | verification (§5.2) |
| `GET /desks` | `200` `{"desks":[Desk…]}` | list, creation order |
| `POST /desks` `{"name"}` | `201` `Desk` | create (§7) |
| `GET /desks/{desk_id}` | `200` `Desk` | one desk by UUID |
| `POST /desks/{desk_id}/retry` | `200` `Desk` | retry a `FAILED` creation (§7.4) |
| `POST /quit` | `202` `{}` then exit | shutdown (§4.2) |

The `Desk` resource:

```json
{ "id": "…", "name": "…", "state": "CREATING|READY|FAILED",
  "workspace_path": "…", "workspace_status": "OK|UNAVAILABLE",
  "workspace_status_reason": "…",
  "created_at_ns": 0, "ready_at_ns": 0, "failure_code": "…", "failure_message": "…" }
```

`workspace_status` is derived at read time (§7.5), never stored; nullable fields are omitted when null.

**Error envelope** (per R0-5), the one shape every later group inherits:

```json
{ "code": "SCREAMING_SNAKE", "message": "English sentence." }
```

R0 codes and statuses: `UNAUTHORIZED` 401, `DESK_NAME_INVALID` 400, `DESK_NAME_TAKEN` 409, `DESK_NOT_FOUND` 404, `DESK_STATE_INVALID` 409 (retry on a non-`FAILED` desk), `INTERNAL` 500. Codes are append-only across milestones; messages may improve, codes never change meaning.

## 7. Desk creation, retry, and validation

### 7.1 Name grammar

1–40 characters, lowercase `a–z`, `0–9`, and single interior hyphens: no leading, trailing, or consecutive hyphen. Anything else is `DESK_NAME_INVALID`. The name is immutable and unique for the installation's lifetime (per D15); R0 has no desk deletion, so uniqueness has no tombstone question.

### 7.2 Creation sequence (per D20)

```text
1. one unit: insert desks row (CREATING) + DESK_CREATED event; commit
2. bootstrap the workspace idempotently:
   - create <desks home>/<name>/
   - write AGENTS.md only if absent (seed: §7.6)
   - write CLAUDE.md whenever absent or different: exactly "@AGENTS.md\n"
3. one unit: state -> READY, ready_at_ns set + DESK_READY event; commit
```

Any bootstrap failure lands in one unit as `FAILED` with `failure_code`/`failure_message` and a `DESK_FAILED` event, preserving the row and partial workspace. `POST /desks` answers only after step 3 or the failure — creation is synchronous; a client never observes `CREATING` except after a crash.

### 7.3 Startup completion

Before serving, startup re-runs step 2–3 for every `CREATING` row found (root §5.2). Bootstrap is idempotent, so a half-written workspace completes; a completion failure follows the same `FAILED` path.

### 7.4 Retry

`POST /desks/{desk_id}/retry` on a `FAILED` desk clears the failure fields, returns the row to `CREATING` with a `DESK_RETRIED` event, and re-runs §7.2 on the same UUID, name, and path. On any other state it answers `DESK_STATE_INVALID`.

### 7.5 `READY` validation

For a `READY` desk, `workspace_status` is computed at read time: `OK` when the workspace directory exists and `AGENTS.md` is readable, otherwise `UNAVAILABLE` with a one-line reason. The durable row stays `READY`; nothing is recreated or rewritten (per D20) except the `CLAUDE.md` shim, which MarketRig owns and reconciles. An unavailable workspace blocks neither other desks nor startup.

### 7.6 Seeds

The R0 `AGENTS.md` seed is exactly:

```markdown
# <desk-name>

This desk's constitution. MarketRig seeded it at desk creation and never
rewrites it; its full content arrives with later MarketRig milestones.
```

The constitution's real wording is deferred (root §18) and grows with the milestones that add the surfaces it names; this placeholder is R0's minimal seed per the feature PRD. `.agents/skills/` and the `.claude/skills` link are not created in R0 (per D21, R4).

## 8. CLI

Grammar: global flags precede the group (root §13.2) — `marketrig [--json] desk <create|list|show|retry> …`. R0 ships exactly the `desk` group:

- `marketrig desk create <name>`
- `marketrig desk list`
- `marketrig desk show <name-or-id>`
- `marketrig desk retry <name-or-id>`

`show` and `retry` accept a name or UUID; the CLI resolves a name through `GET /desks` and never derives a desk any other way. Human output is plain UTF-8 text; `--json` emits the daemon's resource verbatim. On a daemon error the CLI prints `error: <CODE>: <message>` to standard error (or the envelope itself on standard output under `--json`).

Discovery follows §5.2 against the resolved data root. HTTP is blocking with a 2-second connect and 10-second total timeout, redirects disabled, no environment proxy (`proxy(None)` — mandatory per R0-8, D50).

Exit codes (per R0-5), fixed for every future group: `0` success; `1` daemon-reported error; `2` usage error; `3` no usable daemon (connection, `401` against the discovered endpoint, or UUID mismatch — reported as `DAEMON_UNREACHABLE`).

## 9. Diagnostics

`marketrigd` writes JSON Lines through `tracing` to the log root: daily rotation, newest 7 files kept (`max_log_files`, per R0-8), plus standard error when it is a terminal. Log lines never contain the bearer credential or any secret (per D49, D51); the required check greps the whole log root for the live credential. Retention tuning beyond the file count stays deferred (root §18).

## 10. Acceptance: the R0 gate

The gate (per D75, R0-7) spawns the real `marketrigd` binary under `MARKETRIG_TEST_DATA_ROOT` pointing into the run's evidence directory and drives public surfaces only: `marketrig --json`, the loopback API, workspace files, and read-only SQLite. R0 has no attended scenario, so the experiment target exists and is empty until R1.

Evidence bundle: the run directory containing the relocated root (`data/`, `desks/`, `logs/`) plus the harness's own `observations.jsonl`. The harness deletes nothing.

Scenario chain (each names its check in §11):

- **G1 — first start.** Empty root → daemon healthy; one `RECOVERY` event with `previous_daemon_uuid: null`; `endpoint.json` valid and `0600` on macOS.
- **G2 — two isolated desks.** `desk create alpha-<stamp>`, `desk create beta-<stamp>` → both `READY`; two workspace directories, each with the exact §7.6 seed and the exact shim; rows and events as §3.3 and §7.2 specify.
- **G3 — refusals.** Duplicate name → `DESK_NAME_TAKEN`, exit 1; `Bad--Name` → `DESK_NAME_INVALID`; unknown UUID → `DESK_NOT_FOUND`.
- **G4 — clean restart.** `POST /quit` → process exits within bound, `endpoint.json` removed; start again → both desks intact with identical `id`, `name`, `created_at_ns`, workspaces untouched, prior events preserved, and a new `RECOVERY` event naming the first daemon's UUID as previous.
- **G5 — stale credential.** The first daemon's saved credential against the second daemon → `401`; the second daemon's endpoint file verifies.
- **G6 — hard kill.** SIGKILL/TerminateProcess the running daemon → lock is acquirable, restart succeeds, desks intact, `RECOVERY` appended; stale `endpoint.json` from the killed daemon failed verification before restart.
- **G7 — reaping.** Before a start, the harness plants `children.json` naming (a) a harness-spawned sleeper with matching recorded args and (b) a live process whose args do not match → after start, (a) is terminated, (b) survives, the file's records are gone, and the `RECOVERY` payload reports both outcomes. On Windows the scenario asserts discard-only.
- **G8 — failed creation and retry.** A plain file planted at the desk's workspace path → `desk create` yields `FAILED` with its event; obstruction removed → `desk retry` yields `READY` on the same UUID, name, and path, with `DESK_RETRIED` then `DESK_READY` events.
- **G9 — damaged READY workspace.** Delete one desk's `AGENTS.md` → its `workspace_status` is `UNAVAILABLE` with a reason while the row stays `READY`; the other desk reads `OK`; a daemon restart still serves both.
- **G10 — single instance.** A second daemon on the same root exits nonzero naming `ALREADY_RUNNING`; the first is undisturbed.
- **G11 — no daemon.** With no daemon running, `marketrig desk list` exits `3` with `DAEMON_UNREACHABLE`.

G2 + G4 together are the roadmap's R0 evidence line.

## 11. Required checks

The implementation PLAN turns each into a runnable test before R0 is done (root §17). Module checks may construct database and filesystem state directly; gate scenarios may not.

**Module checks** (`cargo test`, fakes allowed):

- `store::migrations_apply_and_stamp` — empty database migrates to `user_version` 1 with the §3.3 schema, STRICT and WAL verified by pragma.
- `store::newer_database_rejected` — `user_version` 2 stops startup with `DATABASE_NEWER`.
- `store::desk_row_checks` — the state/timestamp/failure CHECK constraints reject inconsistent rows.
- `desk::name_grammar` — the §7.1 accept and reject table.
- `desk::bootstrap_idempotent` — running §7.2 step 2 twice yields byte-identical files; a user-modified `AGENTS.md` survives; a drifted `CLAUDE.md` is reconciled.
- `desk::interrupted_creating_completes` — a `CREATING` row with a half-written workspace completes to `READY` through §7.3.
- `daemon::lock_excludes_second_start` — two startups on one root: second fails `ALREADY_RUNNING`; lock is reacquirable after process exit.
- `daemon::endpoint_write_atomic` — `endpoint.json` appears complete or not at all; macOS mode is `0600`.
- `daemon::reap_identity_check` — matching-args live pid terminated; dead pid and args-mismatch pid untouched; outcomes reported.
- `api::envelope_stability` — every error path answers the one §6 envelope with a documented code.
- `cli::exit_codes` — the `0/1/2/3` mapping of §8 against a fake endpoint.
- `log::secret_free` — a daemon run's log root contains no substring of the live credential.

**Gate** (one Cargo test target, unattended, both platforms in CI): scenarios G1–G11 in order, producing the evidence bundle. This is the scenario-to-check mapping D75 requires for R0; later milestones append to it.

**Static checks:** rustfmt, Clippy `-D warnings`, `cargo test` across the workspace, on Windows and macOS CI (per D11, D60, D61).
