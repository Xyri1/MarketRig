//! Schedules, their projections, and the one scheduler task.
//!
//! Contract: `sdd/features/r2-scheduled-triggers/SPEC.md` §2 and §3, per R2-1
//! and R2-2. Recurrence is computed on the wall clock in a fixed UTC frame and
//! resolved through the named zone candidate by candidate; the recurrence crate
//! never sees the zone.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, MappedLocalTime, NaiveDateTime, TimeZone, Timelike};
use rrule::{Frequency, RRule, RRuleSet, Unvalidated};
use rusqlite::{ErrorCode, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

use crate::store::{Store, now_ns};
use crate::trigger::{FiringRow, insert_result_prompt};

/// `dtstart` is naive local wall clock (§2).
const DTSTART_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";
/// Candidates examined per projection or count (§2).
const SCAN_LIMIT: usize = 100_000;
/// The miss record's candidate count is reported capped (§3.2).
const COUNT_CAP: u32 = 10_000;
/// Acceptance is at most this late (§3.2); anything older is a miss.
const LATE_BOUND_NS: i64 = 60_000_000_000;
/// The recheck that keeps a clock change from leaving an obsolete long sleep.
const RECHECK: Duration = Duration::from_secs(60);

/// One of the two schedule shapes (§2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// An absolute UTC instant, consumed by its first accepted firing.
    OneOff { at_ns: i64 },
    /// One RRULE value, a naive local `dtstart`, and an IANA zone.
    Recurring {
        rrule: String,
        dtstart: String,
        tz: String,
    },
}

impl Schedule {
    /// The two JSON shapes and every §2 rejection. `Err` is the English message
    /// the route reports as `TRIGGER_INVALID`.
    pub fn parse(value: &Value, now_ns: i64) -> Result<Schedule, String> {
        let object = value
            .as_object()
            .ok_or("A schedule must be a JSON object.".to_string())?;
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let text = |key: &str| {
            object[key]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("`{key}` must be a string."))
        };
        match keys.as_slice() {
            ["at"] => {
                let at = text("at")?;
                let at_ns = DateTime::parse_from_rfc3339(&at)
                    .map_err(|e| format!("`at` must be an RFC 3339 instant with an offset: {e}."))?
                    .timestamp_nanos_opt()
                    .ok_or("`at` is outside the representable range.".to_string())?;
                if at_ns <= now_ns {
                    return Err("`at` must be strictly in the future.".to_string());
                }
                Ok(Schedule::OneOff { at_ns })
            }
            ["dtstart", "rrule", "tz"] => {
                let (rrule, dtstart, tz) = (text("rrule")?, text("dtstart")?, text("tz")?);
                reject_rule_text(&rrule)?;
                tz.parse::<chrono_tz::Tz>()
                    .map_err(|_| format!("`tz` is not an IANA time-zone name: {tz}."))?;
                let naive =
                    NaiveDateTime::parse_from_str(&dtstart, DTSTART_FORMAT).map_err(|e| {
                        format!(
                            "`dtstart` must be naive local wall clock YYYY-MM-DDTHH:MM:SS: {e}."
                        )
                    })?;
                let anchor = utc_anchor(naive)
                    .ok_or("`dtstart` is outside the representable range.".to_string())?;
                let rule = rrule
                    .parse::<RRule<Unvalidated>>()
                    .map_err(|e| format!("`rrule` is not a recurrence rule: {e}."))?;
                if rule.get_freq() == Frequency::Secondly {
                    return Err("FREQ=SECONDLY is not supported.".to_string());
                }
                if rule.get_count().is_some() {
                    return Err(
                        "COUNT is not supported; a recurring rule is unbounded.".to_string()
                    );
                }
                if rule.get_until().is_some() {
                    return Err(
                        "UNTIL is not supported; a recurring rule is unbounded.".to_string()
                    );
                }
                if !rule.get_by_second().is_empty() {
                    return Err("BYSECOND is not supported.".to_string());
                }
                rule.validate(anchor)
                    .map_err(|e| format!("`rrule` does not validate against `dtstart`: {e}."))?;
                Ok(Schedule::Recurring { rrule, dtstart, tz })
            }
            _ => Err(
                "A schedule is either {\"at\"} or {\"rrule\",\"dtstart\",\"tz\"}, \
                 with no other keys."
                    .to_string(),
            ),
        }
    }

    /// The schedule a `triggers` row carries (§7); the row's CHECKs make the
    /// columns of the named recurrence present.
    pub fn from_row(
        recurrence: &str,
        at_ns: Option<i64>,
        rrule: Option<String>,
        dtstart: Option<String>,
        tz: Option<String>,
    ) -> Schedule {
        if recurrence == "ONE_OFF" {
            Schedule::OneOff {
                at_ns: at_ns.unwrap_or_default(),
            }
        } else {
            Schedule::Recurring {
                rrule: rrule.unwrap_or_default(),
                dtstart: dtstart.unwrap_or_default(),
                tz: tz.unwrap_or_default(),
            }
        }
    }

    /// The `triggers.recurrence` value.
    pub fn recurrence(&self) -> &'static str {
        match self {
            Schedule::OneOff { .. } => "ONE_OFF",
            Schedule::Recurring { .. } => "RECURRING",
        }
    }

    /// The resource's `schedule` object (§8). A one-off reports only its
    /// resolved instant.
    pub fn to_json(&self) -> Value {
        match self {
            Schedule::OneOff { at_ns } => json!({ "at_ns": at_ns }),
            Schedule::Recurring { rrule, dtstart, tz } => {
                json!({ "rrule": rrule, "dtstart": dtstart, "tz": tz })
            }
        }
    }

    /// The projection (§2): the first candidate strictly after `reference_ns`,
    /// always computed from the definition's own anchor. `None` means never
    /// due again — a consumed one-off, or a rule with no candidate inside the
    /// scan bound.
    pub fn next_after(&self, reference_ns: i64) -> Option<i64> {
        match self {
            Schedule::OneOff { at_ns } => (*at_ns > reference_ns).then_some(*at_ns),
            Schedule::Recurring { rrule, dtstart, tz } => {
                candidates(rrule, dtstart, tz)?.find(|c| *c > reference_ns)
            }
        }
    }

    /// The candidates in `[from_ns, through_ns]`, inclusive, and whether the
    /// count was capped (§3.2).
    pub fn count_between(&self, from_ns: i64, through_ns: i64) -> (u32, bool) {
        match self {
            Schedule::OneOff { at_ns } => ((from_ns..=through_ns).contains(at_ns).into(), false),
            Schedule::Recurring { rrule, dtstart, tz } => {
                let Some(candidates) = candidates(rrule, dtstart, tz) else {
                    return (0, false);
                };
                let mut count = 0u32;
                let mut scanned = 0usize;
                for candidate in candidates {
                    scanned += 1;
                    if candidate < from_ns {
                        continue;
                    }
                    if candidate > through_ns {
                        return (count, false);
                    }
                    if count == COUNT_CAP {
                        return (COUNT_CAP, true);
                    }
                    count += 1;
                }
                // The walk ended before the range did: when the scan bound ended
                // it, the count is a floor, reported as capped.
                (count, scanned >= SCAN_LIMIT)
            }
        }
    }
}

/// The rule text rejections that run before parsing (§2), case-insensitive.
fn reject_rule_text(rrule: &str) -> Result<(), String> {
    if rrule.contains('\n') || rrule.contains('\r') {
        return Err("`rrule` is one rule on one line.".to_string());
    }
    let upper = rrule.to_ascii_uppercase();
    for token in ["RRULE:", "DTSTART", "RDATE", "EXDATE", "EXRULE"] {
        if upper.contains(token) {
            return Err(format!(
                "`rrule` is the rule value alone; {token} is not part of it."
            ));
        }
    }
    Ok(())
}

/// `dtstart`'s naive fields stamped `Tz::UTC` — the fixed frame the rule
/// iterates in (R2-1).
fn utc_anchor(naive: NaiveDateTime) -> Option<DateTime<rrule::Tz>> {
    rrule::Tz::UTC
        .with_ymd_and_hms(
            naive.year(),
            naive.month(),
            naive.day(),
            naive.hour(),
            naive.minute(),
            naive.second(),
        )
        .single()
}

/// Every candidate instant of a recurring definition, in order, bounded by the
/// scan limit: the rule iterates on wall clock, and each wall clock is resolved
/// in the zone — a gap is skipped, an overlap takes its earlier instant (§2).
fn candidates(rrule: &str, dtstart: &str, tz: &str) -> Option<impl Iterator<Item = i64>> {
    let zone = tz.parse::<chrono_tz::Tz>().ok()?;
    let anchor = utc_anchor(NaiveDateTime::parse_from_str(dtstart, DTSTART_FORMAT).ok()?)?;
    let rule = rrule
        .parse::<RRule<Unvalidated>>()
        .ok()?
        .validate(anchor)
        .ok()?;
    // `limit()` arms the crate's own 100,000-iteration guard, so a rule that
    // can never match (`BYMONTH=2;BYMONTHDAY=30`) ends instead of walking the
    // whole year range; `take` bounds what is yielded.
    let set = RRuleSet::new(anchor).rrule(rule).limit();
    Some(set.into_iter().take(SCAN_LIMIT).filter_map(move |wall| {
        match zone.from_local_datetime(&wall.naive_utc()) {
            MappedLocalTime::None => None,
            MappedLocalTime::Ambiguous(earlier, _) => earlier.timestamp_nanos_opt(),
            MappedLocalTime::Single(instant) => instant.timestamp_nanos_opt(),
        }
    }))
}

/// What one acceptance pass did (§3.2).
#[derive(Debug, Default)]
pub struct Pass {
    pub accepted: Vec<Accepted>,
    pub missed: u32,
}

/// One accepted firing, enough for the pass to wake the executor.
#[derive(Debug)]
pub struct Accepted {
    pub firing_id: String,
    pub desk_id: String,
    pub has_code: bool,
}

/// The acceptance unit (§3.2), runnable inside any transaction. `now_ns` is
/// read once by the caller: nothing here reads wall time.
pub fn accept_or_miss(
    tx: &Transaction<'_>,
    now_ns: i64,
    started_at_ns: i64,
) -> rusqlite::Result<Pass> {
    let due: Vec<Due> = tx
        .prepare(
            // The snapshot's state joins in so the advance can go through the one
            // projection rule (R5 feature SPEC §3.2). A due row is eligible by
            // construction — only that rule ever writes a projection — so this
            // is the rule living in one place, not a second gate.
            "SELECT t.id, t.desk_id, t.name, t.recurrence, t.brief, t.context, t.at_ns, \
             t.rrule, t.dtstart, t.tz, t.revision, t.code_snapshot_id, \
             t.next_occurrence_ns, c.approval \
             FROM triggers t LEFT JOIN code_snapshots c ON c.id = t.code_snapshot_id \
             WHERE t.deleted_at_ns IS NULL AND t.enabled = 1 \
             AND t.next_occurrence_ns IS NOT NULL AND t.next_occurrence_ns <= ?1 \
             ORDER BY t.next_occurrence_ns, t.id",
        )?
        .query_map(params![now_ns], |r| {
            Ok(Due {
                id: r.get(0)?,
                desk_id: r.get(1)?,
                name: r.get(2)?,
                schedule: Schedule::from_row(
                    &r.get::<_, String>(3)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ),
                brief: r.get(4)?,
                context: r.get(5)?,
                revision: r.get(10)?,
                code_snapshot_id: r.get(11)?,
                occurrence_ns: r.get(12)?,
                approval: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut pass = Pass::default();
    for trigger in due {
        let one_off = matches!(trigger.schedule, Schedule::OneOff { .. });
        if trigger.occurrence_ns >= started_at_ns
            && now_ns.saturating_sub(trigger.occurrence_ns) <= LATE_BOUND_NS
        {
            // The advance is the same after a first acceptance and after
            // losing the race to one: computed here, before the row's fields
            // move into the firing.
            let next = advance(&trigger, one_off, trigger.occurrence_ns);
            let firing = FiringRow {
                id: Uuid::now_v7().to_string(),
                desk_id: trigger.desk_id.clone(),
                trigger_id: trigger.id.clone(),
                occurrence_ns: trigger.occurrence_ns,
                accepted_at_ns: now_ns,
                trigger_revision: trigger.revision,
                brief: trigger.brief,
                context: trigger.context,
                code_snapshot_id: trigger.code_snapshot_id,
            };
            match tx.execute(
                "INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, context, code_snapshot_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    firing.id,
                    firing.desk_id,
                    firing.trigger_id,
                    firing.occurrence_ns,
                    firing.accepted_at_ns,
                    firing.trigger_revision,
                    firing.brief,
                    firing.context,
                    firing.code_snapshot_id,
                ],
            ) {
                Ok(_) => {}
                // Another wake already accepted this occurrence: advance the
                // projection exactly as that acceptance did, so the row never
                // stays due, and carry on (§3.2).
                Err(rusqlite::Error::SqliteFailure(f, _))
                    if f.code == ErrorCode::ConstraintViolation =>
                {
                    project(tx, &trigger.id, next)?;
                    continue;
                }
                Err(e) => return Err(e),
            }
            // A code-free firing has its whole result already; its prompt
            // commits here (§5).
            if firing.code_snapshot_id.is_none() {
                insert_result_prompt(tx, &firing, &trigger.name, None, now_ns)?;
            }
            project(tx, &trigger.id, next)?;
            pass.accepted.push(Accepted {
                has_code: firing.code_snapshot_id.is_some(),
                firing_id: firing.id,
                desk_id: firing.desk_id,
            });
        } else {
            let next = advance(&trigger, one_off, now_ns);
            let (count, count_capped) = trigger
                .schedule
                .count_between(trigger.occurrence_ns, now_ns);
            crate::desk::append_event(
                tx,
                "TRIGGER_MISSED",
                Some(&trigger.desk_id),
                now_ns,
                json!({
                    "trigger_id": trigger.id,
                    "name": trigger.name,
                    "recurrence": trigger.schedule.recurrence(),
                    "missed_from_ns": trigger.occurrence_ns,
                    "missed_through_ns": now_ns,
                    "count": count,
                    "count_capped": count_capped,
                    "next_occurrence_ns": next,
                }),
            )?;
            project(tx, &trigger.id, next)?;
            pass.missed += 1;
        }
    }
    Ok(pass)
}

/// One due `triggers` row.
struct Due {
    id: String,
    desk_id: String,
    name: String,
    schedule: Schedule,
    brief: String,
    context: Option<String>,
    revision: i64,
    code_snapshot_id: Option<String>,
    occurrence_ns: i64,
    /// The code snapshot's approval state, `None` for a code-free trigger.
    approval: Option<String>,
}

/// The advance after an acceptance or a miss, through the one projection rule
/// (R5 feature SPEC §3.2). A due row is enabled and undeleted by construction;
/// a consumed one-off projects nothing, and everything else is R2's scan.
fn advance(trigger: &Due, one_off: bool, reference_ns: i64) -> Option<i64> {
    crate::trigger::projection(true, trigger.approval.as_deref(), || {
        (!one_off)
            .then(|| trigger.schedule.next_after(reference_ns))
            .flatten()
    })
}

fn project(tx: &Transaction<'_>, trigger_id: &str, next: Option<i64>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE triggers SET next_occurrence_ns = ?2 WHERE id = ?1",
        params![trigger_id, next],
    )?;
    Ok(())
}

/// How long the task may sleep: until the earliest eligible projection, and at
/// most the 60-second recheck (§3.1).
pub fn deadline(earliest_ns: Option<i64>, now_ns: i64) -> Duration {
    match earliest_ns {
        Some(earliest) => {
            Duration::from_nanos(earliest.saturating_sub(now_ns).max(0) as u64).min(RECHECK)
        }
        None => RECHECK,
    }
}

/// The earliest eligible projection, or `None` when nothing is armed (§3.1).
fn earliest_eligible(conn: &rusqlite::Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT next_occurrence_ns FROM triggers \
         WHERE deleted_at_ns IS NULL AND enabled = 1 AND next_occurrence_ns IS NOT NULL \
         ORDER BY next_occurrence_ns LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()
}

/// The coordinator task (§3.1): sleep on the earliest projection, the recheck,
/// or a mutation's wake; run one acceptance unit; wake the executor when an
/// accepted firing carries code. Returns when `shutdown` becomes true.
pub async fn run(
    store: Store,
    started_at_ns: i64,
    wake: Arc<Notify>,
    exec_wake: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        // A failed read leaves `None`, so the task rechecks in 60 s rather than
        // spinning.
        let earliest = store.call(earliest_eligible).ok().flatten();
        tokio::select! {
            () = tokio::time::sleep(deadline(earliest, now_ns())) => {}
            () = wake.notified() => {}
            _ = shutdown.changed() => return,
        }
        let pass = match store.unit(move |tx| accept_or_miss(tx, now_ns(), started_at_ns)) {
            Ok(pass) => pass,
            Err(e) => {
                tracing::error!("scheduler pass failed: {e}");
                // A failing unit backs off one full recheck instead of spinning
                // on a projection that stays due.
                tokio::select! {
                    () = tokio::time::sleep(RECHECK) => continue,
                    _ = shutdown.changed() => return,
                }
            }
        };
        if pass.accepted.iter().any(|a| a.has_code) {
            exec_wake.notify_one();
        }
        // A code-free acceptance queued its result prompt (R3 §6.1).
        if pass.accepted.iter().any(|a| !a.has_code) {
            crate::dispatch::wake();
        }
        if !pass.accepted.is_empty() || pass.missed > 0 {
            tracing::info!(
                accepted = pass.accepted.len(),
                missed = pass.missed,
                "scheduler pass"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (feature SPEC §11)
// ---------------------------------------------------------------------------

#[cfg(test)]
use chrono::Utc;

/// A UTC instant as nanoseconds, for fixed clocks and expected candidates.
#[cfg(test)]
fn utc_ns(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    Utc.with_ymd_and_hms(y, m, d, h, mi, s)
        .single()
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap()
}

#[cfg(test)]
fn recurring(rrule: &str, dtstart: &str, tz: &str) -> Schedule {
    Schedule::Recurring {
        rrule: rrule.to_string(),
        dtstart: dtstart.to_string(),
        tz: tz.to_string(),
    }
}

/// §2: every rejection, and both accepted shapes.
#[cfg(test)]
#[test]
fn form_rejected() {
    let now = utc_ns(2026, 9, 3, 12, 0, 0);
    let rule = |rrule: &str| json!({ "rrule": rrule, "dtstart": "2026-09-03T09:30:00", "tz": "America/New_York" });
    for (label, value) in [
        ("FREQ=SECONDLY", rule("FREQ=SECONDLY")),
        ("COUNT", rule("FREQ=DAILY;COUNT=3")),
        ("UNTIL", rule("FREQ=DAILY;UNTIL=20271231T000000Z")),
        ("BYSECOND", rule("FREQ=DAILY;BYSECOND=30")),
        ("a line break", rule("FREQ=DAILY\nFREQ=WEEKLY")),
        ("a carriage return", rule("FREQ=DAILY\rFREQ=WEEKLY")),
        ("RRULE:", rule("RRULE:FREQ=DAILY")),
        ("lowercase rrule:", rule("rrule:FREQ=DAILY")),
        ("DTSTART", rule("DTSTART:20260903T093000Z FREQ=DAILY")),
        ("RDATE", rule("FREQ=DAILY;RDATE=20260903T093000Z")),
        ("EXDATE", rule("FREQ=DAILY;EXDATE=20260903T093000Z")),
        ("EXRULE", rule("FREQ=DAILY;EXRULE=FREQ=WEEKLY")),
        ("an unparseable rule", rule("NOT-A-RULE")),
        (
            "a rule that fails to validate",
            rule("FREQ=DAILY;BYMONTHDAY=99"),
        ),
        (
            "an unknown tz",
            json!({ "rrule": "FREQ=DAILY", "dtstart": "2026-09-03T09:30:00",
                    "tz": "Mars/Olympus" }),
        ),
        (
            "an unparseable dtstart",
            json!({ "rrule": "FREQ=DAILY", "dtstart": "2026-09-03 09:30", "tz": "UTC" }),
        ),
        (
            "a non-string rrule",
            json!({ "rrule": 5, "dtstart": "2026-09-03T09:30:00", "tz": "UTC" }),
        ),
        ("`at` not RFC 3339", json!({ "at": "tomorrow" })),
        (
            "`at` without an offset",
            json!({ "at": "2026-09-03T14:00:00" }),
        ),
        ("`at` equal to now", json!({ "at": "2026-09-03T12:00:00Z" })),
        ("`at` in the past", json!({ "at": "2026-09-03T11:59:59Z" })),
        ("a non-string at", json!({ "at": 5 })),
        ("neither shape", json!({})),
        (
            "both shapes",
            json!({ "at": "2026-09-03T14:00:00Z", "rrule": "FREQ=DAILY",
                    "dtstart": "2026-09-03T09:30:00", "tz": "UTC" }),
        ),
        (
            "an extra key",
            json!({ "at": "2026-09-03T14:00:00Z", "note": "hi" }),
        ),
        (
            "a missing recurring key",
            json!({ "rrule": "FREQ=DAILY", "dtstart": "2026-09-03T09:30:00" }),
        ),
        ("a non-object", json!(5)),
        ("an array", json!(["at"])),
    ] {
        let answer = Schedule::parse(&value, now);
        assert!(answer.is_err(), "{label} must be rejected, got {answer:?}");
    }

    // Both shapes parse, and each round-trips through the row columns.
    let one_off = Schedule::parse(&json!({ "at": "2026-09-03T14:00:00Z" }), now).unwrap();
    assert_eq!(
        one_off,
        Schedule::OneOff {
            at_ns: utc_ns(2026, 9, 3, 14, 0, 0)
        }
    );
    assert_eq!(one_off.recurrence(), "ONE_OFF");
    assert_eq!(
        one_off.to_json(),
        json!({ "at_ns": utc_ns(2026, 9, 3, 14, 0, 0) })
    );
    assert_eq!(
        Schedule::from_row(
            "ONE_OFF",
            Some(utc_ns(2026, 9, 3, 14, 0, 0)),
            None,
            None,
            None
        ),
        one_off
    );
    // An offset other than Z resolves to the same instant.
    assert_eq!(
        Schedule::parse(&json!({ "at": "2026-09-03T10:00:00-04:00" }), now).unwrap(),
        one_off
    );

    let value = json!({ "rrule": "FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
                        "dtstart": "2026-09-03T09:30:00", "tz": "America/New_York" });
    let repeating = Schedule::parse(&value, now).unwrap();
    assert_eq!(
        repeating,
        recurring(
            "FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
            "2026-09-03T09:30:00",
            "America/New_York"
        )
    );
    assert_eq!(repeating.recurrence(), "RECURRING");
    assert_eq!(repeating.to_json(), value);
    assert_eq!(
        Schedule::from_row(
            "RECURRING",
            None,
            Some("FREQ=DAILY;BYHOUR=9;BYMINUTE=30".into()),
            Some("2026-09-03T09:30:00".into()),
            Some("America/New_York".into()),
        ),
        repeating
    );
    // A minutely rule is the finest cadence the form allows.
    assert!(
        Schedule::parse(
            &json!({ "rrule": "FREQ=MINUTELY", "dtstart": "2026-09-03T09:30:00",
                     "tz": "UTC" }),
            now
        )
        .is_ok()
    );
}

/// §2's DST table, in `America/New_York`.
#[cfg(test)]
#[test]
fn dst_gap_skipped_overlap_earlier() {
    // The 2026-03-08 02:30 wall clock does not exist; the next candidate is
    // 03-09 02:30 EDT (UTC-4).
    let gap = recurring("FREQ=DAILY", "2026-03-07T02:30:00", "America/New_York");
    let first = utc_ns(2026, 3, 7, 7, 30, 0); // 02:30 EST
    assert_eq!(gap.next_after(first - 1), Some(first));
    assert_eq!(gap.next_after(first), Some(utc_ns(2026, 3, 9, 6, 30, 0)));
    // The gap day yields nothing at all between the two.
    assert_eq!(
        gap.count_between(first, utc_ns(2026, 3, 9, 6, 29, 59)),
        (1, false)
    );

    // The 2026-11-01 01:30 wall clock exists twice; the earlier is EDT (UTC-4).
    let overlap = recurring("FREQ=DAILY", "2026-10-31T01:30:00", "America/New_York");
    let start = utc_ns(2026, 10, 31, 5, 30, 0); // 01:30 EDT
    assert_eq!(overlap.next_after(start - 1), Some(start));
    assert_eq!(
        overlap.next_after(start),
        Some(utc_ns(2026, 11, 1, 5, 30, 0))
    );

    // A weekday 09:30 rule keeps its wall clock across 2026-11-01 while the
    // UTC offset shifts from -4 to -5.
    let weekdays = recurring(
        "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
        "2026-09-03T09:30:00",
        "America/New_York",
    );
    let friday = utc_ns(2026, 10, 30, 13, 30, 0); // 09:30 EDT
    assert_eq!(weekdays.next_after(friday - 1), Some(friday));
    // Saturday and Sunday are not candidates: Monday 09:30 EST is next.
    assert_eq!(
        weekdays.next_after(friday),
        Some(utc_ns(2026, 11, 2, 14, 30, 0))
    );
    // Six candidates in the week that spans the change.
    assert_eq!(
        weekdays.count_between(friday, utc_ns(2026, 11, 6, 14, 30, 0)),
        (6, false)
    );
}

/// §2's projection: always the first candidate strictly after the reference,
/// computed from the definition's own anchor.
#[cfg(test)]
#[test]
fn projection_from_anchor() {
    let at_ns = utc_ns(2026, 9, 3, 14, 0, 0);
    let one_off = Schedule::OneOff { at_ns };
    // At creation the reference is now.
    assert_eq!(
        one_off.next_after(utc_ns(2026, 9, 3, 12, 0, 0)),
        Some(at_ns)
    );
    // Its own instant is not strictly after itself, so acceptance consumes it.
    assert_eq!(one_off.next_after(at_ns), None);
    // A one-off past its instant projects nothing, so enable leaves it NULL.
    assert_eq!(one_off.next_after(at_ns + 1), None);

    let daily = recurring("FREQ=DAILY", "2026-09-03T09:30:00", "UTC");
    let anchor = utc_ns(2026, 9, 3, 9, 30, 0);
    assert_eq!(daily.next_after(anchor - 1), Some(anchor));
    // After acceptance the reference is the accepted occurrence.
    assert_eq!(daily.next_after(anchor), Some(utc_ns(2026, 9, 4, 9, 30, 0)));
    // After a miss the reference is now, and the anchor's wall clock is kept
    // — the projection is not "now plus a day".
    let now = utc_ns(2026, 9, 6, 3, 15, 0);
    assert_eq!(daily.next_after(now), Some(utc_ns(2026, 9, 6, 9, 30, 0)));
    assert_eq!(
        daily.next_after(utc_ns(2026, 9, 6, 12, 0, 0)),
        Some(utc_ns(2026, 9, 7, 9, 30, 0))
    );

    // Counting is inclusive at both ends.
    assert_eq!(daily.count_between(anchor, anchor), (1, false));
    assert_eq!(
        daily.count_between(anchor, utc_ns(2026, 9, 6, 9, 30, 0)),
        (4, false)
    );
    assert_eq!(daily.count_between(anchor - 2, anchor - 1), (0, false));
    assert_eq!(one_off.count_between(at_ns, at_ns + 5), (1, false));
    assert_eq!(one_off.count_between(at_ns + 1, at_ns + 5), (0, false));

    // The count cap, reported as capped: a minutely rule over 20 days.
    let minutely = recurring("FREQ=MINUTELY", "2026-09-03T00:00:00", "UTC");
    assert_eq!(
        minutely.count_between(utc_ns(2026, 9, 3, 0, 0, 0), utc_ns(2026, 9, 23, 0, 0, 0)),
        (COUNT_CAP, true)
    );

    // A definition whose columns no longer parse projects nothing rather
    // than panicking.
    assert_eq!(
        recurring("FREQ=DAILY", "nonsense", "UTC").next_after(0),
        None
    );
}

// -- the acceptance unit --------------------------------------------------

#[cfg(test)]
const MINUTE: i64 = 60_000_000_000;

/// Inserts a desk and the trigger rows the §3.3 table needs.
#[cfg(test)]
fn seed(store: &Store, rows: Vec<String>) {
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('d1','alpha','READY','/desks/alpha',1,2,NULL,NULL)",
                [],
            )?;
            tx.execute(
                "INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, \
                 timeout_secs, fingerprint, approval, decided_at_ns, created_at_ns) VALUES \
                 ('c1','d1','print(1)','.py','[\"{script}\"]',300,'ff','ALWAYS_ALLOW',1,1)",
                [],
            )?;
            for sql in &rows {
                tx.execute(sql, [])?;
            }
            Ok(())
        })
        .unwrap();
}

/// A `triggers` row insert: one-off.
#[cfg(test)]
fn one_off_row(id: &str, name: &str, at_ns: i64, code: Option<&str>) -> String {
    format!(
        "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
         enabled, revision, code_snapshot_id, next_occurrence_ns, created_at_ns, \
         updated_at_ns) VALUES ('{id}','d1','{name}','SCHEDULED','ONE_OFF','brief {name}',\
         {at_ns},1,7,{code},{at_ns},1,1)",
        code = code.map_or("NULL".to_string(), |c| format!("'{c}'"))
    )
}

/// A `triggers` row insert: recurring, with an explicit stale projection.
#[cfg(test)]
fn recurring_row(id: &str, name: &str, rrule: &str, dtstart: &str, next: i64) -> String {
    format!(
        "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, rrule, \
         dtstart, tz, enabled, revision, next_occurrence_ns, created_at_ns, updated_at_ns) \
         VALUES ('{id}','d1','{name}','SCHEDULED','RECURRING','brief {name}','{rrule}',\
         '{dtstart}','UTC',1,7,{next},1,1)"
    )
}

#[cfg(test)]
fn projection(store: &Store, id: &'static str) -> Option<i64> {
    store
        .call(move |c| {
            c.query_row(
                "SELECT next_occurrence_ns FROM triggers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
        })
        .unwrap()
}

#[cfg(test)]
fn firings(store: &Store, trigger_id: &'static str) -> Vec<(String, i64, i64, i64, String)> {
    store
        .call(move |c| {
            c.prepare(
                "SELECT id, occurrence_ns, accepted_at_ns, trigger_revision, brief \
                 FROM firings WHERE trigger_id = ?1 ORDER BY id",
            )?
            .query_map(params![trigger_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect()
        })
        .unwrap()
}

#[cfg(test)]
fn missed(store: &Store) -> Vec<Value> {
    store
        .call(|c| {
            c.prepare(
                "SELECT payload FROM operational_events WHERE kind = 'TRIGGER_MISSED' \
                 ORDER BY id",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .iter()
        .map(|p| serde_json::from_str(p).unwrap())
        .collect()
}

// The §3.3 check keeps the name the feature SPEC gives it, which is also the
// unit function's; one module can hold only one, so this check alone is nested.
#[cfg(test)]
mod tests {
    use super::*;

    /// §3.3's table against a fixed clock and daemon start.
    #[test]
    fn accept_or_miss() {
        let (_dir, store) = crate::store::open_temp();
        let now = utc_ns(2026, 9, 3, 12, 0, 30);
        let started = now - 5 * MINUTE;

        seed(
            &store,
            vec![
                // Due 2 s ago, this daemon was up: accepted, consumed.
                one_off_row("t-fresh", "fresh", now - 2_000_000_000, None),
                // Due during downtime: missed, terminal.
                one_off_row("t-down", "down", started - MINUTE, None),
                // Exactly 60 s late: still accepted.
                one_off_row("t-edge", "edge", now - MINUTE, None),
                // One nanosecond later than that: missed.
                one_off_row("t-late", "late", now - MINUTE - 1, None),
                // Code-bearing: accepted, and no prompt commits here.
                one_off_row("t-code", "code", now - 1_000_000_000, Some("c1")),
                // Every minute, projection stale by 2.5 minutes: missed, the
                // range counted, the anchor untouched.
                recurring_row(
                    "t-gap",
                    "gap",
                    "FREQ=MINUTELY",
                    "2026-09-03T00:00:00",
                    utc_ns(2026, 9, 3, 11, 58, 0),
                ),
                // Every minute, 30 s late: accepted, advanced past the occurrence.
                recurring_row(
                    "t-tick",
                    "tick",
                    "FREQ=MINUTELY",
                    "2026-09-03T00:00:00",
                    utc_ns(2026, 9, 3, 12, 0, 0),
                ),
                // Disabled and deleted rows are not eligible at all.
                one_off_row("t-off", "off", now - 2_000_000_000, None)
                    .replace(",1,7,NULL,", ",0,7,NULL,"),
                format!(
                    "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, \
                     at_ns, enabled, revision, next_occurrence_ns, created_at_ns, \
                     updated_at_ns, deleted_at_ns) VALUES ('t-gone','d1','gone','SCHEDULED',\
                     'ONE_OFF','brief gone',{at},1,7,{at},1,1,9)",
                    at = now - 2_000_000_000
                ),
            ],
        );

        let pass = store
            .unit(move |tx| super::accept_or_miss(tx, now, started))
            .unwrap();
        assert_eq!(pass.accepted.len(), 4);
        assert_eq!(pass.missed, 3);
        assert_eq!(
            pass.accepted.iter().filter(|a| a.has_code).count(),
            1,
            "only the code-bearing firing wakes the executor"
        );
        assert!(pass.accepted.iter().all(|a| a.desk_id == "d1"));

        // Accepted: one firing each, carrying the definition's wording and
        // revision, and the projection consumed or advanced.
        for (id, occurrence) in [
            ("t-fresh", now - 2_000_000_000),
            ("t-edge", now - MINUTE),
            ("t-code", now - 1_000_000_000),
        ] {
            let rows = firings(&store, id);
            assert_eq!(rows.len(), 1, "{id}");
            assert_eq!(
                (rows[0].1, rows[0].2, rows[0].3),
                (occurrence, now, 7),
                "{id} provenance"
            );
            assert_eq!(projection(&store, id), None, "{id} is consumed");
        }
        assert_eq!(firings(&store, "t-tick").len(), 1);
        // Advanced from the accepted occurrence, not from now.
        assert_eq!(
            projection(&store, "t-tick"),
            Some(utc_ns(2026, 9, 3, 12, 1, 0))
        );

        // Missed: no firing, one event each, and the projection settled.
        for id in ["t-down", "t-late", "t-gap"] {
            assert!(firings(&store, id).is_empty(), "{id}");
        }
        assert_eq!(projection(&store, "t-down"), None);
        assert_eq!(projection(&store, "t-late"), None);
        // Recurring: the first candidate after now, the anchor untouched.
        assert_eq!(
            projection(&store, "t-gap"),
            Some(utc_ns(2026, 9, 3, 12, 1, 0))
        );
        let anchor: (String, String, String) = store
            .call(|c| {
                c.query_row(
                    "SELECT rrule, dtstart, tz FROM triggers WHERE id = 't-gap'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
            })
            .unwrap();
        assert_eq!(
            anchor,
            (
                "FREQ=MINUTELY".into(),
                "2026-09-03T00:00:00".into(),
                "UTC".into()
            )
        );

        let events = missed(&store);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events,
            vec![
                json!({ "trigger_id": "t-down", "name": "down", "recurrence": "ONE_OFF",
                        "missed_from_ns": started - MINUTE, "missed_through_ns": now,
                        "count": 1, "count_capped": false, "next_occurrence_ns": null }),
                json!({ "trigger_id": "t-gap", "name": "gap", "recurrence": "RECURRING",
                        "missed_from_ns": utc_ns(2026, 9, 3, 11, 58, 0),
                        "missed_through_ns": now, "count": 3, "count_capped": false,
                        "next_occurrence_ns": utc_ns(2026, 9, 3, 12, 1, 0) }),
                json!({ "trigger_id": "t-late", "name": "late", "recurrence": "ONE_OFF",
                        "missed_from_ns": now - MINUTE - 1, "missed_through_ns": now,
                        "count": 1, "count_capped": false, "next_occurrence_ns": null }),
            ]
        );

        // Prompts: one per accepted code-free firing, none for the code-bearing
        // one and none for a miss.
        let prompts: Vec<(String, String)> = store
            .call(|c| {
                c.prepare(
                    "SELECT p.kind, f.trigger_id FROM prompts p \
                     JOIN firings f ON f.id = json_extract(p.payload, '$.firing_id') \
                     ORDER BY f.trigger_id",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
            })
            .unwrap();
        assert_eq!(
            prompts,
            vec![
                ("TRIGGER_RESULT".to_string(), "t-edge".to_string()),
                ("TRIGGER_RESULT".to_string(), "t-fresh".to_string()),
                ("TRIGGER_RESULT".to_string(), "t-tick".to_string()),
            ]
        );

        // Ineligible rows are untouched.
        for id in ["t-off", "t-gone"] {
            assert!(firings(&store, id).is_empty(), "{id}");
            assert_eq!(
                projection(&store, id),
                Some(now - 2_000_000_000),
                "{id} keeps its projection"
            );
        }
    }
}

/// §3.2: a repeated wake on the same occurrence creates nothing.
#[cfg(test)]
#[test]
fn duplicate_wake_no_second_firing() {
    let (_dir, store) = crate::store::open_temp();
    let now = utc_ns(2026, 9, 3, 12, 0, 30);
    let started = now - 5 * MINUTE;
    let occurrence = now - 2_000_000_000;
    seed(&store, vec![one_off_row("t1", "once", occurrence, None)]);

    let first = store
        .unit(move |tx| accept_or_miss(tx, now, started))
        .unwrap();
    assert_eq!(first.accepted.len(), 1);
    assert_eq!(projection(&store, "t1"), None);

    // Simulate the losing wake: the projection is still armed at the
    // occurrence the winner already accepted.
    store
        .unit(move |tx| {
            tx.execute(
                "UPDATE triggers SET next_occurrence_ns = ?1 WHERE id = 't1'",
                params![occurrence],
            )
        })
        .unwrap();
    let second = store
        .unit(move |tx| accept_or_miss(tx, now, started))
        .unwrap();
    assert_eq!(second.accepted.len(), 0);
    assert_eq!(second.missed, 0);

    assert_eq!(firings(&store, "t1").len(), 1, "one firing only");
    let prompts: i64 = store
        .call(|c| c.query_row("SELECT count(*) FROM prompts", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(prompts, 1, "one prompt only");
    // The loser advances the projection as the winner did: consumed.
    assert_eq!(projection(&store, "t1"), None);
}

/// §3.1: the sleep bound, and a mutation's wake reaching the task.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_and_recheck() {
    // The bound: never negative, never above the recheck, exact when sooner.
    assert_eq!(deadline(None, 1_000), RECHECK);
    assert_eq!(deadline(Some(1_000), 1_000), Duration::ZERO);
    assert_eq!(deadline(Some(500), 1_000), Duration::ZERO);
    assert_eq!(deadline(Some(i64::MIN), i64::MAX), Duration::ZERO);
    assert_eq!(
        deadline(Some(1_000 + 250_000_000), 1_000),
        Duration::from_millis(250)
    );
    assert_eq!(deadline(Some(1_000 + 120 * MINUTE), 1_000), RECHECK);
    assert_eq!(deadline(Some(i64::MAX), 0), RECHECK);

    let (_dir, store) = crate::store::open_temp();
    let started = now_ns() - 10_000_000_000;
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('d1','alpha','READY','/desks/alpha',1,2,NULL,NULL)",
                [],
            )
        })
        .unwrap();

    let wake = Arc::new(Notify::new());
    let exec_wake = Arc::new(Notify::new());
    let (shut_tx, shut_rx) = watch::channel(false);
    let task = tokio::spawn(run(
        store.clone(),
        started,
        wake.clone(),
        exec_wake.clone(),
        shut_rx,
    ));

    // Nothing is armed, so the task is asleep on the 60-second recheck; the
    // wake after the mutation is what brings the firing inside 2 s.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let occurrence = now_ns() - 1_000_000_000;
    store
        .unit(move |tx| tx.execute(&one_off_row("t1", "once", occurrence, None), []))
        .unwrap();
    wake.notify_one();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no firing in 2 s");
        if !firings(&store, "t1").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(projection(&store, "t1"), None);

    shut_tx.send(true).unwrap();
    wake.notify_one();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the task stops on shutdown")
        .unwrap();
}
