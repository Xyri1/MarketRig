# OpenViking continuity — Feature PRD

**Slice:** [011 — OpenViking memory and skills migration](../../slices/011-openviking-migration.md)
**Status:** Design complete — PRD, DECISIONS, and SPEC written 2026-09-07; implementation not started

This feature replaces the Hindsight memory child and the agent-owned skill tree with OpenViking as the one owner of a desk's experiential memory and procedures. It changes `sdd/SPEC.md` §2, §4.1, §4.4, §4.6, §5.1, §5.2, §7, §13.2, §14, §15, §16, and §17 and supersedes D16, D17, D19, D21, D47, D65, and D81 in the parts the DECISIONS name. It is a change of continuity ownership, not a provider rename.

## 1. Motivation

*Decision basis: per D4, D18, D20, D22, D49, D73; superseding parts of D16, D17, D19, D21, D47, D65, D81.*

R4 closed the loop with two separate stores that nothing connects: lessons go to a Hindsight bank the agent must remember to write, and procedures are files the agent must remember to edit, so the same experience has to be retained twice and neither store knows about the other. The retain step depends on the agent choosing to call a CLI at the end of an evaluation, which the E5 cells showed happens only when the constitution says so and the model complies. OpenViking keeps memories and skills in one per-user context store with one retrieval surface, captures sessions through its own Claude Code and Codex integrations, and extracts typed memories in the background, so a desk's experience accumulates without the agent doing bookkeeping and its procedures are retrievable next to the experience that shaped them. The project is in development with no released users, so the Hindsight path is removed rather than kept beside the new one.

## 2. Outcome

A desk's sessions are captured as they happen. OpenViking, started by `marketrigd` and stopped with it, holds one user per desk; the OpenViking Claude Code and Codex plugins, shipped inside MarketRig and seeded project-scoped into each desk workspace and handed that desk's key through the runtime process environment, append every turn to an OpenViking session, commit it, and inject recalled memory at session start and before each prompt. The agent reads, writes, and deletes its memories and skills through the plugins' MCP tools. Skills live in OpenViking; before every activation the daemon writes the desk's skills into `.agents/skills/` so both runtimes discover them natively, and a skill the agent writes in one session is on disk for the next session on either runtime. Desk B's plugin and agent cannot reach desk A's user. OpenViking being down leaves trading, triggers, and sessions untouched and is visible as one explicit state with a Retry.

## 3. Scope

1. **Setup** (per D18, D49): two operator-named prerequisites, Python and Node, each validated by running it; a private environment at `<data root>/openviking/venv` provisioned offline from the wheel set bundled per platform; the provider row unchanged in shape — one OpenAI-compatible base URL, key in the credential store, one VLM model, one embedding model — written into the child's configuration at start.
2. **The child** (per D73): `openviking-server` started with the daemon under the containment primitive on a daemon-picked loopback port, workspace and configuration under `<data root>/openviking/`, `api_key` mode behind a per-start root key, readiness on `/ready`, stopped on daemon shutdown, and marked `UNAVAILABLE` on the first loss until Retry.
3. **Desk identity**: account `marketrig`, one OpenViking user per desk, its key derived from one installation seed in the credential store, obtained at every child readiness, and held in daemon memory only.
4. **The seeded plugins**: the upstream Claude Code and Codex memory plugins, vendored unmodified into MarketRig and seeded into each workspace, registered through the launch files the daemon already writes, and configured through the runtime process environment; MarketRig owns and reconciles the seeded files.
5. **Skills projection**: OpenViking canonical, `.agents/skills/` a MarketRig-owned read-only projection refreshed before every activation, the `.claude/skills` link kept; the `desk-improvement` skill seeded into the desk's user at creation.
6. **The constitution** rewritten for the plugin's tools and the projection rule; `marketrig` loses every memory and skill command.
7. **Durable evidence**: the setup and child rows, the event kinds, and recovery through `children.json`.
8. **Acceptance**: `openviking-standin` in place of `memory-standin`, adapted gate scenarios, and one attended scenario per cell on real OpenViking and the real plugins.

## 4. Non-goals

- No import, migration, or reading of Hindsight data; no compatibility shim; no coexistence of both memory systems.
- No daemon-authored memory beyond what the upstream plugins capture: the daemon commits nothing itself and never mutates a skill's content.
- No OpenViking skill extraction: `session_skill_extraction_enabled` stays off; only the agent writes skills.
- No automatic restart of the child.
- No bundled Python or Node; no interpreter search; one validated Python minor.
- No OpenViking surface of MarketRig's own: no `marketrig memory`, no `marketrig skill`, no REST pass-through for memory content, no proxying of OpenViking's MCP through `marketrig-mcp`.
- No modification of the upstream plugin scripts; MarketRig changes only their configuration and registration.
- No local models, no reranker, no multi-user OpenViking beyond one account and one user per desk.
- No trigger-code memory path: code runs with no session alive and has no plugin; its result reaches memory when the session reads the `TRIGGER_RESULT` prompt.

## 5. Success criteria

1. On both platforms, setup from the bundled wheels with the validated Python and no network reaches a child that answers `/ready`; an interpreter of any other minor, or a missing Node, is rejected with an explicit code and the daemon keeps serving desks.
2. A configured child starts with the daemon before any desk is activated and stops when the daemon stops; a hard-killed daemon leaves no child alive on macOS and the next start's recovery names what it reaped.
3. A session on desk A ends a turn; the plugin's capture reaches OpenViking as a session of desk A's user; the same query from desk B's key answers nothing of A's.
4. A skill the agent writes through the plugin on desk A appears under `.agents/skills/` at the next activation on either runtime, and a later edit replaces it; a direct edit to the projected file is refused by the filesystem and named in the constitution.
5. A desk created after this feature carries the rewritten constitution, the seeded plugin files, the `.claude/skills` link, and the seeded `desk-improvement` skill in its OpenViking user, projected on first activation.
6. With the child lost, its state is `UNAVAILABLE` with the last output line; a trigger firing, an order, and an activation succeed; Retry starts it again.
7. The provider key, the root key, and every desk key are absent from SQLite, the daemon log, operational events, prompts, and CLI output; a desk key exists only in that desk's runtime process environment.
8. The gate covers criteria 2, 4, 5, 6, and 7 on `openviking-standin` on both platforms; the attended scenario reproduces 1, 3, and 4 once per platform-and-runtime cell on real OpenViking and the real plugins, with the agent-owned steps ending inconclusive rather than failed.
