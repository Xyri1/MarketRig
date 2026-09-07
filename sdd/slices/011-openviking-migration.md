# Slice 011 — OpenViking memory and skills migration

**Status:** Active — C50–C58 implemented on `master` 2026-09-07 (12ebee6 … 714bf9c); the gate G1–G32 then O1–O10 is green on macOS (bundles `target/acceptance/gate-1788772239`, `gate-1788772789`, `gate-1788774270`); offline install of the locked wheel set verified on macOS arm64 with Python 3.12.x (uv-managed CPython, `openviking-server 0.4.17.1`, 159 wheels, 2026-09-07). Windows evidence (2026-09-07, FSOCIETY via a Remote Control session, tree 82591d3): workspace checks 176 pass, gate G1–G32 then O1–O10 green (bundles `gate-1788787849` at 71ba19b, `gate-1788790021` at 82591d3, ~600 s), `pnpm check` green, offline wheel install from `openviking-wheels/windows-x64/` on CPython 3.12.12 with no network (`openviking-server 0.4.17.1`); the Windows lockfile has 163 entries because pip evaluates `sys_platform` markers on the host, so each lockfile is produced on its own platform. Two Windows-found defects were fixed on the way: the POSIX candidate paths (b0cca41) and the suspended `trigger-code` orphan, now recorded in `children.json` and reaped by a recovery arm that previously killed nothing on Windows (82591d3). Still open before freeze: CI green on both platforms at the final commit, and E6 once per cell (needs a provider; no cell has run). Two corrections found during implementation are recorded in the feature docs: OpenViking's MCP `write`/`edit` refuse the `skills/` subtree, so skills are written through `marketrig skill put`/`delete` (SPEC §5.5, OV-4/OV-5); and `/ready` runs a live embeddings probe, so a refused provider is a deadline loss, not a `READY` child (SPEC §2.2/§2.3). The feature folder `sdd/features/openviking-continuity/` (PRD, DECISIONS OV-1…OV-7, SPEC §1–§8) was written 2026-09-07 and is canonical for this work; implementation has not started. Every mechanic the SPEC pins was resolved from upstream source, CI, and current runtime documentation on 2026-09-07; the one execution-dependent item, installing the locked wheel set offline on Python 3.12 on both platforms, is exit evidence below.

## Outcome

Replace the Hindsight integration with OpenViking as the owner of both desk memory and skills, including the procedure save → reuse → revise → later reuse loop. This is a change to continuity ownership and integration, not a provider rename.

The user explicitly opened this slice during design. Before implementation, establish the canonical feature PRD, DECISIONS, and SPEC under `sdd/features/openviking-continuity/`, with concrete scenarios and Required checks. This working plan does not supersede the current product contract; reconcile affected settled decisions and their references when the replacement contract is settled. Frozen slices remain historical evidence.

## Session direction

- OpenViking will handle both memory and skills.
- Deployment is local and Docker is excluded.
- Python is a user-installed prerequisite; MarketRig manages a private dependency environment without modifying global Python packages.
- Bundle OpenViking's wheel and the complete locked dependency wheel set for each supported platform. Install from those bundled artifacts during setup; dependency installation is offline, while hosted model operations still need network access.
- MarketRig's accepted Python versions must be validated against the complete bundled dependency wheel set, not inferred from OpenViking's >=3.10 declaration alone. The exact accepted range remains to be established by that validation.
- Once configured, OpenViking starts with `marketrigd` and stops with it. The daemon owns readiness, failure reporting, and process cleanup; there is no lazy startup or independent service lifecycle. OpenViking startup failure is explicit without preventing the trading harness from running.
- Work so far is documentation-only feasibility research. No OpenViking installation or live integration has been tested.

Settled 2026-09-07 (product decisions; the feature DECISIONS will record each with rationale and the root decisions they supersede):

- **Skills:** OpenViking is canonical. The daemon projects the desk's skills into `.agents/skills/` before every activation and after every skill write; the projection is MarketRig-owned and read-only, and the agent writes skills only through the plugin's tools. Supersedes D19–D21's agent-owned file tree.
- **Ingestion and recall:** automatic transcript capture and injected recall through OpenViking's upstream Claude Code and Codex plugins, shipped locally and seeded project-scoped into each desk folder. The agent's memory operations go through the plugin's hooks and MCP tools; skills are written through `marketrig skill` because OpenViking exposes skill creation on REST alone (found during C55, 2026-09-07). `marketrig` keeps no memory command. Supersedes D16's no-transcript and no-native-integration rules and D17's agent-chooses rule; the memory plane leaves the continuity CLI (D4 to be reconciled).
- **Extraction:** `session_skill_extraction_enabled` stays off. Only the agent writes skills; OpenViking extracts memories only.
- **Isolation:** `api_key` mode. The daemon mints a root key per start, creates account `marketrig` and one user per desk through the admin API, and obtains each desk's key at every child readiness. Refined by verification (OV-3, OV-4): the plugin ignores credentials in workspace config and reads them from environment first, so the key reaches hooks and the MCP proxy through the runtime process environment the daemon already owns — an extension of D49's existing exception, with no key in any file — and the key is derived from one installation seed in the credential store, so it survives child restarts and Retry.
- **Models:** unchanged shape — one user-configured OpenAI-compatible base URL and API key serving both the VLM and embeddings, key held in the credential store and written into the child's configuration at start.
- **Python:** exactly one minor accepted, 3.12 (the one upstream builds and tests on both platforms); an operator-named absolute interpreter path validated by running it; the private environment at `<data root>/openviking/venv`, provisioned offline from bundled wheels.
- **Node:** a second user-installed prerequisite for the plugin hooks and MCP proxy, operator-named, validated at setup, and referenced by absolute path from the seeded hook commands.
- **Hindsight:** removed outright. No import, no migration, no compatibility; existing data roots are not read.
- **License:** OpenViking's AGPL-3.0 wheels are bundled unmodified in the installer and run as a separate loopback process.
- **Seeds:** creation writes the rewritten `AGENTS.md`, the `CLAUDE.md` shim, the plugin registration and per-desk config, and the `.claude/skills` link; the `desk-improvement` skill is uploaded into the desk's OpenViking user and reaches the workspace through the projection.
- **Lifecycle:** starts with the daemon; a loss is `UNAVAILABLE` at once with no automatic restart, cleared by Retry; sessions, triggers, and trading continue and the projection keeps its last files.
- **Gate:** `openviking-standin` replaces `memory-standin`, serving the consumed HTTP subset (health and ready, admin accounts, users, and key regeneration, skills CRUD, sessions and commit with a scripted task, find and read) behind `api_key` auth over an in-memory store; the plugin's hook scripts run only in the attended experiment.

Verified facts these rest on (2026-09-07, OpenViking 0.4.17.1 and `main`): no root key means `dev` mode where every caller is ROOT with no user binding; user keys reach only their own user's data and ROOT cannot read tenant data; skills live at `viking://~/skills/<name>/` with abstract, overview, and `SKILL.md` layers and a REST CRUD surface including auxiliary files; nothing in OpenViking or its plugins exports skills to the agent's filesystem — the Claude Code plugin's only skill hook reads a local `SKILL.md` and injects usage experience; commit archives synchronously and extracts behind a persisted task id; abi3 wheels exist for macOS 14+ arm64 and win_amd64 with no compiler needed; no graceful-shutdown contract is documented, and upstream's own CI starts and drives the server on `windows-latest` and `macos-14` across Python 3.10–3.13, building release wheels on 3.12. The per-mechanic facts (config field names, `409` on `AlreadyExists`, the seeded key formula, Codex hook trust and its bypass flag, the vendored plugin commit) are cited in the feature SPEC.

## Implementation sequence

Design is complete: the feature folder holds the PRD, DECISIONS OV-1…OV-7, and SPEC §1–§9. Chunks land in order on `master`, each green on its named checks before the next starts, each keeping **Commands** in `AGENTS.md` current when a command changes. Chunk checks cite SPEC §9 by number.

- **C50 — Migration 7 and Hindsight removal** (SPEC §6). Drop `memory_child`, add `openviking_setup`, rebuild the event kinds, delete `crates/marketrigd/src/memory.rs`'s child, bank, and pass-through code, the `marketrig memory` group, `memory-standin`, G33–G37, E5, and the frontend's memory calls; keep `memory_provider` and its route. Checks 9, and the workspace suite green with the gate ending at G32 plus the renumbered O7–O10.
- **C51 — Setup and provisioning** (§1). The row, `GET /openviking`, `PUT /openviking/setup` with validation, the provisioning task, `openviking_seed`, the `standin` seam. Checks 1, 2.
- **C52 — Provider dimension and `ov.conf`** (§2.1). `embedding_dimension` measured at `PUT /memory/provider`, the rendered config as a seed file. Check 3.
- **C53 — Child lifecycle and tenancy** (§2.2, §2.3, §3). Start with the daemon, `/ready`, loss, Retry, stop, recovery; account, per-desk users, seeded keys in memory; redaction. Checks 4, 5, 10.
- **C54 — Vendored plugins, registration, and launch environment** (§4). The seed trees, workspace copies and reconcile, both runtimes' registration files with the `commandWindows` form and the Codex bypass flag, the environment set, the `UNCONFIGURED` form. Checks 6, 8 (plugin part).
- **C55 — Skills projection, seeds, and constitution** (§5). Projection at activation and turn end, permissions on both platforms, the seed upload, the rewritten `AGENTS.md` and `desk-improvement` texts. Checks 7, 8.
- **C56 — `openviking-standin` and gate O1–O6** (§7.1, §7.2). Green on both platforms in CI.
- **C57 — Desktop** (§8). Settings section, client regeneration, notification kind, smoke step, Vitest. Frontend checks.
- **C58 — Wheel lockfile and E6** (§1.3, §7.3). `scripts/openviking-wheels.mjs` producing `openviking-wheels/<platform>/` and its lockfile with `pip download` on 3.12; the offline install verified on macOS arm64 and Windows x64 and recorded here; E6 run once per cell and its bundles named here.

Then freeze this slice and merge back: one product `D<n>` summarizing OV-1…OV-7; D4, D16, D17, D19, D20, D21, D47, D49, D65, and D81 edited in place; root SPEC §2, §4.1, §4.4, §4.6, §5.1, §5.2, §7, §13, §14, §15, §16, §17, and §18; ROADMAP R4 and R6's packaging line; `AGENTS.md` guardrails and commands.

## Documentation feasibility baseline

Checked in this session against OpenViking **0.4.17.1**; this is a research baseline, not an implementation pin:

- Published wheels include macOS ARM64 (`macosx_14_0_arm64`) and Windows x64 (`win_amd64`); Python requirement is >=3.10. The prebuilt macOS route therefore implies macOS 14+.
- Local storage and standalone HTTP serving are documented. RAGFS uses an in-process native binding; the server also supports a local vector backend.
- Explicit configuration/workspace paths and a readiness endpoint are documented. A packaged private environment, full writable-path confinement, credential handling, clean-machine dependency completeness, and bounded shutdown still need verification.

Sources: [release files](https://pypi.org/project/openviking/0.4.17.1/#files), [release manifest](https://github.com/volcengine/OpenViking/blob/v0.4.17.1/pyproject.toml), [release FAQ](https://github.com/volcengine/OpenViking/blob/v0.4.17.1/docs/en/faq/faq.md), [server configuration](https://github.com/volcengine/OpenViking/blob/v0.4.17.1/docs/en/configuration/01-server.md), [deployment](https://github.com/volcengine/OpenViking/blob/v0.4.17.1/docs/en/guides/03-deployment.md). The release declares AGPL-3.0; distribution treatment remains to be settled.

## Exit evidence to make runnable after design

- Offline setup from bundled locked wheels with no compiler, and repair with a supported user-installed Python on macOS ARM64 and Windows x64, using scratch storage and preserving existing data. Verify every accepted Python/platform combination and reject unsupported interpreters explicitly.
- Desk A retains an experience and saves a skill; a later session reuses both, revises the skill after a correction, and a later runtime retrieves the revision. Desk B cannot access A's memory or skills.
- Background acceptance is distinguished from completion; restart and failure leave explicit outcomes, without duplicating uncertain writes.
- Server loss leaves trading, triggers, and sessions usable; shutdown and recovery leave no unintended owned processes. Credentials remain outside logs, authoritative rows, and agent files.
- A configured OpenViking starts with the daemon before any memory or skill request; daemon shutdown stops it. An OpenViking startup failure is reported while the trading harness remains available.
- Feature module checks, adapted deterministic acceptance scenarios, and attended real-runtime evidence on both platforms, with agent-owned learning outcomes allowed to be inconclusive.

Freeze only after the final feature's Required checks and this slice's implementation exit checks pass; then merge durable changes into the root SDD and refresh the roadmap and agent guide. Slice 010 retains its independent status.
