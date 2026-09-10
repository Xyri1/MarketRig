//! The gate's stand-in chart feed and stand-in HiThink service.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §10.1, per R1-9, and
//! `sdd/features/hithink-a-share/SPEC.md` §6.1, per HT-6. One loopback HTTP
//! server with two halves: the chart endpoint's shape at `/{symbol}`, scripted
//! per symbol and mutable mid-run from the gate — advance a price, answer a
//! burst of 429s, go dark, or serve nothing for a symbol at all, which is how
//! G18 asserts the retry bound exactly — and HiThink's four endpoints under
//! `/hithink/api/`, which is what the gate sets `MARKETRIG_TEST_HITHINK_URL` to.
//!
//! The daemon polls `{base}/{yahoo_symbol}?interval=1d&range=1d` and reads
//! `chart.result[0].meta.{currency, regularMarketPrice, regularMarketTime}`. That
//! shape, HiThink's own `{code, message, request_id, data}` envelope, and the
//! catalog's symbols are all re-stated here from the feature SPECs on purpose:
//! the harness never links `marketrigd` (root SPEC §17).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The catalog's Yahoo symbols with a currency and a starting price (feature SPEC
/// §3). `300750.SZ` is deliberately absent: G18 needs one catalog instrument the
/// feed never serves, so the desk never accepts an observation for it.
const SEED: [(&str, &str, &str); 14] = [
    ("AAPL", "USD", "316.85"),
    ("MSFT", "USD", "512.40"),
    ("NVDA", "USD", "188.60"),
    ("AMZN", "USD", "241.30"),
    ("TSLA", "USD", "402.15"),
    ("0700.HK", "HKD", "441.40"),
    ("9988.HK", "HKD", "110.40"),
    ("0005.HK", "HKD", "160.60"),
    ("1299.HK", "HKD", "75.95"),
    ("3690.HK", "HKD", "76.65"),
    ("600519.SS", "CNY", "1500.00"),
    ("601318.SS", "CNY", "55.20"),
    ("000001.SZ", "CNY", "12.30"),
    ("000858.SZ", "CNY", "132.50"),
];

/// The one catalog instrument [`SEED`] leaves unserved (G18).
pub const UNSERVED_INSTRUMENT: &str = "300750.XSHE";

/// `meta.regularMarketTime` of the first answer, in seconds.
const BASE_TIME_S: i64 = 1_788_206_401;

/// One scripted tick: a whole currency unit up, a minute later.
const PRICE_STEP: i64 = 100;
const TIME_STEP_S: i64 = 60;

/// One symbol's script.
#[derive(Debug)]
struct Symbol {
    currency: &'static str,
    /// The price in hundredths, so a tick is exact arithmetic and never a float.
    price: i64,
    /// `meta.regularMarketTime`, seconds. It must advance for the daemon to
    /// replace an observation rather than merely refresh it (feature SPEC §2.1).
    time_s: i64,
    /// How many more requests answer 429.
    fail_429: u32,
    /// While true every request is refused: the feed has gone dark.
    dark: bool,
    /// Every request answered for this symbol, whatever the answer.
    hits: u32,
}

type Script = Arc<Mutex<HashMap<String, Symbol>>>;

/// A running stand-in feed.
///
/// ponytail: the server thread is never joined — it dies with the test binary,
/// exactly as the daemon's own scripted fixtures do. The upgrade path is a
/// shutdown signal if a single run ever needs two feeds in sequence.
pub struct Feed {
    base: String,
    script: Script,
    hithink: HithinkScript,
}

impl Feed {
    /// Binds a loopback port and serves the whole seeded catalog from it.
    pub fn start() -> Feed {
        let script: Script = Arc::new(Mutex::new(
            SEED.iter()
                .map(|(symbol, currency, price)| {
                    (
                        (*symbol).to_owned(),
                        Symbol {
                            currency,
                            price: hundredths(price),
                            time_s: BASE_TIME_S,
                            fail_429: 0,
                            dark: false,
                            hits: 0,
                        },
                    )
                })
                .collect(),
        ));

        // Bound synchronously so the caller learns the port before the server
        // thread has started, and loopback only.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the stand-in feed");
        let base = format!("http://{}", listener.local_addr().expect("local address"));
        listener
            .set_nonblocking(true)
            .expect("the stand-in listener is nonblocking");

        let hithink: HithinkScript = Arc::new(Mutex::new(Hithink {
            prices: CN_THSCODES.iter().map(|c| (*c, CN_PRICE)).collect(),
            days: default_trading_days(),
            rate_limited: 0,
            dark: 0,
            big_once: false,
            seen: Vec::new(),
        }));

        let served = Arc::clone(&script);
        let served_hithink = Arc::clone(&hithink);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for the stand-in feed");
            runtime.block_on(async move {
                let listener =
                    tokio::net::TcpListener::from_std(listener).expect("the stand-in listener");
                let app = Router::new()
                    .route("/{symbol}", get(quote))
                    .with_state(served)
                    .merge(
                        Router::new()
                            .route("/hithink/api/meta/tickers/search", get(ht_search))
                            .route("/hithink/api/a-share/prices/snapshot", get(ht_snapshot))
                            .route(
                                "/hithink/api/a-share/calendar/trading-days",
                                get(ht_calendar),
                            )
                            .route("/hithink/api/a-share/prices/historical", get(ht_historical))
                            .route(
                                "/hithink/api/a-share/financials/income-statements",
                                get(ht_income),
                            )
                            .with_state(served_hithink),
                    );
                let _ = axum::serve(listener, app).await;
            });
        });
        Feed {
            base,
            script,
            hithink,
        }
    }

    /// What `MARKETRIG_TEST_QUOTE_URL` is set to (feature SPEC §10.1).
    pub fn base(&self) -> &str {
        &self.base
    }

    /// What `MARKETRIG_TEST_HITHINK_URL` is set to (`hithink-a-share` §1.3,
    /// §6.1): the same server, one path segment along, so the daemon's own
    /// `{base}/api/…` lands on this half.
    pub fn hithink_base(&self) -> String {
        format!("{}/hithink", self.base)
    }

    /// One scripted tick: the price steps up and the source timestamp advances,
    /// which is what makes the daemon replace the observation and bump its
    /// sequence rather than merely refresh health (feature SPEC §2.1). Answers
    /// the new price as the decimal text the observation will carry.
    pub fn tick(&self, symbol: &str) -> String {
        let mut script = self.lock();
        let entry = symbol_of(&mut script, symbol);
        entry.price += PRICE_STEP;
        entry.time_s += TIME_STEP_S;
        decimal(entry.price)
    }

    /// The price this symbol currently serves, as decimal text.
    pub fn price(&self, symbol: &str) -> String {
        decimal(symbol_of(&mut self.lock(), symbol).price)
    }

    /// Answers 429 to the next `answers` requests for this symbol, then serves
    /// normally again (feature SPEC §2.1's retry bound).
    pub fn burst_429(&self, symbol: &str, answers: u32) {
        symbol_of(&mut self.lock(), symbol).fail_429 = answers;
    }

    /// Goes dark for this symbol — every request is refused — or comes back.
    pub fn dark(&self, symbol: &str, dark: bool) {
        symbol_of(&mut self.lock(), symbol).dark = dark;
    }

    /// Every request this symbol has been asked, whatever the answer.
    pub fn hits(&self, symbol: &str) -> u32 {
        symbol_of(&mut self.lock(), symbol).hits
    }

    /// Waits until this symbol has been quiet for a moment, so a script armed
    /// next is not raced by a poll already in flight — which is what makes G18's
    /// exact request counts deterministic. Bounded, and the caller's own bounded
    /// wait judges the outcome either way.
    pub fn quiet(&self, symbol: &str) {
        let mut last = self.hits(symbol);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(300));
            let now = self.hits(symbol);
            if now == last {
                return;
            }
            last = now;
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Symbol>> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[track_caller]
fn symbol_of<'a>(script: &'a mut HashMap<String, Symbol>, symbol: &str) -> &'a mut Symbol {
    script
        .get_mut(symbol)
        .unwrap_or_else(|| panic!("{symbol} is not in the stand-in feed's script"))
}

/// One poll. The order matters: a request is counted before it is judged, so the
/// 429 burst and the dark window are both visible in [`Feed::hits`].
async fn quote(State(script): State<Script>, Path(symbol): Path<String>) -> Response {
    let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(entry) = script.get_mut(&symbol) else {
        // Nothing is served for this symbol at all, ever.
        return (StatusCode::NOT_FOUND, "{}").into_response();
    };
    entry.hits += 1;
    if entry.dark {
        return (StatusCode::SERVICE_UNAVAILABLE, "{}").into_response();
    }
    if entry.fail_429 > 0 {
        entry.fail_429 -= 1;
        return (StatusCode::TOO_MANY_REQUESTS, "{}").into_response();
    }
    (
        [(header::CONTENT_TYPE, "application/json")],
        chart_body(&symbol, entry.currency, &decimal(entry.price), entry.time_s),
    )
        .into_response()
}

/// The chart-endpoint body, trimmed to the three `meta` fields the daemon reads
/// (feature SPEC §2.1). The price is a JSON number, as the endpoint sends it.
fn chart_body(symbol: &str, currency: &str, price: &str, time_s: i64) -> String {
    format!(
        r#"{{"chart":{{"result":[{{"meta":{{"currency":"{currency}","symbol":"{symbol}","regularMarketTime":{time_s},"regularMarketPrice":{price},"priceHint":2}},"timestamp":[{time_s}],"indicators":{{"quote":[{{}}]}}}}],"error":null}}}}"#
    )
}

/// Two-decimal text to hundredths, and back. Prices never become floats here for
/// the same reason they never do in the daemon.
fn hundredths(text: &str) -> i64 {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "00"));
    let fraction = format!("{fraction:0<2}");
    whole.parse::<i64>().expect("a whole number of units") * 100
        + fraction[..2].parse::<i64>().expect("two decimal places")
}

fn decimal(hundredths: i64) -> String {
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

// ---------------------------------------------------------------------------
// HiThink (feature SPEC `hithink-a-share` §6.1, per HT-6)
// ---------------------------------------------------------------------------

/// The key the gate stores through `PUT /research/hithink` — §6.1's `good` role.
/// Distinctive enough to grep for, which is what H1 does: it must reach neither
/// `marketrig.db`, the log root, an event, a launch file, a workspace, a CLI
/// capture, nor the bundle. Every other key is the `bad` role, which the search
/// endpoint refuses with `2003`.
pub const HITHINK_KEY: &str = "hithink-gate-key-7f3a";

/// The `CN` catalog's thscodes in catalog order (feature SPEC §2.1): what one
/// batched snapshot request must name, and what this half prices.
pub const CN_THSCODES: [&str; 5] = [
    "600519.SH",
    "601318.SH",
    "000001.SZ",
    "000858.SZ",
    "300750.SZ",
];

/// One CN `last_price`, in hundredths, so a tick is exact arithmetic and never a
/// float. Deliberately not the chart half's `600519.SS` price, so which provider
/// produced an observation is visible in the number as well as in the field.
const CN_PRICE: i64 = 168_800;

/// One scripted CN tick: a whole yuan up.
const CN_STEP: i64 = 100;

/// `meta/tickers/search`'s one fixed item. These are the exact bytes the
/// passthrough hands `marketrig research hithink`, which is what H3 compares
/// its standard output against.
pub const SEARCH_ENVELOPE: &str = r#"{"code":0,"message":"success","request_id":"gate-search","data":{"total":1,"item":[{"thscode":"600519.SH","ticker":"600519","name":"Kweichow Moutai"}]}}"#;

/// What any other key is answered (§1.2's `2003`).
const REJECTED_ENVELOPE: &str =
    r#"{"code":2003,"message":"invalid api key","request_id":"gate-reject","data":null}"#;

/// What a scripted rate-limited call is answered (§2.2's `4001`).
const RATE_LIMITED_ENVELOPE: &str =
    r#"{"code":4001,"message":"rate limited","request_id":"gate-4001","data":null}"#;

/// `a-share/financials/income-statements`'s one fixed envelope.
pub const INCOME_ENVELOPE: &str = r#"{"code":0,"message":"success","request_id":"gate-income","data":{"total":1,"item":[{"thscode":"600519.SH","report_date":"20251231","revenue":170000000000,"net_profit":86000000000}]}}"#;

/// The 1 MiB body, scripted once (§6.1) — above the CLI's 256 KiB inline
/// ceiling (§4.2) and well below the daemon's 8 MiB cap (§4.1).
const BIG_BODY_BYTES: usize = 1024 * 1024;

/// The HiThink half's script.
#[derive(Debug)]
struct Hithink {
    /// `last_price` per thscode, in hundredths.
    prices: HashMap<&'static str, i64>,
    /// The trading-day list, `yyyyMMdd` in Asia/Shanghai.
    days: BTreeSet<String>,
    /// How many more scriptable calls answer `4001`.
    rate_limited: u32,
    /// How many more scriptable calls go dark.
    dark: u32,
    /// Whether the next scriptable call answers a 1 MiB body.
    big_once: bool,
    /// Every request, whatever the answer: path, query, `X-api-key`.
    seen: Vec<(String, String, String)>,
}

type HithinkScript = Arc<Mutex<Hithink>>;

impl Feed {
    /// Every HiThink request this half has been asked, oldest first: the path
    /// with `/api/` stripped, the raw query, and the `X-api-key` header. H2
    /// reads it for "saw no snapshot request" and for the one request naming
    /// every `CN` thscode.
    pub fn hithink_requests(&self) -> Vec<(String, String, String)> {
        self.hithink_lock().seen.clone()
    }

    /// One scripted CN tick: the price steps up, so the `(last_price, volume,
    /// turnover)` triple changes and the daemon accepts a new observation
    /// rather than refreshing health (feature SPEC §2.2). Answers the new price
    /// as the decimal text the observation will carry.
    pub fn hithink_tick(&self, thscode: &str) -> String {
        let mut script = self.hithink_lock();
        let price = script
            .prices
            .get_mut(thscode)
            .unwrap_or_else(|| panic!("{thscode} is not in the HiThink stand-in's script"));
        *price += CN_STEP;
        decimal(*price)
    }

    /// The price this thscode currently serves, as decimal text.
    pub fn hithink_price(&self, thscode: &str) -> String {
        decimal(
            *self
                .hithink_lock()
                .prices
                .get(thscode)
                .unwrap_or_else(|| panic!("{thscode} is not in the HiThink stand-in's script")),
        )
    }

    /// Puts one `yyyyMMdd` into the trading-day list, or takes it out (§6.1's
    /// removable today, which is what makes the `CN` phase `CLOSED`).
    pub fn hithink_trading_day(&self, date: &str, trading: bool) {
        let mut script = self.hithink_lock();
        if trading {
            script.days.insert(date.to_owned());
        } else {
            script.days.remove(date);
        }
    }

    /// Answers `4001` to the next `calls` scriptable calls (§2.2's retry bound).
    pub fn hithink_rate_limited(&self, calls: u32) {
        self.hithink_lock().rate_limited = calls;
    }

    /// Goes dark for the next `calls` scriptable calls.
    pub fn hithink_dark(&self, calls: u32) {
        self.hithink_lock().dark = calls;
    }

    /// Answers the next scriptable call with a 1 MiB body.
    pub fn hithink_big_once(&self) {
        self.hithink_lock().big_once = true;
    }

    fn hithink_lock(&self) -> MutexGuard<'_, Hithink> {
        self.hithink.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Asia/Shanghai's calendar date for a Unix second, `yyyyMMdd` — the form
/// HiThink's list carries (feature SPEC §3). The zone has had no DST since 1991,
/// so it is UTC+8 flat and the harness needs no date library.
pub fn shanghai_date(unix_s: i64) -> String {
    crate::utc(unix_s + 8 * 3_600)[..10].replace('-', "")
}

/// Today in Asia/Shanghai — what the gate removes from the list.
pub fn shanghai_today() -> String {
    shanghai_date(crate::now_secs() as i64)
}

/// §6.1's default list: every weekday in a two-month window around now, and
/// today whatever weekday it is, so removing today is what closes the market
/// rather than the day the gate happens to run on.
fn default_trading_days() -> BTreeSet<String> {
    let now = crate::now_secs() as i64;
    (-30..=30)
        .map(|day: i64| now + day * 86_400)
        .filter(|at| *at == now || weekday(*at))
        .map(shanghai_date)
        .collect()
}

/// Monday through Friday in Asia/Shanghai. Day 0 of the Unix epoch was a
/// Thursday, so `(days + 4) % 7` is 0 for Sunday.
fn weekday(unix_s: i64) -> bool {
    let days = (unix_s + 8 * 3_600).div_euclid(86_400);
    (1..=5).contains(&(days + 4).rem_euclid(7))
}

/// The `X-api-key` a request carried, empty when it carried none.
fn key_of(headers: &HeaderMap) -> String {
    headers
        .get("X-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn json_body(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Records one request and answers the script's own answer where it has one.
///
/// ponytail: the daemon's two background paths — the batched snapshot and the
/// trading-day list — are recorded but never scripted, so a poll cycle already
/// in flight cannot eat the budget a scenario armed for its own research call.
/// The retry bound on those two is a module check's (`feed::hithink_retry_bound`),
/// not the gate's. The upgrade path is a per-path budget if a scenario ever
/// needs to script the feed itself.
fn scripted(
    script: &mut Hithink,
    path: &str,
    query: &Option<String>,
    headers: &HeaderMap,
    scriptable: bool,
) -> Option<Response> {
    script.seen.push((
        path.to_owned(),
        query.clone().unwrap_or_default(),
        key_of(headers),
    ));
    if !scriptable {
        return None;
    }
    if script.dark > 0 {
        script.dark -= 1;
        return Some((StatusCode::SERVICE_UNAVAILABLE, "{}").into_response());
    }
    if script.rate_limited > 0 {
        script.rate_limited -= 1;
        return Some(json_body(RATE_LIMITED_ENVELOPE.to_owned()));
    }
    if script.big_once {
        script.big_once = false;
        return Some(json_body(format!(
            r#"{{"code":0,"message":"success","request_id":"gate-big","data":"{}"}}"#,
            "x".repeat(BIG_BODY_BYTES)
        )));
    }
    None
}

/// `GET /api/meta/tickers/search` — the daemon's one bounded validation request
/// (§1.2) and H3's research read.
async fn ht_search(
    State(script): State<HithinkScript>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(answer) = scripted(&mut script, "meta/tickers/search", &query, &headers, true) {
        return answer;
    }
    json_body(match key_of(&headers) == HITHINK_KEY {
        true => SEARCH_ENVELOPE.to_owned(),
        false => REJECTED_ENVELOPE.to_owned(),
    })
}

/// `GET /api/a-share/prices/snapshot?thscodes=…` — one item per requested
/// thscode this half prices, in catalog order, with the `timestamp: null` the
/// thscodes mode documents (§2.2).
async fn ht_snapshot(
    State(script): State<HithinkScript>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
    scripted(
        &mut script,
        "a-share/prices/snapshot",
        &query,
        &headers,
        false,
    );
    let raw = query.unwrap_or_default();
    let requested: Vec<&str> = raw
        .strip_prefix("thscodes=")
        .unwrap_or_default()
        .split(',')
        .collect();
    let items: Vec<String> = CN_THSCODES
        .iter()
        .filter(|code| requested.contains(code))
        .map(|code| {
            format!(
                r#"{{"thscode":"{code}","ticker":"{code}","last_price":{},"volume":1200,"turnover":3400}}"#,
                decimal(script.prices[code])
            )
        })
        .collect();
    json_body(format!(
        r#"{{"code":0,"message":"success","request_id":"gate-snapshot","data":{{"timestamp":null,"total":{},"item":[{}]}}}}"#,
        items.len(),
        items.join(",")
    ))
}

/// `GET /api/a-share/calendar/trading-days` — the scripted set (§3).
async fn ht_calendar(
    State(script): State<HithinkScript>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
    scripted(
        &mut script,
        "a-share/calendar/trading-days",
        &query,
        &headers,
        false,
    );
    let items: Vec<String> = script
        .days
        .iter()
        .map(|date| format!(r#"{{"date_ms":0,"date":"{date}"}}"#))
        .collect();
    json_body(format!(
        r#"{{"code":0,"message":"success","request_id":"gate-calendar","data":{{"timestamp":0,"item":[{}]}}}}"#,
        items.join(",")
    ))
}

/// `GET /api/a-share/prices/historical?thscode=…` — the current-day unadjusted
/// daily bar the readiness rule dates an observation by (`a-share-engine` SPEC
/// §2.1): two bars, the newest at today's Asia/Shanghai midnight.
async fn ht_historical(
    State(script): State<HithinkScript>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
    scripted(
        &mut script,
        "a-share/prices/historical",
        &query,
        &headers,
        false,
    );
    let code = query
        .unwrap_or_default()
        .split('&')
        .find_map(|p| p.strip_prefix("thscode=").map(str::to_owned))
        .unwrap_or_default();
    let last = script
        .prices
        .get(code.as_str())
        .copied()
        .unwrap_or(CN_PRICE);
    let today_ms = shanghai_midnight_s() * 1_000;
    json_body(format!(
        r#"{{"code":0,"message":"success","request_id":"gate-historical","data":{{"timestamp":{today_ms},"item":[
           {{"date_ms":{},"close_price":{}}},
           {{"date_ms":{today_ms},"close_price":{}}}]}}}}"#,
        today_ms - 86_400_000,
        decimal(last - CN_STEP),
        decimal(last)
    ))
}

/// Today's Asia/Shanghai midnight, as a Unix second — what a current-day bar's
/// `date_ms` carries (F7 §2.2).
fn shanghai_midnight_s() -> i64 {
    let now = crate::now_secs() as i64;
    (now + 8 * 3_600) / 86_400 * 86_400 - 8 * 3_600
}

/// `GET /api/a-share/financials/income-statements` — one fixed envelope, and
/// the path H3 scripts `4001` and the 1 MiB body onto.
async fn ht_income(
    State(script): State<HithinkScript>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(answer) = scripted(
        &mut script,
        "a-share/financials/income-statements",
        &query,
        &headers,
        true,
    ) {
        return answer;
    }
    json_body(INCOME_ENVELOPE.to_owned())
}

#[test]
fn shanghai_dates_and_weekdays() {
    // 2026-09-10T00:30:00Z is 08:30 on the 10th in Shanghai; 2026-09-09T20:00:00Z
    // is already the 10th there. Both are Thursdays.
    assert_eq!(shanghai_date(1_789_000_200), "20260910");
    assert_eq!(shanghai_date(1_788_984_000), "20260910");
    assert!(weekday(1_789_000_200));
    // 2026-09-12 is a Saturday there, and the day after it a Sunday.
    assert_eq!(shanghai_date(1_789_173_000), "20260912");
    assert!(!weekday(1_789_173_000));
    assert!(!weekday(1_789_173_000 + 86_400));
    // The default list always carries today, whatever weekday it is.
    assert!(default_trading_days().contains(&shanghai_today()));
}
