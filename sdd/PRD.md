# MarketRig Product Requirements Document

**Product:** MarketRig  
**Positioning:** *Vibe trading terminal for agents.*  
**Stage:** MVP / minimum viable experiment  
**Agent runtimes:** Codex and Claude Code  
**Platforms:** Windows and Apple Silicon macOS  
**Trading mode:** paper only, on a bundled NautilusTrader sandbox — equities first (US, Hong Kong, China A-share), Kraken crypto after  
**Languages:** English and Simplified Chinese (desktop); English agent-facing contract

## 1. Product summary

This product boundary is established per D1, D2, D3, D4, D9, D11, D22, D38, D63, and D76.

MarketRig is a local persistent trading harness for general-purpose coding agents.

It does not embed a proprietary model or implement a daemon-owned trading reasoner. It gives Codex or Claude Code durable trader identities, real terminal sessions, market access, paper execution, scheduled and event-driven work, desk-scoped memory and skills, and continuity across disposable conversations.

Its differentiator is a self-improving desk: the agent evaluates realized outcomes, retains experiential lessons in Hindsight, and improves its own reusable skills. MarketRig owns the durable environment, time, approvals, trading history, and access to authoritative trading reality; the agent owns interpretation, decisions, and learning.

## 2. Problem

Coding agents already know how to reason, use a shell, research, write software, and operate tools. Rebuilding those capabilities inside a bespoke trading agent would duplicate them and couple the product to one model.

What they lack is a durable trading environment:

- separate persistent trader identities;
- reliable access to market data;
- a way to reread volatile market values instead of over-trusting stale prices already present in model context;
- isolated paper books and authoritative outcomes;
- code that can react later to schedules or events;
- runtime sessions that can be interrupted, exit, resume explicitly, or be replaced;
- files and state that survive lost conversations;
- complete trading history and outcome signals that support learning across sessions;
- desk-specific experiential memory and reusable procedures that can improve;
- a user control plane for multiple long-running desks.

## 3. Product thesis

> A general-purpose coding agent can operate and improve as a persistent, event-driven paper trader when given a durable market harness that survives individual agent sessions.

MVP does not need to prove profitability, good strategy, or beneficial self-improvement. It must prove the complete mechanical loop from realized outcome through evaluation, Hindsight retention, skill improvement, and later reuse.

## 4. Initial user

The initial user is a technically capable agentic developer/trader who already uses Codex or Claude Code and wants to experiment with autonomous market behavior without building an agent runtime or trading substrate.

MarketRig is developer-oriented, local-first, and single-user in MVP.

## 5. Core product model

### 5.1 Desk

The desk model is defined per D7, D15, D20, D21, and D22.

A **desk** is one durable autonomous trader identity.

Each desk has its own:

- workspace and always-loaded `AGENTS.md` constitution;
- its own Hindsight bank and canonical cross-runtime skills;
- selected runtime and native session pointers;
- scheduled/event triggers;
- isolated paper book;
- approvals, provenance, operational history, and durable trading history.

Multiple desks exist and operate concurrently from day one. Public read data and installation configuration may be shared, but mutable trader state is isolated.

### 5.2 Agent session

Session identity and lifecycle are defined per D8, D27, and D28.

A **session** is the native logical Codex or Claude Code session currently serving a desk. It is not the desk identity and is not a MarketRig Run.

MarketRig knows whether it owns a live process and terminal, retains the last native session pointer per runtime, and records attention, exit, and failure events. It does not invent a durable agent-status state machine. A desk may continue its last native session, start a new one, or switch runtime without changing identity.

MarketRig intentionally has no Run domain entity.

### 5.3 Trigger

The trigger model is defined per D34, D35, D36, and D37.

A **trigger** is a durable desk-bound daemon job. It fires from a schedule or an event, captures the firing-time brief/context and any approved code identity as immutable provenance, may execute approved code, and returns the raw result to the desk agent.

For EVENT triggers, one-off means the first distinct matching event durably creates a firing and consumes the trigger regardless of later execution or delivery outcome. Recurring means every distinct matching event creates one firing; duplicate delivery of the same event identity does not create another.

This lets an agent decide ahead of time: write a rule, wait for time or an event, act through MarketRig if appropriate, and review the result later.

### 5.4 Agent surface

The surface split is defined per D4 and D63.

The agent reaches MarketRig through two surfaces, divided by what the agent is doing:

- **In the market — MCP.** Awareness is re-readable **resources**: current quotes, book, live positions, open orders, and the desk's tradable instruments. Money actions are a small set of typed **tools**: submit and cancel a paper order.
- **In the harness — the `marketrig` CLI.** Durable records, structure, and cognition: trading history, desks, triggers and their code, memory, and prompts.

No capability appears on both surfaces. The daemon is the evidence authority for every action either surface performs; a transcript is never the record.

### 5.5 Durable continuity

Continuity and agent-owned learning are defined per D16, D17, D18, D19, D22, and D38.

Desk files plus authoritative MarketRig and sandbox state are the durable contract. Native conversation resumption is useful but not required. MarketRig's durable store preserves the complete trading history the agent needs: orders, individual fills, position cycles, fees, and realized P&L, exactly as the sandbox produced them.

MarketRig accepts that conversation may be lost. It does not require handoff documents or maintain a normalized transcript archive.

Each desk owns its `AGENTS.md`, Hindsight bank, and canonical skills shared between Codex and Claude Code only within that desk. MarketRig seeds the improvement skill, after which the agent may evolve it. MarketRig does not decide what evidence matters or whether a result warrants a memory or skill change.

## 6. Responsibility model

The responsibility boundary is defined per D5, D6, D10, D22, and D38.

MarketRig uses a modified OODA loop with durable continuity underneath it:

- **Observe — MarketRig:** authoritative market, desk, and paper-book primitives, including stable references to mutable current quotes;
- **Orient — agent:** research, code, Hindsight, skills, workspace, and shell tools;
- **Decide — agent:** whether and how to act;
- **Act — MarketRig:** idempotent sandbox/paper execution;
- **Evaluate — agent:** select relevant history and judge the outcome using realized P&L per closed position cycle as the reward signal;
- **Learn — agent:** retain desk-specific lessons in Hindsight and improve reusable procedures in skills;
- **Repeat:** return to Observe with durable desks, sessions, triggers, files, and history carrying continuity.

MarketRig exposes facts and actions, not strategy conclusions such as `find_alpha`, `should_buy`, or `choose_strategy`.

## 7. MVP experience

A representative journey is:

1. User installs MarketRig and completes first-launch discovery of Codex and Claude Code.
2. User creates multiple desks and chooses a runtime for each.
3. User selects a desk in the left panel and works with the real native agent terminal in the center panel.
4. Agent reads the desk's market resources for current quotes, book, and positions, rereading them whenever exact current values matter, and reviews durable records through `marketrig`. Research is the agent's own, through its shell and tools.
5. Agent researches, decides, and may place a paper order through the typed order tool.
6. Agent creates a scheduled or event trigger, optionally containing code.
7. User approves trigger code or paper orders when the installation settings require it.
8. The desktop may close to tray while desks, sessions, and triggers continue.
9. A trigger fires whether or not the desk has a live managed agent process, runs its code, persists its raw result, and queues an ordinary prompt through the runtime's structured interface.
10. Any realized-P&L event is persisted with its trading evidence and queues an evaluation prompt without interrupting an active turn.
11. The agent selects relevant history, evaluates the outcome, retains any desk-specific lesson in Hindsight, and improves a reusable desk skill when useful.
12. A later session can recall that lesson and load the improved skill.
13. A daemon restart preserves the desk, triggers, session pointers, approvals/provenance, paper state, and trading history.
14. User may continue the last native session, start a new session, or switch runtime without creating a new desk.

## 8. Desktop behavior

Desktop lifecycle, presentation, onboarding, and localization are defined per D18, D26, D29, D30, D33, D49, D52, and D68.

The desktop resembles a three-panel conversational application without replacing the runtime terminal:

- **left:** desk navigation;
- **center:** real attachable Codex/Claude terminal;
- **right:** selected-desk market data, trading state, triggers, approvals, and history.

Closing the window hides the existing warm UI to the tray; terminal presentations and operations continue. Explicit **Quit MarketRig** stops the daemon and managed processes.

The desktop, tray, and notifications are available in English and Simplified Chinese. First-launch onboarding opens in the language detected from the system, lets the user confirm or change it, and the choice is an installation setting changeable later. Everything the agent consumes — the `marketrig` CLI, the MCP surface, JSON, daemon prompts, seeded `AGENTS.md`, and skills — is English under either choice.

First-launch onboarding offers OS-login autostart, enabled by default and configurable. It also configures Hindsight's hosted models through one installation-wide OpenAI-compatible base URL, API key, LLM model selector, and embedding model, where the model list is fetched live whenever opened and is never persisted or cached.

## 9. MVP scope

MVP scope is defined per D11, D12, D13, D14, D16, D17, D18, D39, D68, D74, and D76.

MVP includes:

- Windows and Apple Silicon macOS with feature parity;
- Codex and Claude Code runtime support on both platforms;
- capability-based runtime discovery and compatibility validation of the exact selected Codex CLI and Claude Code launch targets;
- multiple durable concurrent desks;
- daemon-owned native terminal sessions;
- session interrupt, explicit resume, new-session, runtime-switch, and exit behavior;
- resume-first activation with ordinary structured input for fresh context;
- three-panel desktop control plane and tray lifecycle;
- `marketrig` as the canonical agent-facing CLI for durable records, structure, and cognition;
- stable human-readable and machine-readable CLI output;
- one bundled MCP adapter shared by Codex and Claude Code, exposing the desk's re-readable market-awareness resources and its typed paper-order tools, backed by the daemon;
- scheduled and desk-scoped event triggers;
- one-off and recurring triggers;
- optional agent-authored trigger code;
- MarketRig-owned trigger-code and paper-order approval settings;
- durable at-most-once trigger-result delivery;
- a retained `STEER | QUEUE` daemon-prompt setting with only `QUEUE` enabled in MVP, using each runtime's own supported structured input path;
- a bundled NautilusTrader sandbox as the required paper execution and accounting authority;
- one isolated paper book per desk on a multi-currency account, carrying US, Hong Kong, and China A-share equities from one keyless market feed first — realized P&L in each instrument's own currency — and Kraken spot and futures — long and short, on a margin account, with the sandbox's single-order types — when crypto lands (per D74, D76);
- complete immutable sandbox-produced trading history in MarketRig's durable store;
- shared public reads with provider/as-of provenance;
- one installation-wide local Hindsight instance with embedded persistence, one isolated bank per desk, and agent-driven retain, recall, and reflect through `marketrig`;
- hosted Hindsight LLM and embedding models behind one installation-level OpenAI-compatible base URL, API key, and selected models, with no reranking model;
- one canonical cross-runtime skill set and one seeded improvement skill per desk;
- structured session, trigger, approval, action, and failure history;
- an English and Simplified Chinese desktop, tray, and notifications selected per installation, with an English-only agent-facing CLI, MCP, JSON, prompt, and seeded-file contract;
- per-user local installation with no hosted MarketRig control plane.

## 10. Explicit non-goals for MVP

MVP does not include:

- live trading;
- a MarketRig risk-policy engine for spot, leverage, shorting, or bankroll rules;
- asset classes beyond equities and crypto, or venues beyond the supported equity markets (US, Hong Kong, China A-share) and Kraken;
- a real equity order book: the keyless equity feed carries no bid or ask in any market, so the desk's equity book is synthesized and depth and spread are not modeled;
- paper simulation of funding payments, margin interest, liquidation, latency, T+1 settlement, price limits, halts, auctions, or anything else the sandbox does not model (per D74, D76);
- strategy/alpha recommendations;
- a proprietary model or daemon-owned reasoning loop;
- a universal runtime protocol;
- a custom chat UI or normalized conversation transcript;
- mandatory handoff documents;
- multi-agent collaboration within one desk;
- cross-desk capital, positions, triggers, or cognitive state;
- direct NautilusTrader or OpenBB APIs as product contracts;
- OpenBB research integration (deferred past MVP on scope per D9);
- public webhook ingress or a general event platform;
- automatic trigger execution/delivery retries;
- enabled same-turn `STEER` delivery;
- a MarketRig cloud account, remote control plane, or mandatory telemetry;
- Linux support;
- automatic desk sync;
- manual desk export/import;
- an application updater, arbitrary cross-version state-compatibility guarantee, or downgrade support beyond release-packaged forward schema migrations;
- proof of profitability, strategy quality, or beneficial self-improvement;
- MCP as a surface for history, memory, triggers, desks, or research, or the CLI as a surface for market awareness and order actions (per D4);
- market-resource subscriptions or server-pushed updates: awareness is explicit rereads;
- locales beyond English and Simplified Chinese, a localized agent-facing CLI, API, prompt, or seeded file, or a per-desk language.

## 11. Success criteria

MVP is successful when MarketRig reliably demonstrates this loop:

```text
agent observes and researches
-> agent decides and defines later work
-> schedule/event fires
-> approved code may perform an idempotent paper action
-> the sandbox produces the execution/accounting outcome
-> MarketRig persists the outcome and queues evaluation on realized P&L
-> agent evaluates, retains lessons, and improves skills when useful
-> a later session continues from durable history, memory, skills, and files
```

Acceptance requires:

1. At least two concurrent desks remain isolated across workspaces, agent sessions, triggers, and paper books.
2. The real Codex/Claude terminal continues under the daemon while its warm desktop presentation survives tray hide/reopen.
3. MarketRig can interrupt or exit a managed session, resume the selected runtime's exact remembered native session, or start fresh without manufacturing an agent-status state machine.
4. Scheduled and event triggers can persist and fire independently of agent-session activity; EVENT one-offs consume their first distinct match, recurring EVENT triggers fire once per distinct match, and duplicate event identities do not refire.
5. Approved trigger code can use MarketRig public reads and paper actions.
6. Results are persisted before at-most-once structured delivery and can activate a desk with no live managed session without keystroke emulation or delivery-triggered interruption.
7. The sandbox remains authoritative for paper execution and accounting; MarketRig durably preserves its resulting orders, fills, position cycles, fees, and realized P&L without recalculating them.
8. Hindsight unavailability is explicit but does not block sessions, triggers, or paper trading.
9. Core state survives daemon restart without relying on conversation or mandatory handoff.
10. The core smoke flow passes with real Codex and Claude Code on Windows and Apple Silicon macOS.
11. The acceptance exercise demonstrates isolated desk banks and a realized-P&L event durably queueing evaluation, after which the agent retains a Hindsight lesson through `marketrig` and improves a desk skill that a later session uses.
12. Real Codex and Claude Code sessions can resolve and reread the same stable MarketRig market resource; reads expose new observations after market updates and preserve the same sequence when no update occurred; and both runtimes can submit and cancel a paper order through the typed order tools, with malformed arguments refused by MarketRig rather than by the client.
13. The desktop, tray, and notifications work in Simplified Chinese and English, while `marketrig`, the MCP surface, daemon prompts, and seeded desk files are byte-identical under both.
14. An equity round trip on a real market quote closes through one realized-P&L fact, net of the fees the sandbox charged, and queues one evaluation, in at least one US-market instrument and one non-USD-market instrument whose P&L lands in its own currency (per D76).
15. A short position and a futures position each close through one realized-P&L fact and one queued evaluation, and no paper balance changes at a funding instant (per D74).

Realized P&L is the evaluation reward signal; profitability and beneficial learning are not acceptance criteria.

## 12. Product principles

- Reuse external agent capabilities instead of rebuilding them.
- Keep desk identity durable and runtime identity replaceable.
- Let files, trading history, memory, and skills carry continuity.
- Preserve native terminal and session behavior.
- Split the agent surface by what the agent is doing: the market through MCP, the harness through the CLI.
- Deliver daemon prompts through supported structured queueing, never through interruption or keystroke emulation; retain `STEER` as a disabled post-MVP option.
- Treat uncertain delivery as failure rather than replaying agent input.
- Share public reads; isolate mutable trader state.
- Keep volatile market truth outside model context and make exact current values explicitly rereadable.
- Keep the sandbox authoritative for execution and accounting, and MarketRig's durable store authoritative for trading history.
- Keep research the agent's own; when OpenBB returns post-MVP, keep it informational.
- Keep credentials behind the daemon boundary.
- Expose primitives and raw results, not conclusions.
- Add product entities only when they clarify real behavior.
- Defer module mechanics until their dedicated design sessions.
