# MarketRig Decision Log

This file records active product and architecture decisions and why they were made. `sdd/SPEC.md` owns the current mechanical contract, `sdd/PRD.md` owns product outcomes and scope, and `sdd/ROADMAP.md` owns delivery order.

Active decisions are grouped by subject; a decision's number is its stable identity, not its position.

## Product definition and MVP scope

### D1 — Product name is MarketRig

**Decision:** Use **MarketRig** with the positioning line **“Vibe trading terminal for agents.”** Product names naturally derive as `marketrig`, `marketrigd`, and the default desk-workspace home `~/.marketrig/`.

**Rationale:** The name fits a technical agent-facing environment without presenting MarketRig as another trading strategy or fintech service.

**Contract:** [PRD summary](PRD.md#1-product-summary).

### D2 — MarketRig is a harness/environment, not the intelligence

**Decision:** MarketRig provides the persistent environment and toolset. External agent runtimes provide cognition.

**Rationale:** Coding agents already provide planning, research, shell use, code generation, and tool use. Rebuilding those capabilities would couple the product to a proprietary reasoning loop.

**Contract:** [PRD summary](PRD.md#1-product-summary) and [SPEC scope](SPEC.md#1-scope).

### D3 — MVP supports Codex and Claude Code only

**Decision:** Implement concrete support for the standalone Codex CLI and Claude Code CLI on both MVP platforms. MarketRig does not target or discover the Codex binary bundled with the ChatGPT/Codex desktop app. Do not define a universal runtime protocol without evidence from additional runtimes.

**Rationale:** Broad runtime support would force premature abstraction and compatibility work.

**Contract:** [PRD MVP scope](PRD.md#9-mvp-scope) and [SPEC scope](SPEC.md#1-scope).

### D4 — MCP owns the trading plane; the CLI owns the continuity plane

**Decision:** The agent surface is split by what the agent is doing, and no feature appears on both halves. **MCP resources** carry market awareness — quotes, book, live positions, open orders, instrument discovery — enumerated as concrete desk-scoped resources in `resources/list`, never as URI templates, and re-read explicitly. **MCP tools** carry the money actions — order submit and cancel, two or three typed tools — whose arguments the daemon validates server-side and answers with a structured tool error, because the advertised schema is documentation, not a trust boundary. The **`marketrig` CLI** carries everything else: `history`, `desk`, `trigger`, `memory`, `prompt`, and `session hook` — durable records, file-payload actions such as trigger code and RRULEs, and cognition. MarketRig builds no subscription and no completion path. The daemon's SQLite remains the evidence authority for every action (D32, D38); the transcript never is. The seeded `AGENTS.md` names the resource URIs and the desk configuration accounts for the runtime's approval mode.

**Rationale:** MCP is what the agent does *in the market* (observe and act); the CLI is what it does *in the harness* (records, structure, cognition). The split was gated on client support and measured on the wire — six sessions per client against Claude Code 2.1.252 and Codex CLI 0.151.0 with an `rmcp =3.2.0` probe server, arbitrated by raw JSON-RPC logs rather than model prose. Both clients list and read resources, and **neither caches a read**, so a re-read returns a genuinely fresh value: the LSP-style semantics volatile prices need. Claude Code never calls `resources/templates/list` (0 calls in 6 sessions), so a template-only surface is invisible to it and the enumeration must be concrete. Neither client enforces the tool input schema — both forwarded a two-field object against four required fields — so validation is the server's job. Neither issues `resources/subscribe`, and neither negotiates the protocol revision carrying `subscriptions/listen`, so server-side push is dead weight; explicit re-reads were already the model (D63). Codex gates `tools/call` behind approval while resource reads are ungated, so on that runtime the read path is strictly cheaper than the tool path, which is the shape of the split. Two costs are accepted: the acceptance harness needs its own MCP client, because submit and cancel exist nowhere else; and the money path depends on a live stdio child, where a dead server is an *explicit* tool failure — compatible with the rule against retrying an uncertain action.

**Contract:** [SPEC agent surface](SPEC.md#13-agent-surface) and [SPEC MCP trading plane](SPEC.md#131-mcp-trading-plane).

### D5 — MarketRig exposes primitives, not trading conclusions

**Decision:** Agent-facing capabilities provide authoritative observations and bounded actions, not conclusions such as `find_alpha`, `should_buy`, or `choose_strategy`.

**Rationale:** The external agent is responsible for orientation and decisions; MarketRig owns reality and action.

**Contract:** [PRD responsibility model](PRD.md#6-responsibility-model) and [SPEC trading boundary](SPEC.md#12-trading-and-public-data-boundary).

### D6 — MarketRig is modeled as an OODA environment with persistence

**Decision:** MarketRig owns Observe, Act, and Continuity. The agent owns Orient, Decide, and Learning through its research, files, Hindsight, and skills.

**Rationale:** This boundary separates authoritative environment behavior from subjective cognition without inventing a MarketRig reasoning loop.

**Contract:** [PRD responsibility model](PRD.md#6-responsibility-model).

### D7 — Multiple durable desks ship from day one

**Decision:** MarketRig supports multiple durable, isolated desks from day one. The first milestone exercises at least two desks. Every desk-scoped operation carries the durable desk UUID end to end; the daemon has no process-global selected desk, and relationships between desk-owned rows include `desk_id` in their foreign keys.

**Rationale:** A single-desk prototype would avoid the product's core identity and isolation boundary. Explicit UUID scope and composite constraints enforce that boundary instead of relying on UI selection or caller discipline.

**Contract:** [SPEC desk model](SPEC.md#5-desk-model-and-ownership) and [SPEC verification](SPEC.md#17-verification).

### D8 — There is no MarketRig Run entity

**Decision:** **Session** means a native Codex or Claude Code session. Trigger executions and MarketRig actions retain their own records; MarketRig persists no Run identity or state machine.

**Rationale:** A Run duplicated native session lifecycle and mixed unrelated agent, trigger, and action provenance.

**Contract:** [SPEC terminology](SPEC.md#2-canonical-terminology) and [SPEC desk model](SPEC.md#5-desk-model-and-ownership).

### D9 — NautilusTrader is bundled as the trading authority; OpenBB research is deferred past MVP

**Decision:** MarketRig bundles required NautilusTrader for execution-relevant market state and paper execution/accounting. Agents access it through MarketRig-defined capabilities: the canonical CLI plus the narrow real-time market MCP resources required by D63, never its native API. OpenBB research is deferred past MVP on scope: MVP bundles no research provider, and research is the agent's own through its shell, tools, and workspace. When OpenBB arrives it is optionally configured, informational only, never a writer of trading state, reached only through `marketrig`, and hosted in its own locked environment beside the daemon's.

**Rationale:** Trading truth and research data have different authority and failure requirements. The MVP's evidence is the persistent loop, which needs authoritative market state and paper execution but not a bundled research library, and a coding agent already researches with its own tools. Bundling OpenBB would mean one more supervised child of the same shape as Hindsight's, with its own interpreter and locked dependency set (D47, D65) — packaging, supervision, and lifecycle work the MVP's evidence does not need.

`ponytail:` no bundled research in MVP; add OpenBB post-MVP as one supervised child beside Hindsight's, with its own locked environment and installation-level connector credentials in the OS store.

**Contract:** [SPEC trading boundary](SPEC.md#12-trading-and-public-data-boundary).

### D10 — MVP approval is configurable; risk policy is not a daemon feature

**Decision:** MVP structurally permits only NautilusTrader sandbox/paper execution. It has no separate daemon risk-policy engine. One installation-wide **Always allow | Require approval** setting covers interactive and trigger-code paper orders, defaulting to **Always allow** until the desktop's approval controls exist (D70).

**Rationale:** Optional user approval serves the experiment, while bankroll, leverage, shorting, and strategy policy remain user-agent concerns.

**Contract:** [SPEC trading actions](SPEC.md#123-trading-actions-and-approvals) and [SPEC verification](SPEC.md#17-verification).

### D11 — MVP platforms are Windows and Apple Silicon macOS

**Decision:** MVP supports Windows and Apple Silicon macOS with feature parity. Each platform ships a per-user MarketRig release unit; Codex and Claude Code remain external installations.

**Rationale:** Linux would expand terminal, packaging, and lifecycle work without serving the initial target.

**Contract:** [SPEC installation and platform boundary](SPEC.md#4-installation-platform-and-security-boundary).

### D12 — Delivery is a thin persistent vertical slice

**Decision:** Milestones are ordered to prove the smallest end-to-end persistent event-driven paper-trading loop across Windows and Apple Silicon macOS with both Codex CLI and Claude Code — multiple desks, real terminal attachment, scheduled trigger code, paper action, activation when no managed session is live, and daemon-restart persistence — rather than to complete each subsystem horizontally. The first milestone is the thinnest base of that slice: the workspace, the sole-writer store, desk identity, and the loopback API a client can reach.

**Rationale:** A vertical slice tests the defining architecture earlier than horizontally completing each subsystem, and it fails cheaply while there is still almost nothing built on top of it.

**Contract:** [ROADMAP Milestone R0](ROADMAP.md#milestone-r0--workspace-daemon-and-desk-identity) and [SPEC verification](SPEC.md#17-verification).

### D13 — MVP has no cross-device portability or updater

**Decision:** MVP has no desk sync, desk export/import, application updater, arbitrary cross-version state-compatibility guarantee, or downgrade contract. A release may carry the forward-only SQLite schema migrations it needs for its supported upgrade path. Uninstall preserves user data unless the user explicitly erases it.

**Rationale:** Those features would freeze storage and compatibility contracts before the vertical slice validates them.

**Contract:** [SPEC installation](SPEC.md#41-installation) and [ROADMAP deferred work](ROADMAP.md#deferred--post-mvp).

### D14 — MarketRig is licensed under AGPL-3.0-only

**Decision:** License MarketRig under `AGPL-3.0-only` and satisfy corresponding source and notice obligations for distributed releases.

**Rationale:** MarketRig intends to integrate and distribute AGPL-licensed OpenBB after MVP (D9) and to remain open source rather than require a commercial OpenBB license; choosing the license now keeps that path open without a relicensing step.

**Contract:** [SPEC installation](SPEC.md#41-installation).

## Desk identity and continuity

### D15 — One desk is one persistent autonomous trader

**Decision:** A desk is one persistent autonomous trader identity with a lowercase textual UUIDv7 ID and a unique immutable lowercase-kebab name. Its runtime and native sessions may change without changing that identity. Its default workspace is `~/.marketrig/desks/<desk-name>/`; the final folder name does not contain the UUID.

**Rationale:** The durable unit must survive runtime replacement and repeated disposable conversations without becoming a multi-agent organization. A stable internal identity and readable workspace name serve different concerns without leaking UUIDs into the user-facing path.

**Contract:** [PRD desk model](PRD.md#51-desk) and [SPEC desk model](SPEC.md#5-desk-model-and-ownership).

### D16 — Hindsight is the MVP subjective memory system

**Decision:** Use Hindsight for desk-scoped experiential memory in MVP. `marketrigd` owns one installation-wide local Hindsight instance and maps each desk to exactly one bank in its shared Hindsight database. Agents access only MarketRig-defined retain, recall, and reflect capabilities through `marketrig`; MarketRig derives the bank from desk identity and never accepts an agent-supplied bank ID. MarketRig does not install runtime-native Hindsight integrations, automatically retain transcripts, or expose Hindsight's direct API, MCP server, or control plane to agents.

**Rationale:** Its retain, recall, and reflect semantics match the desired optional learning loop outside an LLM context window, while one small cross-runtime MarketRig interface preserves desk isolation and the agent-owned learning contract.

**Contract:** [SPEC memory and skills](SPEC.md#16-memory-and-skills).

### D17 — Hindsight is agent-owned cognition, not MarketRig's system of record

**Decision:** The agent chooses what enters Hindsight. MarketRig and Nautilus state remain authoritative for objective session, trigger, action, and trading reality.

**Rationale:** Subjective learned meaning and objective operational truth have different ownership and correctness requirements.

**Contract:** [SPEC memory and skills](SPEC.md#16-memory-and-skills).

### D18 — Hindsight persists locally and uses hosted models for MVP

**Decision:** Run the Hindsight instance and its embedded pg0 persistence locally. Hindsight's LLM and embedding models are hosted for MVP behind one installation-wide OpenAI-compatible base URL, API key, selected LLM model, and embedding model; MVP uses no reranking model, and any later reranker is hosted, never local. The embedding model is fixed once Hindsight has initialized data with it. Persist the base URL and selected models, keep the API key in the OS credential store, and fetch the provider's model list live whenever the model selector is opened without persisting or caching that list. A model-list fetch failure is explicit and never falls back to a stale list. Hindsight unavailability makes memory capabilities explicitly unavailable but does not block daemon startup, native sessions, triggers, or paper trading.

**Rationale:** This preserves local durable memory while avoiding unnecessary native model packaging and compute requirements on the two target platforms. Embedded pg0 is sufficient for the local experimental MVP; a production storage topology is outside the MVP contract.

**Contract:** [SPEC configuration scopes](SPEC.md#44-configuration-scopes) and [SPEC memory and skills](SPEC.md#16-memory-and-skills).

### D19 — Skills are persistent procedural memory

**Decision:** Keep skills as a distinct agent-owned procedural-memory layer. Borrow the Hermes skill-improvement pattern without adopting Hermes as the product foundation. Agents may create and refine skills; MarketRig preserves the environment but does not govern improvement automatically.

**Rationale:** Procedures can improve across sessions without fine-tuning or adopting another agent framework as the product foundation.

**Contract:** [SPEC memory and skills](SPEC.md#16-memory-and-skills).

### D20 — Desk bootstrap has explicit ownership boundaries

**Decision:** MarketRig/user owns enforced configuration, the user owns the `AGENTS.md` constitution, MarketRig owns designated built-in compatibility material, and the agent owns research, tools, learned skills, and Hindsight content. Desk creation is the recoverable provisioning state machine `CREATING -> READY | FAILED`: persist the row first, bootstrap idempotently, recover incomplete creation on startup, and retry a failure on the same identity. `CLAUDE.md` is MarketRig-owned and contains exactly `@AGENTS.md` followed by one newline. Agent-owned seed material is never rewritten after the desk first becomes ready. Hindsight, NautilusTrader, and runtime resources are provisioned lazily by their owning modules.

**Rationale:** Explicit ownership prevents configuration drift and accidental self-modification of the desk mandate. A persisted provisioning boundary makes filesystem creation recoverable without coupling desk readiness to external subsystems.

**Contract:** [SPEC workspace ownership](SPEC.md#51-workspace-ownership).

### D21 — Canonical skills are shared across supported runtimes per desk

**Decision:** Each desk stores its one canonical skill set under `.agents/skills/`. Claude Code reaches the same physical directory through `.claude/skills`: a relative symlink to `../.agents/skills` on macOS and a directory junction to the absolute workspace path on Windows. MarketRig does not copy or synchronize a second skill tree.

**Rationale:** Changing runtime must not fork the desk's procedural identity.

**Contract:** [SPEC memory and skills](SPEC.md#16-memory-and-skills).

### D22 — Desk self-improvement follows a modified OODA loop

**Decision:** MarketRig's defining loop is **Observe → Orient → Decide → Act → Evaluate → Learn → Observe**. MarketRig exposes authoritative observations and actions, persists continuity, and queues an evaluation prompt for every realized-P&L event. The agent selects relevant evidence, treats realized P&L as the reward signal, decides whether anything was learned, and when useful retains desk-specific lessons in that desk's Hindsight bank and improves reusable procedures in that desk's canonical skills. Each desk's `AGENTS.md` always carries this ownership loop; MarketRig seeds an on-demand improvement skill into the desk, after which the agent owns and may evolve it. MarketRig does not require a canonical transcript, decision-attribution model, reflection cadence, or daemon-authored memory/skill mutation.

**Rationale:** Self-improvement through experiential memory and procedural refinement is MarketRig's differentiator, while leaving cognition and interpretation with the external agent avoids inventing a daemon reasoning engine.

**Contract:** [PRD durable continuity](PRD.md#55-durable-continuity), [PRD responsibility model](PRD.md#6-responsibility-model), and [SPEC memory and skills](SPEC.md#16-memory-and-skills).

## Runtime and desktop lifecycle

### D23 — `marketrigd` is the authoritative long-running runtime coordinator

**Decision:** `marketrigd` owns authoritative MarketRig state, scheduling, runtime processes, terminals, and managed sidecar lifecycle. The desktop and CLI are clients.

**Rationale:** Durable work must survive desktop attachment and window lifecycle.

**Contract:** [SPEC architecture](SPEC.md#3-top-level-architecture).

### D24 — Real Codex and Claude sessions keep their native terminal while structured control stays runtime-native

**Decision:** Host each genuine interactive runtime through Unix PTY on macOS and ConPTY on Windows. `marketrigd` owns one lazy-started installation-wide control plane per runtime, shared by every desk using that runtime and kept alive until Quit. Codex uses one app-server with a native remote TUI per desk session. Claude Code uses one daemon-owned Channel router with one thin bridge per active desk session. Runtime control planes remain isolated from one another, and terminal and runtime-specific mechanics remain private to their adapters.

**Rationale:** MarketRig can preserve the native runtime experience without using terminal keystrokes as an automation protocol or forcing both runtimes behind one invented control surface.

**Contract:** [SPEC managed runtime lifecycle](SPEC.md#6-managed-runtime-lifecycle).

### D25 — Shell choice and PTY choice are separate concerns

**Decision:** PTY/ConPTY is the terminal substrate. Runtime discovery resolves the exact executable MarketRig will launch; sessions are spawned directly without a command shell. On Windows, discovery refreshes the native user/machine `PATH`, supports `.cmd` and `.bat` launchers through `ComSpec`, and ignores PowerShell aliases, functions, and profiles. On macOS, onboarding captures the login-shell environment once with a timeout, keeps it only in memory, and then launches the selected executable directly. User shell policy remains separate from terminal hosting.

**Rationale:** Terminal hosting and user shell policy solve different cross-platform problems.

**Contract:** [SPEC managed runtime lifecycle](SPEC.md#6-managed-runtime-lifecycle).

### D26 — Closing hides to tray; Quit shuts MarketRig down

**Decision:** Closing the main window hides MarketRig to the tray and leaves operations running. Explicit **Quit MarketRig** gracefully stops managed sessions, processes, sidecars, and the daemon.

**Rationale:** Long-running work should continue visibly through a tray lifecycle, while Quit must have unambiguous whole-application semantics.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D27 — MarketRig does not invent a canonical agent-status state machine

**Decision:** A desk has at most one managed native agent session, while trigger code remains separate and may run concurrently. MarketRig records managed process/terminal liveness, last native session pointers per desk and runtime, delivery outcomes, exits/failures, and one-shot attention events. It does not persist or infer canonical `INACTIVE | IDLE | WORKING | WAITING` agent states. Runtime-native status text may be shown as non-authoritative presentation only.

**Rationale:** Process liveness and durable delivery evidence are facts MarketRig owns; interpreting an external agent's cognitive state would create a brittle second lifecycle.

**Contract:** [SPEC managed runtime lifecycle](SPEC.md#6-managed-runtime-lifecycle).

### D28 — Activation resumes when possible and otherwise starts fresh

**Decision:** Activation first attempts the selected runtime's last native session and otherwise starts a new one. Codex starts with `codex --remote <endpoint> -C <workspace>` and resumes with `codex resume <thread-id> --remote <endpoint> -C <workspace>`; MarketRig captures the canonical thread ID from app-server events. Claude Code starts with a MarketRig-generated UUID through `claude --session-id <uuid>` and resumes explicitly with `claude --resume <uuid>` in the desk workspace; MarketRig never uses ambient `--continue`. Runtime-owned arguments for the Claude Channel accompany both launches. No initial prompt is passed on the process command line: orientation and pending notices arrive afterward through structured runtime input as ordinary user-level content, never as a system prompt. Explicit **Continue last session** does not silently fall back to a new session.

**Rationale:** Native continuity should be preserved when available without making it a durability requirement.

**Contract:** [SPEC activation](SPEC.md#7-activation).

### D29 — The desktop is a three-panel control plane over the real terminal

**Decision:** The desktop conceptually provides desk navigation on the left, the attachable native terminal in the center, and selected-desk data and controls on the right.

**Rationale:** Users need durable navigation and MarketRig state without replacing the native agent terminal experience.

**Contract:** [PRD desktop behavior](PRD.md#8-desktop-behavior) and [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D30 — The desktop uses Tauri 2 with ghostty-web

**Decision:** Build the native shell with Tauri 2 and render attached terminals with ghostty-web in the system webview. Tauri remains a thin shell; ghostty-web owns presentation only; `marketrigd` owns terminal and session reality.

**Rationale:** This supplies native tray/window lifecycle and a capable terminal without shipping Electron or moving domain authority into a second backend.

**Contract:** [SPEC architecture](SPEC.md#3-top-level-architecture) and [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D31 — One private terminal manager hosts PTY on macOS and ConPTY on Windows

**Decision:** One private `TerminalManager` owns terminal creation, raw-byte pumping, resize, attachment, process containment, and shutdown, over Unix PTY on macOS and ConPTY on Windows through one maintained Rust PTY crate (crate selection at plan time). Each live terminal has one current attachment generation and a bounded in-memory reconnect ring; resize requests coalesce, shutdown drains before process termination, and no transcript or screen snapshot is persisted. A PTY is never resumable. Whether macOS needs a separate single-threaded spawn helper is settled at plan time against the chosen crate and the daemon's own threading model, because late `forkpty` in an already-threaded process is unsafe.

**Rationale:** Single ownership keeps terminal mechanics out of the runtime adapters, and fork-safety is a property of the host process rather than of the product, so it is established against the daemon's actual threading model rather than assumed from a crate's documentation.

**Contract:** [SPEC terminal manager](SPEC.md#65-terminal-manager).

### D32 — Runtime lifecycle uses structured events, not terminal output as authority

**Decision:** Runtime adapters consume supported structured events associated with a desk and native session only for facts MarketRig needs: pointer discovery, readiness for structured input, explicit attention, interruption, exit, and failure. Codex uses app-server events. Claude Code uses supported hooks plus Channel registration and delivery evidence. Terminal parsing may support presentation but never establishes authoritative lifecycle, approvals, delivery, or actions, and MarketRig does not normalize the agent's cognitive status.

**Rationale:** Hook output and terminal sequences are not reliably observable across runtimes and platforms.

**Contract:** [SPEC runtime adapter](SPEC.md#64-runtime-adapter-contract).

### D33 — Tray hiding keeps frontend terminal presentations warm

**Decision:** Closing hides the existing Tauri window without destroying its webview or daemon connections. The frontend keeps one bounded ghostty-web presentation warm per live managed terminal. If that presentation is lost, the daemon and PTY continue, but exact screen reconstruction is not an MVP guarantee.

**Rationale:** Warm presentation avoids rebuilding terminal-emulator state that ghostty-web does not expose through a supported export/import contract.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

## Triggers and action integrity

### D34 — Triggers are desk-bound daemon jobs

**Decision:** A trigger is a durable desk-bound `marketrigd` job with source `SCHEDULED | EVENT`, recurrence `ONE_OFF | RECURRING`, preserved brief/context, optional code, and a raw result. An occurrence is a schedule or event candidate; only an atomic acceptance transaction creates an immutable firing. That transaction snapshots the current brief/context and approved code identity, consumes a one-off or advances a recurring definition, and is idempotent by trigger plus occurrence identity. Immediate user input is not a trigger. Each EVENT occurrence is durably accepted with a stable identity in its ingress scope. One distinct occurrence may independently fire every enabled matching trigger. Execution or delivery failure does not rearm a consumed one-off, and reaccepting the same occurrence identity creates no additional firing.

**Rationale:** One trigger concept is clearer than separate producer, event, occurrence, delivery, and manual-prompt product entities. Consuming a one-off when its firing becomes durable preserves at-most-once behavior without making execution success an implicit retry policy.

**Contract:** [SPEC trigger model](SPEC.md#8-trigger-model).

### D35 — Trigger code is an approved immutable snapshot

**Decision:** Store each code version as an immutable UTF-8 source snapshot plus a direct argument vector containing one script-path placeholder. Source, launch specification, suffix, timeout, and their SHA-256 fingerprint are approved together under one UUIDv7 snapshot identity; only changing that snapshot requires reapproval. Code runs once per firing as the current user in the desk workspace under daemon-owned timeout, concurrent stream capture, output limits, and process-tree control. Completion persists the execution outcome, artifact metadata, and queued result prompt atomically; failure or uncertain completion is never retried automatically.

**Rationale:** The executed code must remain identical to what the user approved while retaining the usefulness of the user's normal environment.

**Contract:** [SPEC trigger model](SPEC.md#8-trigger-model) and [SPEC trigger execution](SPEC.md#9-trigger-code-execution).

### D36 — Daemon prompts use durable structured queueing; steering is retained but disabled

**Decision:** Persist each daemon-originated prompt before delivery. Delivery state is `QUEUED | DELIVERED | FAILED`, ordered FIFO per desk, with no automatic retry after failure or uncertain acceptance. The installation policy retains `STEER | QUEUE`, but MVP selects `QUEUE` and keeps `STEER` disabled. The adapter waits behind any active native turn using runtime-private safe-delivery gating; this gating is not a product-level agent status. Codex submits through its supported app-server queue. Claude Code receives one FIFO prompt at a time through a MarketRig-owned one-way custom Channel; a thin stdio-MCP bridge binary, built from MarketRig's own Cargo workspace (D53), forwards authenticated loopback WebSocket notifications and owns no durable state or business logic. Claude delivery is considered handed off when written to the Channel transport because the protocol provides no acknowledgement. Message delivery never interrupts a turn and never uses keystroke emulation. Onboarding rejects runtime versions or configurations without the required structured queueing capability.

**Rationale:** Queueing is sufficient for the persistent event-driven loop and is the common safe behavior across both native runtimes. Retaining the disabled steering option preserves the product direction without making it part of the MVP contract.

**Contract:** [SPEC daemon prompt delivery](SPEC.md#11-daemon-prompt-delivery) and [SPEC runtime adapter](SPEC.md#64-runtime-adapter-contract).

### D37 — Missed scheduled occurrences never become firings

**Decision:** A scheduled occurrence becomes a firing only when the current daemon was already running before its deadline and accepts it within a 60-second lateness tolerance. Older occurrences after sleep, downtime, or clock jump are summarized as operational miss evidence and never materialized or replayed as firings. Missed one-offs are terminal; recurring schedules keep their original anchor and advance directly to the next future occurrence. Recurrence uses explicit IANA timezones with defined DST behavior.

**Rationale:** Stale market work can be more dangerous than a recorded miss.

**Contract:** [SPEC scheduled-trigger semantics](SPEC.md#10-scheduled-trigger-semantics).

### D38 — NautilusTrader produces trading facts; MarketRig SQLite preserves their durable history

**Decision:** NautilusTrader is authoritative for paper execution and accounting calculations. `marketrigd` records the resulting immutable orders, individual fills, position cycles, fees, realized P&L, and state needed to restore the desk's paper book in MarketRig's installation-wide SQLite database. That SQLite record is the authoritative durable trading history; MarketRig may build query projections but does not independently recalculate or rewrite Nautilus-produced facts. Persisting a realized-P&L record and its queued evaluation prompt is one transaction. The daemon also owns immutable approval/provenance records, serializes the action boundary, and requires a desk-scoped idempotency identity for every mutating trading command.

**Rationale:** NautilusTrader's stable cache persistence is a recovery mechanism rather than a complete event archive, while the experimental learning loop requires complete, queryable, crash-safe history without adding a database server or depending on its evolving event store.

**Contract:** [SPEC trading boundary](SPEC.md#12-trading-and-public-data-boundary) and [SPEC persistence](SPEC.md#15-persistence-crash-recovery-and-history).

### D39 — MarketRig consumes NautilusTrader through its native Rust crates

**Decision:** Build on the published `nautilus-*` crates.io releases, pinning one exact tested version line across every `nautilus-*` crate in lockstep — `=0.62.0`. MarketRig's market data comes from out-of-tree `DataClient` implementations in MarketRig's own crates, and paper execution comes from `nautilus-sandbox`'s `SandboxExecutionClient` wired through the live node builder's simulated-client entry point. Do not use the Python/PyO3 surface, do not support 1.x, and do not fork or patch the crates. The `high-precision` Cargo feature is set explicitly in the workspace rather than inherited through feature unification.

**Rationale:** Two spikes on 2026-09-01 proved every capability this decision rests on, at `=0.62.0` on rustc 1.98.0 / edition 2024, with no forks, no patches and no private types. The data spike: an out-of-tree `DataClient` plus `DataClientFactory` and `ClientConfig`, in a plain downstream crate, fed live Yahoo AAPL quotes to an actor inside a running `LiveNode` — 440 lines, one compile error — and the shipped `nautilus-kraken` 0.62.0 adapter reaches the engine through the *same* public traits and the *same* `get_data_event_sender()` call with no in-tree privilege, so this is the supported path rather than a back door. The execution spike: `nautilus-sandbox` 0.62.0 is a complete live-wireable paper adapter that needed nothing composed or forked; a full round trip produced `OrderFilled` → `PositionClosed` with net-of-fees realized P&L of 4.83 USD on a +5.00 move (commissions 0.10 and 0.11, charged from the instrument's own rates), and cancelling a resting limit order worked in about 0.3 ms. Multi-book isolation is recorded in D64; the agent-surface evidence is D4's.

Three risks are accepted rather than mitigated:

- **0.x API churn.** Upstream has written into the source that the `DataEvent` routing enum every custom adapter must construct may be replaced by generic dispatch, and three near-identical factory traits are already diverging — `DataClientFactory::create` takes a clock, `ExecutionClientFactory::create` does not, and `SimulatedExecutionClientFactory::create` takes a mutable cache handle instead of a read-only `CacheView`. Neither the crates nor their roots make a semver or stability promise. Bumping the pinned line is an all-or-nothing move verified by the trading module's own checks.
- **Licensing.** Every `nautilus-*` crate is `LGPL-3.0-only` while MarketRig is `AGPL-3.0-only` (D14). Consuming LGPL-3.0-only crates from an AGPL-3.0-only work is compatible, and MarketRig distributes its own corresponding source regardless.
- **Silent feature unification.** `nautilus-model`'s own default feature set is empty, but `nautilus-sandbox` and `nautilus-kraken` default to `high-precision`, so adding an adapter flips `Price` and `Quantity` to 128-bit internals for the entire build with nobody typing a flag. The workspace therefore pins the precision mode explicitly and CI asserts the node banner's reported precision.

**Contract:** [SPEC trading boundary](SPEC.md#12-trading-and-public-data-boundary) and [SPEC trading authority and node topology](SPEC.md#121-trading-authority-and-node-topology).

### D40 — Scheduling stays daemon-owned and uses RRULE recurrence calculation

**Decision:** Keep scheduling in one `marketrigd` coordinator task with SQLite `next_occurrence_ns` projections as durable authority and an in-memory wake signal after schedule mutations. One-offs store an absolute UTC instant. Recurring schedules store one unbounded RFC 5545 RRULE, a naive local `DTSTART`, and an explicit IANA timezone; local recurrence calculation and UTC resolution come from one maintained RFC 5545 recurrence crate and one IANA timezone-database crate, pinned exactly (crate selection at plan time). The first scheduling milestone rejects sub-minute recurrence, `COUNT`/`UNTIL`, multiple rules, and RDATE/EXDATE sets. The scheduler rechecks wall time at most every 60 seconds and uses atomic compare-and-accept transactions rather than a scheduler framework or separate job store.

**Rationale:** A scheduler framework would introduce a duplicate job model and persistence boundary when only calendar recurrence calculation is missing.

**Contract:** [SPEC scheduled-trigger semantics](SPEC.md#10-scheduled-trigger-semantics).

### D41 — Trigger code uses native process-group containment

**Decision:** Launch trigger code directly without a command shell. Use POSIX sessions/process groups on macOS and a kill-on-close Job Object on Windows through a maintained Windows API binding (crate selection at plan time). MarketRig promises ordinary descendant cleanup, not an adversarial sandbox.

**Rationale:** Native containment covers the MVP execution contract without a container, VM, process-tree walker, or extra sidecar.

**Contract:** [SPEC trigger execution](SPEC.md#9-trigger-code-execution).

## Local system architecture

### D42 — MVP is local, single-user, and has two configuration scopes

**Decision:** MVP is a local single-user product with installation settings and desk configuration only. Typed non-secret configuration is authoritative in MarketRig SQLite and changes only through `marketrigd`; there is no live settings file. Provider credentials are installation-level and remain behind the daemon and OS credential-store boundary.

**Rationale:** Remote accounts, multi-user authorization, and layered overrides add complexity unrelated to the experiment.

**Contract:** [SPEC installation and security boundary](SPEC.md#4-installation-platform-and-security-boundary).

### D43 — The implementation stack is Rust, and `marketrigd` is a standalone binary

**Decision:** Implement `marketrigd`, the `marketrig` CLI, and `marketrig-mcp` in Rust on rustc 1.98 or newer with edition 2024 — the pinned `nautilus-*` 0.62.0 crates declare `rust-version = "1.97.1"` and edition 2024, and 1.98.0 is the toolchain both spikes ran on — in one Cargo workspace (D53). `marketrigd` is a library crate plus a thin standalone binary. The Tauri shell spawns and adopts that binary; the daemon is never hosted inside the GUI process.

**Rationale:** NautilusTrader's native crates are the trading authority (D39), and Rust is the only language that consumes them without an interpreter boundary, so the daemon needs no language runtime of its own in the installer. The daemon stays a separate process on product merits, with rebuild cost deliberately ignored: continuity must not be bound to a GUI process lifetime, since quitting, updating, or crashing the window would stop a desk's clock; a GUI process is the wrong host for a scheduler and a trading node, given webview and GPU crash blast radius and macOS App Nap and timer throttling against the 60-second occurrence recheck (D40); and agents reach the daemon over authenticated loopback regardless (D44), so embedding would relocate the API server without deleting it. Keeping `marketrigd` a library crate behind its binary leaves embedding a cheap, reversible option if that ever changes.

**Contract:** [SPEC architecture](SPEC.md#3-top-level-architecture).

### D44 — Desktop and CLI use an authenticated loopback API

**Decision:** `marketrigd` exposes REST/JSON and WebSocket over TCP loopback. Desktop and CLI connect directly; Tauri only starts or discovers the daemon. Connections use a per-start bearer credential, and WebSockets also validate exact allowed origins.

**Rationale:** One authenticated local contract avoids both a Tauri proxy and separate platform-specific transports. Loopback alone is not a trust boundary.

**Contract:** [SPEC local single-user boundary](SPEC.md#43-local-single-user-boundary).

### D45 — MarketRig durable state uses SQLite and plain SQL

**Decision:** Store MarketRig-owned structured state in SQLite through a thin binding that keeps plain SQL in view, using explicit `BEGIN IMMEDIATE` transactions, WAL mode, and `STRICT` tables (`rusqlite` is the candidate — verified on crates.io 2026-09-01, 0.40.x line; the exact pin is set at plan time). `marketrigd` is the sole writer, and persistence stays cohesive and private to it. UUIDv7 text identifiers, `*_ns` nanosecond instants, and decimal **text** for money — never a float column — remain the storage conventions. Do not add an ORM, a query builder, a compile-time query-checking layer, or an alternate-storage interface for one implementation.

**Rationale:** One authoritative local writer needs transactional recovery, not a database server, ORM, alternate-storage interface, or speculative abstraction. The "no ORM" half is stated rather than assumed because the ecosystem's gravity pulls toward an ORM, and gravity is not evidence of need.

**Contract:** [SPEC persistence and history](SPEC.md#15-persistence-crash-recovery-and-history).

### D46 — One installation-wide database stores all MarketRig state

**Decision:** Use one SQLite database for all MarketRig-owned structured state. Desk rows carry durable desk identity; installation rows do not. NautilusTrader books and agent files retain their separate ownership.

**Rationale:** Per-desk databases would complicate installation-wide scheduling, approvals, history, and recovery without creating a security boundary inside one user account.

**Contract:** [SPEC persistence and history](SPEC.md#15-persistence-crash-recovery-and-history).

### D47 — The installer bundles a portable CPython solely for the supervised Hindsight child

**Decision:** Package with Tauri 2 as per-user NSIS on Windows and a DMG-distributed app on Apple Silicon macOS. The bundle carries `marketrigd`, the `marketrig` CLI, and `marketrig-mcp` as native executables, plus **one** private portable CPython runtime and pinned wheel environment whose only purpose is the supervised Hindsight child (D65). Nothing else in MarketRig depends on an interpreter, and no system Python is required.

**Rationale:** Hindsight is a Python package with no Rust equivalent, so an interpreter has to exist somewhere; bundling it with a resolved wheel set keeps first run offline-first and zero-setup and moves every dependency-resolution failure to build time instead of to a desk stuck in `CREATING`. Installer size is not an argument against it, since Hindsight's embedded pg0 is in the bundle regardless. Interpreter security updates ride application releases, over one small wheel set.

**Contract:** [SPEC installation](SPEC.md#41-installation) and [SPEC memory and skills](SPEC.md#16-memory-and-skills).

### D48 — A Rust web framework serves the loopback API

**Decision:** Serve the REST/JSON and WebSocket loopback API from the `marketrigd` process itself on one maintained Rust web framework over Tokio and hyper, listening on `127.0.0.1` only. `axum` is the candidate — verified on crates.io 2026-09-01: 0.8.x line, newest 0.8.9, actively maintained — and the exact pin belongs in SPEC and the workspace manifest, not here. REST and WebSocket share one listener and one process; no process manager, no reverse proxy, no second transport.

**Rationale:** A local single-user coordinator does not need a public-server process manager or platform-specific event-loop acceleration. What it does need is routed handlers, a WebSocket upgrade on the same listener the REST routes use (D44, D66), and a description of its own routes that D59's generator can consume — and nothing beyond that.

**Contract:** [SPEC architecture](SPEC.md#3-top-level-architecture) and [SPEC local boundary](SPEC.md#43-local-single-user-boundary).

### D49 — Provider secrets use native OS credential stores

**Decision:** `marketrigd` alone stores provider secrets in the native per-user credential store — macOS Keychain or Windows Credential Locker — through one maintained, exactly pinned Rust binding (`keyring` is the candidate — verified on crates.io 2026-09-01, 4.x line; selection confirmed at plan time). SQLite keeps only non-secret configuration and opaque references; native-store failure has no plaintext fallback. Secrets never appear in SQLite, logs, prompts, URLs, or CLI output. The one stated exception is the supervised Hindsight child, which receives its endpoint key and the daemon's per-start bearer through its own process environment because Hindsight is configured only that way (D65).

**Rationale:** Native per-user stores avoid exposing credentials to workspaces, the webview, Tauri, SQLite, or a second application vault.

**Contract:** [SPEC configuration scopes](SPEC.md#44-configuration-scopes).

### D50 — The CLI is a thin synchronous client of the daemon

**Decision:** Parse the CLI with one maintained argument parser and call the daemon with a blocking HTTP client using finite timeouts, no environment proxy inheritance, and redirects disabled (crate selections at plan time). Commands are deterministic; machine output is JSON under `--json` and human output is plain text. The CLI is never a generated SDK of the daemon.

**Rationale:** The thin same-release client needs deterministic commands and safe loopback calls, not another application framework or generated SDK.

**Contract:** [SPEC CLI continuity plane](SPEC.md#132-cli-continuity-plane).

### D51 — Diagnostics use bounded local platform logs

**Decision:** Write daemon diagnostics as bounded JSON Lines through one structured-logging facade (`tracing` is the candidate — verified on crates.io 2026-09-01, 0.1.x line) and write selected desktop diagnostics through Tauri's official log plugin. Logs remain local, non-authoritative, and secret-free.

**Rationale:** Persistent failure evidence is useful, but a local MVP does not need hosted observability or another logging framework.

**Contract:** [SPEC persistence and history](SPEC.md#15-persistence-crash-recovery-and-history).

### D52 — Native autostart and notifications use official Tauri plugins

**Decision:** Use Tauri's official autostart and notification plugins with narrowly scoped capabilities. Autostart enters the tray lifecycle; notifications are reserved for actionable daemon conditions.

**Rationale:** These are native shell responsibilities already covered by maintained cross-platform plugins.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D53 — One Cargo workspace releases the daemon, the CLI, and the MCP adapter

**Decision:** One Cargo workspace builds and releases `marketrigd`, the `marketrig` CLI, and the `marketrig-mcp` stdio adapter together: one version, one lockfile, one boundary model, and shared internal crates rather than published libraries. None of the three has an independent lifecycle, and there is no versioning seam between them.

**Rationale:** They share a release and their boundary models — the CLI is a client of the daemon's own contract, and the adapter reads that same daemon on every request — so separate release units would add a compatibility matrix with nothing on the other side of it.

**Contract:** [SPEC architecture](SPEC.md#3-top-level-architecture) and [SPEC installation](SPEC.md#41-installation).

### D54 — The repository is a root Cargo workspace beside the Vue/Vite project

**Decision:** Keep Vue/Vite at the repository root, with its `package.json`, `index.html`, and `src/`. Rust workspace members live under `crates/` — `marketrigd`, `marketrig`, `marketrig-mcp`, and any shared internal crate — with the workspace manifest and `Cargo.lock` at the root, and `src-tauri/` keeps its Tauri-conventional place as a member of that same workspace, so one `cargo fmt`, `cargo clippy`, and `cargo test` pass covers every Rust crate. Use pnpm and Cargo directly, without a monorepo framework or general task runner.

**Rationale:** Each ecosystem keeps its conventional layout without colliding over `src/`: the frontend already owns the root `src/`, so the Rust crates get their own directory rather than the root. One workspace is the release boundary D53 already decided, and orchestration on top of two package managers would buy nothing.

**Contract:** [SPEC architecture](SPEC.md#3-top-level-architecture).

## Frontend and development toolchain

### D55 — The Tauri frontend uses Vue 3, TypeScript, and Vite

**Decision:** Build the system-webview frontend with Vue 3, TypeScript, Vite, Composition API, and `<script setup lang="ts">` single-file components.

**Rationale:** MarketRig needs a typed reactive desktop UI but not React's broader ecosystem or a server-rendered application framework.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D56 — The frontend targets the latest supported TypeScript 6

**Decision:** Pin the newest TypeScript 6.x release verified with the complete Vue, Vite, language-tooling, and type-checking stack. TypeScript 7 or later requires a superseding decision.

**Rationale:** First-class Vue compiler compatibility matters more than adopting a newer compiler through compatibility bridges.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D57 — The frontend uses Tailwind CSS and Reka UI primitives

**Decision:** Style with Tailwind CSS 4 and use Reka UI 2 directly for behavior-heavy accessible primitives. Prefer native HTML for ordinary controls and add local styled components only after repeated use.

**Rationale:** MarketRig needs a distinct visual language and accessible behavior, not a full component-suite dependency.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D58 — Frontend builds use Node.js 24 LTS and pnpm 11

**Decision:** Build with an exact tested Node.js 24 LTS release and a root `packageManager`-pinned pnpm 11 release, including its integrity hash, provisioned through Corepack. The lockfile is authoritative; these tools are not shipped in the installer.

**Rationale:** The frontend toolchain needs reproducible builds without becoming part of MarketRig's installed runtime.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

### D59 — Hey API generates the frontend REST client

**Decision:** Generate frontend REST types and the native-Fetch SDK from the daemon's OpenAPI document using pinned `@hey-api/openapi-ts` and `@hey-api/client-fetch`, and fail CI when the committed client no longer matches that document. The daemon must emit the document from its own route definitions; the emitter is chosen alongside the web framework at plan time (D48). Use the browser WebSocket API separately for terminal bytes and live events.

**Rationale:** Generation avoids duplicate handwritten contracts without adding a query cache, runtime validation framework, or larger SDK stack. The generator's input has to be the daemon's own schema — a handwritten OpenAPI file would reintroduce exactly the duplicate contract this decision exists to remove, so an emitter is a requirement on the framework choice, not an optional extra.

**Contract:** [SPEC local single-user boundary](SPEC.md#43-local-single-user-boundary).

### D60 — Testing uses one focused toolset per layer

**Decision:** Use `cargo test` for every Rust crate — daemon, CLI, MCP adapter, and Tauri shell — with the acceptance chain as its own test target in the same workspace (D75); Vitest, Vue Test Utils 2, and jsdom for Vue/TypeScript; and WebdriverIO with `@wdio/tauri-service` for packaged desktop flows.

**Rationale:** Each layer needs an appropriate test surface, but overlapping runners and browser frameworks would add maintenance without useful coverage.

**Contract:** [SPEC verification](SPEC.md#17-verification).

### D61 — Static checks use one focused tool per concern

**Decision:** Use standard rustfmt and Clippy with `-D warnings` for every Rust crate, and Prettier plus correctness-only ESLint and `vue-tsc` for the frontend. Run them through ordinary project scripts and CI without a commit-hook framework.

**Rationale:** One tool per concern avoids conflicting formatters, linters, type checkers, and hook machinery.

**Contract:** [SPEC verification](SPEC.md#17-verification).

### D62 — The frontend uses Vue's built-in state and composition model

**Decision:** Use component-local state, Composition API, and small ordinary composables fed by REST snapshots and WebSocket events. MVP has no URL routing, client store, query cache, or second persistent frontend state layer.

**Rationale:** One persistent fixed-surface desktop should not duplicate daemon authority or adopt navigation and cache machinery before concrete pressure exists.

**Contract:** [SPEC local single-user boundary](SPEC.md#43-local-single-user-boundary) and [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle).

## Real-time market awareness

### D63 — One stdio MCP adapter delivers the entire MCP surface

**Decision:** Keep the latest execution-relevant public quote observations in an installation-wide in-memory module inside `marketrigd`, fed by NautilusTrader. Bundle one thin Rust stdio MCP adapter used by both Codex and Claude Code; it delivers the whole MCP surface defined per D4 — the awareness resources and the typed order tools — calling the authenticated daemon on every read and every tool call. The adapter owns no market cache and no local authority: reads return daemon-owned truth, and order tools are validated and executed inside the daemon. It is required from the first trading milestone. Correctness never depends on resource subscriptions or automatic context refresh (per D4, neither pinned runtime issues them).

**Rationale:** Volatile prices cannot be made authoritative inside immutable model context; a stable external reference lets either agent reread current daemon-owned truth. One common adapter preserves locality, prevents separate runtime-specific market views, and — now that D4 routes money actions through MCP — keeps the action path behind the same authenticated daemon boundary as every other client.

**Contract:** [SPEC agent surface](SPEC.md#13-agent-surface) and [SPEC MCP trading plane](SPEC.md#131-mcp-trading-plane).

## Paper trading foundation

### D64 — Paper trading is one in-process NautilusTrader node per trading desk

**Decision:** `marketrigd` hosts one `LiveNode` per trading desk, in the daemon process, each on its own thread with its own current-thread Tokio runtime, its own data client or clients, its own sandbox execution client and paper book, and its own cache, account, and matching engines. A node is built and run on one thread and never moved between threads. Adding a desk adds a node; a node failure rebuilds that desk's node from committed state and leaves the other desks' nodes untouched.

Three requirements bind the trading SPEC and are not silently dropped into it:

- a **position cycle** is the realized-P&L unit, and a fill that carries a netting position through zero closes one cycle and opens the next in one unit (D74);
- **book restoration** across a daemon restart works from Nautilus serialization snapshots of account state, open positions, and open orders, re-placing resting limit orders under their original client order IDs;
- every mutating trading command is an **idempotent, attributed `trading_actions` record**, desk-resolved, with stable JSON output.

Whether MarketRig holds its own `Position` objects outside the node is settled in SPEC rather than assumed here: the sandbox client is constructed with a *mutable* cache handle, and the spike read a closed position with its realized P&L back out of the node's own cache, so no platform constraint forces an external ledger.

**Rationale:** The 2026-09-01 execution spike demonstrated the topology rather than arguing it. Two complete `LiveNode`s on two OS threads in one process, trading the *same* instrument on the *same* venue, each filled independently with its own cache, account, matching engine, and venue-order-id sequence, because every Nautilus global is a `thread_local!` rather than a process global; two sandbox clients in one node likewise gave two independent books. Fifteen consecutive runs exited 0 in 3.5–3.6 s with an identical fill price and no hang. The routing rule that limits a venue to one execution client per node is answered by one node per desk rather than by venue aliasing.

Three things stay unproven and are open verification, not blockers: state reload was disabled throughout the spike, and it is what book restoration depends on; real market hours were never exercised, since the data spike ran on a US market holiday against a static last close; and isolation is proven at two books, not at N desks × M instruments, with per-node cost measured only as a flat 17–19 MB single-node soak. Each becomes a required check in the trading SPEC before the milestone closes.

**Contract:** [SPEC trading authority and node topology](SPEC.md#121-trading-authority-and-node-topology) and [SPEC ledger and provenance](SPEC.md#124-ledger-and-provenance).

## Memory foundation

### D65 — Memory is one supervised `hindsight-api-slim` child with a MarketRig-named pg0 instance, derived desk banks, and one hosted endpoint

**Decision:** MarketRig ships `hindsight-api-slim[embedded-db]`, pinned exactly and verified to install on both MVP platforms, inside the bundled interpreter environment (D47), and `marketrigd` locates and runs it as one supervised loopback child process authenticated with a per-start bearer credential, with MCP and telemetry disabled and one automatic restart. Hindsight manages pg0 instance `marketrig` on an auto-assigned port under pg0's own home directory. Each desk's bank is `desk-<hex>` derived from its UUID and provisioned lazily; agents never name banks. One OpenAI-compatible base URL and key serve both the LLM and embeddings, reranking uses Hindsight's rerank-free `rrf` ordering, and the embedding model locks after first initialization. `marketrig memory status|retain|recall|reflect` are synchronous desk-scoped pass-throughs with trigger attribution metadata and no MarketRig-side content store.

**Rationale:** The `hindsight-api` meta-package pulls local ML runtimes the hosted-model decision makes useless, Hindsight is configured only through environment variables and can fail independently of trading, and its `0.9.2` pg0 wrapper offers instance naming but no data-directory setting. A child process, derived bank IDs, and one endpoint deliver the explicit-degradation and isolation promises with the least new machinery. Hindsight being a Python package is the only reason the installer carries an interpreter at all (D47), and this supervised child is the one place that language boundary lives.

**Contract:** [SPEC memory and skills](SPEC.md#16-memory-and-skills).

## Desktop foundation

### D66 — The desktop attaches through an HTTP-free Tauri shell, first-frame WebSocket authentication, and one terminal socket per desk

**Decision:** The Tauri shell reads and watches `runtime/endpoint.json`, spawns `marketrigd` when no usable daemon exists, owns window/tray/single-instance lifecycle, and exposes only `read_endpoint`, `start_daemon`, `exit_app`, and (per D68) `set_locale`; it never performs an HTTP request. The webview verifies the daemon through authenticated health with the UUID match, holds the credential in memory only, and calls the loopback API directly. WebSockets validate a fixed exact-origin allowlist at the handshake and authenticate with a first-frame bearer credential. `WS /desks/{desk_id}/terminal` is the desk's attachment: the newest generation wins, each live terminal replays a 1 MiB ring, input and resize come only from the current generation, and terminal start/exit are reported as facts on that same socket, so desk attachment needs no channel of its own. The frontend keeps one warm ghostty-web presentation and socket per listed desk. Close hides; Quit is `POST /quit` followed by a bounded wait; a second launch focuses the existing window.

**Rationale:** The browser WebSocket API cannot send headers and D44 forbids credentials in URLs, so a first authentication frame is the only compliant path. Attaching to the desk rather than the process lets a hidden desktop notice trigger-driven activation through the socket it already holds, and keeping the shell HTTP-free avoids a second verifier and the proxy D44 rules out.

**Contract:** [SPEC desktop lifecycle](SPEC.md#14-desktop-and-application-lifecycle) and [SPEC local single-user boundary](SPEC.md#43-local-single-user-boundary).

## Acceptance

### D67 — Acceptance drives public surfaces only and verifies agent-owned steps by their side effects

**Decision:** MarketRig is accepted by one ordered scenario chain in its own acceptance harness (D75) that drives the real `marketrigd`, the real NautilusTrader sandbox on live public market data, real Hindsight behind a real hosted endpoint, and the real Codex CLI or Claude Code through public surfaces only — `marketrig --json`, the loopback API, the desk's MCP surface, the per-desk terminal socket, workspace files, and read-only SQLite. There is no test-only product surface. In the attended mode every native session start is operator-attended through a terminal relay so runtime dialogs are answered by a person. Agent-owned steps are instructed by a user-owned `AGENTS.md` addendum and verified by side effects — a tagged `INTERACTIVE` memory on the trading desk and none on the other, a skill file naming the cycle, a later session's file naming memory IDs — with bounded attempts on fresh trade cycles; mechanical assertions get none. The chain includes one clean restart inside a trigger's lead window and one hard kill with a live session, runs in a relocated root with run-stamped desk names, and must pass in every platform-and-runtime cell with an evidence bundle each.

**Rationale:** The claim acceptance makes is that the whole loop exists across real components, and each feature's own checks already own mechanics coverage; the acceptance layer therefore asserts only the roadmap's evidence lines against the real system. Driving public surfaces only is what makes the chain evidence rather than instrumentation — a test-only entry point would prove a path no agent can take. Operator attendance is what the runtimes' own first-run prompts require, and side-effect verifiers keep MarketRig out of deciding what the agent should learn while still making the learning steps executable.

**Contract:** [SPEC verification](SPEC.md#17-verification).

### D75 — Acceptance is two chains from one harness: a deterministic gate on stand-ins and an attended experiment on real components, both in a relocated root

**Decision:** The acceptance harness runs the same ordered scenario chain in two modes. The **gate** is unattended and deterministic: the daemon, the CLI, the database, the NautilusTrader sandbox, and every public surface are real, and the sandbox is driven by the milestone's deterministic stand-in quote source where one exists (D76) and by live public data where it does not, while the runtime CLIs and the memory child are the harness's stand-ins — a stub Codex app-server with a scripted agent that performs the addendum's steps on the prompts it receives, the stub Claude probes, and a fake Hindsight child — so the chain proves MarketRig's mechanics with no model, no key, no first-run dialog, and no operator. The **experiment** is the operator-attended run of the same chain on the real Codex CLI or Claude Code and real Hindsight behind a real hosted endpoint, once per release candidate per cell, whose bundle is archived as the evidence that a real agent closes the loop. Both modes are Cargo test targets in MarketRig's own workspace (D53, D60); their exact invocations are set at plan time.

In the experiment, mechanical scenarios fail the cell; the assertions that wait on the agent to act — the retained lesson and the improved skill, the later session's file that reuses them, and the observed `APPROVAL_REQUESTED` attention — end after their attempts as **inconclusive**, with their evidence, never as a product defect, and the operator decides whether to rerun. Both modes relocate the whole root through the daemon's test seam into the run's evidence directory, so the daemon's JSON-Lines log, the desk workspaces, and every terminal socket's close code land in the bundle by construction and no run touches the per-user root; the packaged desktop smoke is the one leg that does. A resumable pointer is part of the evidence: the chain sends one user message so the runtime persists the session, and a later scenario asserts resume-first strictly.

The chain grows scenario by scenario as milestones land, so no chain waits on a final harness, and the acceptance feature publishes an explicit **scenario-to-check mapping**, so every scenario the chain claims is answered by a named check and none is dropped silently.

**Rationale:** One real-system chain is a bad gate, because it cannot tell its two failure classes apart: a deterministic harness or product defect and a third-party fact — a runtime that persists a thread only once it has had a turn, a memory recall whose result set grows — fail it identically, and each diagnosis costs a rerun of the whole chain. Splitting the modes makes the failing leg the first half of the diagnosis, and bundling the daemon's log by construction rather than by remembering to collect it makes the second half readable. Mature agent CLIs make the same split: Codex gates on a mock server that speaks its real model protocol and a real app-server subprocess in a throwaway home, and keeps every test that needs a key, a network, or a person ignored with a reason; Warp gates on trait-level fakes and runs its model evaluations in a separate pipeline with a judge. The MVP's claim is that the loop exists, not that the learning is good, so an agent that did not retain a lesson within the attempt window is an inconclusive experiment, not a failed product. Relocating the root is the daemon's own test seam, so acceptance never has to run on prepared real state. Harness-relevant probe facts, verified 2026-09-01 on the pinned CLIs: headless `codex exec` requires standard input closed and `--skip-git-repo-check`, and trust dialogs do not block `exec`; unattended Claude Code needs `--strict-mcp-config` plus `--allowedTools` naming the MCP resource-read tools and each server tool it must call.

`ponytail:` the gate's Claude cell waits for a stub that holds a Channel session; until then the gate runs the Codex stand-in only and the runtime-switch scenario takes its skip-with-evidence path. The equity gate drives a deterministic stand-in quote source (D76), so it does not reach the public internet; the non-hermetic reach arrives with the Kraken crypto environment (D74), and a stand-in venue speaking the adapter's protocol is the upgrade path if runner network access proves unreliable.

**Contract:** [SPEC verification](SPEC.md#17-verification).

## Localization

### D68 — The desktop is localized in English and Simplified Chinese; everything the agent consumes stays English

**Decision:** MVP ships exactly `en` and `zh-Hans`. One installation `locale` setting, detected by the desktop from the system language at first launch and changeable in settings, drives the desktop UI, tray menu, and notifications through exactly pinned vue-i18n from its newest stable release line, used with the Composition API only (`legacy: false`), and one added Tauri shell command, `set_locale`, that rebuilds the tray labels. The daemon stores the setting and reads it nowhere: the `marketrig` CLI, JSON contract, error envelope, codes, daemon prompts, seeded `AGENTS.md` and skills, logs, and operational evidence have one English source and are byte-identical under both locales. Financial values display as the daemon's canonical decimal text, never reformatted; the CLI writes UTF-8 unconditionally so Chinese data survives Windows pipes; CI enforces catalog parity and forbids bare strings in Vue templates.

**Rationale:** The human reads the desktop; the agent reads the CLI, prompts, and desk files. Localizing the first serves the initial Chinese-speaking users, while localizing the second would fork a contract trigger code, skills, evidence bundles, and models depend on — and the initial user is a developer who already reads English agent instructions, with an agent that replies in whatever language the user writes. vue-i18n is the maintained standard for Vue 3 and covers typing, plurals, and dates without in-house code.

**Contract:** [SPEC localization](SPEC.md#45-localization).

## Session controls and runtime-native events

### D69 — Session controls are REST-only, an interrupt is structured or it is the user's own keyboard, and each runtime is observed only where it broadcasts

**Decision:** Interrupt, Exit, and Switch are REST routes reachable from the desktop and the acceptance harness and never from the `marketrig` CLI, because the CLI is the agent's interface and the agent never exits itself. Each answers exactly what the runtime answered: no confirmation semantics, no daemon-side confirmation prompt, and no retry of an uncertain action. Interrupt goes only through a runtime's supported programmatic interface — for Codex the app-server's `turn/interrupt` on the live turn read from `thread/turns/list` at the moment it is asked — and a runtime with no structured interrupt answers 409 `INTERRUPT_UNSUPPORTED` before reaching its control plane. Claude Code is that runtime, so its Interrupt is the Escape key the user's own terminal attachment sends, and the daemon appends no `INTERRUPT` event for it. Every `agent_processes` close appends one `SESSION_EXITED` row in the same unit, carrying an `exit_reason` that says who ended the process: `EXITED`, `INTERRUPTED` (MarketRig's Exit or the exit half of a Switch), `QUIT`, `CONTROL_PLANE_LOST`, `DAEMON_LOST`. Switch decides every rejection before it stops anything, then moves `desks.selected_runtime` with `RUNTIME_SWITCHED` in one unit; both runtimes keep their pointers and there is no rollback after activation begins. MarketRig is an observer on the Codex app-server: it takes the broadcast `thread/started` and `thread/status/changed` as its only view of a thread and never expects `turn/started`, `turn/completed`, or the `item/*/requestApproval` and `item/tool/requestUserInput` server requests, which are routed to the desk's own TUI — so it can neither see nor answer a runtime-native approval, and an unanswered one can never block a MarketRig delivery. Claude Code, which has no observer connection, is reported instead by three read-only hooks — `SessionStart`, `Notification`, and `Stop` — declared in a per-launch `--settings` file written 0600 beside that launch's `--mcp-config` file and deleted with it, each running `marketrig --desk <desk-id> session hook`. That command forwards the hook object from standard input unchanged and exits `0` on every outcome, including no daemon and any rejection, and a launch with no `marketrig` entry point simply carries no settings file. Hooks are MarketRig's only Claude event source: no transcript tailing, no terminal parsing, and no hook that writes back.

**Rationale:** The delivery guardrail forbids daemon keystroke emulation, and a silent keyboard fallback would make one runtime's Interrupt mean something different from the other's; saying "unsupported" keeps the difference visible and the daemon out of the terminal. One exit-reason vocabulary lets the desktop, the terminal socket's `exited` frame, and crash recovery read the same fact, and deciding a Switch's rejections first means a rejected switch never costs a live session. Reading the live turn on demand is one call and is always right, where tracking a turn id MarketRig is never told about would have been a guess. Exiting `0` unconditionally from the hook command is the whole safety property: a non-zero hook is a message to the model or a blocked event, and MarketRig's evidence must never become the agent's problem. Verified by running an app-server with two connections on Codex CLI 0.150.1, and by driving Claude Code 2.1.250 headlessly and over a pty on macOS.

`ponytail:` Claude Code's interactive mode exposes no structured interrupt — the Channel is one-way and hooks are read-only — so Interrupt raises `INTERRUPT_UNSUPPORTED` and the `Stop` hook is the only evidence a turn ended, when it fires. Replace the keyboard path with the real call the day Claude Code exposes one.

**Contract:** [SPEC §6.2](SPEC.md#62-lifecycle-actions) and [§13.2](SPEC.md#132-cli-continuity-plane).

## Approval workflow

### D70 — Approvals are three installation policy columns, the records that already exist, and a boundary the agent cannot cross

**Decision:** `installation_settings` gains `trigger_code_policy` (default `REQUIRE_APPROVAL`), `paper_order_policy` (default `ALWAYS_ALLOW`), and `delivery_mode` (`QUEUE`, with a `CHECK` that admits no other value), each read by plain SQL inside the unit that needs it, with no cache and no per-desk override. `GET`/`PUT /settings/policies` is their one resource: it reports `steer_available: false`, refuses `STEER` as `STEER_DISABLED`, and appends one installation-wide `POLICY_CHANGED` per field that actually changes. There is no approvals table — an approval **is** the `trigger_code_snapshots` row or the `trading_actions` row. A snapshot's `approval` is `ALWAYS_ALLOW | PENDING | APPROVED | DENIED` with `decided_at_ns` null exactly while pending, and `triggers.next_occurrence_ns` is non-null only when the trigger is enabled, undeleted, and its snapshot is null, `ALWAYS_ALLOW`, or `APPROVED`, so the scheduler's one query needs no join, an unapproved trigger is never due, enable and disable cannot resurrect it, and an approved elapsed one-off stays null. An order under **Require approval** commits with `state = PENDING_APPROVAL`, enqueues nothing, answers a null order projection, and returns that same row for a repeated idempotency key; approval requires a `RUNNING` node hosting the desk's book and then re-enters the ordinary accept path exactly where acceptance left it, denial is terminal with `failure_code = DENIED`, and a `CANCEL` is never gated. Approvals and policies are reachable only through REST and the desktop: no `marketrig` command reads or writes a policy, lists approvals, or decides one, and the agent sees the state through the records it already reads. A decision queues no daemon prompt.

**Rationale:** Code runs as the user, so the user authorizes it, while orders default to **Always allow** because the experiment is meant to run unattended and the user opts in to supervising it; storing the reserved `STEER` mode makes it a visible refused setting rather than an absent one. Folding eligibility into `next_occurrence_ns` and holding a pending order in its own acceptance row means the whole lifecycle is the rows that already exist — recovery already ignores any state that is not `ACCEPTED` or `HANDED_OFF`, and a separate table would only duplicate them. An approval boundary the agent can cross is not a boundary, which is the entire reason MarketRig owns approvals rather than delegating to the runtime's own prompt; visibility is not a crossing, because the agent needs to know why its order has not filled. MarketRig does not narrate its own control surface back into a turn, so the realized-P&L evaluation stays the one thing it pushes.

**Contract:** [SPEC §4.4](SPEC.md#44-configuration-scopes), [§8.3](SPEC.md#83-trigger-lifecycle), [§11.2](SPEC.md#112-queueing-and-reserved-steering), and [§12.3](SPEC.md#123-trading-actions-and-approvals).

## Live events and delivery history

### D71 — The live-event socket is a tail of one table with a client-held cursor

**Decision:** `WS /events` streams every committed `operational_events` row and nothing else — no topics, no per-kind channels, no snapshot frames, no acknowledgment, and no cursor the daemon remembers for a client. The daemon keeps one `(occurred_at_ns, id)` cursor for the installation, advances it as it publishes, and wakes on the database thread's post-commit signal with a five-second recheck behind it. A client that must miss nothing sends the cursor of the last row it saw as `after` in its auth frame; the daemon subscribes before it reads and replays from the table in bounded pages up to the tail's position at subscription, so the boundary has no gap and no duplicate, and an `after` this history does not contain is ignored with the tail's own position reported back instead. Each connection has a bounded 1 000-event queue, and overflowing it closes that one connection with 4408. Alongside the socket, `operational_events` and `daemon_prompts` are listable newest-first with the same keyset cursors, and `marketrig desk events` and `marketrig prompt list|show` are their agent-facing form: a failed prompt names itself in the next activation prompt, the agent may read its text there on its own initiative, and the daemon still never redelivers it.

**Rationale:** Everything the desktop watches already lands in one append-only table inside the transaction that commits the change it evidences, so a tail of that table is the whole feature; a bus would add topics, fan-out policy, and a second ordering to keep consistent with the one SQLite already guarantees. Putting the cursor on the client makes a daemon restart ordinary — the daemon holds no per-client state to lose, and the client's own reconnect replays the new daemon's `RECOVERY` row, which is how it learns the restart happened at all. The bounded queue and 4408 mirror the terminal socket's rule for the same reason: a consumer that stops reading must cost itself its connection, never the producer.

**Contract:** [SPEC §11.1](SPEC.md#111-states-and-ordering), [§14](SPEC.md#14-desktop-and-application-lifecycle), and [§15](SPEC.md#15-persistence-crash-recovery-and-history).

## Desktop design system and controls

### D72 — One token set, colour reserved for state, and a window that holds a cursor rather than a copy of the daemon

**Decision:** The frontend's whole visual system is one `@theme` block plus one `prefers-color-scheme: dark` override in `src/style.css`: OKLCH colour roles, a 4 px spacing unit, three control radii, an 11–18 px type scale, and two font stacks — `--font-ui` for localized prose and `--font-terminal` for the terminal screen and every machine token. Components consume utilities over those tokens and carry no stylesheet and no literal colour; there is no theme switcher and no `data-theme` attribute. Colour is reserved for five state roles — live, pending, attention, failure, idle — plus one accent that marks selection and focus, reaching the chrome through a 2 px status gutter that never animates, and the terminal well stays dark in both schemes. Reka UI is used for `Tabs`, `Select`, and `AlertDialog` and nothing else. The state model is that the events socket says something changed and the client refetches the resource that changed: `useEvents` owns the only `WS /events` connection and the only client-side memory of the tail — its cursor — and every handler it exposes does nothing but refetch. No mutation updates a list optimistically and no control mirrors a daemon outcome into local state; the two deliberately frontend-local facts are the attention dot, which an `ATTENTION` row sets and the operator's own keyboard clears, and the right panel's collapsed state. The tray receives one number through `set_tray_pending` and nothing else.

**Rationale:** A surface an operator watches for hours has to be quiet: if colour only ever means state, one hue arriving in the chrome is information rather than decoration. One token file is the cheapest way to keep a distinct visual language without a component suite, and following the operating system's scheme removes a setting, a persisted preference, and a class-toggle mechanism no milestone has evidence it needs; splitting the font stacks by provenance makes the localization guardrail visible, because prose is translated and machine tokens never are. A local single-user window watching a local daemon has no reason to model the daemon — a refetch over loopback costs less than the bug where a cached list and the database disagree — and the rule makes every failure honest, because approving an order can be refused and a window that had already moved the row would be lying. Attention has to live in the window or not exist, since `ATTENTION` is a one-shot event and not a status.

**Contract:** [SPEC §14](SPEC.md#14-desktop-and-application-lifecycle).

### D73 — The next daemon start reaps the long-lived children a crashed daemon left behind

**Decision:** The Codex app-server and the Hindsight process are recorded in `runtime/children.json` when the daemon launches them and removed when it stops them; crash recovery's first step terminates any recorded child of a prior daemon whose pid is alive and whose command line still carries the recorded arguments (a shim may replace the executable path), drops every record either way, and reports each outcome in the `RECOVERY` event. Windows discards records only, since its Job Object already ends the children. Nothing is recorded for trigger scripts.

**Rationale:** Containment's own-session design is what lets a live daemon kill a child tree and what lets a dead daemon's child survive on macOS; the platform offers no parent-death signal and the app-server does not watch stdin. Recovery is the existing pre-service place where a new daemon resolves its predecessor's leftovers, and an identity-checked record is the smallest mechanism that cannot kill a recycled pid.

**Contract:** [SPEC §15](SPEC.md#15-persistence-crash-recovery-and-history).

## Full crypto paper trading

### D74 — Paper trading is full Kraken crypto: spot and futures, long and short, with the sandbox's physics and nothing more

**Decision:** MVP paper trading covers everything Kraken's public feeds carry — spot pairs on margin (long and short, leverage up to the venue's advertised tier per pair) and futures (inverse and linear perpetuals and dated contracts) — through one NautilusTrader margin netting sandbox account per desk that spans both product types on the single `KRAKEN` venue, fed by one spot and one futures data client per node, both keyless. The account takes every single-order type the sandbox matches — market, limit, stop-market, stop-limit, market-if-touched, limit-if-touched, trailing-stop-market, trailing-stop-limit — with GTC, IOC, FOK, and GTD, post-only, and reduce-only. A fill that carries a netting position through zero closes one cycle and opens the next in one unit, as NautilusTrader's own execution engine does. MarketRig inherits the sandbox's physics exactly and states the gap instead of approximating it: no funding payments, margin interest or rollover fees, liquidation, or auto-deleveraging are simulated, funding rates and mark and index prices are exposed as observations only, and the seeded `AGENTS.md` names those limits so the agent trades knowing them. Inverse contracts settle in their base crypto, so their collateral is the desk's own spot holdings, as on the real venue. Order lists (brackets, OCO), execution algorithms, asset classes beyond crypto and equities, and venues beyond Kraken stay post-MVP.

**Rationale:** A harness that allows only long spot removes half of what a crypto trader decides about. NautilusTrader's sandbox already matches margin and derivative orders against live quotes, and Kraken's futures feed loads its 294 instruments without a key (verified 2026-08-29), so the full scope costs configuration and ledger work rather than new authority. The physics the sandbox does not model are stated rather than approximated because MarketRig never computes a trading fact itself (D38): a paper desk that is never liquidated is honest evidence about the loop, not a claim about the market. Asset classes beyond crypto and equities need paid or gateway-bound feeds and are a different product question, kept on the roadmap's deferred list.

`ponytail:` no funding, margin interest, or liquidation on paper; revisit when NautilusTrader's sandbox models them, or when a desk's evaluation evidence shows the gap misleads the agent.

**Contract:** [PRD MVP scope](PRD.md#9-mvp-scope), [SPEC trading boundary](SPEC.md#12-trading-and-public-data-boundary), and [SPEC trading actions and approvals](SPEC.md#123-trading-actions-and-approvals).

## Equity paper trading

### D76 — Stocks first: the first trading milestone is equity paper trading

**Decision:** The first trading milestone is **equity paper trading across three markets — US, Hong Kong, and China A-share — on one keyless Yahoo Finance `DataClient`** (D39) against the NautilusTrader sandbox, with one multi-currency paper account per desk (USD, HKD, and CNY balances; realized P&L in each instrument's own currency). Full crypto is not dropped or narrowed: the Kraken crypto environment (per D74) lands in a later milestone. The client stays thin: transport, retry, and reconnect ride `nautilus-network`'s shipped machinery, and the community `yahoo_finance_api` crate — which owns the crumb/cookie and user-agent quirks, at bus factor one — is the candidate endpoint layer, pinned or replaced at plan time; that crate and `nautilus-network` sit on the same reqwest line (0.13.4, verified 2026-09-01), so one HTTP stack serves both. The alternatives were evaluated and rejected on 2026-09-01: Databento on cost and signup terms; Alpaca — the only free source with a real bid/ask, terms verified clean — on market coverage, since it serves US listings only; the remaining keyed vendors on licence clauses (display-only use, or no durable storage). Four mitigations for the unofficial-API risk bind before implementation:

1. The equity feature SPEC is written **before** any implementation, and it settles what an undocumented endpoint and three exchanges leave open: per-exchange market-hours calendars (including the Hong Kong and mainland lunch breaks) and staleness semantics for a feed whose non-US quotes may be delayed, with the delay characteristics verified per exchange at spec time rather than assumed; the faked book — Yahoo's chart endpoint carries no bid or ask, so both sides and both sizes are synthesized in every market and everything downstream that would reason about spread or depth is told so; the instrument metadata MarketRig must encode correctly (the Hong Kong price-band tick ladder and per-stock board lots, the A-share 100-share lot, each market's currency); the equity fee model per market; and the retry policy for HTTP 429, which the data spike observed probabilistically on a majority of cold requests from one IP, with eight attempts 400 ms apart sufficient for 15 of 15 polls.
2. Sandbox physics gaps are **stated, not approximated**, per D74's pattern: no T+1 settlement, daily price limits, trading halts, or opening and closing auctions are simulated, and the seeded desk constitution names those limits so the agent trades knowing them.
3. Per D75's split, the **gate drives a deterministic stand-in quote source** and Yahoo rides only the live and attended legs, so "is it my harness or is it Yahoo?" is answered by which leg failed rather than by re-running until it passes.
4. The data spike's landmines become **required checks** in that SPEC: the publish path is a thread-local that panics when obtained off the node thread, so the sender is taken on the node thread and a clone moved into any polling task; and the sandbox silently *drops* a quote whose price or size precision does not match the cached instrument, so quote precision is derived from the instrument and never from a formatting choice, with the precision mode pinned per D39.

**Rationale:** The user's call, taken over a crypto-first recommendation, and — on the feed — taken knowingly over a real-book alternative: US, Hong Kong, and A-share are the markets this desk is meant to trade, Yahoo is the only keyless source covering all three, and market coverage outweighs the honest bid/ask a US-only keyed feed would offer. The accepted cost is a uniformly synthesized zero-spread book: round trips cost only fees, so the realized-P&L reward overstates outcomes by the unpaid spread, stated to the agent rather than hidden. The one capability all three markets depend on — an out-of-tree data client for a feed no venue adapter ships — is exactly what the 2026-09-01 data spike proved on the native crates (D39). Kraken's advantage is that its adapter is already a published crate on the same pinned version line, which is a sequencing argument rather than a product one, so it takes the later milestone. The risk that remains is the source, not the platform: the endpoint is undocumented, unsupported, and rate-limits unpredictably — which is what the four mitigations are for.

`ponytail:` the synthesized zero-spread book is a deliberate reward-signal ceiling; the upgrade path is a keyed real-book feed per market (a broker OpenAPI covering these exchanges, or Alpaca for the US leg alone) behind the same `DataClient` seam, if learning evidence ever needs honest spreads.

**Contract:** [SPEC trading actions and approvals](SPEC.md#123-trading-actions-and-approvals) and [SPEC open verification](SPEC.md#125-open-verification).
