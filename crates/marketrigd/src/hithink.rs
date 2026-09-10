//! HiThink: the installation's one A-share provider — its row and its key, the
//! one upstream request every caller shares, and the trading-day calendar.
//!
//! Contract: `sdd/features/hithink-a-share/SPEC.md` §1, §2.2, §3, per HT-1,
//! HT-2, HT-3.
//!
//! One key, one row, one client. The feed (`crate::feed::poll_hithink`), the
//! calendar below, and the research passthrough all go through [`Hithink::request`],
//! so the retry table and the `2003` rule are written once.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::DateTime;
use chrono_tz::Tz;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog::Market;
use crate::desk::append_event;
use crate::feed::{self, Phase};
use crate::memory::{Memory, MemoryError};
use crate::store::{Store, StoreError, now_ns};

/// The credential-store account the key lives under, in `memory`'s service
/// (§1.1, per D49). SQLite holds only the row.
pub const ACCOUNT: &str = "hithink_api_key";

/// The compiled-in service (§1.1). Its only override is the seam below.
const BASE_URL: &str = "https://fuyao.aicubes.cn";

/// Points HiThink at the gate's stand-in (§1.3). Honored only alongside
/// [`crate::store::TEST_DATA_ROOT_ENV`], and it lifts `MARKETRIG_TEST_NO_TRADING`
/// exactly as the quote stand-in does.
pub const TEST_HITHINK_URL_ENV: &str = "MARKETRIG_TEST_HITHINK_URL";

/// HiThink's retry contract (§2.2, per HT-2; `a-share-engine` SPEC §2.1): three
/// attempts, 500 ms then 1 s apart, on `4001`, envelope `429`, `5001`–`5003`,
/// HTTP 429 or 5xx, and transport errors — never on any other `1xxx`/`2xxx`
/// code. The real service answers rate limiting as envelope `429`, not `4001`
/// (F7 §3), so both codes are in the set.
pub const ATTEMPTS: u32 = 3;
pub const BACKOFF: [Duration; 2] = [Duration::from_millis(500), Duration::from_secs(1)];
/// What a stand-in waits instead, so the module checks pay no real seconds for
/// the bound. The policy the daemon runs is [`BACKOFF`].
const STANDIN_BACKOFF: [Duration; 2] = [Duration::from_millis(1), Duration::from_millis(1)];
const RATE_LIMITED: [i64; 2] = [4001, 429];
const KEY_REJECTED: i64 = 2003;
const RETRYABLE_SERVER: std::ops::RangeInclusive<i64> = 5001..=5003;

/// One upstream call's own bounds (§2.2, §4.1).
const TIMEOUT: Duration = Duration::from_secs(30);
pub const BODY_CAP: usize = 8 * 1024 * 1024;

/// The passthrough's installation-wide rhythm (§4.1, per HT-4): one research
/// call at a time, at least this long after the previous one.
const SPACING: Duration = Duration::from_millis(200);

/// The three paths the daemon itself calls (§1.2, §2.2, §3).
const VALIDATE_PATH: &str = "meta/tickers/search";
const VALIDATE_QUERY: &str = "q=600519&limit=1";
pub const SNAPSHOT_PATH: &str = "a-share/prices/snapshot";
const CALENDAR_PATH: &str = "a-share/calendar/trading-days";
/// The unadjusted daily bar that dates an observation (`a-share-engine` SPEC
/// §2.1). It is also a documented research path, and it shares this module's
/// one client and one key with the other three.
const HISTORICAL_PATH: &str = "a-share/prices/historical";
/// The bar window: ten days back from the read, wide enough to carry the
/// current day's bar across any holiday run.
const BAR_WINDOW_MS: i64 = 10 * 24 * 60 * 60 * 1_000;

const EVENT: &str = "HITHINK_PROVIDER_CHANGED";

const SELECT: &str = "SELECT state, a_share_feed, validated_at_ns, failure_code, \
                      failure_message FROM hithink_provider WHERE id = 1";

/// Which client serves the `CN` catalog entries (§1.1). The operator's choice,
/// never the daemon's: a rejected key degrades the leg, it never substitutes
/// Yahoo (root §12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum AShareFeed {
    Yahoo,
    Hithink,
}

impl AShareFeed {
    pub fn as_str(self) -> &'static str {
        match self {
            AShareFeed::Yahoo => "YAHOO",
            AShareFeed::Hithink => "HITHINK",
        }
    }

    fn from_row(raw: &str) -> AShareFeed {
        match raw {
            "HITHINK" => AShareFeed::Hithink,
            // The column's own CHECK is the vocabulary; anything else is Yahoo.
            _ => AShareFeed::Yahoo,
        }
    }
}

/// Which rule labeled an observation's phase (§3, per HT-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Calendar {
    /// HiThink's trading-day list.
    Hithink,
    /// R1's Monday–Friday session rule, which every `US` and `HK` observation
    /// carries too (per D78).
    Weekday,
}

/// Why CN execution is not available, in MarketRig's own vocabulary
/// (`a-share-engine` SPEC §2.1, §2.3, per AE-3, AE-8). One string enum shared by
/// the readiness checks here, the per-node execution state, and the order
/// refusals. Every variant is a block: out-of-session is `CLOSED`/`PAUSED` with
/// no reason at all, and the fill policy is its own field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Reason {
    NoCalendar,
    CalendarRefused,
    NotTradingDay,
    DateUnproven,
    NoReference,
    ReferenceChanged,
    PriceOutOfBand,
    FeedLost,
    RateLimited,
    PublicationFailed,
    PublicationPending,
    NodeNotStarted,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NoCalendar => "NO_CALENDAR",
            Reason::CalendarRefused => "CALENDAR_REFUSED",
            Reason::NotTradingDay => "NOT_TRADING_DAY",
            Reason::DateUnproven => "DATE_UNPROVEN",
            Reason::NoReference => "NO_REFERENCE",
            Reason::ReferenceChanged => "REFERENCE_CHANGED",
            Reason::PriceOutOfBand => "PRICE_OUT_OF_BAND",
            Reason::FeedLost => "FEED_LOST",
            Reason::RateLimited => "RATE_LIMITED",
            Reason::PublicationFailed => "PUBLICATION_FAILED",
            Reason::PublicationPending => "PUBLICATION_PENDING",
            Reason::NodeNotStarted => "NODE_NOT_STARTED",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The day's trading-day evidence (`a-share-engine` SPEC §2.1): the list, and
/// the Shanghai date it was adopted for. It authorizes execution only while that
/// date is still today — never carried across the rollover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradingDays {
    pub shanghai_date: String,
    pub days: BTreeSet<String>,
}

/// What one [`Hithink::refresh_calendar_if_due`] call did (§2.1), so a caller
/// can surface `CALENDAR_REFUSED` rather than guess from a phase label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarRefresh {
    /// No provider is available: there is no calendar to fetch.
    NotDue,
    /// Today's evidence already stands; no request was issued, so no refusal
    /// could revoke it.
    Held,
    /// A successful list for today, adopted — replacing any contradictory one.
    Adopted,
    Refused(Reason),
}

/// The `hithink_provider` row as the routes answer it — never a key (§1.2).
/// Named `HithinkProvider` in the OpenAPI document, because the memory
/// provider's own resource already holds `Provider` there (R5 §6.1).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = HithinkProvider)]
pub struct Provider {
    pub state: String,
    pub a_share_feed: AShareFeed,
    pub api_key_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// The compiled-in or seam value, never operator-set (§1.2).
    pub base_url: String,
}

/// A HiThink failure carrying a stable SCREAMING_SNAKE code (§1.2, §4.1).
#[derive(Debug)]
pub enum HithinkError {
    Validation(String),
    /// The upstream envelope refused the key: its own `code` and `message`.
    ProviderRejected {
        code: i64,
        message: String,
    },
    ProviderUnreachable(String),
    CredentialStoreUnavailable(String),
    /// No key is stored, so there is nothing to research or to toggle.
    ResearchUnconfigured,
    /// §4.1's three refusals. The passthrough route constructs them.
    ResearchPathUnknown(String),
    ResearchUnreachable {
        attempts: u32,
        last: String,
    },
    ResearchTooLarge(usize),
    Store(String),
}

impl HithinkError {
    pub fn code(&self) -> &'static str {
        match self {
            HithinkError::Validation(_) => "VALIDATION",
            HithinkError::ProviderRejected { .. } => "PROVIDER_REJECTED",
            HithinkError::ProviderUnreachable(_) => "PROVIDER_UNREACHABLE",
            HithinkError::CredentialStoreUnavailable(_) => "CREDENTIAL_STORE_UNAVAILABLE",
            HithinkError::ResearchUnconfigured => "RESEARCH_UNCONFIGURED",
            HithinkError::ResearchPathUnknown(_) => "RESEARCH_PATH_UNKNOWN",
            HithinkError::ResearchUnreachable { .. } => "RESEARCH_UNREACHABLE",
            HithinkError::ResearchTooLarge(_) => "RESEARCH_TOO_LARGE",
            HithinkError::Store(_) => "STORE",
        }
    }
}

impl fmt::Display for HithinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HithinkError::Validation(m) | HithinkError::Store(m) => write!(f, "{m}"),
            HithinkError::ProviderRejected { code, message } => {
                write!(
                    f,
                    "HiThink refused the request with code {code} ({message})."
                )
            }
            HithinkError::ProviderUnreachable(m) => write!(f, "HiThink did not answer ({m})."),
            HithinkError::CredentialStoreUnavailable(m) => {
                write!(f, "The credential store is unavailable ({m}).")
            }
            HithinkError::ResearchUnconfigured => write!(
                f,
                "HiThink is not configured: store an API key on /research/hithink first."
            ),
            HithinkError::ResearchPathUnknown(path) => {
                write!(f, "{path} is not a HiThink endpoint MarketRig knows.")
            }
            HithinkError::ResearchUnreachable { attempts, last } => write!(
                f,
                "HiThink did not answer in {attempts} attempts; the last was {last}."
            ),
            HithinkError::ResearchTooLarge(bytes) => write!(
                f,
                "The HiThink answer was larger than the {BODY_CAP}-byte ceiling ({bytes} bytes)."
            ),
        }
    }
}

impl std::error::Error for HithinkError {}

impl From<StoreError> for HithinkError {
    fn from(e: StoreError) -> Self {
        HithinkError::Store(e.to_string())
    }
}

impl From<MemoryError> for HithinkError {
    fn from(e: MemoryError) -> Self {
        HithinkError::CredentialStoreUnavailable(e.to_string())
    }
}

/// One upstream answer: the body verbatim (the passthrough returns it unchanged)
/// and the envelope fields the daemon itself reads (§2.2, §4.1).
#[derive(Debug)]
pub struct Answer {
    pub bytes: Vec<u8>,
    pub code: i64,
    pub message: String,
}

/// Why an upstream call yielded no envelope at all (§2.2).
#[derive(Debug)]
pub enum Failure {
    Unreachable { attempts: u32, last: String },
    TooLarge(usize),
}

impl Failure {
    /// The `PUT /research/hithink` mapping (§1.2).
    pub fn provider(self) -> HithinkError {
        HithinkError::ProviderUnreachable(match self {
            Failure::Unreachable { attempts, last } => format!("{last} ({attempts} attempts)"),
            Failure::TooLarge(bytes) => format!("the answer was {bytes} bytes"),
        })
    }

    /// The passthrough mapping (§4.1).
    pub fn research(self) -> HithinkError {
        match self {
            Failure::Unreachable { attempts, last } => {
                HithinkError::ResearchUnreachable { attempts, last }
            }
            Failure::TooLarge(bytes) => HithinkError::ResearchTooLarge(bytes),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Unreachable { attempts, last } => write!(f, "{last} ({attempts} attempts)"),
            Failure::TooLarge(bytes) => write!(f, "{bytes} bytes"),
        }
    }
}

/// The base URL and whether it is the seam's (§1.3): the compiled-in service
/// unless *both* seam variables are set, exactly like the quote feed's.
pub fn resolve(test_data_root: Option<&Path>, test_hithink_url: Option<&str>) -> (String, bool) {
    match (test_data_root, test_hithink_url) {
        (Some(_), Some(url)) => (url.trim_end_matches('/').to_owned(), true),
        _ => (BASE_URL.to_owned(), false),
    }
}

/// What one daemon run holds in memory: the row's own fields as the poller reads
/// them each cycle, the key, and the calendar (§3 — never persisted).
#[derive(Debug)]
struct Live {
    feed: AShareFeed,
    key: Option<String>,
    available: bool,
    /// Today's trading-day evidence, valid only while its own Shanghai date is
    /// today (`a-share-engine` SPEC §2.1).
    calendar: Option<TradingDays>,
    /// The Shanghai date whose calendar refresh was refused, so a day that
    /// never adopted a list can say `CALENDAR_REFUSED` instead of `NO_CALENDAR`.
    calendar_refused_on: Option<String>,
    /// Per instrument, the Shanghai date a current-day unadjusted bar proved
    /// (§2.1). Cleared by the rollover, like the calendar.
    bars: HashMap<&'static str, String>,
}

/// The installation's HiThink provider. One per daemon, in `ApiState` and on the
/// market state the nodes' pollers read.
pub struct Hithink {
    store: Store,
    /// The credential seam (§1.1): the same service and store the memory
    /// provider's key uses, under [`ACCOUNT`].
    memory: Arc<Memory>,
    base_url: String,
    /// True only for the two-variable seam's override (§1.3).
    pub standin: bool,
    /// False keeps this daemon off the public service entirely
    /// (`MARKETRIG_TEST_NO_TRADING`, §1.3).
    enabled: bool,
    /// The waits between the [`ATTEMPTS`] attempts: [`BACKOFF`] in production,
    /// [`STANDIN_BACKOFF`] against a stand-in.
    backoff: [Duration; 2],
    http: reqwest::Client,
    state: Mutex<Live>,
    /// The research gate (§4.1): held across the upstream call, carrying the
    /// instant the previous one finished.
    gate: tokio::sync::Mutex<Option<Instant>>,
}

impl fmt::Debug for Hithink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hithink")
            .field("base_url", &self.base_url)
            .field("standin", &self.standin)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl Hithink {
    /// Reads the seam variables once, like [`crate::feed::feed_base_from_env`],
    /// and loads the row and the key.
    pub fn new(store: Store, memory: Arc<Memory>) -> Arc<Hithink> {
        let test_data_root = env::var_os(crate::store::TEST_DATA_ROOT_ENV).map(PathBuf::from);
        let test_url = env::var(TEST_HITHINK_URL_ENV).ok();
        let (base_url, standin) = resolve(test_data_root.as_deref(), test_url.as_deref());
        let enabled = standin || env::var_os(feed::TEST_NO_TRADING_ENV).is_none();
        Hithink::build(store, memory, base_url, standin, enabled)
    }

    /// A stand-in base URL, as the gate's seam and the module fixtures build it.
    pub fn standin(store: Store, memory: Arc<Memory>, base_url: String) -> Arc<Hithink> {
        Hithink::build(store, memory, base_url, true, true)
    }

    fn build(
        store: Store,
        memory: Arc<Memory>,
        base_url: String,
        standin: bool,
        enabled: bool,
    ) -> Arc<Hithink> {
        let row = store.call(|c| c.query_row(SELECT, [], read_row));
        let row = row.unwrap_or_else(|e| {
            tracing::error!(error = %e, "the HiThink provider row could not be read");
            Row::unconfigured()
        });
        let key = memory.load_secret(ACCOUNT).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "the HiThink key could not be read");
            None
        });
        Arc::new(Hithink {
            store,
            memory,
            base_url,
            standin,
            enabled,
            backoff: if standin { STANDIN_BACKOFF } else { BACKOFF },
            http: reqwest::Client::builder()
                .no_proxy()
                .user_agent(concat!("MarketRig/", env!("CARGO_PKG_VERSION")))
                .timeout(TIMEOUT)
                .build()
                .expect("the HiThink HTTP client builds from constants"),
            state: Mutex::new(Live {
                available: row.state == "AVAILABLE",
                feed: row.a_share_feed,
                key,
                calendar: None,
                calendar_refused_on: None,
                bars: HashMap::new(),
            }),
            gate: tokio::sync::Mutex::new(None),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Live> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The feed the `CN` poller reads each cycle (§2.2).
    pub fn a_share_feed(&self) -> AShareFeed {
        self.lock().feed
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Whether a batched snapshot may be issued at all: the operator chose
    /// HiThink, a key is stored, and it has not been rejected (§2.2).
    pub fn feed_ready(&self) -> bool {
        let live = self.lock();
        live.feed == AShareFeed::Hithink && live.available && live.key.is_some() && self.enabled
    }

    // -----------------------------------------------------------------------
    // The one upstream call (§2.2)
    // -----------------------------------------------------------------------

    /// The request every caller shares: `GET {base}/api/{path}?{query}` with the
    /// stored key, HiThink's retry table, and the `2003` rule (§1.2, §2.2).
    pub async fn request(&self, path: &str, query: &str) -> Result<Answer, Failure> {
        let key = self.lock().key.clone();
        let Some(key) = key else {
            return Err(Failure::Unreachable {
                attempts: 0,
                last: "no HiThink key is stored".to_string(),
            });
        };
        let answer = self.fetch(path, query, &key).await?;
        if answer.code == KEY_REJECTED {
            self.key_rejected(&answer.message);
        }
        Ok(answer)
    }

    /// `GET /research/hithink/{path}` (§4.1, per HT-4): the two refusals, the
    /// installation-wide spacing gate, then the upstream body verbatim whatever
    /// its `code`. No event, no row — except the `2003` rule [`request`] owns.
    ///
    /// [`request`]: Self::request
    pub async fn research(&self, path: &str, query: &str) -> Result<Vec<u8>, HithinkError> {
        if !self.lock().available {
            return Err(HithinkError::ResearchUnconfigured);
        }
        if !crate::research_paths::RESEARCH_PATHS.contains(&path) {
            return Err(HithinkError::ResearchPathUnknown(path.to_string()));
        }
        Ok(self
            .spaced(path, query)
            .await
            .map_err(Failure::research)?
            .bytes)
    }

    /// The same call against a key that is not stored yet — validation's one
    /// bounded request (§1.2), which writes nothing whatever it answers.
    async fn fetch(&self, path: &str, query: &str, key: &str) -> Result<Answer, Failure> {
        if !self.enabled {
            return Err(Failure::Unreachable {
                attempts: 0,
                last: "this daemon is kept off the HiThink service".to_string(),
            });
        }
        let url = if query.is_empty() {
            format!("{}/api/{path}", self.base_url)
        } else {
            format!("{}/api/{path}?{query}", self.base_url)
        };
        let mut last = String::new();
        for attempt in 1..=ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(self.backoff[attempt as usize - 2]).await;
            }
            match self.attempt(&url, key).await {
                Once::Done(answer) => return Ok(answer),
                Once::TooLarge(bytes) => return Err(Failure::TooLarge(bytes)),
                Once::Fatal(why) => {
                    return Err(Failure::Unreachable {
                        attempts: attempt,
                        last: why,
                    });
                }
                Once::Retry(why) => last = why,
            }
        }
        Err(Failure::Unreachable {
            attempts: ATTEMPTS,
            last,
        })
    }

    async fn attempt(&self, url: &str, key: &str) -> Once {
        let response = match self.http.get(url).header("X-api-key", key).send().await {
            Ok(response) => response,
            Err(e) => return Once::Retry(self.memory.redact(&e.to_string())),
        };
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Once::Retry(format!("HTTP {}", status.as_u16()));
        }
        if let Some(length) = response.content_length()
            && length > BODY_CAP as u64
        {
            return Once::TooLarge(length as usize);
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return Once::Retry(self.memory.redact(&e.to_string())),
        };
        if bytes.len() > BODY_CAP {
            return Once::TooLarge(bytes.len());
        }
        let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
            return Once::Fatal(format!("HTTP {} carried no JSON envelope", status.as_u16()));
        };
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if RATE_LIMITED.contains(&code) || RETRYABLE_SERVER.contains(&code) {
            return Once::Retry(format!("code {code}"));
        }
        Once::Done(Answer {
            bytes: bytes.to_vec(),
            code,
            message: body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// A key HiThink rejected mid-run (§1.2): the row goes `UNAVAILABLE` with
    /// one event, and the `CN` leg degrades rather than switching provider.
    /// Idempotent — a second `2003` while already unavailable writes nothing.
    fn key_rejected(&self, message: &str) {
        let feed = {
            let mut live = self.lock();
            if !live.available {
                return;
            }
            live.available = false;
            live.feed
        };
        let at_ns = now_ns();
        let message = self.memory.redact(message);
        if let Err(e) = self.store.unit(move |tx| {
            tx.execute(
                "UPDATE hithink_provider SET state = 'UNAVAILABLE', \
                 failure_code = 'KEY_REJECTED', failure_message = ?1, updated_at_ns = ?2 \
                 WHERE id = 1",
                params![message, at_ns],
            )?;
            append_event(
                tx,
                EVENT,
                None,
                at_ns,
                json!({ "state": "UNAVAILABLE", "a_share_feed": feed.as_str() }),
            )
        }) {
            tracing::error!(error = %e, "the rejected HiThink key could not be recorded");
        }
    }

    // -----------------------------------------------------------------------
    // The routes (§1.2)
    // -----------------------------------------------------------------------

    /// `GET /research/hithink`.
    pub fn provider(&self) -> Result<Provider, HithinkError> {
        let row = self.store.call(|c| c.query_row(SELECT, [], read_row))?;
        Ok(self.resource(row))
    }

    /// `PUT /research/hithink {api_key}` (§1.2): one bounded request, then the
    /// key, then the row and its event in one unit. A refusal writes nothing.
    pub async fn put(&self, api_key: &str) -> Result<Provider, HithinkError> {
        let key = api_key.trim().to_string();
        if key.is_empty() {
            return Err(HithinkError::Validation(
                "The api_key must not be empty.".to_string(),
            ));
        }
        let answer = self
            .fetch(VALIDATE_PATH, VALIDATE_QUERY, &key)
            .await
            .map_err(Failure::provider)?;
        if answer.code != 0 {
            return Err(HithinkError::ProviderRejected {
                code: answer.code,
                message: answer.message,
            });
        }
        self.memory.store_secret(ACCOUNT, &key)?;

        let at_ns = now_ns();
        let row = self.store.unit(move |tx| {
            tx.execute(
                "UPDATE hithink_provider SET state = 'AVAILABLE', a_share_feed = 'HITHINK', \
                 validated_at_ns = ?1, failure_code = NULL, failure_message = NULL, \
                 updated_at_ns = ?1 WHERE id = 1",
                params![at_ns],
            )?;
            append_event(
                tx,
                EVENT,
                None,
                at_ns,
                json!({ "state": "AVAILABLE", "a_share_feed": "HITHINK" }),
            )?;
            tx.query_row(SELECT, [], read_row)
        })?;
        {
            let mut live = self.lock();
            live.key = Some(key);
            live.feed = AShareFeed::Hithink;
            live.available = true;
        }
        Ok(self.resource(row))
    }

    /// `DELETE /research/hithink` (§1.2): the key goes, the row returns to
    /// `UNCONFIGURED` with `YAHOO`, and a row that was already there is a
    /// no-op with no second event.
    pub fn delete(&self) -> Result<Provider, HithinkError> {
        self.memory.delete_secret(ACCOUNT)?;
        let at_ns = now_ns();
        let row = self.store.unit(move |tx| {
            let changed = tx.execute(
                "UPDATE hithink_provider SET state = 'UNCONFIGURED', a_share_feed = 'YAHOO', \
                 validated_at_ns = NULL, failure_code = NULL, failure_message = NULL, \
                 updated_at_ns = ?1 WHERE id = 1 AND state <> 'UNCONFIGURED'",
                params![at_ns],
            )?;
            if changed > 0 {
                append_event(
                    tx,
                    EVENT,
                    None,
                    at_ns,
                    json!({ "state": "UNCONFIGURED", "a_share_feed": "YAHOO" }),
                )?;
            }
            tx.query_row(SELECT, [], read_row)
        })?;
        {
            let mut live = self.lock();
            live.key = None;
            live.feed = AShareFeed::Yahoo;
            live.available = false;
            live.calendar = None;
            live.calendar_refused_on = None;
            live.bars.clear();
        }
        Ok(self.resource(row))
    }

    /// `PATCH /research/hithink {a_share_feed}` (§1.2): the toggle needs a key,
    /// and it appends an event only when the field moved.
    pub fn patch(&self, feed: AShareFeed) -> Result<Provider, HithinkError> {
        if self.lock().key.is_none() {
            return Err(HithinkError::ResearchUnconfigured);
        }
        let at_ns = now_ns();
        let chosen = feed.as_str();
        let row = self.store.unit(move |tx| {
            let changed = tx.execute(
                "UPDATE hithink_provider SET a_share_feed = ?1, updated_at_ns = ?2 \
                 WHERE id = 1 AND a_share_feed <> ?1",
                params![chosen, at_ns],
            )?;
            if changed > 0 {
                let state: String =
                    tx.query_row("SELECT state FROM hithink_provider WHERE id = 1", [], |r| {
                        r.get(0)
                    })?;
                append_event(
                    tx,
                    EVENT,
                    None,
                    at_ns,
                    json!({ "state": state, "a_share_feed": chosen }),
                )?;
            }
            tx.query_row(SELECT, [], read_row)
        })?;
        self.lock().feed = feed;
        Ok(self.resource(row))
    }

    fn resource(&self, row: Row) -> Provider {
        Provider {
            state: row.state,
            a_share_feed: row.a_share_feed,
            api_key_present: self.lock().key.is_some(),
            validated_at_ns: row.validated_at_ns,
            failure_code: row.failure_code,
            failure_message: row.failure_message,
            base_url: self.base_url.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // The calendar (§3)
    // -----------------------------------------------------------------------

    /// The `CN` phase and the rule that labeled it (§3, per HT-3) — awareness
    /// only. Under Yahoo, before any list has been fetched, or once the held
    /// list belongs to a past Shanghai day, this is R1's weekday rule exactly,
    /// and that fallback never authorizes execution ([`trading_day`] does).
    ///
    /// [`trading_day`]: Self::trading_day
    pub fn cn_phase(&self, at_ns: i64) -> (Phase, Calendar) {
        let session = feed::phase(Market::Cn, at_ns);
        let today = shanghai_date(at_ns);
        let live = self.lock();
        match (live.feed, live.calendar.as_ref()) {
            (AShareFeed::Hithink, Some(held)) if held.shanghai_date == today => {
                let open = session == Phase::Open && held.days.contains(&today);
                let phase = if open { Phase::Open } else { Phase::Closed };
                (phase, Calendar::Hithink)
            }
            _ => (session, Calendar::Weekday),
        }
    }

    /// Whether today is a proven trading day (`a-share-engine` SPEC §2.1) —
    /// the execution gate, which only a same-day list can open. The weekday
    /// fallback is not evidence: with no list this is `NO_CALENDAR`, and with a
    /// refused one for today, `CALENDAR_REFUSED`.
    pub fn trading_day(&self, at_ns: i64) -> Result<(), Reason> {
        let today = shanghai_date(at_ns);
        let live = self.lock();
        match live.calendar.as_ref() {
            Some(held) if held.shanghai_date == today => {
                if held.days.contains(&today) {
                    Ok(())
                } else {
                    Err(Reason::NotTradingDay)
                }
            }
            _ if live.calendar_refused_on.as_deref() == Some(today.as_str()) => {
                Err(Reason::CalendarRefused)
            }
            _ => Err(Reason::NoCalendar),
        }
    }

    /// Fetches the trading-day list on the first `CN` cycle of the day and
    /// again on the first cycle past Shanghai midnight (§3, `a-share-engine`
    /// SPEC §2.1). The calendar is the *provider's* answer, not the feed's: it
    /// is fetched whenever the provider row is `AVAILABLE`, whichever feed the
    /// operator chose, because Yahoo CN execution needs the same confirmed
    /// trading day (§2.1, AE-7). Today's adopted list is held for the day, so no redundant
    /// read is issued and no refusal can revoke it; a refusal on a day with no
    /// list is remembered as `CALENDAR_REFUSED` and retried next cycle; a
    /// successful list always replaces whatever was held, including a
    /// contradictory one.
    pub async fn refresh_calendar_if_due(&self, at_ns: i64) -> CalendarRefresh {
        let today = shanghai_date(at_ns);
        {
            let mut live = self.lock();
            if !live.available {
                return CalendarRefresh::NotDue;
            }
            // The rollover carries no evidence: a list, a refusal and every bar
            // from a past Shanghai day go before anything is asked again.
            if live
                .calendar
                .as_ref()
                .is_some_and(|held| held.shanghai_date != today)
            {
                live.calendar = None;
            }
            if live.calendar.is_some() {
                return CalendarRefresh::Held;
            }
            if live
                .calendar_refused_on
                .as_deref()
                .is_some_and(|d| d != today)
            {
                live.calendar_refused_on = None;
            }
            live.bars.retain(|_, proven| *proven == today);
        }
        let refused = |why: Reason| {
            self.lock().calendar_refused_on = Some(today.clone());
            CalendarRefresh::Refused(why)
        };
        match self.request(CALENDAR_PATH, "").await {
            Ok(answer) if answer.code == 0 => {
                let days = trading_days(&answer.bytes);
                if days.is_empty() {
                    tracing::warn!("the HiThink trading-day list was empty");
                    return refused(Reason::CalendarRefused);
                }
                let mut live = self.lock();
                live.calendar = Some(TradingDays {
                    shanghai_date: today,
                    days,
                });
                live.calendar_refused_on = None;
                CalendarRefresh::Adopted
            }
            Ok(answer) => {
                tracing::warn!(
                    code = answer.code,
                    "the HiThink trading-day list was refused"
                );
                refused(rate_limited_or(answer.code, Reason::CalendarRefused))
            }
            Err(e) => {
                tracing::warn!("the HiThink trading-day list did not arrive: {e}");
                refused(match failure_reason(&e) {
                    Reason::RateLimited => Reason::RateLimited,
                    _ => Reason::CalendarRefused,
                })
            }
        }
    }

    /// Per-instrument current-day bar evidence (`a-share-engine` SPEC §2.1):
    /// one successful unadjusted daily read per instrument per Shanghai day,
    /// whose newest bar must be dated today. Success is held for the day and
    /// cleared by the rollover; a failed or refused read answers `Err` for this
    /// call and is retried on the next one — the poll cadence, never a loop.
    pub async fn bar_evidence(
        &self,
        entry: &'static crate::catalog::Entry,
        at_ns: i64,
    ) -> Result<(), Reason> {
        let today = shanghai_date(at_ns);
        let Some(thscode) = entry.hithink_symbol else {
            return Err(Reason::DateUnproven);
        };
        {
            let mut live = self.lock();
            live.bars.retain(|_, proven| *proven == today);
            if live.bars.get(entry.instrument_id) == Some(&today) {
                return Ok(());
            }
        }
        // The F7 request shape: one thscode, daily, unadjusted, a window that
        // ends now and is wide enough to carry the current day's bar.
        let end_ms = at_ns / 1_000_000;
        let start_ms = end_ms - BAR_WINDOW_MS;
        let query =
            format!("thscode={thscode}&interval=1d&start={start_ms}&end={end_ms}&adjust=none");
        let answer = match self.spaced(HISTORICAL_PATH, &query).await {
            Ok(answer) if answer.code == 0 => answer,
            Ok(answer) => {
                tracing::warn!(code = answer.code, thscode, "the HiThink bar was refused");
                return Err(rate_limited_or(answer.code, Reason::DateUnproven));
            }
            Err(e) => {
                tracing::warn!(thscode, "the HiThink bar did not arrive: {e}");
                return Err(match failure_reason(&e) {
                    Reason::RateLimited => Reason::RateLimited,
                    _ => Reason::DateUnproven,
                });
            }
        };
        match latest_bar_date(&answer.bytes) {
            Some(date) if date == today => {
                self.lock().bars.insert(entry.instrument_id, today);
                Ok(())
            }
            other => {
                tracing::warn!(thscode, bar = other, "the HiThink bar is not today's");
                Err(Reason::DateUnproven)
            }
        }
    }

    /// One upstream call under the installation-wide spacing gate (§4.1), which
    /// the passthrough and the bar read share.
    async fn spaced(&self, path: &str, query: &str) -> Result<Answer, Failure> {
        let mut gate = self.gate.lock().await;
        if let Some(wait) = gate.and_then(|last| SPACING.checked_sub(last.elapsed())) {
            tokio::time::sleep(wait).await;
        }
        let answer = self.request(path, query).await;
        *gate = Some(Instant::now());
        answer
    }
}

/// Whether an exhausted call's last reason was rate limiting — the two strings
/// [`Hithink::attempt`] writes for `HTTP 429` and the envelope codes.
///
/// ponytail: the reason travels as text because [`Failure`] carries no code.
/// The upgrade path is a typed last-reason on `Failure` if a second caller
/// needs to branch on it.
pub(crate) fn failure_reason(failure: &Failure) -> Reason {
    match failure {
        Failure::Unreachable { last, .. } if is_rate_limit(last) => Reason::RateLimited,
        _ => Reason::FeedLost,
    }
}

fn is_rate_limit(last: &str) -> bool {
    RATE_LIMITED
        .iter()
        .any(|code| last == format!("code {code}"))
        || last == "HTTP 429"
}

pub(crate) fn rate_limited_or(code: i64, otherwise: Reason) -> Reason {
    if RATE_LIMITED.contains(&code) {
        Reason::RateLimited
    } else {
        otherwise
    }
}

/// One attempt's outcome, before the retry table decides what it means.
enum Once {
    Done(Answer),
    /// Retryable: `4001`, `5001`–`5003`, HTTP 429 or 5xx, a transport error.
    Retry(String),
    /// Never retried: any other status or a body that is no JSON envelope.
    Fatal(String),
    TooLarge(usize),
}

/// The row's own fields, key-free by construction.
#[derive(Debug)]
struct Row {
    state: String,
    a_share_feed: AShareFeed,
    validated_at_ns: Option<i64>,
    failure_code: Option<String>,
    failure_message: Option<String>,
}

impl Row {
    fn unconfigured() -> Row {
        Row {
            state: "UNCONFIGURED".to_string(),
            a_share_feed: AShareFeed::Yahoo,
            validated_at_ns: None,
            failure_code: None,
            failure_message: None,
        }
    }
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        state: row.get(0)?,
        a_share_feed: AShareFeed::from_row(&row.get::<_, String>(1)?),
        validated_at_ns: row.get(2)?,
        failure_code: row.get(3)?,
        failure_message: row.get(4)?,
    })
}

/// The Shanghai calendar date of an instant, `yyyyMMdd` — the form HiThink's
/// list carries (§3).
pub(crate) fn shanghai_date(at_ns: i64) -> String {
    DateTime::from_timestamp_nanos(at_ns)
        .with_timezone(&Tz::Asia__Shanghai)
        .format("%Y%m%d")
        .to_string()
}

/// The Shanghai date of the newest `data.item[].date_ms` in a daily-bar answer
/// (`a-share-engine` SPEC §2.1). The bars carry milliseconds alone; the readable
/// `date` string is the calendar endpoint's, not this one's.
fn latest_bar_date(bytes: &[u8]) -> Option<String> {
    let body = serde_json::from_slice::<Value>(bytes).ok()?;
    let newest = body["data"]["item"]
        .as_array()?
        .iter()
        .filter_map(|item| item.get("date_ms")?.as_i64())
        .max()?;
    Some(shanghai_date(newest * 1_000_000))
}

/// `data.item[].date` from the trading-days answer (§3).
fn trading_days(bytes: &[u8]) -> BTreeSet<String> {
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return BTreeSet::new();
    };
    body["data"]["item"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("date")?.as_str().map(str::to_owned))
        .collect()
}

// ---------------------------------------------------------------------------
// provider::* (feature SPEC `hithink-a-share` §7)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod provider {
    use super::*;

    /// The upstream envelope shape (§1.2, §2.2): `{code, message, request_id, data}`.
    pub(crate) fn envelope(code: i64, data: &str) -> String {
        format!(
            r#"{{"code":{code},"message":"{}","request_id":"r-1","data":{data}}}"#,
            if code == 0 { "success" } else { "refused" }
        )
    }

    /// A [`Hithink`] on a scratch root, always on the file credential seam and
    /// always against a stand-in, so no test reaches this machine's keychain or
    /// the public service.
    pub(crate) fn scratch(base_url: &str) -> (tempfile::TempDir, Arc<Hithink>) {
        let dir = tempfile::tempdir().unwrap();
        let roots = crate::store::Roots::resolve(Some(dir.path())).unwrap();
        roots.create_dirs().unwrap();
        let store = Store::open(&roots.database()).unwrap();
        let memory = Arc::new(crate::memory::seam_memory(store.clone(), roots));
        let hithink = Hithink::standin(store, memory, base_url.to_string());
        (dir, hithink)
    }

    /// Every `HITHINK_PROVIDER_CHANGED` payload, oldest first.
    pub(crate) fn events(store: &Store) -> Vec<Value> {
        store
            .call(|c| {
                c.prepare(
                    "SELECT payload FROM operational_events WHERE kind = ?1 \
                     ORDER BY occurred_at_ns, id",
                )?
                .query_map([EVENT], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .iter()
            .map(|p| serde_json::from_str(p).unwrap())
            .collect()
    }

    const KEY: &str = "hithink-fake-0123456789abcdef";

    #[test]
    fn seam_only_with_data_root() {
        let root = Path::new("/scratch/marketrig");
        let stand_in = "http://127.0.0.1:52002";

        // The URL alone is inert: only the data root marks a test run (§1.3).
        assert_eq!(resolve(None, Some(stand_in)), (BASE_URL.to_string(), false));
        assert_eq!(resolve(Some(root), None), (BASE_URL.to_string(), false));
        assert_eq!(resolve(None, None), (BASE_URL.to_string(), false));
        // Both set: the stand-in, trailing slash trimmed.
        assert_eq!(
            resolve(Some(root), Some("http://127.0.0.1:52002/")),
            (stand_in.to_string(), true)
        );
        assert_eq!(TEST_HITHINK_URL_ENV, "MARKETRIG_TEST_HITHINK_URL");
        assert_eq!(BASE_URL, "https://fuyao.aicubes.cn");
    }

    #[tokio::test]
    async fn validate_with_one_bounded_request() {
        // A bad key: one request, `PROVIDER_REJECTED` carrying the upstream code
        // and message, and nothing written anywhere (§1.2).
        let (base, _hits, seen) = feed::scripted_server(vec![(200, envelope(2003, "null"))]);
        let (_dir, hithink) = scratch(&base);
        let error = hithink.put(KEY).await.unwrap_err();
        assert_eq!(error.code(), "PROVIDER_REJECTED");
        assert!(error.to_string().contains("2003"), "{error}");
        let requests = seen.lock().unwrap().clone();
        assert_eq!(
            requests,
            ["GET /api/meta/tickers/search?q=600519&limit=1"],
            "exactly one bounded request"
        );
        let row = hithink.provider().unwrap();
        assert_eq!(row.state, "UNCONFIGURED");
        assert!(!row.api_key_present);
        assert!(events(&hithink.store).is_empty());
        assert!(hithink.memory.load_secret(ACCOUNT).unwrap().is_none());

        // An empty key never reaches the wire.
        let (_dir, hithink) = scratch(&base);
        assert_eq!(
            hithink.put("   ").await.unwrap_err().code(),
            "VALIDATION",
            "an empty key is refused before the request"
        );

        // A good key: one request again, then the key, the row, and one event.
        let (base, _hits, seen) = feed::scripted_server(vec![(
            200,
            envelope(0, r#"{"item":[{"thscode":"600519.SH"}]}"#),
        )]);
        let (_dir, hithink) = scratch(&base);
        let provider = hithink.put(&format!("  {KEY}  ")).await.unwrap();
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "one request, not a probe loop"
        );
        assert_eq!(provider.state, "AVAILABLE");
        assert_eq!(provider.a_share_feed, AShareFeed::Hithink);
        assert!(provider.api_key_present);
        assert!(provider.validated_at_ns.is_some());
        assert_eq!(provider.base_url, base);
        assert_eq!(
            hithink.memory.load_secret(ACCOUNT).unwrap().as_deref(),
            Some(KEY),
            "the key is trimmed and stored in the credential store alone"
        );
        assert_eq!(
            events(&hithink.store),
            [json!({ "state": "AVAILABLE", "a_share_feed": "HITHINK" })]
        );

        // A transport failure is PROVIDER_UNREACHABLE, and it writes nothing.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let (_dir, hithink) = scratch(&dead);
        assert_eq!(
            hithink.put(KEY).await.unwrap_err().code(),
            "PROVIDER_UNREACHABLE"
        );
        assert_eq!(hithink.provider().unwrap().state, "UNCONFIGURED");
    }

    #[tokio::test]
    async fn toggle_requires_key() {
        let (base, _hits, _seen) = feed::scripted_server(vec![(200, envelope(0, "null"))]);
        let (_dir, hithink) = scratch(&base);

        // No key: the toggle is 409 and the row does not move (§1.2).
        assert_eq!(
            hithink.patch(AShareFeed::Hithink).unwrap_err().code(),
            "RESEARCH_UNCONFIGURED"
        );
        assert_eq!(hithink.a_share_feed(), AShareFeed::Yahoo);
        assert!(events(&hithink.store).is_empty());

        // A key: `PUT` chooses HiThink, and the toggle moves it either way,
        // appending an event only when the field changed.
        hithink.put(KEY).await.unwrap();
        assert_eq!(hithink.a_share_feed(), AShareFeed::Hithink);
        assert_eq!(
            hithink.patch(AShareFeed::Hithink).unwrap().a_share_feed,
            AShareFeed::Hithink
        );
        assert_eq!(
            events(&hithink.store).len(),
            1,
            "an unchanged PATCH is quiet"
        );
        let back = hithink.patch(AShareFeed::Yahoo).unwrap();
        assert_eq!(back.a_share_feed, AShareFeed::Yahoo);
        assert_eq!(hithink.a_share_feed(), AShareFeed::Yahoo);
        assert_eq!(
            events(&hithink.store),
            [
                json!({ "state": "AVAILABLE", "a_share_feed": "HITHINK" }),
                json!({ "state": "AVAILABLE", "a_share_feed": "YAHOO" }),
            ]
        );

        // `DELETE` removes the key, forces Yahoo, and is idempotent.
        let removed = hithink.delete().unwrap();
        assert_eq!(removed.state, "UNCONFIGURED");
        assert_eq!(removed.a_share_feed, AShareFeed::Yahoo);
        assert!(!removed.api_key_present);
        assert!(hithink.memory.load_secret(ACCOUNT).unwrap().is_none());
        assert_eq!(events(&hithink.store).len(), 3);
        assert_eq!(hithink.delete().unwrap().state, "UNCONFIGURED");
        assert_eq!(events(&hithink.store).len(), 3, "a second DELETE is quiet");
        assert_eq!(
            hithink.patch(AShareFeed::Hithink).unwrap_err().code(),
            "RESEARCH_UNCONFIGURED",
            "the toggle needs a key again"
        );
    }

    /// The key is in no answer, no event, and no row (§1.2, §6.2 H1).
    #[tokio::test]
    async fn key_never_answered() {
        let (base, _hits, _seen) = feed::scripted_server(vec![
            (200, envelope(0, "null")),
            (200, envelope(2003, "null")),
        ]);
        let (_dir, hithink) = scratch(&base);

        let mut answers = vec![serde_json::to_string(&hithink.provider().unwrap()).unwrap()];
        answers.push(serde_json::to_string(&hithink.put(KEY).await.unwrap()).unwrap());
        answers.push(serde_json::to_string(&hithink.patch(AShareFeed::Yahoo).unwrap()).unwrap());
        // A rejected key mid-run writes its own row and event too (§1.2).
        hithink.patch(AShareFeed::Hithink).unwrap();
        let answer = hithink.request(SNAPSHOT_PATH, "thscodes=600519.SH").await;
        assert_eq!(answer.unwrap().code, 2003);
        answers.push(serde_json::to_string(&hithink.provider().unwrap()).unwrap());
        answers.push(serde_json::to_string(&hithink.delete().unwrap()).unwrap());

        for answer in &answers {
            assert!(
                !answer.contains(KEY),
                "a route answer carries the key: {answer}"
            );
        }
        for payload in events(&hithink.store) {
            assert!(
                !payload.to_string().contains(KEY),
                "an event payload carries the key: {payload}"
            );
        }
        // And the whole row, column by column, is key-free.
        let row: String = hithink
            .store
            .call(|c| {
                c.query_row(
                    "SELECT group_concat(coalesce(state,'') || coalesce(a_share_feed,'') || \
                     coalesce(failure_code,'') || coalesce(failure_message,''), '|') \
                     FROM hithink_provider",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(!row.contains(KEY), "the row carries the key: {row}");
    }
}

// ---------------------------------------------------------------------------
// research::* (feature SPEC `hithink-a-share` §7)
// ---------------------------------------------------------------------------

/// The passthrough's own two checks (§4.1, per HT-4), on the same stand-in the
/// provider checks use: what each refusal answers, and the spacing gate.
#[cfg(test)]
mod research {
    use super::provider::{envelope, scratch};
    use super::*;

    const KEY: &str = "hithink-fake-0123456789abcdef";
    const PATH: &str = "meta/tickers/search";

    /// A provider that has validated, so the passthrough is past §4.1's first
    /// refusal: reply 0 is the one bounded validation request.
    async fn available(replies: Vec<(u16, String)>) -> (tempfile::TempDir, Arc<Hithink>, Seen) {
        let mut script = vec![(200, envelope(0, "null"))];
        script.extend(replies);
        let (base, _hits, seen) = feed::scripted_server(script);
        let (dir, hithink) = scratch(&base);
        hithink.put(KEY).await.unwrap();
        (dir, hithink, seen)
    }

    type Seen = std::sync::Arc<Mutex<Vec<String>>>;

    fn requests(seen: &Seen) -> Vec<String> {
        seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    #[tokio::test]
    async fn codes() {
        // 1. No key: the refusal comes before any allowlist lookup or request.
        let (base, _hits, seen) = feed::scripted_server(vec![(200, envelope(0, "null"))]);
        let (_dir, hithink) = scratch(&base);
        let error = hithink.research(PATH, "q=600519").await.unwrap_err();
        assert_eq!(error.code(), "RESEARCH_UNCONFIGURED");
        assert!(requests(&seen).is_empty(), "nothing reaches HiThink");

        // 2. A path the capability map does not document, with the key stored:
        // refused here, never forwarded.
        let (_dir, hithink, seen) = available(vec![(200, envelope(0, "null"))]).await;
        let error = hithink
            .research("meta/tickers/invent", "")
            .await
            .unwrap_err();
        assert_eq!(error.code(), "RESEARCH_PATH_UNKNOWN");
        assert!(error.to_string().contains("meta/tickers/invent"), "{error}");
        assert_eq!(
            requests(&seen).len(),
            1,
            "only the validation request was issued"
        );

        // 3. `4001` every time: three attempts, then the exhaustion refusal
        // naming the count (§2.2's bound, reused by §4.1).
        let (_dir, hithink, seen) = available(vec![(200, envelope(4001, "null"))]).await;
        let error = hithink.research(PATH, "q=600519").await.unwrap_err();
        assert_eq!(error.code(), "RESEARCH_UNREACHABLE");
        assert!(error.to_string().contains("3 attempts"), "{error}");
        assert_eq!(
            requests(&seen).len() - 1,
            ATTEMPTS as usize,
            "the retry bound is {ATTEMPTS} attempts"
        );

        // 4. One byte past the ceiling: refused on the declared length, before
        // the body is read.
        let huge = envelope(0, &format!("\"{}\"", "x".repeat(BODY_CAP)));
        assert!(huge.len() > BODY_CAP);
        let (_dir, hithink, _seen) = available(vec![(200, huge)]).await;
        let error = hithink.research(PATH, "q=600519").await.unwrap_err();
        assert_eq!(error.code(), "RESEARCH_TOO_LARGE");
        assert!(error.to_string().contains(&BODY_CAP.to_string()), "{error}");

        // 5. An upstream envelope with a nonzero `code` is HiThink's answer, not
        // MarketRig's failure: the bytes come back verbatim for the route to
        // hand on as `200`, and nothing about the row moves.
        let refused = envelope(1001, r#"{"item":[]}"#);
        let (_dir, hithink, _seen) = available(vec![(200, refused.clone())]).await;
        let body = hithink.research(PATH, "q=600519").await.unwrap();
        assert_eq!(String::from_utf8(body).unwrap(), refused);
        assert_eq!(hithink.provider().unwrap().state, "AVAILABLE");
        assert_eq!(
            super::provider::events(hithink.store()).len(),
            1,
            "the passthrough appends no event of its own"
        );
    }

    /// Back-to-back reads are spaced installation-wide, whatever the caller
    /// (§4.1): the gate holds the second one until 200 ms after the first
    /// upstream call finished.
    #[tokio::test]
    async fn spacing() {
        let (_dir, hithink, seen) = available(vec![(200, envelope(0, "null"))]).await;
        hithink.research(PATH, "q=600519").await.unwrap();
        let after_first = Instant::now();
        hithink.research(PATH, "q=600520").await.unwrap();
        assert!(
            after_first.elapsed() >= SPACING,
            "the second read reached HiThink {:?} after the first, under the {SPACING:?} gate",
            after_first.elapsed()
        );
        assert_eq!(
            requests(&seen).len(),
            3,
            "one validation and two research requests"
        );
        assert_eq!(SPACING, Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------
// readiness::* (feature SPEC `a-share-engine` §2.1)
// ---------------------------------------------------------------------------

/// The day's evidence: what authorizes CN execution, what merely labels a
/// phase, and what a refusal may and may not take away.
#[cfg(test)]
mod readiness {
    use super::provider::{envelope, scratch};
    use super::*;

    const KEY: &str = "hithink-fake-key-0123456789";

    fn at(y: i32, m: u32, d: u32, hour: u32) -> i64 {
        use chrono::TimeZone;

        Tz::Asia__Shanghai
            .with_ymd_and_hms(y, m, d, hour, 0, 0)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap()
    }

    /// The trading-day answer for a set of `yyyyMMdd` dates.
    fn days(dates: &[&str]) -> (u16, String) {
        let item = dates
            .iter()
            .map(|d| format!(r#"{{"date_ms":0,"date":"{d}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            envelope(0, &format!(r#"{{"timestamp":0,"item":[{item}]}}"#)),
        )
    }

    /// A daily-bar answer whose newest bar is dated `at_ns`'s Shanghai day,
    /// with one older bar behind it (F7 §2.2: `date_ms` and `close_price`).
    fn bars(at_ns: i64) -> (u16, String) {
        let newest = at_ns / 1_000_000;
        (
            200,
            envelope(
                0,
                &format!(
                    r#"{{"timestamp":{newest},"item":[
                       {{"date_ms":{},"close_price":1309.30}},
                       {{"date_ms":{newest},"close_price":1290.88}}]}}"#,
                    newest - 86_400_000
                ),
            ),
        )
    }

    /// A validated provider on `HITHINK`, over a stand-in running `replies`
    /// after the one validation reply `put` consumes.
    async fn standin(replies: Vec<(u16, String)>) -> (tempfile::TempDir, Arc<Hithink>, Requests) {
        let mut script = vec![(200, envelope(0, "null"))];
        script.extend(replies);
        let (base, _hits, seen) = feed::scripted_server(script);
        let (dir, hithink) = scratch(&base);
        hithink.put(KEY).await.unwrap();
        (dir, hithink, seen)
    }

    type Requests = Arc<Mutex<Vec<String>>>;

    /// The requests the stand-in saw after `put`'s validation call.
    fn asked(seen: &Requests) -> Vec<String> {
        seen.lock().unwrap_or_else(PoisonError::into_inner)[1..].to_vec()
    }

    /// The calendar dates the day, and only today's list authorizes execution:
    /// a refusal can never revoke a day that succeeded, the rollover always
    /// revokes, and the weekday fallback authorizes nothing at any point.
    #[tokio::test]
    async fn calendar_evidence_is_dated_and_a_refusal_never_revokes_today() {
        let wednesday = at(2026, 3, 4, 10);
        let thursday = at(2026, 3, 5, 10);
        let (_dir, hithink, seen) = standin(vec![
            days(&["20260304", "20260305"]),
            (429, envelope(429, "null")),
        ])
        .await;

        // 1. The day starts unavailable: the weekday rule may call the session
        // OPEN, but it is not evidence and cannot authorize execution.
        assert_eq!(
            hithink.cn_phase(wednesday),
            (Phase::Open, Calendar::Weekday)
        );
        assert_eq!(hithink.trading_day(wednesday), Err(Reason::NoCalendar));

        // 2. The day's one read is adopted, and it is what opens execution.
        assert_eq!(
            hithink.refresh_calendar_if_due(wednesday).await,
            CalendarRefresh::Adopted
        );
        assert_eq!(hithink.trading_day(wednesday), Ok(()));
        assert_eq!(
            hithink.cn_phase(wednesday),
            (Phase::Open, Calendar::Hithink)
        );

        // 3. Later the same Shanghai day, with the stand-in now refusing
        // everything: nothing is asked, so nothing can be revoked.
        for hour in [11, 13, 14] {
            assert_eq!(
                hithink.refresh_calendar_if_due(at(2026, 3, 4, hour)).await,
                CalendarRefresh::Held
            );
        }
        assert_eq!(asked(&seen).len(), 1, "one calendar read per Shanghai day");
        assert_eq!(hithink.trading_day(at(2026, 3, 4, 14)), Ok(()));

        // 4. The rollover revokes it before anything is asked, and the refused
        // refresh leaves the new day unavailable — never yesterday's list.
        assert_eq!(
            hithink.refresh_calendar_if_due(thursday).await,
            CalendarRefresh::Refused(Reason::RateLimited),
            "envelope 429 is rate limiting, and it exhausts the bound"
        );
        assert_eq!(asked(&seen).len(), 1 + ATTEMPTS as usize);
        assert_eq!(hithink.trading_day(thursday), Err(Reason::CalendarRefused));
        assert_eq!(
            hithink.cn_phase(thursday),
            (Phase::Open, Calendar::Weekday),
            "a past day's list labels nothing, and the fallback is awareness only"
        );
    }

    /// A successful list always replaces what is held, and a list without today
    /// is `NOT_TRADING_DAY` rather than an absence of evidence.
    #[tokio::test]
    async fn a_contradictory_list_replaces_and_a_holiday_blocks() {
        let wednesday = at(2026, 3, 4, 10);
        let thursday = at(2026, 3, 5, 10);
        let (_dir, hithink, _seen) =
            standin(vec![days(&["20260304", "20260305"]), days(&["20260306"])]).await;

        hithink.refresh_calendar_if_due(wednesday).await;
        assert_eq!(hithink.trading_day(wednesday), Ok(()));
        // The next day's read contradicts it — Thursday is no longer a trading
        // day — and the newer answer wins.
        assert_eq!(
            hithink.refresh_calendar_if_due(thursday).await,
            CalendarRefresh::Adopted
        );
        assert_eq!(hithink.trading_day(thursday), Err(Reason::NotTradingDay));
        assert_eq!(
            hithink.cn_phase(thursday),
            (Phase::Closed, Calendar::Hithink)
        );
    }

    /// The calendar is the provider's answer, not the feed's: an available
    /// provider on the Yahoo feed still fetches it, because Yahoo CN execution
    /// needs the same confirmed trading day (`a-share-engine` SPEC §2.1, AE-7).
    /// Only the *phase* label stays R1's weekday rule under Yahoo (§3).
    #[tokio::test]
    async fn the_yahoo_feed_still_confirms_the_trading_day() {
        let wednesday = at(2026, 3, 4, 10);
        let (_dir, hithink, seen) = standin(vec![days(&["20260304"])]).await;
        hithink.patch(AShareFeed::Yahoo).unwrap();
        assert_eq!(
            hithink.refresh_calendar_if_due(wednesday).await,
            CalendarRefresh::Adopted
        );
        assert_eq!(asked(&seen).len(), 1);
        assert_eq!(hithink.trading_day(wednesday), Ok(()));
        assert_eq!(
            hithink.cn_phase(wednesday),
            (Phase::Open, Calendar::Weekday),
            "the phase label is awareness and stays WEEKDAY under Yahoo"
        );
    }

    /// With no provider row available at all there is nothing to ask, and CN
    /// execution stays `NO_CALENDAR` under either feed (§2.1, AE-7).
    #[tokio::test]
    async fn no_provider_means_no_calendar() {
        let wednesday = at(2026, 3, 4, 10);
        let (_dir, hithink, seen) = standin(vec![days(&["20260304"])]).await;
        hithink.delete().unwrap();
        assert_eq!(
            hithink.refresh_calendar_if_due(wednesday).await,
            CalendarRefresh::NotDue
        );
        assert!(asked(&seen).is_empty());
        assert_eq!(hithink.trading_day(wednesday), Err(Reason::NoCalendar));
    }

    /// One successful current-day bar per instrument per Shanghai day: held for
    /// the day, refetched after the rollover, and retried on the poll cadence
    /// while it fails.
    #[tokio::test]
    async fn bar_evidence_is_read_once_a_day_and_retried_while_it_fails() {
        let wednesday = at(2026, 3, 4, 10);
        let thursday = at(2026, 3, 5, 10);
        let entry = crate::catalog::find("600519.XSHG").unwrap();
        let other = crate::catalog::find("300750.XSHE").unwrap();
        let refusal = (200, envelope(4001, "null"));
        let (_dir, hithink, seen) = standin(vec![
            bars(wednesday),
            bars(wednesday),
            // Thursday's first call refuses on all three attempts; the call
            // after it succeeds.
            refusal.clone(),
            refusal.clone(),
            refusal,
            bars(thursday),
        ])
        .await;

        // The request is F7's shape: one thscode, daily, unadjusted.
        assert_eq!(hithink.bar_evidence(entry, wednesday).await, Ok(()));
        let first = asked(&seen);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].starts_with(
                "GET /api/a-share/prices/historical?thscode=600519.SH&interval=1d&start="
            ) && first[0].ends_with("&adjust=none"),
            "{first:?}"
        );

        // Held for the day: the same instrument asks nothing more, while a
        // second instrument has its own evidence to establish.
        assert_eq!(
            hithink.bar_evidence(entry, at(2026, 3, 4, 14)).await,
            Ok(())
        );
        assert_eq!(asked(&seen).len(), 1, "one bar read per instrument per day");
        assert_eq!(hithink.bar_evidence(other, wednesday).await, Ok(()));
        assert_eq!(asked(&seen).len(), 2);

        // The rollover clears it. The refused read answers for this call alone
        // — `4001` exhausts the bound as rate limiting — and the next call
        // retries and succeeds.
        assert_eq!(
            hithink.bar_evidence(entry, thursday).await,
            Err(Reason::RateLimited)
        );
        assert_eq!(asked(&seen).len(), 2 + ATTEMPTS as usize);
        assert_eq!(hithink.bar_evidence(entry, thursday).await, Ok(()));
    }

    /// A bar that is not the current day's proves nothing: `DATE_UNPROVEN`,
    /// and nothing is held.
    #[tokio::test]
    async fn a_stale_bar_is_not_evidence() {
        let wednesday = at(2026, 3, 4, 10);
        let entry = crate::catalog::find("600519.XSHG").unwrap();
        let (_dir, hithink, _seen) = standin(vec![bars(at(2026, 3, 3, 10)), bars(wednesday)]).await;
        assert_eq!(
            hithink.bar_evidence(entry, wednesday).await,
            Err(Reason::DateUnproven)
        );
        assert_eq!(hithink.bar_evidence(entry, wednesday).await, Ok(()));
    }
}
