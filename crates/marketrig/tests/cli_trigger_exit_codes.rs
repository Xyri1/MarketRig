//! `cli::trigger_exit_codes` — the `trigger` group's 0/1/2/3 mapping, its
//! request bodies, and its human output (feature SPEC `r2-scheduled-triggers`
//! §9) against the shared fake endpoint of `tests/common/mod.rs`.

use std::process::Output;

use serde_json::{Value, json};

mod common;

use common::{Requests, code, fake_daemon, health_ok, marketrig, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";
const TRIGGER: &str = "01997f00-0000-7000-8000-00000000000b";
const FIRING: &str = "01997f00-0000-7000-8000-00000000000c";

const DESKS: &str =
    r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY"}]}"#;

const TRIGGERS: &str = r#"{"triggers":[{"id":"01997f00-0000-7000-8000-00000000000b","desk_id":"01997f00-0000-7000-8000-00000000000a","name":"morning","source":"SCHEDULED","recurrence":"ONE_OFF","brief":"Check the tape.","context":"AAPL.XNAS","schedule":{"at_ns":1780000000000000000},"enabled":true,"revision":1,"next_occurrence_ns":1780000000000000000,"created_at_ns":10,"updated_at_ns":10},{"id":"01997f00-0000-7000-8000-00000000000d","desk_id":"01997f00-0000-7000-8000-00000000000a","name":"weekday","source":"SCHEDULED","recurrence":"RECURRING","brief":"Trade the open.","schedule":{"rrule":"FREQ=DAILY;BYHOUR=9","dtstart":"2026-09-03T09:30:00","tz":"America/New_York"},"enabled":false,"revision":2,"created_at_ns":11,"updated_at_ns":12}]}"#;

const TRIGGER_RESOURCE: &str = r#"{"id":"01997f00-0000-7000-8000-00000000000b","desk_id":"01997f00-0000-7000-8000-00000000000a","name":"morning","source":"SCHEDULED","recurrence":"ONE_OFF","brief":"Check the tape.","context":"AAPL.XNAS","schedule":{"at_ns":1780000000000000000},"enabled":true,"revision":1,"next_occurrence_ns":1780000000000000000,"code":{"snapshot_id":"01997f00-0000-7000-8000-0000000000ee","suffix":".py","argv":["{script}"],"timeout_secs":300,"fingerprint":"e3b0","approved_at_ns":10,"source_bytes":9,"source":"print(1)\n"},"created_at_ns":10,"updated_at_ns":10}"#;

const FIRINGS: &str = r#"{"firings":[{"id":"01997f00-0000-7000-8000-00000000000c","desk_id":"01997f00-0000-7000-8000-00000000000a","trigger_id":"01997f00-0000-7000-8000-00000000000b","occurrence_ns":1780000000000000000,"accepted_at_ns":1780000000000000005,"trigger_revision":1,"brief":"Check the tape.","execution":{"state":"COMPLETE","outcome":"EXITED","exit_code":0}},{"id":"01997f00-0000-7000-8000-00000000000e","desk_id":"01997f00-0000-7000-8000-00000000000a","trigger_id":"01997f00-0000-7000-8000-00000000000b","occurrence_ns":1779999999940000000,"accepted_at_ns":1779999999940000003,"trigger_revision":1,"brief":"Check the tape."}]}"#;

const FIRING_RESOURCE: &str = r#"{"id":"01997f00-0000-7000-8000-00000000000c","desk_id":"01997f00-0000-7000-8000-00000000000a","trigger_id":"01997f00-0000-7000-8000-00000000000b","occurrence_ns":1780000000000000000,"accepted_at_ns":1780000000000000005,"trigger_revision":1,"brief":"Check the tape.","execution":{"outcome":"EXITED","stdout":"ok\n"}}"#;

/// Every documented route of §8, answered from canned resources.
fn respond(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, DESKS),
        "POST /desks/01997f00-0000-7000-8000-00000000000a/triggers" => (201, TRIGGER_RESOURCE),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/triggers" => (200, TRIGGERS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/triggers/01997f00-0000-7000-8000-00000000000b"
        | "PATCH /desks/01997f00-0000-7000-8000-00000000000a/triggers/01997f00-0000-7000-8000-00000000000b"
        | "DELETE /desks/01997f00-0000-7000-8000-00000000000a/triggers/01997f00-0000-7000-8000-00000000000b" => {
            (200, TRIGGER_RESOURCE)
        }
        "GET /desks/01997f00-0000-7000-8000-00000000000a/triggers/01997f00-0000-7000-8000-00000000000b/firings" => {
            (200, FIRINGS)
        }
        "GET /desks/01997f00-0000-7000-8000-00000000000a/firings/01997f00-0000-7000-8000-00000000000c" => {
            (200, FIRING_RESOURCE)
        }
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    }
}

/// Drains the request log into `(route, parsed body)`; a bodiless request
/// parses as `null`.
fn sent(requests: &Requests) -> Vec<(String, Value)> {
    requests
        .lock()
        .expect("request log")
        .drain(..)
        .map(|(route, body)| (route, serde_json::from_str(&body).unwrap_or(Value::Null)))
        .collect()
}

fn routes(requests: &Requests) -> Vec<String> {
    sent(requests).into_iter().map(|(route, _)| route).collect()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn lines(output: &Output) -> Vec<String> {
    stdout(output).lines().map(str::to_string).collect()
}

#[test]
fn create_sends_the_documented_bodies() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    // A one-off: the schedule is `{"at": …}` and every value passes through.
    let one_off = marketrig(
        root.path(),
        &[
            "trigger",
            "create",
            "alpha",
            "--name",
            "morning",
            "--brief",
            "Check the tape.",
            "--context",
            "AAPL.XNAS",
            "--at",
            "2026-09-03T14:00:00Z",
        ],
    );
    assert_eq!(code(&one_off), 0, "{one_off:?}");
    assert_eq!(
        sent(&requests),
        [
            ("GET /health".to_string(), Value::Null),
            ("GET /desks".to_string(), Value::Null),
            (
                format!("POST /desks/{DESK}/triggers"),
                json!({
                    "name": "morning",
                    "brief": "Check the tape.",
                    "context": "AAPL.XNAS",
                    "schedule": { "at": "2026-09-03T14:00:00Z" },
                }),
            ),
        ]
    );

    // The recurring trio, with code read from a file: the suffix defaults to
    // the file's extension and `argv` to `{script}` alone (§4.1).
    let script = root.path().join("job.py");
    std::fs::write(&script, "print(1)\n").expect("write script");
    let recurring = marketrig(
        root.path(),
        &[
            "trigger",
            "create",
            DESK,
            "--name",
            "weekday",
            "--brief",
            "Trade the open.",
            "--rrule",
            "FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
            "--dtstart",
            "2026-09-03T09:30:00",
            "--tz",
            "America/New_York",
            "--code",
            script.to_str().expect("utf-8 path"),
        ],
    );
    assert_eq!(code(&recurring), 0, "{recurring:?}");
    assert_eq!(
        sent(&requests),
        [
            ("GET /health".to_string(), Value::Null),
            (
                format!("POST /desks/{DESK}/triggers"),
                json!({
                    "name": "weekday",
                    "brief": "Trade the open.",
                    "schedule": {
                        "rrule": "FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
                        "dtstart": "2026-09-03T09:30:00",
                        "tz": "America/New_York",
                    },
                    "code": {
                        "source": "print(1)\n",
                        "suffix": ".py",
                        "argv": ["{script}"],
                    },
                }),
            ),
        ],
        "a UUID desk skips GET /desks"
    );

    // The complements: no extension is an empty suffix, `--arg` repeats into
    // `argv`, and `--timeout` becomes `timeout_secs`.
    let plain = root.path().join("job");
    std::fs::write(&plain, "order AAPL.XNAS BUY 1\n").expect("write script");
    let explicit = marketrig(
        root.path(),
        &[
            "trigger",
            "create",
            DESK,
            "--name",
            "order",
            "--brief",
            "Buy one lot.",
            "--at",
            "2026-09-03T14:00:00Z",
            "--code",
            plain.to_str().expect("utf-8 path"),
            "--arg",
            "/opt/trigger-code",
            "--arg",
            "{script}",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(code(&explicit), 0, "{explicit:?}");
    assert_eq!(
        sent(&requests)[1].1["code"],
        json!({
            "source": "order AAPL.XNAS BUY 1\n",
            "suffix": "",
            "argv": ["/opt/trigger-code", "{script}"],
            "timeout_secs": 30,
        })
    );

    // `--json` is the route's body verbatim.
    let json = marketrig(
        root.path(),
        &[
            "--json",
            "trigger",
            "create",
            DESK,
            "--name",
            "morning",
            "--brief",
            "Check the tape.",
            "--at",
            "2026-09-03T14:00:00Z",
        ],
    );
    assert_eq!(code(&json), 0, "{json:?}");
    assert_eq!(stdout(&json).trim(), TRIGGER_RESOURCE);
}

#[test]
fn daemon_error_exits_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, DESKS),
        _ => (
            400,
            r#"{"code":"TRIGGER_INVALID","message":"FREQ=SECONDLY is not accepted."}"#,
        ),
    });
    write_endpoint(root.path(), port);

    let human = marketrig(
        root.path(),
        &[
            "trigger",
            "create",
            "alpha",
            "--name",
            "tick",
            "--brief",
            "Too often.",
            "--rrule",
            "FREQ=SECONDLY",
            "--dtstart",
            "2026-09-03T09:30:00",
            "--tz",
            "UTC",
        ],
    );
    assert_eq!(code(&human), 1, "{human:?}");
    assert_eq!(
        String::from_utf8(human.stderr).expect("utf-8 stderr"),
        "error: TRIGGER_INVALID: FREQ=SECONDLY is not accepted.\n"
    );

    let json = marketrig(
        root.path(),
        &[
            "--json",
            "trigger",
            "create",
            "alpha",
            "--name",
            "tick",
            "--brief",
            "Too often.",
            "--rrule",
            "FREQ=SECONDLY",
            "--dtstart",
            "2026-09-03T09:30:00",
            "--tz",
            "UTC",
        ],
    );
    assert_eq!(code(&json), 1, "{json:?}");
    assert_eq!(
        stdout(&json).trim(),
        r#"{"code":"TRIGGER_INVALID","message":"FREQ=SECONDLY is not accepted."}"#
    );
}

#[test]
fn a_trigger_name_resolves_through_the_listing_and_a_uuid_does_not() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    let by_name = marketrig(root.path(), &["trigger", "show", "alpha", "morning"]);
    assert_eq!(code(&by_name), 0, "{by_name:?}");
    assert_eq!(
        routes(&requests),
        [
            "GET /health".to_string(),
            "GET /desks".to_string(),
            format!("GET /desks/{DESK}/triggers"),
            format!("GET /desks/{DESK}/triggers/{TRIGGER}"),
        ]
    );

    let by_id = marketrig(root.path(), &["trigger", "show", DESK, TRIGGER]);
    assert_eq!(code(&by_id), 0, "{by_id:?}");
    assert_eq!(
        routes(&requests),
        [
            "GET /health".to_string(),
            format!("GET /desks/{DESK}/triggers/{TRIGGER}"),
        ],
        "canonical UUIDs skip both listings"
    );
    assert_eq!(by_name.stdout, by_id.stdout);
}

#[test]
fn an_unknown_trigger_name_exits_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    let output = marketrig(root.path(), &["trigger", "show", "alpha", "nowhere"]);
    assert_eq!(code(&output), 1, "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
        "error: TRIGGER_NOT_FOUND: No trigger is named nowhere.\n"
    );
}

#[test]
fn usage_errors_exit_two() {
    let root = tempfile::tempdir().expect("tempdir");
    // Deliberately no endpoint file: a usage error is diagnosed before any
    // daemon is contacted, so none of these can reach exit 3.
    let binary = root.path().join("binary.py");
    std::fs::write(&binary, [0x70, 0x79, 0xff, 0xfe, 0x0a]).expect("write binary file");
    let binary = binary.to_str().expect("utf-8 path");
    let missing = root.path().join("absent.py");
    let missing = missing.to_str().expect("utf-8 path");

    for args in [
        // an update with no flags at all
        vec!["trigger", "update", "alpha", "morning"],
        // a code file that is not UTF-8, and one that is not there
        vec![
            "trigger",
            "create",
            "alpha",
            "--name",
            "n",
            "--brief",
            "b",
            "--at",
            "2026-09-03T14:00:00Z",
            "--code",
            binary,
        ],
        vec![
            "trigger",
            "create",
            "alpha",
            "--name",
            "n",
            "--brief",
            "b",
            "--at",
            "2026-09-03T14:00:00Z",
            "--code",
            missing,
        ],
        // both schedule shapes at once, and half the recurring trio
        vec![
            "trigger",
            "create",
            "alpha",
            "--name",
            "n",
            "--brief",
            "b",
            "--at",
            "2026-09-03T14:00:00Z",
            "--rrule",
            "FREQ=DAILY",
            "--dtstart",
            "2026-09-03T09:30:00",
            "--tz",
            "UTC",
        ],
        vec![
            "trigger",
            "create",
            "alpha",
            "--name",
            "n",
            "--brief",
            "b",
            "--rrule",
            "FREQ=DAILY",
        ],
        // no schedule at all on create
        vec!["trigger", "create", "alpha", "--name", "n", "--brief", "b"],
        // the two exclusive clearing flags
        vec![
            "trigger",
            "update",
            "alpha",
            "morning",
            "--context",
            "c",
            "--no-context",
        ],
        vec![
            "trigger",
            "update",
            "alpha",
            "morning",
            "--no-code",
            "--code",
            binary,
        ],
        // a code option without the file it decorates
        vec![
            "trigger",
            "create",
            "alpha",
            "--name",
            "n",
            "--brief",
            "b",
            "--at",
            "2026-09-03T14:00:00Z",
            "--suffix",
            ".py",
        ],
        // the global flag precedes the group
        vec!["trigger", "list", "alpha", "--json"],
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 2, "{args:?} -> {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).starts_with("error: "),
            "{args:?} -> {output:?}"
        );
    }
}

#[test]
fn enable_disable_and_delete_use_patch_patch_and_delete() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    for (command, method, body) in [
        ("enable", "PATCH", json!({ "enabled": true })),
        ("disable", "PATCH", json!({ "enabled": false })),
        ("delete", "DELETE", Value::Null),
    ] {
        let output = marketrig(root.path(), &["trigger", command, DESK, TRIGGER]);
        assert_eq!(code(&output), 0, "{command} -> {output:?}");
        assert_eq!(
            sent(&requests),
            [
                ("GET /health".to_string(), Value::Null),
                (format!("{method} /desks/{DESK}/triggers/{TRIGGER}"), body),
            ]
        );
    }

    // An update carries only the fields the caller named; `--no-context` and
    // `--no-code` are explicit nulls (§8).
    let update = marketrig(
        root.path(),
        &[
            "trigger",
            "update",
            DESK,
            TRIGGER,
            "--brief",
            "Check the close.",
            "--no-context",
            "--no-code",
            "--at",
            "2026-09-04T14:00:00Z",
        ],
    );
    assert_eq!(code(&update), 0, "{update:?}");
    assert_eq!(
        sent(&requests)[1],
        (
            format!("PATCH /desks/{DESK}/triggers/{TRIGGER}"),
            json!({
                "brief": "Check the close.",
                "context": null,
                "code": null,
                "schedule": { "at": "2026-09-04T14:00:00Z" },
            }),
        )
    );
}

#[test]
fn without_a_daemon_the_group_exits_three() {
    let root = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["trigger", "list", "alpha"],
        vec!["trigger", "firings", DESK, TRIGGER],
        vec!["trigger", "firing", DESK, FIRING],
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 3, "{args:?} -> {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).starts_with("error: DAEMON_UNREACHABLE: "),
            "{args:?} -> {output:?}"
        );
    }
}

#[test]
fn human_output_carries_the_documented_rows_and_fields() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    // Listing: name, recurrence, enabled, next occurrence, id. A trigger that
    // is never due prints a blank cell rather than shifting a column.
    let list = marketrig(root.path(), &["trigger", "list", DESK]);
    assert_eq!(code(&list), 0, "{list:?}");
    assert_eq!(
        lines(&list),
        [
            format!("morning\tONE_OFF\ttrue\t1780000000000000000\t{TRIGGER}"),
            "weekday\tRECURRING\tfalse\t\t01997f00-0000-7000-8000-00000000000d".to_string(),
        ]
    );

    // Firings: id, occurrence, accepted, outcome — one level down into the
    // execution summary, blank while there is none.
    let firings = marketrig(root.path(), &["trigger", "firings", DESK, TRIGGER]);
    assert_eq!(code(&firings), 0, "{firings:?}");
    assert_eq!(
        lines(&firings),
        [
            format!("{FIRING}\t1780000000000000000\t1780000000000000005\tEXITED"),
            "01997f00-0000-7000-8000-00000000000e\t1779999999940000000\t1779999999940000003\t"
                .to_string(),
        ]
    );

    // A single resource: every field the daemon sent, in its own key order,
    // nested objects as compact JSON.
    let show = marketrig(root.path(), &["trigger", "show", DESK, TRIGGER]);
    assert_eq!(code(&show), 0, "{show:?}");
    assert_eq!(
        lines(&show),
        [
            format!("id: {TRIGGER}"),
            format!("desk_id: {DESK}"),
            "name: morning".to_string(),
            "source: SCHEDULED".to_string(),
            "recurrence: ONE_OFF".to_string(),
            "brief: Check the tape.".to_string(),
            "context: AAPL.XNAS".to_string(),
            format!("schedule: {}", json!({ "at_ns": 1780000000000000000u64 })),
            "enabled: true".to_string(),
            "revision: 1".to_string(),
            "next_occurrence_ns: 1780000000000000000".to_string(),
            format!(
                "code: {}",
                json!({
                    "snapshot_id": "01997f00-0000-7000-8000-0000000000ee",
                    "suffix": ".py",
                    "argv": ["{script}"],
                    "timeout_secs": 300,
                    "fingerprint": "e3b0",
                    "approved_at_ns": 10,
                    "source_bytes": 9,
                    "source": "print(1)\n",
                })
            ),
            "created_at_ns: 10".to_string(),
            "updated_at_ns: 10".to_string(),
        ]
    );

    let firing = marketrig(root.path(), &["trigger", "firing", DESK, FIRING]);
    assert_eq!(code(&firing), 0, "{firing:?}");
    assert_eq!(
        lines(&firing),
        [
            format!("id: {FIRING}"),
            format!("desk_id: {DESK}"),
            format!("trigger_id: {TRIGGER}"),
            "occurrence_ns: 1780000000000000000".to_string(),
            "accepted_at_ns: 1780000000000000005".to_string(),
            "trigger_revision: 1".to_string(),
            "brief: Check the tape.".to_string(),
            format!(
                "execution: {}",
                json!({ "outcome": "EXITED", "stdout": "ok\n" })
            ),
        ]
    );
}
