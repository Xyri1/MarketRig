# MarketRig — Agent Guide

MarketRig is a _vibe trading terminal for agents_: a local, persistent paper-trading harness in which an external coding agent (Codex CLI or Claude Code) observes markets, trades on a NautilusTrader sandbox, keeps desk-scoped memory and skills, sleeps, wakes, and keeps improving. MarketRig is the environment; the agent is the intelligence. The MVP is an experiment in persistent agent behavior, not a production trading system.

`CLAUDE.md` is `@AGENTS.md`; edit this file only.

## Repository state

- The root SDD set (`sdd/PRD.md`, `sdd/DECISIONS.md` through D84, `sdd/SPEC.md`, `sdd/ROADMAP.md`) was founded fresh on 2026-09-01 and is the only product truth. Milestones R0–R5 are delivered (slices `sdd/slices/001-r0-foundation.md`, `002-r1-equity-paper-trading.md`, `003-r2-scheduled-triggers.md`, `004-r3-runtime-delivery.md`, `005-r4-memory-skills-loop.md`, `006-r5-approval-policies.md`, `007-r5-shell-control-plane.md`, and `008-r5-tray-quit-smoke.md`, all frozen; per D77 through D82): the Cargo workspace, `marketrigd`, `marketrig`, `marketrig-mcp`, the equity paper-trading plane, scheduled triggers with code execution, runtime discovery, the terminal manager, the Codex and Claude Code adapters with dispatcher-driven delivery, the seeded constitution and skills, the two approval policies with the approvals routes and the events tail behind browser-grade sockets, the `marketrig-desktop` Tauri shell over the root Vue frontend, and the acceptance gate, experiment, and packaged smoke exist. Slice `008-r5-tray-quit-smoke.md` — close-hides, the tray, Quit, autostart, and the WebdriverIO packaged smoke — is implemented and frozen (2026-09-06), which closes R5 per D82; Milestone R6 (HiThink A-share data, per D84: the A-share feed, calendar, and research through `marketrig`; crypto deferred past MVP) is Active as slice `013-r6-hithink-a-share.md` over `sdd/features/hithink-a-share/`; R7 is event triggers, localization, and packaging, R8 acceptance. Corrective slice `009-r5-terminal-rendering.md` is implemented and frozen (2026-09-06): per-desk measurable rendering, WebGL fallback, Unicode 11 widths, ConPTY metadata, and reconnect continuity; native visual confirmation remains outstanding. Slice `010-r5-dev-lifecycle.md` (owned `pnpm dev` stack) is Active. Slice `011-openviking-migration.md` (OpenViking owns memory and skills; feature folder `sdd/features/openviking-continuity/`) is frozen (2026-09-08) and merged back as D83: the `openviking-server` child, the seeded upstream plugins, the read-only `.agents/skills/` projection, `marketrig skill put|delete`, `openviking-standin`, gate G1–G32 then O1–O10 (H1–H4 follow them, per R6), and E6 in place of E5, with the Windows E6 pair, R6's entry check, passed on 2026-09-08. Slice `014-a-share-engine.md` is frozen (2026-09-11), closed by operator confirmation: AE-1–AE-10 in `sdd/features/a-share-engine/` govern the delivered CN execution restrictions and staged E7; root-spec reconciliation is tracked in ROADMAP. Trigger-invocation design is complete in `sdd/features/event-triggers/` (2026-09-11; direct invocation replaces event-name matching per the amended D34) and slice `015-trigger-invocation.md` is implemented and frozen (2026-09-12: migration 10, the optional schedule, `POST …/triggers/{id}/invocations`, `marketrig trigger invoke`, the firing document's and result prompt's `invocation`; gate T1–T5 green on both platforms, E8 passed on all four cells, CI green at 0d3c94f; root SPEC §15 and §17 record migration 10 and T1–T5/E8, ROADMAP the R7 delivery). Localization design is complete in `sdd/features/localization/` (2026-09-12; LZ-1–LZ-7 refine D68) and slice `016-localization.md` is Active: migration 11's nullable `locale` column and `GET`/`PUT /settings/locale`, BCP 47 lookup over `navigator.languages` in the webview, the `zh-Hans` catalog, the Settings **Language** select, the shell's `set_locale`, gate L1 after T5, and the packaged smoke in `zh-Hans`. Corrective slice `012-limit-order-history.md` is Active: an order's stored chain is the node's own event list, so a limit order that fills on arrival — whose `OrderSubmitted` and `OrderAccepted` NautilusTrader never publishes — replays into `GET /desks/{id}/history/orders` (found by E6 on 2026-09-08; covered by G14).
- The daemon `marketrigd`, the CLI `marketrig`, and the stdio adapter `marketrig-mcp` are Rust binaries from one Cargo workspace (`crates/`, with `src-tauri/` a member); the Vue 3 frontend lives at the repository root; MarketRig ships no interpreter — the operator names a Python 3.12 and a Node ≥ 22, and the daemon provisions the supervised OpenViking memory child offline from the bundled wheels. See `sdd/SPEC.md` §3.
- When code lands, keep **Commands** below current in the same change. Feature folders under `sdd/features/` are created fresh as milestones activate.

## Where truth lives

| Need                                                             | File                                                                  |
| ---------------------------------------------------------------- | --------------------------------------------------------------------- |
| Why the product exists, MVP scope, success criteria              | `sdd/PRD.md`                                                          |
| Settled decisions with rationale (`D1`…`D83`)                    | `sdd/DECISIONS.md`                                                    |
| Current mechanical contract and invariants — the source of truth | `sdd/SPEC.md`                                                         |
| Milestones R0–R8, order, non-goals, deferred work                | `sdd/ROADMAP.md`                                                      |
| One feature's motivation / decisions / spec delta                | `sdd/features/<slug>/{PRD,DECISIONS,SPEC}.md` (created per milestone) |
| One slice's implementation plan (frozen once implemented)        | `sdd/slices/NNN-<slug>.md`                                            |
| Mechanics intentionally left unresolved                          | `sdd/SPEC.md` §18                                                     |

Reading order for any task: `PRD.md` → `DECISIONS.md` → `SPEC.md` → `ROADMAP.md` → the relevant `sdd/features/<slug>/`. Feature specs refine the product spec; they never contradict it without a recorded decision.

## How to work here (SDD)

1. Create `sdd/features/<slug>/` only when real content exists; never scaffold empty templates.
2. Write the feature PRD (motivation, outcome, scope, non-goals, success criteria), then DECISIONS, then SPEC with concrete scenarios and a closing **Required checks** section.
3. Implementation proceeds in slices: `sdd/slices/NNN-<slug>.md`, numbered sequentially, each the implementation plan for one or more features (or part of one), created only when that implementation is about to start. An active slice is the working plan; drift discovered during implementation is corrected in the feature docs in the same change — `sdd/features/` stays canonical, a slice never does. When a slice's exit checks are green, freeze it (status line; never edited again), then merge durable spec changes into `sdd/SPEC.md`, durable decisions into `sdd/DECISIONS.md`, and refresh `sdd/ROADMAP.md`.
4. Mark a roadmap item "design complete" only once its feature folder has all three documents.

Decision rules:

- Product decisions are `D<n>`, sequential, active only; cite them as `per D<n>`. Each entry is **Decision** / **Rationale** / **Contract** (links to the governing sections).
- Feature decisions use a short unique local prefix and are summarized as one product `D<n>` when merged.
- Changing a settled decision means editing it in place and updating every document that cites it in the same change.
- A `ponytail:` note inside a decision marks a deliberate ceiling and its upgrade path; keep it while the shortcut stands.

Document conventions:

- `sdd/SPEC.md` section numbers are link anchors cited across the repo; add subsections (`4.6`) rather than renumbering.
- A section that rests on decisions opens with `*Decision basis: per D…*`; extend it when you add a decision.
- Say "deferred" and point at `sdd/SPEC.md` §18 instead of inventing a schema, protocol, wording, or pin.
- Verify library facts against current documentation before pinning them in a spec, and name the version line you checked.
- The SDD set and feature folders never reference the pre-founding implementation — no migration framing, no legacy paths; the archived snapshot exists for humans, never for citations.

## Git worktrees

For isolated or parallel work, create a worktree under `.worktrees/<slug>/` from the repo root:

```bash
git worktree add .worktrees/<slug> -b <branch>
```

Work, build, and commit inside that directory. Remove it when done (`git worktree remove .worktrees/<slug>`). Use only `.worktrees/` — not sibling paths or `.claude/worktrees/`. Once daemons exist, set `MARKETRIG_TEST_DATA_ROOT` to a scratch dir in that worktree.

## Guardrails — do not cross without a recorded decision

Product boundary:

- MarketRig is a harness, not a trader: no daemon-owned reasoning, no `find_alpha` / `should_buy` / `choose_strategy`, no strategy engine, no risk-policy engine, no multi-agent orchestration, no live trading.
- MarketRig owns Observe, Act, time, and durable continuity; the agent owns Orient, Decide, Evaluate, and Learn. A realized-P&L event queues an evaluation prompt; it never forces a memory or skill write.
- Do not leak NautilusTrader, OpenBB, or OpenViking APIs as product contract. The agent surface is split per D4: the market plane (awareness resources and typed order tools) through the one `marketrig-mcp` adapter, the continuity plane (records, triggers, prompts, skill writes) through `marketrig`, and the memory plane through the seeded OpenViking plugin's own MCP server (per D4, D83). No capability appears on two.

Identity and state:

- One desk is one trader identity (UUIDv7 plus immutable kebab name); runtimes and native sessions are replaceable. There is no `Run` entity and no `INACTIVE | IDLE | WORKING | WAITING` agent-status state machine.
- Every desk-scoped operation carries the desk UUID end to end; desk-owned rows are keyed and scoped by `desk_id` referencing `desks`; the daemon has no process-global selected desk.
- `marketrigd` is the sole writer of authoritative state: SQLite through a thin binding, plain SQL, explicit `BEGIN IMMEDIATE`, WAL, `STRICT` tables, UUIDv7 text IDs, `*_ns` nanosecond instants, and decimal **text** for money — never `REAL`, never a float, never an ORM.
- NautilusTrader computes every trading fact; MarketRig stores its payloads verbatim and never recalculates P&L, fees, or averages. The daemon consumes the `nautilus-*` Rust crates pinned in lockstep (`=0.62.0`), never the Python/PyO3 surface (per D39); the numeric precision feature is set explicitly and asserted at startup.
- After a desk is `READY`, never rewrite the agent-owned `AGENTS.md`; MarketRig reconciles only `CLAUDE.md`, the `.claude/skills` link, and `.marketrig/plugins/`, and rewrites `.agents/skills/` only as the read-only projection of the desk's OpenViking skills (per D19–D21, D83).

Runtime and delivery:

- Daemon-to-agent input goes only through structured runtime paths: never keystroke emulation, never interrupting a turn, never an automatic retry of an uncertain delivery or trading action. Prefer supported programmatic interfaces everywhere.
- Activation is resume-first with explicit pointers (`codex resume <thread>`, `claude --resume <uuid>`); never ambient `--continue`; no prompt on the command line.
- Occurrences are candidates; firings exist only through atomic acceptance. Missed schedules become miss evidence, never catch-up firings. A one-off is consumed by its first accepted firing regardless of later failure.
- Secrets live only in the OS credential store behind the daemon: never in SQLite, logs, prompts, URLs, or CLI output. The two recorded exceptions are the OpenViking child's own environment and each desk's daemon-owned `0600` `ovcli.conf`; nothing is on a runtime process environment (per D49, D83).

Localization:

- The desktop, tray, and notifications ship in `en` and `zh-Hans`. Everything the agent consumes — CLI, MCP, JSON, error codes and messages, daemon prompts, seeded `AGENTS.md` and skills, logs — is English and byte-identical under both locales.

## Stack and layout

Settled per D30, D43, D47, D53–D62; see `sdd/SPEC.md` §3 for the architecture and §4.1 for packaging.

| Path         | Contents                                                                                                                                                                                   |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `/` (root)   | Vue 3 + TypeScript 6 + Vite frontend (Tailwind CSS 4, Reka UI 2, vue-i18n, xterm.js, Hey API-generated REST client; Node.js 24 LTS and pnpm 11 via Corepack); the Cargo workspace manifest |
| `crates/`    | `marketrigd` (library crate + thin binary; axum-served loopback API), `marketrig` (CLI), `marketrig-mcp` (stdio adapter, `rmcp =3.2.0`), shared internal crates                            |
| `src-tauri/` | Tauri 2 Rust shell, a member of the root workspace: window, tray, single instance, daemon bootstrap, no HTTP                                                                               |

Conventions:

- Use pnpm and Cargo directly; no monorepo tool, task runner, workspace framework beyond Cargo's own, or commit-hook framework.
- Pin dependencies exactly. Bumping a pin is a version change verified by that module's checks, not a new decision. Crates named in DECISIONS as candidates (axum, rusqlite, keyring, tracing, …) are pinned at plan time.
- No ORM, no abstraction with one implementation, no framework where the standard library or platform covers it.
- Tests: `cargo test` per Rust crate with both acceptance modes as workspace test targets; Vitest + Vue Test Utils + jsdom; WebdriverIO packaged desktop smoke. Checks: rustfmt, Clippy `-D warnings`, Prettier, correctness-only ESLint, `vue-tsc`.
- Each feature SPEC ends with **Required checks**; the implementing slice turns them into runnable tests before the work is considered done.

## Commands

The frontend is the repository root: `pnpm install` (Corepack provides the pinned pnpm), `pnpm generate` (rewrites `openapi.json` from `marketrigd --openapi` and regenerates `src/client`; both are committed and CI checks the diff), `pnpm check` (Prettier, ESLint, `vue-tsc`, Vitest), and `pnpm dev` (`scripts/dev.mjs`: builds and supervises `marketrigd`, Vite, and Tauri under `target/dev-data`, which needs the `src-tauri/` crate). `pnpm build` (`scripts/build.mjs`) refuses until `openviking-wheels/<platform>/` matches its lockfile (`node scripts/openviking-wheels.mjs --check`) and bundles that directory as a Tauri resource through `src-tauri/wheels.<platform>.conf.json`, passed with `--config` so plain `cargo` builds never copy it. `MARKETRIG_SMOKE_WIPE=1 pnpm build` then `MARKETRIG_SMOKE_WIPE=1 pnpm smoke` is the packaged desktop smoke (feature SPEC §7.3): operator-run, never in CI, it kills every MarketRig process, wipes the real per-user data root, both log roots, and `~/.marketrig`, drives the packaged application — the `.app` on macOS, `target/release/marketrig-desktop.exe` on Windows, built with the `wdio` Cargo feature, which compiles the embedded WebDriver server in; shipped builds never carry it — through `@wdio/tauri-service`, and leaves its report and the shell log in `target/acceptance/smoke-<platform>-<stamp>/`. Gate bundles carry the sealed skills projection (`0555`/`0444`), so `cargo clean` or `rm -rf target/acceptance` needs `chmod -R u+w target/acceptance` first on macOS. `node scripts/hithink-skill.mjs` rewrites the vendored HiThink skill (`vendor/hithink-finance/`, pinned in `sdd/features/hithink-a-share/DECISIONS.md` HT-5) into `crates/marketrigd/seed/skills/hithink-finance/` and `crates/marketrigd/src/research_paths.rs`, both committed; `--check` verifies the diff and is what CI and `cargo test -p marketrigd --lib skill::` run. Terminal-only frontend checks: `pnpm exec vitest run src/composables/useTerminal.test.ts src/composables/terminalParser.test.ts`; daemon terminal/socket checks: `cargo test -p marketrigd --lib terminal`. Everything else is Cargo:

```bash
cargo fmt --check                                  # formatting
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                             # module checks + gate
cargo test -p marketrig-acceptance --test gate     # the gate alone (G1–G32, then O1–O10, then H1–H4, then A1–A6, then T1–T5, ~30 min)
cargo run -p marketrigd -- --openapi > openapi.json  # the REST document the frontend client is generated from; no data root touched

# The attended experiment, one cell at a time; unset it and every cell skips.
# A cell runs E1 or E2, then E3, E4, E6, E7, and E8, serialized: they share the operator.
# E6 needs a real Python 3.12, Node >= 22, the locked wheel set, and a provider, and skips with evidence without them; E7 needs the real HiThink key and skips with evidence without it; E8 (a producer invokes a schedule-less trigger through `marketrig trigger invoke` and the replay answers `DUPLICATE`) needs only the cell's runtime.
node scripts/openviking-wheels.mjs --python <python3.12> --platform macos-arm64|windows-x64   # openviking-wheels/<platform>/ + its committed lockfile; --check verifies
# Each lockfile is generated on its own platform (pip evaluates environment markers against the host) and also names the pinned uv binary the script fetches beside the wheels and the daemon installs with; a download that drifts from the committed lockfile fails until --write-lock accepts it.
export MARKETRIG_EXPERIMENT_PYTHON=<python3.12> MARKETRIG_EXPERIMENT_NODE=<node> MARKETRIG_EXPERIMENT_WHEELS=<repo>/openviking-wheels/<platform>
export MARKETRIG_EXPERIMENT_MEMORY_BASE_URL=…  MARKETRIG_EXPERIMENT_MEMORY_API_KEY=…
export MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL=…  MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL=…
export MARKETRIG_EXPERIMENT_HITHINK_API_KEY=…   # E7; never echoed, deleted from the row before the bundle is kept
MARKETRIG_EXPERIMENT=codex  cargo test -p marketrig-acceptance --test experiment -- --nocapture
MARKETRIG_EXPERIMENT=claude cargo test -p marketrig-acceptance --test experiment -- --nocapture
```

The operator's procedure for an attended cell — timing, project-scoped adapter registration, driving E1/E2, E3, E4, E6, E7, and E8 (the last three make the console _the_ desk's terminal and register nothing, because MarketRig launches the runtime itself; E6 provisions the real OpenViking child offline from the wheels; E7 trades the CN leg on the real HiThink service), reading the bundle — is `crates/marketrig-acceptance/EXPERIMENT.md`.

Both acceptance modes build and drive the real binaries themselves — `marketrigd`, `marketrig`, `marketrig-mcp`, `trigger-code`, the acceptance-only helper every code-bearing trigger names as `argv[0]` (per `sdd/features/r2-scheduled-triggers/SPEC.md` §10.1), `runtime-standin`, the gate's stand-in runtime, which G27–G32 register by explicit path (per `sdd/features/r3-runtime-delivery/SPEC.md` §9.1), and `openviking-standin`, the gate's stand-in OpenViking child, which O1–O6 register through `PUT /openviking/setup {standin}` under the test seam and the daemon then starts, stops, and reprovisions itself (per `sdd/features/openviking-continuity/SPEC.md` §7.1), both scripted through the one JSON file `MARKETRIG_STANDIN_SCRIPT` names on the daemon's environment — the runtime stand-in reads its top-level keys, the OpenViking one the `openviking` object beside them (`ready_after_ms`, `exit_after_ready_ms`, `exit_code`, `commit_task_status`) — and write an evidence bundle to `target/acceptance/gate-<stamp>/`, `experiment-<cell>-<stamp>/`, `experiment-e3-<cell>-<stamp>/`, or `experiment-e4-<cell>-<stamp>/` (`MARKETRIG_ACCEPTANCE_OUT` overrides, so leave it unset for an experiment cell, whose three scenarios would then share one directory): `observations.jsonl` one JSON line per step, `marketrigd-N.stderr` per daemon, the relocated `data/`, `desks/`, and `logs/`, the gate's `scripts/`, and the experiment's `instructions.txt`, `instructions-e3.txt`, or `instructions-e4.txt`. The gate runs G1–G32, then O1–O6 (OpenViking setup, tenancy, projection, registration, loss and Retry, reprovision) and O7–O10, R5's four scenarios renumbered by `sdd/features/openviking-continuity/SPEC.md` §7.2, then H1–H4 (the HiThink provider and its key, the `CN` leg on both feeds, the research passthrough, the seeded skill's projection) against the same stand-in server's HiThink half named by `MARKETRIG_TEST_HITHINK_URL` (per `sdd/features/hithink-a-share/SPEC.md` §6), then A1–A6 (T+1 and reservation, bands and quantities, limit-fill policy, session and expiry, restart and data loss, sampled execution; per `sdd/features/a-share-engine/SPEC.md` §6), then T1–T5 (invoke, replay, and burst; the document; a one-off through both doors; refusals buffer nothing; duplicates survive restart; per `sdd/features/event-triggers/SPEC.md` §7.1, with no runtime registered so every queued prompt resolves `RUNTIME_UNAVAILABLE`); R4's old memory stand-in, G33–G37, and E5 are gone; E6 replaced E5 (per D83). Both modes also put trigger code on Always allow through `PUT /settings/policies` before their first code-bearing trigger — G21's prologue and E3's setup — because R5's installed default gates it (per `sdd/features/r5-desktop-approval-controls/SPEC.md` §7.1). The gate runs its own stand-in feed on loopback; the experiment polls real Yahoo and prints what the operator must do by hand. CI (`.github/workflows/ci.yml`) runs the three checks on macOS and Windows; the experiment stays operator-run.

Never run `marketrigd` or `marketrig` without `MARKETRIG_TEST_DATA_ROOT` pointing at a scratch directory: without it they write to the real per-user data root and `~/.marketrig`. `MARKETRIG_TEST_NO_TRADING=1` additionally keeps a daemon off the public market feed, and `MARKETRIG_TEST_QUOTE_URL` (honored only alongside the data root, and outranking `NO_TRADING`) points it at a stand-in feed instead (per `sdd/SPEC.md` §17 and `sdd/features/r1-equity-paper-trading/SPEC.md` §10.1). `MARKETRIG_TEST_HITHINK_URL` is the same seam for HiThink — honored only alongside the data root, outranking `NO_TRADING`, and lifting the A-share cadence gate the way the quote stand-in does (per `sdd/features/hithink-a-share/SPEC.md` §1.3, §3). `MARKETRIG_TEST_CLOCK_NS` is the controlled-clock seam — honored only alongside the data root — and seeds every node the daemon starts with a `TestClock` at that nanosecond instant, which the seam's own route `PUT /test/clock {"now_ns"}` (registered only under the seam, absent from the OpenAPI document) advances for every started node, dispatching the time events it releases, and — like the HiThink stand-in — lifts the A-share cadence gate, so a staged daemon keeps polling the real service while the wall-clock market is closed. Every CN session, trading-date, T+1 and 14:57 decision reads that clock, which is how H1–H4, A1–A6 and the attended E7 run at any wall-clock hour (per `sdd/features/a-share-engine/SPEC.md` §5.1, §6).

## Verification philosophy

- The MVP does not need to prove profitable trading, good strategy, or good learning. It must prove the loop: observe → decide → define later work → trigger fires → paper action → authoritative outcome → durable history → queued realized-P&L evaluation → agent retains a lesson or improves a skill → a later session reuses them.
- Prefer the smallest end-to-end experiment that validates that loop over broad feature completeness.
- Three layers (per D75, `sdd/SPEC.md` §17): each feature's **Required checks** (unit/module, fakes allowed); the acceptance **gate** (the scenario chain, unattended and deterministic, on stand-ins, grown scenario by scenario from R0); and the same chain attended as the **experiment**, one run per platform/runtime cell on the real CLIs and real OpenViking, whose agent-owned scenarios end inconclusive rather than failed. Do not restate one layer in another; a failure found by the experiment gets its regression in the gate or a module check.
- When an adjacent capability looks attractive, ask whether the active milestone's evidence needs it; if not, defer it in `sdd/ROADMAP.md`.
