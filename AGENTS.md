# MarketRig — Agent Guide

MarketRig is a *vibe trading terminal for agents*: a local, persistent paper-trading harness in which an external coding agent (Codex CLI or Claude Code) observes markets, trades on a NautilusTrader sandbox, keeps desk-scoped memory and skills, sleeps, wakes, and keeps improving. MarketRig is the environment; the agent is the intelligence. The MVP is an experiment in persistent agent behavior, not a production trading system.

`CLAUDE.md` is `@AGENTS.md`; edit this file only.

## Repository state

- The root SDD set (`sdd/PRD.md`, `sdd/DECISIONS.md` through D81, `sdd/SPEC.md`, `sdd/ROADMAP.md`) was founded fresh on 2026-09-01 and is the only product truth. Milestones R0–R4 are delivered (slices `sdd/slices/001-r0-foundation.md`, `002-r1-equity-paper-trading.md`, `003-r2-scheduled-triggers.md`, `004-r3-runtime-delivery.md`, and `005-r4-memory-skills-loop.md`, all frozen; per D77, D78, D79, D80, and D81): the Cargo workspace, `marketrigd`, `marketrig`, `marketrig-mcp`, the equity paper-trading plane, scheduled triggers with code execution, runtime discovery, the terminal manager, the Codex and Claude Code adapters with dispatcher-driven delivery, the supervised Hindsight child with desk memory, the seeded constitution and skills, and the acceptance gate and experiment exist. Milestone R5 (desktop and approval controls) is design complete in `sdd/features/r5-desktop-approval-controls/` (2026-09-04); its first slice `sdd/slices/006-r5-approval-policies.md` — the installation policies, trigger-code and paper-order approval, the approvals routes, the events tail with browser-grade sockets, and `marketrigd --openapi` — is implemented and frozen (2026-09-05, gate G38–G41); slice `007-r5-shell-control-plane.md` — the `marketrig-desktop` Tauri crate, the root Vue frontend over the generated client, the composables and panels, and CI's `frontend` job — is implemented and frozen (2026-09-05); slice `008-r5-tray-quit-smoke.md` (close-hides, tray, Quit, the packaged smoke) is Active and not started.
- The daemon `marketrigd`, the CLI `marketrig`, and the stdio adapter `marketrig-mcp` are Rust binaries from one Cargo workspace (`crates/`, with `src-tauri/` a member); the Vue 3 frontend lives at the repository root; the one interpreter MarketRig ships runs only the supervised Hindsight memory child. See `sdd/SPEC.md` §3.
- When code lands, keep **Commands** below current in the same change. Feature folders under `sdd/features/` are created fresh as milestones activate.

## Where truth lives

| Need | File |
| --- | --- |
| Why the product exists, MVP scope, success criteria | `sdd/PRD.md` |
| Settled decisions with rationale (`D1`…`D81`) | `sdd/DECISIONS.md` |
| Current mechanical contract and invariants — the source of truth | `sdd/SPEC.md` |
| Milestones R0–R7, order, non-goals, deferred work | `sdd/ROADMAP.md` |
| One feature's motivation / decisions / spec delta | `sdd/features/<slug>/{PRD,DECISIONS,SPEC}.md` (created per milestone) |
| One slice's implementation plan (frozen once implemented) | `sdd/slices/NNN-<slug>.md` |
| Mechanics intentionally left unresolved | `sdd/SPEC.md` §18 |

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
- Do not leak NautilusTrader, OpenBB, or Hindsight APIs as product contract. The agent surface is split per D4: the market plane (awareness resources and typed order tools) through the one `marketrig-mcp` adapter, the continuity plane (records, triggers, memory, prompts) through `marketrig`. No capability appears on both.

Identity and state:

- One desk is one trader identity (UUIDv7 plus immutable kebab name); runtimes and native sessions are replaceable. There is no `Run` entity and no `INACTIVE | IDLE | WORKING | WAITING` agent-status state machine.
- Every desk-scoped operation carries the desk UUID end to end; desk-owned rows are keyed and scoped by `desk_id` referencing `desks`; the daemon has no process-global selected desk.
- `marketrigd` is the sole writer of authoritative state: SQLite through a thin binding, plain SQL, explicit `BEGIN IMMEDIATE`, WAL, `STRICT` tables, UUIDv7 text IDs, `*_ns` nanosecond instants, and decimal **text** for money — never `REAL`, never a float, never an ORM.
- NautilusTrader computes every trading fact; MarketRig stores its payloads verbatim and never recalculates P&L, fees, or averages. The daemon consumes the `nautilus-*` Rust crates pinned in lockstep (`=0.62.0`), never the Python/PyO3 surface (per D39); the numeric precision feature is set explicitly and asserted at startup.
- After a desk is `READY`, never rewrite agent-owned files (`AGENTS.md`, `.agents/skills/`); MarketRig reconciles only `CLAUDE.md` and the `.claude/skills` link.

Runtime and delivery:

- Daemon-to-agent input goes only through structured runtime paths: never keystroke emulation, never interrupting a turn, never an automatic retry of an uncertain delivery or trading action. Prefer supported programmatic interfaces everywhere.
- Activation is resume-first with explicit pointers (`codex resume <thread>`, `claude --resume <uuid>`); never ambient `--continue`; no prompt on the command line.
- Occurrences are candidates; firings exist only through atomic acceptance. Missed schedules become miss evidence, never catch-up firings. A one-off is consumed by its first accepted firing regardless of later failure.
- Secrets live only in the OS credential store behind the daemon: never in SQLite, logs, prompts, URLs, or CLI output. The single recorded exception is the Hindsight child's environment (per D49).

Localization:

- The desktop, tray, and notifications ship in `en` and `zh-Hans`. Everything the agent consumes — CLI, MCP, JSON, error codes and messages, daemon prompts, seeded `AGENTS.md` and skills, logs — is English and byte-identical under both locales.

## Stack and layout

Settled per D30, D43, D47, D53–D62; see `sdd/SPEC.md` §3 for the architecture and §4.1 for packaging.

| Path | Contents |
| --- | --- |
| `/` (root) | Vue 3 + TypeScript 6 + Vite frontend (Tailwind CSS 4, Reka UI 2, vue-i18n, xterm.js, Hey API-generated REST client; Node.js 24 LTS and pnpm 11 via Corepack); the Cargo workspace manifest |
| `crates/` | `marketrigd` (library crate + thin binary; axum-served loopback API), `marketrig` (CLI), `marketrig-mcp` (stdio adapter, `rmcp =3.2.0`), shared internal crates |
| `src-tauri/` | Tauri 2 Rust shell, a member of the root workspace: window, tray, single instance, daemon bootstrap, no HTTP |

Conventions:

- Use pnpm and Cargo directly; no monorepo tool, task runner, workspace framework beyond Cargo's own, or commit-hook framework.
- Pin dependencies exactly. Bumping a pin is a version change verified by that module's checks, not a new decision. Crates named in DECISIONS as candidates (axum, rusqlite, keyring, tracing, …) are pinned at plan time.
- No ORM, no abstraction with one implementation, no framework where the standard library or platform covers it.
- Tests: `cargo test` per Rust crate with both acceptance modes as workspace test targets; Vitest + Vue Test Utils + jsdom; WebdriverIO packaged desktop smoke. Checks: rustfmt, Clippy `-D warnings`, Prettier, correctness-only ESLint, `vue-tsc`.
- Each feature SPEC ends with **Required checks**; the implementing slice turns them into runnable tests before the work is considered done.

## Commands

The frontend is the repository root: `pnpm install` (Corepack provides the pinned pnpm), `pnpm generate` (rewrites `openapi.json` from `marketrigd --openapi` and regenerates `src/client`; both are committed and CI checks the diff), `pnpm check` (Prettier, ESLint, `vue-tsc`, Vitest), and `pnpm dev` (`tauri dev`, which needs the `src-tauri/` crate). Everything else is Cargo:

```bash
cargo fmt --check                                  # formatting
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                             # module checks + gate
cargo test -p marketrig-acceptance --test gate     # the gate alone (G1–G41, ~10 min)
cargo run -p marketrigd -- --openapi > openapi.json  # the REST document the frontend client is generated from; no data root touched

# The attended experiment, one cell at a time; unset it and every cell skips.
# A cell runs E1 or E2, then E3, E4, and E5, serialized: they share the operator.
# E5 needs the real memory child and provider, and skips with evidence without them.
export MARKETRIG_EXPERIMENT_HINDSIGHT=<venv>/bin/hindsight-api
export MARKETRIG_EXPERIMENT_MEMORY_BASE_URL=…  MARKETRIG_EXPERIMENT_MEMORY_API_KEY=…
export MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL=…  MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL=…
MARKETRIG_EXPERIMENT=codex  cargo test -p marketrig-acceptance --test experiment -- --nocapture
MARKETRIG_EXPERIMENT=claude cargo test -p marketrig-acceptance --test experiment -- --nocapture
```

The operator's procedure for an attended cell — timing, project-scoped adapter registration, driving E1/E2, E3, E4, and E5 (the last two make the console *the* desk's terminal and register nothing, because MarketRig launches the runtime itself; E5 also needs a uv environment carrying the Hindsight wheel), reading the bundle — is `crates/marketrig-acceptance/EXPERIMENT.md`.

Both acceptance modes build and drive the real binaries themselves — `marketrigd`, `marketrig`, `marketrig-mcp`, `trigger-code`, the acceptance-only helper every code-bearing trigger names as `argv[0]` (per `sdd/features/r2-scheduled-triggers/SPEC.md` §10.1), `runtime-standin`, the gate's stand-in runtime, which G27–G32 register by explicit path (per `sdd/features/r3-runtime-delivery/SPEC.md` §9.1), and `memory-standin`, the gate's stand-in memory child, which G33–G37 register as the memory launcher and which the gate also starts itself as the provider (per `sdd/features/r4-memory-skills-loop/SPEC.md` §7.1) — both scripted through the one JSON file `MARKETRIG_STANDIN_SCRIPT` names on the daemon's environment — and write an evidence bundle to `target/acceptance/gate-<stamp>/`, `experiment-<cell>-<stamp>/`, `experiment-e3-<cell>-<stamp>/`, `experiment-e4-<cell>-<stamp>/`, or `experiment-e5-<cell>-<stamp>/` (`MARKETRIG_ACCEPTANCE_OUT` overrides, so leave it unset for an experiment cell, whose four scenarios would then share one directory): `observations.jsonl` one JSON line per step, `marketrigd-N.stderr` per daemon, the relocated `data/`, `desks/`, and `logs/`, the gate's `scripts/`, and the experiment's `instructions.txt`, `instructions-e3.txt`, `instructions-e4.txt`, or `instructions-e5.txt`. Both modes also put trigger code on Always allow through `PUT /settings/policies` before their first code-bearing trigger — G21's prologue and E3's setup — because R5's installed default gates it (per `sdd/features/r5-desktop-approval-controls/SPEC.md` §7.1). The gate runs its own stand-in feed on loopback; the experiment polls real Yahoo and prints what the operator must do by hand. CI (`.github/workflows/ci.yml`) runs the three checks on macOS and Windows; the experiment stays operator-run.

Never run `marketrigd` or `marketrig` without `MARKETRIG_TEST_DATA_ROOT` pointing at a scratch directory: without it they write to the real per-user data root and `~/.marketrig`. `MARKETRIG_TEST_NO_TRADING=1` additionally keeps a daemon off the public market feed, and `MARKETRIG_TEST_QUOTE_URL` (honored only alongside the data root, and outranking `NO_TRADING`) points it at a stand-in feed instead (per `sdd/SPEC.md` §17 and `sdd/features/r1-equity-paper-trading/SPEC.md` §10.1).

## Verification philosophy

- The MVP does not need to prove profitable trading, good strategy, or good learning. It must prove the loop: observe → decide → define later work → trigger fires → paper action → authoritative outcome → durable history → queued realized-P&L evaluation → agent retains a lesson or improves a skill → a later session reuses them.
- Prefer the smallest end-to-end experiment that validates that loop over broad feature completeness.
- Three layers (per D75, `sdd/SPEC.md` §17): each feature's **Required checks** (unit/module, fakes allowed); the acceptance **gate** (the scenario chain, unattended and deterministic, on stand-ins, grown scenario by scenario from R0); and the same chain attended as the **experiment**, one run per platform/runtime cell on the real CLIs and real Hindsight, whose agent-owned scenarios end inconclusive rather than failed. Do not restate one layer in another; a failure found by the experiment gets its regression in the gate or a module check.
- When an adjacent capability looks attractive, ask whether the active milestone's evidence needs it; if not, defer it in `sdd/ROADMAP.md`.
