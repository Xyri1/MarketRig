//! The equity feed. This chunk lands the market calendars and the phase derived
//! from them; the polling chart client and the market state follow.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §2.2, per R1-3.

use chrono::{DateTime, Datelike, Timelike, Weekday};
use chrono_tz::Tz;
use serde::Serialize;

use crate::catalog::Market;

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
