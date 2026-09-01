# Slice 002 — R1 equity paper trading

**Status:** Active 2026-09-01
**Implements:** all of [`features/r1-equity-paper-trading/`](../features/r1-equity-paper-trading/SPEC.md)
**Exit:** feature SPEC §11 in full — the 16 module checks, gate G12–G20 after G11, and the static checks green on macOS and Windows CI, plus E1 and E2 attended once per platform-and-runtime cell (agent-behavior aspects end inconclusive rather than failed, root §17).

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

## 1. Pins

Verified against crates.io and the crate sources on 2026-09-01. Chunk numbering, toolchain, and the R0 pins continue from slice 001 unchanged.

| Crate | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `nautilus-common`, `-core`, `-data`, `-execution`, `-live`, `-model`, `-sandbox` | `=0.62.0` | marketrigd | lockstep per D39; **`default-features = false` on every entry** — `nautilus-sandbox` alone defaults `high-precision`, and Cargo feature unification would silently flip the whole graph to 128-bit (the D76 landmine). The direct set may shrink at chunk time; the lockstep pin is the invariant. C11 found `LiveNode` and the data-event sender behind non-default features: `nautilus-live` needs `node`, `nautilus-common` needs `live` — both precision-neutral. |
| `anyhow` | `=1.0.104` | marketrigd | the `DataClient` trait's return type (found at C11; the line the nautilus graph already resolves) |
| `async-trait` | `=0.1.92` | marketrigd | the `DataClient` trait is `#[async_trait(?Send)]` (found at C11; same line as the nautilus graph) |
| `reqwest` | `=0.13.4` | marketrigd | the feed's transport — the exact line `nautilus-network` requires (`^0.13.4`), so one HTTP stack per D76; feature `json`; built with `Proxy::no_proxy()` so the gate's stand-in is never detoured through a machine proxy |
| `rust_decimal` | `=1.42.1` | marketrigd | fee rates on the catalog instruments (§2 below); equals nautilus-model's own requirement |
| `chrono` | `=0.4.45` | marketrigd | feature SPEC §2.2 calendars; the line the nautilus graph already carries |
| `chrono-tz` | `=0.10.4` | marketrigd | the three IANA zones, DST-correct |
| `rmcp` | `=3.2.0` | marketrig-mcp, acceptance | the line D4's wire evidence was gathered on, still newest today; server features in the adapter, client features in the harness — G19's own MCP client per D75 |

Reused R0 pins: `axum =0.8.9` (the stand-in feed server inside `marketrig-acceptance`), `ureq =3.4.0` + `serde`/`serde_json` (the adapter's daemon calls, through the shared lib target below), `tokio =1.53.1` (`nautilus-network` wants `^1.53.1` — compatible; gained the `time` feature at C10 for the retry backoff, recorded while Active as slice 001 did for `sync`).

**Endpoint layer — replaced, not pinned** (the choice D76 left to plan time): `yahoo_finance_api` 5.0.0 compiles its chart URL in privately — no builder override, its mock server is `#[cfg(test)]`-internal — so the R1-9 seam cannot pass through it. The feed's endpoint layer is MarketRig's own thin chart client (URL construction plus serde parsing of the chart response) on the pinned reqwest line. R1-1, feature SPEC §1, and D76 record this in the same change as this slice.

## 2. Plan-time settlements

Facts the feature docs deferred to slice time, each verified in the pinned 0.62.0 sources today:

- **Fee mechanism (R1-4):** both branches at once. `SandboxExecutionClientConfig.fee_model: Option<FeeModelAny>` is runtime-only and is set **explicitly** to `FeeModelAny::MakerTaker(MakerTakerFeeModel)`; each catalog `Equity` carries `maker_fee = taker_fee = <per-market per-side rate>` as `Decimal`. `MakerTakerFeeModel` (nautilus-execution `models/fee.rs`) charges notional × the instrument's own rate in the instrument's currency — the rate rides the instrument type's fee fields *through* an explicitly configured model, satisfying root §12.1's both-directions rule.
- **Precision assertion (`node::precision_asserted`):** `nautilus_model::types::fixed::HIGH_PRECISION_MODE == 0` and `PRECISION_BYTES == 8`, both public.
- **Shared daemon discovery:** `crates/marketrig` gains a library target exposing R0's endpoint discovery, health verification, and HTTP client; `marketrig-mcp` depends on it. No new shared crate.
- **Stand-in feed (R1-9):** an axum server inside `marketrig-acceptance`, scripted per scenario — advance price, 429 burst, go dark.
- **Seam interplay:** `MARKETRIG_TEST_NO_TRADING` keeps the daemon off the compiled-in public URL; it does not suppress polling a stand-in named by `MARKETRIG_TEST_QUOTE_URL` (clause added to feature SPEC §10.1 in this change).
- **Test targets:** G12–G20 extend `cargo test -p marketrig-acceptance --test gate` in order after G11; the empty `--test experiment` target gains E1/E2 as its first content.

## 3. Chunks

One coding-agent work unit each; a chunk is done when its named checks pass locally. Numbering continues from slice 001.

| # | Chunk | Builds (feature SPEC) | Lands with (§11 checks) | Needs |
| --- | --- | --- | --- | --- |
| C8 | Pins, migration 2, shared lib target | §1; §5 schema + widened operational kinds; the `marketrig` lib extraction | `store::trading_migration_applies`; static checks green with the new pins | — |
| C9 | Catalog and calendars | §2.2, §3 — every tick and lot venue-verified as the data lands (R1-2) | `catalog::entries_valid`, `feed::phase_from_calendar` | C8 |
| C10 | Feed client and market state | §2.1, §2.3, §10.1 seam | `feed::retry_on_429_bounded`, `feed::cadence_two_tier`, `feed::observation_provenance`, `feed::base_url_seam_only` | C9 |
| C11 | Node lifecycle and account | §4.1, §4.3 (restoration from an absent snapshot = fresh book; the round trip closes in C12) | `node::precision_asserted`, `node::sender_on_node_thread` | C10 |
| C12 | Order path, capture, snapshots, evaluation queue | §4.2, §5 write paths, §6 | `trade::cycle_and_prompt_atomic`, `trade::snapshot_restores_book`, `api::action_replay` | C11 |
| C13 | REST surface | §7 | `api::market_codes` | C12 |
| C14 | MCP adapter | §8 | `mcp::resources_and_freshness`, `mcp::server_side_validation` | C8 |
| C15 | CLI history group | §9 | `cli::history_exit_codes` | C8 |
| C16 | Gate and experiment | §10 | G12–G20 on both platforms; E1/E2 target content | all |

C9, C14, and C15 may run in parallel after C8 — C14 and C15 code against the pinned §7/§8/§9 contracts and fake endpoints, as C6 did in R0. C10→C13 are sequential along the node chain; C16 is last.

## 4. Execution

Orchestrator plus one coding agent per chunk, sequential along the Needs column, parallel where §3 allows; parallel chunks work in `.worktrees/<chunk>/` per AGENTS.md. Each agent receives this slice, the feature SPEC, and its chunk row, and delivers a diff with its checks green; the orchestrator runs the static checks after every merge. E1/E2 are operator-attended after C16 merges, one run per platform-and-runtime cell, evidence bundled per root §17.

## 5. Freeze and merge-back

When the exit checks are green: freeze this slice, then per AGENTS.md merge durable R1 mechanics into root `SPEC.md` (§12.2–§12.4, §13.1 URI grammar and tools, §13.2 history group, §15 migration 2, §17 third seam variable), summarize R1-1…R1-9 as one product `D<n>`, refresh `ROADMAP.md` (R1 delivered, evidence line), and grow the AGENTS.md **Commands** section with the experiment invocation.
