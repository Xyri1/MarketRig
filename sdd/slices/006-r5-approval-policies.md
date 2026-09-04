# Slice 006 — R5 approval policies and the events tail in the daemon

**Status:** Frozen — created 2026-09-04; implemented and exit checks green 2026-09-05 (chunks C34–C40; feature SPEC §8 checks 1–7 as module checks; gate G1–G41 green on macOS, bundles `target/acceptance/gate-1788546188/` and `gate-1788546882/`; Windows CI runs on push). Never edited again.
**Implements:** [`features/r5-desktop-approval-controls/`](../features/r5-desktop-approval-controls/SPEC.md) §1 (daemon and CLI parts), §2, §3, §4, and §7.1; the daemon-side half of §6.1 (`--openapi`)
**Exit:** feature SPEC §8 checks 1–7, gate G38–G41 after G37 (with G21's prologue and E3's setup change), and the static checks green on macOS and Windows CI. No attended scenario: R5's evidence is mechanical.

The feature docs are the contract; this slice only pins, names, and orders. On any conflict discovered while implementing, fix the feature docs in the same change and continue — this file is corrected only while Active and never after freeze.

This is the first of R5's three slices (ROADMAP, planned 2026-09-04): daemon only, no window, so the gate keeps growing headless and the REST surface the frontend is generated from stops moving before slice 007 generates from it.

## 1. Pins

Verified against crates.io on 2026-09-04. Chunk numbering, toolchain, and the R0–R4 pins continue unchanged.

| Dependency | Pin | Used by | Notes |
| --- | --- | --- | --- |
| `utoipa` | `=5.5.0` | marketrigd | features `axum_extras`; `ToSchema` on every request, response, and the envelope; `#[utoipa::path]` on every handler. |
| `utoipa-axum` | `=0.2.0` | marketrigd | `OpenApiRouter` + `routes!`; requires `axum ^0.8`, the pinned line. `split_for_parts()` yields the served `Router` and the document. |
| `tokio` | `=1.53.1` (existing) | marketrigd | `sync::Notify` for the post-commit signal; `sync::mpsc` bounded (1 000) per events subscriber. |

`ponytail:` no `utoipa-swagger-ui`, no served `/openapi.json` route — the document is a CLI flag's standard output, because its only consumer is the generator at build time.

## 2. Plan-time settlements

- **Migration 6 (`store/006_r5.sql`):** `installation_settings` created and seeded (`id = 1`, defaults per R5-1); `code_snapshots` and `trading_actions` rebuilt (migration-2 pattern) with `approval` and `decided_at_ns`, backfilled `ALWAYS_ALLOW` / `created_at_ns`, `approved_at_ns` dropped; `operational_events` rebuilt with `POLICY_CHANGED`, `APPROVAL_REQUESTED`, `APPROVAL_DECIDED`. `CHECK ((approval = 'PENDING') = (decided_at_ns IS NULL))` on both tables.
- **Policy read:** one helper `policy::read(tx) -> Policies` executed inside the snapshot-insert and action-insert transactions; no cache, no `ApiState` field.
- **Projection rule (R5-3):** `trigger::projection(...)` gains the snapshot-state argument and is the only place the rule lives; every caller (create, patch, enable/disable, scheduler advance, approve) goes through it. The scheduler's due query is unchanged — eligibility stays folded into `next_occurrence_ns`.
- **Pending order (R5-4):** `trade::submit` splits after `begin` into `gate(policy)`: `ALWAYS_ALLOW` continues as today; `REQUIRE_APPROVAL` returns the record with `202`. `trade::approve(store, node, desk, action_id)` re-runs the post-`begin` half from the stored `request` JSON — the same function body, not a copy. `ORDER_PENDING_APPROVAL` is a `TradeError` variant.
- **Approvals (R5-2):** `policy::approvals` is one `UNION ALL` over the two tables for the listing and two lookups for the decision; the decision handler dispatches to `trigger::decide` or `trade::decide`. Both decision units append `APPROVAL_DECIDED`; both creation units append `APPROVAL_REQUESTED`.
- **Post-commit signal:** `Store::submit` pulses a `Notify` held in `Store` after a successful commit; `events::Publisher` (one task, spawned in `lib.rs::serve` after the listener binds) owns the cursor and the subscriber list under a `std::sync::Mutex`.
- **WebSocket layer (R5-5):** one `api::ws_gate` used by the three socket routes: origin check before upgrade (`403 ORIGIN_REFUSED`), then header bearer if present, else upgrade and `first_frame_auth(socket, 5 s)`. The terminal route's post-upgrade checks map `DESK_NOT_FOUND → 4404`, `NO_LIVE_SESSION → 4409`. The channel route calls the gate with `header_only`.
- **`--openapi`:** `marketrigd --openapi` builds the `OpenApiRouter` with a placeholder state, prints `to_pretty_json()`, exits `0` before step 1 of startup (no data root, no lock). A module check asserts every route in `api::router` appears in `paths`.
- **CLI:** `desk events <desk> [--limit n]` and `history actions <desk>` in `crates/marketrig/src/lib.rs`, tab-separated human lines per feature SPEC §3.3 and §4.3.
- **Seed:** the constitution paragraph (feature SPEC §3.3) appended to `crates/marketrigd/seed/AGENTS.md` under *The paper environment*; R4's byte-for-byte module check and R4 feature SPEC §5.1 updated in the same chunk.
- **Gate prologue:** G21 gains `PUT /settings/policies {"trigger_code_policy":"ALWAYS_ALLOW"}` before its first code trigger; the experiment's E3 setup does the same through the harness's REST client; `EXPERIMENT.md` notes it. G38 restores the default.
- **Test targets:** G38–G41 extend `--test gate` after G37; the harness gains a WebSocket client with first-frame auth (tokio-tungstenite, already pinned) for G38 and G41.

## 3. Chunks

One coding-agent work unit each; a chunk is done when its named checks pass locally. Numbering continues from slice 005.

| # | Chunk | Builds (feature SPEC) | Lands with (§8 checks) | Needs |
| --- | --- | --- | --- | --- |
| C34 | Foundation: pins, migration 6, `policy.rs` (row, resource, `POLICY_CHANGED`), the in-unit read helper, `APPROVAL_*` kinds | §1; §2; §3.1 listing shapes as types | checks 1, 7 | — |
| C35 | Trigger code approval: snapshot state, the projection rule at every site, approve/deny, resource fields, `trigger show` | §3.2 | check 2 | C34 |
| C36 | Paper order approval: pending path, approve re-entry, deny, `ORDER_PENDING_APPROVAL`, `history/actions` route and CLI, MCP tool result, seed paragraph | §3.3 | check 3 | C34 |
| C37 | Approvals listing and decision route, desk scoping, events | §3.1 | check 4 | C35, C36 |
| C38 | Events: post-commit signal, publisher, `WS /events`, listing, `desk events` | §4.1–§4.3 | check 5 | C34 |
| C39 | Sockets and OpenAPI: origin allowlist, first-frame auth on `/events` and the terminal, `utoipa` annotations on every route, `--openapi` | §4.4; §6.1 daemon half | check 6 | C37, C38 |
| C40 | Gate G38–G41, G21 prologue, E3 setup, `EXPERIMENT.md` and AGENTS.md **Commands** refreshed | §7.1 | G38–G41 on both platforms | all |

C35, C36, and C38 run in parallel after C34; C37 after C35 and C36; C39 last before the gate chunk because the annotations touch every handler and must not race other chunks' edits to `api.rs`.

## 4. Execution

Orchestrator plus one coding agent per chunk, sequential along the Needs column, parallel where §3 allows; parallel chunks work in `.worktrees/<chunk>/` per AGENTS.md with a shared `CARGO_TARGET_DIR`. Each agent receives this slice, the feature SPEC, and its chunk row, and delivers a diff with its checks green; the orchestrator runs the static checks after every merge and, before C40, smokes the real binaries on macOS under the seam — set `REQUIRE_APPROVAL` for both policies, create a code trigger and a paper order, list `/approvals`, approve one and deny the other, tail `WS /events` with a first-frame bearer, run `marketrigd --openapi | jq .paths | length`.

## 5. Freeze and merge-back

When the exit checks are green: freeze this slice. Root merge-back waits for slice 008, since R5 merges as one product decision; this slice's durable mechanics are recorded in the feature SPEC until then.
