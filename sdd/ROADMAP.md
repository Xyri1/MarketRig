# MarketRig Roadmap

This roadmap orders work by the smallest end-to-end evidence needed to validate MarketRig. It deliberately avoids completing subsystems horizontally before testing the persistent trading loop. The vertical-slice delivery strategy is set per D12; the milestone ladder runs R0 through R7.

Every milestone names the evidence that closes it, never a date. A milestone is done when its evidence has been produced by the checks that milestone authored — not when its feature list is exhausted. Each milestone's design lands in its own `features/<slug>/` folder before its implementation starts.

Milestones R0, R1, R2, and R3 are delivered; Milestone R4's design is next.

## Milestone R0 — Workspace, daemon, and desk identity

This milestone realizes the choices recorded per D7, D15, D20, D23, D42, D43, D44, D45, D46, D48, D49, D50, D51, D53, D54, D60, D61, and D73, and settles the daemon's boot contract as D77.

**Delivered 2026-09-01** — designed in [`features/r0-workspace-desk-identity/`](features/r0-workspace-desk-identity/PRD.md) (PRD, DECISIONS, SPEC) and implemented in [`slices/001-r0-foundation.md`](slices/001-r0-foundation.md). Its twelve module checks, the gate's G1–G11, and the static checks are green on macOS and Windows CI.

**Goal:** Stand up the smallest authoritative harness a desk can live in.

Expected outcomes:

- the workspace and its release boundary — the daemon and the CLI ship together, with no versioning seam between them (per D43, D53, D54);
- the durable store and its invariants: sole-writer discipline, explicit transactions, strict schemas, opaque identifiers, nanosecond instants, and decimal-text money (per D45, D46);
- desk identity and bootstrap through `CREATING → READY | FAILED`, isolated workspaces, and the ownership boundary over agent-owned files;
- the authenticated loopback API with its per-start credential, and the endpoint discovery file a client or shell uses to find or start the daemon (per D44, D48);
- the `marketrig` CLI skeleton: deterministic commands, machine-readable output, finite timeouts, no generated SDK (per D50);
- bounded local diagnostics, secret-free (per D51);
- crash recovery and reaping of a previous daemon's recorded children (per D73);
- the acceptance harness itself, born small: a scratch data root, the deterministic gate, and the evidence bundle (per D75).

Evidence of completion:

```text
create desk A and desk B with isolated workspaces
-> stop marketrigd
-> start marketrigd
-> both desks, their identities, and their provenance are intact and still isolated
```

**Produced** by the gate's G2 and G4 on both platforms: two desks created with isolated workspaces and their seeded files, then a clean shutdown through `POST /quit` and a restart that leaves both identities, creation instants, workspaces, and prior events intact, with the new start's recovery event naming the previous daemon.

**Why here:** nothing else can be desk-scoped until desk identity and a sole-writer durable store exist, and the harness that will judge every later milestone is cheapest to build while there is almost nothing to judge.

Dependencies: none.

## Milestone R1 — Equity paper trading and the agent's market surface

This milestone realizes the choices recorded per D4, D5, D9, D38, D39, D63, D64, and D76, and settles the equity market plane as D78.

**Delivered 2026-09-02** — designed in [`features/r1-equity-paper-trading/`](features/r1-equity-paper-trading/PRD.md) (PRD, DECISIONS, SPEC) and implemented in [`slices/002-r1-equity-paper-trading.md`](slices/002-r1-equity-paper-trading.md). Its sixteen module checks, the gate's G12–G20, and the static checks are green on macOS and Windows CI, and E1 and E2 ran attended on all four platform-and-runtime cells.

**Goal:** Produce the first authoritative trading fact, and give a real agent both halves of the market surface.

Expected outcomes:

- one keyless equity data client covering the US, Hong Kong, and China A-share markets, feeding the desk's own sandbox paper book — one book per desk on a multi-currency account — with the per-exchange market-hours, staleness, synthesized-book, instrument-metadata, fee, and retry semantics specified before implementation (per D76);
- the sandbox as the only source of fills, positions, fees, and P&L; MarketRig stores its payloads and recalculates nothing (per D38);
- durable trading history — orders, individual fills, position cycles, fees, realized P&L — and its restoration across a daemon restart;
- the realized-P&L signal: a closed position cycle persists one fact and durably queues one evaluation prompt (delivery arrives with the runtimes in R3);
- the MCP adapter: concretely enumerated awareness resources — quotes, book, positions, open orders, instruments — plus typed submit and cancel order tools, validated server-side, with no subscriptions and no completion (per D4, D63);
- the CLI's trading read surface over durable records: `history` for orders, individual fills, and closed position cycles exactly as the sandbox produced them; live positions, open orders, and instrument discovery belong to the MCP plane (per D4);
- paper-order approval fixed at **Always allow**; the policy and its surfaces arrive in R5.

Evidence of completion:

```text
a desk buys and sells one equity on a real market quote
-> the sandbox reports the fill, the close, and net-of-fees realized P&L
-> MarketRig stores every fact unmodified and one evaluation is queued
-> a second round trip in a non-USD market closes with realized P&L in its own currency
-> marketrigd restarts and the book and the history are still there
-> a real Codex session and a real Claude Code session each read the desk's quote
   resource twice, see a fresh value, and submit and cancel a paper order through
   the typed tools
```

**Produced** by the gate's G13, G15, and G17 on both platforms — a USD round trip whose closing fill commits the cycle and its queued evaluation in one transaction, a Hong Kong round trip closing in HKD with commissions at the HK rate, and a restart after which a resting order stands under its original client order identifier — and by the attended E1 and E2 on 2026-09-02 across the four cells: a real Codex CLI session and a real Claude Code session each read the desk's quote resource twice, then submitted and cancelled a resting paper order through the typed tools, the harness verifying by the daemon's own rows (macOS bundles `experiment-codex-1788316954` and `experiment-claude-1788317581`; Windows bundles `experiment-codex-1788357048` and `experiment-claude-1788356517`).

**Why here:** stocks lead the trading ladder (per D76), the trading plane is the product's reason to exist and the one thing the spikes proved outright, and running both real runtimes against the MCP surface this early settles the surface-split risk (per D4) before four other milestones depend on it.

Dependencies: Milestone R0.

## Milestone R2 — Scheduled triggers

**Delivered 2026-09-02** — designed in [`features/r2-scheduled-triggers/`](features/r2-scheduled-triggers/PRD.md) (PRD, DECISIONS, SPEC) and implemented in [`slices/003-r2-scheduled-triggers.md`](slices/003-r2-scheduled-triggers.md). Its eighteen module checks, the gate's G21–G26, and the static checks are green on macOS and Windows CI (run 33607212054); E3 attended on all four cells on 2026-09-02.

This milestone realizes the choices recorded per D34, D35, D37, D40, and D41, and settles their mechanics as D79.

**Goal:** Let a desk define work now that happens later, with or without an agent alive.

Expected outcomes:

- the daemon-owned scheduler: recurrence rules over named time zones, projected next occurrences, and a periodic recheck, with no scheduler framework (per D40);
- occurrences as candidates and firings as atomic acceptance; a one-off consumed by its first accepted firing regardless of later failure (per D34);
- missed schedules recorded as miss evidence, never replayed as catch-up firings (per D37);
- trigger code as an approved immutable snapshot, executed without a command shell under native process-group containment (per D35, D41);
- firing-time brief and context captured as immutable provenance;
- result persistence before delivery, and durable at-most-once handoff;
- trigger-code approval fixed at **Always allow**; the policy and its surfaces arrive in R5.

Evidence of completion:

```text
a scheduled trigger with approved code fires while no agent process is alive
-> the code places an attributable, idempotent paper action
-> the raw result is persisted before anything is delivered
-> a schedule missed across a daemon downtime becomes miss evidence, not a firing
-> a restart mid-flight loses neither the firing nor the result
```

**Produced** by the gate's G22, G25, and G26 on both platforms (macOS bundle `gate-1788336248`) — an `order` trigger firing with no session alive and placing one attributed, idempotent paper action through the real adapter, its result prompt queued before anything is delivered; a one-off and a minutely rule missed across a `POST /quit` becoming one `TRIGGER_MISSED` each and never a firing; and a hard kill mid-execution after which recovery lists the execution under `executions_lost`, completes it `DAEMON_LOST`, and leaves the firing and its prompt intact — and by the attended E3 on all four cells on 2026-09-02: a real Codex CLI session and a real Claude Code session each wrote a one-line script and defined a one-off trigger through the CLI, and the daemon fired it with no session alive, ran the code, and recorded one market buy attributed to the firing (macOS bundles `experiment-e3-codex-1788361144` and `experiment-e3-claude-1788360833`; Windows bundles `experiment-e3-codex-1788357245` and `experiment-e3-claude-1788356751`).

**Why here:** later work is the half of the loop R1 cannot produce, and it needs something worth scheduling but not a live session — a trigger fires whether or not a desk has one.

Dependencies: Milestone R1.

## Milestone R3 — Runtime adapters and delivery

**Delivered 2026-09-04** — designed in [`features/r3-runtime-delivery/`](features/r3-runtime-delivery/PRD.md) (PRD, DECISIONS R3-1 … R3-9, SPEC) and implemented in [`slices/004-r3-runtime-delivery.md`](slices/004-r3-runtime-delivery.md). Its eight module checks, the gate's G27–G32, and the static checks are green on macOS and Windows CI (run 33747939130, commit 3a05223); E4 attended on all four cells on 2026-09-03 and 2026-09-04.

This milestone realizes the choices recorded per D3, D24, D25, D27, D28, D31, D32, D36, and D69, and settles their mechanics as D80. Runtime facts were verified against Codex CLI 0.152.1 and Claude Code 2.1.258, which are the version floors.

**Goal:** Make the harness reach a real agent, and let it exit and come back.

Expected outcomes:

- runtime discovery and compatibility validation of the exact selected Codex and Claude Code launch targets;
- daemon-owned native terminals on both platforms, with one control plane per runtime (per D24, D25, D31);
- structured runtime events — not terminal output — as the authority for liveness, attention, exit, and failure (per D27, D32);
- resume-first activation through explicit pointers, never ambient continuation, never a prompt on the command line (per D28);
- durable FIFO prompt queueing over each runtime's own supported input path, with `STEER` retained and disabled (per D36);
- session controls: interrupt where the runtime supports one, exit, start new, and runtime switch without changing desk identity (per D69);
- delivery failure and handoff-unknown recorded as history, with no automatic retry.

Evidence of completion:

```text
a trigger fires for a desk with no live session
-> MarketRig resumes the remembered native session, or starts a fresh one
-> the result is handed off exactly once as ordinary input, with no keystroke
   emulation and no interruption of an active turn
-> a queued realized-P&L evaluation waits behind active work and then arrives
-> the desk switches runtime and keeps its identity, history, and book
```

**Produced** by the gate's G28–G32 on both platforms (macOS bundle `gate-1788433079`) — a code-less one-off firing with no session alive, the daemon discovering and launching the stand-in runtime in a daemon-owned terminal, orientation heading the FIFO and the `TRIGGER_RESULT` arriving as `INPUT 2` through `turn/start`; two prompts queued behind a 5-second active turn delivered in order after the next `idle`, then a resume through the same thread pointer; the Claude half over the channel bridge with hooks recorded and Interrupt refused; an activation failure and a dropped app-server socket recorded as `ACTIVATION_FAILED` and `HANDOFF_UNKNOWN`, disclosed once to the next new session; and a hard kill mid-attempt recovered as `sessions_lost` and `prompts_unknown` with the pointer intact — and by the attended E4 on all four cells: with no operator registration, a real Codex CLI and a real Claude Code session were each launched by MarketRig into the operator's console, received a scheduled firing's result as their own input, took a second prompt queued behind a busy turn, and switched runtime with the desk's history intact (macOS bundles `experiment-e4-codex-1788411421` and `experiment-e4-claude-1788411080`, codex-cli 0.152.1 and Claude Code 2.1.259; Windows bundles `experiment-e4-codex-1788491136` and `experiment-e4-claude-1788491543`, codex-cli 0.153.0 and Claude Code 2.1.260).

**Why here:** it is the largest and least testable surface, and it only becomes provable once there is something worth delivering — a trigger result and a queued evaluation both exist by R2.

Dependencies: Milestone R2.

## Milestone R4 — Memory, skills, and the closed loop

This milestone realizes the choices recorded per D16, D17, D18, D19, D21, D22, D47, and D65.

**Goal:** Close the loop — an outcome becomes a retained lesson that a later session reuses.

Expected outcomes:

- one installation-wide Hindsight instance as a supervised child, launched against the interpreter and environment the installation ships for it and nothing else (per D47, D65);
- one isolated bank per desk, derived from desk identity, with explicit degradation when Hindsight is unavailable (per D18);
- the `marketrig memory` retain / recall / reflect surface;
- the seeded `AGENTS.md` constitution, one canonical cross-runtime skill set per desk, and the seeded improvement skill the agent may then evolve (per D19, D21);
- the agent-owned Evaluate and Learn cycle driven by the queued realized-P&L prompt (per D22).

Evidence of completion:

```text
a realized-P&L evaluation prompt reaches a live session
-> the agent selects its own evidence and judges the outcome
-> it retains one desk-specific lesson and improves one desk skill
-> a later session recalls the lesson and loads the improved skill
-> the other desk's bank is untouched throughout
-> Hindsight stopped: sessions, triggers, and paper trading continue, and the
   failure is explicit
```

**Why here:** it is the last piece of the loop, and retention is only meaningful once a realized outcome can actually reach a live session, which needs R1 and R3 both standing. At the end of this milestone the product thesis is proven without a desktop.

Dependencies: Milestone R3.

## Milestone R5 — Desktop and approval controls

This milestone realizes the choices recorded per D10, D26, D29, D30, D33, D52, D55, D56, D57, D58, D59, D62, D66, D70, D71, and D72.

**Goal:** Give the user a control plane over desks that are already running.

Expected outcomes:

- the shell: window, tray, single instance, daemon bootstrap through endpoint discovery, and no HTTP of its own (per D30, D66);
- the Vue frontend over the daemon's REST surface, with its client generated from the daemon's own OpenAPI document rather than handwritten (per D59);
- the three-panel control plane over the real terminal, with warm presentations surviving tray hide and reopen (per D29, D33);
- the live-event tail with a client-held cursor, and the desk's history, trading state, and triggers in the right panel (per D71);
- the trigger-code and paper-order **Always allow | Require approval** policies, their pending lifecycles, and the desktop and notification surfaces that resolve them (per D10, D52, D70);
- close-hides-to-tray and Quit semantics on both platforms (per D26);
- the packaged desktop smoke.

Evidence of completion:

```text
a desk's real terminal attaches, survives a tray hide and reopen, and keeps working
-> a paper order raised under Require approval waits, is approved from the desktop,
   and only then reaches the sandbox
-> a denied trigger-code approval leaves no action behind
-> the packaged application does all of the above from a freshly wiped per-user root
```

**Why here:** the control plane is cheapest to build once the daemon's surface has stopped moving, and nothing before it needs a window — the loop is already proven headless by R4; approvals ride with it because the approval boundary is worth little until something can present the prompt.

Dependencies: Milestone R4.

## Milestone R6 — Crypto, event triggers, localization, and packaging

This milestone realizes the choices recorded per D11, D13, D34, D49, D68, and D74.

**Goal:** Widen every proven path to the full MVP surface.

Expected outcomes:

- the full Kraken crypto paper environment per D74: one margin account per desk across spot and futures, long and short, the sandbox's single-order types, funding/mark/index observations, and the inherited-limits statement in the seeded constitution;
- desk-scoped EVENT trigger ingress, ingress-scoped occurrence identity, exact event-name matching, and duplicate suppression (per D34);
- localization parity: the desktop, tray, and notifications in English and Simplified Chinese, the onboarding language step and locale setting, and an agent-facing contract that stays byte-identical under both (per D68);
- packaging and distribution for both platforms, including the interpreter and environment the memory child needs, code signing, autostart, and notifications;
- uninstall preserves user data unless erasure is explicitly requested (per D13).

Evidence of completion:

```text
a short position and a futures position each close through one realized-P&L fact
   and one queued evaluation, and a funding instant moves no paper balance
-> one desk-scoped event fires every matching enabled trigger, its duplicate is
   ignored, and a distinct later event refires only the recurring ones
-> the packaged application runs the whole loop in zh-Hans while marketrig, the MCP
   surface, prompts, and seeded files stay byte-identical to the en run
```

**Why here:** each item widens a mechanism the earlier milestones already proved — a second venue on the same trading topology, a second ingress on the same firing pipeline, a second locale over the same strings — so none of it buys new loop evidence and all of it can wait until the loop is closed.

Dependencies: Milestone R5.

## Milestone R7 — MVP acceptance

Acceptance exercises the decisions recorded per D6, D7, D11, D12, D22, D38, D63, D67, and D75.

**Goal:** Validate MarketRig as a persistent event-driven paper-trading harness, on the real systems.

The acceptance flow is:

```text
create multiple isolated desks
-> launch and attach real native agent terminals
-> resolve and reread stable market resources backed by authoritative state
-> observe authoritative book state
-> agent defines later scheduled/event-driven work
-> trigger fires whether or not a managed agent process is live
-> approved code performs idempotent paper actions that realize P&L
-> the sandbox produces the authoritative execution/accounting outcome
-> MarketRig preserves complete trading history, raw result, approval, and provenance
-> any realized P&L queues evaluation exactly once without interrupting active work
-> agent selects evidence, retains a Hindsight lesson, and improves a desk skill
-> a later session uses durable history, files, memory, and skills
-> daemon restart does not erase identity or truth
```

Expected outcomes:

- the attended experiment run once per platform/runtime cell — Windows/Codex, Windows/Claude Code, macOS/Codex, macOS/Claude Code — on the real CLIs and real Hindsight, with agent-owned scenarios ending inconclusive rather than failed (per D75);
- the scenario-to-check mapping, so every scenario of the acceptance flow is answered by a named check (per D75);
- an evidence bundle per cell.

The MVP is accepted even if the strategy loses money or the agent learns nothing useful. Profitability and beneficial self-improvement are not gates.

**Why here:** the deterministic gate grows scenario by scenario from R0 onward, so nothing waits for a final harness; what genuinely cannot run early is the attended cross-platform experiment on real runtimes, and that is all this milestone is. Every milestone before it keeps Windows in CI (per D11), so the attended Windows cells are the only platform work this milestone carries.

Dependencies: Milestones R0–R6.

# Known debts and open evidence

These are known and unpaid. Each names the milestone that clears it, or the ceiling that stands.

- **Scale is unmeasured beyond one trading desk per daemon.** R1's gate trades on one desk; isolation across concurrent books, larger fan-out, and per-desk footprint stay open and are deferred rather than designed for.
- **The deterministic gate is hermetic for equities only** — R1's stand-in feed keeps it off the public market; the crypto milestone owes a stand-in venue speaking the Kraken adapter's protocol before its scenarios join the gate.
- **Claude Code exposes no structured interrupt**, so interruption on that runtime is the user's keyboard and the harness records only end-of-turn evidence.
- **The memory child's credentials reach it through its environment** — a stated, deliberate exception to the credential boundary (per D49) that must be restated wherever that child is specified, not quietly dropped.
- **The memory child's embedded database ships with default loopback credentials** — an unclosed ceiling with a written upgrade path.
- **Paper physics gaps stand as stated, not approximated** (per D74, D76): no funding, margin interest, rollover, liquidation, latency, T+1 settlement, price limits, halts, or auctions; and the equity book carries no real bid or ask in any market.

# Deferred / post-MVP

Portability is excluded per D13. Mechanics intentionally left unresolved are listed in [`SPEC.md`](SPEC.md) §18 rather than invented here.

- live trading;
- a MarketRig risk-policy engine;
- Linux support;
- public webhooks and remote event ingress;
- connector retry/deduplication semantics;
- MarketRig cloud accounts or remote control plane;
- automatic desk sync;
- manual desk export/import;
- application updater;
- arbitrary cross-version state compatibility and downgrade support;
- additional agent runtimes;
- multi-agent collaboration within one desk;
- cross-desk capital, positions, or trigger fan-out;
- OpenBB research integration (per D9), deferred on scope: MVP evidence does not need research data;
- direct NautilusTrader or OpenBB APIs as product contracts;
- automatic trigger execution or delivery retry;
- pre-trusting seeded desk workspaces in the runtimes' own configuration at provision time, so a desk whose first-ever session is a dispatcher activation does not stall on a trust dialog and lose that firing's prompt;
- venues beyond the supported equity markets and Kraken, and asset classes beyond equities and crypto (FX, options, …);
- a keyed real-book equity feed (broker OpenAPI or Alpaca-class source) behind the same data-client seam, if learning evidence needs honest spreads (per D76's ponytail note);
- per-market exchange holiday calendars, a maintained instrument-catalog source or per-band tick logic, an installation-wide fetch layer behind the per-node feed clients, and a side-aware equity fee model (per D78's ponytail notes);
- order lists (brackets, OCO) and execution algorithms;
- paper simulation of funding payments, margin interest, or liquidation (per D74);
- normalized universal agent transcripts;
- automatic handoff or daemon-authored memory, reflection, or skill-governance workflows;
- a canonical decision-attribution model beyond realized P&L as the reward signal;
- profitability/self-improvement guarantees;
- MCP beyond the desk's awareness resources and its typed order tools (per D4), and MCP resource subscriptions or completion — neither runtime issues either;
- mandatory telemetry;
- enabling same-turn `STEER` delivery after runtime contracts support it safely;
- locales beyond English and Simplified Chinese, a localized agent-facing CLI/API/prompt/seed contract, and per-desk language.

# Explicit "not doing" rule

When implementation reveals an attractive adjacent capability, ask whether it is necessary for the active milestone's evidence. If not, defer it rather than expanding the milestone.
