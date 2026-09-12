# Localization — SPEC

_Decision basis: LZ-1 … LZ-7 (this feature); root D68, D42, D66, D52, D4._

Refines root [§4.5](../../SPEC.md#45-localization); the root section keeps the contract, this document the mechanics it deferred: detection, endpoint, catalog mechanics, font and input-method requirements, and parity checks.

## 1. The setting (LZ-1)

### 1.1 Migration 11

```sql
ALTER TABLE installation_settings
  ADD COLUMN locale TEXT CHECK (locale IN ('en','zh-Hans'));
```

Nullable; the one row's value is `NULL` after the migration. `updated_at_ns` is stamped by a locale write as by a policy write.

### 1.2 Routes

| Route                  | Body                              | Answer                                             |
| ---------------------- | --------------------------------- | -------------------------------------------------- |
| `GET /settings/locale` | —                                 | `200 { "locale": "en" \| "zh-Hans" \| null }`      |
| `PUT /settings/locale` | `{ "locale": "en" \| "zh-Hans" }` | `200` the resource; anything else `400 VALIDATION` |

Both are `#[utoipa::path]` routes with a `ToSchema` body so the generated client carries the type; both need the bearer like every route. A write that does not change the value still answers `200` and stamps nothing. No event is published and no CLI command exists.

### 1.3 Scenarios

- A fresh data root: `GET` answers `null`.
- `PUT {"locale":"zh-Hans"}`, restart the daemon, `GET` answers `zh-Hans`.
- `PUT {"locale":"zh"}`, `PUT {"locale":"zh-Hant"}`, `PUT {}` each answer `400 VALIDATION` and leave the column as it was.

## 2. Detection and application (LZ-2, LZ-3)

### 2.1 The detection rule

`src/locale.ts` exports one pure function, BCP 47 lookup over CLDR likely subtags against the shipped catalogs:

```ts
export const LOCALES = ["en", "zh-Hans"] as const;
export type Locale = (typeof LOCALES)[number];

export function detectLocale(preferred: readonly string[]): Locale {
  for (const tag of preferred) {
    let full: string;
    try {
      full = new Intl.Locale(tag).maximize().baseName; // zh-CN → zh-Hans-CN
    } catch {
      continue; // "" or a malformed tag
    }
    const parts = full.split("-");
    while (parts.length) {
      const candidate = parts.join("-");
      if ((LOCALES as readonly string[]).includes(candidate))
        return candidate as Locale;
      parts.pop();
    }
  }
  return "en";
}
```

Startup calls it with `navigator.languages`. Adding a catalog is one entry in `LOCALES`.

| `navigator.languages`                                | Maximized first tag             | Result    |
| ---------------------------------------------------- | ------------------------------- | --------- |
| `["zh-CN"]`, `["zh"]`, `["zh-SG"]`, `["zh-Hans-CN"]` | `zh-Hans-*`                     | `zh-Hans` |
| `["zh-TW"]`, `["zh-HK"]`, `["zh-Hant"]`              | `zh-Hant-*`                     | `en`      |
| `["ja", "zh-CN"]`                                    | `ja-Jpan-JP`, then `zh-Hans-CN` | `zh-Hans` |
| `["en-US"]`, `[""]`, `[]`                            | —                               | `en`      |

### 2.2 Startup

After `useDaemon` has authenticated health, `App.vue` reads `GET /settings/locale`. A value is applied. `null` is detected from `navigator.languages` by §2.1, written through `PUT /settings/locale`, then applied; a failed write applies the detected value anyway for this run and leaves the column `null` for the next launch to try again. The `en` catalog is what renders until the read answers, which is the same English R5 renders today.

### 2.3 `applyLocale`

```ts
export async function applyLocale(locale: "en" | "zh-Hans"): Promise<void> {
  i18n.global.locale.value = locale;
  document.documentElement.lang = locale;
  await invoke("set_locale", { locale });
}
```

The one function startup and the Settings control call. Outside Tauri (Vitest, a browser during `pnpm dev` without the shell) `invoke` rejects; the rejection is caught and logged and the first two lines still hold.

### 2.4 The catalogs

`src/locales/zh-Hans.json` carries every key of `en.json` with the same nesting; `src/i18n.ts` becomes:

```ts
export default createI18n<[MessageSchema], "en" | "zh-Hans">({
  legacy: false,
  locale: "en",
  fallbackLocale: "en",
  messages: { en, "zh-Hans": zhHans },
});
```

Placeholders are vue-i18n named interpolation, `{desk}` and `{detail}`, and every string that has one in `en` has the same names in `zh-Hans`. Word order differs between the languages, so a placeholder's position is free; its presence is not.

Never translated, in either catalog or anywhere the frontend composes text: `MarketRig`, desk names, instrument identifiers, currency codes, `codex` and `claude`, provider names (`HiThink`, `OpenViking`), event kinds, error codes, policy values as codes. Their human labels (`Always allow` → `始终允许`) are catalog entries; the codes behind them are not.

Financial values render the daemon's decimal text unchanged (root §4.5). The desktop formats no instant and no number through `d()` or `n()` today; the first surface that needs one adds a named format to `createI18n` and this section.

### 2.5 The Language control

The first section of the Settings tab, above **Runtimes**, titled by `settings.language.title`; one native `<select>` whose two options are labelled in their own language, `English` and `简体中文`, as literals under keys `settings.language.en` and `settings.language.zhHans` whose values are identical in both catalogs. Changing it calls `PUT /settings/locale` and, on `200`, `applyLocale`; on failure the select snaps back and the daemon-unavailable banner already covers the cause. Because Settings is auto-selected while no runtime is `AVAILABLE`, this section is the onboarding language step the roadmap names, with no wizard added.

## 3. Tray and notifications (LZ-4, LZ-3)

### 3.1 `set_locale`

```rust
#[tauri::command]
fn set_locale(locale: String, app: AppHandle, tray: State<'_, Tray>) -> Result<(), String>
```

Refuses anything but `"en"` and `"zh-Hans"` with `Err("VALIDATION: …")`. Stores the locale in the managed `Tray` state (which absorbs `TrayPending`: the pending item, the open and quit items, the locale, and the last `n`), then re-texts the three items and, on Windows, the tooltip. The labels:

| Item      | `en`                    | `zh-Hans`        |
| --------- | ----------------------- | ---------------- |
| `open`    | `Open MarketRig`        | `打开 MarketRig` |
| `pending` | `{n} pending approvals` | `{n} 项待审批`   |
| `quit`    | `Quit MarketRig`        | `退出 MarketRig` |

`tray_label(locale, item, n) -> String` is the one function both `set_locale` and `set_tray_pending` call; `pending_label` folds into it. The shell starts English and the webview's startup calls `set_locale` after §2.2, so a launch shows English labels until then. `set_locale` is an app command and needs no capability entry (`src-tauri/capabilities/main.json` explains why).

### 3.2 Notifications

Unchanged in code: `src/notifications.ts` resolves `notify.<KIND>.title` and `.body` through `i18n.global.t` at send time, so a notification sent after `applyLocale` is in the active language. The `{detail}` line stays the English payload fragment it is today (an instrument, a failure code, a title), because those are agent-facing identifiers and never translated.

## 4. What stays byte-identical (LZ-5, LZ-6)

The agent-facing artifacts, each produced under both locales by L1 (§6.1) and compared byte for byte:

1. the seeded `AGENTS.md` and every seeded skill file of a desk created under that locale;
2. the firing document `trigger-code` receives for a code-bearing trigger, and the `TRIGGER_RESULT` prompt text read from the prompt row;
3. an `EVALUATION` prompt text and an `ORIENTATION` prompt text from the prompt rows;
4. `marketrig --help`, `marketrig trigger --help`, and the plain and `--json` output of `marketrig desk show`;
5. one error envelope, `GET /desks/<unknown>` → `DESK_NOT_FOUND`, and one CLI error line, `marketrig desk show <unknown>`;
6. `marketrigd --openapi`;
7. the `marketrig-mcp` resource and tool listing, read through the adapter's stdio `resources/list` and `tools/list`;
8. the daemon's stderr lines for the steps above, after the timestamps are stripped.

No code path in the three Rust binaries reads `installation_settings.locale` but `GET /settings/locale`; the SQL that reads it appears once.

UTF-8: one module check in `marketrig` creates a trigger whose `--brief` is `交易复盘`, runs `trigger show` in plain and `--json` form with stdout as a pipe, and asserts the exact UTF-8 bytes of the brief in both; the check runs on both CI platforms.

## 5. Fonts and input (LZ-7)

`--font-ui` and `--font-terminal` stay as `src/style.css` sets them; nothing is bundled. The smoke's `zh-Hans` run types `你好` into the well and asserts those bytes reached the stand-in's input (its banner echoes `INPUT n:` lines, as smoke step 3 already relies on). Rendering quality and the OS input-method popup are confirmed by the operator on the packaged application and written into the slice's evidence line.

## Surfaces

| Surface  | Change                                                                                                                                                     |
| -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Daemon   | migration 11; `GET`/`PUT /settings/locale`; OpenAPI regenerated                                                                                            |
| CLI      | none (the UTF-8 check only)                                                                                                                                |
| MCP      | none                                                                                                                                                       |
| Shell    | `set_locale`; `Tray` state and `tray_label`                                                                                                                |
| Frontend | `src/locale.ts` (`detectLocale`, `applyLocale`); `src/locales/zh-Hans.json`; `src/i18n.ts` two locales; Settings **Language**; `index.html` `lang` follows |
| Seeds    | none                                                                                                                                                       |
| Gate     | L1 after T5                                                                                                                                                |
| Smoke    | the `zh-Hans` run                                                                                                                                          |

## 6. Acceptance

### 6.1 Gate scenario (continuing the chain after T5)

**L1 — byte-identity under both locales.** `PUT /settings/locale {zh-Hans}`; create desk `l1-zh` with the seeded skills; create one code-bearing trigger and invoke it (T1's path) so a firing document, an execution, and a `TRIGGER_RESULT` prompt exist; close one round trip on the stand-in feed so an `EVALUATION` prompt exists (G13's path); collect the eight artifact groups of §4. `PUT /settings/locale {en}`; repeat with desk `l1-en`; compare each pair after substituting the two desk ids and names, the two trigger ids, firing ids, prompt ids, and request ids, and stripping stderr timestamps. Every pair is byte-equal. `GET /settings/locale` answers `en` at the end so the later scenarios see the seeded default's equivalent. The gate registers no runtime here, as T1–T5 do not, so every queued prompt resolves `RUNTIME_UNAVAILABLE`; the texts under comparison are the stored prompt rows.

### 6.2 The packaged smoke in `zh-Hans`

`pnpm smoke` gains, between its steps 1 and 2: `PUT /settings/locale {zh-Hans}` through the bearer; the Settings heading reads the `zh-Hans` value of `settings.runtimes.title`; the smoke keeps selecting by `data-testid`, so no other label is read, and the one text assertion imports `src/locales/zh-Hans.json` rather than repeating the string. Step 2 additionally types `你好` into the well and waits for the stand-in's `INPUT` echo of those bytes. The R5 `en` smoke is not rerun: the `zh-Hans` run drives the same steps and is the R7 evidence line's desktop half. One run per platform, operator-run, recorded in the slice.

## 7. Required checks

Module checks (`cargo test -p marketrigd` unless named; fakes allowed):

1. `store` — migration 11 on a migration-10 database keeps every row, adds the nullable column at `NULL`, and rejects `'zh'` and `'zh-Hant'` through the `CHECK`.
2. `policy::locale` — `GET` answers `null` on a fresh root; `PUT` each valid value then `GET`; `VALIDATION` on the three bad bodies of §1.3; the value after a daemon restart.
3. `marketrig` (`cargo test -p marketrig`) — the UTF-8 pipe check of §4, both platforms.
4. `marketrig-desktop` (`cargo test -p marketrig-desktop`) — `tray_label` for every `(locale, item)` and `n ∈ {0, 1, 12}`; `set_locale("zh")` is refused.
5. Frontend, through `pnpm check`:
   - `src/test/catalog.test.ts` — the existing bare-string and existing-key checks, plus: the key sets of `en.json` and `zh-Hans.json` are equal; for every leaf, the `{name}` placeholder sets are equal; the two `settings.language.*` values are identical in both.
   - `src/locale.test.ts` — the table of §2.1; `applyLocale` sets the global locale and `documentElement.lang` and swallows a rejected `invoke`.
   - `src/components/SettingsTab.test.ts` — the select writes `PUT /settings/locale` against the fake daemon, applies on `200`, and snaps back on an error.
   - `src/notifications.test.ts` — a notification sent after `applyLocale("zh-Hans")` carries the `zh-Hans` title.
   - `App` — a `null` locale with `navigator.languages = ["zh-CN"]` writes `zh-Hans` once and renders it; a stored `en` writes nothing.
6. Gate L1 after T5 on macOS and Windows CI.
7. The packaged smoke in `zh-Hans`, once per platform, operator-run and recorded in the implementing slice's evidence line.

## 8. Root reconciliation this SPEC requires

When the implementing slice freezes:

- root §4.5: name the resource, the migration, the detection rule, and `applyLocale`; delete the "deferred to the localization feature specification" sentence.
- root §4.3 and §5 (the shell): `set_locale` exists; replace "Tray labels are English until R7" with the English-first-second of LZ-4.
- root §15: migration 11 in the migration paragraph.
- root §17: L1 after T5 and the `zh-Hans` smoke.
- root §18: drop the localized-copy deferral for surfaces this feature now covers; keep it for surfaces later milestones add.
- D68: amend in place with the LZ summary; ROADMAP R7 records delivery; AGENTS.md's gate list gains L1.
- `src-tauri/src/lib.rs`: the `ponytail:` note on the tray labels is retired.
