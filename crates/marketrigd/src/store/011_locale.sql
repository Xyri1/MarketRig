-- Migration 11 — the desktop locale (feature SPEC `localization` §1.1, per
-- LZ-1). Forward-only; never edited.
--
-- One nullable column on the one settings row: NULL is "not chosen yet", which
-- is what lets the desktop detect a language once and store the answer. No
-- rebuild and no backfill — a seeded default would make a Chinese first launch
-- flash English, and nothing the agent reads takes a locale at all (§4).
ALTER TABLE installation_settings
  ADD COLUMN locale TEXT CHECK (locale IN ('en','zh-Hans'));
