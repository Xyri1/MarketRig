# Localization — Decisions

Local prefix `LZ-`; summarized as one product decision when merged (D68 is amended in place, not replaced). Root basis: D68, D42 (two configuration scopes), D66 (the shell performs no HTTP), D52 (notifications are for actionable conditions), D4 (the agent surfaces).

## Settled decisions

### LZ-1 — The locale is one nullable column on `installation_settings`, one REST resource, and nothing else

**Decision:** Migration 11 adds `locale TEXT CHECK (locale IN ('en','zh-Hans'))`, nullable, `NULL` until written. `GET /settings/locale` answers `{ "locale": "en" | "zh-Hans" | null }`; `PUT /settings/locale { "locale" }` accepts exactly the two values, anything else `400 VALIDATION`, writes the column with `updated_at_ns`, and answers the resource. No operational event, no history, no CLI command: nothing reads a locale change after the fact.

**Rationale:** Root §4.4 names desktop locale an installation setting and the table for those exists (per D82). `NULL` is the honest "not chosen yet" that lets the desktop detect once and never again (LZ-2); a seeded `en` default would make a Chinese first launch flash English and then require a write the operator did not make. A `POLICY_CHANGED`-style event would be evidence with no reader.

`ponytail:` a second column on the one-row table, not a `settings` key-value layer, which root §4.4 forbids.

### LZ-2 — The webview detects the language once, from `navigator.language`, and stores the answer

**Decision:** On every startup, after health, the frontend reads `GET /settings/locale`. A stored value is applied. A `null` is detected by locale lookup: for each tag of `navigator.languages` in preference order, canonicalize it with CLDR likely subtags (`Intl.Locale.maximize()`, so `zh-CN` becomes `zh-Hans-CN` and `zh-TW` becomes `zh-Hant-TW`), then try the tag and each right-truncation of it against the shipped catalogs; the first hit wins and no hit is `en`. The result is written through `PUT /settings/locale` before it is applied, so the daemon holds the choice from the first launch on. The detection rule is one pure function with its own unit check.

**Rationale:** This is the lookup every locale-aware platform performs (BCP 47 lookup over likely subtags, as ICU, the operating systems, and vue-i18n's own fallback chain do), and the browser carries the CLDR data, so no library or table is added. The system webview reports the OS display languages on both platforms (WKWebView the app's preferred languages, WebView2 the Windows display languages), which is what root §4.5 asks for and needs no plugin, no OS crate, and no shell command. Storing before applying makes the desktop, tray, and daemon agree from the first frame after detection. A Traditional Chinese system resolves to `zh-Hant`, which ships no catalog, so it falls back to `en` like any other unshipped language; the visible Language control is one click away.

`ponytail:` no re-detection when the OS language changes later; the stored setting wins, and the operator changes it in Settings.

### LZ-3 — Two catalogs, one key tree, switched through vue-i18n's global locale

**Decision:** `src/locales/zh-Hans.json` mirrors `en.json` key for key and placeholder for placeholder. `createI18n` gets both message trees, `locale: 'en'`, `fallbackLocale: 'en'`, and `legacy: false` as today; applying a locale is `i18n.global.locale.value = locale`, `document.documentElement.lang = locale`, and the shell's `set_locale` (LZ-4), in that order, from one `applyLocale` function that Settings and startup both call. Components keep using `useI18n()` and `t('…')`; no component reads the locale itself. The catalog test grows two assertions: the two key sets are equal, and for every leaf the set of `{name}` placeholders is equal. Values that are legitimately identical in both languages (the product name, `API key`, `URL`) are not flagged.

**Rationale:** The R5 SPEC §6.5 promised "R6 adds `zh-Hans.json` and nothing else", and vue-i18n's reactive global locale makes that true: every mounted `t()` re-renders on the switch, including the notification titles resolved at send time (`src/notifications.ts`). vue-i18n `11.4.10` is the pinned release and the newest published tag (npm `dist-tags.latest`, checked 2026-09-12); the 11 line is the one D68's "newest stable release line" names.

`ponytail:` no lazy-loaded catalogs; two JSON files of under 200 keys ship in the bundle.

### LZ-4 — `set_locale` re-texts the tray from a label table the shell owns

**Decision:** The shell gains the D68 command `set_locale(locale: String)`, which refuses anything but the two values, stores the locale in managed state beside `TrayPending`, and calls `set_text` on the three menu items with the labels of that locale; `set_tray_pending` reads the stored locale for its `n pending approvals` line, and on Windows the tooltip. The labels are one Rust `match` over `(locale, item)` in `src-tauri/src/lib.rs` — six strings plus the pending format — not a catalog file the shell parses. The shell starts English and the webview calls `set_locale` on every startup after LZ-2, so the tray is English for the second or so before the webview has read the setting.

**Rationale:** Tauri 2's `MenuItem::set_text` already re-texts the pending line, so re-texting three items needs no menu rebuild. The shell holds no state but the tray (per D66) and performs no HTTP, so the webview, which has the setting, tells it. Six strings do not justify a catalog format on the Rust side.

`ponytail:` the English-first second at launch stands; removing it would need the shell to read the daemon's setting itself, which D66 forbids, or a settings file, which D42 forbids.

### LZ-5 — Byte-identity is proven by comparison, not asserted by review

**Decision:** No code in `marketrigd`, `marketrig`, or `marketrig-mcp` reads `installation_settings.locale` except the settings route, and no prompt renderer, seed writer, firing-document builder, error envelope, CLI help, or log line takes a locale parameter. Gate scenario L1, after T5, sets `zh-Hans`, produces one of each agent-facing artifact, sets `en`, produces them again, and asserts byte equality of every pair. The same artifacts are listed in the SPEC so the list can grow with the surfaces.

**Rationale:** Root §4.5 is a guarantee about the agent's world, and the acceptance layer that proves guarantees on stand-ins is the gate (per D75). A grep for `locale` is a review habit, not evidence; a byte comparison under both settings is.

### LZ-6 — The CLI's UTF-8 output is the standard library's, recorded and checked once

**Decision:** `marketrig` writes UTF-8 to standard output and error and adds no code to do so: Rust's standard library writes raw UTF-8 bytes to a pipe or file on both platforms and, on a Windows console handle, converts to UTF-16 for `WriteConsoleW`, so a Chinese desk artifact survives `marketrig … --json | …` and `> file` under any code page. One module check pipes a trigger whose `brief` is Chinese through both the JSON and the plain output and compares bytes.

**Rationale:** D68's "writes UTF-8 unconditionally" is a property, not a feature; the check is what keeps a later change from breaking it silently.

`ponytail:` a legacy console with a raster font may still render a box for a CJK glyph; that is the console's rendering, not the bytes, and outside the contract.

### LZ-7 — Fonts and input methods are the platform's; MarketRig names them and ships none

**Decision:** The UI stack stays `system-ui, -apple-system, "Segoe UI Variable", "PingFang SC", "Microsoft YaHei UI", sans-serif` and the terminal stack stays as R5 set it, with the platform's CJK monospace fallback filling gaps. Chinese input in the terminal well relies on xterm.js's composition handling over its hidden textarea and slice 009's Unicode 11 width tables; the desk-name field is kebab-case ASCII by identity (per D2) and never takes Chinese. Neither is a runnable check: the smoke's `zh-Hans` run types one Chinese line into the well and asserts the bytes reached the stand-in, and the rest is the operator's eyes on the packaged application, recorded in the slice's evidence line.

**Rationale:** Both platforms ship the fonts and the input methods; shipping a font would add tens of megabytes for no behaviour. Width handling and reconnect continuity were fixed for CJK by slice 009 and are covered there.

## Mechanics reserved for the SPEC session

- The exact route shapes, the migration text, the detection function's signature, the tray label table, the catalog test's parity assertions, L1's artifact list, and the smoke's `zh-Hans` steps are in [SPEC.md](SPEC.md).
