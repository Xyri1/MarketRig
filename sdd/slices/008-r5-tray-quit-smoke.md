# Slice 008 — R5 tray, Quit, and the packaged smoke

**Status:** Active — created 2026-09-04; implementation not started
**Implements:** [`features/r5-desktop-approval-controls/`](../features/r5-desktop-approval-controls/SPEC.md) §5 (close-hides, tray, Quit, second launch, autostart), §7.3, and the tray part of §8 check 8
**Exit:** feature SPEC §8 check 8 complete, the static checks and the CI `frontend` job green on both platforms, and `pnpm smoke` green once per platform from a wiped per-user root, operator-run, with its WebdriverIO report and the shell log kept as evidence.

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

Depends on slice 007 frozen. Closes Milestone R5.

## 1. Pins

Verified against npm, crates.io, and the service's own packaged documentation on 2026-09-06.

| Dependency | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `@wdio/cli` | `9.31.5` | smoke (dev) | with `@wdio/local-runner`, `@wdio/mocha-framework`, `@wdio/globals`, and `webdriverio` on the same line. |
| `@wdio/spec-reporter` | `9.31.2` | smoke (dev) | the reporter's line stops at 9.31.2; it does not publish 9.31.5. |
| `@types/mocha`, `@types/node` | `10.0.10`, `26.4.1` | smoke (dev) | the spec is type-checked by its own `smoke/tsconfig.json`, outside the root `vue-tsc` program. |
| `@wdio/tauri-service` | `1.3.0` | smoke (dev) | `driverProvider: 'embedded'` on both platforms — macOS has no WKWebView WebDriver and the CrabNebula one is paid, and one mode everywhere is one config. Peer `webdriverio ^9`. |
| `tauri-plugin-wdio-webdriver` | `=1.3.0` | `marketrig-desktop` (optional) | the embedded provider *is* this crate: the service spawns the app with `TAURI_WEBDRIVER_PORT` and polls the server the plugin's `init()` starts. Behind the `wdio` Cargo feature, because `init()` opens an unauthenticated W3C WebDriver server on loopback and must never ship. |

`ponytail:` no `tauri-driver` install step, no Windows-only branch; the embedded server is the whole driver story — at the price of one feature-gated crate in the shell and a smoked bundle that differs from the shipped one by that plugin.

## 2. Plan-time settlements

- **Close-hides:** `on_window_event` on `main`: `CloseRequested { api, .. }` → `api.prevent_close()`, `window.hide()`; `RunEvent::ExitRequested { api, code: None, .. }` → `api.prevent_exit()` so the app lives with no visible window. `--hidden` on the command line skips the initial `show`.
- **Tray:** built in `setup` with `TrayIconBuilder` — the app icon, menu `open` / `pending` (disabled `MenuItem`, text from `set_tray_pending`) / `quit`; `on_menu_event` handles `open` (unminimize, show, focus) and `quit` (emit `marketrig://quit` to `main`); `on_tray_icon_event` left-click-up does what `open` does. `set_tray_pending(n)` sets the menu item text and, on macOS, `tray.set_title(Some(n.to_string()))` when `n > 0` else `None`; on Windows `set_tooltip`. Tray labels are English in R5; R6's `set_locale` rebuilds them.
- **Quit sequence:** the webview's `useDaemon.quit()` — `POST /quit`, poll `GET /health` every 250 ms until it fails or 10 s pass, `exit_app`. Both the Settings tab's button (behind `AlertDialog`) and the tray event call it.
- **Autostart:** `SettingsTab` reads `isEnabled()` and toggles `enable()` / `disable()`; first launch (no runtime `AVAILABLE` and autostart never touched) calls `enable()` once and records nothing — the plugin's own state is the setting.
- **The `wdio` feature:** `marketrig-desktop` gains `tauri-plugin-wdio-webdriver` as an optional dependency behind a `wdio` feature and one `#[cfg(feature = "wdio")]` line registering it. `scripts/build.mjs` passes `--features wdio` to `tauri build` only when `MARKETRIG_SMOKE_WIPE=1` — the same guard the wipe already needs — so every other `pnpm build` is the shipped artifact. The capability is untouched: the plugin's `default` permission set is empty, and naming it would break the feature-off build.
- **Smoke harness:** `wdio.conf.ts` with the tauri service pointing at the bundled binary path per platform. `src-tauri` is a member of the root Cargo workspace, so its artifacts land in the workspace target directory, not `src-tauri/target/`: `<CARGO_TARGET_DIR or target>/release/bundle/macos/MarketRig.app/Contents/MacOS/MarketRig` and `<…>/release/MarketRig.exe`. `maxInstances: 1`, mocha, 120 s timeout; `smoke/wipe.ts` runs in `onPrepare`: refuses without `MARKETRIG_SMOKE_WIPE=1`, kills any `MarketRig`, `marketrigd`, `runtime-standin` process, removes the data root, the log directory (`app_log_dir()`, which keys on the bundle identifier — `~/Library/Logs/dev.marketrig.desktop` and `%LOCALAPPDATA%\dev.marketrig.desktop\logs`), and `~/.marketrig`; `onPrepare` also builds `runtime-standin` (`cargo build --release -p marketrig-acceptance`). `smoke/smoke.spec.ts` is the five steps of feature SPEC §7.3, using the bearer read from the real endpoint file for its REST calls and `runtime-standin` from `target/release/`, armed by `MARKETRIG_STANDIN_SCRIPT` on the wdio process's environment — the shell and the daemon inherit it, exactly as the gate's harness sets it on the daemon's. Test ids (`data-testid`) are added to the controls the spec drives; nothing else changes in the frontend for the smoke.
- **Second launch in the smoke:** `child_process.spawn` of the same binary; the single-instance plugin exits it; the spec asserts the webview survived through `browser.getWindowHandles()` and a `window.__marketrigSmokeMarker` set before hiding.
- **Evidence:** the WebdriverIO report is written straight into `target/acceptance/smoke-<platform>-<stamp>/` through the reporter's and the runner's `outputDir`, and `onComplete` copies the shell's `MarketRig.log` beside it.

## 3. Chunks

| # | Chunk | Builds (feature SPEC) | Lands with (§8 checks) | Needs |
| --- | --- | --- | --- | --- |
| C47 | Close-hides, tray, `set_tray_pending`, Quit sequence, autostart toggle and first-launch enable, `--hidden` | §5 (window, tray, Quit, autostart) | check 8 (tray label); `pnpm check` | — |
| C48 | Smoke harness: pins, `wdio.conf.ts`, wipe guard, the spec, test ids, evidence copy, `pnpm smoke` and `pnpm build` wiring | §7.3 | `pnpm smoke` green on macOS | C47 |
| C49 | Windows packaged run, defects it finds fixed with their regressions, AGENTS.md **Commands** and `EXPERIMENT.md` refreshed for the smoke | §7.3 | `pnpm smoke` green on Windows | C48 |

## 4. Execution

C47 is one coding agent; C48 one agent on macOS with the orchestrator running the smoke itself (it wipes the real root, so it runs on the operator's machine only, never in CI, and never with a daemon the operator cares about running); C49 is operator-run on the Windows machine with a coding agent for whatever it finds. Every defect the smoke finds that a module check or gate scenario could catch gets that regression, not a smoke assertion (root §17).

## 5. Freeze and merge-back

When the exit checks are green: freeze this slice, then per AGENTS.md merge durable R5 mechanics into root `SPEC.md` (§4.3 the origin allowlist, first-frame authentication, and the bundled client; §4.4 the `installation_settings` row and the policy resource; §6.5 the terminal socket's first-frame path and close codes; §8.3 and §10 the projection rule; §12.3 the pending order, decision, cancel code, and actions listing; §13.1 the tool result; §13.2 `desk events` and `history actions`; §14 the shell commands, the panels, the tokens, notifications, and the events socket; §15 migration 6 and the three kinds; §17 G38–G41, the frontend job, and the smoke; §18 the resolved deferrals removed), summarize R5-1…R5-8 as one product `D<n>`, refresh D70's wording to the `approval` vocabulary, refresh `ROADMAP.md` (R5 delivered, evidence line), and grow the AGENTS.md **Commands** section for the frontend and the smoke.
