# R0 — Workspace, daemon, and desk identity: Feature PRD

**Milestone:** [R0](../../ROADMAP.md#milestone-r0--workspace-daemon-and-desk-identity)
**Status:** Design complete — PRD, DECISIONS, and SPEC accepted 2026-09-01

This feature designs Milestone R0: the smallest authoritative harness a desk can live in. It refines `sdd/SPEC.md` §3, §4.1, §4.3, §5, §15, and §17 and invents nothing beyond them.

## 1. Motivation

*Decision basis: per D12.*

Nothing else in MarketRig can be desk-scoped until desk identity and a sole-writer durable store exist. Every later milestone — trading history, triggers, delivery, memory — writes desk-owned rows into the store this milestone founds and is judged by the acceptance harness this milestone births. Both are cheapest to build while there is almost nothing to judge, and a wrong foundation fails cheaply now rather than expensively under R1–R4.

## 2. Outcome

A daemon a client can find and authenticate to; two desks with isolated workspaces whose identities and provenance survive a daemon stop and restart; a CLI that proves it with machine-readable output; and the deterministic gate that demonstrates all of it from a scratch data root with an evidence bundle.

## 3. Scope

R0 delivers eight things, each a thin vertical of a contract `sdd/SPEC.md` already states:

1. **Workspace and release boundary** (per D43, D53, D54): the root Cargo workspace with `crates/marketrigd` (library crate plus thin binary) and `crates/marketrig`, one version, one lockfile, rustc ≥ 1.98 / edition 2024. `marketrig-mcp` joins the same workspace in R1 with no versioning seam.
2. **Durable store** (per D45, D46): one installation-wide SQLite database under the application-data root; sole-writer `marketrigd`, one database thread, explicit `BEGIN IMMEDIATE`, WAL, `STRICT` tables with enforced foreign keys, numbered forward-only migrations, newer-database rejection; UUIDv7 text identifiers, `*_ns` instants, decimal-text money conventions in force from the first table.
3. **Desk identity and bootstrap** (per D7, D15, D20): UUIDv7 plus immutable kebab name, `CREATING → READY | FAILED` with idempotent workspace bootstrap, startup completion of interrupted `CREATING` rows, retry of `FAILED` on the same identity, workspace validation for `READY` desks, and the ownership boundary — agent-owned material is never rewritten after first `READY`.
4. **Authenticated loopback API** (per D44, D48): the pinned web framework on `127.0.0.1` with an OS-assigned port, per-start bearer credential and daemon UUID, `runtime/endpoint.json` plus the daemon lifetime lock beside it, and authenticated health. The framework choice binds D59's OpenAPI-emission requirement now, even though the first generated client arrives with R5.
5. **CLI skeleton** (per D50): thin blocking client of the loopback API — deterministic commands, `--json` machine output, finite timeouts, no proxy inheritance, redirects disabled — carrying only the commands R0's evidence needs.
6. **Bounded local diagnostics** (per D49, D51): JSON Lines through the pinned logging facade in the OS application-log directory, secret-free. R0 stores no provider secret; the credential-store binding waits for the first milestone that has one, and only the discipline (no secret in SQLite, logs, or output) binds now.
7. **Crash recovery and reaping** (per D73): the pre-service recovery transaction with its ordered module steps, one `RECOVERY` event on every start, and `runtime/children.json` record-and-reap mechanics — proven against a fake recorded child until real long-lived children exist.
8. **The acceptance harness, born small** (per D75): the `MARKETRIG_TEST_DATA_ROOT` and `MARKETRIG_TEST_NO_TRADING` seams, the deterministic gate as a Cargo test target, the evidence-bundle layout, and the first scenarios — exactly R0's evidence line.

Two adjacent items are deferred rather than settled here:

- the `.agents/skills/` tree, the `.claude/skills` link, and the seeded improvement skill arrive with R4 (per D21); R0's bootstrap creates none of them;
- the seeded `AGENTS.md` constitution's wording is deferred (`sdd/SPEC.md` §18) and grows with the milestones that add the surfaces it names; R0's bootstrap creates the file as §5.2 requires, with the minimal seed this feature's SPEC pins.

## 4. Non-goals

Everything R1–R6 owns, in particular:

- no NautilusTrader dependency, trading node, paper book, or market data;
- no `marketrig-mcp` crate or MCP surface;
- no triggers, scheduler, or event ingress;
- no runtime discovery, adapters, terminals, sessions, or activation;
- no Hindsight child, memory surface, or bundled interpreter;
- no Tauri shell, frontend, desktop, tray, or notifications;
- no approval policies or policy surfaces;
- no localization surfaces (the agent-facing contract is English-only regardless, per D68);
- no packaging, installer, code signing, or autostart;
- no desk deletion, rename, or erasure — uninstall-preserves-data stands per D13, and no R0 evidence needs a destructive desk operation.

Per the roadmap's rule: an attractive adjacent capability that R0's evidence does not need is deferred, not designed.

## 5. Success criteria

1. The roadmap's evidence line passes in the gate: create desk A and desk B with isolated workspaces → stop `marketrigd` → start `marketrigd` → both desks, their identities, and their provenance are intact and still isolated.
2. A client discovers a running daemon through `runtime/endpoint.json`, authenticates with the per-start bearer, and verifies the reported daemon UUID; a stale endpoint file or credential from a previous daemon start never authenticates.
3. A desk creation killed mid-`CREATING` completes on the next startup; a `FAILED` creation retried explicitly reuses its UUID, name, and workspace path; a `READY` desk with a damaged workspace reports workspace-unavailable without blocking other desks or startup.
4. Store invariants are demonstrably in force: `STRICT` tables, WAL, forward-only migrations, and rejection of a newer database.
5. Every daemon start appends one `RECOVERY` event naming the previous and new daemon UUIDs, and recovery reaps a recorded live child of a prior daemon while leaving a recycled pid untouched.
6. Diagnostics are bounded and contain no bearer credential.
7. The whole chain runs unattended under `MARKETRIG_TEST_DATA_ROOT`, touches no per-user root, and leaves an evidence bundle; rustfmt, Clippy `-D warnings`, and `cargo test` are green on both MVP platforms in CI (per D11, D60, D61).

R0 is done when this evidence exists — produced by the checks this feature's SPEC names — not when the deliverable list is exhausted.
