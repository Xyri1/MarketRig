# Slice 013 — HiThink A-share data

**Status:** Active (opened 2026-09-08).

The implementation plan for Milestone R6, designed in [`features/hithink-a-share/`](../features/hithink-a-share/PRD.md) (PRD, DECISIONS HT-1 … HT-6, SPEC §1–§7) and settled as D84. The feature folder is canonical; drift found here is corrected there in the same change.

## Outcome

The `CN` catalog instruments trade on HiThink's snapshot behind one operator key, their phase respects HiThink's trading-day calendar, an agent reads any HiThink endpoint through `marketrig research hithink`, and every new desk carries the vendored skill rewritten to name that one path. Yahoo keeps US and Hong Kong. The key is on no runtime, file, prompt, log, event, or CLI output. The gate proves it on a loopback stand-in; E7 proves it once per cell on the real service.

## Implementation sequence

Chunks land in order on `master`, each green on its named checks before the next starts, each keeping **Commands** in `AGENTS.md` current when a command changes. Check names cite the feature SPEC §7.

- **C59 — Vendoring and the rewrite script** (SPEC §5.1, §5.2). `vendor/hithink-finance/` at a pinned commit with `VENDOR.md` and `LICENSE`; `scripts/hithink-skill.mjs` and `scripts/hithink-skill-preamble.md`; the committed output under `crates/marketrigd/seed/skills/hithink-finance/` and the generated `research_paths.rs`; CI's diff check. Pin the commit in HT-5 and the feature SPEC §5.1 in the same change. Checks: `skill::rewritten_examples_route_through_marketrig`, `skill::seed_is_current`, `research::allowlist_matches_capability_map`.
- **C60 — Migration 9 and the provider** (§1). The `hithink_provider` row with `a_share_feed`, credential-store account, `GET`/`PUT`/`PATCH`/`DELETE /research/hithink`, the bounded validation request, `HITHINK_PROVIDER_CHANGED`, the `MARKETRIG_TEST_HITHINK_URL` seam. Checks: `store::migration_9_applies`, `provider::validate_with_one_bounded_request`, `provider::seam_only_with_data_root`, `provider::key_never_answered`, `provider::toggle_requires_key`.
- **C61 — Catalog and the `CN` feed** (§2). `feed` and `feed_symbol` on every entry; the `hithink` `DataClient` beside the Yahoo one on the node, the per-cycle `a_share_feed` routing, batched polling, change detection, HiThink's retry table, `2003` flipping the row, `UNAVAILABLE` without a key; `provider` and null `source_time_ns` on the observation and the MCP quote resource; `instruments` lists `feed`. Checks: `catalog::entries_valid`, `feed::hithink_batches_one_request`, `feed::a_share_feed_switch`, `feed::hithink_retry_bound`, `feed::hithink_change_detection`, `feed::observation_provenance`.
- **C62 — The calendar** (§3). Trading-days fetch at first `CN` subscription and after Shanghai midnight, the in-memory set, the `calendar` field, the stand-in cadence lift. Check: `feed::cn_phase_from_trading_days`.
- **C63 — The research passthrough** (§4). The route, the allowlist, the spacing gate, the size ceiling, the codes; `marketrig research hithink` with `--param`, `--out`, and the 256 KiB spill. Checks: `research::codes`, `research::spacing`, `cli::research_spill`.
- **C64 — Seeding** (§5.3). The `hithink-finance` upload beside `desk-improvement` at desk creation; the constitution paragraph for desks created from here on. Covered by C59's checks plus H4.
- **C65 — Stand-in and gate H1–H4** (§6.1, §6.2). The HiThink routes and script controls on the acceptance stand-in server; H1–H4 after O10; green on both platforms in CI.
- **C66 — Desktop** (§1.4). The Settings block with the tick box, client regeneration, Vitest. Frontend checks.
- **C67 — E7** (§6.3). One attended run per cell on the real service; bundles named here.

## Exit checks

- Every module check in the feature SPEC §7 green on macOS and Windows CI.
- `cargo test -p marketrig-acceptance --test gate` green through H4 on both platforms, with the bundle's `marketrig.db`, log root, events, launch files, workspace files, and CLI captures grep-clean of the gate's key.
- `pnpm check` green; the seed diff check green.
- E7 bundles for the four cells, each showing the provider row `AVAILABLE`, a `CN` observation with `provider: "hithink"` and a named calendar, and a closed cycle with its queued evaluation.

Then freeze this slice and merge back: the root SPEC §4.4, §12.2, §13.2, §16, and §17 gain the concrete mechanics; D84 gains its `ponytail` record if any ceiling moved; ROADMAP R6 records the evidence; `AGENTS.md` **Commands** carries the vendoring script and the seam variable.
