# Slice 010 — One owned development stack

**Status:** Active (2026-09-06).

Under D30/D33, development is explicitly command-owned; packaged close-to-tray and detached-daemon behavior stays unchanged.

1. `pnpm dev` builds the daemon, CLI, and MCP adapter before launching the GUI. Copy runnable binaries outside Cargo's output paths so a live dev process cannot lock linker outputs.
2. A Node stdlib runner owns the daemon, Vite, and Tauri. Ctrl+C, startup failure, or any service exit shuts down the other services. The daemon's stdin pipe is a lifetime link: EOF requests its ordinary graceful shutdown, including children and endpoint cleanup.
3. Use persistent `target/dev-data` by default, honoring an explicit `MARKETRIG_TEST_DATA_ROOT`; never adopt a packaged daemon. The supervised shell may read the runner's endpoint but must not spawn a detached replacement. Use a distinct dev application identifier.
4. Add focused runner lifecycle tests and exercise the real daemon with piped stdin in scratch storage. Verify startup/stop and subsequent rebuild on Windows; macOS native confirmation remains outstanding when unavailable.

**Exit checks:** frontend checks plus Node lifecycle tests, scoped Rust tests/Clippy, a real scratch daemon EOF shutdown, and an owned dev startup/stop/rebuild check. No release lifecycle or unrelated terminal changes.
