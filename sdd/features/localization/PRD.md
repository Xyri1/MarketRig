# Localization — PRD

Milestone R7, second item (per D68, D84). Root contract: [`sdd/SPEC.md` §4.5](../../SPEC.md#45-localization).

## 1. Motivation

The initial users read Simplified Chinese. R5 shipped the desktop with every prose string behind `t('…')` and one `en` catalog, a font stack that already names PingFang SC and Microsoft YaHei UI, and a `ponytail:` note on the tray saying the labels stay English until `set_locale` exists. Nothing the human reads is Chinese yet, and nothing records which language the human wants.

The agent must not notice any of this. Trigger code, skills, evidence bundles, the seeded constitution, and both runtimes depend on one English contract; a second-language fork of the CLI, MCP, prompts, or seed files would break the one thing the harness proves (root §4.5). The feature therefore has two halves that pull in opposite directions: everything the human reads follows the locale, and everything the agent reads ignores it, provably.

## 2. Outcome

An operator whose system language is Chinese launches MarketRig for the first time and sees the desktop, the tray menu, and every notification in Simplified Chinese; the operator can change the language in Settings at any time and the whole surface follows without a restart. Under either language, `marketrig`, the MCP surface, every daemon prompt, every seeded file, and every log line are byte-identical to the English run, and the gate proves that on stand-ins.

## 3. Scope

- One installation `locale` setting held by the daemon, `en` or `zh-Hans`, unset until the desktop's first launch detects it from the system language, readable and writable through one REST resource, read by the daemon for nothing else.
- The `zh-Hans` catalog with the same key tree and the same placeholders as `en`, switched at runtime through vue-i18n's Composition API; the frontend applies the stored locale at startup and on change.
- A **Language** section at the top of Settings, which is also the onboarding language step because Settings is the whole of first-launch onboarding (R5 PRD §4).
- The tray menu relabelled by the shell's `set_locale` command; notifications titled from the active catalog.
- The agent-facing byte-identity: no daemon, CLI, or adapter code path reads the locale except the settings resource, and gate scenario L1 compares the agent-facing surfaces byte for byte under both locales.
- The `marketrig` CLI's UTF-8 output on both platforms, recorded as the property the standard library already gives and checked once.
- The packaged smoke runs in `zh-Hans`, which is the R7 evidence line's desktop half.

## 4. Non-goals

- A third locale, a per-desk language, or a localized agent-facing surface (root PRD non-goals).
- Traditional Chinese: a `zh-Hant` system resolves to a catalog MarketRig does not ship and falls back to `en` like any other language; the operator chooses `zh-Hans` in Settings if they prefer it. A `zh-Hant` catalog is deferred (root §18).
- Locale-specific number, currency, or date formatting: financial values stay the daemon's canonical decimal text (root §4.5), and the desktop formats no instant today, so there is nothing to localize.
- Translating desk names, instrument identifiers, currency codes, provider names, event kinds, error codes, or the product name.
- A translation workflow, a string-extraction tool, or a hosted translation service: two JSON files, edited by hand, checked for parity by the existing catalog test.
- Localized packaging metadata beyond the application name, which is the product name and never translated; the installer is R7's packaging item.
- An input-method or font shipped by MarketRig: both platforms ship CJK fonts and input methods, and the desktop's font stacks already name them.

## 5. Success criteria

1. First launch on a `zh-CN` system shows Settings in Simplified Chinese, the tray's three labels in Simplified Chinese, and the stored locale `zh-Hans`; on an `en-US` system the same first launch stores `en`.
2. Changing the language in Settings re-renders the desktop, the tray, and the next notification without a restart, and the stored locale survives a restart.
3. Gate L1 is green on macOS and Windows: under `zh-Hans`, the seeded `AGENTS.md` and skills, a firing document, a delivered prompt's text, `marketrig --help`, an error envelope, and the OpenAPI document are byte-identical to the same artifacts under `en`.
4. The catalog test fails on a key present in one catalog and missing from the other, on a placeholder present in one language's string and missing from the other's, and on a bare string in any template, as it does today.
5. The packaged smoke passes once per platform in `zh-Hans`, recorded in the implementing slice's evidence line beside the R5 `en` runs.
