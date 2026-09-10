//! F8 item 5, provider half — owned by one agent; see README.md.
//!
//! Two questions, both against the loopback stand-in (`feed::scripted_server`),
//! never the real service:
//!
//! A. Do HTTP 429 and envelope-only 429 (HTTP 200, body `code: 429`) both take
//!    `hithink::Hithink::fetch`'s bounded retry path (3 attempts, 500 ms then
//!    1 s), for the snapshot, the historical bar and the calendar?
//! B. Does a refused redundant calendar read revoke today's successful calendar
//!    evidence?
//!
//! Every test below asserts **today's** behaviour, not the behaviour
//! `sdd/features/a-share-engine/SPEC.md` §2.1 asks for. Where the two differ the
//! test name says so.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::TimeZone;
use chrono_tz::Tz;

use crate::feed::{Phase, scripted_server};
use crate::hithink::provider::{envelope, scratch};
use crate::hithink::{ATTEMPTS, Calendar, Hithink, SNAPSHOT_PATH};

/// The bar read the readiness rule needs. It is not one of the three paths the
/// daemon calls itself; it reaches HiThink through the research passthrough,
/// which shares `request` with the other two.
const HISTORICAL_PATH: &str = "a-share/prices/historical";
const SNAPSHOT_QUERY: &str = "thscodes=600519.SH";
const HISTORICAL_QUERY: &str = "thscode=600519.SH&interval=1d&adjust=none";

/// 500 ms + 1 s, less scheduling slack. `BACKOFF` is a private const in
/// `hithink.rs` with no injection seam, so this is measured, not driven.
const BACKOFF_FLOOR: Duration = Duration::from_millis(1_400);

/// What one bounded call ended as: the envelope `code` it returned, or the
/// attempt count in its `Failure::Unreachable`.
type Outcome = Result<i64, u32>;

/// The stand-in reply the calendar adopts: one trading day, `date` = `at_ns`'s
/// Shanghai date.
fn days(at_ns: i64) -> String {
    let date = chrono::DateTime::from_timestamp_nanos(at_ns)
        .with_timezone(&Tz::Asia__Shanghai)
        .format("%Y%m%d")
        .to_string();
    envelope(
        0,
        &format!(r#"{{"timestamp":0,"item":[{{"date_ms":0,"date":"{date}"}}]}}"#),
    )
}

fn at(y: i32, m: u32, d: u32, hour: u32) -> i64 {
    Tz::Asia__Shanghai
        .with_ymd_and_hms(y, m, d, hour, 0, 0)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap()
}

/// A validated provider on `HITHINK` over a stand-in running `replies` after the
/// one validation reply `put` consumes. Returns the request log the stand-in
/// keeps, so every assertion below counts what the server actually saw.
async fn standin(
    replies: Vec<(u16, String)>,
) -> (
    tempfile::TempDir,
    Arc<Hithink>,
    Arc<std::sync::Mutex<Vec<String>>>,
) {
    let mut script = vec![(200, envelope(0, "null"))];
    script.extend(replies);
    let (base, _hits, seen) = scripted_server(script);
    let (dir, hithink) = scratch(&base);
    hithink.put("hithink-fake-key-0123456789").await.unwrap();
    (dir, hithink, seen)
}

/// Requests the stand-in saw after `put`'s validation call.
fn after_validation(seen: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    let seen = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    seen[1..].to_vec()
}

fn code_of(bytes: &[u8]) -> i64 {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("code").and_then(serde_json::Value::as_i64))
        .unwrap_or(-1)
}

/// `feed::poll_hithink`'s own request, driven directly so the outcome is the
/// bounded call's and not the poll's degrade-all wrapper.
async fn snapshot(replies: Vec<(u16, String)>) -> (usize, Outcome, Duration) {
    let (_dir, hithink, seen) = standin(replies).await;
    let started = Instant::now();
    let answer = hithink.request(SNAPSHOT_PATH, SNAPSHOT_QUERY).await;
    let elapsed = started.elapsed();
    let requests = after_validation(&seen);
    assert!(
        requests.iter().all(|r| r.contains(SNAPSHOT_PATH)),
        "{requests:?}"
    );
    eprintln!("snapshot: {} attempts in {elapsed:?}", requests.len());
    (
        requests.len(),
        match answer {
            Ok(answer) => Ok(answer.code),
            Err(crate::hithink::Failure::Unreachable { attempts, .. }) => Err(attempts),
            Err(other) => panic!("unexpected failure {other}"),
        },
        elapsed,
    )
}

async fn historical(replies: Vec<(u16, String)>) -> (usize, Outcome, Duration) {
    let (_dir, hithink, seen) = standin(replies).await;
    let started = Instant::now();
    let answer = hithink.research(HISTORICAL_PATH, HISTORICAL_QUERY).await;
    let elapsed = started.elapsed();
    let requests = after_validation(&seen);
    assert!(
        requests.iter().all(|r| r.contains(HISTORICAL_PATH)),
        "{requests:?}"
    );
    eprintln!("historical: {} attempts in {elapsed:?}", requests.len());
    (
        requests.len(),
        match answer {
            Ok(bytes) => Ok(code_of(&bytes)),
            Err(crate::hithink::HithinkError::ResearchUnreachable { attempts, .. }) => {
                Err(attempts)
            }
            Err(other) => panic!("unexpected failure {other}"),
        },
        elapsed,
    )
}

/// The calendar reports no code: `refresh_calendar_if_due` swallows every
/// outcome. Its only observable is whether the list was adopted, which
/// `cn_phase` names.
async fn calendar(replies: Vec<(u16, String)>, at_ns: i64) -> (usize, bool, Duration) {
    let (_dir, hithink, seen) = standin(replies).await;
    let started = Instant::now();
    hithink.refresh_calendar_if_due(at_ns).await;
    let elapsed = started.elapsed();
    let requests = after_validation(&seen);
    assert!(
        requests
            .iter()
            .all(|r| r.contains("a-share/calendar/trading-days")),
        "{requests:?}"
    );
    eprintln!("calendar: {} attempts in {elapsed:?}", requests.len());
    let adopted = hithink.cn_phase(at_ns).1 == Calendar::Hithink;
    (requests.len(), adopted, elapsed)
}

// ---------------------------------------------------------------------------
// A — the bounded retry path
// ---------------------------------------------------------------------------

/// (i) HTTP 429 twice, then a good body: three attempts, 500 ms + 1 s apart,
/// on all three endpoints (`hithink.rs:469`, before the body is parsed).
#[tokio::test]
async fn http_429_twice_then_200_retries_on_all_three_endpoints() {
    let refusal = (429, envelope(429, "null"));
    let good = (200, envelope(0, r#"{"item":[]}"#));

    let (n, outcome, elapsed) =
        snapshot(vec![refusal.clone(), refusal.clone(), good.clone()]).await;
    assert_eq!((n, outcome), (3, Ok(0)), "snapshot");
    assert!(elapsed >= BACKOFF_FLOOR, "snapshot backoff {elapsed:?}");

    let (n, outcome, elapsed) =
        historical(vec![refusal.clone(), refusal.clone(), good.clone()]).await;
    assert_eq!((n, outcome), (3, Ok(0)), "historical bar");
    assert!(elapsed >= BACKOFF_FLOOR, "historical backoff {elapsed:?}");

    let wednesday = at(2026, 3, 4, 10);
    let (n, adopted, elapsed) = calendar(
        vec![refusal.clone(), refusal, (200, days(wednesday))],
        wednesday,
    )
    .await;
    assert_eq!((n, adopted), (3, true), "calendar");
    assert!(elapsed >= BACKOFF_FLOOR, "calendar backoff {elapsed:?}");
}

/// (ii) HTTP 200 with envelope `code: 429`: **not** retried today. One attempt,
/// the refusal handed straight back to the caller.
///
/// This is the FAIL against `sdd/features/a-share-engine/SPEC.md` §2.1 ("Back
/// off on HTTP 429 or envelope 429/4001 using the existing three-attempt
/// bound"). `RATE_LIMITED` is `4001` (`hithink.rs:47`) and `RETRYABLE_SERVER` is
/// `5001..=5003` (`hithink.rs:49`); `429` is in neither, so `attempt`
/// (`hithink.rs:488`) returns `Once::Done`.
#[tokio::test]
async fn envelope_only_429_is_not_retried_today() {
    let refusal = (200, envelope(429, "null"));
    let good = (200, envelope(0, r#"{"item":[]}"#));

    let (n, outcome, elapsed) =
        snapshot(vec![refusal.clone(), refusal.clone(), good.clone()]).await;
    assert_eq!(
        (n, outcome),
        (1, Ok(429)),
        "the envelope-only 429 is a delivered answer, not a retry"
    );
    assert!(
        elapsed < Duration::from_millis(400),
        "no backoff {elapsed:?}"
    );

    let (n, outcome, _) = historical(vec![refusal.clone(), refusal.clone(), good]).await;
    assert_eq!(
        (n, outcome),
        (1, Ok(429)),
        "the passthrough hands the refusal to the agent verbatim"
    );

    // The calendar's own `code != 0` arm logs and returns, so one refusal
    // leaves the day with no list at all and the WEEKDAY fallback standing.
    let wednesday = at(2026, 3, 4, 10);
    let (n, adopted, _) = calendar(
        vec![refusal.clone(), refusal, (200, days(wednesday))],
        wednesday,
    )
    .await;
    assert_eq!((n, adopted), (1, false), "calendar");
}

/// (iii) Envelope `4001` twice, then a good body: the documented retryable
/// envelope code, three attempts, same shape and same terminal outcome as (i).
#[tokio::test]
async fn envelope_4001_twice_then_good_retries_on_all_three_endpoints() {
    let refusal = (200, envelope(4001, "null"));
    let good = (200, envelope(0, r#"{"item":[]}"#));

    let (n, outcome, elapsed) =
        snapshot(vec![refusal.clone(), refusal.clone(), good.clone()]).await;
    assert_eq!((n, outcome), (3, Ok(0)), "snapshot");
    assert!(elapsed >= BACKOFF_FLOOR, "snapshot backoff {elapsed:?}");

    let (n, outcome, _) = historical(vec![refusal.clone(), refusal.clone(), good]).await;
    assert_eq!((n, outcome), (3, Ok(0)), "historical bar");

    let wednesday = at(2026, 3, 4, 10);
    let (n, adopted, _) = calendar(
        vec![refusal.clone(), refusal, (200, days(wednesday))],
        wednesday,
    )
    .await;
    assert_eq!((n, adopted), (3, true), "calendar");
}

/// (iv) Three refusals: the bound holds at `ATTEMPTS`, the wait is 500 ms + 1 s,
/// and each endpoint ends in its own mapping of the same `Failure::Unreachable`.
#[tokio::test]
async fn three_refusals_exhaust_the_bound_on_all_three_endpoints() {
    let refusal = (429, envelope(429, "null"));
    assert_eq!(ATTEMPTS, 3);

    let (n, outcome, elapsed) = snapshot(vec![refusal.clone()]).await;
    assert_eq!((n, outcome), (3, Err(ATTEMPTS)), "snapshot");
    assert!(elapsed >= BACKOFF_FLOOR, "snapshot backoff {elapsed:?}");

    let (n, outcome, elapsed) = historical(vec![refusal.clone()]).await;
    assert_eq!(
        (n, outcome),
        (3, Err(ATTEMPTS)),
        "historical bar; RESEARCH_UNREACHABLE"
    );
    assert!(elapsed >= BACKOFF_FLOOR, "historical backoff {elapsed:?}");

    // Exhaustion and one refusal are indistinguishable to the calendar: both
    // just leave the list unset.
    let wednesday = at(2026, 3, 4, 10);
    let (n, adopted, elapsed) = calendar(vec![refusal], wednesday).await;
    assert_eq!((n, adopted), (3, false), "calendar");
    assert!(elapsed >= BACKOFF_FLOOR, "calendar backoff {elapsed:?}");
}

// ---------------------------------------------------------------------------
// B — what a refused redundant calendar read does to today's evidence
// ---------------------------------------------------------------------------

/// Today's whole calendar story, in one pass.
///
/// 1. With no list, a session hour is `(OPEN, WEEKDAY)` — there is no
///    start-unavailable readiness state (`hithink.rs:678`).
/// 2. A positive list is adopted for the Shanghai day.
/// 3. Within that day no redundant read is issued at all, so no refusal can
///    revoke it: `refresh_calendar_if_due` is not due (`hithink.rs:694`).
/// 4. Across the rollover the refusal keeps the **stale** set instead, which is
///    the opposite failure from the one §2.1 guards against.
/// 5. Nothing CN-execution-relevant flips either way, because today no CN
///    execution path consults the calendar: `cn_phase`'s only callers are
///    `feed.rs:537 phase_of` (the awareness `market_phase`/`calendar` fields)
///    and `node.rs:812 poll_cn` (the poll cadence).
#[tokio::test]
async fn a_refusal_cannot_revoke_todays_calendar_because_no_second_read_is_issued() {
    let wednesday = at(2026, 3, 4, 10);
    let thursday = at(2026, 3, 5, 10);
    let (_dir, hithink, seen) =
        standin(vec![(200, days(wednesday)), (429, envelope(429, "null"))]).await;

    // 1. No list yet: the weekday rule reports the session OPEN.
    assert_eq!(
        hithink.cn_phase(wednesday),
        (Phase::Open, Calendar::Weekday),
        "no calendar evidence still reads OPEN (§2.1 wants UNAVAILABLE/NO_CALENDAR)"
    );

    // 2. The day's one read succeeds and is adopted.
    hithink.refresh_calendar_if_due(wednesday).await;
    assert_eq!(after_validation(&seen).len(), 1);
    assert_eq!(
        hithink.cn_phase(wednesday),
        (Phase::Open, Calendar::Hithink)
    );

    // 3. Later the same Shanghai day, with the stand-in now refusing every
    //    request: no request is issued, so the evidence cannot be revoked.
    for hour in [11, 13, 14] {
        hithink.refresh_calendar_if_due(at(2026, 3, 4, hour)).await;
    }
    assert_eq!(
        after_validation(&seen).len(),
        1,
        "one calendar read per Shanghai day, refusal or not"
    );
    assert_eq!(
        hithink.cn_phase(at(2026, 3, 4, 14)),
        (Phase::Open, Calendar::Hithink),
        "today's evidence stands"
    );
    assert!(hithink.feed_ready(), "a 429 never touches the provider row");

    // 4. Rollover: now it is due, the refusal exhausts, and yesterday's set is
    //    kept and still consulted — retention with no expiry.
    hithink.refresh_calendar_if_due(thursday).await;
    assert_eq!(after_validation(&seen).len(), 1 + ATTEMPTS as usize);
    assert_eq!(
        hithink.cn_phase(thursday),
        (Phase::Closed, Calendar::Hithink),
        "the stale set labels the new day, and is named HITHINK while being yesterday's"
    );
}
