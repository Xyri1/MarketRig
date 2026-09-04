# R5 — Desktop and approval controls: Feature SPEC

*Decision basis: per D10, D26, D29, D30, D33, D52, D55, D56, D57, D58, D59, D60, D61, D62, D66, D68, D70, D71, D72, D75 and this feature's R5-1 … R5-8.* This document refines root `sdd/SPEC.md` §4.3, §4.4, §6.5, §8.3, §11.2, §12.3, §13.2, §14, §15, and §17. Where it names a Tauri, Hey API, utoipa, or ghostty-web fact, the fact was verified on 2026-09-04 against the versions R5-7 pins.

## 1. Workspace additions

- `crates/marketrigd/src/policy.rs` (the settings row, the policy resource, the approvals listing and decision route; R5-1, R5-2), `trigger.rs` grows the snapshot state and the projection rule (R5-3), `trade.rs` the pending path and the actions listing (R5-4), `events.rs` (the publisher, `WS /events`, the listing; R5-5), `api.rs` gains the WebSocket authentication and origin layer, the `utoipa` annotations, and `--openapi`; `store/006_r5.sql`.
- `crates/marketrig` gains `desk events` and `history actions`.
- `src-tauri/` — the `marketrig-desktop` crate (R5-6), a workspace member; `tauri.conf.json`, `capabilities/main.json`, `icons/`.
- Root frontend: `package.json` (`packageManager` pinned), `pnpm-lock.yaml`, `vite.config.ts`, `tsconfig.json`, `openapi-ts.config.ts`, `index.html`, `src/` (`main.ts`, `App.vue`, `style.css`, `client/` generated, `composables/`, `components/`, `locales/en.json`), `wdio.conf.ts`, `smoke/`.
- New Cargo pins: `utoipa =5.5.0` (features `axum_extras`), `utoipa-axum =0.2.0`; `tauri =2.11.5`, `tauri-build =2.6.3`, `tauri-plugin-single-instance =2.4.4`, `tauri-plugin-notification =2.4.0`, `tauri-plugin-autostart =2.5.1`, `tauri-plugin-log =2.9.1`. The daemon's post-commit signal uses `tokio`'s `Notify`, already pinned. Frontend pins are R5-7's and R5-8's, exact, in `package.json`.

## 2. Policies (R5-1)

| Route | Body | Answers |
| --- | --- | --- |
| `GET /settings/policies` | — | `200 {trigger_code_policy, paper_order_policy, delivery_mode, steer_available: false, updated_at_ns}` |
| `PUT /settings/policies` | any subset of `{trigger_code_policy, paper_order_policy, delivery_mode}` | `200` the resource; `400 VALIDATION` (unknown field, unknown value, empty object); `409 STEER_DISABLED` |

Vocabulary: `ALWAYS_ALLOW | REQUIRE_APPROVAL` for the two policies; `delivery_mode` admits only `QUEUE`. Each field that changes appends `POLICY_CHANGED {field, from, to}` (installation-wide, `desk_id` null) in the same unit as the update; a `PUT` that changes nothing writes nothing and answers `200`. The unit that inserts a code snapshot (§3.2) or a trading action (§3.3) reads its column with `SELECT … FROM installation_settings WHERE id = 1` inside its own transaction.

Scenarios:

- **Steer stays refused.** `PUT {"delivery_mode":"STEER"}` → `409 STEER_DISABLED`, no row change, no event; `GET` still reports `steer_available: false`.
- **One event per changed field.** `PUT {"trigger_code_policy":"ALWAYS_ALLOW","paper_order_policy":"ALWAYS_ALLOW"}` on a fresh install → exactly one `POLICY_CHANGED` (`trigger_code_policy`, `REQUIRE_APPROVAL → ALWAYS_ALLOW`).
- **Pending survives a policy change.** A `PENDING` snapshot exists; `PUT {"trigger_code_policy":"ALWAYS_ALLOW"}` → the snapshot is still `PENDING`, its trigger still undue.

## 3. Approvals (R5-2, R5-3, R5-4)

### 3.1 The listing and the decision

| Route | Body | Answers |
| --- | --- | --- |
| `GET /approvals?state=PENDING\|DECIDED\|ALL` (default `PENDING`) | — | `200 {approvals: [Approval…]}` newest first by `requested_at_ns`; `400 VALIDATION` (any other `state`) |
| `GET /approvals/{id}` | — | `200` Approval with the snapshot's `source`; `404 APPROVAL_NOT_FOUND` |
| `POST /desks/{d}/approvals/{id}` | `{"decision": "APPROVE" \| "DENY"}` | `200` Approval; `400 VALIDATION`; `404 APPROVAL_NOT_FOUND`; `409 APPROVAL_DECIDED`; `503 MARKET_UNAVAILABLE` (order approval, node cannot start) |

An **Approval** is `{kind: "TRIGGER_CODE" | "PAPER_ORDER", id, desk_id, desk_name, approval, requested_at_ns, decided_at_ns, detail}`; `id` is `code_snapshots.id` or `trading_actions.id`; `requested_at_ns` is the row's `created_at_ns`. For `TRIGGER_CODE`, `detail` is `{trigger_id, trigger_name, suffix, argv, timeout_secs, fingerprint, source_bytes}` plus `source` on the single read; for `PAPER_ORDER`, `detail` is `{action_id, source, trigger_id?, firing_id?, request}` where `request` is the stored submit body, plus `outcome` once the row has one. A record the policy never gated — `approval` is `ALWAYS_ALLOW` — is not an approval and no `state` lists it; `PENDING` lists `PENDING`, `DECIDED` lists `APPROVED` and `DENIED`, and `ALL` lists all three. A `TRIGGER_CODE` item names the trigger that holds the snapshot, or, once a patch has superseded it, the trigger of the last firing that ran it. The decision route resolves `id` in `code_snapshots` then `trading_actions`, both filtered by the path's desk; a decision on a row that is not `PENDING` is `409 APPROVAL_DECIDED` carrying the existing state. Every decision appends `APPROVAL_DECIDED {kind, id, decision}` on the desk, in the unit that writes `approval` and `decided_at_ns`; every pending record's creation appends `APPROVAL_REQUESTED {kind, id, trigger_id | action_id}` on the desk. Neither route exists on the CLI, and no decision queues a prompt (per D70).

### 3.2 Trigger code

Under `REQUIRE_APPROVAL`, `POST /desks/{d}/triggers` with `code` and `PATCH` attaching code whose fingerprint differs from the current snapshot's insert the snapshot `PENDING` with `decided_at_ns NULL` and set `next_occurrence_ns = NULL`; under `ALWAYS_ALLOW` the snapshot is `ALWAYS_ALLOW` with `decided_at_ns = created_at_ns` and the projection is R2's. The projection rule, everywhere it is computed (create, patch, enable, disable, the scheduler's advance after acceptance or a miss):

```text
next_occurrence_ns = NULL                      if deleted, disabled, or snapshot.approval IN ('PENDING','DENIED')
                   = first candidate > now     otherwise (R2 §2; an elapsed one-off stays NULL)
```

`APPROVE` writes `APPROVED`, `decided_at_ns`, recomputes the projection from the decision instant, and signals the scheduler; `DENY` writes `DENIED` and the trigger keeps the snapshot. A denied or pending trigger accepts every patch R2 allows; only a patch that attaches different code (a new `PENDING` or `ALWAYS_ALLOW` snapshot under the current policy) or detaches it (`code: null`) can make it due again. The Trigger resource's `code` object gains `approval` and `decided_at_ns` and keeps `approved_at_ns` (equal to `decided_at_ns` when `approval` is `ALWAYS_ALLOW` or `APPROVED`, absent otherwise); `marketrig trigger show` prints `approval` in its `code:` block. Disabled or pending time creates no miss record (root §10).

Scenarios:

- **Never due.** Under `REQUIRE_APPROVAL`, a one-off with code `at` 2 s ahead: after 5 s no firing, no miss, `next_occurrence_ns` absent; `disable` then `enable` → still absent; `APPROVE` → still absent (elapsed) and `TRIGGER_MISSED` not appended; `update --at` 2 s ahead → fires once with `code_snapshot_id` naming the approved snapshot.
- **Recurring approved.** An every-minute trigger created pending, approved 90 s later → its next occurrence is the first minute boundary after the decision, and no firing or miss exists for the boundaries before it.
- **Denied leaves nothing.** `DENY` → `APPROVAL_DECIDED {decision: "DENY"}`; after two occurrences, zero rows in `firings`, `executions`, and `prompts` for the trigger; the trigger reads `approval: "DENIED"`; `update --code` with different code → a new `PENDING` snapshot and a second `APPROVAL_REQUESTED`.
- **Same code, no reapproval.** `update --brief` on an approved trigger keeps the snapshot and its `APPROVED`; `update --code` with byte-identical code, suffix, argv, and timeout keeps it too.

### 3.3 Paper orders

Under `REQUIRE_APPROVAL`, `POST /desks/{d}/orders` and the MCP `submit_order` tool:

```text
validate form (ORDER_INVALID), attribution (ATTRIBUTION_INVALID), desk READY (DESK_NOT_READY)   -- as today
if (desk, action_id) exists -> 200 the stored record                                        -- as today
insert trading_actions {approval: PENDING, decided_at_ns: NULL, outcome: NULL}
append APPROVAL_REQUESTED {kind: PAPER_ORDER, id, action_id, instrument_id, side, type, quantity, price?}
answer 202 ActionRecord {action_id, id, kind, source, trigger_id?, firing_id?, approval: "PENDING", created_at_ns}
```

The node is neither started nor consulted. `APPROVE` starts the node lazily if needed (`503 MARKET_UNAVAILABLE` leaves the row `PENDING`), writes `APPROVED` and `decided_at_ns`, then runs the accept path from the stored `request` exactly as an ungated submit does after its row insert — `client_order_id = action_id`, synchronous through the sandbox, the outcome landing on the row, a sandbox refusal recorded in `outcome` with its reason and answered by the decision route as the Approval with that outcome in `detail`. `DENY` writes `DENIED`, `decided_at_ns`, and `outcome = {"failure_code": "DENIED"}`. A `CANCEL` naming a `client_order_id` whose submit action is `PENDING` answers `409 ORDER_PENDING_APPROVAL` and records nothing; a cancel of any sandbox-known order is ungated as today. Under `ALWAYS_ALLOW` every action row is `ALWAYS_ALLOW` with `decided_at_ns = created_at_ns` and nothing else changes. The **ActionRecord** everywhere gains `approval` and `decided_at_ns`; `outcome` is absent while pending.

`GET /desks/{d}/history/actions` answers `{actions: [ActionRecord…]}` newest first, daemon-local, any desk state; `marketrig [--json] history actions <desk>` prints one tab-separated line per action — `created_at_ns`, `kind`, `action_id`, `approval`, and the outcome's `status` or `failure_code` or `-`.

Recovery: a `PENDING` row is pure SQLite and survives any restart untouched; an `APPROVED` row whose `outcome` is still null after a crash is the same uncertain action R1 already leaves alone — never resubmitted, its record answering the replay.

The seeded constitution (R4 feature SPEC §5.1) gains, under *The paper environment*, for desks created from R5 on:

```markdown
The user may require approval of paper orders and of trigger code. A gated order answers
`approval: PENDING` with no order and reaches the sandbox only once approved in the MarketRig
desktop; read its state with `marketrig history actions <name>`. A gated trigger is never due
until approved (`marketrig trigger show`). You cannot approve, deny, or change the policy.
```

Scenarios:

- **Pending then approved.** Policy `REQUIRE_APPROVAL`; `submit_order` market buy of one AAPL lot → `202`, `approval: "PENDING"`, no `outcome`; `GET /desks/{d}/orders` and `positions` unchanged; `history actions` lists it `PENDING`; a repeated `action_id` → `200` the same record; `APPROVE` → fill row, order events, position, balance moved, `APPROVAL_DECIDED`, the record now `APPROVED` with the order projection; a market sell to flat then closes one cycle and queues one `EVALUATION` exactly as under `ALWAYS_ALLOW`.
- **Denied.** A pending limit order; `DENY` → record `DENIED` with `outcome.failure_code = "DENIED"`; no `order_events` row, no sandbox order; repeated `action_id` → `200` that record; `cancel_order` on it → `ORDER_NOT_FOUND` (terminal), and on a still-pending one → `ORDER_PENDING_APPROVAL`.
- **Trigger code is gated too.** A code-bearing firing whose script orders through `marketrig-mcp` under `REQUIRE_APPROVAL` → a `PENDING` action with `source: TRIGGER` and both ids, listed under `/approvals` with `detail.source = "TRIGGER"`.
- **Refused after approval is still a record.** A pending buy exceeding the CNY balance; `APPROVE` → the decision answers `200` with `approval: "APPROVED"` and `detail.outcome` carrying the sandbox's `ORDER_REJECTED` reason; the replay returns that record.

## 4. The events tail and browser-grade sockets (R5-5)

### 4.1 Publisher

The store's database thread pulses one `Notify` after each committed unit. One publisher task per daemon holds `cursor = (occurred_at_ns, id)` initialised to the table's last row at start (so a subscriber with no `after` sees only rows committed after it connects), wakes on the pulse or after 5 s, reads `WHERE (occurred_at_ns, id) > cursor ORDER BY occurred_at_ns, id LIMIT 500` until short, and pushes each row to every subscriber queue (1 000 frames); a push that finds a queue full closes that subscriber `4408 SLOW_CONSUMER` and drops it.

### 4.2 `WS /events`

```text
handshake   Origin (if present) ∈ allowlist else 403 ORIGIN_REFUSED; upgrade
frame 1 ←   {"bearer": "<credential>", "after": "<occurred_at_ns>:<id>"?}   within 5 s, else close 4401
            wrong bearer → close 4401 UNAUTHORIZED; unparseable → close 4400 VALIDATION
subscribe   register the queue, take tail = publisher cursor
replay      rows > after and ≤ tail, in pages of 500, as ordinary event frames
frame →     {"tail": "<cursor>"}          -- the position live frames continue from
frames →    {"id","kind","desk_id","occurred_at_ns","payload"}   one per row, `desk_id` absent when null
```

An `after` naming no row is ignored — the connection starts at the tail and the `tail` frame says so. The client sends nothing after frame 1; a client text frame is ignored. Cursor strings are `<occurred_at_ns>:<id>` with the decimal instant and the UUID.

### 4.3 The listing and the CLI

`GET /events?desk_id=<id>&before=<cursor>&limit=<1…500, default 100>` answers `{events: [event…], next_before?}` newest first, `desk_id` filtering to that desk's rows (installation-wide rows excluded) and `before` paging back; `marketrig [--json] desk events <desk> [--limit n]` prints one tab-separated line per row — `occurred_at_ns`, `kind`, `payload` as one-line JSON. The prompt listings of R2 stay as they are.

### 4.4 Origin and first-frame authentication on every WebSocket

The allowlist is exactly `tauri://localhost` (macOS production), `http://tauri.localhost` (Windows production), and `http://localhost:1420` (Vite's dev server, `build.devUrl`); a present `Origin` outside it is `403 ORIGIN_REFUSED` before any upgrade, on `/events`, `/desks/{d}/terminal`, and `/desks/{d}/channel` alike. Requests carrying no `Origin` pass to the bearer check. `/desks/{d}/terminal` with an `Authorization` header behaves exactly as R3 §3 specifies; without one it upgrades, requires `{"bearer": …}` as its first text frame within 5 s (`4401`), then answers its pre-attachment checks as close codes — `4404 DESK_NOT_FOUND`, `4409 NO_LIVE_SESSION` — and only then takes the attachment generation, so an unauthenticated or refused connection never supersedes a live viewer. `/desks/{d}/channel` stays header-only. Close-code reasons carry the same SCREAMING_SNAKE code as text.

Scenarios:

- **Gapless reconnect.** A client reads 20 events, disconnects, 30 more commit, it reconnects with `after` = the 20th → receives exactly the 30, then `tail`, then live rows; a second client with no `after` receives `tail` first and none of the 50.
- **Restart is a row.** The daemon restarts; the client's reconnect with its last cursor receives the new daemon's `RECOVERY` row first.
- **Slow consumer.** A subscriber that stops reading is closed `4408` while another subscriber and the daemon's commits continue unaffected.
- **Refused origin.** `Origin: https://example.com` → `403 ORIGIN_REFUSED`, no upgrade, on all three sockets; `Origin: tauri://localhost` with no header and a wrong first frame → upgrade then `4401`; a header-authenticated terminal client (the harness) is untouched by the new path.

## 5. The shell (R5-6)

`src-tauri/` — crate `marketrig-desktop`, product name `MarketRig`, identifier `dev.marketrig.desktop`, one window `main` (1280×800 default, 960×600 minimum, `visible: true` unless launched with `--hidden`), `build.devUrl = http://localhost:1420`, `build.frontendDist = ../dist`, `bundle.externalBin = ["binaries/marketrigd", "binaries/marketrig", "binaries/marketrig-mcp"]` (Tauri's sidecar convention: the source files carry the `-<target-triple>` suffix the CLI strips when bundling, and `pnpm tauri build` is preceded by `cargo build --release` and a copy step in `package.json`), `bundle.targets` `dmg` on macOS and `nsis` on Windows, and the capability `main.json` granting only `core:default`, `core:window:allow-hide`, `core:window:allow-show`, `core:window:allow-set-focus`, `core:window:allow-unminimize`, `notification:default`, `autostart:default`, `log:default`, and the four commands.

```text
read_endpoint()          -> {port, bearer, daemon_uuid} | null      reads <data root>/runtime/endpoint.json
start_daemon()           -> {port, bearer, daemon_uuid}             spawns <app dir>/marketrigd detached; polls the file every 250 ms for a new daemon_uuid, 30 s; Err(DAEMON_START_FAILED {stderr tail}) on exit or timeout
set_tray_pending(n)      -> ()                                       menu line "n pending approvals" (disabled), tray title on macOS, tooltip on Windows
exit_app()               -> never returns                            app.exit(0)
```

`start_daemon` spawns the sidecar with the daemon's normal environment and cwd the data root, `setsid` on macOS and `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS` on Windows, never holding a handle that would kill it when the shell exits — the shell's crash must not take the daemon with it; the daemon's own `MARKETRIG_TEST_*` seam variables are inherited from the shell's environment untouched, which is how the packaged smoke would relocate a root if it ever needed to (it does not; it wipes the real one, §7.3). The bootstrap in the webview is: `read_endpoint` → `GET /health` with the bearer and a UUID match → on any failure `start_daemon` → verify again → show the panels or the *daemon unavailable* state with a Retry.

Window and tray: `on_window_event(CloseRequested)` calls `api.prevent_close()` and `window.hide()`; `RunEvent::ExitRequested` is prevented so a hidden window keeps the app alive; the tray icon is built at setup with the menu *Open MarketRig* / *0 pending approvals* (disabled) / *Quit MarketRig*; *Open* and a left click on the icon `unminimize`, `show`, `set_focus`; *Quit* emits `marketrig://quit` to the webview, which runs `POST /quit`, polls `GET /health` until it fails or 10 s pass, then calls `exit_app` — the same sequence the window's own Quit control runs after its `AlertDialog`. The single-instance plugin's callback shows and focuses the window (a second launch never starts a second daemon: the shell it belongs to exits inside the plugin). Autostart uses the plugin with `args(["--hidden"])`; the app name is `MarketRig`; Settings shows its state through `isEnabled` and toggles it. The log plugin writes `MarketRig.log` to the platform log directory (`LogDir` target), rotated at 5 MiB keeping one file. On macOS the dock icon stays while hidden (no activation-policy change); a badge is not used.

Scenarios:

- **Cold start from nothing.** No endpoint file: the window shows *Starting MarketRig…*, `start_daemon` returns within 30 s, health matches, desks load.
- **Stale endpoint.** A file from a dead daemon: health fails to connect → `start_daemon` → a new UUID → verified.
- **Close is hide.** Close → window hidden, `GET /health` still answers, the terminal socket still open (bytes counted by the frontend keep rising); tray *Open* → the same webview, no reload, the counter continued.
- **Quit is quit.** Tray *Quit* → every open `agent_processes` row `QUIT`, the endpoint file gone, the shell process exited.
- **Second launch.** Launching the binary again while hidden → the window shows and focuses; one daemon.

## 6. The frontend (R5-7)

### 6.1 Toolchain and generation

`package.json` pins `packageManager: pnpm@11.25.0+sha512.<hash>` and `engines.node: 24.18.0`; every dependency is exact. `openapi-ts.config.ts`:

```ts
export default { input: 'openapi.json', output: 'src/client',
  plugins: ['@hey-api/typescript', { name: '@hey-api/sdk', operations: { strategy: 'flat' } },
            { name: '@hey-api/client-fetch', runtimeConfigPath: './src/hey-api.ts' }] };
```

`openapi.json` is written by `cargo run -p marketrigd -- --openapi`, a daemon flag that prints the document and exits without touching a data root; the document comes from `utoipa_axum::router::OpenApiRouter` with `#[utoipa::path]` on every handler and `ToSchema` on every request and response type, `split_for_parts()` giving the served `Router`. `src/hey-api.ts` sets `baseUrl` and the `Authorization` header from `useDaemon` at runtime. `pnpm generate` runs both steps; `pnpm check` runs Prettier, ESLint, `vue-tsc --noEmit`, and Vitest. The error envelope is one `ToSchema` type and every route declares it for its non-2xx statuses, so generated calls carry typed codes.

### 6.2 Composables

```text
useDaemon      endpoint {port, bearer, daemon_uuid} in memory only; verify(); start(); status: STARTING | READY | UNAVAILABLE
useEvents      the one WS /events socket; cursor; on(kind | kind[], refetch): every handler refetches a resource and nothing else;
               reconnects with backoff (1 s … 10 s) sending the cursor; ATTENTION rows set a per-desk attention flag the
               operator's keyboard input on that terminal clears (the two frontend-local facts, per D72)
useTerminal    per desk: one ghostty-web Terminal (created after init(), FitAddon, observeResize) and one terminal socket;
               created when SESSION_STARTED / a live session is seen, kept while the desk is listed and the process lives,
               disposed on SESSION_EXITED; the mounted <div> is swapped by selection, the Terminal is never recreated for it
useApprovals   GET /approvals?state=PENDING; per-desk counts; total → set_tray_pending; refetched on APPROVAL_REQUESTED
               and APPROVAL_DECIDED
```

Terminal wiring: `socket.binaryType = 'arraybuffer'`; binary frames → `term.write(new Uint8Array(data))`; `term.onData(s => socket.send(encoder.encode(s)))`; `term.onResize(({cols, rows}) => socket.send(JSON.stringify({resize: {cols, rows}})))`, sent once more on attach; the `exited` frame writes one dim line *process exited (<reason>, <code>)* to the well and the composable disposes on `SESSION_EXITED`. A window hide neither closes the socket nor pauses writes; a reload reattaches and the ring replays. No mutation updates a list optimistically; every control awaits the daemon and lets the refetch redraw (per D72).

### 6.3 Layout

```text
┌─────────────┬──────────────────────────────────────────┬──────────────────────┐
│ MarketRig   │ alpha · codex · session live · Interrupt Exit Switch  │ Desk Triggers Approvals │
│ ▌alpha  ●2  │                                          │ Activity Settings    │
│ ▌beta       │        terminal well (ghostty-web)       │                      │
│ ▌gamma  ○   │        dark in both schemes              │  selected tab body   │
│             │                                          │                      │
│ + New desk  │                                          │                      │
└─────────────┴──────────────────────────────────────────┴──────────────────────┘
  240 px         flexible, min 480 px                       360 px, collapses to a 40 px rail
```

- **Left.** Desk rows in creation order: a 2 px status gutter (live → `--state-live`, pending approvals → `--state-pending`, attention → `--state-attention`, `FAILED` or `UNAVAILABLE` workspace → `--state-failure`, else `--state-idle`), the name in the terminal stack, a pending-approval count, an attention dot. *New desk* opens an inline form (name, runtime `Select`); a `FAILED` desk shows *Retry*. Selection is the accent.
- **Center.** A header line — desk name, selected runtime, session state from `GET /desks/{d}/session`, and the §6.2 controls of root SPEC as buttons (*Start* / *Continue* when no process, *Interrupt* disabled on Claude Code with its reason, *Exit*, *Switch runtime* as a `Select`) — over the well. With no live session the well shows the last known screen if a presentation exists, else an empty well with *No session* and the start controls.
- **Right.** Reka UI `Tabs`: **Desk** — quotes, book, positions, open orders (`GET …/market/quotes`, `book`, `positions`, `orders`, refetched on trading events and every 15 s while visible); **Triggers** — the listing with enable/disable, each trigger's `approval` when it has code, and the firings drawer; **Approvals** — the pending items across desks, newest first, each with kind, desk, request or code (source in a `<pre>` in the terminal stack), *Approve* and *Deny* (Deny behind `AlertDialog`); **Activity** — `GET /events?desk_id=` newest first, kind and payload, *Load more* through `before`, live rows prepended by refetch; **Settings** — runtimes (path, version, state, *Discover*, *Retry*, an explicit-path field), memory child and provider (the R4 routes; the model `Select` fetches live on open), policies (two `Select`s; delivery mode shown disabled at *Queue next turn* with *Steer* visibly disabled), autostart, and *Quit MarketRig*. The Settings tab is selected automatically while no runtime is `AVAILABLE`, which is the whole of first-launch onboarding.

### 6.4 Notifications

Sent through `@tauri-apps/plugin-notification` (`isPermissionGranted` then `requestPermission` once, at first need) only while the window is hidden or unfocused, one per event, title and body from the `en` catalog with the desk name: `APPROVAL_REQUESTED` (*alpha: approval needed — market buy 100 AAPL.XNAS*), `SESSION_ATTENTION` (kinds other than `session_start`), `PROMPT_FAILED`, `DESK_FAILED`, `TRADING_NODE_FAILED`, `RUNTIME_UNAVAILABLE`, `CONTROL_PLANE_LOST`, `MEMORY_UNAVAILABLE`, `TRIGGER_MISSED`, and a `TRIGGER_RESULT` prompt whose execution `outcome` is not `EXITED` with code `0` (read through the firing route on `PROMPT_DELIVERED`). Routine results — fills, deliveries, retains — never notify (per D52). Click behaviour beyond the platform default stays deferred (root §18).

### 6.5 Tokens

`src/style.css` is Tailwind's `@import "tailwindcss"` plus this block and one dark override; components carry no stylesheet and no literal colour (per D72):

```css
@theme {
  --font-ui: system-ui, -apple-system, "Segoe UI Variable", "PingFang SC", "Microsoft YaHei UI", sans-serif;
  --font-terminal: "SF Mono", Menlo, "Cascadia Mono", Consolas, ui-monospace, monospace;
  --text-xs: 11px; --text-sm: 12px; --text-base: 13px; --text-lg: 15px; --text-xl: 18px;
  --spacing: 4px;
  --radius-control: 4px; --radius-panel: 8px; --radius-pill: 999px;
  --color-ground: oklch(0.985 0.004 250);   --color-panel: oklch(0.965 0.005 250);
  --color-line:   oklch(0.88  0.008 250);   --color-ink:   oklch(0.22  0.01  250);
  --color-ink-muted: oklch(0.50 0.01 250);  --color-well:  oklch(0.17  0.01  250);
  --color-accent:  oklch(0.55 0.15 265);    --color-accent-ink: oklch(0.99 0 0);
  --color-state-live: oklch(0.62 0.17 150); --color-state-pending: oklch(0.78 0.16 85);
  --color-state-attention: oklch(0.68 0.19 55); --color-state-failure: oklch(0.58 0.22 25);
  --color-state-idle: oklch(0.72 0.01 250);
}
@media (prefers-color-scheme: dark) { :root {
  --color-ground: oklch(0.19 0.01 250); --color-panel: oklch(0.23 0.01 250); --color-line: oklch(0.32 0.01 250);
  --color-ink: oklch(0.92 0.005 250);  --color-ink-muted: oklch(0.65 0.01 250); --color-well: oklch(0.14 0.01 250);
  --color-accent: oklch(0.72 0.13 265); --color-accent-ink: oklch(0.15 0.02 265);
} }
```

Colour appears only through the five `state-*` roles, in the gutter, the attention dot, and the approval badge, and through `accent` for selection and focus rings; everything else is the neutral ramp. The terminal well is `--color-well` in both schemes; ghostty-web's theme is derived from the tokens at mount. The gutter never animates; the only motion is the 120 ms opacity of a tab body. Type below 13 px is reserved for machine tokens in the terminal stack. Every prose string is `t('…')` from `src/locales/en.json`; a Vitest check fails on a bare string in a template (per D68) so R6 adds `zh-Hans.json` and nothing else.

## 7. Verification (R5-8)

### 7.1 Gate scenarios (continuing R4's chain)

The daemon now defaults `trigger_code_policy` to `REQUIRE_APPROVAL`, so G21's prologue sets it to `ALWAYS_ALLOW` through `PUT /settings/policies` before the first code-bearing trigger, and the experiment's E3 setup does the same; G38 restores the default.

- **G38 — policies and the tail.** `GET /settings/policies` reports the two `ALWAYS_ALLOW` values G21 set and `steer_available: false`; `PUT {"delivery_mode":"STEER"}` → `409 STEER_DISABLED`; `PUT` restoring `REQUIRE_APPROVAL` for code → one `POLICY_CHANGED`. Then the tail: one `WS /events` client with the first-frame bearer reads the `POLICY_CHANGED` row live; it disconnects, `desk create` twice, it reconnects with its cursor and receives exactly the desk events, then `tail`; a client that never reads is closed `4408` after ≥ 1 000 events are produced by a flood of `PUT`s that alternate a policy; `desk events` lists newest first and `before` pages.
- **G39 — trigger code approval.** On a fresh desk under `REQUIRE_APPROVAL`: a code one-off 2 s ahead → `APPROVAL_REQUESTED`, `approval: PENDING`, no projection, no firing after 5 s, no miss; `enable`/`disable` change nothing; `APPROVE` → still undue; reschedule 2 s ahead → fires, its firing names the approved snapshot, the stand-in session receives the `TRIGGER_RESULT`. A second code one-off → `DENY` → after its time, no firing, execution, or prompt; `update --code` with other code → a new pending snapshot. Under `ALWAYS_ALLOW` the same create fires unprompted.
- **G40 — paper order approval.** `PUT paper_order_policy REQUIRE_APPROVAL`; on the G35 desk (stand-in feed): `submit_order` through the harness's MCP client → `approval: PENDING`, no outcome, no position; replay `200`; `cancel_order` → `ORDER_PENDING_APPROVAL`; `history actions` lists it; `APPROVE` → fill, position, `APPROVAL_DECIDED`; a sell to flat → one cycle and one `EVALUATION` as in G13; a second submit → `DENY` → `outcome.failure_code = DENIED`, no `order_events`; a code-bearing one-off whose script orders → a `PENDING` action with `source: TRIGGER`; a pending buy exceeding the CNY balance → `APPROVE` → `APPROVED` with the sandbox's `ORDER_REJECTED` in the outcome. Finally `PUT` back to `ALWAYS_ALLOW` and one ungated submit whose row reads `ALWAYS_ALLOW`.
- **G41 — sockets and a hard kill.** `Origin: https://example.com` on `/events`, the terminal, and the channel → `403 ORIGIN_REFUSED`; `Origin: tauri://localhost` and a wrong first frame → `4401`; the terminal without a header on a desk with no session → `4409` and the live viewer of another desk untouched; a header-authenticated terminal attachment still works. Then a pending order and a pending snapshot exist; hard-kill the daemon; the next start's recovery event names nothing about them, both are still `PENDING` and decidable, and a client reconnecting with its pre-kill cursor receives the `RECOVERY` row first.

### 7.2 Frontend checks

Vitest with Vue Test Utils and jsdom, against a fake `fetch` and a fake `WebSocket`: `useDaemon` verifies UUID match and falls back to `start_daemon` once; `useEvents` sends the cursor on reconnect, dispatches by kind, and calls only refetches; `useTerminal` keeps one `Terminal` per desk across selection changes and disposes on `SESSION_EXITED`; `useApprovals` counts per desk and calls `set_tray_pending`; the Approvals tab renders both kinds and disables its buttons while a decision is in flight; the policy `Select` renders *Steer* disabled; the catalog check finds no bare template string; the drift check is CI's `git diff --exit-code src/client` after `pnpm generate`.

### 7.3 The packaged smoke

`pnpm smoke`, operator-run, refuses to start unless `MARKETRIG_SMOKE_WIPE=1`; it then quits any running MarketRig, deletes the per-user data root, the log root, and `~/.marketrig`, and drives the bundled application (`src-tauri/target/release/bundle/…`) through `@wdio/tauri-service` in `driverProvider: 'embedded'` mode on both platforms, one spec:

1. the window appears; Settings is selected (no runtime); the daemon endpoint file exists and health answers with the bearer it carries;
2. register `runtime-standin` (built by `cargo build -p marketrig-acceptance`) as `codex` through the explicit-path field; create desk `smoke`; *Start*; the well shows the stand-in's banner;
3. hide by closing the window (the WebDriver session stays attached to the hidden webview); create through REST a code-free one-off trigger 2 s ahead, whose `TRIGGER_RESULT` the stand-in echoes into the terminal; launch the binary a second time; the window is visible again, the same webview (a marker set in `window` before hiding is still there), and the well contains the bytes written while hidden;
4. set paper orders to *Require approval*; `POST /desks/{d}/orders` with the bearer → the Approvals tab shows one item and the tray count is 1; *Approve* → the Desk tab shows the position; a second order → *Deny* → `DENIED` in history actions;
5. *Quit MarketRig* through the Settings tab → the endpoint file is gone, no `marketrigd` or `runtime-standin` process survives, the application process has exited.

It is the one leg that touches the per-user root (root §17) and stays out of CI.

## 8. Required checks

Module checks (`cargo test -p marketrigd` unless named; fakes allowed):

1. `policy` — the row's defaults, `PUT` validation, `STEER_DISABLED`, one `POLICY_CHANGED` per changed field, the in-unit read, a pending record untouched by a policy change.
2. `trigger` — pending and always-allow snapshot creation under each policy, the projection rule at every computation site, approve recomputing from the decision instant (recurring next boundary, elapsed one-off `NULL`), deny keeping the snapshot, identical-fingerprint patch keeping the state, the resource fields, `marketrig trigger show`.
3. `trade` — pending insert without node contact, `202` and the replay `200`, `APPROVAL_REQUESTED` payload, approve re-entering acceptance against the in-process sandbox (fill and a refusal both recorded), deny's outcome, `ORDER_PENDING_APPROVAL`, `history actions` order and fields, `TRIGGER` source gating, the constitution paragraph in the seed file byte for byte.
4. `policy::approvals` — the union listing's order and shapes, `state` filter, `APPROVAL_NOT_FOUND`, `APPROVAL_DECIDED`, desk scoping (a snapshot decided through another desk's path is `404`), `APPROVAL_DECIDED` events.
5. `events` — the publisher's cursor and pages, the gapless replay boundary, `tail`, ignored `after`, `4408`, `4401`/`4400`, the listing with `before`, `marketrig desk events`.
6. `api` — origin allowlist on the three sockets, header path unchanged, first-frame terminal path with `4404`/`4409`, `--openapi` prints a document every route appears in (`paths` count equals the router's) and exits `0` without a data root.
7. `store` — migration 6 on a migration-5 database keeps every row, backfills `ALWAYS_ALLOW` with `decided_at_ns = created_at_ns` on both tables, drops `approved_at_ns`, and accepts the three new kinds.
8. `marketrig-desktop` (`cargo test -p marketrig-desktop`) — `read_endpoint` parses and rejects a malformed file; `start_daemon` against a fake sidecar that writes the file (success) and one that exits (`DAEMON_START_FAILED`); the tray menu label for `set_tray_pending`.
9. Frontend — §7.2 through `pnpm check`.

Gate G38–G41 on macOS and Windows CI; the frontend job on both; the packaged smoke once per platform, operator-run and recorded in the slice's evidence line; static checks green. Marked as R5 exit in the implementing slices.
