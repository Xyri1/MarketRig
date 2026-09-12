# Slice 016 — Localization

**Status:** Active (2026-09-12). No step implemented; no runtime pass claimed.

The implementation plan for [`features/localization/`](../features/localization/PRD.md), decisions LZ-1–LZ-7 and SPEC §1–§8. The feature folder is canonical; drift found while implementing is corrected there in the same change. Root §4.5 keeps the contract and defers these mechanics to the feature, so the root merge at the freeze is the list in feature SPEC §8 and nothing more.

## Outcome

A Chinese-language system launches MarketRig for the first time and sees the desktop, the tray, and every notification in Simplified Chinese; Settings changes the language without a restart; the daemon holds the choice across restarts. Under either locale `marketrig`, `marketrig-mcp`, every prompt, seeded file, log line, and the OpenAPI document are byte-identical to the English run, and gate L1 proves it.

## Boundaries

- Worktree `.worktrees/localization/` on a fresh branch; every spawned binary under a scratch `MARKETRIG_TEST_DATA_ROOT`.
- Frozen slices and feature SPECs are not edited. R5's `ponytail:` note on the tray labels is retired by step 3, and its SPEC §6.5 sentence "R6 adds `zh-Hans.json` and nothing else" stays as frozen history.
- No new crate, dependency, table, event kind, CLI command, or MCP change. The locale is one column and one route; the shell gains one command; the frontend gains one module, one catalog, and one Settings section.
- Nothing in `crates/marketrig`, `crates/marketrig-mcp`, `crates/marketrigd/seed/`, or the prompt renderers changes, and no code path but the settings route reads the column. A step that finds itself editing one of those is drifting and stops.
- The generated client changes (one new route): `pnpm generate` runs and `openapi.json` and `src/client` are committed in the same change.

## Implementation sequence

Each step leaves its focused checks green before the next.

### 1. The setting (feature SPEC §1)

Touch `crates/marketrigd/src/store/011_locale.sql`, `store.rs`, `policy.rs`, `api.rs`.

- Migration 11: `ALTER TABLE installation_settings ADD COLUMN locale TEXT CHECK (locale IN ('en','zh-Hans'))`, appended to the `include_str!` list. No rebuild, no backfill.
- `policy::locale_get` and `policy::locale_put` beside the policies resource: `PUT` parses `{locale}`, refuses anything but the two values as `VALIDATION`, writes the column and `updated_at_ns` in one unit, answers `{locale}`. `GET` answers `{locale: null}` on the fresh row.
- `GET`/`PUT /settings/locale` behind the bearer with `#[utoipa::path]` and a `ToSchema` body; `pnpm generate` afterwards.

Checks: `store::locale_migration_applies` (rows kept, column `NULL`, `'zh'` and `'zh-Hant'` refused by the `CHECK`), `policy::locale_resource` (§1.3's scenarios, including a daemon restart), `api::openapi_lists_locale`.

### 2. Detection, catalog, and the Language control (feature SPEC §2)

Touch `src/locale.ts`, `src/locale.test.ts`, `src/i18n.ts`, `src/locales/zh-Hans.json`, `src/App.vue`, `src/components/SettingsTab.vue`, `src/test/catalog.test.ts`, `index.html`.

- `src/locale.ts`: `LOCALES`, `Locale`, `detectLocale(preferred)` exactly as §2.1, and `applyLocale` as §2.3 with the `invoke` rejection caught and logged.
- `src/i18n.ts` registers both catalogs with `fallbackLocale: "en"`; `zh-Hans.json` mirrors `en.json` key for key, placeholders preserved, the never-translated identifiers of §2.4 left as they are.
- `App.vue`, after health: `GET /settings/locale`; a value applies; `null` detects from `navigator.languages`, `PUT`s, then applies, and a failed `PUT` applies anyway for this run (§2.2).
- `SettingsTab.vue`: the **Language** section first, a native `<select>` over `LOCALES` labelled `settings.language.en` and `settings.language.zhHans`; change → `PUT` → `applyLocale` on `200`, snap back otherwise.
- `catalog.test.ts` gains the two parity assertions of §7.5 and reads both catalogs; the existing bare-string and existing-key checks now run the key lookup against `en` as before.

Checks: `src/locale.test.ts` (the §2.1 table; `applyLocale` sets the global locale and `documentElement.lang`, survives a rejected `invoke`), `catalog.test.ts` parity, `SettingsTab.test.ts` (write, apply, snap back against the fake daemon), `notifications.test.ts` (a title in `zh-Hans` after `applyLocale`), an `App` check for the `null`-then-detect path writing once and a stored value writing nothing; `pnpm check` green.

### 3. The tray (feature SPEC §3)

Touch `src-tauri/src/lib.rs`.

- `TrayPending` becomes `Tray { open, pending, quit, locale, n }` under one `Mutex`; `tray_label(locale, item, n) -> String` holds the six labels and the pending format; `pending_label` folds into it.
- `set_locale(locale)`: refuse anything but the two values, store, re-text the three items, and on Windows the tooltip; `set_tray_pending` reads the stored locale. Registered in `generate_handler!`; no capability change. The R5 `ponytail:` note is retired.

Checks: `cargo test -p marketrig-desktop` — `tray_label` for every `(locale, item)` and `n ∈ {0, 1, 12}`; `set_locale("zh")` refused.

### 4. UTF-8 and byte-identity (feature SPEC §4)

Touch `crates/marketrig/tests/` (or the crate's existing check module), `crates/marketrig-acceptance/tests/gate.rs`.

- The `marketrig` module check of §4: a trigger whose `--brief` is `交易复盘`, `trigger show` plain and `--json` with stdout as a pipe, the exact bytes asserted; runs on both CI platforms.
- Gate L1 as its own `localization(g, …)` function called after `invocation(…)`, so `gate()` grows one line (slice 015's rustfmt lesson). It sets `zh-Hans`, builds desk `l1-zh` and the artifacts of §4 through T1's and G13's paths, sets `en`, repeats as `l1-en`, substitutes the ids and names, strips stderr timestamps, asserts byte equality per pair, and leaves the locale at `en`. Each pair is one observation line naming the artifact.

Checks: the CLI check; `cargo test -p marketrig-acceptance --test gate` green through L1 on macOS.

### 5. The packaged smoke in `zh-Hans` (feature SPEC §6.2)

Touch `smoke/smoke.spec.ts`.

- Between steps 1 and 2: `PUT /settings/locale {zh-Hans}` with the bearer; the Settings heading equals the imported `zh-Hans` value of `settings.runtimes.title`. Step 2 types `你好` into the well and waits for the stand-in's `INPUT` echo of those bytes. Every other selector stays a `data-testid`.
- Operator-run once per platform, `MARKETRIG_SMOKE_WIPE=1 pnpm build` then `MARKETRIG_SMOKE_WIPE=1 pnpm smoke`, per AGENTS.md; the operator also confirms the tray labels, one notification, and the input-method popup by eye and records them here.

## Exit checks

- Feature SPEC §7's seven checks pass; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm generate` (no diff after commit), `pnpm check` — on macOS and in CI on both platforms.
- The gate passes on both platforms through L1 with its evidence bundle.
- The `zh-Hans` smoke passes on macOS with its report under `target/acceptance/smoke-macos-<stamp>/`; the Windows run is recorded here when run and is not a freeze condition if the macOS run and CI are green (as slices 014 and 015 were frozen).
- `grep -rn locale crates/marketrig crates/marketrig-mcp crates/marketrigd/seed crates/marketrigd/src` names only the settings route, the migration, and the `session.rs` and `dispatch.rs` comments that already say the agent surface ignores it.

After these checks, freeze this slice and perform feature SPEC §8's root reconciliation: root §4.5, §4.3/§5, §15, §17, §18; amend D68 in place with the LZ summary; ROADMAP's R7 item records the delivery; AGENTS.md's repository state, gate list (L1 after T5), and the smoke sentence; allocate no new product D number.

## Verification record

Planning (2026-09-12): feature folder complete, local links checked; no runtime pass claimed.
