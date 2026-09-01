# R0 — Workspace, daemon, and desk identity: Feature Decisions

Local decisions for [Milestone R0](../../ROADMAP.md#milestone-r0--workspace-daemon-and-desk-identity), prefixed `R0-<n>`. They resolve mechanics the root SPEC defers (§18) for R0's scope and contradict nothing settled; on merge they are summarized as one product `D<n>` in `sdd/DECISIONS.md`.

### R0-1 — The daemon is started by its operator, never by the CLI

**Decision:** `marketrig` discovers a running daemon through `runtime/endpoint.json` and authenticated health, and fails explicitly with a stable error code when none is reachable. It never spawns `marketrigd`. In R0 the daemon is started by the operator or the acceptance harness; from R5 the Tauri shell owns bootstrap (per D66).

**Rationale:** The CLI is the agent's surface, and an agent-spawned daemon would be a sole-writer coordinator with no owner to stop it — and a nondeterministic variable in every gate run. Explicit failure matches the product rule that nothing is silently repaired.

**Contract:** root [SPEC §4.3](../../SPEC.md#43-local-single-user-boundary) and [§13.2](../../SPEC.md#132-cli-continuity-plane); this feature's [SPEC](SPEC.md).

### R0-2 — One daemon per data root, enforced by an OS file lock held for the daemon's lifetime

**Decision:** The daemon lifetime lock is a file under `runtime/` in the data root, taken exclusively with the standard library's advisory file locking (`File::try_lock`, stabilized in Rust 1.89 — verified 2026-09-01 — under R0's ≥ 1.98 toolchain) before `runtime/endpoint.json` is written and held until process exit. A second daemon on the same root exits with a clear error having written nothing. `endpoint.json` is a pointer, never proof of liveness: clients always verify through authenticated health and the UUID match (per D44), so a stale file from a dead daemon simply fails verification.

**Rationale:** An OS lock releases on any process death, including a hard kill, so no stale-pidfile heuristic is needed, and the standard library covers it without a crate. Scoping the instance to the data root — not the machine — is what lets the harness run concurrent daemons in scratch roots (per D75).

**Contract:** root [SPEC §4.3](../../SPEC.md#43-local-single-user-boundary) and [§15](../../SPEC.md#15-persistence-crash-recovery-and-history); this feature's [SPEC](SPEC.md).

### R0-3 — Schema version is `PRAGMA user_version`; migrations are embedded, numbered, applied at startup

**Decision:** The daemon embeds its ordered migration list in the binary. At startup, before recovery and before serving, it applies pending migrations and records the resulting number in `PRAGMA user_version`. A database whose `user_version` exceeds the binary's newest migration is rejected and the daemon refuses to start (per D13, §15). No migration bookkeeping table, no external migration files, no down migrations.

**Rationale:** `user_version` is SQLite's own slot for exactly this fact; a bookkeeping table would duplicate it. Embedding the list keeps binary and schema one release unit (per D53) with nothing to install beside the executable.

**Contract:** root [SPEC §15](../../SPEC.md#15-persistence-crash-recovery-and-history); this feature's [SPEC](SPEC.md).

### R0-4 — `operational_events` is born in R0 as the one append-only evidence table

**Decision:** The single installation-wide `operational_events` table that D71 later tails exists from R0. Rows are append-only and committed inside the transaction of the change they evidence; the kind is a closed vocabulary grown milestone by milestone, starting with `RECOVERY` and the desk-lifecycle kinds this feature's SPEC enumerates. There are no per-domain event tables.

**Rationale:** §15's recovery event and R0's desk provenance need a durable home now, and D71 already commits the live-event socket to tailing one table — founding it first means later milestones append kinds instead of migrating scattered event rows into it.

**Contract:** root [SPEC §15](../../SPEC.md#15-persistence-crash-recovery-and-history); this feature's [SPEC](SPEC.md).

### R0-5 — One error envelope and one exit-code contract, defined once in R0

**Decision:** Every daemon error crosses REST as one JSON envelope — a stable SCREAMING_SNAKE machine code plus an English message (per D68) — and `marketrig` passes it through: code and message as text for humans, the envelope itself under `--json`, with one small fixed exit-code set. Later feature groups add codes; none redefines the envelope shape or an exit code's meaning. The concrete shape, first codes, and exit codes are pinned in this feature's SPEC.

**Rationale:** §18 defers per-group error details, but the envelope is cross-cutting: defined with the first endpoint, every later group inherits it; left to each group, the agent-facing contract forks per command family.

**Contract:** root [SPEC §13.2](../../SPEC.md#132-cli-continuity-plane) and [§18](../../SPEC.md#18-implementation-deferred-contracts); this feature's [SPEC](SPEC.md).

### R0-6 — Daemon shutdown is an authenticated route from R0

**Decision:** `POST /quit` — the route D66 later gives the desktop shell — exists from R0. It authenticates like any request, stops accepting work, drains the database thread, removes `endpoint.json`, releases the lifetime lock, and exits within a bounded wait. Ctrl+C on a terminal-attached daemon takes the same graceful path. The gate's clean stop uses the route; its crash scenarios use a hard process kill, which leaves the files behind for recovery and verification to handle.

**Rationale:** The evidence line requires a deterministic cross-platform stop, and Windows offers no signal the gate could portably send; reusing D66's route means the shell's Quit finds it already standing in R5.

**Contract:** root [SPEC §4.3](../../SPEC.md#43-local-single-user-boundary) and [§14](../../SPEC.md#14-desktop-and-application-lifecycle); this feature's [SPEC](SPEC.md).

### R0-7 — The acceptance harness is one dedicated internal crate that drives the real binaries

**Decision:** The gate lives in its own internal workspace crate, run as a Cargo test target (exact name and invocation at plan time, per D75). It spawns the real `marketrigd` binary and drives public surfaces only — `marketrig --json`, the loopback API, workspace files, read-only SQLite — under `MARKETRIG_TEST_DATA_ROOT`, and each run's evidence directory receives the relocated root, the daemon's JSON-Lines log, and the harness's own observations by construction. The attended experiment target joins the same crate when the first scenario needs an operator (R1's runtime evidence).

**Rationale:** A harness inside `marketrigd`'s own test tree could reach private surfaces, making D67's public-surfaces-only rule a discipline; a separate crate that can only spawn binaries makes it structural.

**Contract:** root [SPEC §17](../../SPEC.md#17-verification); this feature's [SPEC](SPEC.md).

### R0-8 — R0's new crate candidates

**Decision:** Beyond the candidates the product log already names for R0 — `axum` (per D48), `rusqlite` (per D45), `tracing` (per D51) — this feature names four more, each verified against current documentation on 2026-09-01:

- `clap` for the CLI grammar (per D50);
- `ureq` for the CLI's blocking HTTP (per D50): granular and global timeouts and `max_redirects(0)` are in its current configuration surface; environment proxy pickup is **on by default** and disabled with an explicit `proxy(None)`, so that configuration line is mandatory, not optional;
- `uuid` for identifiers (per D45): the v7 feature is stable with no unstable flag, verified on the 1.26.0 line;
- `tracing-appender` for log rotation (per D51): `max_log_files` bounds the kept files, verified on the 0.2.5 line; rotation is time-based only, so boundedness is file count — a size-conditioned appender is the recorded swap if one interval's file ever needs its own cap;
- `sysinfo` for the pid-plus-command-line liveness check reaping needs (per D73): `Process::cmd()` covers it, and its Windows privilege caveat is moot because Windows discards records without checking (per D73).

The lifetime lock uses the standard library and no crate (R0-2). Every selection is pinned exactly at plan time, and the OpenAPI emitter is chosen alongside `axum` then (per D48, D59).

**Rationale:** Naming candidates now keeps the plan a verification-and-pinning exercise rather than a design session, matching the product log's own convention; each candidate is the boring maintained default for its one concern, and none introduces a framework beyond what a decision already admits.

**Contract:** root [SPEC §18](../../SPEC.md#18-implementation-deferred-contracts); this feature's [SPEC](SPEC.md) and the implementation PLAN.
