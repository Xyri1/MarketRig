# R3 — Runtime adapters and delivery: Feature SPEC

*Decision basis: per D3, D24, D25, D27, D28, D31, D32, D36, D63, D69, D70, D71 and this feature's R3-1 … R3-8.* This document refines root `sdd/SPEC.md` §4.4, §6, §7, §11, §13.1, §15, and §17. Where it names a runtime flag or protocol method, the fact was verified on 2026-09-02 against Codex CLI 0.152.1 and Claude Code 2.1.258.

## 1. Workspace additions

- `crates/marketrigd/src/runtime.rs` (discovery, the `runtimes` rows), `terminal.rs` (R3-2), `codex.rs` (R3-3), `claude.rs` (R3-4), `dispatch.rs` (R3-5), `session.rs` (R3-6 routes and the `agent_processes` unit helpers); `store/004_r3.sql`.
- `crates/marketrig-mcp` gains the `--channel` mode (R3-4); `crates/marketrig` gains `session hook` (root §13.2).
- `crates/marketrig-acceptance/src/bin/runtime-standin.rs` (R3-8).
- New pins at plan time: `portable-pty =0.9.0`; axum's `ws` feature and the `tokio-tungstenite` line it resolves, used both by the daemon's two sockets and by the app-server client; the already-in-graph `libc` (unix target) and `windows` (windows target) named as marketrigd dependencies for the terminal manager's process-tree termination; `futures-util =0.3.34` (already in the graph under `tokio-tungstenite` and `rmcp`) for the `StreamExt` the WebSocket clients read with. No other crate.

## 2. Runtime discovery (R3-1)

`runtimes` starts with two `UNDISCOVERED` rows. Discovery is one function `discover(runtime, explicit: Option<PathBuf>) -> Outcome`, run by startup step 6a (after desks complete, before binding) for every `UNDISCOVERED` row, and by `POST /runtimes/{runtime}/discover` and `/retry`. Startup step 6a is skipped under `MARKETRIG_TEST_DATA_ROOT` (root §17): the gate registers its own stand-in explicitly and must never see the operator's real installations.

Resolution without an explicit path:

| Platform | Source of `PATH` | Accepted launchers |
| --- | --- | --- |
| macOS | `$SHELL -l -c 'printf %s "$PATH"'`, 10 s timeout, captured once per daemon start, in memory only; the daemon's own `PATH` when the shell fails | Mach-O executable |
| Windows | `HKCU\Environment\Path` + `HKLM\…\Session Manager\Environment\Path`, read fresh each discovery | `.exe` directly; `.cmd`/`.bat` through `%ComSpec% /d /c` |

Validation: `<executable> --version` with a 10 s timeout; the first `\d+\.\d+\.\d+` on standard output is the version; floors Codex `0.152.1`, Claude Code `2.1.258`. Then `--help` must contain `app-server` (Codex) or both `--dangerously-load-development-channels` and `--settings` (Claude). Outcome `AVAILABLE` writes path, version, `validated_at_ns`, clears the failure, and appends `RUNTIME_DISCOVERED {runtime, executable_path, version}`; any failure writes `UNAVAILABLE` with `failure_code IN ('NOT_FOUND','VERSION_UNSUPPORTED','CAPABILITY_MISSING','PROBE_FAILED')` and appends `RUNTIME_UNAVAILABLE`.

Scenarios:

- **Explicit path wins.** `POST /runtimes/codex/discover {"executable": "/x/codex"}` validates `/x/codex` and never consults `PATH`; a relative path is `400 VALIDATION` (root §4.3 envelope).
- **Bundled desktop Codex is not a target.** A resolved path under a `Codex.app` or `ChatGPT.app` bundle on macOS is `CAPABILITY_MISSING` with the message naming the bundle (per D3).
- **Version floor.** `codex --version` printing `codex-cli 0.150.1` is `VERSION_UNSUPPORTED` and the row stays launchable-never until re-discovery.

## 3. Terminal manager (R3-2)

```text
spawn(desk_id, command, cwd, env, size) -> Terminal { generation: 0, ring: 256 KiB }
attach(desk_id) -> generation n+1; previous attachment closed 4001
write(desk_id, generation, bytes) -> rejected silently when generation is stale
resize(desk_id, generation, cols, rows) -> coalesced; stale generation ignored
shutdown(desk_id) -> stop input, drain ≤ 2 s, terminate tree (R2 primitive), join reader
```

`GET /desks/{desk_id}/terminal` (bearer as every route; the `Sec-WebSocket-Protocol` header is not used) answers `404 DESK_NOT_FOUND`, `409 NO_LIVE_SESSION` when no terminal exists, and `400 VALIDATION` when the request is not a WebSocket upgrade — all three before any attachment is taken, so a request that never upgrades cannot supersede a live one — else upgrades. Frames: binary = bytes; text = `{"resize":{"cols":n,"rows":n}}` from the client, `{"exited":{"reason":…,"code":…}}` from the server followed by close `1000`. The ring is replayed as one binary frame before live bytes. The terminal manager owns no process record: `agent_processes` rows are the adapters' (R3-6).

Scenarios:

- **Superseded attachment.** Two attachments; the first receives close `4001`; input on the first is dropped; the second sees the ring then live bytes.
- **Slow consumer.** A client that never reads does not block the child: the sender drops the attachment after its 1 MiB send buffer fills (the socket is then closed `4001`, as for a supersede) and the ring keeps the newest 256 KiB.
- **Exit frame.** The child exiting sends `exited` with `reason: "EXITED"` and the code; MarketRig's Exit sends `reason: "INTERRUPTED"`.

## 4. Codex adapter (R3-3)

### 4.1 Control plane

Started lazily by the first Codex activation: bind `127.0.0.1:0`, take the port, release, write a fresh capability token 0600 to `runtime/codex-ws-token`, spawn `<codex> app-server --listen ws://127.0.0.1:<port> --ws-auth capability-token --ws-token-file <that path>` under the R2 containment primitive with cwd the data root, record it in `runtime/children.json`, connect within 15 s (retrying every 250 ms) with an `Authorization: Bearer <token>` header, send `initialize` (`clientInfo {name: "marketrigd", version}`) then `initialized`, append `CONTROL_PLANE_STARTED {runtime: "codex", pid, port}`. The connection reads broadcasts only:

| Broadcast | Use |
| --- | --- |
| `thread/started` | pointer discovery: first non-`ephemeral` thread whose `cwd` is the launching desk's workspace; its `thread.status` is that thread's first known status (spike S: a new session's only `idle` arrives here) |
| `thread/status/changed` | `idle` → ready and delivery gate open; `active` → gate closed; `systemError` → `SESSION_ATTENTION {kind: "system_error"}`; `notLoaded` → gate closed |
| `thread/closed` | gate closed; no exit (the process row closes on process end) |
| `error` | logged; a `threadId`-bearing error for a desk's thread appends `SESSION_ATTENTION {kind: "error"}` |

Loss: the child exiting or the socket closing ends every Codex `agent_processes` row with `CONTROL_PLANE_LOST` (terminals shut down, pointers kept), appends `CONTROL_PLANE_LOST`, and retries the start once; a second failure sets `runtimes.codex.state = UNAVAILABLE` with `CONTROL_PLANE_FAILED` until `POST /runtimes/codex/retry`, which clears it and tries again on the next activation. Quit stops the app-server after every terminal.

### 4.2 Sessions

Before either launch the daemon writes `<workspace>/.codex/config.toml` with `[mcp_servers.marketrig]` `command = "<marketrig-mcp>"`, `args = ["--desk","<desk-id>"]`, and `env = {MARKETRIG_TEST_DATA_ROOT = "…"}` (the env key only under the test seam); `-c mcp_servers.*` on the TUI command line does not reach the remote thread (spike S). New: `<codex> --remote ws://127.0.0.1:<port> --remote-auth-token-env MARKETRIG_CODEX_WS_TOKEN -C <workspace>`. Resume: `<codex> resume <thread-id> --remote … -C <workspace>`, same flags. Environment: the captured login `PATH`, `HOME`, `TERM=xterm-256color`, `LANG` and `LC_*` from the daemon, `MARKETRIG_DESK_ID`, and `MARKETRIG_CODEX_WS_TOKEN` (the app-server capability token, the one secret the TUI needs to reach its own control plane); nothing else (no daemon bearer). Pointer: `native_sessions[desk, codex] = thread.id` on `thread/started`; a resume already carries it, and a resume whose TUI exits before readiness is unresumable (R3-5) — the 120-second activation deadline (§6.1) is the only clock. Ready: the first time the pointer's thread is `idle` → `agent_processes.ready_at_ns`, `SESSION_READY`. Status comes from either broadcast, which is what makes one rule cover both launches: a new session's `thread/started` carries `status: idle` and no `thread/status/changed` follows it, while a resume emits no `thread/started` at all and reaches `idle` through `thread/status/changed` (`notLoaded` → `idle`), both observed on Codex 0.152.1 on 2026-09-03.

### 4.3 Delivery and interrupt

Deliver(prompt): precondition last status `idle`; `thread/turns/list {threadId, limit: 1}` — an item whose `status` is active closes the gate and the prompt waits; else `turn/start {threadId, input: [{type:"text", text}]}`. Outcome mapping is R3-3's. The delivered text is §6.3's rendering.

Interrupt: `thread/turns/list`; no active turn → 409 `NO_ACTIVE_TURN`; else `turn/interrupt {threadId, turnId}` → `202` with the response, `SESSION_INTERRUPTED {turn_id}`; an app-server error → 502 `RUNTIME_ERROR` carrying the JSON-RPC error message.

## 5. Claude Code adapter (R3-4)

### 5.1 Launch files

`runtime/launch/<desk-id>/mcp.json` (0600):

```json
{"mcpServers":{
  "marketrig":         {"command":"<marketrig-mcp>","args":["--desk","<desk-id>"],"env":{…seam…}},
  "marketrig-channel": {"command":"<marketrig-mcp>","args":["--desk","<desk-id>","--channel"],"env":{…seam…}}}}
```

`runtime/launch/<desk-id>/settings.json` (0600): `{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"<marketrig> --desk <desk-id> session hook"}]}],"Notification":[…same…],"Stop":[…same…]}}`. `<marketrig>` is the CLI beside the daemon binary; when it is missing the settings file is omitted and the launch carries no hooks (per D69). Both files are deleted when the process row closes.

Launch: `<claude> --session-id <uuid> --mcp-config <mcp.json> --settings <settings.json> --dangerously-load-development-channels server:marketrig-channel` (new; `uuid` = UUIDv4 minted by the daemon) or `<claude> --resume <uuid> …same flags` (resume), cwd the workspace, environment as §4.2 plus `MARKETRIG_DESK_ID`. The pointer is written at spawn for a new session and confirmed by `SessionStart {source: "startup"}`; for a resume it is confirmed by `SessionStart {source: "resume"}` with the same `session_id`; a process that exits before either, or a `SessionStart` for a different `session_id` on a resume, is unresumable.

### 5.2 Hook ingress

`marketrig --desk <desk-id> session hook` reads standard input to EOF (≤ 64 KiB), posts it unchanged to `POST /desks/{desk_id}/session/hook`, and exits `0` regardless of outcome, printing nothing. The route (bearer as usual — the CLI discovers the endpoint like any command) accepts a JSON object and records by `hook_event_name`:

| Event | Effect |
| --- | --- |
| `SessionStart`, `source: startup\|resume` | pointer confirmation; readiness is §5.3's |
| `SessionStart`, `source: clear` | `native_sessions` repointed to `session_id`; `SESSION_POINTER_CHANGED {from, to, cause: "clear"}` |
| `SessionStart`, other `source` | recorded as `SESSION_ATTENTION {kind: "session_start", source}` only |
| `Notification` | `SESSION_ATTENTION {kind: notification_type, title}`; `message` is not stored |
| `Stop` | `SESSION_TURN_ENDED` |
| anything else | `202` and no row |

A hook whose `session_id` is neither the desk's pointer nor a `clear` transition is `202` with `SESSION_ATTENTION {kind: "foreign_session"}`; the route never answers non-2xx to a well-formed object, and an unparseable body is `400` that the CLI still swallows.

### 5.3 Channel and delivery

`GET /desks/{desk_id}/channel` upgrades to a WebSocket for the bridge; one connection per desk, a second closes the first `4001`. The bridge's connection is readiness (`ready_at_ns`, `SESSION_READY`) provided the process row is open; a connection with no open process is closed `4002`. Deliver(prompt): send one text frame `{"content": <§6.3's rendering>, "prompt_id": …, "kind": …}` — the meta rides in the frame because the bridge republishes it and never sees the daemon's rows; write completion → `DELIVERED`; no connection → wait 30 s for one, then `FAILED CHANNEL_UNAVAILABLE`; write error → `FAILED CHANNEL_UNAVAILABLE`. The bridge (`marketrig-mcp --desk <id> --channel`) serves stdio MCP with `experimental: {"claude/channel": {}}`, `instructions` = one sentence naming the source and that each event is a MarketRig prompt to act on, and forwards each frame as `notifications/claude/channel {content, meta: {prompt_id, kind}}`, taking all three from the frame and dropping a frame that is not that object; it exits when the socket closes or standard input ends.

Interrupt → 409 `INTERRUPT_UNSUPPORTED` before anything is touched.

## 6. Activation and the dispatcher (R3-5)

### 6.1 The task

```text
loop: wait(notify | 30 s)
  for desk in desks with QUEUED prompts, oldest prompt first per desk:
    match live process for desk:
      None            -> activate(desk)            (once per pass; other desks continue)
      Some(not ready) -> continue
      Some(ready)     -> adapter.deliver(head)     (one prompt per desk per pass)
```

`activate(desk)`: runtime = `desks.selected_runtime`; its `runtimes` row must be `AVAILABLE`, else every `QUEUED` prompt of the desk → `FAILED RUNTIME_UNAVAILABLE` now. Pointer present → resume; absent → new. In one unit: insert `agent_processes` (`ready_at_ns NULL`), append `SESSION_STARTED {runtime, mode: RESUME|NEW, native_session_id}`, and for a new session insert `ORIENTATION` (and `DISCLOSURE` if undisclosed `FAILED` rows exist) with `created_at_ns` = the unit's instant minus 1 ns and 2 ns so they head the FIFO. Readiness deadline 120 s from spawn.

### 6.2 Outcomes

| Event | Effect |
| --- | --- |
| ready within deadline | `SESSION_READY`; delivery proceeds |
| resume unresumable (§4.2/§5.1), dispatcher path | end process `EXITED`, `SESSION_POINTER_CHANGED {from, to: null, cause: "unresumable"}`, start new once |
| resume unresumable, explicit Continue | end process `EXITED`; the route answered `202` already; evidence only, no new session |
| deadline passed or exit before ready | end process (reason per evidence), every `QUEUED` prompt of the desk → `FAILED ACTIVATION_FAILED` with `failure_detail` (exit code or `"timeout"`) |
| process exits while prompts are queued | prompts stay `QUEUED`; the next pass activates again |

Delivery attempt unit: set `attempted_at_ns`, `runtime`, `native_session_id`; then the adapter call; then a second unit writing `state`, `resolved_at_ns`, `failure_code`, and `PROMPT_DELIVERED {prompt_id, kind, runtime, native_session_id}` or `PROMPT_FAILED {…, failure_code}`. Failure codes: `DELIVERY_REFUSED`, `HANDOFF_UNKNOWN`, `CHANNEL_UNAVAILABLE`, `ACTIVATION_FAILED`, `RUNTIME_UNAVAILABLE`.

### 6.3 Renderings

Every prompt is one English text. `TRIGGER_RESULT` and `EVALUATION` render their stored payload as a fenced JSON block preceded by one line naming the kind and prompt id (`MarketRig TRIGGER_RESULT <id>:`). `ORIENTATION` is root §7's paragraph, filled with desk name, workspace path, `AGENTS.md` path, the `marketrig` command, and the five resource URIs. `DISCLOSURE` lists each undisclosed failed prompt as `<id> <kind> <failure_code>` on its own line, with content never included; delivering it sets `disclosed_at_ns` on those rows in the same unit. Rendering is byte-identical under both locales.

Scenarios:

- **Trigger fires, nobody home.** Firing accepted (R2) → `TRIGGER_RESULT` `QUEUED` → the notify wakes the dispatcher → activation → ready → `DELIVERED`; rows: `SESSION_STARTED`, `SESSION_READY`, `PROMPT_DELIVERED` in that order, `agent_processes.native_session_id` = the pointer.
- **Queued behind a turn (Codex).** Two prompts; the stand-in reports `active` after the first `turn/start`; the second is delivered only after the next `idle`, and its `resolved_at_ns` is after that broadcast.
- **Queued behind a turn (Claude).** Two prompts are two frames in order; both `DELIVERED` on write; the stand-in's echo shows both `<channel>` events in FIFO order at its next turn.
- **Uncertain handoff.** The app-server socket drops between `turn/start` and its response → `FAILED HANDOFF_UNKNOWN`; the prompt is listed by `marketrig prompt list` with that code and is never resubmitted.
- **Disclosure once.** After that failure, the next new session's `DISCLOSURE` names it; a third session has nothing to disclose.

## 7. Session controls (R3-6)

| Route | Body | Answers |
| --- | --- | --- |
| `GET /desks/{d}/session` | — | `{process: null}` or `{process: {id, runtime, native_session_id, pid, started_at_ns, ready_at_ns}}` |
| `POST /desks/{d}/session/activate` | `{"mode":"CONTINUE"\|"NEW"}` | `202 {process}`; `409 SESSION_LIVE`, `409 NO_NATIVE_SESSION`, `409 RUNTIME_UNAVAILABLE` |
| `POST /desks/{d}/session/interrupt` | — | `202 {turn_id}`; `409 NO_LIVE_SESSION`, `409 NO_ACTIVE_TURN`, `409 INTERRUPT_UNSUPPORTED`, `502 RUNTIME_ERROR` |
| `POST /desks/{d}/session/exit` | — | `202 {}` after the row closes `INTERRUPTED`; `409 NO_LIVE_SESSION` |
| `POST /desks/{d}/session/switch` | `{"runtime":…}` | `200 {selected_runtime, pointers}`; `400 VALIDATION`, `409 RUNTIME_UNAVAILABLE`, `409 SAME_RUNTIME` |
| `GET /runtimes` | — | both rows, secrets-free |
| `POST /runtimes/{r}/discover` | `{"executable"?: path}` | `200 {row}` whichever state results |
| `POST /runtimes/{r}/retry` | — | `200 {row}` |

`POST /desks` gains optional `"runtime"` (default `codex`); an unknown value is `400 VALIDATION`. `GET /desks/{d}` reports `selected_runtime` and `native_sessions` (an object keyed by runtime, `{}` when there is no pointer). A `{runtime}` segment naming neither runtime is `404 RUNTIME_NOT_FOUND`. `exit` and `switch` wait up to 5 s for the process row to close before answering; a process that survives termination that long answers `502 RUNTIME_ERROR` with the row still open and the shutdown continuing. Quit closes every open row `QUIT`.

Scenarios:

- **Switch keeps everything.** Codex desk with a thread pointer, one closed cycle, one resting order; `switch {claude}` → `RUNTIME_SWITCHED`, `selected_runtime = claude`, the Codex pointer still present, history and book unchanged, no process; `activate {NEW}` starts Claude in the same workspace.
- **Refused switch costs nothing.** `switch {codex}` while `codex` is `UNAVAILABLE` → `409` and the live Claude process is untouched.
- **Interrupt on Claude** → `409 INTERRUPT_UNSUPPORTED`, no event, no process contact.

## 8. Durable schema (migration 4, R3-7)

```sql
ALTER TABLE desks ADD COLUMN selected_runtime TEXT NOT NULL DEFAULT 'codex'
  CHECK (selected_runtime IN ('codex','claude'));

CREATE TABLE runtimes (
  runtime TEXT NOT NULL PRIMARY KEY CHECK (runtime IN ('codex','claude')),
  state TEXT NOT NULL CHECK (state IN ('UNDISCOVERED','AVAILABLE','UNAVAILABLE')),
  executable_path TEXT, version TEXT, validated_at_ns INTEGER,
  failure_code TEXT, failure_message TEXT
) STRICT;
INSERT INTO runtimes (runtime, state) VALUES ('codex','UNDISCOVERED'), ('claude','UNDISCOVERED');

CREATE TABLE native_sessions (
  desk_id TEXT NOT NULL REFERENCES desks(id),
  runtime TEXT NOT NULL CHECK (runtime IN ('codex','claude')),
  native_session_id TEXT NOT NULL, updated_at_ns INTEGER NOT NULL,
  PRIMARY KEY (desk_id, runtime)
) STRICT;

CREATE TABLE agent_processes (
  id TEXT NOT NULL PRIMARY KEY, desk_id TEXT NOT NULL REFERENCES desks(id),
  runtime TEXT NOT NULL CHECK (runtime IN ('codex','claude')),
  native_session_id TEXT, pid INTEGER NOT NULL, daemon_uuid TEXT NOT NULL,
  started_at_ns INTEGER NOT NULL, ready_at_ns INTEGER, ended_at_ns INTEGER,
  exit_reason TEXT CHECK (exit_reason IN ('EXITED','INTERRUPTED','QUIT','CONTROL_PLANE_LOST','DAEMON_LOST')),
  exit_code INTEGER,
  CHECK ((ended_at_ns IS NULL) = (exit_reason IS NULL))
) STRICT;
CREATE UNIQUE INDEX agent_processes_live ON agent_processes (desk_id) WHERE ended_at_ns IS NULL;

-- prompts rebuilt (migration-3 pattern): kinds + delivery columns
--   kind IN ('EVALUATION','TRIGGER_RESULT','ORIENTATION','DISCLOSURE')
--   state IN ('QUEUED','DELIVERED','FAILED')
--   attempted_at_ns, resolved_at_ns, runtime, native_session_id, failure_code, disclosed_at_ns
--   CHECK ((state <> 'QUEUED') = (resolved_at_ns IS NOT NULL))
--   CHECK ((state = 'FAILED') = (failure_code IS NOT NULL))
CREATE INDEX prompts_queue ON prompts (desk_id, created_at_ns) WHERE state = 'QUEUED';

-- operational_events rebuilt with the R3 kinds (R3-7)
```

Migration 4's `prompts` rebuild carries every row: migration 3's only `state` was `QUEUED`, which is the new vocabulary's `QUEUED` unchanged, and the attempt columns arrive `NULL`.

Recovery step `sessions` (registered after `executions`): open `agent_processes` of another `daemon_uuid` → `ended_at_ns = now`, `exit_reason = 'DAEMON_LOST'`, one `SESSION_EXITED` each; prompts with `attempted_at_ns NOT NULL AND state = 'QUEUED'` → `FAILED HANDOFF_UNKNOWN`; both lists in the recovery event (`sessions_lost`, `prompts_unknown`). Launch directories under `runtime/launch/` are removed at startup regardless.

## 9. Acceptance (R3-8)

### 9.1 The stand-in runtime

`runtime-standin` reads `MARKETRIG_STANDIN_SCRIPT` (a JSON file the gate writes) naming, per launch, `ready_after_ms`, `active_after_input_ms`, `exit_before_ready`, `mcp_read` (a resource URI to read through the registered adapter and echo), and for Claude `hooks: true`. As Codex: `app-server --listen ws://…` implements `initialize`, `thread/start`, `thread/resume` (unknown id → JSON-RPC error), `thread/turns/list`, `turn/start` (echoes the text to the TUI's socket, broadcasts `active` then `idle` per script), `turn/interrupt`, and rejects every other method; `--remote … -C` and `resume <id> --remote … -C` connect and drive the thread. As Claude: honors `--session-id`, `--resume` (unknown id → exit 1), `--mcp-config` (spawns the listed servers over stdio, completes MCP `initialize`, and for `marketrig-channel` reads `notifications/claude/channel`), `--settings` (runs the hook commands with the documented input objects), and `--dangerously-load-development-channels`. Every echoed input line is `INPUT <n>: <text>` on the PTY so the gate can read the attachment.

### 9.2 Gate scenarios (continuing R2's chain)

- **G27 — discovery.** Both runtimes `UNDISCOVERED`; explicit discover to the stand-in → `AVAILABLE` with version `99.0.0`; a discover to a nonexistent path → `UNAVAILABLE NOT_FOUND`; a stand-in scripted to print `0.1.0` → `VERSION_UNSUPPORTED`.
- **G28 — trigger fires, nobody home (Codex).** A code-less one-off fires → activation → `SESSION_STARTED`, `SESSION_READY`, `PROMPT_DELIVERED`; the attachment shows `INPUT 1: MarketRig TRIGGER_RESULT …`; the stand-in's `mcp_read` of `…/quotes` succeeds through the adapter the launch registered.
- **G29 — FIFO behind a turn, then resume.** Two firings while the stand-in stays `active` 5 s; order and timing per §6.3's scenarios; `exit` → `INTERRUPTED`; a third firing → `SESSION_STARTED {mode: RESUME}` with the same thread id.
- **G30 — the Claude half.** G28 and G29 repeated on `claude` after `switch`: channel readiness, two frames FIFO, hooks recorded (`SESSION_TURN_ENDED` after each turn, `SESSION_POINTER_CHANGED` on a scripted `clear`), Interrupt `409`.
- **G31 — failure and disclosure.** Stand-in scripted `exit_before_ready` → `PROMPT_FAILED ACTIVATION_FAILED`; the stand-in's app-server scripted to drop the socket on `turn/start` → `HANDOFF_UNKNOWN`, `CONTROL_PLANE_LOST`, one restart, `CONTROL_PLANE_STARTED`; the next new session's first input is the `DISCLOSURE` naming both ids and codes; `prompt show` reads the failed content.
- **G32 — hard kill.** Daemon killed with a live session and a prompt mid-attempt (the stand-in delays its `turn/start` response) → recovery event lists `sessions_lost` and `prompts_unknown`, no stand-in process survives, the pointer survives, the terminal route answers `NO_LIVE_SESSION`.

### 9.3 Experiment scenario

**E4** per cell, after E3 in the same invocation: the harness discovers the real runtime, creates a desk on it, attaches the terminal to the operator's console (raw mode, resize relayed) so the operator answers trust and channel confirmations, then defines a code-less one-off two minutes out and prints nothing further. Steps recorded: activation, readiness, `TRIGGER_RESULT` `DELIVERED`, and the operator's confirmation on the console that the prompt appeared as the session's input; a second one-off queued while the operator has the agent busy, delivered after; `switch` to the other runtime with the desk's history intact. Agent-behavior aspects end `INCONCLUSIVE`; a delivery the daemon's own rows contradict fails the cell.

## 10. Required checks

Module checks (`cargo test -p marketrigd`, fakes allowed):

1. `runtime::discover` — explicit path, `PATH` resolution on each platform, the three failure codes, the desktop-bundle rejection, version floor comparison.
2. `terminal` — spawn/attach/supersede, ring replay of exactly the newest 256 KiB, resize coalescing, slow-consumer drop, shutdown drains then terminates the tree (a child that spawns a grandchild).
3. `codex` — against an in-process fake app-server: pointer from the first non-ephemeral matching `thread/started`, gate open only on `idle` + no active turn, the three `turn/start` outcomes, `turn/interrupt` mapping, control-plane loss → `CONTROL_PLANE_LOST` rows + one restart + `UNAVAILABLE`.
4. `claude` — launch files written 0600 and deleted, hook route mappings for every event in §5.2 (including foreign session and `clear` repoint), channel readiness and the two failure paths, the bridge's `initialize` and notification framing.
5. `dispatch` — FIFO per desk, one prompt per desk per pass, orientation/disclosure ordering, activation failure fails only that desk's queued prompts, unresumable-then-new on the dispatcher path only, `disclosed_at_ns` set in the delivery unit.
6. `session` — every route's answers in §7, Switch validates before stopping, exit-reason for each ending, Quit closes rows `QUIT`.
7. `store` — migration 4 on a migration-3 database keeps every prompt row; recovery closes foreign open processes and fails attempted prompts, listing both.
8. `marketrig session hook` — exits 0 and prints nothing with no daemon, a rejecting daemon, and an oversize body.

Gate G27–G32 on macOS and Windows CI; E4 attended once per cell; static checks green. Marked as R3 exit in the implementing slice.
