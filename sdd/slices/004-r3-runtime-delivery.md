# Slice 004 — R3 runtime adapters and delivery

**Status:** Active 2026-09-02
**Implements:** all of [`features/r3-runtime-delivery/`](../features/r3-runtime-delivery/SPEC.md)
**Exit:** feature SPEC §10 in full — the eight module checks, gate G27–G32 after G26, and the static checks green on macOS and Windows CI, plus E4 attended once per platform-and-runtime cell (agent-behavior aspects end inconclusive rather than failed, root §17).

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

## 1. Pins

Verified against the local registry index, the lockfile, and the crate sources on 2026-09-02. Chunk numbering, toolchain, and the R0–R2 pins continue unchanged.

| Crate | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `portable-pty` | `=0.9.0` | marketrigd | MIT, wezterm's PTY crate; `default-features = false`. Requires `nix ^0.28` (a second `nix` beside the graph's 0.31.3), `smol ^2.0`, `serial2 ^0.2`, `filedescriptor ^0.8.3`, `shell-words ^1.1`, `downcast-rs`, `libc`, `log`, `anyhow`, `futures` (present); Windows adds `winapi 0.3`, `winreg 0.10`, `shared_library`, `bitflags 1.3`, `lazy_static`. |
| `axum` | `=0.8.9` (existing) | marketrigd | gains the `ws` feature, which resolves `tokio-tungstenite ^0.29.0`. |
| `libc` | `=0.2.189` | marketrigd (unix target) | already in the graph (`portable-pty`, `nix`); the terminal manager's `killpg` on the child's session. |
| `windows` | `=0.62.2` | marketrigd (windows target) | already in the graph; features `Win32_Foundation`, `Win32_Security`, `Win32_System_JobObjects`, `Win32_System_Threading` for the terminal manager's Job Object termination. |
| `tokio-tungstenite` | `=0.29.0` | marketrigd, marketrig-mcp | the daemon's app-server client and the bridge's channel client share the line axum's `ws` resolves, so one copy; `default-features = false`, feature `connect` only (loopback, no TLS). |

`ponytail:` `portable-pty` drags a second `nix` and the `smol` runtime into the graph for one `openpty`/ConPTY pair. Root §6.5 (per D31) wants one pinned crate rather than a hand-rolled `forkpty`, so it stays; the upgrade path if the graph cost ever matters is the ~200-line in-tree PTY over `libc` and the already-present `windows` crate, recorded as a decision change.

## 2. Plan-time settlements

Facts the feature docs left to the slice, verified in the pinned sources today:

- **Terminal (R3-2):** `portable_pty::native_pty_system().openpty(PtySize)` → `PtyPair { master, slave }`; `slave.spawn_command(CommandBuilder)` (cwd, env, argv; Unix `pre_exec` does only `setsid` + `TIOCSCTTY`); `master.take_writer()` and `master.try_clone_reader()` give the blocking writer and reader the manager's threads own; `master.resize(PtySize)`. The child is wrapped by the R2 primitive for termination — `spawn_command` returns a `Child` whose `process_id()` seeds `ProcessSession`-equivalent group kill on Unix (`killpg` on the child's own session, which `setsid` created) and `JobObject` assignment on Windows (`OpenProcess` on the returned `process_id()` with `PROCESS_SET_QUOTA | PROCESS_TERMINATE`, then `CreateJobObjectW`, `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and `AssignProcessToJobObject`, all immediately after `spawn_command` so that whatever the session starts later is inside the job; the terminal keeps the job handle and closing it at shutdown kills the tree, and a failed assignment falls back to `TerminateProcess` on the leader). No spawn helper. The manager lives in `ApiState` as an `Arc`, and the exit notification is one `mpsc` from `Manager::new()`.
- **Sockets:** both daemon routes use `axum::extract::ws::WebSocketUpgrade`; the bearer is checked by the same middleware as every route before the upgrade. Client side, `tokio_tungstenite::connect_async("ws://127.0.0.1:<port>")` for the app-server and the bridge; JSON-RPC 2.0 frames are `serde_json::Value` with an `AtomicI64` request id and a `HashMap<i64, oneshot::Sender>` pending map — no JSON-RPC crate.
- **App-server port:** `TcpListener::bind("127.0.0.1:0")` → `local_addr().port()` → drop → spawn with `--listen ws://127.0.0.1:<port>`; a bind race surfaces as the 15-second connect failure and takes the control-plane failure path.
- **Bridge (R3-4):** `rmcp =3.2.0` serves the channel with `ServerCapabilities { experimental: Some(ExperimentalCapabilities(map{"claude/channel": {}})), ..Default }` and pushes each frame with `peer.send_notification(ServerNotification::CustomNotification(CustomNotification { method: "notifications/claude/channel", params }))`; `CustomNotification` exists at the pin (`model.rs`).
- **Hook route body:** `String` extractor, content type checked after the body is consumed (the Windows 10053 lesson from slice 003 stays the rule for every body-taking route).
- **Recovery registration:** `daemon::recover` gains the `sessions` step after `executions`, calling `session::recovery_step(tx, daemon_uuid, now_ns) -> (Vec<Value>, Vec<Value>)` (the instant is passed in, as `exec::recovery_step` already takes it, so the whole unit shares one) for `sessions_lost` and `prompts_unknown`.
- **Startup and shutdown glue (`lib.rs::serve`):** discovery runs as step 6a before the listener; a third `Arc<Notify>` (dispatcher wake) joins the two from slice 003 and is signalled by every prompt insert (R2's `insert_result_prompt`, the orientation and disclosure inserts) and by every adapter readiness or exit event; shutdown ends every open `agent_processes` row `QUIT` (terminals first, then the app-server child) inside the existing 5-second deadline, and the dispatcher task is awaited after the executor.
- **Spike S, run 2026-09-03 on Codex 0.152.1, before adapter code:** (S1) `-c mcp_servers.marketrig.*` on the `--remote` TUI command line does **not** reach the remote thread — `mcpServerStatus/list {threadId}` omitted it — so the fallback stands: the daemon writes `<workspace>/.codex/config.toml`, which the same call does list. (S2) a `turn/start` from a second connection routes its approval to the TUI, never to that connection (only `thread/status/changed` `active {activeFlags:["waitingOnApproval"]}` was seen). (S3) `--ws-auth capability-token` starts on a loopback listener when given `--ws-token-file`, and the TUI takes the token through `--remote-auth-token-env`. Recorded in R3-3's ponytail note; feature SPEC §4.1–§4.2 carry the two mechanism changes.
- **Test targets:** G27–G32 extend `--test gate` after G26; E4 joins `--test experiment` as a fourth attended test gated by `MARKETRIG_EXPERIMENT`, serialized after E3 in the same invocation. `MARKETRIG_STANDIN_SCRIPT` is read only by `runtime-standin`.

## 3. Chunks

One coding-agent work unit each; a chunk is done when its named checks pass locally. Numbering continues from slice 003.

| # | Chunk | Builds (feature SPEC) | Lands with (§10 checks) | Needs |
| --- | --- | --- | --- | --- |
| C23 | Foundation: pins, migration 4, runtimes and discovery, pointers and process rows, hook ingress, recovery | §1; §2; §8 in full; `session::` row helpers (open, ready, close-with-reason, repoint); `POST /desks/{d}/session/hook` and `marketrig session hook`; `GET /runtimes`, `discover`, `retry`; `POST /desks` `runtime` field | checks 1, 7, 8; static checks green with the new pins | — |
| C24 | Terminal manager and the attachment socket | §3 | check 2 | C23 |
| C25 | Codex adapter (spike S first) | §4 | check 3 | C24 |
| C26 | Claude adapter, launch files, channel socket, bridge mode | §5 | check 4 | C24 |
| C27 | Dispatcher, activation, renderings, session-control routes, Quit glue | §6, §7 | checks 5, 6 | C25, C26 |
| C28 | Gate G27–G32, `runtime-standin`, experiment E4, operator guide | §9; `EXPERIMENT.md` and AGENTS.md **Commands** refreshed | G27–G32 on both platforms; E4 target content | all |

C25 and C26 run in parallel after C24, each against the `Adapter` trait C24 lands at the end of `session.rs` (`spawn`, `deliver`, `interrupt`, `exit` over `async_trait`, with `AdapterEvent` readiness, pointer, attention and exit evidence over one `mpsc` the dispatcher drains); C27 follows both; C28 is last. C24's checks drive a plain `cat`/`cmd` child, not a runtime.

## 4. Execution

Orchestrator plus one coding agent per chunk, sequential along the Needs column, parallel where §3 allows; parallel chunks work in `.worktrees/<chunk>/` per AGENTS.md. Each agent receives this slice, the feature SPEC, and its chunk row, and delivers a diff with its checks green; the orchestrator runs the static checks after every merge and smokes the real binaries with the real CLIs on macOS (discover → create desk → activate NEW → attach the terminal → exit → quit) before briefing C28. E4 is operator-attended after C28 merges, one run per platform-and-runtime cell, evidence bundled per root §17.

## 5. Freeze and merge-back

When the exit checks are green: freeze this slice, then per AGENTS.md merge durable R3 mechanics into root `SPEC.md` (§4.4 the `runtimes` resource, §6.2 the routes and exit reasons, §6.3–§6.4 the per-runtime mechanics no longer deferred, §6.5 the ring size and timings, §7 orientation and disclosure as rows, §11.1 the delivery states and failure codes, §13.1 registration takeover, §15 migration 4 and the recovery step, §17 the stand-in runtime and E4, §18 the resolved deferrals removed), summarize R3-1…R3-8 as one product `D<n>`, refresh `ROADMAP.md` (R3 delivered, evidence line), and grow the AGENTS.md **Commands** section for E4.
