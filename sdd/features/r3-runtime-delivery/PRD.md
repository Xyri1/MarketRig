# R3 — Runtime adapters and delivery: Feature PRD

**Milestone:** [R3](../../ROADMAP.md#milestone-r3--runtime-adapters-and-delivery)
**Status:** Design complete — PRD, DECISIONS, and SPEC accepted 2026-09-02

This feature designs Milestone R3: the half of the loop that reaches a real agent. It refines `sdd/SPEC.md` §4.4, §6, §7, §11, §13.1, §15, and §17 and invents nothing beyond them.

## 1. Motivation

*Decision basis: per D3, D24, D25, D27, D28, D31, D32, D36, D69.*

R2 left every trigger result as a `QUEUED` prompt row that nothing reads. The product's claim is a trader identity that keeps working while no conversation exists, and that claim is only true once the daemon can wake the agent, hand the result over, and let the agent exit and come back. R3 is where MarketRig first launches Codex CLI and Claude Code itself, hosts them in daemon-owned terminals, observes their own structured events, and delivers daemon prompts through each runtime's supported input path. It is the largest and least testable surface in the MVP, and it is provable now only because R2 produces something worth delivering.

## 2. Outcome

A trigger fires for a desk with no live session. `marketrigd` resolves the desk's selected runtime to an exact, validated executable, opens a daemon-owned terminal, resumes the remembered native session or starts a fresh one, waits for the runtime's own readiness signal, and hands the queued `TRIGGER_RESULT` prompt over exactly once as ordinary input — through the Codex app-server on one runtime, through a MarketRig-owned Claude Channel on the other — never by keystrokes, never interrupting a turn. A second prompt queued during that turn waits and then arrives. The prompt rows read `DELIVERED`, every attempt has its operational record, and a failed or uncertain handoff reads `FAILED` and is disclosed once at the next activation, never redelivered. The user can interrupt (Codex), exit, start new, and switch runtime from REST while the desk keeps its identity, history, and book. A daemon that dies mid-flight leaves no orphaned runtime, keeps every pointer, and marks in-flight deliveries completion-unknown.

## 3. Scope

R3 delivers eight things, each a thin vertical of a contract `sdd/SPEC.md` already states:

1. **Runtime discovery and validation** (per D3, D25): the exact `codex` and `claude` executables MarketRig will launch, resolved from the user's real environment or set explicitly, version-gated against the lines verified in this design, and stored as installation settings with their validation outcome.
2. **The terminal manager** (per D31): one private module over Unix PTY and ConPTY, with one attachment generation per desk, a bounded reconnect ring, coalesced resize, and drained shutdown, exposed through one authenticated WebSocket route the desktop (R5) and the acceptance harness attach to.
3. **The Codex adapter** (per D24, D28, D32, D69): one lazily started app-server per installation under the R2 containment primitive, one native remote TUI per desk session in its own terminal, thread identity from `thread/started`, status from `thread/status/changed`, delivery through `turn/start` behind an idle gate, interrupt through `turn/interrupt`.
4. **The Claude Code adapter** (per D24, D28, D32, D36, D69): one interactive `claude` per desk session with a MarketRig-generated session UUID, three read-only hooks through a per-launch settings file, and one-way delivery through the bundled Channel bridge over an authenticated loopback WebSocket the daemon serves.
5. **Activation and the dispatcher** (per D28, D36): resume-first with explicit pointers, orientation and disclosure as ordinary prompts, FIFO delivery per desk, `QUEUED → DELIVERED | FAILED` with no retry, and activation failure recorded as delivery failure.
6. **Session controls** (per D69): Interrupt, Exit, Continue, Start new, and Switch as REST routes, each answering what the runtime answered, with one exit-reason vocabulary.
7. **MCP registration takeover** (per D63, §13.1): the runtime adapters register `marketrig-mcp` for every managed session, retiring the operator-performed registration R1 and R2 used.
8. **Durable evidence** (per D27, D36, D71): pointers, processes, delivery outcomes, attention, interrupts, exits, and runtime availability as rows and operational events, with a recovery step that resolves what a crashed daemon left behind.

The acceptance chain grows accordingly: gate scenarios against harness-owned stand-in runtimes that speak the exact protocol subset the adapters consume, and one attended scenario per cell where a real trigger firing wakes a real session and the result lands.

## 4. Non-goals

- No desktop, tray, or notification surface; the terminal socket exists for R5 and the harness, and R3 renders nothing.
- No `STEER`: the setting stays refused, and no steering path is built (per D70).
- No evaluation prompts: their producer arrives with R4; R3 delivers any `EVALUATION` row that exists but creates none.
- No approval workflows: both policies stay **Always allow** (per D70), and MarketRig answers no runtime-native approval.
- No agent-status state machine, no transcript reading, no terminal parsing for lifecycle, no normalization of tool calls or cognitive status (per D27, D32).
- No Claude interrupt path: Claude Code exposes no structured interrupt, so the route answers `INTERRUPT_UNSUPPORTED` (per D69).
- No pre-trusting of desk workspaces in the runtimes' own configuration; a first-ever session that stalls on a trust or channel-confirmation dialog times out as delivery failure and stays deferred (root §18).
- No prompt-queue backpressure and no batching across prompts.
- No runtime beyond Codex CLI and Claude Code, and no universal runtime protocol (per D3).

## 5. Success criteria

1. On both platforms, with no session alive, an accepted trigger firing leads — with no operator action — to a managed runtime process in a daemon-owned terminal, a `SESSION_READY` event, and the firing's `TRIGGER_RESULT` prompt in state `DELIVERED` with its attempt record naming the runtime and native session.
2. A prompt queued while a turn is active is delivered only after that runtime's own next-turn boundary, and prompts for one desk are delivered in acceptance order.
3. A remembered native session is resumed through its explicit pointer; a missing or unresumable pointer starts a new session automatically on the dispatcher path and never on explicit Continue.
4. Every ended managed process leaves one `SESSION_EXITED` event with an exit reason from the fixed vocabulary; a hard-killed daemon leaves no live runtime, every pointer intact, and every in-flight delivery `FAILED` with `HANDOFF_UNKNOWN`.
5. Switch runtime keeps the desk UUID, name, workspace, history, and paper book, keeps both pointers, and refuses before stopping anything when it must refuse.
6. Every managed session reaches the daemon through `marketrig-mcp` without operator registration.
7. The gate covers all of the above unattended on stand-in runtimes on both platforms; the attended scenario reproduces criteria 1–3 and 5 once per platform-and-runtime cell on the real CLIs.
