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
