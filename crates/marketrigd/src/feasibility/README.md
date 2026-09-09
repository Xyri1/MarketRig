# Feasibility spike — common brief for every agent

Read `sdd/features/a-share-engine/FEASIBILITY.md` first (the task, the bounds, the
deliverable), then `sdd/features/a-share-engine/{PRD,DECISIONS,SPEC,RESEARCH}.md`,
root `AGENTS.md`, and the parts of `sdd/SPEC.md` your question touches (§5 orders,
§12.3 approvals, §17 acceptance). Read the whole flow you test before writing.

## Ground rules (from FEASIBILITY.md — binding)

- Worktree: `/Users/xyril/Projects/MarketRig/.worktrees/a-share-feasibility`, branch
  `codex/a-share-feasibility`. Run every command from here. Never touch the main
  checkout, the real data root, `~/.marketrig`, credentials, or the smoke path.
- `export CARGO_TARGET_DIR=/Users/xyril/Projects/MarketRig/target` before every cargo
  command (shared cache; do not create another target dir — disk is tight).
- Module tests only: `cargo test -p marketrigd --lib feasibility::<your file>`. No
  gate, no experiment, no real orders, no daemon binary. If you must run
  `marketrigd`/`marketrig`, set `MARKETRIG_TEST_DATA_ROOT` to a scratch dir inside
  this worktree and `MARKETRIG_TEST_NO_TRADING=1` or a loopback stand-in feed.
- Pinned crates only, public seams only: `nautilus-* =0.62.0` at
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nautilus-*-0.62.0/`. No
  fork, no pin bump, no private-field mutation, no second engine, no daemon fee/P&L
  arithmetic, no edited fill events. If a question needs one, record BLOCKED with
  the exact missing seam (path + symbol).
- Source inspection alone is not PASS for an execution question; the assertion must
  run against the real sandbox node. Do not replace a controlled clock with sleeps
  or wall-clock claims; if controlled time cannot drive the real path, record that as
  a deterministic-acceptance blocker.
- Do not assume: quote suppression disables cached matching; DAY schedules expiry;
  a closed upstream issue means shipped support; a method's existence proves the
  execution path. Keep "official exchange rule" distinct from "our conservative
  fill policy".
- Do not spawn subagents. Do not edit sibling `feasibility/*.rs` files (other agents
  own them concurrently). If the crate fails to compile in a file that is not yours,
  wait ~60 s and retry; do not fix it. Keep your own file compiling at all times
  (commit-worthy, `cargo fmt`, clippy-clean under `-D warnings` for `--all-targets`).
- Do not edit product code (`node.rs`, `trade.rs`, `feed.rs`, …) unless a
  `#[cfg(test)]`-gated seam is strictly required; if so, keep it minimal, name it in
  your report, and never change non-test behaviour.
- Never print or commit a secret. `.env` at the main repo root exists; do not read it
  unless your brief says so.

## Reusable helpers (read them before writing new ones)

- `crate::node::{Registry, Node, NodeContext, seeded_desk, within}` — start a desk
  node on its own thread; `Node::call` runs a closure on the node thread with the
  cache and the kernel clock (`Rc<RefCell<dyn Clock>>`).
- `crate::node::build` (private) is where `LiveNode::builder(...)` is configured; the
  pinned builder has `with_clock_factory(|| Rc<RefCell<dyn Clock>>)` (see
  `nautilus-live-0.62.0/src/node/builder.rs:301`, `nautilus-system-0.62.0/src/builder.rs:228`).
  `nautilus_common::clock::TestClock` has `set_time`/`advance_time` (`clock.rs:870+`).
- `crate::feed::{scripted_server, chart_body, FeedBase::standin, MarketState}` — a
  loopback stand-in quote feed; `crate::trade::{submit, cancel, decide, restore,
  open_orders, history_orders}` and the tests `snapshot_restores_book`,
  `limit_order_history_replays`, `pending_order_approval` in `trade.rs` show the whole
  submit → fill → capture → snapshot → restart flow. `crate::store::open_temp`.
- `crate::hithink` + `feed::poll_cn` is the CN leg; `MARKETRIG_TEST_HITHINK_URL`
  stand-in shape is in `crates/marketrig-acceptance` (H1–H4).
- The `clock.rs` file in this module (written by the first agent) exposes the
  controlled-clock harness once it exists; read it and reuse it.

## Report format (returned to the orchestrator; it goes into FEASIBILITY.md)

For each question you own: `PASS | FAIL | BLOCKED | NOT RUN`, then
- exact commands and exit codes, test names, evidence paths;
- the native events observed (order status sequence, event type names) and the
  source paths/symbols that produce them;
- the smallest supported mechanism you found, and anything the design must change;
- what you could not establish, stated plainly.
Keep prose short and literal; no metaphors.
