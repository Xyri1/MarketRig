# Slice 003 — R2 scheduled triggers

**Status:** Active (2026-09-02)
**Implements:** all of [`features/r2-scheduled-triggers/`](../features/r2-scheduled-triggers/SPEC.md)
**Exit:** feature SPEC §11 in full — the 18 module checks, gate G21–G26 after G20, and the static checks green on macOS and Windows CI, plus E3 attended once per platform-and-runtime cell (agent-behavior aspects end inconclusive rather than failed, root §17).

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

## 1. Pins

Verified against crates.io, the lockfile, and the crate sources on 2026-09-02. Chunk numbering, toolchain, and the R0/R1 pins continue unchanged.

| Crate | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `rrule` | `=0.14.0` | marketrigd | `default-features = false` (its defaults are empty; `serde`, `exrule`, `by-easter`, `cli-tool` stay off). Requires `chrono ^0.4.39`, `chrono-tz ^0.10.1`, `regex ^1.11.1`, `thiserror ^2.0.11` — all already resolved in the graph at the pinned lines, so one copy of each. |
| `process-wrap` | `=9.1.0` | marketrigd | already in the graph under `rmcp =3.2.0` with its default features; MarketRig names `tokio1`, `process-session`, `job-object` and turns defaults off. Brings no new crate (`nix 0.31.3`, `windows 0.62.2` are already present). |
| `ring` | `=0.17.14` | marketrigd | the fingerprint's SHA-256 (`ring::digest::digest(&SHA256, …)`); already in the graph under rustls. |
| `tokio` | `=1.53.1` | marketrigd | gains the `process` feature on the existing pin. |

The lockfile gains exactly one `[[package]]` entry, `rrule 0.14.0` itself; its dependencies and the other two pins resolve to entries already present (verified at C17). `sysinfo =0.39.6` is reused by the `exec::group_terminated_on_timeout` check (already a daemon dependency).

## 2. Plan-time settlements

Facts the feature docs left to the slice, verified in the pinned sources today:

- **Recurrence frame (R2-1):** `RRuleSet::new(dtstart)` with `dtstart` built as `rrule::Tz::UTC.with_ymd_and_hms(naive fields)`, `.rrule("…".parse::<RRule<Unvalidated>>()?.validate(dtstart)?)`; `into_iter()` is the unbounded iterator (bounded by the projection's 100,000 cap); each yielded `DateTime<Tz>`'s `naive_utc()` fields are the wall-clock candidate, resolved with `chrono_tz::Tz::from_local_datetime`. The crate's own `Tz` is only ever `UTC`. Getters `get_freq`, `get_count`, `get_until`, `get_by_second` back the form rejections; the text checks (line breaks, `RRULE:`, `DTSTART`, `RDATE`, `EXDATE`, `EXRULE`) run before parsing.
- **Containment (R2-3):** `process_wrap::tokio::CommandWrap::with_new(argv[0], |c| { … })` then `.wrap(ProcessSession)` on Unix / `.wrap(JobObject)` on Windows; `spawn()` yields `Box<dyn ChildWrapper>` with `stdin()/stdout()/stderr()`, `id()`, `wait()`, and `kill()` — `ProcessGroupChild::start_kill` sends `SIGKILL` to the group, `JobObject` terminates the job. `exec::spawn` wraps exactly that and exposes nothing wider (feature SPEC §4.5).
- **Streams:** stdout and stderr read concurrently with `tokio::io::AsyncReadExt::read` into capped buffers; the cap breach terminates the group and is the `OUTPUT_LIMIT` outcome; `tokio::time::timeout` bounds `wait` for `TIMED_OUT`.
- **Recovery registration:** `daemon::recover` gains the executions step between reaping and the `RECOVERY` insert, calling `exec::recovery_step(tx, daemon_uuid) -> Vec<Value>` whose return is the payload's `executions_lost`.
- **Startup and shutdown glue (`lib.rs::serve`):** two `Arc<Notify>` (scheduler wake, executor wake) and the shutdown `watch` are created in `serve`, handed to `api::ApiState` and to the two tasks, which start after the listener is live; shutdown awaits the `exec::run` task itself (it returns only after terminating every group and persisting `QUIT`) inside the existing 5-second deadline before `startup.shutdown()`. The `JobObject` is kill-on-close only with the crate's `kill-on-drop` feature and a `KillOnDrop` wrap, so both are on.
- **Attribution (R2-6):** `trade::begin` takes a `Source` (`Session` | `Trigger { trigger_id, firing_id }`) that `api` derives from the headers after validating the firing row; the ActionRecord serializes `source` and the two ids.
- **Test targets:** G21–G26 extend `--test gate` after G20; E3 joins `--test experiment` as a third attended test gated by the same `MARKETRIG_EXPERIMENT` cell variable — the operator runs E1/E2 and E3 in the same invocation per cell.

## 3. Chunks

One coding-agent work unit each; a chunk is done when its named checks pass locally. Numbering continues from slice 002.

| # | Chunk | Builds (feature SPEC) | Lands with (§11 checks) | Needs |
| --- | --- | --- | --- | --- |
| C17 | Foundation: pins, migration 3, attribution, shared trigger rows | §1; §7 in full; §6 (client headers, `trade::Source`, the two order routes, `ATTRIBUTION_INVALID`); `trigger.rs` with the row types, `load_firing`, and `insert_result_prompt` (§5) | `store::trigger_migration_applies`, `api::action_attribution`, `client::attribution_headers_from_env`; static checks green with the new pins | — |
| C18 | Schedules and the scheduler | §2, §3 — `schedule::Schedule` (`parse`, `next_after`, `count_between`), the acceptance unit, the task, `TRIGGER_MISSED` | `schedule::form_rejected`, `schedule::dst_gap_skipped_overlap_earlier`, `schedule::projection_from_anchor`, `schedule::tests::accept_or_miss`, `schedule::duplicate_wake_no_second_firing`, `schedule::wake_and_recheck` | C17 |
| C19 | Trigger definitions, firings, prompts, REST | §4.1 snapshots and fingerprint, §8 in full, the wake signal after mutations | `trigger::fingerprint_stable`, `api::trigger_codes` | C18 |
| C20 | Executor, containment, outcomes, recovery, shutdown | §4.2–§4.5, §5 completion unit | `exec::document_and_environment`, `exec::outcomes_one_record_one_prompt`, `exec::group_terminated_on_timeout`, `exec::fifo_per_desk_concurrent_across_desks`, `exec::recovery_marks_daemon_lost` | C17 |
| C21 | CLI `trigger` and `prompt` groups | §9 | `cli::trigger_exit_codes`, `cli::prompt_exit_codes` | C17 |
| C22 | Gate, `trigger-code`, experiment E3, operator guide | §10; `EXPERIMENT.md` and AGENTS.md **Commands** refreshed | G21–G26 on both platforms; E3 target content | all |

C18, C20, and C21 run in parallel after C17 (C20 and C21 code against the pinned §7/§8/§9 contracts); C19 follows C18; C22 is last. C20 inserts firings directly in its tests — it does not need the scheduler.

## 4. Execution

Orchestrator plus one coding agent per chunk, sequential along the Needs column, parallel where §3 allows; parallel chunks work in `.worktrees/<chunk>/` per AGENTS.md. Each agent receives this slice, the feature SPEC, and its chunk row, and delivers a diff with its checks green; the orchestrator runs the static checks after every merge and smokes the real binaries (start → create desk → create trigger → firing → quit) before briefing C22. E3 is operator-attended after C22 merges, one run per platform-and-runtime cell, evidence bundled per root §17.

## 5. Freeze and merge-back

When the exit checks are green: freeze this slice, then per AGENTS.md merge durable R2 mechanics into root `SPEC.md` (§8.3–§8.4 lifecycle mechanics, §9 the firing document and bounds, §10 storage and transactions, §11.1 the result payload, §12.3 attribution, §13.2 the two groups, §15 migration 3 and the recovery step, §17 the trigger-code binary), summarize R2-1…R2-8 as one product `D<n>`, refresh `ROADMAP.md` (R2 delivered, evidence line), and grow the AGENTS.md **Commands** section if the invocation changes.
