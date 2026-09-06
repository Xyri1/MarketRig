# Slice 009 — Per-desk terminal rendering and reconnect correctness

**Status:** Implemented and frozen (2026-09-06).

Refines R5 SPEC §6.2 and R3 SPEC §3 under D30/D33. One desk still owns one warm terminal; the daemon remains the PTY authority.

1. Open the renderer only in a connected, measurable pane; retain parser state and dimensions while detached. Fit visible panes and repaint on selection.
2. Use xterm 6.0.0 with fit 0.11.0, WebGL 0.19.0 (DOM fallback on initialization failure/context loss), and Unicode11 0.9.0. Pins verified against npm on 2026-09-06.
3. Precede socket replay with terminal identity, absolute byte offset, replay length, current dimensions, and native ConPTY compatibility information. Skip duplicate replay in a warm frontend; disclose and stop on an unrecoverable gap or changed terminal. Fresh attachment remains best-effort, not screen reconstruction.
4. Bound frontend pending writes without blocking the PTY producer.

**Exit checks:** `pnpm check`; focused daemon terminal/socket tests; Rust formatting and Clippy for changed crate. Tests exercise real Unicode parsing, per-desk lifecycle, hidden fitting, renderer failure, replay overlap/gaps, pending-write accounting, and metadata before replay. Native Windows/macOS visual confirmation remains an attended check; no real user data root is touched.

**Verification:** pnpm check passed (10 files, 57 tests); pnpm exec vite build passed (bundle-size advisory only); cargo test -p marketrigd --lib terminal passed all 3 Windows checks; Clippy with -D warnings passed for marketrigd and marketrig-acceptance, all targets; cargo fmt --check and git diff --check passed. The attended console relay ignores attachment metadata so it cannot draw protocol fields into the native terminal. Native visual checks and the full acceptance gate were not run.
