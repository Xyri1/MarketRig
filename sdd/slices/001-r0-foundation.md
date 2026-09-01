# Slice 001 — R0 foundation

**Status:** Frozen 2026-09-01 — exit checks green on macOS and Windows CI (run 33494211710, commit 07faea6)
**Implements:** all of [`features/r0-workspace-desk-identity/`](../features/r0-workspace-desk-identity/SPEC.md)
**Exit:** feature SPEC §11 in full — the 12 module checks, gate G1–G11, and the static checks — green on macOS and Windows CI.

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

## 1. Pins

Verified against crates.io on 2026-09-01. Toolchain: `rust-toolchain.toml` pins **rustc 1.98.0** (matches the machine toolchain), edition 2024, one workspace version `0.1.0`. Per AGENTS.md, bumping a pin later is a version change verified by that module's checks, not a decision.

| Crate | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `axum` | `=0.8.9` | marketrigd | per D48 |
| `tokio` | `=1.53.1` | marketrigd | features `rt-multi-thread`, `net`, `signal` (cross-platform Ctrl+C, §4.2), `macros`, `sync` (the `/quit` channel) |
| `rusqlite` | `=0.40.2` | marketrigd | feature `bundled` — SQLite compiled in, one release unit per D53 |
| `uuid` | `=1.26.0` | marketrigd, marketrig | feature `v7` |
| `getrandom` | `=0.4.3` | marketrigd | bearer credential bytes (§4.1); already in-tree via `uuid` |
| `serde` | `=1.0.229` | all | feature `derive` |
| `serde_json` | `=1.0.151` | all | |
| `tracing` | `=0.1.44` | marketrigd | per D51 |
| `tracing-subscriber` | `=0.3.23` | marketrigd | feature `json` (§9) |
| `tracing-appender` | `=0.2.5` | marketrigd | daily rotation, `max_log_files(7)` per R0-8 |
| `sysinfo` | `=0.39.6` | marketrigd | reaping's pid-plus-cmdline check (§4.4) |
| `clap` | `=4.6.6` | marketrig | feature `derive`, per D50 |
| `ureq` | `=3.4.0` | marketrig, acceptance | feature `json`; `proxy(None)` and redirects-off are mandatory config per R0-8 |
| `tempfile` | `=3.27.0` | dev-dependencies | scratch roots for module checks |

**OpenAPI emitter — chosen, not shipped** (per R0-8, D48, D59): `utoipa =5.5.0` + `utoipa-axum =0.2.0`, verified current against the axum 0.8 line today. Emission wiring stays deferred (root §18); the dependency lands when R5 wires it, with compatibility re-verified then.

## 2. Plan-time names

- Acceptance crate (per R0-7): `crates/marketrig-acceptance`, unpublished.
- Gate invocation: `cargo test -p marketrig-acceptance --test gate` — G1–G11 in order in one target. The experiment target is `--test experiment`, present and empty until R1 (feature SPEC §10).
- CI: GitHub Actions, `macos-latest` + `windows-latest`; rustfmt check, Clippy `-D warnings`, `cargo test` across the workspace.
- `marketrigd` library modules mirror the check prefixes: `store`, `desk`, `daemon`, `api`.

## 3. Chunks

One coding-agent work unit each; a chunk is done when its named checks pass locally.

| # | Chunk | Builds (feature SPEC) | Lands with (§11 checks) | Needs |
| --- | --- | --- | --- | --- |
| C1 | Workspace scaffold | §1; the pins above; CI | static checks green on the empty workspace | — |
| C2 | Store | §2 (roots, test seam), §3 | `store::migrations_apply_and_stamp`, `store::newer_database_rejected`, `store::desk_row_checks` | C1 |
| C3 | Desk domain | §7 | `desk::name_grammar`, `desk::bootstrap_idempotent`, `desk::interrupted_creating_completes` | C2 |
| C4 | Daemon lifecycle | §4, §5.1, §9 | `daemon::lock_excludes_second_start`, `daemon::endpoint_write_atomic`, `daemon::reap_identity_check`, `log::secret_free` | C3 |
| C5 | REST surface | §6 | `api::envelope_stability` | C3 |
| C6 | CLI | §5.2, §8 | `cli::exit_codes` | C1 |
| C7 | Gate | §10 | G1–G11, both platforms | all |

C4, C5, and C6 may run in parallel after their needs are merged — C6 codes against the pinned §6/§8 contract and a fake endpoint, so it needs only the scaffold.

## 4. Execution

Orchestrator plus one coding agent per chunk, sequential along the Needs column, parallel where §3 allows; parallel chunks work in `.worktrees/<chunk>/` per AGENTS.md. Each agent receives this slice, the feature SPEC, and its chunk row, and delivers a diff with its checks green; the orchestrator runs the static checks after every merge.

## 5. Freeze and merge-back

When the exit checks are green on both CI platforms: freeze this slice, then per AGENTS.md merge durable R0 mechanics into root `SPEC.md`, summarize R0-1…R0-8 as one product `D<n>`, refresh `ROADMAP.md` (R0 done, evidence produced), and grow the AGENTS.md **Commands** section with the workspace's real commands.
