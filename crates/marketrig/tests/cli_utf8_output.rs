//! `cli::utf8_output` — the CLI writes its answers as UTF-8 bytes on both
//! platforms, whatever the console's code page (`localization` feature SPEC §4,
//! per LZ-6), against the shared fake endpoint of `tests/common/mod.rs`.
//!
//! `Command::output()` gives the child a pipe for standard output, which is the
//! path a Chinese brief takes through `marketrig … | …` and `> file`; Rust's
//! standard library writes raw UTF-8 to it and converts only for a real Windows
//! console handle, so the exact bytes are the whole assertion.

use serde_json::Value;

mod common;

use common::{code, fake_daemon, health_ok, marketrig, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";
const TRIGGER: &str = "01997f00-0000-7000-8000-00000000000b";

/// 交易复盘 — "trading review", the brief under test.
const BRIEF: &str = "交易复盘";

/// The same four characters spelled as the UTF-8 bytes they must be on the
/// wire: twelve bytes, three per character, and never a code page's own.
const BRIEF_BYTES: &[u8] = b"\xe4\xba\xa4\xe6\x98\x93\xe5\xa4\x8d\xe7\x9b\x98";

const DESKS: &str =
    r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY"}]}"#;

const TRIGGERS: &str = r#"{"triggers":[{"id":"01997f00-0000-7000-8000-00000000000b","desk_id":"01997f00-0000-7000-8000-00000000000a","name":"review","recurrence":"RECURRING","brief":"交易复盘","enabled":true,"revision":1,"created_at_ns":10,"updated_at_ns":10}]}"#;

const TRIGGER_RESOURCE: &str = r#"{"id":"01997f00-0000-7000-8000-00000000000b","desk_id":"01997f00-0000-7000-8000-00000000000a","name":"review","recurrence":"RECURRING","brief":"交易复盘","enabled":true,"revision":1,"created_at_ns":10,"updated_at_ns":10}"#;

fn respond(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, DESKS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/triggers" => (200, TRIGGERS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/triggers/01997f00-0000-7000-8000-00000000000b" => {
            (200, TRIGGER_RESOURCE)
        }
        "POST /desks/01997f00-0000-7000-8000-00000000000a/triggers" => (201, TRIGGER_RESOURCE),
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    }
}

#[track_caller]
fn carries_the_brief(what: &str, bytes: &[u8]) {
    assert!(
        bytes
            .windows(BRIEF_BYTES.len())
            .any(|window| window == BRIEF_BYTES),
        "{what} lost the brief's UTF-8 bytes: {:?}",
        String::from_utf8_lossy(bytes)
    );
}

#[test]
fn a_chinese_brief_reaches_standard_output_as_utf8_bytes() {
    assert_eq!(BRIEF.as_bytes(), BRIEF_BYTES, "the literal's own encoding");

    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    // Human output: `brief: 交易复盘` among the resource's own lines.
    let human = marketrig(root.path(), &["trigger", "show", "alpha", "review"]);
    assert_eq!(code(&human), 0, "{human:?}");
    carries_the_brief("the human output", &human.stdout);
    assert!(
        String::from_utf8(human.stdout.clone())
            .expect("utf-8 stdout")
            .lines()
            .any(|line| line == format!("brief: {BRIEF}")),
        "{human:?}"
    );

    // `--json` is the route's body verbatim, so the same bytes travel again and
    // the parsed field is the string itself.
    let json = marketrig(root.path(), &["--json", "trigger", "show", DESK, TRIGGER]);
    assert_eq!(code(&json), 0, "{json:?}");
    carries_the_brief("the --json output", &json.stdout);
    let parsed: Value = serde_json::from_slice(&json.stdout).expect("the --json body parses");
    assert_eq!(parsed["brief"], BRIEF);
}

#[test]
fn a_chinese_brief_reaches_the_request_body_as_utf8_bytes() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    let created = marketrig(
        root.path(),
        &[
            "trigger", "create", DESK, "--name", "review", "--brief", BRIEF,
        ],
    );
    assert_eq!(code(&created), 0, "{created:?}");

    let sent = requests.lock().expect("request log").clone();
    let (route, body) = sent.last().expect("the create request");
    assert_eq!(route, &format!("POST /desks/{DESK}/triggers"));
    carries_the_brief("the request body", body.as_bytes());
    let parsed: Value = serde_json::from_str(body).expect("the request body parses");
    assert_eq!(parsed["brief"], BRIEF);
}
