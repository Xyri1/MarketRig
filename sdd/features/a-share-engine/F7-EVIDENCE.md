# F7 — provider date, calendar, freshness, and reference semantics

Run 2026-09-09, macOS 26.3.1 arm64, worktree `.worktrees/a-share-feasibility`, branch
`codex/a-share-feasibility`, at `63e95e2`. Live read-only GETs against
`https://fuyao.aicubes.cn` with the real key held in an environment variable and passed
only as `X-api-key`, exactly as `crates/marketrigd/src/hithink.rs:464` builds it. No key
appears in this file or in `f7/`. No daemon, no order, no write.

## Status

| Sub-question | Status |
| --- | --- |
| Date attribution — which trading day a snapshot describes | **BLOCKED on the snapshot alone; PASS with one extra request** (§4.1) |
| Calendar confirmation — is today a trading day | **PASS** (§4.2) |
| Freshness — is this observation of the current session | **FAIL as specified; only a weak rule is supportable** (§4.3) |
| Ex-dividend reference correctness of `prev_price` | **PASS against the exchange formula, 15/16 exact, 1/16 off by one tick** (§4.4) |
| Independent authoritative exchange confirmation | **NOT RUN — exchange endpoints unreachable** (§4.5) |

## 1. What MarketRig consumes today

`crates/marketrigd/src/feed.rs:602` (`poll_hithink`) parses exactly three fields per item:
`last_price`, `volume`, `turnover`. It never reads `prev_price`, `price_change`,
`open_price`, `high_price`, `low_price`, or `data.timestamp`. `accept_hithink`
(`feed.rs:464`) hardcodes `source_time_ns: None` and stamps `received_at_ns` from
`store::now_ns()`.

`hithink.rs:694` (`refresh_calendar_if_due`) fetches `a-share/calendar/trading-days` once
per Shanghai day and keeps `data.item[].date` (`yyyyMMdd`) in memory only.
`hithink.rs:678` (`cn_phase`) is `Phase::Open` iff R1's Shanghai session rule holds **and**
today's `yyyyMMdd` is in that set; with no set fetched it silently degrades to
`Calendar::Weekday` and reports `OPEN` on a holiday.

Nothing persists a trading date. `grep -niE 'trading_day|trading_date|trade_date|session_date|calendar'`
over `crates/marketrigd/src/store/*.sql` returns nothing. `fills` carries `occurred_at_ns`
only (`store/002_r1.sql:24`); observations are never persisted at all (root §15). So the
only current evidence that an observation belongs to a trading date is
`received_at_ns` plus the in-memory calendar set — and after a restart that set is empty
until the next refresh, which is the "uncertain date" state SPEC §2.1 requires to block.

## 2. The official contract, verbatim

Source: `HiThink-Tech/Financial-API` at commit `44b7aa34dd504675f3ddaa15b3d478ea16f97884`.
The text below is from `docs/api/endpoints-prices.md` and `docs/api/endpoints-calendar.md`,
fetched over HTTP, and is byte-identical to the vendored copy at
`vendor/hithink-finance/references/api/` (HT-5).

### 2.1 Snapshot — `GET /api/a-share/prices/snapshot` (`endpoints-prices.md` §1)

`data.SnapshotData`:

> | `timestamp` | long \| null | 数据就绪时间（毫秒）。按 `thscodes` 显式取数时为 `null`；分页模式下为序列中最新有效时间。 |
> | `total` | int | 全市场代码表总数（分页模式用于估算页数）。 |
> | `item` | array | 快照记录列表。 |

`item[].PriceSnapshotItem`, the price fields:

> | `last_price` | number | 最新成交价（原始货币）。 |
> | `price_change` | number | 相对前收盘价的涨跌额。 |
> | `price_change_ratio_pct` | number | 涨跌幅（百分比数值，如 `1.74` 表示 +1.74%）。 |
> | `prev_price` | number | 前收盘价。 |

**No field on the snapshot names a trading date.** `prev_price` is documented as three
characters — 前收盘价, "previous close" — with no statement about ex-rights or
ex-dividend adjustment. That is the whole documented reference contract.

### 2.2 Historical daily bars — `GET /api/a-share/prices/historical` (`endpoints-prices.md` §2)

> | `adjust` | query | string | 否 | 复权方式：`none` / `forward`（前复权）/ `backward`（后复权）。 | `forward` |

`data.HistoricalData`:

> | `timestamp` | long | 数据就绪时间（毫秒），为序列中最新一根 K 线的上游有效时间。 |

`item[].PriceBarItem`:

> | `date_ms` | long | K 线日期（毫秒）。 |
> | `close_price` | number | 收盘价。 |

Constraints: 每次请求仅一个 thscode, window ≤ 10 years, `interval` only `1d`.

### 2.3 Corporate actions — `GET /api/a-share/corporate-actions/adjustment-factors` (`endpoints-prices.md` §3)

> | `ex_date_ms` | long | 除权除息日，Asia/Shanghai 00:00:00 毫秒 Unix 时间戳。 |
> | `dividend_per_share` | number | 每股现金分红（税前，原始货币）。非现金事件为 `0`。 |
> | `per_share_bonus` | number | 每股送股比例（如 `0.1` 表示 10 送 1）。纯现金分红事件为 `0`。 |

> **字段约定**：响应**不返回** `event_type` / `record_date` / `adjust_factor`。

The prose says the stream covers 现金分红 / 送股 / **配股**, but the JSON response carries no
rights-issue fields. Only the full-market parquet dump does
(`endpoints-market-dumps.md` §3): `allotment_ratio`, `allotment_price`. A rights issue's
exchange reference is therefore not reconstructible from the per-thscode JSON endpoint.

### 2.4 Calendar — `GET /api/a-share/calendar/trading-days` (`endpoints-calendar.md`)

> 无入参。窗口固定为 `[今日 - 1 年, 今日]`（Asia/Shanghai 时区）。

> | `timestamp` | long | 数据就绪时间（毫秒）。 |
> | `date_ms` | long | 交易日，Asia/Shanghai 00:00:00 毫秒 Unix 时间戳。 |
> | `date` | string | 可读日期，格式 `yyyyMMdd`（如 `20250701`）。 |

> - 用它判断「今天是否开盘」：先确认今天是否在返回的 `item` 列表中。

### 2.5 Auction — `GET /api/a-share/auction/snapshot` (`endpoints-auction.md` §1)

> `data` 为 `{timestamp, auction_phase, data_status, total, item[]}`。`timestamp` 始终是接口响应组装时间…；`data_status` 用于区分数据尚未就绪、竞价完成或停牌等状态。

> | `pre_close_price` / `open_price` / `last_price` | number/null | 昨收、开盘和最新价。 |

A second, independent `pre_close_price`, plus the only provider-asserted phase and halt
signal in the contract.

## 3. Where the live service contradicts the contract

**The batched snapshot's `timestamp` is not null, and it is the response clock.**
Two identical `?thscodes=600519.SH` calls 13 s apart (`f7/snapshot-timestamp-drift.txt`):

```
17:12:42+08:00  data.timestamp = 1788945162000 -> 2026-09-09T17:12:42+08:00  last_price 1290.88
17:12:55+08:00  data.timestamp = 1788945175000 -> 2026-09-09T17:12:55+08:00  last_price 1290.88
```

The field advanced with the wall clock while the price did not, four hours after the
15:00 close. It is response-assembly time. The calendar's `timestamp` behaves the same
(17:12:10.299 for a request issued at 17:12:10). So the documented "`null` in batch mode"
is stale, and the value that arrives instead is **worse than null**: it looks like a data
time and is not one. `poll_hithink` is accidentally correct in ignoring it; do not adopt
it as `source_time_ns`.

The historical endpoint's `timestamp` *is* a data time: `1788883200000` =
2026-09-09T00:00:00+08:00, equal to the latest bar's `date_ms`, exactly as documented.

**Rate limiting answers `code: 429`, not `4001`.** After ~30 requests in a few minutes,
one call returned `{"code": 429, "message": "request limit exceeded", "data": null}`.
`hithink.rs:47` treats only `4001` and `5001..=5003` as retryable envelope codes. The HTTP
status of that response was not captured, so whether `attempt()`'s
`status == TOO_MANY_REQUESTS` branch would have caught it is **unverified**. If the HTTP
status was 200, `poll_hithink` degrades the whole `CN` leg on a rate limit instead of
retrying. Outside F7's scope; recorded for the implementation slice.

## 4. Live results

### 4.1 Date attribution — the snapshot alone cannot carry it

The snapshot has no date field and its `timestamp` is the response clock (§3). Receipt
time plus a changed price triple does not establish a trading date: after the close the
triple is frozen and identical to yesterday's read on a halted or untraded name.

One extra request settles it. `GET /api/a-share/prices/historical?thscode=<code>&interval=1d&adjust=none&start=…&end=now`
returned, for 600519.SH (`f7/historical-600519-none.json`):

```
2026-09-08  date_ms 1788796800000  close 1309.30
2026-09-09  date_ms 1788883200000  close 1290.88   <- snapshot last_price = 1290.88
```

and `data.timestamp == 1788883200000 == max(date_ms)`. The same held for all 16 ex-date
names in §4.4: `bar_close_eq_snapshot_last` is `true` 16/16
(`f7/exdate-comparison.json`). So the last bar's `date_ms` dates the snapshot's
`last_price`, and it is a real provider-asserted date, not a local clock inference.

Limitation: every sample was taken at 17:1x, after the close. **Whether the current-day
bar exists and tracks the running last price during the session is not verified.** That
one intraday sample is the missing check; it costs one request during 09:30–15:00.

Cost: the endpoint is single-`thscode`, so this is N requests per cycle for N `CN`
instruments, against one batched snapshot. It is a readiness check, not a per-tick check —
run it once per instrument per trading day, on the same schedule as the calendar refresh.

### 4.2 Calendar confirmation — PASS

`GET /api/a-share/calendar/trading-days` at 17:12 (`f7/calendar-trading-days.tail.json`):
243 items, first `20250909`, last `20260909`, and `20260909` present. The window is fixed
to `[today − 1 year, today]`, so `max(item[].date)` is the provider's own most recent
trading day at or before its own today.

That gives a complete rule from one request:

- `max(date) == local Shanghai date` → today is a trading day and the list is current.
- `max(date) < local Shanghai date` → either a holiday or a stale list. Disambiguate with
  the same response's `data.timestamp`, which §3 established is the **server clock**:
  `shanghai_date(data.timestamp) == local Shanghai date` and `max(date) < that date`
  means holiday, not staleness. This is the one legitimate use of that field.

This is strictly stronger than the shipped `cn_phase` set-membership test, which cannot
tell a holiday from a lost set and falls back to `WEEKDAY` in both cases.

### 4.3 Freshness — no supportable per-observation rule

There is no per-item time anywhere on the batched snapshot. The only candidates are:

- `data.timestamp` — response clock, useless (§3);
- `received_at_ns` — MarketRig's own clock, proves nothing about the datum;
- the `(last_price, volume, turnover)` change triple — proves movement when it changes,
  proves nothing when it does not;
- `auction/snapshot`'s `data_status` / `auction_phase` — the only provider-asserted phase
  and the only halt (停牌) signal in the contract. At 17:12 it returned
  `auction_phase: "closed"`, `data_status: "final"` (`f7/auction-snapshot.json`).
  **Whether it distinguishes the lunch break, continuous trading, and a halted name
  intraday is not verified** — one after-hours sample cannot show that.

The smallest rule that is actually supportable today: **a `CN` observation is fresh enough
to execute against only while `volume` has strictly increased since a read taken within the
current proven trading date**, plus the §4.1 date attribution and §4.2 calendar
confirmation. Monotonic `volume` is the one field that cannot go backwards inside a
session and cannot advance outside one. It still cannot distinguish a halted name from an
untraded one; only `auction/snapshot`'s `data_status` can, and that needs the intraday
sample above before it can be relied on.

Do not express freshness as an `age_ms` bound. There is no source time to bound.

### 4.4 Ex-dividend reference — PASS

The decisive test needs a snapshot taken **on** an ex-date, which is not observable
retroactively. It was made observable by finding today's ex-dividend names in the
full-market parquet dump (`GET /api/dump/market-dumps/adjustment-factors/download-url`,
52k rows, 289 KiB, presigned URL not recorded): **16 A-shares have `ex_date_ms` =
2026-09-09 00:00 Asia/Shanghai**, all pure cash, all `per_share_bonus = 0`
(`f7/corporate-actions-exdate-2026-09-09.json`). The dump also carries ex-dates up to
2026-09-18, so it is forward-looking.

Exchange reference per SSE trading rules §4.3.2–4.3.3 (the ex-rights/ex-dividend reference
is the displayed previous close and the price-band basis; cited in `RESEARCH.md`):
`ref = round_half_up((prior raw close − dividend_per_share) / (1 + per_share_bonus), 0.01)`.
Prior raw close is `prices/historical?adjust=none` for 2026-09-08.

| thscode | ex-date | raw close 09-08 | dividend | exchange ref | provider `prev_price` | match |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| 001400.SZ | 2026-09-09 | 80.17 | 0.5 | 79.67 | 79.67 | yes |
| 002073.SZ | 2026-09-09 | 5.88 | 0.02 | 5.86 | 5.86 | yes |
| 002315.SZ | 2026-09-09 | 25.98 | 0.5 | 25.48 | 25.48 | yes |
| 002322.SZ | 2026-09-09 | 12.71 | 0.34 | 12.37 | **12.38** | **no, +0.01** |
| 002441.SZ | 2026-09-09 | 8.38 | 0.2 | 8.18 | 8.18 | yes |
| 002833.SZ | 2026-09-09 | 18.91 | 0.3 | 18.61 | 18.61 | yes |
| 002841.SZ | 2026-09-09 | 47.40 | 0.5 | 46.90 | 46.90 | yes |
| 300196.SZ | 2026-09-09 | 16.90 | 0.2 | 16.70 | 16.70 | yes |
| 300622.SZ | 2026-09-09 | 15.62 | 0.16 | 15.46 | 15.46 | yes |
| 301151.SZ | 2026-09-09 | 22.58 | 0.2 | 22.38 | 22.38 | yes |
| 600114.SH | 2026-09-09 | 28.83 | 0.1 | 28.73 | 28.73 | yes |
| 603992.SH | 2026-09-09 | 20.30 | 0.28 | 20.02 | 20.02 | yes |
| 603993.SH | 2026-09-09 | 18.94 | 0.095 | 18.85 | 18.85 | yes |
| 605377.SH | 2026-09-09 | 8.22 | 0.2 | 8.02 | 8.02 | yes |
| 688128.SH | 2026-09-09 | 22.14 | 0.25 | 21.89 | 21.89 | yes |
| 688271.SH | 2026-09-09 | 107.00 | 0.13 | 106.87 | 106.87 | yes |

**`prev_price` == raw prior close: 0/16. `prev_price` == exchange ex-dividend reference:
15/16 exact, 1/16 one tick high.** `price_change == last_price − prev_price` for all 16,
so the provider's own change and percentage are computed against the ex-reference too —
the exchange convention.

The 002322.SZ cent. HiThink's own forward-adjusted series uses `dividend_per_share = 0.34`
and yields 12.37 for 2026-09-08 (`raw 12.71 − fwd 12.37 = 0.34`), disagreeing with its own
`prev_price` of 12.38. The `auction/snapshot` endpoint independently reports
`pre_close_price: 12.38` with `auction_price 12.37` and `auction_pct −0.0808`
(= (12.37 − 12.38)/12.38), so two quote surfaces say 12.38 and only the adjustment-factor
pipeline says 12.37. The consistent reading: **`prev_price` is the exchange's published
reference, carried through; `dividend_per_share` is rounded to 2 dp** (a true 0.335 gives
12.71 − 0.335 = 12.375 → 12.38 half-up). The per-thscode endpoint returns the same 0.34, so
the rounding is in the datum, not in the dump.

Consequence, and it is the important one: **a reference derived locally from
`historical(adjust=none)` + `corporate-actions` is accurate to ±0.01, not exact.** One cent
of error in `prev` moves `limit_up = round(prev × 1.1, 0.01)` by about a cent, which
decides an `ORDER_INVALID` at the boundary. The derived reference is a **cross-check**, not
a substitute for `prev_price`.

Second, independent confirmation that HiThink applies the exchange's *arithmetic*
ex-dividend subtraction rather than a proportional factor — 600519.SH across its
2026-06-26 ex-date, dividend 28.02423 (`f7/historical-600519-ex-2026-06-26.json`):

```
2026-06-25  adjust=none 1212.10   adjust=forward 1184.07577   difference exactly 28.02423
2026-06-26  adjust=none 1168.63   adjust=forward 1168.63      difference 0
```

`fwd/raw` is not constant across the pre-ex bars (0.97693, 0.97743, 0.97708, 0.97679,
0.97689), so the adjustment is a subtraction, not a ratio — the exchange formula.

### 4.5 Authoritative exchange confirmation — NOT RUN

Three attempts, none reached an exchange:

- `https://yunhq.sse.com.cn:32042/v1/sh1/snapshot?select=…&code=600114` → **HTTP 400**
  (nginx, no body).
- `https://yunhq.sse.com.cn:32041/v1/sh1/snapshot?code=600114` → **HTTP 000**, TLS
  handshake refused by LibreSSL 3.3.6 (`tlsv1 alert protocol version`).
- `http://www.cninfo.com.cn/new/hisAnnouncement/query` (POST, two parameter forms) →
  **HTTP 200 with `totalAnnouncement: 0`**, no announcement rows.

A web search located 002322's 2025 annual distribution announcement but not its per-share
figure. So the *number* published by the exchange for a specific stock on 2026-09-09 was
not independently obtained. What stands instead: the exchange **rule** (SSE §4.3.2–4.3.3,
already cited in `RESEARCH.md`) plus 15/16 exact agreement with the rule computed from two
independent HiThink endpoints, plus the auction endpoint agreeing on the sixteenth. That
is strong, and it is not the same as an exchange page. Treat §4.4 as PASS against the
formula and NOT RUN against the exchange itself.

## 5. Minimal evidence set

Per trading day, per `CN` desk, before `CN` execution is enabled:

1. **Calendar and date** — one `GET /api/a-share/calendar/trading-days`.
   Require `max(item[].date) == shanghai_date(now)`. On failure, use the same response's
   `data.timestamp` as the server clock to separate holiday from stale list (§4.2).
   This is the *only* proof that today is a trading day; the weekday fallback in
   `cn_phase` must not satisfy it. Missing list → `MARKET_UNAVAILABLE`, never `WEEKDAY`.
2. **Date attribution** — one `GET /api/a-share/prices/historical?thscode=<code>&interval=1d&adjust=none`
   per `CN` instrument, window the last few days. Require the last bar's `date_ms` to equal
   today's Shanghai midnight and its `close_price` to equal the snapshot's `last_price`
   (§4.1). This is what binds an observation to a trading date; it replaces
   "received_at_ns plus a changed triple" outright.
3. **Reference** — `prev_price` from the batched snapshot, which §4.4 established *is* the
   ex-rights/ex-dividend reference. Cross-check it once per day against
   `round_half_up((prior bar close − dividend)/(1 + bonus), tick)` from step 2's bars and
   `corporate-actions/adjustment-factors`; require agreement within one tick and block on a
   larger divergence. Do not compute the band from the derived value — use `prev_price`.
4. **Freshness** — monotonically increasing `volume` since a read inside the proven trading
   date (§4.3). No `age_ms` bound; there is no source time to bound.
5. **Never** use the snapshot's `data.timestamp` for anything. It is the response clock
   (§3). Keep `source_time_ns: null` exactly as `feed.rs:489` already does, and keep
   `age_ms` documented as counted from receipt.

What must be persisted, which nothing is today (§1): the proven trading date and the
reference it was proven for, per instrument, so that a restart cannot resume matching
against a reference from a previous day. A volatile in-memory set cannot satisfy SPEC
§5.3's "day rollover invalidates readiness".

Rights issues (配股) are out of reach of steps 1–4: the per-thscode endpoint returns no
`allotment_ratio` or `allotment_price` (§2.3), so `prev_price` on a rights ex-date cannot
be cross-checked at all. Either take the parquet dump, which does carry both columns, or
declare rights-issue ex-dates a blocked instrument-day.

## 6. Limitations

- Every live sample was taken at 17:1x Asia/Shanghai, after the close. The intraday
  behaviour of the current-day bar (§4.1) and of `auction/snapshot`'s `data_status` (§4.3)
  is unverified. One session-hours sample closes both.
- All 16 ex-date cases were pure cash dividends (`per_share_bonus = 0`). The bonus-share
  and rights-issue arms of the formula are untested against a live `prev_price`.
- One authoritative exchange number was not obtained (§4.5).
- The `code: 429` rate-limit envelope (§3) was observed once without its HTTP status.
- `f7/` holds the redacted responses these findings were computed from. The parquet dump
  and the presigned URL are not committed.

---

# R2 — intraday readiness (2026-09-09 preparation)

Run 2026-09-09 18:10–18:30 Asia/Shanghai, macOS arm64, worktree
`.worktrees/a-share-feasibility`, branch `codex/a-share-feasibility` at `6c18e74` plus
this commit. The exchange was **CLOSED** for the whole of this session, so **no session
sample was taken and none was substituted**. Live reads were the same read-only GETs as
the initial run, key from the operator's `.env` in an environment variable, passed only as
`X-api-key`. No key appears in this file, in `f7/`, or in `f7/intraday/`.

## R2 status

| Part | Status |
| --- | --- |
| Session samples (open, lunch reopen, afternoon, close) | **NOT RUN** — windows pending, listed in §R2.3 |
| Bounded capture script, dry-run proven end to end | **DONE** — `f7/capture-intraday.sh`, §R2.2 |
| Is `prev_price` documented as the ex-rights/ex-dividend reference? | **ANSWERED — not documented** (§R2.4) |
| Documented source-observation timestamp or maximum delay? | **ANSWERED — none exists** (§R2.5) |
| Independent exchange reference for the 002322.SZ 2026-09-09 ex-date | **OBTAINED — and it corrects §4.4** (§R2.6) |
| Conservative calendar re-establishment after startup | **PROPOSED, text only** (§R2.7) |

Nothing here upgrades an inference to proof. The intraday questions the handoff asks —
does today's bar exist and move during the session, what `auction/snapshot`'s
`data_status` says during continuous trading and the lunch break, whether the snapshot's
`data.timestamp` tracks anything upstream while the market moves — remain open until the
§R2.3 windows are captured.

## R2.1 What the initial run could not settle, restated

Every §1–§6 sample was taken at 17:1x, after the 15:00 close. After the close the
`(last_price, volume, turnover)` triple is frozen, so a match between the snapshot and the
day's last bar corroborates a date and cannot uniquely date the observation. That is why
§4.1 is "PASS with one extra request" and not "PASS": the extra request was verified only
in a state where nothing could move.

## R2.2 Capture script and its after-hours dry run

`sdd/features/a-share-engine/f7/capture-intraday.sh <label>` — bash, curl and jq, with a
`python3 -c` fallback used only for JSON parsing when jq is absent. It reads the key from
`/Users/xyril/Projects/MarketRig/.env` (override with `MARKETRIG_ENV_FILE`) into
`HITHINK_API_KEY`, sends it only as `X-api-key` against `https://fuyao.aicubes.cn`, exactly
as `crates/marketrigd/src/hithink.rs:35` and `:464` build the request.

One invocation is one sample set, at most **14 requests**:

| Order | Request | Count |
| --- | --- | ---: |
| 1, 5, 9 | `GET /api/a-share/prices/snapshot?thscodes=600519.SH,601318.SH,000001.SZ,000858.SZ,300750.SZ` (the five `CN` catalog entries, `catalog.rs:106-110`) | 3 |
| 2–4, 6–8, 10–12 | `GET /api/a-share/prices/historical?thscode=<code>&interval=1d&start=<now−10d>&end=<now>&adjust=none` for `600519.SH`, `000001.SZ`, `300750.SZ` | 9 |
| 13 | `GET /api/a-share/calendar/trading-days` | 1 |
| 14 | `GET /api/a-share/auction/snapshot?thscodes=<the five>&stage=final` | 1 |

The snapshot + three-bar pair repeats three times ~20 s apart inside the one invocation,
so a run takes ~42 s and shows whether anything moved. Recorded per request: local
`started_at` / `ended_at` (ISO, `+0800`, `TZ=Asia/Shanghai` forced), `http_status`,
envelope `code`, and only the redacted fields the handoff names. The run stops at the
first `http_status: 429` **or** envelope `code: 429`, records it in `stopped_early`, and
still writes the file. Output is `f7/intraday/<label>-<YYYYMMDDTHHMMSS+0800>.json`, one
file per invocation, so it is idempotent and safe under cron. Before the file is kept the
script itself greps it for the key and deletes the file and exits 4 if it is present.

**Dry run, after hours** — `./sdd/features/a-share-engine/f7/capture-intraday.sh
dryrun-afterhours` at 18:15:32, exit 0, 42.2 s wall,
`f7/intraday/dryrun-afterhours-20260909T181532+0800.json`, labelled in the file itself
`"note": "after-hours dry run — NOT a session sample"`.

- 14/14 requests `http_status: 200`, envelope `code: 0`, `stopped_early: null`. No rate
  limit was reached at this volume.
- Key check: `grep -c "$HITHINK_API_KEY" sdd/features/a-share-engine/f7/intraday/*` → `0`.

New observations from the dry run, all after hours and none a session sample:

- The snapshot's `data.timestamp` is **second-granular and never later than the request**:
  request starts 18:15:32 / 18:15:53 / 18:16:13 returned `…932000` / `…950000` /
  `…973000` = 18:15:32 / 18:15:50 / 18:16:13, i.e. 0 s, −3 s, 0 s. The calendar's and the
  auction's timestamps carry milliseconds (`…974620`, `…974837`) and are plainly
  response-assembly clocks. So §3's "the snapshot's timestamp is the response clock" is
  right that it tracks the wall clock and wrong to imply it is the same clock the calendar
  stamps; it may be a coarse cache-fill time. Either way it is **not a per-datum time**:
  all three reads returned identical prices and volumes four hours after the close.
  Whether it tracks anything upstream while the market moves is exactly what the session
  samples must show.
- `auction/snapshot` at 18:15 returned `auction_phase: "closed"`, `data_status: "final"`
  for all five — the same pair as 17:12, six hours after the auction ended. One
  after-hours value still says nothing about the lunch break or a halt.
- `prev_price` (snapshot) == `pre_close_price` (auction) 5/5: 1309.30, 55.70, 11.78,
  71.65, 335.49. Two surfaces, one reference.
- Today's bar exists on all three bar codes with `date 2026-09-09`, `close_price` equal to
  the snapshot's `last_price` and `volume` equal to the snapshot's `volume`
  (1290.88/3222611, 11.70/58230598, 336.84/39024000), and `data.timestamp` =
  `1788883200000` = 2026-09-09 00:00 +0800 on all three. This reproduces §4.1 after hours
  and adds nothing to it.

## R2.3 The windows still required

All times Asia/Shanghai. **2026-09-10 is a Thursday and is expected to be a trading day,
which is not yet provable**: `calendar/trading-days` returns `[today − 1 year, today]`, so
tomorrow cannot appear in a list fetched today. The 09:31 run's own calendar request is
the confirmation — if its `max_date` is not `20260910`, that day is a holiday, every
sample from it is void, and the windows move to the next day whose `max_date` matches.

| # | Window (start) | Session state it must catch | Command |
| --- | --- | --- | --- |
| 1 | 2026-09-10 09:31:00 | continuous trading, one minute after the open | `./sdd/features/a-share-engine/f7/capture-intraday.sh open-0931` |
| 2 | 2026-09-10 09:45:00 | continuous trading, settled | `./sdd/features/a-share-engine/f7/capture-intraday.sh open-0945` |
| 3 | 2026-09-10 12:59:00 | lunch break, one minute before reopen — expected closed/paused | `./sdd/features/a-share-engine/f7/capture-intraday.sh lunch-1259` |
| 4 | 2026-09-10 13:01:00 | continuous trading, one minute after reopen | `./sdd/features/a-share-engine/f7/capture-intraday.sh lunch-1301` |
| 5 | 2026-09-10 14:30:00 | continuous trading, afternoon | `./sdd/features/a-share-engine/f7/capture-intraday.sh pm-1430` |
| 6 | 2026-09-10 14:57:00 | last minutes before the 15:00 close | `./sdd/features/a-share-engine/f7/capture-intraday.sh close-1457` |

Run each from the worktree root:

```bash
cd /Users/xyril/Projects/MarketRig/.worktrees/a-share-feasibility
./sdd/features/a-share-engine/f7/capture-intraday.sh open-0931     # 09:31:00
./sdd/features/a-share-engine/f7/capture-intraday.sh open-0945     # 09:45:00
./sdd/features/a-share-engine/f7/capture-intraday.sh lunch-1259    # 12:59:00
./sdd/features/a-share-engine/f7/capture-intraday.sh lunch-1301    # 13:01:00
./sdd/features/a-share-engine/f7/capture-intraday.sh pm-1430       # 14:30:00
./sdd/features/a-share-engine/f7/capture-intraday.sh close-1457    # 14:57:00
```

or, unattended, as six cron lines (each run finishes ~42 s after it starts, so the 12:59
run stays inside the lunch break and the 14:57 run inside the session):

```cron
CAP=/Users/xyril/Projects/MarketRig/.worktrees/a-share-feasibility/sdd/features/a-share-engine/f7/capture-intraday.sh
31 9  * * 1-5 $CAP open-0931  >> /tmp/f7-capture.log 2>&1
45 9  * * 1-5 $CAP open-0945  >> /tmp/f7-capture.log 2>&1
59 12 * * 1-5 $CAP lunch-1259 >> /tmp/f7-capture.log 2>&1
1  13 * * 1-5 $CAP lunch-1301 >> /tmp/f7-capture.log 2>&1
30 14 * * 1-5 $CAP pm-1430    >> /tmp/f7-capture.log 2>&1
57 14 * * 1-5 $CAP close-1457 >> /tmp/f7-capture.log 2>&1
```

Six windows × 14 requests = 84 requests across a trading day, which is below the volume
that produced the single `code: 429` in §3. If any run reports `stopped_early`, stop for
the day and record it; do not retry the window.

What the six files must be read for, and what none of them can prove: whether the
current-day bar exists at 09:31 and again at 13:01; whether its `close_price` tracks the
snapshot's `last_price` intraday or lags it; whether the two ever disagree because the
requests race, which is to be **kept**, not smoothed; what `auction_phase` and
`data_status` say during continuous trading and during the lunch break; whether the
snapshot's `data.timestamp` stops tracking the wall clock when the market is paused. None
of that establishes a source delay (§R2.5).

## R2.4 Is `prev_price` documented as the exchange ex-rights/ex-dividend reference? — **No**

Searched the **whole** `HiThink-Tech/Financial-API` repository, not only the vendored
skill subtree: `docs/` (20 files), `README.md`, `CHANGELOG.md`, `AGENTS.md`, `examples/`,
`python/`, `hithink-finance-cli/`, `skills/`. At the pinned commit
`44b7aa34dd504675f3ddaa15b3d478ea16f97884` and at HEAD — which are **the same commit**:
`GET https://api.github.com/repos/HiThink-Tech/Financial-API/commits/main` returned
`44b7aa34dd504675f3ddaa15b3d478ea16f97884`, committed `2026-09-08T12:12:16Z`, and
`diff -rq` between the two downloaded trees reports no difference. There is no separate
HEAD text to quote.

Every occurrence of `prev_price` or 前收盘 in the repository:

- `docs/api/endpoints-prices.md:57` and `:52`, verbatim:

  ```
  | `prev_price` | number | 前收盘价。 |
  | `price_change` | number | 相对前收盘价的涨跌额。 |
  ```

- `docs/api/endpoints-fund.md:170` — `prev_price` named in a field list, no definition.
- `skills/hithink-finance/references/api/…` — byte-identical copies of the two lines above.
- `examples/inspirations/05-watchlist-anomalies/example.html:106,110` — sample data and
  the label 开盘 / 昨收.
- `hithink-finance-cli/src/infrastructure/duckdb/factors.ts:14` — a comment in the local
  DuckDB adjustment-factor builder, verbatim:

  ```
   * 3. event_ratio — 基于前收盘价计算除权日的复权比率
  ```

  It describes the CLI computing its own factors from the daily bar's previous close; it
  says nothing about the snapshot's `prev_price`.

**Not documented.** The whole documented contract for `prev_price` is the three characters
前收盘价, "previous close", with no statement about ex-rights, ex-dividend or any exchange
adjustment. §4.4's finding stands as an *observation* of behaviour, now confirmed against
the exchange (§R2.6), and it remains undocumented behaviour the provider has never
promised.

## R2.5 Documented source-observation timestamp or maximum delay? — **None**

No delay, latency or freshness guarantee exists anywhere in the repository. The complete
set of statements that touch data time:

- `docs/api/endpoints-prices.md:41` (snapshot), verbatim:

  ```
  | `timestamp` | long \| null | 数据就绪时间（毫秒）。按 `thscodes` 显式取数时为 `null`；分页模式下为序列中最新有效时间。 |
  ```

  The live service contradicts this (§3, §R2.2): in `thscodes` mode it is not null.

- `docs/api/endpoints-prices.md:107` (historical), verbatim:

  ```
  | `timestamp` | long | 数据就绪时间（毫秒），为序列中最新一根 K 线的上游有效时间。 |
  ```

  The only "upstream valid time" in the contract, and it is a **daily** bar time.

- `docs/api/endpoints-auction.md:23`, verbatim:

  ```
  `timestamp` 始终是接口响应组装时间，在 `live`、`final`、`suspended` 和 `not_ready` 场景都会返回；上游行情时间仅用于判断数据新鲜度，不表示响应时间。`data_status` 用于区分数据尚未就绪、竞价完成或停牌等状态。
  ```

  It names an 上游行情时间 as the thing that would judge freshness — and does not return it.

- `docs/mcp/hithink-finance-a-share.md:40` states the same rule as an anti-pattern,
  verbatim:

  ```
  把 `timestamp` 当上游竞价发生时间，或省略标的拉全市场
  ```
- `docs/api/endpoints-calendar.md:30`, `endpoints-index.md:47,93`,
  `endpoints-special-data.md:43,98,173,260,442` — 数据就绪时间（毫秒）, unqualified.
- `README.md:511` (调用频率与限流), verbatim:

  ```
  服务可能根据实际运行情况动态调整限流策略。如触发限流，请主动降低请求频率和并发度，并在适当延迟后重试。
  ```

  A throttling statement, not a data-delay statement.
- `README.md` 当前公开能力边界 lists 「A股最新行情快照」 as a capability and 分钟 K and
  tick 数据 as 当前暂不公开提供, with no freshness qualifier on the snapshot.

The word 延迟 appears twice in the docs and never as a bound: once in the throttling
paragraph above, once at `docs/api/endpoints-special-data.md:277` about a ranking list —
> `把榜单排名当作无延迟交易信号：榜单数据有延迟，不构成交易信号。`

**No source-delay guarantee documented.** There is no per-item observation time, no
upstream quote time, no stated maximum age, and no SLA. This cannot be fixed by sampling:
the §R2.3 windows can show that a number *changed*, never how old the number was.

## R2.6 Independent exchange reference for 002322.SZ, 2026-09-09 — obtained, and it corrects §4.4

Source: the Shenzhen Stock Exchange's own disclosure service, not a data vendor.

1. `POST https://www.szse.cn/api/disc/announcement/annList` (JSON body
   `{"seDate":["2026-08-01","2026-09-09"],"stock":["002322"],"channelCode":["listedNotice_disc"],"pageSize":30,"pageNum":1}`)
   → **HTTP 200**, 14 announcements, including
   `理工能科：2026年半年度权益分派实施公告`, published 2026-09-03,
   `attachPath: /disc/disk03/finalpage/2026-09-03/ab1bff83-2295-4a4b-b40e-1663d1b0678e.PDF`.
2. `GET https://disc.static.szse.cn/download/disc/disk03/finalpage/2026-09-03/ab1bff83-2295-4a4b-b40e-1663d1b0678e.PDF`
   → **HTTP 200**, `application/pdf`, 124,738 bytes, 4 pages, 公告编号 2026-045,
   宁波理工环境能源科技股份有限公司.
3. `POST http://www.cninfo.com.cn/new/hisAnnouncement/query` (retried with a
   `searchkey=权益分派` form) → **HTTP 200** with `"totalAnnouncement":0` again, as in §4.5.
   cninfo's query API returned nothing for this issuer on either attempt; SZSE's own
   service did.

**Correction: this is the 2026 半年度 (interim) distribution, not a 2025 annual one.** The
2026-09-09 ex-date comes from the interim distribution approved on 2026-08-31. Verbatim,
from the PDF:

> 向全体股东每 10 股派送现金红利 3.4 元（含税），以自有资金共计派送 118,835,293.8 元。
> 公司 2026 年半年度不以资本公积金转增股本，不送红股。

> 三、股权登记日与除权除息日
> 本次权益分派股权登记日为：2026 年 9 月 8 日；
> 除权除息日为：2026 年 9 月 9 日。

> 六、相关参数调整情况
> 本次权益分派实施后除权除息价格计算时，每 10 股现金红利=实际现金分红总金额/股权登记日的
> 总股本*10 股=118,835,293.8÷365,527,970 股×10 股=3.251058 元（不四舍五入）。（每股现金
> 红利=实际现金分红总额/股权登记日的总股本=118,835,293.8÷365,527,970 股=0.3251058 元。）
> 本次权益分派实施后除权除息价格=股权登记日收盘价-0.3251058 元/股。

The declared dividend is 3.4 元 per 10 shares on a base of 349,515,570 shares, which
**excludes 16,012,400 shares held in the buy-back account** (回购专用证券账户所持有的本公司
股份不参与本次权益分派). The ex-dividend **reference price** is computed on the full
365,527,970 shares outstanding at the record date, so the per-share cash that comes off
the reference is 0.3251058, not 0.34.

Applying the issuer's own published formula to the record-date close from
`prices/historical?adjust=none` (`f7/exdate-comparison.json`, 2026-09-08 close 12.71):

```
12.71 − 0.3251058 = 12.3848942  →  12.38 at a 0.01 tick
```

which is HiThink's `prev_price` **exactly**.

Consequences for §4.4, stated as corrections:

- The count is **16/16 exact**, not 15/16 with one tick of error. `prev_price` reproduced
  the exchange's published ex-dividend reference on every one of the 16 names.
- §4.4's explanation of the cent — "`dividend_per_share` is rounded to 2 dp, a true 0.335
  would give 12.38" — is **wrong and is withdrawn**. The true figure is 0.3251058 and the
  cause is the buy-back exclusion, not rounding.
- §5 step 3's cross-check is the part that must change. `corporate-actions/adjustment-factors`
  returns the **declared** `dividend_per_share` (0.34); the exchange reference uses the
  **effective** per-share cash after the buy-back dilution (0.3251058). The endpoint
  carries no total-share-capital or buy-back column, so the effective figure is not
  derivable from it. A locally derived reference is therefore wrong by an unbounded, not
  ±0.01, amount whenever an issuer excludes treasury shares — here 0.0149, and it would be
  larger for a bigger buy-back. **Do not gate on `|derived − prev_price| ≤ one tick`**; it
  would have blocked 002322.SZ on a day the provider was correct. Use `prev_price` as the
  reference, and keep the derivation only as a logged, non-blocking sanity note.
- What still stands unproven: 16 pure-cash events on one day are not the 送股 or 配股 arms
  of the formula, and this is one exchange (SZSE) confirmation for one name.

## R2.7 Conservative calendar re-establishment after startup (proposal, text only)

`hithink.rs:678` `cn_phase` reads `(live.feed, live.calendar)`: only the
`(Hithink, Some(days))` arm consults the trading-day set; every other case — including
`Hithink` with **no set fetched yet** — falls through to `(session, Calendar::Weekday)`,
which reports `OPEN` on a holiday and after a restart until the next refresh lands.
`hithink.rs:694` `refresh_calendar_if_due` fires on the first `CN` cycle and again on the
first cycle past Shanghai midnight, keeps the set in memory only, and on failure keeps
whatever it holds — including nothing — while reporting `WEEKDAY`.

Proposed rule, in order:

1. **Start UNAVAILABLE.** With no calendar response yet under the HiThink feed, CN
   execution is `UNAVAILABLE` with reason `NO_CALENDAR`. Never `WEEKDAY`, never `OPEN`.
   The weekday rule may still label the *display* phase; it must not gate execution.
2. **Become available only on a positive calendar response**: `code == 0` and
   `max(item[].date) == shanghai_date(now)`. That is one request, and it is the provider
   asserting its own most recent trading day at or before its own today (§4.2).
3. **Distinguish holiday from staleness on the negative case**, using the same response:
   if `shanghai_date(data.timestamp) == shanghai_date(now)` and `max(date) < that date`,
   today is a **holiday** — reason `MARKET_CLOSED_HOLIDAY`, and that is a settled, quiet
   state, not a fault. Otherwise the list is stale or refused: reason `NO_CALENDAR`, retry
   next cycle, stay unavailable.
4. **Revalidate on day rollover.** The existing `shanghai_date(fetched_at) != shanghai_date(now)`
   trigger is right; what must change is the failure behaviour — on rollover the held set
   becomes **not applicable to today** immediately, so execution returns to `UNAVAILABLE`
   until step 2 succeeds for the new date, instead of continuing on yesterday's set.
5. **Also revalidate after any provider outage** that made the feed unavailable
   (`KEY_REJECTED`, repeated `Unreachable`), for the same reason: the set is evidence
   about a day, and the day may have turned while the feed was down.

**Storage: none is needed.** The rule above re-derives everything from one request per
day, and the conservative default in the absence of that request is already the safe one.
Holding the set in memory and starting `UNAVAILABLE` is strictly safer than persisting it,
because a persisted set is exactly the artefact that can outlive its day and re-enable
execution without a fresh read. This contradicts §5's closing line ("what must be
persisted … the proven trading date"): for the **calendar** nothing must be persisted.
Any persistence question that remains belongs to the per-instrument reference (`prev_price`
and the date it was proven for), and per the handoff it is not added merely to make a read
look proven — the same "start unavailable, prove it again" rule covers a restart at no
storage cost, at the price of N + 1 requests after every restart.

## R2.8 Confirmed provider facts versus assumptions

| # | Statement | Standing | Basis |
| --- | --- | --- | --- |
| 1 | `prev_price` equals the exchange's published ex-dividend reference | **Confirmed**, 16/16, one of them against SZSE's own announcement | §4.4, §R2.6 |
| 2 | `prev_price` is *documented* to be that reference | **False** — documented only as 前收盘价 | §R2.4 |
| 3 | `prev_price` == auction `pre_close_price` | **Confirmed** on 21 observations (16 ex-date names + 5 catalog names) | §4.4, §R2.2 |
| 4 | `historical?adjust=none` last bar's `date_ms` is a provider-asserted trading date | **Confirmed** | §4.1 |
| 5 | That bar's `close_price` equals the snapshot's `last_price` | **Confirmed after the close only** (19 observations, all after 17:00) — assumption intraday | §4.1, §R2.2 |
| 6 | `calendar/trading-days` `max(date) == today` proves today is a trading day | **Confirmed** | §4.2 |
| 7 | The calendar's `data.timestamp` is the server clock and separates holiday from stale list | **Confirmed** | §3, §4.2 |
| 8 | The snapshot's `data.timestamp` is a data time | **False** — tracks the wall clock, second-granular, 0–3 s before the request, while prices were frozen for 4 h | §3, §R2.2 |
| 9 | The snapshot's `data.timestamp` is exactly the response-assembly clock | **Unproven** — plausibly a coarse cache-fill time; the calendar and auction stamps carry ms, the snapshot's do not | §R2.2 |
| 10 | `auction_phase` / `data_status` distinguish the lunch break, continuous trading and a halt | **Assumption** — two after-hours samples, both `closed` / `final` | §4.3, §R2.2 |
| 11 | Monotonic `volume` proves the observation is of the current session | **Assumption** — increase may itself be delayed; unfalsifiable without a source time | §4.3 |
| 12 | A locally derived reference agrees with `prev_price` within one tick | **False** — 002322.SZ diverges by 0.0149 because the buy-back account is excluded from the distribution but not from the reference base | §R2.6 |
| 13 | Rights issues (配股) are reconstructible from the per-thscode endpoints | **False** — no `allotment_*` fields outside the parquet dump | §2.3 |
| 14 | Any documented maximum source delay exists | **False — none documented anywhere** | §R2.5 |
| 15 | Today's bar exists and moves during the session | **NOT RUN** — the §R2.3 windows | — |
| 16 | Rate limiting answers `code: 429`; its HTTP status | **Envelope code confirmed once; HTTP status still uncaptured.** The dry run's 14 requests did not trigger it | §3, §R2.2 |

## R2.9 Source delay is unknown

**MarketRig cannot state, bound, or measure how old a HiThink `CN` snapshot datum is.**
The provider publishes no per-item observation time, no upstream quote time, no maximum
delay, and no SLA (§R2.5). The three quantities that exist are the response envelope's own
clock (§R2.2 — not a datum time), MarketRig's `received_at_ns` (its own clock), and the
change in `(last_price, volume, turnover)` between two reads (evidence of movement, of
unknown lag). The §R2.3 session samples will not change this: a value that changes proves
the datum is not older than the previous read *by the provider's own clock*, which is not
the exchange's. Any `age_ms` MarketRig shows is age **since receipt** and must be labelled
so wherever it appears.

## R2.10 The proposed weaker product ceiling, for user acceptance

Restated from the handoff, precisely, and not yet earned — §R2.3 must pass first:

> Confirmed trading day and adequately supported current-day reference; execution pauses
> on feed failure or missing reference; receipt age is visible; source delay remains
> unknown. Volume changes are supporting evidence, not a freshness certificate.

Mechanically, that is: CN execution is enabled only while (a) the day is confirmed by one
`calendar/trading-days` response whose `max(date)` is today (§R2.7), (b) each instrument's
current-day bar `date_ms` is today and its `close_price` agrees with the snapshot's
`last_price` (§4.1), and (c) `prev_price` is present, and it — not a derived value — is the
band basis (§R2.6). It pauses on feed failure, on a missing or unproven reference, and on
day rollover until (a) is re-established. The UI and the agent surface show age since
receipt, named as such.

This is **weaker than the current feature SPEC**, which promises a snapshot simulation
whose data is fresh within a bound. There is no bound to promise. Accepting this ceiling
means accepting these assumptions, each of which the product would rely on without proof:

1. **Unknown source delay.** A `CN` fill may be simulated against a price that is an
   unknown number of seconds old. Nothing in the product can detect or bound it.
2. **Movement, not freshness.** A changing `volume` is the only liveness signal, and its
   own lag is unknown. A name that is halted, and a name that simply did not trade, are
   indistinguishable to MarketRig unless assumption 3 holds.
3. **`auction/snapshot`'s `data_status` reports halts** (停牌) usefully during continuous
   trading. Currently an assumption on two after-hours samples (§R2.8 #10); the §R2.3
   windows test it, and if it fails, halts are invisible and the ceiling drops further.
4. **`prev_price` remains the exchange reference.** Confirmed 16/16 for pure-cash events
   (§R2.6) and undocumented (§R2.4), so the provider may change it without notice, and the
   送股 / 配股 arms are untested. There is no local cross-check that can catch a regression
   without false positives (§R2.8 #12).
5. **The current-day bar tracks the running last price intraday.** Confirmed only after the
   close (§R2.8 #5). If it lags intraday, the date attribution in (b) will disagree with a
   moving snapshot and the rule needs a tolerance the samples must define.

The recommendation on whether the blockers close under this ceiling is deliberately
withheld until the §R2.3 samples exist. What is already decided by evidence: `prev_price`
is the reference and a derived cross-check must not gate (§R2.6), no source-delay
guarantee can be offered (§R2.9), and the calendar needs no storage to be re-established
conservatively (§R2.7).
