# HiThink A-share data — Feature SPEC

*Decision basis: per D4, D9, D42, D49, D75, D76, D78, D83, D84; HT-1 … HT-6.*

This specification refines root `SPEC.md` §4.4, §12.1, §12.2, §13.2, §16, and §17. Everything the daemon types from HiThink is in §2 and §3; everything else crosses the daemon as bytes. HiThink facts cite the upstream contract at the pinned commit (HT-5); the version line checked is the `main` branch on 2026-09-08.

## 1. Provider setup (HT-1)

### 1.1 The row

`hithink_provider`, one row (migration 9): `state` (`UNCONFIGURED | AVAILABLE | UNAVAILABLE`), `a_share_feed` (`YAHOO | HITHINK`, `YAHOO` whenever `state` is `UNCONFIGURED`, enforced by a `CHECK`), `validated_at_ns`, `failure_code`, `failure_message`, `updated_at_ns`. The key is credential-store account `hithink_api_key` under service `marketrig`; SQLite holds only the row. Under the test seam it is one more entry in `runtime/credentials.json` (root §17).

### 1.2 Routes

- `GET /research/hithink` → `{state, a_share_feed, api_key_present, validated_at_ns, failure_code, failure_message, base_url}`; `base_url` is the compiled-in or seam value, never operator-set.
- `PUT /research/hithink {api_key}` → stores nothing until one request `GET {base}/api/meta/tickers/search?q=600519&limit=1` with `X-api-key` answers a JSON envelope with `code == 0`; then writes the key, the row `AVAILABLE` with `a_share_feed: HITHINK`, appends `HITHINK_PROVIDER_CHANGED {state: "AVAILABLE", a_share_feed: "HITHINK"}`, answers the resource. Envelope `code != 0` → `400 PROVIDER_REJECTED` carrying the upstream `code` and `message`, nothing written; transport failure or non-JSON → `502 PROVIDER_UNREACHABLE`; credential store failure → `CREDENTIAL_STORE_UNAVAILABLE`. An empty or whitespace key is `VALIDATION`.
- `DELETE /research/hithink` → removes the key, row `UNCONFIGURED` with `a_share_feed: YAHOO`, appends the event, answers the resource; idempotent.
- `PATCH /research/hithink {a_share_feed}` → `409 RESEARCH_UNCONFIGURED` while no key is stored, `400 VALIDATION` on any other value; otherwise writes the field, appends the event when it changed, answers the resource. The `CN` poller reads the field on its next cycle (§2.2).
- Any later `2003` from the feed or the passthrough sets the row `UNAVAILABLE` with `failure_code: "KEY_REJECTED"` and appends the event once; a successful `PUT` clears it.

### 1.3 Seam

`MARKETRIG_TEST_HITHINK_URL` replaces the base URL, honored only beside `MARKETRIG_TEST_DATA_ROOT` (`provider::seam_only_with_data_root`). `MARKETRIG_TEST_NO_TRADING` keeps the daemon off the public base URL; it does not suppress a stand-in named by the seam.

### 1.4 Desktop

Settings gains a "HiThink" block beside the memory provider: state, validated-at, one masked key field, Save, Remove, and one tick box "Use HiThink for A-share" bound to `a_share_feed`, disabled and unticked while no key is stored. Localized like the rest of Settings (root §4.5). No desk-level surface.

## 2. The A-share feed (HT-2)

### 2.1 Catalog delta

Each entry (root §12.2, R1 SPEC §3) becomes:

```text
instrument_id      600519.XSHG
yahoo_symbol       600519.SS              unchanged
hithink_symbol     600519.SH              the thscode; present exactly on CN entries
market, currency, price_increment, lot_size   unchanged
```

`catalog::entries_valid` additionally asserts: `market == CN ⇔ hithink_symbol present`, and for those the suffix derives from the venue (`XSHG → .SH`, `XSHE → .SZ`) with the symbol equal to the Nautilus symbol. The `instruments` resource lists `hithink_symbol` where present.

### 2.2 Client behavior

One `DataClient` per node, as R1 ships it. Its `CN` poll task is one task for the whole `CN` leg and reads `a_share_feed` at the top of every cycle: the due instruments go through the Yahoo chart client (R1 SPEC §2.1, unchanged) when `YAHOO` and through HiThink's batched snapshot when `HITHINK`; a switch is visible on the next observation's `provider` and never resets the sequence. Because one HiThink cycle is one request for the whole leg, R1's two tiers apply to the leg rather than to each instrument: the leg is exposed when any `CN` instrument is (per HT-2's second `ponytail`). Under `HITHINK`, per cycle:

1. take the whole `CN` leg (R1's 30 s / 10 s tiers; once at subscription whatever the phase);
2. issue one `GET {base}/api/a-share/prices/snapshot?thscodes=<comma list>` with `X-api-key`;
3. retry on `4001`, `5001`–`5003`, HTTP 429 or 5xx, and transport errors: 3 attempts, backoff 500 ms then 1 s; never on `1xxx`/`2xxx`; `2003` also sets §1.2's `UNAVAILABLE`;
4. for each `item`: parse `last_price` to decimal text at the instrument's precision; if `(last_price, volume, turnover)` equals the last accepted triple, refresh health only; otherwise accept an observation with `source_time_ns: null`, `received_at_ns` now, sequence advanced;
5. an instrument absent from the reply is a failure for that instrument alone (`DEGRADED`, last observation standing);
6. exhaustion or any other failure: every instrument in the batch `DEGRADED`.

With `a_share_feed: HITHINK` and `state: UNAVAILABLE` (a key rejected mid-run), `CN` instruments read `DEGRADED` with the last HiThink observation standing and no request issued; the daemon never switches to Yahoo on its own (root §12.2). The node starts whatever the row says.

### 2.3 Observation delta

The root §12.2 read gains two fields on every instrument and permits one null:

```json
{ "instrument_id": "600519.XSHG", "provider": "hithink", "venue": "XSHG",
  "last": "1688.00", "currency": "CNY",
  "source_time_ns": null, "received_at_ns": 0, "read_at_ns": 0, "age_ms": 0,
  "sequence": 3, "market_phase": "OPEN", "calendar": "HITHINK", "health": "LIVE",
  "book_synthesized": true }
```

`provider` is `yahoo` or `hithink`; `calendar` is `HITHINK` or `WEEKDAY` (§3) and is `WEEKDAY` on every Yahoo observation; `source_time_ns` is `null` exactly on `hithink` observations, and `age_ms` then counts from `received_at_ns`. Health vocabulary unchanged. The MCP quote resource carries the same fields.

## 3. The A-share calendar (HT-3)

- While `a_share_feed` is `YAHOO`, no list is fetched and the `CN` phase is R1's weekday rule with `calendar: "WEEKDAY"`.
- While `HITHINK`: on the first `CN` cycle under it, and on the first cycle after 00:00 Asia/Shanghai, fetch `GET {base}/api/a-share/calendar/trading-days` under §2.2's retry bound; on success replace the in-memory set of `date` strings (`yyyyMMdd`) and record `calendar_fetched_at_ns`; on failure keep the previous set or none.
- `CN` phase: `OPEN` iff the Shanghai session rule (R1 SPEC §2.2) holds **and** either today's `yyyyMMdd` is in the set (`calendar: "HITHINK"`) or no set has been fetched (`calendar: "WEEKDAY"`). With a set fetched and today absent, `CLOSED` with `calendar: "HITHINK"`.
- Under `MARKETRIG_TEST_HITHINK_URL` the cadence gate is lifted exactly as under `MARKETRIG_TEST_QUOTE_URL` (R1 SPEC §10.1): the poller ticks at any hour and observations still label the real phase from the stand-in's calendar.
- The set is never persisted (root §15's volatile rule).

## 4. The research passthrough (HT-4)

### 4.1 Route

`GET /research/hithink/{path}?<query>` (the OpenAPI document writes the template as `{*path}`, axum's whole-tail capture; the wire path is the same), `path` one of the allowlist in `crates/marketrigd/src/research_paths.rs` (generated from `vendor/hithink-finance/references/api/capability-map.md` by `scripts/hithink-skill.mjs`; 59 paths at design time; `research::allowlist_matches_capability_map` fails when the vendored map and the list differ). Behavior:

1. `hithink_provider.state != AVAILABLE` → `409 RESEARCH_UNCONFIGURED`;
2. path not allowlisted → `404 RESEARCH_PATH_UNKNOWN`;
3. acquire the installation-wide research gate (one at a time, ≥ 200 ms since the previous upstream call);
4. `GET {base}/api/{path}?<query verbatim>` with `X-api-key`, 30 s total timeout, §2.2's retry bound on `4001`/`5xxx`/transport, body capped at 8 MiB → `502 RESEARCH_TOO_LARGE`;
5. a JSON body → `200` with the body verbatim and `Content-Type: application/json`, whatever its `code`; `2003` additionally §1.2's `UNAVAILABLE`;
6. exhaustion or a non-JSON body → `502 RESEARCH_UNREACHABLE` with the attempt count and last status.

No event, no row, no attribution headers read. The route is `GET` only; there is no `POST`.

### 4.2 CLI

`marketrig [--json] research hithink <path> [--param key=value]… [--out <file>]`:

- `--param` repeats and is URL-encoded into the query; a `key=value` without `=` is a usage error (exit 2);
- the request's ceiling is 45 s (above the daemon's 30 s);
- success: body ≤ 256 KiB → standard output verbatim (plain and `--json` identical, since the body is the machine form); larger, or `--out` given → written to `--out` or `hithink-<path with / as ->-<unix seconds>.json` in the working directory, and the command prints `wrote <path> (<n> bytes)` (or `{"path": …, "bytes": …}` under `--json`), exit 0;
- a MarketRig envelope error → the CLI's standard `error: <CODE>: <message>` and exit 1; an upstream envelope with nonzero `code` is a success at the daemon and exits 0 — the agent reads `code` and `message` from the body, as the seeded skill tells it to.

## 5. The seeded skill (HT-5)

### 5.1 Vendoring

`vendor/hithink-finance/` = upstream `skills/hithink-finance/` at commit `44b7aa34dd504675f3ddaa15b3d478ea16f97884` (`main`, vendored 2026-09-08) plus `LICENSE`, with `VENDOR.md` naming the repository, the commit, the date and the re-vendoring recipe. Unmodified.

### 5.2 The rewrite script

`scripts/hithink-skill.mjs` (Node, no dependencies) writes `crates/marketrigd/seed/skills/hithink-finance/`:

1. copy `SKILL.md`, `references/api.md`, `references/api/**` and `LICENSE` (upstream is MIT: the notice travels with the copy); skip `references/cli*`, `references/mcp*`, `references/python-sdk*` and `agents/`;
2. in `SKILL.md`, replace the frontmatter `description` with an English one naming `marketrig research hithink`; delete the sections whose headings are `## Skill 低频自更新引导`, `## CLI 低频静默更新自检`, `## 接入方式决策`, `## 统一 API Key`, `## CLI 推荐与联动`, `## 故障路由`; insert after the title the English preamble from `scripts/hithink-skill-preamble.md`, which states: one path — `marketrig research hithink <path> [--param k=v]…`; the daemon holds the key, nothing to install, configure, log into, or update; the response is HiThink's own envelope (`code`, `message`, `request_id`, `data`), success is `code == 0`; large results arrive as a file path; the service covers A-share only, with `thscode` disambiguation through `meta/tickers/search` first; paper trading itself is through the `marketrig` MCP server, not through this data;
3. in every kept page, rewrite each fenced `curl [-flags] 'https://fuyao.aicubes.cn/api/<path>?<q>' \ -H 'X-api-key: …'` block into `marketrig research hithink <path> --param k=v …` by pattern, whatever wraps it (a `$(…)` capture, a trailing pipe) surviving; then drop every remaining line naming a surface the desk cannot reach — the CLI, an MCP server, the Python SDK, an API key or credential file, a dropped page, the four-surface routing itself — together with the list children that line introduced, renumbering an ordered list the drop broke, and drop `api.md`'s `## 维护规则`, which tells its reader to edit the projection and run upstream's mirror script (the projection is read-only, per D83). All links in the kept tree are relative and resolve inside it;
4. emit `research_paths.rs` from `references/api/capability-map.md`'s endpoint tables: the header, `RESEARCH_PATHS`, and the `#[cfg(test)]` module lines for the two hand-written check modules under `crates/marketrigd/src/research_paths/`.

The output is committed; CI runs the script and fails on a diff (like `pnpm generate`).

### 5.3 Seeding

At desk creation, after `desk-improvement`, the daemon uploads `hithink-finance` into the desk's OpenViking user by the same path (root §16), skipped when one of that name exists; it reaches the workspace through the projection. The upload is part of `openviking-continuity` §3.2's provisioning, which runs for every `READY` desk at every OpenViking readiness, so a desk created before this feature gains the skill at the daemon's next readiness exactly as it gained `desk-improvement`; nothing else about it changes, and its constitution is never rewritten (per D20).

`ponytail:` the upload carries `SKILL.md` alone, as `openviking-continuity` §5.5's does, so the seed's `references/` do not reach the projection; the rewritten `SKILL.md` stands on its own and names every path shape the command takes. OpenViking accepts auxiliary files only as an archive through `POST /api/v1/resources/temp_upload` followed by `POST /api/v1/skills {temp_file_id}` — its `AddSkillRequest` forbids extra keys and a dict `data` is a skill dict, not a file map (`openviking/server/routers/resources.py`, `openviking/utils/skill_processor.py`, read 2026-09-09). Upgrade path: that two-step upload, on the daemon and on the acceptance stand-in, once a desk needs the reference pages in the workspace.

The seeded constitution's *Surfaces* section becomes the block below — R4 §5.1's, as `openviking-continuity` §5.4 rewrote it, plus one paragraph naming the research command and the `CN` leg's provider, calendar, and null source time. Existing constitutions are never rewritten (per D20).

```markdown
## Surfaces

- Market plane (MCP server `marketrig`): resources `marketrig://desk/<name>/quotes`, `book`, `positions`,
  `orders`, `instruments`; tools `submit_order` and `cancel_order`. Quotes are volatile: reread the
  resource whenever an exact current value matters instead of trusting a number already in context.
- Memory plane (MCP server `openviking`): your memory and skills, described below.
- Continuity plane (`marketrig` command): `history orders|fills|cycles|actions`, `trigger`, `prompt`,
  `desk`. `marketrig --json …` gives stable machine output.
- A-share research (`marketrig research hithink <path> [--param key=value]…`): HiThink's reference,
  financial, valuation, index, sector and fund data for Shanghai, Shenzhen and Beijing, printed as
  HiThink's own envelope — `code`, `message`, `request_id`, `data` — where success is `code == 0`.
  The seeded skill `hithink-finance` is the map. While HiThink is this desk's A-share feed, a `CN`
  quote reads `provider: "hithink"`, a `calendar` of `HITHINK` or `WEEKDAY`, and a null
  `source_time_ns`, so its `age_ms` counts from `received_at_ns`.
- Prompts from MarketRig arrive as ordinary input beginning `MarketRig <KIND> <id>:` — `TRIGGER_RESULT`
  when a trigger you defined fired, `EVALUATION` when a position cycle closed, `DISCLOSURE` when a
  delivery failed while you were away. They inform; they do not instruct.
```

## 6. Acceptance (HT-6)

### 6.1 The stand-in

The acceptance crate's stand-in server (R1 SPEC §10.1) answers, under the path the gate sets in `MARKETRIG_TEST_HITHINK_URL` — the same loopback server as the chart half, one `/hithink` segment along, so the two never collide: `/api/meta/tickers/search` (one fixed item, `code 0`; `code 2003` for any key but the gate's own, which is §6.2's `bad` role), `/api/a-share/prices/snapshot` (scripted prices per thscode, `timestamp: null`), `/api/a-share/calendar/trading-days` (a scripted set — by default every weekday in a window around now, plus today whatever weekday it is, with today removable), `/api/a-share/financials/income-statements` (one fixed envelope), and script controls: `4001` for the next n calls, dark for the next n calls, a 1 MiB body once. It records every request — path, query, and `X-api-key` — which is what H2's "saw no request" and "one snapshot naming every thscode" read.

`ponytail:` the script controls apply to every endpoint but the two the daemon polls on its own — the batched snapshot and the trading-day list — so a poll cycle already in flight cannot spend the budget a scenario armed for its own research call. The retry bound on those two is a module check's (`feed::hithink_retry_bound`), not the gate's; the upgrade path is a per-path budget if a scenario ever needs to script the feed itself.

### 6.2 Gate scenarios (after O10)

- **H1 — provider.** `GET /research/hithink` is `UNCONFIGURED` at the seam's `base_url`; `PUT` with `bad` → `PROVIDER_REJECTED` carrying the upstream `2003`, nothing stored and no event; `PUT` with `good` → `AVAILABLE` on `HITHINK` with one `HITHINK_PROVIDER_CHANGED` and no `api_key` in the answer; the key is absent from `marketrig.db`, the log root, every event, the launch files, the workspaces, the daemons' stderr, and the CLI captures in the bundle (`openviking-continuity` §7.2's own grep, on this key); `DELETE` returns to `UNCONFIGURED` on `YAHOO`.
- **H2 — the CN leg.** On a desk of its own, the only one this daemon starts a node for, so one node is one `CN` poll task. With the provider `UNCONFIGURED`, `600519.XSHG` reads `provider: "yahoo"` from the quote stand-in and the HiThink stand-in saw no snapshot request; configured, `a_share_feed` is `HITHINK` and one poll cycle produces exactly one snapshot request naming every `CN` catalog thscode, the observation reads `provider: "hithink"`, `source_time_ns: null`, `calendar: "HITHINK"`; the stand-in removes today from its calendar and the same key is removed and stored again, which is what clears the set the daemon already holds (§3 fetches it once a day) → the next read is `CLOSED` with `calendar: "HITHINK"`; `PATCH {a_share_feed: "YAHOO"}` → the next observation is `provider: "yahoo"`, `calendar: "WEEKDAY"`, with a higher sequence and no `TRADING_NODE_STARTED`; `PATCH` back → `hithink` again; a `CN` market round trip closes with realized P&L in `CNY` and the CN fee rate (the R1 G15 shape on the new feed).
- **H3 — research.** `marketrig research hithink meta/tickers/search --param q=600519` prints the stand-in envelope verbatim; an unknown path → `RESEARCH_PATH_UNKNOWN`, exit 1; two scripted `4001` then success → one accepted answer; three → `RESEARCH_UNREACHABLE` naming 3 attempts, which is §2.2's whole bound; the 1 MiB body → written to a file whose path the command prints; with the provider removed → `RESEARCH_UNCONFIGURED`.
- **H4 — the seed.** A desk created now lists exactly `desk-improvement` and `hithink-finance` in its projection, the second file matches the committed seed byte for byte, and no line of it contains `X-api-key`, `fuyao.aicubes.cn/mcp`, `hithink-finance auth`, `pip install`, or `npx`.

### 6.3 Experiment scenario

- **E7 — A-share on the real service.** Attended, one cell per platform and runtime, real key. The operator configures the provider in Settings. The session is asked to read `600519.XSHG`'s latest income statement and valuation through `marketrig`, then to trade one lot on the paper book and close it. The harness verifies the provider row, the `CN` observation's provider and calendar fields, the cycle row and its queued evaluation; the reads themselves are inconclusive by construction.

## 7. Required checks

**Module checks** (`cargo test`, fakes allowed):

- `provider::validate_with_one_bounded_request` — exactly one search request; `code 2003` → `PROVIDER_REJECTED` and no store write.
- `provider::seam_only_with_data_root` — `MARKETRIG_TEST_HITHINK_URL` inert without the data root.
- `provider::key_never_answered` — every route answer and event payload is key-free.
- `catalog::entries_valid` — the §2.1 additions.
- `feed::hithink_batches_one_request` — one cycle over the n `CN` instruments → one request naming all n.
- `feed::a_share_feed_switch` — flipping the setting between cycles changes the next observation's `provider` without a node restart or a sequence reset.
- `provider::toggle_requires_key` — `PATCH` refused `RESEARCH_UNCONFIGURED` without a key; `DELETE` forces `YAHOO`.
- `feed::hithink_retry_bound` — 3 attempts on `4001`, none on `1001`/`2003`; `2003` flips the provider row.
- `feed::hithink_change_detection` — an unchanged triple refreshes health without advancing sequence.
- `feed::cn_phase_from_trading_days` — today absent → `CLOSED` with `HITHINK`; no set → weekday rule with `WEEKDAY`; refresh after Shanghai midnight.
- `feed::observation_provenance` — extended with `provider`, `calendar`, and the null `source_time_ns`.
- `research::allowlist_matches_capability_map` — parse the vendored capability map and compare; 59 paths.
- `research::codes` — each §4.1 refusal; upstream nonzero `code` passes through as `200`.
- `research::spacing` — two back-to-back calls are ≥ 200 ms apart upstream.
- `cli::research_spill` — 256 KiB boundary and `--out`.
- `skill::rewritten_examples_route_through_marketrig` — the seed contains no forbidden strings (§6.2 H4) and reports how many curl blocks the pattern rewrote versus left.
- `skill::seed_is_current` — `node scripts/hithink-skill.mjs --check` succeeds; skipped with a printed note where `node` is not on PATH, since CI runs the same check itself.
- `store::migration_9_applies`.

**Gate:** H1–H4 after O10 on the stand-in.

**Experiment:** E7, once per cell, real HiThink.

**Static checks:** unchanged, plus the script diff check in CI's `frontend` job.
