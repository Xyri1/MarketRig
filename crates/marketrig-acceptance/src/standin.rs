//! The gate's stand-in chart feed.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §10.1, per R1-9. A
//! loopback HTTP server speaking the chart endpoint's shape, scripted per symbol
//! and mutable mid-run from the gate: advance a price, answer a burst of 429s, go
//! dark, or serve nothing for a symbol at all. It counts every request it
//! answered per symbol, which is how G18 asserts the retry bound exactly.
//!
//! The daemon polls `{base}/{yahoo_symbol}?interval=1d&range=1d` and reads
//! `chart.result[0].meta.{currency, regularMarketPrice, regularMarketTime}`. That
//! shape and the catalog's symbols are re-stated here from the feature SPEC on
//! purpose: the harness never links `marketrigd` (root SPEC §17).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
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

        let served = Arc::clone(&script);
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
                    .with_state(served);
                let _ = axum::serve(listener, app).await;
            });
        });
        Feed { base, script }
    }

    /// What `MARKETRIG_TEST_QUOTE_URL` is set to (feature SPEC §10.1).
    pub fn base(&self) -> &str {
        &self.base
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
