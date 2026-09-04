# Slice 005 — R4 memory, skills, and the closed loop

**Status:** Active — created 2026-09-04
**Implements:** all of [`features/r4-memory-skills-loop/`](../features/r4-memory-skills-loop/SPEC.md)
**Evidence so far (2026-09-04, macOS arm64, commit 604a19b):** module checks and the gate G1–G37 green (`target/acceptance/gate-1788509422`); E5 codex cell complete on Codex CLI 0.153.2 and `hindsight-api-slim[embedded-db]==0.9.2` via OpenRouter (`z-ai/glm-5.3-flash`, `openai/text-embedding-3-small`) — `experiment-e5-codex-1788514468`: one AAPL.XNAS cycle, `EVALUATION` delivered, `MEMORY_RETAINED` (tags `lesson`, `AAPL.XNAS`), the improvement skill edited by the session, `MEMORY_RECALLED` from the resumed session, the second desk's bank empty; the key present only in the relocated `credentials.json`. E5 claude cell complete on Claude Code 2.1.260 — `experiment-e5-claude-1788515113`: same shape, the session's own skill edit names `market_phase`/`book_synthesized`. Both cells found one defect the gate cannot see (the stand-in has no subprocess): Hindsight's embedded PostgreSQL survived the stop, fixed by the `SIGTERM`-first stop and the postmaster reap (feature SPEC §2.3, module check `memory::stopping_the_child_takes_its_daemonized_grandchild`); the follow-up smoke found the launcher's stderr held the crash reason, so the tail now takes both streams (§2.2). Pending: Windows CI.

**Exit:** feature SPEC §8 in full — the eight module checks, gate G33–G37 after G32, and the static checks green on macOS and Windows CI, plus E5 attended once per platform-and-runtime cell (agent-behavior aspects end inconclusive rather than failed, root §17).

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

## 1. Pins

Verified against crates.io and PyPI on 2026-09-04. Chunk numbering, toolchain, and the R0–R3 pins continue unchanged.

| Dependency | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `keyring-core` | `=1.0.0` | marketrigd | MIT OR Apache-2.0; the `Entry` API and `set_default_store`. The `keyring` 4.2.0 facade is not used: it drags `clap`, `rpassword`, `rprompt`, and `base64 0.23` for its CLI. |
| `apple-native-keyring-store` | `=1.0.2` | marketrigd (macOS target) | the Keychain store; constructed and set as the default store in `lib.rs::serve` before any route. |
| `windows-native-keyring-store` | `=1.1.0` | marketrigd (windows target) | the Credential Locker store; same wiring. |
| `windows` | `=0.62.2` (existing) | marketrigd (windows target) | gains `Win32_Storage_FileSystem` and `Win32_System_Ioctl` for the junction (`CreateFileW` + `DeviceIoControl` with `FSCTL_SET_REPARSE_POINT`, `IO_REPARSE_TAG_MOUNT_POINT`). |
| `reqwest` | `=0.13.4` (existing) | marketrigd | the Hindsight client and the provider `/models` fetch; loopback and plain `https`, `json` feature already on. |
| `hindsight-api-slim[embedded-db]` | `==0.9.2` | the launcher the operator names (E5) and R6's bundle | PyPI, Requires-Python ≥ 3.11; resolves `pg0-embedded 0.15.1`. Not a Cargo pin; recorded here as the wheel every launcher must carry. |

`ponytail:` two store crates and a core instead of one facade is three pins for one concern; the facade's own extra dependencies are the reason, and the upgrade path is the facade with `default-features = false` the day it stops pulling its CLI.

## 2. Plan-time settlements

Facts the feature docs left to the slice:

- **Spike H (first, before C29):** run the pinned `hindsight-api` from a scratch uv environment with the §2.2 environment and `HOME` pointed at a scratch directory; confirm where the pg0 instance directory lands (`<HOME>/.pg0/…` or elsewhere), that `/health` answers `200` after start, that a wrong LLM key makes the child exit before `/health` answers (or, if it stays up, what `/health` returns — the loss rule then keys on that), and how long a cold start takes on this machine (sets the 120 s deadline or corrects it). Record the outcome in R4-1's ponytail note and feature SPEC §2.2/§2.3 in the spike's own commit. **Done 2026-09-04 (macOS arm64):** pg0 honors `HOME`; `/health 200` in 12.6 s cold and 6.1 s warm, so the 120 s deadline stands; a wrong key does *not* stop the child — it starts healthy and the retain fails `500`, so the loss rule keys only on exit or a missed deadline; the launcher logs to standard output, not standard error. The feature docs carry all four.
- **Credential store (R4-2):** `keyring_core::set_default_store(<Store>::new()?)` once in `serve`; `Entry::new("marketrig", "hindsight-provider")` for `set_password`/`get_password`; under `MARKETRIG_TEST_DATA_ROOT` a file store of MarketRig's own (`runtime/credentials.json`, 0600, `serde_json` map) behind the same three-function seam, chosen by the seam flag rather than by a trait object.
- **Child (R4-1):** spawned through `exec::spawn` (the R2 primitive, already the one path for every managed child), recorded with `daemon::record_child`; the reader task drains standard **output** into a 4 KiB ring (Spike H: the launcher logs there and leaves standard error empty); readiness polled with the daemon's `reqwest` client (1 s per attempt, 500 ms apart); the bearer minted with `getrandom`. The child, its state, and the provider row live in one `memory::Memory` struct in `ApiState`, mutated under a tokio `Mutex` — operations are seconds long and per installation, so no finer lock. That mutex is never held across a wait: readiness is signalled by the live state itself, which a caller arriving during `STARTING` and the per-start supervisor task both re-read every 250 ms, so `GET /memory` answers throughout a cold start and the child handle stays where a stop reaches it. Standard error is `null` rather than piped: Spike H found it empty and there is nothing to drain.
- **Hindsight client (R4-3):** three functions over `reqwest`, each building the path from the derived bank, sending the bearer, mapping status to the §4.3 codes; response bodies deserialized into the subset structs the routes answer, unknown fields ignored.
- **Attribution:** the memory retain route reuses the order routes' header validation (`trade::attribution` or its current home) verbatim; no second validator.
- **Junction (R4-4):** on Windows `.claude/skills` is created with `CreateFileW(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)` on a fresh empty directory and `DeviceIoControl(FSCTL_SET_REPARSE_POINT)` with a `REPARSE_DATA_BUFFER` mount-point record naming `\??\<absolute .agents\skills>`; detection reads it back with `FSCTL_GET_REPARSE_POINT`. On macOS `std::os::unix::fs::symlink("../.agents/skills", …)` and `read_link`. Both behind two functions in `desk.rs` (`link_skills`, `skills_link_target`).
- **Seeds:** the §5.1 and §5.2 texts are `include_str!` files under `crates/marketrigd/seed/` (`AGENTS.md`, `desk-improvement.SKILL.md`) with one `<name>` substitution, so a wording change is a file change and the module check compares byte for byte against the SPEC's blocks.
- **Startup glue (`lib.rs::serve`):** step 6b after 6a, skipped under the seam; the store set before binding; shutdown stops the memory child after the terminals and the app-server, inside the existing 5 s bound.
- **Migration 5:** `store/005_r4.sql` per feature SPEC §6; the `operational_events` rebuild copies migration 4's block and appends the six kinds.
- **Test targets:** G33–G37 extend `--test gate` after G32; E5 joins `--test experiment` as a fifth attended test, gated by `MARKETRIG_EXPERIMENT` and skipping with evidence when the `MARKETRIG_EXPERIMENT_HINDSIGHT` or `MARKETRIG_EXPERIMENT_MEMORY_*` variables are unset. `cargo test -p marketrig-acceptance --test standin` grows a `memory-standin` half. The gate starts `memory-standin --models <port>` itself once per run and registers the same binary as the memory launcher through `POST /memory/discover`.

## 3. Chunks

One coding-agent work unit each; a chunk is done when its named checks pass locally. Numbering continues from slice 004.

| # | Chunk | Builds (feature SPEC) | Lands with (§8 checks) | Needs |
| --- | --- | --- | --- | --- |
| H | Spike H: the real launcher under the §2.2 environment (docs-only edits) | R4-1 ponytail note; §2.2–§2.3 corrections if any | — | — |
| C29 | Foundation: pins, migration 5, the `memory_child` and `memory_provider` rows, discovery and step 6b, the credential-store seam, provider routes and `/models`, `GET /memory` | §1; §2.1; §3; §6 | checks 1, 3, 6, 7; static checks green with the new pins | — |
| C30 | The child: launch environment, readiness, loss, restart, `UNAVAILABLE`, retry, Quit, `children.json` | §2.2; §2.3 | check 2 | C29, H |
| C31 | Operations: bank derivation, the three Hindsight calls, attribution, limits, codes, events, desk-scoped routes, the `marketrig memory` group | §4 | checks 4, 8 | C29 |
| C32 | Seeds and the link: constitution and skill files, creation ordering, link reconciliation on both platforms, workspace-status reason | §5 | check 5 | — |
| C33 | Gate G33–G37, `memory-standin`, experiment E5, operator guide | §7; `EXPERIMENT.md` and AGENTS.md **Commands** refreshed | G33–G37 on both platforms; E5 target content | all |

C32 runs in parallel with C29 (they share only `desk.rs`'s bootstrap and `lib.rs`); C30 and C31 run in parallel after C29, against the `memory::Memory` struct and the three-function Hindsight client signature C29 lands as stubs (`retain`, `recall`, `reflect` over `(bank, body) -> Result<Value, MemoryError>`); C33 is last. C30's module check drives an in-process fake `hindsight-api` (an axum server answering `/health` and the three routes), not the real one.

## 4. Execution

Orchestrator plus one coding agent per chunk, sequential along the Needs column, parallel where §3 allows; parallel chunks work in `.worktrees/<chunk>/` per AGENTS.md with a shared `CARGO_TARGET_DIR`. Spike H is a docs-only agent run concurrently with C29. Each agent receives this slice, the feature SPEC, and its chunk row, and delivers a diff with its checks green; the orchestrator runs the static checks after every merge and smokes the real binaries on macOS before briefing C33 — start `marketrigd` under the seam, `discover` the uv-environment launcher, `PUT` the provider with a real key, `memory retain` on one desk, `recall` on it and on a second desk, `POST /quit` — so the first real-Hindsight contact happens before the gate chunk, not inside E5. E5 is operator-attended after C33 merges, one run per platform-and-runtime cell, evidence bundled per root §17.

## 5. Freeze and merge-back

When the exit checks are green: freeze this slice, then per AGENTS.md merge durable R4 mechanics into root `SPEC.md` (§4.4 the `memory_child` and `memory_provider` resources and the seam credential file, §4.6 step 6b and the shutdown order, §5.1–§5.2 the seeds and the link reconciliation, §13.2 the `memory` group, §15 migration 5, §16 the child lifecycle, bank derivation, operations, codes, and the seeded texts' home, §17 the stand-in memory child, G33–G37, and E5, §18 the resolved deferrals removed), summarize R4-1…R4-6 as one product `D<n>`, refresh `ROADMAP.md` (R4 delivered, evidence line), and grow the AGENTS.md **Commands** section for E5.
