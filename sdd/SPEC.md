# MarketRig Product Specification

This document is the canonical mechanical and architectural contract for MarketRig. Feature specifications refine deferred mechanics but never contradict these invariants without a recorded decision.

Section numbers are link anchors cited across the repository: add a subsection (`4.6`) rather than renumber an existing one.

Where a decision or a recorded spike settles a mechanism, this document states it. Everywhere else it says **deferred**, and §18 holds the item until the feature specification that owns it is written; a resolved mechanism is merged back here.

## 1. Scope

*Decision basis: per D1, D2, D3, D5, D6, D9, D10, D11, D14, D22, D38, D39, D43, D63, and D76.*

MarketRig is a local persistent harness for external coding agents operating and improving in a paper-trading environment. It owns durable desks, time, runtime lifecycle, public-data access, trigger execution, approvals, trading history, and authoritative access to trading reality. Codex or Claude Code owns orientation, decisions, evaluation, and learning.

MVP platforms are Windows and macOS on Apple Silicon. MVP agent runtimes are Codex CLI and Claude Code. MVP trading is NautilusTrader sandbox and paper execution only, equities first — US, Hong Kong, and China A-share — and Kraken crypto after. `marketrigd`, `marketrig`, and `marketrig-mcp` are Rust binaries on rustc 1.98 or newer with edition 2024. The one interpreter MarketRig ships exists solely for the supervised Hindsight child.

## 2. Canonical terminology

*Decision basis: per D4, D8, D15, D16, D17, D19, D23, and D34.*

### MarketRig

The complete local product: desktop app, tray lifecycle, `marketrigd`, `marketrig`, `marketrig-mcp`, managed children, workspaces, and durable state.

### `marketrigd`

The per-user local daemon and sole writer of authoritative MarketRig state. It owns desks, agent processes and terminals, triggers, approvals, delivery history, the per-desk trading nodes, and the trading action boundary.

### `marketrig`

The shell-native CLI and the agent's continuity-plane client of `marketrigd`. It holds no independent business state or trading logic.

### `marketrig-mcp`

The bundled stdio MCP adapter that carries the agent's whole market plane — the desk's awareness resources and its typed order tools. It calls `marketrigd` on every read and every tool call and holds no market cache, provider connection, durable state, or action authority.

### Agent surface

The two planes together: the MCP trading plane and the `marketrig` continuity plane. No capability appears on both.

### Desk

One durable autonomous trader identity and the isolation boundary for mutable state. A desk survives agent-session exit, runtime replacement, desktop closure, and daemon restart.

### Agent runtime

Codex CLI or Claude Code, the external software that supplies cognition.

### Agent session

A runtime-native logical resumable Codex or Claude Code session. **Session** always means this; it never means a desktop attachment, terminal, process, trigger execution, or MarketRig Run.

### Agent process

The live runtime CLI operating-system process associated with an active agent session.

### Terminal

The daemon-owned PTY on macOS or ConPTY on Windows hosting the real interactive agent runtime.

### Activation

The operation that makes a desk's agent available by resuming its last native session when possible and starting a new native session otherwise.

### Activation prompt

Ordinary user-level input sent when a new agent session needs orientation or when pending notices must be delivered during activation. It is never a system prompt and never reaches the process command line.

### Trigger

A durable desk-bound `marketrigd` job that fires from a schedule or an event, optionally runs an immutable code snapshot, and produces a result for the desk agent.

### Event occurrence

A durably accepted desk-scoped EVENT ingress item with a stable identity inside its ingress scope. It is input to matching triggers, not a separate user-managed job.

### Trigger result

The raw output of a trigger firing, including execution diagnostics when applicable. MarketRig preserves it without interpreting its market meaning.

### Daemon prompt delivery

The at-most-once attempt to submit a persisted trigger result or evaluation signal to the desk agent as ordinary input through a supported structured runtime interface.

### Public read

Market information that is not private to a desk and may therefore use installation-wide connections and caches.

### Market observation

One atomically accepted, time-stamped view of mutable public market state. A value already present in agent context remains that historical observation; it is never authoritative current state.

### Market resource

One concretely enumerated desk-scoped MCP resource whose read returns the observation `marketrigd` holds at read time. Resources are enumerated in `resources/list`, never expressed only as URI templates.

### Order tool

One of the small set of typed MCP tools through which the agent submits or cancels a paper order. Its arguments are validated by the daemon, not by the client.

### Paper book

The NautilusTrader-owned current sandbox execution and accounting state for one desk: orders, fills, positions, balances, margins, and related execution state.

### Trading history

The immutable sandbox-produced orders, individual fills, position cycles, fees, realized P&L, and restoration state persisted by MarketRig in SQLite for one desk.

### Position cycle

One open-to-flat lifetime of a desk's holding in one instrument, produced by NautilusTrader position arithmetic. It is the unit of realized P&L and of evaluation.

### Evaluation signal

A daemon-originated ordinary prompt caused by a realized-P&L event. It points the agent at authoritative trading evidence and prescribes no evidence, conclusion, or learning action.

### Hindsight memory

Agent-owned subjective and experiential memory. It is not authoritative trading reality.

### Skill

Durable procedural guidance stored in the desk workspace and shared across supported runtimes within that desk.

### Run

Not a canonical MarketRig entity. UI prose may informally say "pause the run," but no Run identity, record, or state machine exists.

## 3. Top-level architecture

*Decision basis: per D4, D16, D23, D24, D30, D31, D39, D42, D43, D44, D45, D46, D48, D53, D54, D55, D63, D64, and D77.*

```text
Tauri 2 / Vue 3          marketrig CLI          Codex CLI / Claude Code
      |                        |                          |
      |                        |                          | stdio MCP
      |                        |                          v
      |                        |                    marketrig-mcp
      +--- authenticated loopback REST + WebSocket --------+
                               |
                          marketrigd
                          ├── runtime adapters
                          │   ├── Codex app-server -> remote native TUI -> PTY/ConPTY
                          │   └── Claude Channel router -> native Claude TUI -> PTY/ConPTY
                          ├── trigger scheduler and runner -> trigger code
                          ├── one NautilusTrader LiveNode per trading desk
                          │   -> MarketRig data clients + sandbox paper execution
                          ├── SQLite -> MarketRig state + durable trading history
                          └── supervised Hindsight child -> one bank per desk
```

Architectural invariants:

- `marketrigd` is the sole authoritative coordinator and the sole writer of durable MarketRig state.
- The daemon is a library crate behind a thin standalone binary. The Tauri shell spawns and adopts that binary; the daemon is never hosted inside the GUI process, because continuity must not be bound to a GUI process lifetime and a webview process is the wrong host for a scheduler and a trading node (per D43).
- NautilusTrader produces execution and accounting facts; MarketRig's SQLite is the durable authority for their immutable history.
- Desktop and CLI are clients. Closing the desktop transfers no ownership.
- Runtime adapters preserve native runtime behavior rather than defining a universal agent protocol.
- Agent-facing trading semantics never leak NautilusTrader APIs; agent-facing memory semantics are retain, recall, and reflect only, and Hindsight topology, credentials, endpoints, and bank identifiers stay private to `marketrigd`.
- Current market observations live only in `marketrigd`; `marketrig-mcp` dereferences them and never becomes a second truth owner.
- Market awareness is explicit re-reads. Correctness never depends on resource subscriptions, server push, or automatic context refresh (per D4, D63).
- Mutable workspace, session, trigger, approval, and trading state stays desk-bound.
- Shared public reads are read-only with respect to desk identity.

One Cargo workspace builds and releases `marketrigd`, `marketrig`, and `marketrig-mcp` together: one version, one lockfile, one boundary model, shared internal crates rather than published libraries, and no versioning seam between them (per D53).

| Path | Contents |
| --- | --- |
| `/` (root) | the Vue 3 / TypeScript / Vite frontend with its `package.json`, `index.html`, and `src/`; the Cargo workspace manifest and `Cargo.lock` |
| `crates/` | `marketrigd`, `marketrig`, `marketrig-mcp`, the internal acceptance-harness crate (§17), and shared internal crates |
| `src-tauri/` | the Tauri 2 Rust shell, a member of the same Cargo workspace |

pnpm and Cargo are used directly. There is no monorepo framework, general task runner, or commit-hook framework (per D54, D61). Dependencies are pinned exactly; bumping a pin is a version change verified by that module's own checks.

## 4. Installation, platform, and security boundary

*Decision basis: per D3, D11, D13, D14, D18, D25, D42, D44, D47, D48, D49, D52, D58, D59, D66, D68, D70, D77, and D80.*

### 4.1 Installation

Each supported platform ships one per-user release unit containing:

- the Tauri 2 desktop and tray application;
- `marketrigd`, `marketrig`, and `marketrig-mcp` as native executables, with NautilusTrader linked into the daemon rather than installed beside it;
- one private portable CPython runtime and pinned wheel environment whose only purpose is the supervised Hindsight child (per D47, D65).

Nothing else in MarketRig depends on an interpreter, and no system interpreter or build toolchain is required at run time. The Tauri 2 bundler produces a per-user NSIS installer for Windows and a `.app` distributed in a DMG for Apple Silicon macOS. Node.js, pnpm, and Corepack are build-time only.

MarketRig source and releases are licensed `AGPL-3.0-only`. Every distributed binary release provides the matching corresponding source, build and install scripts, license texts, and third-party notices. The `nautilus-*` crates are `LGPL-3.0-only`; consuming them from an AGPL-3.0-only work is compatible, and MarketRig distributes its own corresponding source regardless (per D14, D39).

Codex CLI and Claude Code are external. First-launch onboarding must make at least one runtime usable; the other may be configured later. MarketRig neither installs nor updates them, never inspects or stores their credentials, and refuses a selected launch target that lacks a required capability.

Discovery and validation are capability-based rather than version-only:

- probe the exact executable MarketRig will execute and show its path and version;
- consider the persisted target first, then the refreshed `PATH`, then the official per-user CLI locations, then manual selection; never scan the disk;
- permit selection among genuine CLI candidates, but exclude any candidate resolving inside the ChatGPT/Codex desktop app bundle: it is listed with its exclusion reason, never probed, never launched (per D3);
- retain a stable launcher or symlink path rather than resolving it to a versioned binary;
- persist only the selected non-secret launch target; authentication and capability results are live observations, not durable truth;
- recheck on explicit Retry, when runtime settings open, and before that runtime's first lazy control-plane start; never poll and never interrupt a live session.

Readiness is the capability set the product depends on: the runtime's structured queueing path (per D36), its explicit session-identity and resume flags (per D28), its structured event source (per D32, D69), and MCP resource-read and tool support (per D4). Concretely (per D80), validation runs `<executable> --version` with a 10-second timeout and reads the first dotted version on standard output; the floors are Codex CLI `0.152.1` and Claude Code `2.1.258`, the lines the adapters were verified against. `--help` must then name `app-server` (Codex) or both `--dangerously-load-development-channels` and `--settings` (Claude); a Claude marker the line hides from `--help` is probed differentially — `<executable> <flag> --version` must not be refused as an unknown option while a bogus flag is. Success writes the `runtimes` row `AVAILABLE` with path, version, and `validated_at_ns` and appends `RUNTIME_DISCOVERED`; any failure writes `UNAVAILABLE` with `NOT_FOUND`, `VERSION_UNSUPPORTED`, `CAPABILITY_MISSING` (the code a path inside a `Codex.app` or `ChatGPT.app` bundle also earns, naming the bundle), or `PROBE_FAILED` and appends `RUNTIME_UNAVAILABLE`. An explicit executable is validated as given, never resolved through `PATH`, and a relative one is `VALIDATION`. Without one, macOS resolves the bare name through the login shell's `PATH` — `$SHELL -l -c 'printf %s "$PATH"'`, 10-second timeout, captured once per daemon start and kept only in memory, the daemon's own `PATH` when the shell fails — and Windows reads the user and machine `Path` values fresh from the registry, accepting `.exe` directly and `.cmd` or `.bat` through `%ComSpec% /d /c`.

On Windows, discovery refreshes the native user and machine `PATH` and invokes selected `.cmd` or `.bat` launchers through `ComSpec`; PowerShell aliases, functions, and profiles are outside the launch contract. On macOS, onboarding captures the login-shell environment once with a timeout, retains it only in daemon memory, and then launches the selected executable directly (per D25).

MVP has no application updater, arbitrary cross-version state-compatibility guarantee, downgrade contract, automatic desk sync, or manual desk export/import. A release may carry the forward-only SQLite migrations its supported upgrade path needs. Uninstallation preserves desk workspaces and durable state; erasure is a separate explicit destructive action (per D13).

### 4.2 Platform parity

Windows and Apple Silicon macOS expose the same desk, session, trigger, approval, data, and paper-trading capabilities. Operating-system-specific process, terminal, autostart, and packaging mechanics stay private implementation details.

### 4.3 Local single-user boundary

- `marketrigd` binds only to TCP loopback and exposes REST/JSON for queries and commands plus WebSocket for terminal bytes and live events, from one listener in one process. No process manager, no reverse proxy, no second transport (per D48).
- The API is served on one maintained Rust web framework over Tokio and hyper, listening on `127.0.0.1` on an operating-system-assigned port. That framework is `axum`, pinned exactly in the workspace manifest (per D77).
- That framework must emit an OpenAPI document from its own route definitions; a handwritten document is not acceptable input, because generation exists to remove a duplicated contract (per D59). The emitter is `utoipa` with its axum integration; its wiring lands with the milestone whose generated client needs it (§18).
- Exactly one daemon serves one data root, enforced by an exclusive advisory lock on a file under `runtime/` taken with the standard library before anything is published and held until the process exits. A second daemon on the same root exits nonzero naming `ALREADY_RUNNING` and writes nothing. The lock scope is the data root, not the machine, which is what lets the acceptance harness run concurrent daemons in scratch roots (per D75, D77).
- Each daemon start mints a new high-entropy bearer credential and publishes its selected port, credential, and per-start daemon UUID through OS-permission-protected per-user endpoint metadata at `runtime/endpoint.json` under the application-data root, written atomically through a temporary file and rename once the listener is live. The daemon lifetime lock lives beside it.
- REST requests require that credential. WebSocket connections validate an exact allowed origin at the handshake and authenticate with a first-frame bearer credential before any application message is accepted, because the browser WebSocket API cannot send headers and credentials never appear in URLs (per D44, D66).
- `runtime/endpoint.json` is a pointer, never proof of liveness. Clients verify the daemon through authenticated health and require its reported UUID to match the endpoint metadata; a connection failure, a `401`, or a UUID mismatch means no usable daemon. A stale file a dead daemon left behind therefore fails verification instead of misleading a client, and no client ever starts a daemon of its own — the operator owns that, and from the desktop milestone the shell does (per D66, D77).
- Every daemon error crosses REST as one JSON envelope carrying a stable SCREAMING_SNAKE machine code and an English message (per D68). Codes are append-only across milestones: a later feature group adds codes but redefines neither the envelope nor a code's meaning, and a message may improve while its code cannot.
- Loopback location and a random port are not authentication.
- The frontend REST client is generated from the daemon's OpenAPI document with exactly pinned `@hey-api/openapi-ts` and `@hey-api/client-fetch`, committed, and re-checked in CI so client and daemon cannot drift (per D59). Terminal bytes and live events use the browser's native WebSocket API separately.
- The supervised Hindsight child binds only to loopback, requires a MarketRig-owned bearer credential, and exposes neither its MCP server nor its control plane. Only `marketrigd` calls it.
- MVP has no MarketRig account, hosted or remote control plane, multi-user authorization model, or mandatory telemetry.
- Network access is limited to external runtimes and configured market and memory providers.
- Trigger code and agent runtimes execute with the current user's normal environment and authority.

Exact REST and WebSocket framing beyond the endpoint-discovery contract is **deferred** (§18).

### 4.4 Configuration scopes

There are exactly two product configuration scopes (per D42):

- **installation settings**: runtime discovery targets, provider credentials and model selections, autostart, trigger-code authorization, paper-order approval, trigger-delivery mode, and desktop locale;
- **desk configuration**: workspace, selected runtime, triggers, and paper environment.

There is no layered override hierarchy, no live settings file, no generic key/value configuration layer, and no environment-variable override hierarchy. Typed non-secret configuration is authoritative in the installation-wide SQLite database and changes only through `marketrigd`. Runtime discovery targets are the `runtimes` resource (per D80): one row per runtime, `codex` and `claude`, with `state IN ('UNDISCOVERED','AVAILABLE','UNAVAILABLE')`, the exact executable, its version, `validated_at_ns`, and a failure code and message; read through `GET /runtimes`, re-resolved through `POST /runtimes/{runtime}/discover` with an optional explicit `executable`, and cleared of a control-plane failure through `POST /runtimes/{runtime}/retry` — every answer the resulting row, secrets-free, and a segment naming neither runtime `RUNTIME_NOT_FOUND`. The desk's selected runtime is `desks.selected_runtime`, set at creation by `POST /desks`' optional `runtime` (default `codex`, anything else `VALIDATION`) and moved only by Switch (§6.2). The `~/.marketrig/` directory holds default desk workspaces and no authoritative configuration or daemon state.

Trigger-code authorization, paper-order approval, and trigger-delivery mode are one installation resource (per D70). Code defaults to **Require approval**, orders to **Always allow**, and delivery to `QUEUE`; `STEER` is refused as a disabled mode and any other value as invalid. Each changed field appends one installation-wide policy-changed record. Policies and approvals are reachable only through REST and the desktop: no `marketrig` command reads a policy, lists approvals, or decides one. Until the milestone that delivers the approval workflows, both policies are fixed at **Always allow**.

Provider secrets live only in the native per-user credential store — macOS Keychain or Windows Credential Locker — reached by `marketrigd` alone through one maintained, exactly pinned binding that names the platform backend explicitly rather than relying on backend discovery (`keyring` is the candidate — verified on crates.io 2026-09-01, 4.x line; the pin is confirmed at plan time). SQLite stores only non-secret configuration and opaque references. Native-store failure surfaces as an unavailable capability with no plaintext, environment-file, or second-vault fallback. Secrets never appear in SQLite, logs, prompts, URLs, desk files, trigger results, or CLI output (per D49).

There is exactly one recorded exception, stated wherever that child is specified: the supervised Hindsight child receives its endpoint key and the daemon's per-start bearer through its own process environment, because Hindsight is configured only that way (per D49, D65).

For Hindsight's hosted models the user sets one OpenAI-compatible base URL, API key, LLM model, and embedding model; the same endpoint and key serve both, and no reranking model is configured. SQLite persists the base URL and selected models; the key stays in the credential store. The provider's model list is fetched live whenever the selector opens, never persisted or cached, and a fetch failure is explicit rather than a stale list. The embedding model locks once Hindsight has initialized data with it (per D18).

### 4.5 Localization

*Decision basis: per D68.*

MVP ships exactly `en` and `zh-Hans`. One installation `locale` setting, detected by the desktop from the system language at first launch and changeable in settings, drives the desktop UI, tray menu, and notifications.

Everything the agent consumes has one English source and is byte-identical under both locales: the `marketrig` CLI grammar, help, human output, and error message text; the MCP surface, including resource names, descriptions, tool schemas, and tool errors; the REST/JSON contract, error codes, enumerations, field names, and identifiers; daemon prompts; the seeded `AGENTS.md` and skills; diagnostic logs, operational events, and the trigger firing document. The daemon and CLI never read the locale for rendering. Desk names, instrument identifiers, currency codes, provider names, and the product name are never translated. Financial values display as the daemon's canonical decimal text and are never reformatted. The CLI writes UTF-8 to standard output and error unconditionally so Unicode data survives pipes on both platforms.

Detection, endpoints, catalog mechanics, font and input-method requirements, and parity checks are **deferred** to the localization feature specification.

### 4.6 Daemon startup and shutdown

*Decision basis: per D66, D73, D77, and D80.*

Every `marketrigd` start follows one fixed order, so a daemon a client can reach is a daemon whose own state is already resolved:

```text
1. resolve the data, desks, and log roots through the test seam (§17), creating what is missing
2. acquire the runtime lock exclusively (§4.3); on failure exit nonzero: ALREADY_RUNNING
3. open SQLite, set WAL and enforced foreign keys, apply pending migrations (§15)
4. mint the per-start daemon UUID, which the recovery event itself names
5. run the recovery transaction (§15)
6. complete every interrupted CREATING desk (§5.2)
6a. discover every UNDISCOVERED runtime (§4.1); skipped under MARKETRIG_TEST_DATA_ROOT, so the gate registers its stand-in explicitly and never sees the operator's installations
7. bind 127.0.0.1 on an operating-system-assigned port and mint the bearer credential; the listener is handed to the async runtime as is, never duplicated, because on Windows a duplicated socket is inheritable and every child would keep a dead daemon's port completing handshakes
8. write runtime/endpoint.json atomically — the daemon is now discoverable
```

Shutdown is an authenticated route, `POST /quit`, from the first milestone; the desktop's Quit (§14) uses that same route, and Ctrl+C on a terminal-attached daemon takes the same path. It stops accepting work, ends every open `agent_processes` row `QUIT` — terminals first, then the Codex app-server child (per D80) — drains the database thread, removes `runtime/endpoint.json`, and exits within a bounded wait, after which it exits regardless. A hard kill leaves the endpoint file behind; the operating system releases the lock, and the stale file fails client verification (§4.3).

## 5. Desk model and ownership

*Decision basis: per D7, D8, D15, D20, D21, D22, D46, and D77.*

MarketRig supports multiple durable concurrent desks from day one.

Each desk logically owns:

```text
Desk
├── workspace and AGENTS.md constitution
├── canonical .agents/skills tree
├── Hindsight bank
├── selected runtime
├── last native session pointer per runtime
├── triggers and trigger history
├── paper book and its trading node
├── durable trading history
├── approvals and MarketRig action provenance
└── operational and session history
```

Desk invariants:

- each desk has one lowercase textual UUIDv7 identity and one unique immutable lowercase-kebab name of 1 to 40 characters over `a`–`z`, `0`–`9`, and single interior hyphens, with no leading, trailing, or consecutive hyphen;
- its default workspace is `~/.marketrig/desks/<desk-name>/`, with no UUID in the final folder name;
- one desk is exactly one trader identity;
- at most one agent session is active for a desk, while trigger code is separate and may run concurrently;
- different desks may have active sessions and trigger code at the same time;
- paper books, orders, positions, balances, trigger definitions, workspaces, and cognitive state never cross desks;
- every desk-scoped operation carries the durable desk UUID end to end; the daemon has no process-global selected desk;
- desk-owned relationships include `desk_id` in composite foreign keys, so a child row cannot reference another desk's parent;
- changing runtime or native session does not change desk identity;
- there is no MarketRig Run object.

### 5.1 Workspace ownership

- MarketRig and the user own desk configuration and enforced product physics.
- The user owns the desk constitution in `AGENTS.md`.
- `CLAUDE.md` is a MarketRig-owned compatibility shim containing exactly `@AGENTS.md` followed by one newline.
- MarketRig owns runtime compatibility material and seeds the initial improvement skill.
- After the desk first becomes `READY`, the agent owns all desk skills as well as research, tools, experiments, and Hindsight content; MarketRig never rewrites agent-owned workspace material and reconciles only its own shim and link.

Each desk stores its canonical skills under `.agents/skills/`. Claude Code reaches that same physical directory through `.claude/skills`: a relative symlink to `../.agents/skills` on macOS and a directory junction to the absolute workspace path on Windows. MarketRig maintains no second, copied skill tree and never replaces an ordinary directory found at `.claude/skills` (per D21).

Desk files plus authoritative MarketRig and sandbox state are the durable continuity contract. MarketRig requires no handoff document and keeps no normalized transcript. Lost conversation is accepted.

### 5.2 Creation and recovery

Desk provisioning is distinct from managed runtime lifecycle:

```text
new row -> CREATING -> READY
                    -> FAILED -> CREATING on explicit retry
```

`marketrigd` persists a `CREATING` desk row before touching its workspace. It bootstraps `AGENTS.md`, the exact `CLAUDE.md` shim, `.agents/skills/` with the seeded improvement skill, and the `.claude/skills` link idempotently, then marks the same row `READY`. A failure preserves the row and partial workspace as `FAILED` with a failure code and message; retry reuses its UUID, name, and path. Creation answers only once the row is `READY` or `FAILED`, so a client observes `CREATING` only after a crash, and startup completes every interrupted `CREATING` desk before accepting work.

Readiness covers only the durable row and the local workspace. Hindsight bank setup, trading-node and paper-book setup, runtime activation, and trigger work stay lazy and keyed by the desk UUID.

For a `READY` desk, startup validates the workspace without recreating or rewriting agent-owned material. A missing or unusable workspace, or an unreadable `AGENTS.md`, produces a derived workspace-unavailable status — computed at read time with a one-line reason, never stored, and carried only by a `READY` desk — while the durable desk stays `READY`, and blocks neither other desks nor daemon startup.

The desk schema, the creation and retry sequences, the workspace-status derivation, and the desk group's own verification are settled in [`features/r0-workspace-desk-identity/SPEC.md`](features/r0-workspace-desk-identity/SPEC.md) (per D77); later milestones extend those tables by migration.

## 6. Managed runtime lifecycle

*Decision basis: per D24, D25, D27, D28, D31, D32, D69, and D80.*

### 6.1 Facts MarketRig owns

A desk has at most one live managed native agent process and terminal. MarketRig knows process and terminal liveness and persists the last native session pointer separately for Codex and Claude Code. Runtime-native session catalogs stay owned by their runtimes.

MarketRig neither exposes nor persists a canonical agent-status state machine, and in particular does not translate runtime behavior into `INACTIVE | IDLE | WORKING | WAITING`. Adapter-private safe-delivery gating and runtime-native status text are presentation, not domain state. Durable operational evidence is limited to pointer changes, structured-input readiness and delivery outcomes, explicit attention events, interrupts, exits, and failures.

### 6.2 Lifecycle actions

- **Interrupt**: ask the selected runtime to interrupt its current turn without destroying desk identity or the native session pointer, through that runtime's supported programmatic interface only. A runtime with no structured interrupt answers explicitly that interruption is unsupported rather than falling back to keystrokes; the user's own terminal attachment remains their keyboard (per D69).
- **Continue last session**: resume the selected runtime's exact remembered native session; failure is explicit and never silently becomes Start new.
- **Start new session**: create a new native session and replace that runtime's remembered pointer once its canonical identity is known.
- **Switch runtime**: decide every rejection before anything is stopped, so a refused switch costs no live session; then move the selection and preserve desk identity and both runtime pointers. There is no rollback once activation has begun.
- **Exit**: stop the managed native process and terminal while preserving its last pointer.
- Native `/clear` or equivalent stays runtime-owned and may replace the logical native session without a MarketRig handoff. MarketRig records the replacement and repoints the desk, so its own delivery keeps reaching the session the user is in.

These are user actions on MarketRig's own surfaces — the desktop and, for verification, REST. The agent never invokes them and the `marketrig` CLI never exposes them, because the CLI is the agent's interface and the agent does not exit itself (per D69). Every managed process that ends leaves durable evidence naming what ended it: itself, MarketRig's Exit or Switch, MarketRig's Quit, a lost control plane, or a lost daemon. Concretely (per D80), every `agent_processes` row closes with `exit_reason IN ('EXITED','INTERRUPTED','QUIT','CONTROL_PLANE_LOST','DAEMON_LOST')` and one `SESSION_EXITED` event in the same unit. The routes are five under the desk: `GET /desks/{desk_id}/session` describes the live process or `null`; `POST …/session/activate` with `{"mode": "CONTINUE" | "NEW"}` answers `202` with the process, `409 SESSION_LIVE` while one is live, `409 NO_NATIVE_SESSION` for `CONTINUE` without a pointer, and `409 RUNTIME_UNAVAILABLE` when the selected runtime is not `AVAILABLE`; `POST …/session/interrupt` answers `202` with the turn id, `409 NO_LIVE_SESSION`, `409 NO_ACTIVE_TURN`, `409 INTERRUPT_UNSUPPORTED` (Claude Code), or `502 RUNTIME_ERROR`; `POST …/session/exit` ends the process `INTERRUPTED` and answers `202` once the row closes, else `409 NO_LIVE_SESSION`; `POST …/session/switch` with `{"runtime": …}` validates first — a known runtime (`VALIDATION`), different from the selection (`409 SAME_RUNTIME`), `AVAILABLE` (`409 RUNTIME_UNAVAILABLE`) — then ends any live process `INTERRUPTED`, moves `desks.selected_runtime`, appends `RUNTIME_SWITCHED` in one unit, and does not activate. Exit and Switch wait up to 5 seconds for the row to close and otherwise answer `502 RUNTIME_ERROR` with the shutdown continuing. A native `clear` reported by the runtime's own event repoints the desk to the new session id with `SESSION_POINTER_CHANGED {cause: "clear"}`.

### 6.3 Shared control planes

`marketrigd` owns one control plane per runtime for the installation:

- start it lazily before the first session or delivery using that runtime;
- share it across every desk on that runtime while keeping each desk's native session and terminal isolated;
- keep it alive until explicit Quit;
- isolate a Codex control-plane failure from Claude Code and the reverse.

If a control plane exits unexpectedly, the adapter terminates and cleans every process and terminal associated with it, preserves native session pointers, and emits failure evidence. It attempts one automatic control-plane restart; if that fails, the runtime is unavailable until explicit Retry. A restart never resumes native sessions or replays input.

Concretely (per D80), Codex's control plane is `<codex> app-server --listen ws://127.0.0.1:<port> --ws-auth capability-token --ws-token-file <path>`: the daemon binds `127.0.0.1:0` to pick the port and releases it, writes a fresh capability token `0600` to `runtime/codex-ws-token`, spawns the child under the R2 containment primitive with the data root as cwd, records it in `runtime/children.json`, connects within 15 seconds with an `Authorization: Bearer <token>` header, sends `initialize` then `initialized`, and appends `CONTROL_PLANE_STARTED {pid, port}`. That one JSON-RPC connection reads broadcasts only — `thread/started`, `thread/status/changed`, `thread/closed`, `error` — and sends `turn/start`, `thread/turns/list`, and `turn/interrupt` on behalf of desks. The child exiting or the socket closing ends every Codex `agent_processes` row `CONTROL_PLANE_LOST` with its terminal shut down and its pointer kept, appends `CONTROL_PLANE_LOST`, and restarts once; a second failure sets `runtimes.codex` `UNAVAILABLE` with `CONTROL_PLANE_FAILED` until `POST /runtimes/codex/retry`. Quit stops the app-server after every terminal. Claude Code has no shared control plane: each desk's channel connection (§6.4) is that session's readiness and its only delivery path, so nothing is shared across desks and nothing restarts.

### 6.4 Runtime adapter contract

Each concrete runtime adapter provides only:

- discover and validate its exact launch target and required capabilities;
- lazily start, retry, and stop its shared control plane;
- start or explicitly resume one native session for a desk;
- enqueue daemon input through its supported structured interface with adapter-private safe-delivery gating;
- register the bundled `marketrig-mcp` adapter for every managed session, with the desk configuration that runtime's approval mode requires (§13.1);
- interrupt the current turn where the runtime supports one;
- expose the pointer, readiness, attention, exit, and failure events MarketRig needs;
- exit and clean up its process and terminal.

Adapters normalize no reasoning, cognitive status, tool call, transcript, or approval UX, and enforce no handoff behavior. Terminal escape sequences may support presentation but never establish authoritative lifecycle, approvals, delivery, or actions (per D32).

Both adapters are bound by the same three rules. Structured events associated with a desk and native session are the only authority for pointer discovery, readiness for structured input, explicit attention, interruption, exit, and failure. Activation is resume-first through explicit pointers — `codex resume <thread-id>` and `claude --resume <uuid>` — never ambient continuation, and never a prompt on the process command line (per D28). MarketRig is an observer of the runtime's own event stream and answers no runtime-native approval, so an unanswered runtime approval can never block a MarketRig delivery (per D69).

Two components are fixed regardless of runtime: the Channel bridge is a Rust binary built from MarketRig's own Cargo workspace, owning no durable state and no business logic (per D36, D53), and the terminal substrate is §6.5's. Both adapters implement one private trait — `spawn`, `deliver`, `interrupt`, `exit` — and report readiness, pointer, attention, and exit evidence as events over one channel the dispatcher (§7) drains; events that arrive before the spawning desk's process row exists are held until it does. The per-runtime mechanics are settled (per D80; the full record is the R3 feature SPEC §4–§5):

- **Codex.** Before a launch the daemon writes `<workspace>/.codex/config.toml` with `[mcp_servers.marketrig]` naming `marketrig-mcp --desk <desk-id>` (a `-c` override on the TUI command line does not reach a remote thread). A new session is `<codex> --remote ws://127.0.0.1:<port> --remote-auth-token-env MARKETRIG_CODEX_WS_TOKEN -C <workspace>` in the desk's terminal; a resume is `<codex> resume <thread-id>` with the same flags. The pointer is the first non-`ephemeral` `thread/started` whose `cwd` is the workspace, written to `native_sessions`; readiness is the first time that thread is `idle`, read from `thread/started`'s inline status or a later `thread/status/changed`, which is what makes one rule cover both launches (a new session's only `idle` arrives inline; a resume reaches it through `notLoaded` → `idle`). Delivery is `turn/start {threadId, input: [{type: "text", text}]}` behind the idle gate — the last broadcast status `idle` and `thread/turns/list` showing no active turn (a listing the app-server refuses leaves the gate as the only check); a closed gate makes the prompt wait, never fail. Interrupt is `turn/interrupt` with the turn id from `thread/turns/list`, `NO_ACTIVE_TURN` when there is none. `systemError` and a thread-bearing `error` append `SESSION_ATTENTION`.
- **Claude Code.** A new session is `<claude> --session-id <uuid>` with a UUIDv4 the daemon mints; a resume is `<claude> --resume <uuid>`; both add `--mcp-config <launch>/mcp.json --settings <launch>/settings.json --dangerously-load-development-channels server:marketrig-channel`, where `<launch>` is `runtime/launch/<desk-id>/`, written `0600` before spawn and deleted when the process row closes. `mcp.json` registers two stdio servers, `marketrig` (`marketrig-mcp --desk <id>`) and `marketrig-channel` (`marketrig-mcp --desk <id> --channel`); `settings.json` declares `SessionStart`, `Notification`, and `Stop` command hooks, each `marketrig --desk <id> session hook` (§13.2), omitted when the CLI is not beside the daemon. The pointer is written at spawn and confirmed by `SessionStart {source: startup | resume}` carrying the same `session_id`; `source: clear` repoints; a `session_id` that is neither is `SESSION_ATTENTION {kind: "foreign_session"}`; `Notification` is `SESSION_ATTENTION` with its type and title; `Stop` is `SESSION_TURN_ENDED`. Readiness is the bridge's connection to `GET /desks/{desk_id}/channel` (an authenticated WebSocket, one per desk, a second closing the first `4001`, one with no open process closed `4002`); the bridge opens it only after the client's `notifications/initialized`, because Claude Code drops a channel notification pushed earlier. Delivery is one text frame `{"content", "prompt_id", "kind"}` per prompt, which the bridge republishes as `notifications/claude/channel {content, meta: {prompt_id, kind}}`; a completed write is `DELIVERED` (per D36), no connection within 30 seconds or a write error is `FAILED CHANNEL_UNAVAILABLE`; there is no idle gate, because Claude Code queues channel events itself. Interrupt answers `409 INTERRUPT_UNSUPPORTED` before anything is touched.
- **Launch environment.** The terminal spawn clears the daemon's environment and passes only the captured login `PATH`, `HOME`, `TERM=xterm-256color`, `LANG` and `LC_*`, on Windows the system variables Winsock, the shell shims, and the runtimes' state directories need (`SYSTEMROOT`, `COMSPEC`, `PATHEXT`, `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`, `APPDATA`, `LOCALAPPDATA`, `TEMP`, `TMP`), `MARKETRIG_DESK_ID`, the test seam's own `MARKETRIG_*` variables, and for Codex `MARKETRIG_CODEX_WS_TOKEN` — never the daemon bearer, and never an inherited `CLAUDE_CODE_CHILD_SESSION`, which silently disables Claude's transcript and breaks every later resume.

### 6.5 Terminal manager

One private terminal manager owns terminal creation, attachment, raw-byte pumping, resize, process containment, and shutdown for both adapters, over Unix PTY on macOS and ConPTY on Windows through one maintained, exactly pinned Rust PTY crate (selection at plan time, per D31):

- one worker continuously drains each live terminal so a slow consumer cannot block the child;
- each desk has one current attachment generation; attaching a new generation invalidates the previous writer, and input and resize come only from the current generation;
- each live terminal keeps one bounded in-memory reconnect ring, replayed to a new generation before live bytes;
- resize requests coalesce to the newest dimensions;
- shutdown stops input, drains available output, terminates the contained process tree, closes handles, and joins the worker;
- no terminal transcript, screen snapshot, or reconnect ring survives process or daemon exit, and a PTY is never resumable.

Concretely (per D80), the crate is `portable-pty =0.9.0`: its Unix spawn is `std::process::Command` with a `pre_exec` that only calls `setsid` and `TIOCSCTTY` before `exec`, so no single-threaded spawn helper exists (per D31). The ring is 256 KiB, replayed as one binary frame; shutdown drains for at most 2 seconds, then terminates the tree — `killpg` on the child's own session on macOS, a kill-on-close Job Object the terminal assigns immediately after spawn on Windows — and the exit is the child's wait, never the reader's EOF. The attachment is `GET /desks/{desk_id}/terminal`, a bearer-authenticated WebSocket answering `404 DESK_NOT_FOUND`, `409 NO_LIVE_SESSION`, and `400 VALIDATION` for a non-upgrade before any attachment is taken: binary frames are raw bytes both ways, the client's text frame `{"resize":{"cols":n,"rows":n}}` resizes, a new attachment closes the previous one `4001`, a consumer whose 1 MiB send buffer fills is dropped the same way, and process end sends `{"exited":{"reason":…,"code":…}}` then close `1000`. One byte is interpreted, and only one: ConPTY writes the cursor-position query `ESC[6n` at spawn and holds the child's console initialisation until the host answers, so with no attachment live the reader answers `ESC[1;1R` itself and drops the query from the ring; with an attachment live the bytes pass through and the viewer's terminal answers, as on macOS. It is the sole exception to raw-byte pumping, and it is not presentation (per D30).

## 7. Activation

*Decision basis: per D22, D28, D36, and D80.*

The canonical automatic activation path is:

```text
try selected runtime's last native session
-> if usable, resume it
-> otherwise start a new native session
-> wait for the runtime's structured-input path to be ready
-> deliver fresh orientation, pending daemon prompts, and failure notices as ordinary input
```

Rules:

- explicit **Continue last session** failure is shown to the user and never silently becomes Start new;
- daemon-prompt-driven activation starts new automatically when the remembered session is missing or unresumable;
- runtime-wide launch, authentication, configuration, or spawn failure fails daemon prompt delivery;
- activation prompts are ordinary structured input after launch, never command-line or system prompts;
- a fresh desk receives one paragraph of orientation naming the desk, its workspace, its `AGENTS.md`, the `marketrig` command, and the desk's market resources, and ending by asking what the user has in mind; nothing is decided for the agent;
- prompts that failed since the previous activation are disclosed once, as identity, kind, and failure code, with their content never repeated and never redelivered;
- bootstrapping durable files such as `AGENTS.md` is code-driven, never hidden in the activation prompt.

Concretely (per D80), activation is one dispatcher task per daemon, woken by a `Notify` every prompt insert and every adapter readiness or exit event signals, and by a 30-second recheck. Each pass visits every desk with `QUEUED` prompts, oldest first: a desk with a ready live session gets one delivery of its head prompt; one with a live session not yet ready waits; one with no live session is activated — resume when `native_sessions` holds a pointer for `desks.selected_runtime`, otherwise new — in one unit that inserts the `agent_processes` row, appends `SESSION_STARTED {runtime, mode, native_session_id}`, and for a new session inserts an `ORIENTATION` prompt, preceded by one `DISCLOSURE` when undisclosed `FAILED` prompts exist, dated just before the desk's oldest queued prompt so they head the FIFO. A selected runtime not `AVAILABLE` fails every queued prompt of the desk `RUNTIME_UNAVAILABLE` at once. Readiness must arrive within 120 seconds of spawn; the deadline passing or the process exiting first ends the process with the reason the evidence supports and fails every prompt `QUEUED` for that desk at that moment `ACTIVATION_FAILED`, carrying the exit code or `timeout`. A resume that exits before readiness on the dispatcher path ends `EXITED`, appends `SESSION_POINTER_CHANGED {to: null, cause: "unresumable"}`, and starts new once; on explicit Continue it is evidence only. A process that exits while prompts are queued leaves them `QUEUED` for the next pass.

## 8. Trigger model

*Decision basis: per D34, D35, and D70.*

### 8.1 Definition

A trigger has these semantic dimensions:

- exactly one owning desk;
- source: `SCHEDULED | EVENT`;
- recurrence: `ONE_OFF | RECURRING`;
- preserved agent-authored brief and context;
- optional immutable code snapshot;
- enabled and authorization state;
- durable execution and delivery history.

There is no `MANUAL` trigger kind. Immediate user input goes directly to the active runtime; a user request may cause the agent to create a scheduled or event trigger for later work.

Trigger terminology is exact:

- an **occurrence** is a calendar or event candidate that may be accepted, skipped, or deduplicated;
- a **firing** is the immutable desk-bound work instance created only by atomic occurrence acceptance;
- an **execution** is the at-most-once code attempt for a code-bearing firing;
- a **result** is the persisted code-free or execution outcome from which one daemon prompt is queued.

A missed scheduled occurrence is operational evidence, never a firing.

`SCHEDULED` lands with the scheduling milestone; `EVENT` is a defined contract for the milestone that widens ingress.

### 8.2 Event occurrence semantics

An EVENT occurrence means desk-scoped input with a stable ingress-scoped identity durably reached `marketrigd`:

- local CLI-to-daemon ingress only; there is no public webhook;
- ingress targets one desk;
- matching uses the exact desk-scoped event name;
- one distinct occurrence independently creates a firing for every enabled matching trigger;
- an EVENT `ONE_OFF` commits its firing and disables itself atomically on the first distinct match; later execution or delivery failure never rearms it;
- an EVENT `RECURRING` commits one firing for every distinct match, even while an earlier firing is executing or queued;
- reaccepting the same occurrence identity creates no additional firing;
- the event may carry raw data but does not inherently contain code.

Connector framing, occurrence-identity construction, payload limits, deduplication storage, filtering, backpressure, and public ingress are **deferred** (§18).

### 8.3 Trigger lifecycle

- A code-free trigger becomes active immediately when created.
- A code-bearing trigger becomes active immediately under **Always allow**.
- Under **Require approval** the complete trigger is written first and its code snapshot stays pending — with the trigger carrying no next occurrence, so the scheduler cannot see it — until approved in MarketRig. An unapproved trigger is never due, enable and disable cannot resurrect it, and an approved elapsed one-off stays undue (per D70).
- Only a code or launch-snapshot change requires reapproval; brief, schedule, event name, and enablement changes do not.
- Each accepted firing atomically captures the occurrence identity, current brief and context, trigger revision, and approved code-snapshot identity as immutable firing-time provenance. The same transaction consumes a one-off or advances a recurring definition. Later trigger edits affect only future firings.
- Disabled triggers do not fire, buffer input, or create missed occurrences.
- Re-enabling a recurring scheduled trigger resumes at its next future occurrence; re-enabling a recurring EVENT trigger waits for the next distinct matching occurrence.
- A past scheduled one-off must be explicitly rescheduled; a consumed EVENT one-off stays disabled unless explicitly re-enabled.
- Deleting a trigger removes its definition and future firing while preserving past history.
- Editing, disabling, or deleting a definition does not cancel an already persisted result, and MVP has no manual cancellation of queued results.

### 8.4 Concurrency

- Trigger code executions are FIFO-serialized per desk by daemon acceptance order.
- Different desks execute trigger code concurrently.
- Trigger code may execute while the same desk's agent session is active.
- MarketRig serializes and attributes mutating actions at the desk action boundary.

## 9. Trigger code execution

*Decision basis: per D35, D41, and D79.*

Trigger code:

- is stored as a MarketRig-managed immutable UTF-8 source snapshot;
- carries a filename suffix and a direct argument vector containing exactly one whole-argument script-path placeholder; no command shell and no string interpolation participates;
- fingerprints the source, suffix, argument vector, and timeout with SHA-256 under one UUIDv7 snapshot identity, and only a change to that snapshot requires reapproval;
- runs once per firing, as the current user, in the desk workspace;
- chooses its own executable and language from the user's installed environment;
- inherits the captured user environment plus the desk, trigger, and firing identifiers, minus MarketRig's private runtime entries;
- receives one versioned UTF-8 JSON firing document on standard input and then EOF;
- returns its raw result on standard output, with standard error, exit status, timings, resolved executable, and termination diagnostics recorded beside it;
- runs to completion or its approved daemon-owned timeout, under daemon-owned concurrent stream capture and output limits;
- receives no automatic execution retry;
- may call the desk-scoped `marketrig` CLI and the desk's MCP surface, including paper actions.

`marketrigd` launches trigger code directly without a command shell. macOS uses a dedicated POSIX session and process group; Windows uses a kill-on-close Job Object through one maintained, exactly pinned Windows API binding (selection at plan time); the kill-on-close limit is set by MarketRig itself, because the pinned wrapper crate does not (R2 feature SPEC §4.5). Timeout or cancellation terminates the managed group and its ordinary descendants. One internal containment primitive implements this spawn-and-terminate contract for every managed child MarketRig owns, so both platforms have exactly one process-tree lifecycle path (per D41, D73).

MarketRig promises ordinary descendant cleanup, not an adversarial sandbox. Code that deliberately escapes macOS process-group containment violates the trigger execution contract and is outside the MVP guarantee.

Completion persists the artifact metadata, the execution outcome, the result, and one queued daemon prompt in a single transaction. A code-free firing creates its result and queued prompt in the same transaction as firing acceptance. A code-bearing firing is claimed FIFO at most once per desk. Normal exit, nonzero exit, timeout, output limit, spawn failure, Quit, and daemon-loss recovery all produce exactly one durable terminal outcome and at most one result prompt; none reruns the code (per D35).

The concrete shape (per D79; the full record is the R2 feature SPEC §4): a snapshot is `source` (UTF-8, at most 256 KiB), `suffix` (empty, or `.` and 1–15 alphanumerics), `argv` (1–64 strings of at most 4 KiB, exactly one the whole argument `{script}`), and `timeout_secs` (1–3,600, default 300), fingerprinted as lowercase-hex SHA-256 over those four joined by NUL. The source is written to `<data root>/runtime/scripts/<firing_id><suffix>` (mode `0700` on macOS) and removed afterwards, best effort; the child runs in the desk workspace with the daemon's environment plus `MARKETRIG_DESK_ID`, `MARKETRIG_DESK_NAME`, `MARKETRIG_TRIGGER_ID`, and `MARKETRIG_FIRING_ID`. The version-1 firing document is `{ version, desk: {id, name, workspace_path}, trigger: {id, name, recurrence, revision}, firing: {id, occurrence_ns, accepted_at_ns}, brief, context, code_snapshot_id }`. Standard output and error are captured concurrently under caps of 1 MiB and 256 KiB and stored as bytes on the execution row — there are no artifact files. Outcomes: `EXITED` (exit code, or the signal in `error`), `TIMED_OUT`, `OUTPUT_LIMIT` (naming the stream), `SPAWN_FAILED` (the OS error), `QUIT`, and `DAEMON_LOST` (the dead daemon's UUID). The containment primitive is `process-wrap =9.1.0`: `ProcessSession` on macOS, `JobObject` with `KillOnDrop` (kill-on-close) on Windows.

## 10. Scheduled-trigger semantics

*Decision basis: per D37, D40, and D79.*

Scheduling is daemon-owned. One coordinator task reads the earliest eligible SQLite `next_occurrence_ns` projection and waits on that deadline, a recheck of at most 60 seconds, or an in-memory wake signal published after a schedule mutation; it holds no armed occurrence in memory. The recheck keeps a clock change from leaving an obsolete long sleep. SQLite is the only durable authority: no scheduler framework and no separate job store participates.

- A one-off schedule stores an absolute UTC instant.
- A recurring schedule stores one unbounded RFC 5545 RRULE, a naive local `DTSTART`, and an explicit IANA timezone. Local recurrence calculation and UTC resolution come from one maintained RFC 5545 recurrence crate and one IANA timezone-database crate, both pinned exactly (selection at plan time).
- The first scheduling milestone rejects sub-minute recurrence, `COUNT` and `UNTIL`, multiple rules, and RDATE/EXDATE sets.
- Daylight-saving behavior is explicit: a candidate with no valid round trip through UTC is skipped, and an ambiguous candidate resolves to its first occurrence.
- When the scheduler wakes, one transaction examines every enabled, approved, undeleted scheduled trigger whose occurrence is due. If the current daemon was already running before that occurrence's deadline and acceptance begins no more than 60 seconds late, the transaction inserts its unique firing and consumes or advances the trigger; otherwise it records a miss and advances.
- An occurrence whose deadline passed before this daemon started, or that is more than 60 seconds old at acceptance, never becomes a firing. One reconciliation record summarizes the missed range. A missed one-off is terminal; a recurring trigger keeps its original anchor and advances directly to its first future candidate (per D37).
- Disabled or approval-pending time creates no miss record. Re-enabling or approving computes the first future candidate; an elapsed one-off must be explicitly rescheduled.
- A one-off is atomically consumed at its first accepted firing, even if code later fails or times out.
- A recurring trigger stays enabled after execution failure; MVP has no automatic circuit breaker, and definitions and results do not expire automatically.

Scheduled occurrence identity is the trigger identity plus its scheduled instant, and the firing table enforces that uniqueness, so a duplicate wake cannot execute or deliver the same occurrence twice. Storage and the unit (per D79; the full record is the R2 feature SPEC §2–§3): a one-off is `{ "at": <RFC 3339> }` stored as `at_ns`; a recurring schedule is `{ rrule, dtstart, tz }` — one RRULE value without prefix, a naive `YYYY-MM-DDTHH:MM:SS`, an IANA name — iterated by `rrule =0.14.0` in a fixed UTC frame over the wall clock and resolved candidate by candidate through `chrono-tz` in the named zone. Rejected as `TRIGGER_INVALID`: `FREQ=SECONDLY`, `COUNT`, `UNTIL`, `BYSECOND`, any line break or `RRULE:`, `DTSTART`, `RDATE`, `EXDATE`, `EXRULE` token, an unparseable rule or `dtstart`, an unknown zone, a one-off not in the future. The projection `triggers.next_occurrence_ns` is the first candidate strictly after a reference — the accepted occurrence after acceptance; now after a miss, enable, reschedule, or creation — and the partial index over enabled, undeleted, non-null projections is the eligible set the one task reads. Acceptance inserts the firing (occurrence, brief, context, revision, snapshot id) and, for a code-free firing, its `TRIGGER_RESULT` prompt; a miss appends one `TRIGGER_MISSED` event carrying the range from the stale projection through now and the candidate count (capped at 10,000).

## 11. Daemon prompt delivery

*Decision basis: per D36, D70, D71, and D80.*

### 11.1 States and ordering

```text
QUEUED     prompt durably accepted but not submitted to the runtime
DELIVERED  adapter completed the runtime's supported handoff boundary
FAILED     handoff did not complete, or completion is unknown
```

Rules:

- persist the source payload and the delivery record before attempting delivery;
- deliver at most once;
- never retry or replay a failed or uncertain delivery automatically;
- preserve each prompt independently;
- order prompts FIFO per desk by durable daemon acceptance, retaining source timestamps separately;
- batching may optimize transport but may not merge, drop, or reorder semantic prompts;
- if the supported handoff completed and the runtime later crashes, the state stays `DELIVERED` with separate runtime-failure evidence;
- a prompt waiting under `QUEUE` stays durable if the current turn or session ends before submission;
- failed prompts and notices appear in the next activation prompt as disclosure, never as redelivery;
- each attempt appends one structured operational record naming the desk, prompt, kind, runtime, native session, and outcome code.

Concretely (per D80), the handoff boundary is the Codex `turn/start` response — a `turn` in the result is `DELIVERED`, a JSON-RPC error `FAILED DELIVERY_REFUSED`, a connection lost before the response `FAILED HANDOFF_UNKNOWN` — and the Claude channel write, `DELIVERED` on completion and `FAILED CHANNEL_UNAVAILABLE` on error or with no connection after 30 seconds; the dispatcher adds `ACTIVATION_FAILED` and `RUNTIME_UNAVAILABLE` (§7), and those five are the whole `failure_code` vocabulary. An attempt writes `attempted_at_ns`, `runtime`, and `native_session_id` in one unit *before* the adapter call, and `state`, `resolved_at_ns`, `failure_code`, and `PROMPT_DELIVERED` or `PROMPT_FAILED` (the latter carrying `failure_detail`) in a second unit after it, which is what lets recovery resolve a daemon lost mid-handoff as `HANDOFF_UNKNOWN` (§15); a closed gate clears `attempted_at_ns` again and leaves the row `QUEUED`, so a wait is never recovered as a handoff. Every prompt is delivered as one English text, byte-identical under both locales: `TRIGGER_RESULT` and `EVALUATION` render their payload as a fenced JSON block after one line `MarketRig <KIND> <id>:`; `ORIENTATION` is §7's paragraph; `DISCLOSURE` lists each undisclosed failed prompt as `<id> <kind> <failure_code>` on its own line and stamps `disclosed_at_ns` on those rows in the delivery unit.

Trigger-result input carries only the trigger reference, the firing-time brief and context snapshot, and the raw result or artifact reference. Concretely (per D79), a `TRIGGER_RESULT` prompt's payload is the trigger (id, name), the firing (id, occurrence, acceptance), the brief and context, and — for a code-bearing firing — an execution summary (outcome, exit code, byte counts, truncation flags); the captured streams stay on the execution row and are read through the firing route. Evaluation input carries a stable realized-P&L history reference with enough order, fill, position, fee, and provenance identity for the agent to query supporting history. Operational queue status and installation policy are queryable but are never injected into a prompt.

Queue status is queryable per desk, newest first: a listing that carries every delivery fact except the prompt text, and a single read that adds it. Reading a failed prompt's content there is the agent's own initiative and is not redelivery; the daemon still never resubmits it (per D71).

Trigger payloads are untrusted data. The firing-time brief and context snapshot is instruction. Both are ordinary user-level content.

If the desk has no live managed session, MarketRig activates it resume-first and waits for structured-input readiness. If it has a live session, the adapter applies its runtime-private safe queue boundary. Neither path creates a product-level agent status. There is no queue cap; backpressure policy is **deferred** until EVENT volume makes it necessary.

### 11.2 Queueing and reserved steering

The installation setting defines two product modes:

- `STEER` (**Steer active turn**): reserved for post-MVP and disabled throughout MVP;
- `QUEUE` (**Queue next turn**, the enabled default): submit through the runtime's supported queue so an active turn is never interrupted and the prompt starts only at that runtime's safe next-turn boundary.

Every trigger result and realized-P&L evaluation prompt uses `QUEUE`. Delivery never interrupts or cancels a turn, is never keystroke emulation, and is never an automatic retry of an uncertain handoff. Interrupt stays a separate explicit user control (§6.2). Direct user prompts are not triggers and keep native runtime behavior. FIFO order is preserved per desk.

Enabling `STEER` requires a post-MVP decision and a runtime contract; it cannot be selected through settings, CLI, or API. The stored delivery mode admits only `QUEUE`, the policy resource reports steering as unavailable so the desktop can render it visibly disabled, and a request selecting `STEER` is refused (per D70).

Claude delivery is considered handed off when written to the Channel transport, because the protocol provides no acknowledgement; MarketRig never retries after a write with uncertain acceptance (per D36). Onboarding rejects a runtime version or configuration without the required structured queueing capability.

## 12. Trading and public-data boundary

*Decision basis: per D5, D9, D10, D38, D39, D63, D64, D70, D74, D76, and D78.*

### 12.1 Trading authority and node topology

MarketRig builds on the published `nautilus-*` crates.io releases, pinning one exact tested version line across every `nautilus-*` crate in lockstep — `=0.62.0`. MarketRig does not use the Python or PyO3 surface, does not support the 1.x line, and does not fork or patch the crates (per D39). Bumping the pinned line is an all-or-nothing move verified by the trading module's own checks.

NautilusTrader is authoritative for tradable instruments and execution-relevant market state, paper execution and fill production, position, balance, fee, and realized-P&L calculation, and each desk's current sandbox execution and accounting state. MarketRig computes no trading fact of its own.

Topology (per D64):

- `marketrigd` hosts one `LiveNode` per trading desk, in the daemon process, each on its own OS thread with its own current-thread Tokio runtime, its own data client or clients, its own sandbox execution client and paper book, and its own cache, account, and matching engines;
- a node is built and run on one thread and is never moved between threads, because every NautilusTrader global is thread-local;
- adding a desk adds a node; a node failure rebuilds that desk's node from committed state and leaves the other desks' nodes untouched;
- a venue routes to one execution client per node, which is answered by one node per desk rather than by venue aliasing;
- a node that cannot start, or a feed that disconnects, makes that desk's paper trading and quote reads explicitly unavailable; it never fails daemon startup and never affects another desk.

Normative build and wiring rules, each settled by the 2026-09-01 verification spikes:

- market data comes from out-of-tree `DataClient` implementations in MarketRig's own crates, registered on the live node builder — the same public traits and the same publish call the shipped venue adapters use;
- paper execution comes from the sandbox execution client registered through the builder's simulated-client entry point, which is a distinct factory trait from the live-adapter one;
- the workspace sets the numeric precision feature **explicitly** rather than inheriting it through Cargo feature unification, and the daemon asserts the precision the node reports at startup; CI asserts the same;
- the data-event sender is a thread-local that panics when obtained off the node thread, so it is taken on the node thread and a clone is moved into any polling task;
- quote precision is derived from the cached instrument and never from a formatting choice, because the sandbox silently drops a quote whose price or size precision does not match the instrument;
- the fee model is configured **explicitly**, in both directions: an absent fee model means "charge the instrument's own declared rates," not "charge nothing";
- a node's clock delivers time events to one default handler and the last registration wins, so timer callbacks have exactly one owner per node; components sharing a node must not each assume a timer.

OpenBB research is deferred past MVP on scope (per D9): MarketRig bundles no research provider, and research is the agent's own through its shell, tools, and workspace. When it arrives it is informational only, never a writer of trading state, reached only through `marketrig`, and hosted as its own supervised child beside Hindsight's.

### 12.2 Shared reads and isolated books

- Any public read may use installation-wide provider connections and caches, and is read-only with respect to desk identity.
- `marketrigd` holds one installation-wide in-memory market-state module fed by the desks' nodes. It keeps the latest atomically accepted execution-relevant observation for each canonical instrument and never restores volatile quotes from SQLite after a restart.
- Each accepted update advances that instrument's MarketRig observation sequence; reads alone never do.
- A successful read exposes canonical decimal prices, provider and venue, source observation time when available, MarketRig receipt time, read time, computed age, and current feed-health evidence.
- After a disconnect, a read may return the last observation only with its original sequence and times, an increasing age, and explicit disconnected health. With no accepted observation the read fails explicitly as unavailable. No read silently substitutes another provider.
- Canonical instrument identity is the NautilusTrader instrument identifier.
- Each desk's paper book is isolated: orders, positions, balances, and mutable execution state never cross desks.
- One keyless equity feed serves the US, Hong Kong, and China A-share markets. It carries no bid or ask, so the desk's equity book is synthesized in every market and everything downstream that would reason about spread or depth is told so; non-US observations may be delayed, which the read's provenance, timing, and age expose rather than hide (per D76).
- The equity feed is one polled Yahoo chart client per node on MarketRig's own thin endpoint layer. It polls each catalog instrument once at subscription whatever the phase, then only while the instrument's market is `OPEN` — every 30 seconds, tightened to every 10 seconds while the desk holds an open order or a nonflat position in it. HTTP 429 is retried up to 8 attempts 400 ms apart; exhaustion or any other failure leaves the last accepted observation standing and marks health `DEGRADED`. The feed declares no delay figure — `exchangeDataDelayedBy` is null on all three exchanges — so source time against receipt time is the only delay evidence, and the contract promises no delay constant (per D78).
- Health is `LIVE`, `DEGRADED`, or `UNAVAILABLE`; an unavailable read omits exactly the price fields and keeps identity, phase, health, and the synthesized-book flag. Every observation is labeled with its market phase — `OPEN` inside a weekly session, `CLOSED` otherwise: US 09:30–16:00 America/New_York; Hong Kong 09:30–12:00 and 13:00–16:00 Asia/Hong_Kong; China A-share 09:30–11:30 and 13:00–15:00 Asia/Shanghai; Monday through Friday. Phase gates polling and labels observations; it never gates an order. There is no holiday calendar (per D78).
- Tradable instruments are a curated catalog compiled into the daemon — per entry the NautilusTrader identifier, the feed symbol, the market key, the currency, a fixed price increment, and the lot size; fifteen entries at R1. The catalog is the polling universe and the `instruments` resource; anything outside it answers `INSTRUMENT_UNKNOWN`, and extending it is a data change rather than a decision (per D78).

### 12.3 Trading actions and approvals

MVP structurally exposes sandbox and paper execution only. MarketRig adds no risk-policy engine for spot, leverage, shorting, or bankroll limits; the user handles those through interaction with the agent (per D10).

Every mutating trading command carries a desk-scoped idempotency identity, and retrying after an uncertain client response must not create a second action for the same identity. Every action is an attributed record naming its source — an interactive session or a firing's trigger code — with stable machine-readable output (per D64). Attribution is two request headers, `X-MarketRig-Trigger-Id` and `X-MarketRig-Firing-Id`, which the shared daemon client attaches when both `MARKETRIG_TRIGGER_ID` and `MARKETRIG_FIRING_ID` are in its environment: both naming a firing of the path's desk under that trigger → `source TRIGGER` with both ids on the row and the record; absent → `SESSION`; anything else → `ATTRIBUTION_INVALID` and nothing recorded (per D79).

The installation-wide paper-order setting is **Always allow** or **Require approval**, applying equally to interactive and trigger-code orders. Approvals belong to the MarketRig desktop and tray, never to the native runtime approval system and never to the `marketrig` CLI. Under **Require approval** an order commits as pending, is never handed to the sandbox until approved, answers a null order projection, and returns the same record for a repeated idempotency key; approval re-enters the ordinary accept path exactly where acceptance left it, denial is terminal, and a cancel is never gated. A decision queues no daemon prompt. The agent may read the state of its own records but can neither set a policy nor decide an approval (per D70).

Paper scope arrives in two stages, and MarketRig inherits the sandbox's physics exactly rather than approximating them:

- **Equities first** (per D76): one keyless feed covering the US, Hong Kong, and China A-share markets against the sandbox, on one multi-currency paper account per desk with realized P&L in each instrument's own currency. T+1 settlement, daily price limits, trading halts, and opening and closing auctions are not simulated, and the seeded desk constitution names those limits. Per-exchange market hours and staleness, the synthesized book, instrument metadata, the fee model, and the retry policy were settled before implementation by the equity paper-trading feature specification and are summarized in §12.2 and below (per D78).
- **Full Kraken crypto after** (per D74): one margin netting account per desk spanning spot and futures on the single Kraken venue, long and short, with leverage bounded by the venue's advertised tier per instrument and every single-order type the sandbox matches, under the time-in-force, post-only, and reduce-only options it supports. Funding payments, margin interest, rollover fees, liquidation, and auto-deleveraging are not simulated; funding rates and mark and index prices are observations only; inverse contracts settle in their base crypto. The seeded desk constitution names those limits so the agent trades knowing them.

Equity paper mechanics (per D78):

- each desk's book is one NautilusTrader **cash** account with netting positions, seeded once with 100,000 USD, 1,000,000 HKD, and 1,000,000 CNY and never converting between them — an instrument trades against its own currency. Under the hood it is one venue account per venue, a market's seed split evenly across its venues, so a sufficiency refusal is per-venue with the sandbox's own reason saying so;
- the synthesized book puts both sides at the last observation at the instrument's precision, one lot each: a market order fills at last for up to one lot and steps past it for more, a limit order rests until last crosses it, and round trips cost only fees;
- R1 accepts `MARKET` and `LIMIT` orders, time in force GTC, plus cancel. The daemon validates **form** only — catalog membership, side, type, a quantity that is a positive multiple of the lot, a limit price that is a positive multiple of the tick and present exactly on `LIMIT` — answering `ORDER_INVALID`; sufficiency is the node's judgment, and any node refusal answers `ORDER_REJECTED` carrying the node's own reason, because a daemon predicting balance or position sufficiency would be computing a trading fact (per D38). Submission is synchronous through the sandbox; a desk that is not `READY` answers `DESK_NOT_READY`;
- fees are charged by the sandbox at each market's declared per-side rate — US 0 bp, Hong Kong 11 bp, China A-share 3 bp — through an explicitly configured maker-taker fee model reading the rates each catalog instrument carries;
- the desk-scoped idempotency identity above is a caller-supplied `action_id`, 1–64 characters of `[a-z0-9-]`, unique per desk, recorded in `trading_actions` before the sandbox sees the order. A repeat returns the stored record with `200` and acts on nothing, and that holds for refused actions too — the record, not the original status, is the contract. A submit's `action_id` is reused as the order's `client_order_id`; a cancel names that id plus its own `action_id`, and an unknown or terminal order answers `ORDER_NOT_FOUND`.

### 12.4 Ledger and provenance

NautilusTrader produces execution and accounting facts. `marketrigd` appends those facts to the desk's immutable SQLite trading history: orders, individual fills, position cycles, fees, realized P&L, and the state required to restore the current paper book. That SQLite history is the durable authority. MarketRig may build query projections but never independently recalculates or edits a sandbox-produced fact (per D38).

Capture rules:

- the sandbox's own event and account payloads are stored as versioned opaque bytes beside the normalized fields MarketRig queries, so a captured fact can always be read back exactly as produced;
- a **position cycle** is the unit of realized P&L, and a fill that carries a netting position through zero closes one cycle and opens the next in one unit (per D64, D74);
- the evaluation signal is the net-of-fees realized P&L the closing event reports, not the gross realized return the same event also carries;
- a realized-P&L fact and its queued evaluation prompt commit in one transaction, so neither can survive without the other (per D22, D38);
- **book restoration** across a daemon restart works from serialization snapshots of account state, open positions, and open orders, re-placing resting limit orders under their original client order identifiers — never from history replay (per D64).

`marketrigd` also retains authoritative records for trigger executions and delivery, code and order approvals, action source and idempotency identity, firing-time brief and context with code-snapshot identity and trigger attribution, runtime and session lifecycle and failures, and the sandbox-produced trading facts with their provenance. Agents may query but never edit these records or sandbox trading reality.

MarketRig holds no position objects outside the node: the node's own cache is the live authority and SQLite the durable one (per D78). The durable representation is migration 2's tables — `trading_actions`, `order_events`, `fills`, `position_events`, `position_cycles`, `prompts`, and `book_snapshots` — every row desk-scoped, every sandbox event stored verbatim as versioned JSON beside the normalized columns MarketRig queries. Order and position listings are query projections over those tables, never stored ones. `book_snapshots` holds exactly one serialization snapshot per desk — account state, open positions, open orders — rewritten in the same unit as every event that changes the book. A closing fill's unit inserts the `position_cycles` row and its `EVALUATION` prompt, born `QUEUED`, whose payload is a stable history reference: the cycle id, the instrument, net realized P&L with its currency, the close instant, and the client order and fill ids behind it (§11.1).

A desk's node starts lazily on its first market-plane operation after daemon start, never eagerly: assert precision, build the node with the data client on the builder, load the catalog into the cache, run the node, restore from `book_snapshots` before the node is published to any caller — resting limit orders re-placed under their original client order identifiers — and append `TRADING_NODE_STARTED`. A start failure appends `TRADING_NODE_FAILED`, answers `MARKET_UNAVAILABLE` to that desk's market-plane operations, blocks nothing else, and the next operation may retry. Daemon shutdown stops every node inside §4.6's bound (per D78).

### 12.5 Open verification

Of the three things the 2026-09-01 spikes left unproven (per D64, D76), R1 closed two (per D78): paper-book reload across a restart is proven by `trade::snapshot_restores_book` and the gate's G17, and the equity feed's behavior under a moving book — tick ordering, staleness, duplicate suppression, and the 429 bound — by the feed module checks and the gate's G18 on the stand-in, exercised against live sessions by the attended cells. Isolation beyond one trading desk per daemon stays unmeasured: the R1 gate trades on one desk, and larger fan-out is deferred rather than designed for.

## 13. Agent surface

*Decision basis: per D4, D50, D63, D68, D69, D71, D77, D78, and D80.*

The agent surface is split by what the agent is doing, and no capability appears on both halves (per D4). **MCP is the market plane** — what the agent does *in the market*: observe and act. **The `marketrig` CLI is the continuity plane** — what the agent does *in the harness*: durable records, structure, and cognition. The daemon's SQLite is the evidence authority for every action either plane performs; the transcript never is.

The split is measured, not assumed: six sessions per client against Claude Code 2.1.252 and Codex CLI 0.151.0 with an `rmcp =3.2.0` probe server, arbitrated by raw JSON-RPC logs rather than model prose. Every rule below that names a client behavior rests on that wire evidence.

### 13.1 MCP trading plane

One bundled stdio adapter, `marketrig-mcp`, delivers the whole plane to both runtimes. It calls the authenticated daemon on every resource read and every tool call and owns no market cache, provider connection, durable state, private book access, or action authority. The runtime adapter registers it for every managed session (per D63, §6.4). It is required from the first trading milestone.

**Resources carry awareness**: the desk's current quotes, its book, its live positions, its open orders, and its tradable instruments. They are:

- **enumerated concretely** in `resources/list`, desk-scoped, never expressed only as URI templates — one pinned client never asks for templates at all, so a template-only surface is invisible to it;
- **re-read explicitly**; a read returns the observation the daemon holds at read time, and neither pinned client caches a read, so a re-read genuinely refreshes;
- kept small in count and naming, because one client lists resources eagerly at session start and pays that cost in every session's context.

**Tools carry the money actions**: two or three typed tools for paper-order submit and cancel. The daemon validates every argument server-side and answers a malformed call with a structured tool error, because neither pinned client enforces the advertised input schema — the schema is documentation plus daemon-side validation, never a trust boundary.

Rules that bind the whole plane:

- MarketRig builds **no subscription path and no completion path**, and does not advertise subscription capability: neither client issues either call, and neither negotiates the protocol revision that would carry server-side push. Explicit re-reads are the model.
- The seeded `AGENTS.md` names the resource URIs, because the prompt is the only discovery channel common to both runtimes — one lists eagerly but may never read, the other discovers lazily and may never look.
- Desk configuration accounts for the runtime's approval mode: on Codex, `tools/call` is approval-gated while resource reads are ungated, and a session configured never to ask for approval hard-blocks MCP tools entirely. The read path is therefore strictly cheaper than the tool path on that runtime, which is the shape of the split.
- A dead adapter or an unreachable daemon is an **explicit tool failure**, never a silent one and never an automatic retry of an uncertain action.
- The acceptance harness carries its own MCP client, because submit and cancel exist nowhere else (per D75).

The plane's concrete shape (per D78): `marketrig-mcp --desk <name-or-id>` binds to exactly one desk, discovers and verifies the daemon exactly as the CLI does (§4.3), resolving a name through the daemon's listing, and holds no credential beyond that handoff. It enumerates five aggregate resources — `marketrig://desk/<name>/quotes`, `…/book`, `…/positions`, `…/orders`, `…/instruments` — whose bodies are the REST routes they proxy, so the enumeration stays at five however the catalog grows, and it declares a zero cache lifetime on every read so no client default can stand in for freshness. Its two tools are `submit_order` and `cancel_order`; the daemon validates everything, and the one adapter-side check is a path-segment guard on the cancel's `client_order_id`, answered as `ORDER_INVALID`. A daemon refusal, an unreachable daemon, or a failed verification is a structured tool error carrying §4.3's envelope. The runtime adapters register it for every managed session (per D80, §6.4): on Codex through the `[mcp_servers.marketrig]` table MarketRig writes to `<workspace>/.codex/config.toml`, on Claude Code through the per-launch `mcp.json`, which names two servers — `marketrig`, this plane, and `marketrig-channel`, the same binary in `--channel` mode carrying delivery and no capability, so D4's split holds with two names on one runtime.

### 13.2 CLI continuity plane

The CLI carries everything else: `history`, `desk`, `trigger`, `memory`, `prompt`, and `session hook` — durable records, file-payload actions such as trigger code and recurrence rules, and cognition. Live positions, open orders, and instrument discovery belong to the market plane; the CLI's trading half is the durable record, including historical orders and closed position cycles.

The CLI is a thin client of `marketrigd` and never a generated SDK of it. It parses with `clap` and calls the authenticated loopback API with `ureq`, blocking, with finite connect and total timeouts, no environment proxy inheritance, and redirects disabled — both pinned exactly (per D50, D77). Commands are deterministic; `--json` produces machine output and plain text produces human output, carrying the same facts. Its text is English only and identical under every installation locale (§4.5).

A daemon error reaches the caller as §4.3's envelope and nothing else: `error: <CODE>: <message>` on standard error, or the envelope itself on standard output under `--json`. Exit codes are one small fixed set every command group inherits and none redefines: `0` success, `1` a daemon-reported error, `2` a usage error, `3` no usable daemon — diagnosed as `DAEMON_UNREACHABLE` in the same two shapes, because the CLI reports an unreachable daemon and never starts one (§4.3).

Every agent-facing command provides concise human-readable output, stable machine-readable output, non-secret provider and observation timing where applicable, and explicit failure rather than silent provider fallback.

Two rules constrain what the CLI may become. It never exposes a session lifecycle control — Interrupt, Exit, Start new, and Switch are REST and desktop actions, because the agent does not exit itself (per D69, §6.2). It never reads or writes a policy, lists approvals, or decides one (per D70, §4.4).

One command is deliberately none of the above, because it is not the agent speaking: `marketrig session hook` exists only because Claude Code reports a session event by running a command. It forwards the hook object from standard input to the daemon unchanged, carries no attribution, prints nothing, and exits `0` on every outcome — including no daemon and any rejection — so MarketRig's evidence can never become the agent's problem (per D69).

Global flags precede the group. Desk scope is explicit or resolved by the daemon from the caller's working directory; the CLI never derives a desk itself, and it resolves a desk name to its UUID through the daemon's own listing. Mutating requests carry the trigger environment as attribution, and nothing else is inferred. The `history` group is `marketrig [--json] history <orders|fills|cycles> <desk-name-or-id>`: name-or-id resolved through the daemon's listing, plain rows newest first, `--json` the route's body verbatim, exit codes unchanged (per D78). The `trigger` group is `marketrig [--json] trigger <create|list|show|update|enable|disable|delete|firings|firing> <desk-name-or-id> …` — `create` and `update` take `--name`, `--brief`, `--context`, one schedule shape (`--at`, or `--rrule --dtstart --tz`), and `--code <file>` with `--suffix`, `--arg`, `--timeout` (defaults: the file's extension, `{script}` alone, the daemon's); the `prompt` group is `marketrig [--json] prompt <list|show> <desk-name-or-id> …`; trigger names resolve through the daemon's per-desk listing, code files are read before the daemon is contacted, and single resources print `field: value` lines in the route's key order (per D79). The per-group grammar beyond `desk`, `history`, `trigger`, and `prompt`, pagination, and later groups' output shapes are **deferred** to the feature specifications that own each group.

## 14. Desktop and application lifecycle

*Decision basis: per D26, D29, D30, D33, D52, D55, D56, D57, D58, D59, D62, D66, D68, D69, D71, and D72.*

The desktop shell is Tauri 2. Its system webview hosts a Vue 3 and TypeScript 6 application built by Vite, styled with Tailwind CSS 4, using Reka UI 2 directly for behavior-heavy accessible primitives and native HTML for ordinary controls. Frontend builds use an exact tested Node.js 24 LTS release and a `packageManager`-pinned pnpm 11 release with its integrity hash, provisioned through Corepack; the committed lockfile is authoritative and none of these tools ship in the installer. The frontend pins the newest tested TypeScript 6.x compatible with the whole Vue and Vite toolchain, and exact Tailwind 4.x, Reka UI 2.x, and vue-i18n releases.

The frontend calls `marketrigd` directly over the authenticated loopback API, using the generated native-Fetch SDK for REST and the browser WebSocket API for terminal bytes and live events. Tauri's Rust layer owns only native desktop concerns — window, tray, single instance, daemon bootstrap through endpoint discovery, and the locale command — and performs no HTTP request of its own, proxies no API traffic, and holds no authoritative state (per D66). ghostty-web owns warm presentation only; the daemon-owned PTY or ConPTY remains the terminal and session authority.

The desktop follows a three-panel product model:

- left: durable desk navigation, with creation, retry, an attention indicator, and a pending-approval count;
- center: the attachable native terminal, with the session controls of §6.2;
- right: the selected desk's market and trading state, triggers, approvals, activity, and settings.

### Close main window

- hide the existing window to the tray without destroying its webview or unmounting the application;
- keep `marketrigd`, sessions, triggers, and managed children running;
- keep the authenticated connections and one bounded ghostty-web presentation per live managed terminal warm and consuming output.

### Reopen

- show and focus the existing warm window;
- preserve the selected desk and its terminal presentation;
- reconnect only if the connection was lost.

If the webview or its presentation state is destroyed, the daemon and the live terminal continue independently. The replacement frontend reconnects to live state, but exact reconstruction of the lost terminal screen is not an MVP guarantee: MarketRig persists no transcript and keeps no second emulator for that case (per D33).

### Quit MarketRig

- stop active agent processes, terminate active trigger-code process trees, close managed terminals and children;
- stop `marketrigd` through its own shutdown request and a bounded wait;
- exit the desktop and tray processes.

A second launch focuses the existing window rather than starting a second application.

State and presentation rules (per D71, D72):

- one live-event socket streams the installation's committed operational events with a client-held cursor; the daemon remembers no per-client cursor, and a client that must miss nothing sends the last row it saw and receives a gapless, duplicate-free replay;
- the socket only says which resource changed; the window refetches that resource and keeps no copy of daemon state beyond its cursor. No mutation updates a list optimistically and no control mirrors a daemon outcome into local state;
- a bounded per-connection queue is enforced by closing that one connection, never by slowing the producer — the same rule the terminal socket applies;
- the visual system is one token set with colour reserved for state roles plus one selection-and-focus accent, following the operating system's colour scheme with no theme switcher, and splitting the font stacks by provenance so that localized prose and machine tokens are visibly different things.

Onboarding offers OS-login autostart, enabled by default and configurable, and configures Hindsight's hosted models (§4.4). Autostart and notifications use Tauri's official plugins with narrowly scoped capabilities; notifications are reserved for actionable conditions — an agent waiting for input, a MarketRig approval, a daemon or runtime failure, a trigger failure — while routine results stay in desk history (per D52).

The REST surface, the live-event socket, the right panel's composition, the shell commands, and the design tokens' concrete values are **deferred** to the desktop feature specification; the per-desk terminal socket the desktop attaches to is §6.5's.

## 15. Persistence, crash recovery, and history

*Decision basis: per D22, D23, D36, D38, D45, D46, D51, D71, D73, D77, D78, D79, and D80.*

Durable state covers desk identity and configuration, runtime selection and last native pointers, triggers and code snapshots, daemon prompts and delivery, approvals and provenance, paper-book restoration state, and complete trading history. The current market-observation cache and the MCP adapter's state are deliberately non-durable: after a restart each quote stays unavailable until a new observation arrives.

MarketRig-owned structured state lives in one installation-wide SQLite database (per D46). Desk-owned rows are scoped by durable desk identity; installation-owned rows are not. Agent-owned workspace content stays ordinary files. The database is `marketrig.sqlite3` under `%LOCALAPPDATA%\MarketRig` on Windows and `~/Library/Application Support/MarketRig` on macOS, beside the `runtime` subdirectory that holds the daemon lifetime lock, `runtime/endpoint.json`, and `runtime/children.json`.

Storage conventions (per D45), stated explicitly because the ecosystem's gravity pulls toward an ORM and gravity is not evidence of need:

- SQLite is reached through a thin binding that keeps plain SQL in view (`rusqlite`, pinned exactly, with SQLite compiled into the daemon so binary and schema stay one release unit — per D53, D77);
- `marketrigd` is the sole writer, and persistence stays cohesive and private to it;
- transactions are explicit `BEGIN IMMEDIATE`; the journal mode is WAL; tables are `STRICT` with enforced foreign keys;
- MarketRig-owned durable identifiers are lowercase UUIDv7 text, and source-native identities stay canonical text;
- UTC instants are Unix nanoseconds in `*_ns` integer columns;
- authoritative financial values are canonical decimal **text** with their precision or currency, never a `REAL` column and never a float;
- opaque source payloads stay versioned bytes beside the normalized fields;
- no ORM, no query builder, no compile-time query-checking layer, and no alternate-storage interface for one implementation.

Schema evolution uses numbered forward-only migrations. The daemon embeds its ordered migration list, applies whatever is pending at startup before recovery and before serving, and records the applied number in SQLite's own `PRAGMA user_version`: no bookkeeping table, no external migration file, and no down migration. A database whose `user_version` exceeds the binary's newest migration is rejected with `DATABASE_NEWER` and the daemon refuses to start; there is no downgrade path and no alternate database location. One connection is owned by one database thread that accepts submitted units and read functions; no connection, cursor, or transaction handle escapes it, and callers that must be atomic across modules submit one unit rather than composing several. The daemon relocates its whole root through one test seam (§17), which is load-bearing for both developer safety and acceptance evidence.

Migration 2 (per D78) adds the trading tables named in §12.4 and widens `operational_events.kind` with `TRADING_NODE_STARTED` and `TRADING_NODE_FAILED`. SQLite cannot alter a `CHECK`, so a kind widening is a table rebuild — create the widened `STRICT` table, copy, drop, rename, recreate the index — and every later widening repeats that pattern. Migration 3 (per D79) adds `code_snapshots`, `triggers` (with the projection column, a partial unique index on live names, and the partial due index), `firings` (unique per desk, trigger, and occurrence), and `executions` (keyed by firing, `RUNNING | COMPLETE`, the outcome and streams), rebuilds `prompts` for `TRIGGER_RESULT`, `trading_actions` for `source TRIGGER` with `trigger_id` and `firing_id`, and `operational_events` for `TRIGGER_MISSED`. Trigger deletion is soft — `deleted_at_ns`, projection `NULL`, hidden from listings, history readable — because firings reference their trigger. Migration 4 (per D80) adds `desks.selected_runtime` (`codex | claude`, default `codex`), `runtimes` (§4.4, seeded with two `UNDISCOVERED` rows), `native_sessions` keyed by desk and runtime, and `agent_processes` — one row per managed process with runtime, native session, pid, daemon UUID, `started_at_ns`, `ready_at_ns`, `ended_at_ns`, `exit_reason`, and `exit_code`, `ended_at_ns` and `exit_reason` null together, and a partial unique index on `desk_id` over open rows so a desk has at most one live process; rebuilds `prompts` with `state IN ('QUEUED','DELIVERED','FAILED')`, the kinds `ORIENTATION` and `DISCLOSURE`, and the attempt columns `attempted_at_ns`, `resolved_at_ns`, `runtime`, `native_session_id`, `failure_code`, and `disclosed_at_ns` (`resolved_at_ns` set exactly when the state is not `QUEUED`, `failure_code` exactly when it is `FAILED`), carrying every existing row; and widens `operational_events.kind` with `RUNTIME_DISCOVERED`, `RUNTIME_UNAVAILABLE`, `CONTROL_PLANE_STARTED`, `CONTROL_PLANE_LOST`, `SESSION_STARTED`, `SESSION_READY`, `SESSION_POINTER_CHANGED`, `SESSION_ATTENTION`, `SESSION_TURN_ENDED`, `SESSION_INTERRUPTED`, `SESSION_EXITED`, `PROMPT_DELIVERED`, `PROMPT_FAILED`, and `RUNTIME_SWITCHED`.

Operational evidence has one home: the installation-wide append-only `operational_events` table. Every row commits inside the transaction of the change it evidences, its `kind` is a closed vocabulary that each milestone extends by migration, and its `(occurred_at_ns, id)` order is the cursor the live-event socket tails (per D71). There are no per-domain event tables.

MarketRig history is structured operational evidence, not a conversation archive: native session pointers and managed-process lifecycle, trigger execution and delivery, trigger-code diagnostics, approvals and actions, and the immutable sandbox-produced trading facts with their provenance. Raw trading facts stay retained even when MarketRig builds normalized projections. MarketRig persists no canonical decision trajectory, no normalized transcript, and no Run history; Codex and Claude Code own their own session catalogs.

Diagnostic logs are separate from that authoritative history. `marketrigd` writes bounded JSON Lines through `tracing`, pinned exactly, rotating daily and keeping the newest seven files, and the desktop writes a separate bounded local file through Tauri's official log plugin. Both use the operating system's application-log directory, separate from the application-data root that holds the database. The daemon additionally emits to standard error when it is a terminal. Nothing is uploaded automatically, and no log contains a provider secret or a loopback credential (per D51).

After an unclean daemon or process failure:

- no orphaned terminal or in-flight trigger-code process is considered resumable;
- no affected agent process or terminal stays live, while its last native session pointer is retained;
- in-flight code or delivery becomes failed or unknown as the persisted evidence supports;
- no execution or delivery is retried automatically;
- future activation follows the ordinary resume-first path.

Recovery is one pre-service transaction owned by the new daemon start. Each module registers its step, the transaction runs the registered steps in one fixed order, and it appends one recovery event naming the previous and new daemon UUIDs on every start, including a clean one with nothing to resolve. Never-attempted prompts stay queued; attempts without confirmed handoff completion fail as completion-unknown; completed handoffs stay delivered; uncertain trading actions are never blindly resubmitted; in-flight rows identify the daemon that started them. The executions step (per D79) completes every `RUNNING` execution of another daemon as `DAEMON_LOST` with its result prompt and lists them in the recovery event under `executions_lost`; a firing accepted but never claimed stays pending and is claimed by the new daemon in FIFO order. The sessions step (per D80), registered after it, closes every open `agent_processes` row of another daemon `DAEMON_LOST` with one `SESSION_EXITED` each and fails every prompt whose attempt began but was never resolved `HANDOFF_UNKNOWN`, listing both under `sessions_lost` and `prompts_unknown`; `runtime/launch/` is cleared at every start, and the Codex app-server child is the `children.json` step's below.

Long-lived children a crashed daemon left running are recorded in `runtime/children.json` when launched and removed when stopped. Recovery's first step terminates any recorded child of a prior daemon whose process is alive and whose command line still carries the recorded arguments, drops every record either way, and reports each outcome — terminated, not running, pid recycled, or discarded — in the recovery event. A record carrying no arguments carries no identity evidence, so it never matches and is never terminated. Windows discards records only, because its Job Object has already ended the children. `runtime/children.json` is removed only after the recovery transaction commits, so a failed commit keeps its records as evidence for the next start. Nothing is recorded for trigger scripts (per D73).

## 16. Memory and skills

*Decision basis: per D16, D17, D18, D19, D21, D22, D47, D49, and D65.*

Each desk self-improves through the agent-owned Evaluate and Learn stages of the modified OODA loop:

```text
inspect authoritative trading history
-> choose relevant evidence
-> judge the outcome using realized P&L as reward
-> retain desk-specific lessons in Hindsight when useful
-> improve reusable procedures in desk skills when useful
-> return to Observe
```

- `marketrigd` owns one installation-wide local Hindsight instance, run as one supervised loopback child process on the bundled interpreter and pinned wheel environment (per D47, D65), authenticated with a per-start bearer credential, with its own MCP server and telemetry disabled and one automatic restart. Its embedded database is Hindsight-managed under a MarketRig-named instance.
- The instance serves every desk; each desk maps to exactly one isolated bank derived from its UUID and provisioned lazily. Neither the CLI nor the agent ever supplies a bank identifier.
- Agents use only desk-scoped `marketrig memory` status, retain, recall, and reflect: synchronous desk-scoped pass-throughs carrying trigger attribution metadata, with no MarketRig-side content store.
- MarketRig does not install Hindsight's runtime-native integrations, automatically retain transcripts, or expose Hindsight's direct API, MCP server, or control plane to agents.
- Hindsight's LLM and embedding models are hosted behind one OpenAI-compatible endpoint; reranking uses Hindsight's rerank-free fusion ordering, and the embedding model locks once data is initialized with it (per D18).
- The child receives its endpoint key and the daemon's bearer through its own process environment — the one recorded exception to the credential boundary (§4.4, per D49) — and its embedded database runs on default loopback credentials, an open ceiling with a written upgrade path.
- Each desk has its own `AGENTS.md` carrying the loop and the ownership boundary as always-loaded context, and one canonical skill set exposed compatibly to both runtimes within that desk only (§5.1).
- MarketRig seeds an on-demand improvement skill and then the agent owns and may evolve it. The agent chooses what to retain, recall, reflect on, create, or change.
- MarketRig queues Evaluate for every realized-P&L event and never automatically writes a memory or mutates a skill. It requires no reflection cadence and no special reflection session (per D22).
- Authoritative reality always stays in MarketRig and sandbox state (per D17).
- Hindsight startup or provider failure is reported as an explicitly unavailable memory capability. It never blocks daemon startup, native sessions, triggers, or paper trading, and MarketRig never silently substitutes another memory system or model.

The exact dependency pin, child and database lifecycle, bank derivation, settings schema, command set, and error codes are **deferred** to the memory feature specification.

## 17. Verification

*Decision basis: per D60, D61, D67, D75, D76, D77, D78, and D80.*

Verification has three layers, and no layer restates another:

```text
feature required checks   unit and module, fakes allowed, per commit
acceptance gate           the scenario chain, unattended and deterministic, on stand-ins
acceptance experiment     the same chain, operator-attended, on the real components
```

Each feature specification ends with a **Required checks** section, and its implementation plan turns those into runnable tests before the work is considered done. A failure the experiment finds gets its regression in the gate or in a module check, never a restatement.

The **acceptance chain** is one ordered sequence of scenarios that asserts the roadmap's evidence lines and nothing a feature check already proves. It grows scenario by scenario as milestones land, so no chain waits on a final harness. The acceptance feature publishes an explicit **scenario-to-check mapping**, so every scenario the chain claims has a named check and none is dropped silently.

**The gate** is unattended and deterministic. The daemon, the CLI, the database, the NautilusTrader sandbox, and every public surface are real; the sandbox is driven by the milestone's deterministic stand-in quote source where one exists (per D76) and by live public data where it does not; and the runtime CLIs and the memory child are the harness's stand-ins — `runtime-standin`, which passes both runtimes' probes and speaks exactly the protocol subset the adapters consume, scripted per launch, and a fake memory child. The chain therefore proves MarketRig's mechanics with no model, no key, no first-run dialog, and no operator.

**The experiment** is the operator-attended run of the same chain on the real Codex CLI or Claude Code and real Hindsight behind a real hosted endpoint, once per release candidate per platform-and-runtime cell, whose bundle is archived as the evidence that a real agent closes the loop. Every native session start is operator-attended through a terminal relay so runtime dialogs are answered by a person.

Rules both modes share (per D67, D75):

- the chain drives **public surfaces only** — the CLI's machine output, the loopback API, the desk's MCP surface through the harness's own MCP client, the per-desk terminal socket, workspace files, and read-only SQLite. There is no test-only product surface;
- the harness is one internal workspace crate of its own, carrying the gate and the experiment as two test targets and spawning the shipped binaries rather than linking the daemon, so the public-surfaces-only rule is structural rather than a discipline (per D77);
- both modes relocate the whole data root through the daemon's test seam into the run's evidence directory, so the daemon's log, the desk workspaces, and every socket close code land in the bundle by construction and no run touches the per-user root. The packaged desktop smoke is the one leg that does;
- the environment seam is three variables: `MARKETRIG_TEST_DATA_ROOT` relocates the root; `MARKETRIG_TEST_NO_TRADING` additionally keeps a daemon off the public market feed; and `MARKETRIG_TEST_QUOTE_URL`, honored only alongside the first and outranking the second, points the equity feed at a harness-owned stand-in speaking the chart endpoint's shape with scripted prices, under which the poller treats every phase as `OPEN` while observations keep labeling the real one (per D78). The daemon never reads the third otherwise, and no configuration surface can change the feed URL. `marketrigd` and `marketrig` are never run by hand without the first;
- agent-owned steps are instructed by a user-owned `AGENTS.md` addendum and verified by side effects, with bounded attempts on fresh trade cycles; mechanical assertions get none, because MarketRig must not decide what the agent should learn while still making the learning steps executable;
- mechanical scenarios fail the cell, while the assertions that wait on the agent to act end as **inconclusive** with their evidence, never as a product defect, and the operator decides whether to rerun;
- desk names are run-stamped, the harness deletes nothing, and each cell produces an evidence bundle;
- the harness ships one helper binary, `trigger-code`, that the gate names as every code-bearing trigger's executable and drives through the snapshot's one-line source — print the environment and document, order twice through the real adapter, exit, sleep, flood — so trigger scenarios run the real executor, adapter, and attribution path with no interpreter the CI image might lack (per D79). G21–G26 extend the chain after G20; E3 is the attended scenario in which the real session writes its own trigger code;
- the harness ships a second helper binary, `runtime-standin`, that the gate registers by explicit path as both runtimes and scripts per launch through the one JSON file `MARKETRIG_STANDIN_SCRIPT` names on the daemon's environment (per D80). As Codex it serves the app-server's `initialize`, `thread/start`, `thread/resume`, `thread/turns/list`, `turn/start`, and `turn/interrupt` behind the capability token and the four broadcasts, and its TUI half connects with `--remote`; as Claude Code it honors `--session-id`, `--resume`, `--mcp-config`, `--settings`, and the channel flag, spawns the listed stdio servers, runs the hooks, and sends `initialized` only after a delay so a bridge that connected early would be caught. Both halves echo every delivered input and one scripted MCP read to their terminal, which is how the gate reads delivery order and the registration takeover. G27–G32 extend the chain after G26 — discovery, trigger-fires-nobody-home on Codex, FIFO behind a turn then resume, the Claude half after a switch, activation failure and disclosure with one control-plane loss, and a hard kill mid-attempt; E4 is the attended scenario in which MarketRig launches the real runtime itself, the operator's console *is* the desk's terminal, and a scheduled result lands as the session's own input.

Tooling (per D60, D61): `cargo test` for every Rust crate — daemon, CLI, MCP adapter, and Tauri shell — with both acceptance modes as test targets in the same workspace; Vitest with Vue Test Utils 2 and jsdom for the Vue and TypeScript frontend; and WebdriverIO with its Tauri service for packaged desktop flows, whose test-only plugins are excluded from release artifacts. Static checks are standard rustfmt and Clippy with `-D warnings` for every Rust crate, and Prettier plus correctness-only ESLint and `vue-tsc` for the frontend. All of it runs through ordinary pnpm and Cargo scripts and CI, without a commit-hook framework. CI runs on both MVP platforms; the packaged smoke and the attended experiment stay operator-run.

Realized P&L is the reward signal. Profitability, strategy quality, and beneficial learning are not acceptance criteria: acceptance proves the mechanical loop and durable reuse.

## 18. Implementation-deferred contracts

The following stay intentionally unresolved until their dedicated feature sessions. Every "deferred" in this document points here. These are module-specific design questions, never permission to invent additional product entities or a universal agent protocol.

- WebSocket framing, REST mechanics beyond the endpoint-discovery contract and the error envelope (§4.3), and the wiring of the daemon's OpenAPI emission from its chosen emitter;
- exact packaging layout, launch shims, code signing and notarization, per-user CLI PATH registration, autostart launch arguments and recovery, and notification interaction details;
- the exact crate selections and pins still named as candidates in §4.4, §9, and §10, each confirmed against current documentation at plan time and pinned in the workspace manifest; §4.3's web framework and §15's SQLite binding and logging facade are pinned already (per D77);
- domain-owned SQLite table details for each milestone, including EVENT ingress; approvals own no table of their own, because the code-snapshot and trading-action rows are the records;
- CLI invocation grammar beyond the `desk`, `history`, `trigger`, and `prompt` groups, pagination, and later groups' output shapes;
- EVENT connector framing, occurrence-identity construction, payload limits, deduplication storage and index mechanics, filtering, and execution backpressure;
- OpenBB research as a whole — its supervised child, connectors, secret references, and credential flows — deferred past MVP on scope (per D9);
- exact localized copy for surfaces later milestones add, renderer-loss recovery details, detailed interaction design, and the design tokens' concrete values;
- notification platform mechanics;
- Hindsight bank profile tuning, hosted reranker configuration, embedding-model change, and data erasure and relocation workflows;
- operational-record retention and pruning, diagnostic-log retention beyond the daemon log's rotation bound (§15), and record fields and export mechanics;
- backpressure policy for the daemon-prompt queue, until EVENT volume makes it necessary;
- pre-trusting seeded desk workspaces in the runtimes' own configuration at provision time, so a desk whose first-ever session is a dispatcher activation does not stall on a trust dialog and lose that firing's prompt;
- the seeded `AGENTS.md` constitution's wording, grown by the milestones that add the surfaces it names (per D20, D22).
