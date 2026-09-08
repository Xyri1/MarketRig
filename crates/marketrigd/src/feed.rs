//! The equity feed: the market calendars and their phase, MarketRig's own thin
//! Yahoo chart client, and the installation-wide market state the client feeds.
//! The `DataClient` registration, the node wiring, and the polling task that
//! drives [`next_delay`] belong to the node module.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §2.1, §2.2, §2.3,
//! §10.1, per R1-1, R1-3, R1-9; root `sdd/SPEC.md` §12.2.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Datelike, Timelike, Weekday};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::catalog::{Entry, Market};

/// The one fact a calendar yields (§2.2). Phase gates polling and labels
/// observations; it never gates an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Phase {
    Open,
    Closed,
}

/// One session as minutes from local midnight, half-open `[open, close)`: the
/// opening second (09:30:00) is `OPEN`, the closing second (16:00:00) is already
/// `CLOSED`.
type Session = (u32, u32);

const fn hm(hour: u32, minute: u32) -> u32 {
    hour * 60 + minute
}

const US: [Session; 1] = [(hm(9, 30), hm(16, 0))];
const HK: [Session; 2] = [(hm(9, 30), hm(12, 0)), (hm(13, 0), hm(16, 0))];
const CN: [Session; 2] = [(hm(9, 30), hm(11, 30)), (hm(13, 0), hm(15, 0))];

/// The §2.2 weekly table: one IANA zone and its Monday–Friday sessions.
///
/// ponytail: no holiday calendar (R1-3) — on an exchange holiday the phase reads
/// `OPEN`, polling proceeds, and the observation simply stops advancing, which age
/// and source time expose. The upgrade path is a per-market holiday table here.
pub(crate) fn calendar(market: Market) -> (Tz, &'static [Session]) {
    match market {
        Market::Us => (Tz::America__New_York, &US),
        Market::Hk => (Tz::Asia__Hong_Kong, &HK),
        Market::Cn => (Tz::Asia__Shanghai, &CN),
    }
}

/// The market phase at a UTC nanosecond instant ([`crate::store::now_ns`]'s clock).
pub fn phase(market: Market, at_ns: i64) -> Phase {
    let (zone, sessions) = calendar(market);
    let local = DateTime::from_timestamp_nanos(at_ns).with_timezone(&zone);
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
        return Phase::Closed;
    }
    let minute = local.hour() * 60 + local.minute();
    if sessions
        .iter()
        .any(|&(open, close)| minute >= open && minute < close)
    {
        Phase::Open
    } else {
        Phase::Closed
    }
}

/// The polling cadence (§2.1, R1-1): the steady tier, and the tightened one for
/// an instrument the desk is exposed to.
pub const IDLE_INTERVAL: Duration = Duration::from_secs(30);
pub const EXPOSED_INTERVAL: Duration = Duration::from_secs(10);

/// How long to wait before polling an instrument again — `None` while its market
/// is `CLOSED`, when nothing is polled at all (§2.1).
///
/// The node's polling task polls once at subscription whatever the phase, then
/// consults this before every later poll: `Some(d)` means sleep `d` and poll,
/// `None` means sleep [`IDLE_INTERVAL`] and re-consult without polling. `exposed`
/// is "the desk holds an open order or a nonflat position in this instrument".
pub fn next_delay(phase: Phase, exposed: bool) -> Option<Duration> {
    match phase {
        Phase::Closed => None,
        Phase::Open if exposed => Some(EXPOSED_INTERVAL),
        Phase::Open => Some(IDLE_INTERVAL),
    }
}

/// The compiled-in chart endpoint (§2.1). Its only override is the acceptance
/// seam below; no configuration surface reaches it (R1-9).
const CHART_BASE_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Points the feed at the gate's stand-in feed (§10.1, R1-9). Honored only
/// alongside [`crate::store::TEST_DATA_ROOT_ENV`].
pub const TEST_QUOTE_URL_ENV: &str = "MARKETRIG_TEST_QUOTE_URL";

/// Keeps the daemon off the compiled-in public endpoint (root §17). It does not
/// suppress polling a stand-in named by [`TEST_QUOTE_URL_ENV`] (§10.1).
pub const TEST_NO_TRADING_ENV: &str = "MARKETRIG_TEST_NO_TRADING";

/// The 429 retry policy (§2.1, per D76): 8 attempts in all, 400 ms apart.
pub const RETRY_ATTEMPTS: u32 = 8;
pub const RETRY_DELAY: Duration = Duration::from_millis(400);

/// The endpoint's base URL: the compiled-in one unless *both* test seam
/// variables are set (§10.1).
pub fn resolve_base_url(test_data_root: Option<&Path>, test_quote_url: Option<&str>) -> String {
    match (test_data_root, test_quote_url) {
        (Some(_), Some(url)) => url.trim_end_matches('/').to_owned(),
        _ => CHART_BASE_URL.to_owned(),
    }
}

/// The feed one daemon run polls (§10.1). A stand-in also lifts the calendar
/// gate on cadence: the gate must tick at any wall-clock hour, while phase
/// gating stays proven by the module checks and observations keep labeling the
/// real phase (R1-9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedBase {
    pub url: String,
    /// True only for the two-variable test seam's override.
    pub standin: bool,
}

impl FeedBase {
    /// A test stand-in, as the gate and the module fixtures construct it.
    pub fn standin(url: String) -> FeedBase {
        FeedBase { url, standin: true }
    }
}

/// The one feed base this daemon run polls, read once at startup and passed down
/// so nothing else depends on process environment ([`crate::store::Roots::from_env`]).
///
/// `None` is "no feed at all": nodes still start, no polling task runs, and every
/// quote stays `UNAVAILABLE`. That is what `MARKETRIG_TEST_NO_TRADING` buys — and
/// a stand-in named by both seam variables outranks it (§10.1).
pub fn feed_base_from_env() -> Option<FeedBase> {
    let test_data_root = env::var_os(crate::store::TEST_DATA_ROOT_ENV).map(PathBuf::from);
    let test_quote_url = env::var(TEST_QUOTE_URL_ENV).ok();
    match (test_data_root.as_deref(), test_quote_url.as_deref()) {
        (Some(root), Some(url)) => Some(FeedBase::standin(resolve_base_url(Some(root), Some(url)))),
        _ if env::var_os(TEST_NO_TRADING_ENV).is_some() => None,
        _ => Some(FeedBase {
            url: CHART_BASE_URL.to_owned(),
            standin: false,
        }),
    }
}

/// One accepted chart response: the three metadata fields the observation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartQuote {
    pub price: Decimal,
    pub currency: String,
    /// `meta.regularMarketTime`, seconds, widened to the `*_ns` clock.
    pub source_time_ns: i64,
}

/// MarketRig's own thin Yahoo chart client (§2.1, R1-1): URL construction and
/// response parsing, nothing more.
#[derive(Debug, Clone)]
pub struct ChartClient {
    http: reqwest::Client,
    base_url: String,
}

impl ChartClient {
    /// Builds the client against `base_url` (from [`feed_base_from_env`]).
    pub fn new(base_url: String) -> Result<ChartClient, String> {
        let http = reqwest::Client::builder()
            // slice §1: never detour the gate's loopback stand-in through a
            // machine proxy — and the public endpoint needs no proxy either.
            .no_proxy()
            // The endpoint answers 429 to a request with no User-Agent at all
            // (verified 2026-09-01); any plain one is accepted.
            .user_agent(concat!("MarketRig/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(ChartClient {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Fetches one instrument's latest chart metadata.
    ///
    /// HTTP 429 is retried up to [`RETRY_ATTEMPTS`] times [`RETRY_DELAY`] apart;
    /// exhaustion and every other failure — transport, non-200, unparseable body,
    /// missing field — answer `Err` for the caller to turn into `DEGRADED`. There
    /// is no panic and no substituted price (§2.1).
    pub async fn fetch(&self, yahoo_symbol: &str) -> Result<ChartQuote, String> {
        // The catalog's Yahoo symbols are ASCII letters, digits, and dots (§3),
        // so the path needs no percent-encoding.
        let url = format!("{}/{yahoo_symbol}?interval=1d&range=1d", self.base_url);
        let mut attempt = 1;
        loop {
            let response = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("{yahoo_symbol}: request failed: {e}"))?;
            let status = response.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt == RETRY_ATTEMPTS {
                    return Err(format!(
                        "{yahoo_symbol}: 429 on all {RETRY_ATTEMPTS} attempts"
                    ));
                }
                attempt += 1;
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
            if !status.is_success() {
                return Err(format!("{yahoo_symbol}: HTTP {}", status.as_u16()));
            }
            let body: ChartEnvelope = response
                .json()
                .await
                .map_err(|e| format!("{yahoo_symbol}: unparseable chart response: {e}"))?;
            return body.quote(yahoo_symbol);
        }
    }
}

/// The slice of the chart response MarketRig reads. Every other field — the
/// candle arrays, the trading periods — is ignored on purpose.
#[derive(Debug, Deserialize)]
struct ChartEnvelope {
    chart: ChartBody,
}

#[derive(Debug, Deserialize)]
struct ChartBody {
    result: Option<Vec<ChartResult>>,
}

#[derive(Debug, Deserialize)]
struct ChartResult {
    meta: ChartMeta,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChartMeta {
    currency: Option<String>,
    /// A JSON number; its own text is what becomes the decimal, never an `f64`.
    regular_market_price: Option<serde_json::Number>,
    regular_market_time: Option<i64>,
}

impl ChartEnvelope {
    fn quote(self, yahoo_symbol: &str) -> Result<ChartQuote, String> {
        let missing = |field: &str| format!("{yahoo_symbol}: chart response has no {field}");
        let meta = self
            .chart
            .result
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| missing("result"))?
            .meta;
        let number = meta
            .regular_market_price
            .ok_or_else(|| missing("meta.regularMarketPrice"))?;
        let price: Decimal = number
            .to_string()
            .parse()
            .map_err(|e| format!("{yahoo_symbol}: meta.regularMarketPrice {number}: {e}"))?;
        Ok(ChartQuote {
            price,
            currency: meta.currency.ok_or_else(|| missing("meta.currency"))?,
            source_time_ns: meta
                .regular_market_time
                .ok_or_else(|| missing("meta.regularMarketTime"))?
                * 1_000_000_000,
        })
    }
}

/// Feed-health evidence on a read (§2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Health {
    /// The most recent poll cycle succeeded.
    Live,
    /// A failure since the last success; the shown observation is the last
    /// accepted one, aging.
    Degraded,
    /// No observation was ever accepted; the price fields are omitted.
    Unavailable,
}

/// The two providers an observation can name (§2.3, feature SPEC
/// `hithink-a-share` §2.3): the one that produced it, never a substitute
/// (root §12.2).
const PROVIDER: &str = "yahoo";
pub const PROVIDER_HITHINK: &str = "hithink";

/// What a read yields, per instrument (§2.3). Serialized field-for-field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub instrument_id: &'static str,
    pub provider: &'static str,
    pub venue: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Omitted while nothing was ever observed, and serialized `null` on a
    /// HiThink observation, whose batched reply documents no source time
    /// (feature SPEC `hithink-a-share` §2.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time_ns: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_at_ns: Option<i64>,
    pub read_at_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<i64>,
    pub sequence: u64,
    pub market_phase: Phase,
    /// Which calendar rule labeled `market_phase` (feature SPEC
    /// `hithink-a-share` §3): `WEEKDAY` everywhere but a `CN` instrument on a
    /// fetched HiThink trading-day list.
    pub calendar: crate::hithink::Calendar,
    pub health: Health,
    pub book_synthesized: bool,
}

/// One instrument's synthesized top of book (§4.1, per D76): the observation it
/// is derived from, plus both sides equal to its last price at the instrument's
/// precision and both sizes one lot. An instrument with no observation carries no
/// price or size fields at all, exactly as §2.3 omits them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BookTop {
    #[serde(flatten)]
    pub observation: Observation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_size: Option<String>,
}

impl BookTop {
    fn of(observation: Observation, entry: &Entry) -> BookTop {
        let size = observation
            .last
            .is_some()
            .then(|| entry.lot_size.to_string());
        BookTop {
            bid_price: observation.last.clone(),
            ask_price: observation.last.clone(),
            bid_size: size.clone(),
            ask_size: size,
            observation,
        }
    }
}

/// The latest accepted observation for one instrument.
#[derive(Debug, Clone)]
struct Accepted {
    last: String,
    currency: String,
    /// The provider that produced it (§2.3, feature SPEC `hithink-a-share` §2.3).
    provider: &'static str,
    /// `None` on a HiThink observation: the batched snapshot carries no
    /// timestamp, so age counts from receipt.
    source_time_ns: Option<i64>,
    /// HiThink's change-detection triple, beside `last`: the raw JSON number
    /// text of `volume` and `turnover`, empty on a Yahoo observation.
    volume: String,
    turnover: String,
    received_at_ns: i64,
    sequence: u64,
}

#[derive(Debug, Default)]
struct Slot {
    observed: Option<Accepted>,
    /// A poll has failed since the last success (§2.3 `DEGRADED`).
    degraded: bool,
}

/// The daemon's one installation-wide market state (root §12.2): the latest
/// accepted observation per instrument, shared by every desk's node and never
/// persisted (root §15).
///
/// ponytail: one mutex over the whole instrument map — the catalog is fifteen
/// entries and a poll touches one of them, so contention is not a thing yet. The
/// upgrade path is a per-instrument lock (or a concurrent map) if the catalog and
/// the desk count both grow.
#[derive(Debug, Default)]
pub struct MarketState {
    slots: Mutex<HashMap<&'static str, Slot>>,
    /// The installation's HiThink provider, attached by the daemon at startup —
    /// it is what labels a `CN` read's phase and calendar, and what the `CN`
    /// poller reads each cycle (feature SPEC `hithink-a-share` §2.2, §3).
    ///
    /// ponytail: a `OnceLock` rather than a constructor argument, so the dozen
    /// module checks that build a bare `MarketState` keep building one; a
    /// daemon attaches exactly once, before any node starts.
    hithink: std::sync::OnceLock<std::sync::Arc<crate::hithink::Hithink>>,
}

impl MarketState {
    pub fn new() -> MarketState {
        MarketState::default()
    }

    /// Attaches the installation's HiThink provider. Called once, at startup.
    pub fn attach(&self, hithink: std::sync::Arc<crate::hithink::Hithink>) {
        let _ = self.hithink.set(hithink);
    }

    pub fn hithink(&self) -> Option<&std::sync::Arc<crate::hithink::Hithink>> {
        self.hithink.get()
    }

    /// Records a successful poll (§2.1). A source timestamp that advances
    /// replaces the observation and bumps the sequence; one that does not
    /// replaces nothing but still refreshes health to `LIVE`. An observation
    /// with no source time at all — HiThink's — always advances, which is what
    /// makes a provider switch visible on the next read.
    pub fn accept(&self, entry: &Entry, quote: &ChartQuote, received_at_ns: i64) {
        let mut slots = self.lock();
        let slot = slots.entry(entry.instrument_id).or_default();
        slot.degraded = false;
        let advances = slot
            .observed
            .as_ref()
            .is_none_or(|o| o.source_time_ns.is_none_or(|at| quote.source_time_ns > at));
        if advances {
            let sequence = slot.observed.as_ref().map_or(0, |o| o.sequence) + 1;
            slot.observed = Some(Accepted {
                last: at_precision(quote.price, entry.price_increment),
                currency: quote.currency.clone(),
                provider: PROVIDER,
                source_time_ns: Some(quote.source_time_ns),
                volume: String::new(),
                turnover: String::new(),
                received_at_ns,
                sequence,
            });
        }
    }

    /// Records one item of a HiThink snapshot (feature SPEC `hithink-a-share`
    /// §2.2). The batched reply carries no timestamp, so "did the market move"
    /// is the `(last_price, volume, turnover)` triple: an unchanged triple
    /// refreshes health alone, a changed one replaces the observation and
    /// advances the sequence. Returns the accepted price text, so the caller
    /// can publish its tick.
    pub fn accept_hithink(
        &self,
        entry: &Entry,
        price: Decimal,
        volume: &str,
        turnover: &str,
        received_at_ns: i64,
    ) -> Option<String> {
        let last = at_precision(price, entry.price_increment);
        let mut slots = self.lock();
        let slot = slots.entry(entry.instrument_id).or_default();
        slot.degraded = false;
        if slot.observed.as_ref().is_some_and(|o| {
            o.provider == PROVIDER_HITHINK
                && o.last == last
                && o.volume == volume
                && o.turnover == turnover
        }) {
            return None;
        }
        let sequence = slot.observed.as_ref().map_or(0, |o| o.sequence) + 1;
        slot.observed = Some(Accepted {
            last: last.clone(),
            currency: entry.currency.to_owned(),
            provider: PROVIDER_HITHINK,
            source_time_ns: None,
            volume: volume.to_owned(),
            turnover: turnover.to_owned(),
            received_at_ns,
            sequence,
        });
        Some(last)
    }

    /// Records a failed poll (§2.1): the last accepted observation stands and
    /// health becomes `DEGRADED` — never a silent substitution.
    pub fn mark_degraded(&self, instrument_id: &'static str) {
        self.lock().entry(instrument_id).or_default().degraded = true;
    }

    /// The §2.3 read for one instrument. Reads never mutate — the sequence
    /// advances on accepted updates alone (root §12.2).
    pub fn read(&self, entry: &Entry, read_at_ns: i64) -> Observation {
        // Outside the slot lock: the HiThink provider takes its own.
        let (market_phase, calendar) = self.phase_of(entry, read_at_ns);
        let slots = self.lock();
        let slot = slots.get(entry.instrument_id);
        let observed = slot.and_then(|s| s.observed.as_ref());
        Observation {
            instrument_id: entry.instrument_id,
            provider: observed.map_or(PROVIDER, |o| o.provider),
            venue: venue_of(entry.instrument_id),
            last: observed.map(|o| o.last.clone()),
            currency: observed.map(|o| o.currency.clone()),
            source_time_ns: observed.map(|o| o.source_time_ns),
            received_at_ns: observed.map(|o| o.received_at_ns),
            read_at_ns,
            age_ms: observed.map(|o| (read_at_ns - o.received_at_ns).max(0) / 1_000_000),
            sequence: observed.map_or(0, |o| o.sequence),
            market_phase,
            calendar,
            health: match (observed, slot.is_some_and(|s| s.degraded)) {
                (None, _) => Health::Unavailable,
                (Some(_), true) => Health::Degraded,
                (Some(_), false) => Health::Live,
            },
            book_synthesized: true,
        }
    }

    /// The phase and the rule that labeled it (feature SPEC `hithink-a-share`
    /// §3): a `CN` instrument asks the HiThink provider, everything else is
    /// R1's weekday rule.
    fn phase_of(&self, entry: &Entry, at_ns: i64) -> (Phase, crate::hithink::Calendar) {
        match (entry.market, self.hithink()) {
            (Market::Cn, Some(hithink)) => hithink.cn_phase(at_ns),
            _ => (
                phase(entry.market, at_ns),
                crate::hithink::Calendar::Weekday,
            ),
        }
    }

    /// The whole catalog, in catalog order — the `market/quotes` body (§7).
    pub fn read_all(&self, read_at_ns: i64) -> Vec<Observation> {
        crate::catalog::ENTRIES
            .iter()
            .map(|e| self.read(e, read_at_ns))
            .collect()
    }

    /// The whole catalog's synthesized top of book — the `market/book` body (§7).
    pub fn book_all(&self, read_at_ns: i64) -> Vec<BookTop> {
        crate::catalog::ENTRIES
            .iter()
            .map(|e| BookTop::of(self.read(e, read_at_ns), e))
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<&'static str, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The venue half of a `SYMBOL.VENUE` instrument identifier (§2.3).
fn venue_of(instrument_id: &'static str) -> &'static str {
    instrument_id
        .rsplit_once('.')
        .map_or("", |(_, venue)| venue)
}

/// Canonical decimal text at the instrument's precision — the decimal places of
/// its tick, never a formatting choice (§2.1, per D76). The node's polling task
/// builds its synthesized `QuoteTick` prices from this same text, so the sandbox
/// never sees a precision the instrument does not carry.
pub(crate) fn at_precision(price: Decimal, price_increment: &str) -> String {
    let tick: Decimal = price_increment
        .parse()
        .expect("catalog tick is decimal text (catalog::entries_valid)");
    let mut price = price;
    price.rescale(tick.scale());
    price.to_string()
}

/// The `CN` catalog entries, which are exactly the ones HiThink serves
/// (feature SPEC `hithink-a-share` §2.1).
pub fn cn_entries() -> impl Iterator<Item = &'static Entry> {
    crate::catalog::ENTRIES
        .iter()
        .filter(|e| e.market == Market::Cn)
}

/// One HiThink poll cycle for the whole `CN` leg (feature SPEC
/// `hithink-a-share` §2.2, per HT-2): one batched snapshot naming every `CN`
/// thscode, then one accept per item. Returns the instruments whose observation
/// advanced, with their accepted price text and receipt instant, so the caller
/// can publish their ticks; an instrument missing from the reply, and every
/// instrument on a failure, is `DEGRADED` with its last observation standing.
pub async fn poll_hithink(
    hithink: &crate::hithink::Hithink,
    market: &MarketState,
) -> Vec<(&'static Entry, String, i64)> {
    let entries: Vec<&'static Entry> = cn_entries().collect();
    let codes = entries
        .iter()
        .filter_map(|e| e.hithink_symbol)
        .collect::<Vec<_>>()
        .join(",");
    let degrade_all = || {
        for entry in &entries {
            market.mark_degraded(entry.instrument_id);
        }
    };
    let answer = match hithink
        .request(crate::hithink::SNAPSHOT_PATH, &format!("thscodes={codes}"))
        .await
    {
        Ok(answer) if answer.code == 0 => answer,
        Ok(answer) => {
            tracing::warn!(code = answer.code, "the HiThink snapshot was refused");
            degrade_all();
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!("the HiThink snapshot did not arrive: {e}");
            degrade_all();
            return Vec::new();
        }
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&answer.bytes) else {
        degrade_all();
        return Vec::new();
    };
    let items = body["data"]["item"].as_array().cloned().unwrap_or_default();
    let received_at_ns = crate::store::now_ns();
    let mut accepted = Vec::new();
    for entry in entries {
        let item = items
            .iter()
            .find(|item| item.get("thscode").and_then(|c| c.as_str()) == entry.hithink_symbol);
        // A JSON number's own text is what becomes the decimal, never an `f64`.
        let text = |item: &serde_json::Value, field: &str| {
            item.get(field)
                .and_then(|v| v.as_number().map(|n| n.to_string()))
        };
        let parsed = item.and_then(|item| {
            let price: Decimal = text(item, "last_price")?.parse().ok()?;
            Some((price, text(item, "volume")?, text(item, "turnover")?))
        });
        match parsed {
            Some((price, volume, turnover)) => {
                if let Some(last) =
                    market.accept_hithink(entry, price, &volume, &turnover, received_at_ns)
                {
                    accepted.push((entry, last, received_at_ns));
                }
            }
            None => {
                tracing::warn!(
                    instrument_id = entry.instrument_id,
                    "the HiThink snapshot carried no usable item"
                );
                market.mark_degraded(entry.instrument_id);
            }
        }
    }
    accepted
}

/// The instant of a wall-clock time in one of the calendar zones.
#[cfg(test)]
fn at(zone: Tz, y: i32, m: u32, d: u32, hour: u32, minute: u32, second: u32) -> i64 {
    use chrono::TimeZone;

    zone.with_ymd_and_hms(y, m, d, hour, minute, second)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap()
}

#[cfg(test)]
#[test]
fn phase_from_calendar() {
    use Market::{Cn, Hk, Us};

    let ny = Tz::America__New_York;
    let hkt = Tz::Asia__Hong_Kong;
    let sh = Tz::Asia__Shanghai;

    // US, Wednesday 2026-03-04: both half-open bounds, and no lunch break.
    assert_eq!(phase(Us, at(ny, 2026, 3, 4, 9, 29, 59)), Phase::Closed);
    assert_eq!(phase(Us, at(ny, 2026, 3, 4, 9, 30, 0)), Phase::Open);
    assert_eq!(phase(Us, at(ny, 2026, 3, 4, 12, 30, 0)), Phase::Open);
    assert_eq!(phase(Us, at(ny, 2026, 3, 4, 15, 59, 59)), Phase::Open);
    assert_eq!(phase(Us, at(ny, 2026, 3, 4, 16, 0, 0)), Phase::Closed);

    // HK: the 12:00–13:00 break is CLOSED between two OPEN sessions.
    assert_eq!(phase(Hk, at(hkt, 2026, 3, 4, 11, 59, 59)), Phase::Open);
    assert_eq!(phase(Hk, at(hkt, 2026, 3, 4, 12, 0, 0)), Phase::Closed);
    assert_eq!(phase(Hk, at(hkt, 2026, 3, 4, 12, 30, 0)), Phase::Closed);
    assert_eq!(phase(Hk, at(hkt, 2026, 3, 4, 13, 0, 0)), Phase::Open);
    assert_eq!(phase(Hk, at(hkt, 2026, 3, 4, 16, 0, 0)), Phase::Closed);

    // CN: the break is 11:30–13:00 and the close is 15:00.
    assert_eq!(phase(Cn, at(sh, 2026, 3, 4, 11, 29, 59)), Phase::Open);
    assert_eq!(phase(Cn, at(sh, 2026, 3, 4, 11, 30, 0)), Phase::Closed);
    assert_eq!(phase(Cn, at(sh, 2026, 3, 4, 12, 30, 0)), Phase::Closed);
    assert_eq!(phase(Cn, at(sh, 2026, 3, 4, 13, 0, 0)), Phase::Open);
    assert_eq!(phase(Cn, at(sh, 2026, 3, 4, 14, 59, 59)), Phase::Open);
    assert_eq!(phase(Cn, at(sh, 2026, 3, 4, 15, 0, 0)), Phase::Closed);

    // Weekends: Saturday 2026-03-07 and Sunday 2026-03-08.
    assert_eq!(phase(Us, at(ny, 2026, 3, 7, 12, 0, 0)), Phase::Closed);
    assert_eq!(phase(Hk, at(hkt, 2026, 3, 8, 10, 0, 0)), Phase::Closed);
    assert_eq!(phase(Cn, at(sh, 2026, 3, 7, 10, 0, 0)), Phase::Closed);

    // A US DST boundary: DST began Sunday 2026-03-08, so the same 09:30 wall clock
    // is two UTC instants 71 real hours apart (14:30 UTC on EST Friday, 13:30 UTC
    // on EDT Monday) and both are OPEN.
    let est = at(ny, 2026, 3, 6, 9, 30, 0);
    let edt = at(ny, 2026, 3, 9, 9, 30, 0);
    assert_eq!(edt - est, 71 * 3_600 * 1_000_000_000);
    assert_eq!(DateTime::from_timestamp_nanos(est).hour(), 14);
    assert_eq!(DateTime::from_timestamp_nanos(edt).hour(), 13);
    assert_eq!(phase(Us, est), Phase::Open);
    assert_eq!(phase(Us, edt), Phase::Open);
}

/// A local HTTP server answering a scripted list of `(status, body)` replies in
/// order (the last one repeating), counting what it served and recording each
/// request line (`GET /path?query`). Small enough to keep the retry and batching
/// checks honest about the wire without an HTTP framework in the test.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) fn scripted_server(
    replies: Vec<(u16, String)>,
) -> (
    String,
    std::sync::Arc<AtomicUsize>,
    std::sync::Arc<Mutex<Vec<String>>>,
) {
    use std::io::{BufRead, BufReader, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let served = std::sync::Arc::clone(&hits);
    let requests = std::sync::Arc::new(Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&requests);
    // Detached: a script the client abandons early leaves this thread parked in
    // accept() until the test binary exits, which is what we want.
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let mut first = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" => break,
                    Ok(_) => {
                        if first.is_empty() {
                            // `GET /path?query HTTP/1.1` without the version.
                            first = line
                                .trim_end()
                                .rsplit_once(' ')
                                .map_or(line.trim_end().to_owned(), |(head, _)| head.to_owned());
                            seen.lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(first.clone());
                        }
                    }
                }
            }
            let (status, body) = replies
                .get(n)
                .or_else(|| replies.last())
                .cloned()
                .unwrap_or((429, String::new()));
            served.fetch_add(1, Ordering::SeqCst);
            let reason = if status == 200 {
                "OK"
            } else {
                "Too Many Requests"
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    (base, hits, requests)
}

/// The chart-endpoint body shape, trimmed to what [`ChartEnvelope`] reads.
#[cfg(test)]
pub(crate) fn chart_body(symbol: &str, currency: &str, price: &str, time_s: i64) -> String {
    format!(
        r#"{{"chart":{{"result":[{{"meta":{{"currency":"{currency}","symbol":"{symbol}",
        "regularMarketTime":{time_s},"regularMarketPrice":{price},"priceHint":2}},
        "timestamp":[{time_s}],"indicators":{{"quote":[{{}}]}}}}],"error":null}}}}"#
    )
}

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
#[tokio::test]
async fn retry_on_429_bounded() {
    let entry = crate::catalog::find("AAPL.XNAS").unwrap();
    let state = MarketState::new();

    // Seven 429s then a 200: accepted on the eighth — the bound's last attempt.
    let mut script = vec![(429, String::new()); 7];
    script.push((200, chart_body("AAPL", "USD", "316.85", 1_788_206_401)));
    let (base, hits, _) = scripted_server(script);
    let quote = ChartClient::new(base)
        .unwrap()
        .fetch(entry.yahoo_symbol)
        .await
        .expect("the eighth attempt succeeds");
    assert_eq!(hits.load(Ordering::SeqCst), 8, "exactly eight requests");
    assert_eq!(quote.source_time_ns, 1_788_206_401 * 1_000_000_000);
    state.accept(entry, &quote, 1_000_000_000);
    let read = state.read(entry, 1_000_000_000);
    assert_eq!(read.health, Health::Live);
    assert_eq!(read.sequence, 1);
    assert_eq!(read.last.as_deref(), Some("316.85"));

    // Nine straight 429s: the client stops at eight, the observation stands.
    let (base, hits, _) = scripted_server(vec![(429, String::new()); 9]);
    let started = std::time::Instant::now();
    let error = ChartClient::new(base)
        .unwrap()
        .fetch(entry.yahoo_symbol)
        .await
        .expect_err("exhaustion is a failure, never a substituted price");
    let elapsed = started.elapsed();
    assert!(error.contains("429"), "{error}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        8,
        "the bound is eight attempts"
    );
    // Eight attempts are seven waits apart, so the run cannot be quicker than
    // that — the spacing is the other half of the §2.1 policy, and the policy is
    // the literal pair, not whatever the constants happen to say.
    assert_eq!(RETRY_ATTEMPTS, 8);
    assert_eq!(RETRY_DELAY, Duration::from_millis(400));
    assert!(
        elapsed >= RETRY_DELAY * 7,
        "eight attempts must be {RETRY_DELAY:?} apart, not {elapsed:?}"
    );
    assert!(
        elapsed < RETRY_DELAY * 14,
        "and the retries must not stretch: {elapsed:?}"
    );
    state.mark_degraded(entry.instrument_id);
    let read = state.read(entry, 2_000_000_000);
    assert_eq!(read.health, Health::Degraded);
    assert_eq!(
        read.last.as_deref(),
        Some("316.85"),
        "the prior observation"
    );
    assert_eq!(read.sequence, 1, "a failure never advances the sequence");
    assert_eq!(read.age_ms, Some(1_000), "and it ages");
}

#[cfg(test)]
#[test]
fn cadence_two_tier() {
    // Once at subscription whatever the phase, then nothing while CLOSED.
    assert_eq!(next_delay(Phase::Closed, false), None);
    assert_eq!(next_delay(Phase::Closed, true), None);

    // OPEN and idle: the 30-second tier. Exposed: the 10-second one.
    assert_eq!(next_delay(Phase::Open, false), Some(IDLE_INTERVAL));
    assert_eq!(next_delay(Phase::Open, true), Some(EXPOSED_INTERVAL));
    assert_eq!(IDLE_INTERVAL, Duration::from_secs(30));
    assert_eq!(EXPOSED_INTERVAL, Duration::from_secs(10));

    // The desk's exposure is the only thing that moves an OPEN instrument between
    // tiers, and it moves it back when the book goes flat and orderless.
    let mut exposed = false;
    let open = at(Tz::America__New_York, 2026, 3, 4, 10, 0, 0);
    let market = crate::catalog::find("AAPL.XNAS").unwrap().market;
    assert_eq!(
        next_delay(phase(market, open), exposed),
        Some(IDLE_INTERVAL)
    );
    exposed = true; // an open order, or a nonflat position
    assert_eq!(
        next_delay(phase(market, open), exposed),
        Some(EXPOSED_INTERVAL)
    );
    exposed = false; // flat and orderless again
    assert_eq!(
        next_delay(phase(market, open), exposed),
        Some(IDLE_INTERVAL)
    );

    // Same exposure, closed market: still nothing.
    let closed = at(Tz::America__New_York, 2026, 3, 4, 18, 0, 0);
    assert_eq!(next_delay(phase(market, closed), true), None);
}

#[cfg(test)]
#[test]
fn observation_provenance() {
    let entry = crate::catalog::find("0700.XHKG").unwrap(); // tick 0.20 → 2 places
    let state = MarketState::new();
    let received = at(Tz::Asia__Hong_Kong, 2026, 3, 4, 10, 0, 0);
    let source_s = received / 1_000_000_000 - 1;
    let read_at = received + 1_500_000_000;

    // Never observed: UNAVAILABLE with no price fields.
    let value = serde_json::to_value(state.read(entry, read_at)).unwrap();
    assert_eq!(value["health"], "UNAVAILABLE");
    assert_eq!(value["sequence"], 0);
    for omitted in [
        "last",
        "currency",
        "source_time_ns",
        "received_at_ns",
        "age_ms",
    ] {
        assert!(value.get(omitted).is_none(), "{omitted} must be omitted");
    }

    let quote = |price: &str, at_s: i64| ChartQuote {
        price: price.parse().unwrap(),
        currency: "HKD".to_owned(),
        source_time_ns: at_s * 1_000_000_000,
    };

    // The §2.3 shape, field for field.
    state.accept(entry, &quote("441.4", source_s), received);
    let value = serde_json::to_value(state.read(entry, read_at)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "instrument_id": "0700.XHKG", "provider": "yahoo", "venue": "XHKG",
            "last": "441.40", "currency": "HKD",
            "source_time_ns": source_s * 1_000_000_000,
            "received_at_ns": received, "read_at_ns": read_at, "age_ms": 1_500,
            "sequence": 1, "market_phase": "OPEN", "calendar": "WEEKDAY",
            "health": "LIVE", "book_synthesized": true,
        })
    );

    // A source timestamp that does not advance replaces nothing — but a poll that
    // succeeded refreshes health.
    state.mark_degraded(entry.instrument_id);
    assert_eq!(state.read(entry, read_at).health, Health::Degraded);
    state.accept(entry, &quote("999.00", source_s), read_at);
    let read = state.read(entry, read_at);
    assert_eq!(
        read.health,
        Health::Live,
        "a stale poll still refreshes health"
    );
    assert_eq!(read.sequence, 1, "and never advances the sequence");
    assert_eq!(read.last.as_deref(), Some("441.40"));
    assert_eq!(read.received_at_ns, Some(received));

    // An advancing one replaces the observation and bumps the sequence.
    state.accept(entry, &quote("442.6", source_s + 1), read_at);
    let read = state.read(entry, read_at);
    assert_eq!(read.sequence, 2);
    assert_eq!(read.last.as_deref(), Some("442.60"));
    assert_eq!(read.age_ms, Some(0));

    // Reads alone never advance anything.
    assert_eq!(state.read(entry, read_at + 1), read.clone_with(read_at + 1));

    // Precision is the instrument's tick, not the response's shape: the endpoint
    // quotes AAPL day highs to three places against a $0.01 tick.
    let aapl = crate::catalog::find("AAPL.XNAS").unwrap();
    state.accept(
        aapl,
        &ChartQuote {
            price: "321.235".parse().unwrap(),
            currency: "USD".to_owned(),
            source_time_ns: 1,
        },
        2,
    );
    assert_eq!(state.read(aapl, 2).last.as_deref(), Some("321.24"));
    assert_eq!(state.read_all(2).len(), crate::catalog::ENTRIES.len());

    // A HiThink observation names its own provider and carries `source_time_ns`
    // as an explicit `null`, because the batched snapshot has none — age counts
    // from receipt (feature SPEC `hithink-a-share` §2.3). With no provider
    // attached the calendar is still R1's weekday rule.
    let cn = crate::catalog::find("600519.XSHG").unwrap();
    let received = at(Tz::Asia__Shanghai, 2026, 3, 4, 10, 0, 0);
    state.accept_hithink(cn, "1688.5".parse().unwrap(), "12", "34", received);
    let value = serde_json::to_value(state.read(cn, received + 2_000_000_000)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "instrument_id": "600519.XSHG", "provider": "hithink", "venue": "XSHG",
            "last": "1688.50", "currency": "CNY",
            "source_time_ns": serde_json::Value::Null,
            "received_at_ns": received, "read_at_ns": received + 2_000_000_000,
            "age_ms": 2_000, "sequence": 1, "market_phase": "OPEN",
            "calendar": "WEEKDAY", "health": "LIVE", "book_synthesized": true,
        })
    );
}

// ---------------------------------------------------------------------------
// feed::hithink_* and feed::cn_phase_from_trading_days
// (feature SPEC `hithink-a-share` §7)
// ---------------------------------------------------------------------------

/// The snapshot body shape, trimmed to what [`poll_hithink`] reads: `timestamp`
/// is documented `null` in thscodes mode (feature SPEC `hithink-a-share` §2.2).
#[cfg(test)]
fn snapshot_body(items: &[(&str, &str, &str, &str)]) -> String {
    let item = items
        .iter()
        .map(|(thscode, last, volume, turnover)| {
            format!(
                r#"{{"thscode":"{thscode}","ticker":"{thscode}","last_price":{last},
                 "volume":{volume},"turnover":{turnover}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    crate::hithink::provider::envelope(
        0,
        &format!(
            r#"{{"timestamp":null,"total":{},"item":[{item}]}}"#,
            items.len()
        ),
    )
}

/// The whole `CN` leg's snapshot, at one price each.
#[cfg(test)]
fn every_cn(last: &str) -> String {
    let items: Vec<(&str, &str, &str, &str)> = cn_entries()
        .map(|e| (e.hithink_symbol.unwrap(), last, "1200", "3400"))
        .collect();
    snapshot_body(&items)
}

/// A stand-in HiThink whose key is already validated and stored, so the leg is
/// `AVAILABLE` with `a_share_feed: HITHINK`. Consumes the script's first reply.
#[cfg(test)]
async fn configured(base: &str) -> (tempfile::TempDir, std::sync::Arc<crate::hithink::Hithink>) {
    let (dir, hithink) = crate::hithink::provider::scratch(base);
    hithink.put("hithink-fake-key-0123456789").await.unwrap();
    (dir, hithink)
}

/// A `MarketState` with that provider attached, as the daemon wires it.
#[cfg(test)]
fn attached(hithink: &std::sync::Arc<crate::hithink::Hithink>) -> MarketState {
    let state = MarketState::new();
    state.attach(std::sync::Arc::clone(hithink));
    state
}

#[cfg(test)]
#[test]
fn hithink_change_detection() {
    let entry = crate::catalog::find("600519.XSHG").unwrap();
    let state = MarketState::new();
    let price: Decimal = "1688.5".parse().unwrap();

    // The first item is an observation; the same triple again is health alone.
    assert_eq!(
        state.accept_hithink(entry, price, "1200", "3400", 1_000),
        Some("1688.50".to_string())
    );
    assert_eq!(state.read(entry, 1_000).sequence, 1);
    state.mark_degraded(entry.instrument_id);
    assert_eq!(state.read(entry, 1_000).health, Health::Degraded);
    assert_eq!(
        state.accept_hithink(entry, price, "1200", "3400", 2_000),
        None,
        "an unchanged triple replaces nothing"
    );
    let read = state.read(entry, 2_000);
    assert_eq!(read.health, Health::Live, "but it does refresh health");
    assert_eq!(read.sequence, 1, "and never advances the sequence");
    assert_eq!(read.received_at_ns, Some(1_000), "nor the receipt");

    // Any leg of the triple moving is a new observation.
    assert!(
        state
            .accept_hithink(entry, price, "1300", "3400", 3_000)
            .is_some(),
        "volume moved"
    );
    assert_eq!(state.read(entry, 3_000).sequence, 2);
    assert!(
        state
            .accept_hithink(entry, price, "1300", "3500", 4_000)
            .is_some(),
        "turnover moved"
    );
    assert!(
        state
            .accept_hithink(entry, "1689.0".parse().unwrap(), "1300", "3500", 5_000)
            .is_some(),
        "the price moved"
    );
    let read = state.read(entry, 5_000);
    assert_eq!(read.sequence, 4);
    assert_eq!(read.last.as_deref(), Some("1689.00"));
    assert_eq!(
        read.source_time_ns,
        Some(None),
        "and still carries no source time"
    );
}

#[cfg(test)]
#[tokio::test]
async fn hithink_batches_one_request() {
    let (base, _hits, seen) = scripted_server(vec![
        (200, crate::hithink::provider::envelope(0, "null")),
        (200, every_cn("1688.5")),
    ]);
    let (_dir, hithink) = configured(&base).await;
    let market = attached(&hithink);

    let accepted = poll_hithink(&hithink, &market).await;
    assert_eq!(accepted.len(), 5, "every CN instrument advanced");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        2,
        "validation, then one snapshot for the whole leg: {requests:?}"
    );
    let codes: Vec<&'static str> = cn_entries().map(|e| e.hithink_symbol.unwrap()).collect();
    assert_eq!(
        requests[1],
        format!(
            "GET /api/a-share/prices/snapshot?thscodes={}",
            codes.join(",")
        ),
        "one request naming every CN thscode"
    );
    for entry in cn_entries() {
        let read = market.read(entry, 1);
        assert_eq!(read.health, Health::Live, "{}", entry.instrument_id);
        assert_eq!(read.provider, "hithink");
        assert_eq!(read.last.as_deref(), Some("1688.50"));
    }
    // US and HK are untouched by the CN leg.
    let aapl = crate::catalog::find("AAPL.XNAS").unwrap();
    assert_eq!(market.read(aapl, 1).health, Health::Unavailable);
}

#[cfg(test)]
#[tokio::test]
async fn a_share_feed_switch() {
    let (base, _hits, _seen) = scripted_server(vec![
        (200, crate::hithink::provider::envelope(0, "null")),
        (200, every_cn("1688.5")),
    ]);
    let (_dir, hithink) = configured(&base).await;
    let market = attached(&hithink);
    let entry = crate::catalog::find("600519.XSHG").unwrap();

    // One Yahoo observation, as R1 ships it.
    let yahoo = |price: &str, at_s: i64| ChartQuote {
        price: price.parse().unwrap(),
        currency: "CNY".to_owned(),
        source_time_ns: at_s * 1_000_000_000,
    };
    market.accept(entry, &yahoo("1680.0", 1_000), 1_000_000_000);
    let read = market.read(entry, 1_000_000_000);
    assert_eq!((read.provider, read.sequence), ("yahoo", 1));

    // The operator flips the toggle: the very next cycle names the other
    // provider, on the same node, with the sequence continuing (§2.2).
    assert_eq!(hithink.a_share_feed(), crate::hithink::AShareFeed::Hithink);
    poll_hithink(&hithink, &market).await;
    let read = market.read(entry, 2_000_000_000);
    assert_eq!((read.provider, read.sequence), ("hithink", 2));
    assert_eq!(read.source_time_ns, Some(None));

    // And back: a Yahoo poll after a HiThink one advances again, because a
    // HiThink observation carries no source time to compare against.
    hithink.patch(crate::hithink::AShareFeed::Yahoo).unwrap();
    market.accept(entry, &yahoo("1681.0", 1_001), 3_000_000_000);
    let read = market.read(entry, 3_000_000_000);
    assert_eq!((read.provider, read.sequence), ("yahoo", 3));
    assert_eq!(read.last.as_deref(), Some("1681.00"));
}

#[cfg(test)]
#[tokio::test]
async fn hithink_retry_bound() {
    let rate_limited = crate::hithink::provider::envelope(4001, "null");
    let (base, _hits, seen) = scripted_server(vec![
        (200, crate::hithink::provider::envelope(0, "null")),
        (200, rate_limited.clone()),
    ]);
    let (_dir, hithink) = configured(&base).await;
    let market = attached(&hithink);

    // `4001`: three attempts, and then the leg is degraded rather than fed a
    // substituted price (§2.2).
    let started = std::time::Instant::now();
    assert!(poll_hithink(&hithink, &market).await.is_empty());
    assert_eq!(
        seen.lock().unwrap().len(),
        1 + crate::hithink::ATTEMPTS as usize,
        "validation plus the bounded retry"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(1_500),
        "three attempts are 500 ms then 1 s apart"
    );
    for entry in cn_entries() {
        assert_eq!(market.read(entry, 1).health, Health::Unavailable);
    }

    // `1001` is a HiThink `1xxx`: answered once, never retried.
    let (base, _hits, seen) = scripted_server(vec![
        (200, crate::hithink::provider::envelope(0, "null")),
        (200, crate::hithink::provider::envelope(1001, "null")),
    ]);
    let (_dir, hithink) = configured(&base).await;
    let market = attached(&hithink);
    assert!(poll_hithink(&hithink, &market).await.is_empty());
    assert_eq!(seen.lock().unwrap().len(), 2, "one attempt on a 1xxx");

    // `2003` is answered once too, and it flips the row with exactly one event.
    let (base, _hits, seen) = scripted_server(vec![
        (200, crate::hithink::provider::envelope(0, "null")),
        (200, crate::hithink::provider::envelope(2003, "null")),
    ]);
    let (_dir, hithink) = configured(&base).await;
    let market = attached(&hithink);
    assert!(poll_hithink(&hithink, &market).await.is_empty());
    assert_eq!(seen.lock().unwrap().len(), 2, "one attempt on a 2xxx");
    let row = hithink.provider().unwrap();
    assert_eq!(row.state, "UNAVAILABLE");
    assert_eq!(row.failure_code.as_deref(), Some("KEY_REJECTED"));
    assert_eq!(
        row.a_share_feed,
        crate::hithink::AShareFeed::Hithink,
        "the daemon never switches the feed on its own"
    );
    assert!(!hithink.feed_ready(), "and no further request is issued");
    assert_eq!(
        crate::hithink::provider::events(hithink.store()).len(),
        2,
        "the PUT and the rejection"
    );
    // A second rejection while already unavailable writes nothing.
    poll_hithink(&hithink, &market).await;
    assert_eq!(crate::hithink::provider::events(hithink.store()).len(), 2);
}

#[cfg(test)]
#[tokio::test]
async fn cn_phase_from_trading_days() {
    let sh = Tz::Asia__Shanghai;
    let wednesday = at(sh, 2026, 3, 4, 10, 0, 0);
    let thursday = at(sh, 2026, 3, 5, 10, 0, 0);
    let days = |dates: &[&str]| {
        let item = dates
            .iter()
            .map(|d| format!(r#"{{"date_ms":0,"date":"{d}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        crate::hithink::provider::envelope(0, &format!(r#"{{"timestamp":0,"item":[{item}]}}"#))
    };
    let (base, _hits, seen) = scripted_server(vec![
        (200, crate::hithink::provider::envelope(0, "null")),
        (200, days(&["20260304", "20260305"])),
        (200, days(&["20260306"])),
    ]);
    let (_dir, hithink) = configured(&base).await;

    // Before any list: R1's weekday rule, named as such (§3).
    assert_eq!(
        hithink.cn_phase(wednesday),
        (Phase::Open, crate::hithink::Calendar::Weekday)
    );

    // The first CN cycle fetches it; a date on the list is OPEN under HITHINK.
    hithink.refresh_calendar_if_due(wednesday).await;
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(
        seen.lock().unwrap()[1],
        "GET /api/a-share/calendar/trading-days"
    );
    assert_eq!(
        hithink.cn_phase(wednesday),
        (Phase::Open, crate::hithink::Calendar::Hithink)
    );
    // Same Shanghai day: nothing is fetched again.
    hithink
        .refresh_calendar_if_due(wednesday + 3_600_000_000_000)
        .await;
    assert_eq!(seen.lock().unwrap().len(), 2, "one fetch per Shanghai day");

    // Past Shanghai midnight it refreshes, and a session hour on a date the new
    // list does not carry is CLOSED — still named HITHINK.
    hithink.refresh_calendar_if_due(thursday).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "the next Shanghai day refetches"
    );
    assert_eq!(
        hithink.cn_phase(thursday),
        (Phase::Closed, crate::hithink::Calendar::Hithink)
    );

    // Under Yahoo no list applies at all, whatever was fetched (§3).
    hithink.patch(crate::hithink::AShareFeed::Yahoo).unwrap();
    assert_eq!(
        hithink.cn_phase(thursday),
        (Phase::Open, crate::hithink::Calendar::Weekday)
    );
    hithink
        .refresh_calendar_if_due(at(sh, 2026, 3, 6, 10, 0, 0))
        .await;
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "and nothing is fetched under it"
    );
}

#[cfg(test)]
impl Observation {
    /// The same observation as read one nanosecond later: only `read_at_ns` moves.
    fn clone_with(&self, read_at_ns: i64) -> Observation {
        Observation {
            read_at_ns,
            ..self.clone()
        }
    }
}

#[cfg(test)]
#[test]
fn base_url_seam_only() {
    let root = Path::new("/scratch/marketrig");
    let stand_in = "http://127.0.0.1:52001/chart";

    // The quote URL alone is inert: only the data root marks a test run (§10.1).
    assert_eq!(resolve_base_url(None, Some(stand_in)), CHART_BASE_URL);
    // Both set: the stand-in, trailing slash trimmed.
    assert_eq!(
        resolve_base_url(Some(root), Some("http://127.0.0.1:52001/chart/")),
        stand_in
    );
    // Either half missing: the compiled-in endpoint.
    assert_eq!(resolve_base_url(Some(root), None), CHART_BASE_URL);
    assert_eq!(resolve_base_url(None, None), CHART_BASE_URL);
    assert_eq!(TEST_QUOTE_URL_ENV, "MARKETRIG_TEST_QUOTE_URL");
}
