# Slice 015 — Trigger invocation

**Status:** Active (2026-09-11). Implementation not started.

The implementation plan for [`features/event-triggers/`](../features/event-triggers/PRD.md), decisions ET-1–ET-7 and SPEC §1–§8. The feature folder is canonical; drift found while implementing is corrected there in the same change. Root D34, SPEC §8, §10, §11.1, §13.2, §15, §18, the PRD, ROADMAP, and AGENTS.md were reconciled to direct invocation on 2026-09-11, before this slice, so root and feature agree and no root merge waits on the freeze beyond the migration and check records.

## Outcome

A local producer runs `marketrig trigger invoke <desk> <trigger> --request-id <id> [--input …]` and the daemon accepts one firing of that trigger, or answers the original firing on a replay, or refuses with a reason and stores nothing. The firing carries the input to the code's document and to the result prompt; execution and delivery are R2 and R3's unchanged. A one-off is consumed through either door; a recurring trigger fires per accepted request and keeps its schedule. No event name, listener, fan-out, ceiling, or new surface.

## Boundaries

- Worktree `.worktrees/trigger-invocation/` on a fresh branch; every spawned binary under a scratch `MARKETRIG_TEST_DATA_ROOT`.
- Frozen slices and feature SPECs are not edited. R2's `trigger-code` `retain` mode is already gone (5c5c881); the R2 and R4 SPEC sentences describing it stay as frozen history.
- No new crate, table beyond §4's rebuilds, event kind, desktop view, or CLI group. No caller identity beyond the bearer, no producer timestamp field, no content comparison on a duplicate.
- The generated client changes (the Trigger resource loses `source`, the route is new): `pnpm generate` runs and `openapi.json` and `src/client` are committed in the same change.

## Implementation sequence

Each step leaves its focused checks green before the next.

### 1. Schema and definition (feature SPEC §1, §4)

Touch `crates/marketrigd/src/store/010_invocation.sql`, `store.rs`, `schedule.rs`, `trigger.rs`.

- Migration 10 under the foreign-keys-off window: rebuild `triggers` without `source` and with the relaxed schedule checks; rebuild `firings` with `request_id`, `input`, the input check, and the two partial unique indexes; recreate the three indexes. `PRAGMA foreign_key_check` empty afterwards.
- `Schedule::parse` accepts absence; create body `schedule` optional; patch `schedule: null` detaches. The projection closure yields `None` with no schedule.
- The scheduler's one-off acceptance sets `enabled = 0` beside the `NULL` projection. Existing tests that assert `enabled: true` after a scheduled one-off fires are updated to `false`.
- The Trigger resource drops `source` and omits `schedule` when none; the Firing resource and per-trigger listing gain `request_id`, `input_bytes`, and (single read only) `input`.

Checks: `store::invocation_migration_applies`, `trigger::schedule_optional`, `schedule::tests::accept_or_miss` extended.

### 2. The invocation unit and route (feature SPEC §2)

Touch `trigger.rs`, `api.rs`.

- `trigger::invoke` runs §2.2's five steps in one `BEGIN IMMEDIATE` unit with `now` read once: duplicate lookup, target, eligibility (join the snapshot's `approval`), the firing insert with `occurrence_ns = accepted_at_ns`, the code-free prompt in the same unit, and the one-off advance. Four new `TriggerError` variants: `InvocationInvalid`, `Disabled { consumed_by }`, `Unapproved`, `Elapsed`.
- `POST /desks/{desk_id}/triggers/{trigger_id}/invocations` behind the bearer, ignoring the attribution headers, answering `201 {outcome: ACCEPTED, firing}` or `200 {outcome: DUPLICATE, firing}`; wakes the executor or the dispatcher after commit like the scheduler's pass. `utoipa` annotations and the four new codes in the OpenAPI document.

Checks: `trigger::invoke_unit` (the §2.3 table), `trigger::invoke_request_form`, `trigger::one_off_consumed_by_either_door`, `api::invocation_codes`.

### 3. The input's two homes (feature SPEC §3)

Touch `exec.rs`, `trigger.rs` (`insert_result_prompt`).

- The firing document gains `invocation: {request_id, input?}` on an invoked firing, absent otherwise; version stays 1.
- The `TRIGGER_RESULT` payload gains `invocation: {request_id, input_bytes, input?}` with `input` inline up to 16,384 bytes.

Checks: `exec::document_carries_invocation`, `trigger::result_prompt_input_bound`.

### 4. CLI, seed, client (feature SPEC §5, §6)

Touch `crates/marketrig/src/main.rs`, `lib.rs`, `crates/marketrigd/seed/AGENTS.md`, `openapi.json`, `src/client`.

- `trigger invoke` with `--request-id`, `--input | --input-file` (file read before contact; unreadable, non-UTF-8, or both flags → exit 2); exit 0 on both outcomes; human output `outcome:` then the firing lines; `--json` verbatim. `create`'s schedule optional; `update --no-schedule`. `trigger firings` gains the request-id column.
- The seeded `AGENTS.md`'s continuity-plane line names `invoke` and one paragraph teaches the pattern: define the job once, have the producer invoke it with a request id it can safely repeat.
- `pnpm generate`; the triggers tab needs no change, but `pnpm check` must pass against the regenerated client (it reads no `source`).

Checks: `cli::trigger_invoke_exit_codes`, `cli::trigger_invoke_output`, `pnpm check`, `node scripts/hithink-skill.mjs --check` unaffected.

### 5. Gate and attended acceptance (feature SPEC §7)

Touch `crates/marketrig-acceptance/tests/gate.rs`, `tests/experiment.rs`, `EXPERIMENT.md`.

- T1–T5 after A6 in the one gate test, on real binaries and the stand-in feed, no runtime registered; T4 sets `trigger_code_policy` through `PUT /settings/policies` and restores it. G21's listing assertion updated for `enabled: false` after the one-off fires.
- E8 after E7 per cell: the harness defines `review-research` with no schedule, attaches the console as E4 does, invokes with a three-line text, records `ACCEPTED`, the firing, `TRIGGER_RESULT DELIVERED`, the operator's confirmation, and the `DUPLICATE` replay; agent-behavior steps end `INCONCLUSIVE`. `EXPERIMENT.md` gains the E8 procedure. Skips with evidence when the cell's runtime is absent, like E4.

## Exit checks

- Feature SPEC §8's eleven module checks pass; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm generate` (no diff after commit), `pnpm check` — on macOS and in CI on both platforms.
- The gate passes on both platforms through T5 with its evidence bundle; the migration-9 upgrade path is covered by the module check, not by a hand-carried database.
- E8 closes on the macOS Codex and Claude cells; the Windows pair is recorded here when run, and is not a freeze condition if the macOS pair and CI are green (as slice 014 was frozen with its Windows E7 pair recorded later).
- No credential, input, or request id appears in a daemon log line beyond the refusal's reason and ids.

After these checks, freeze this slice, record migration 10 and T1–T5/E8 as delivered in root SPEC §15 and §17 and ROADMAP's R7 item, update AGENTS.md's repository state and Commands (`trigger invoke`), and allocate no product D number: D34 already carries the amendment.

## Verification record

Planning (2026-09-11): feature folder complete, root reconciled, local links checked; no runtime pass claimed.
