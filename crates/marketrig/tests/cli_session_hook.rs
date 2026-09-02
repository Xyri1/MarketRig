//! `cli::session_hook` — the hook ingress exits 0 and prints nothing whatever
//! the daemon does, and never posts an oversize body (feature SPEC
//! `r3-runtime-delivery` §5.2, §10 check 8).

mod common;

use common::{Requests, code, fake_daemon, health_ok, marketrig_stdin, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";

const HOOK: &[u8] = br#"{"hook_event_name":"Stop","session_id":"s-1"}"#;

fn args() -> Vec<&'static str> {
    vec!["--desk", DESK, "session", "hook"]
}

fn silent(output: &std::process::Output, label: &str) {
    assert_eq!(code(output), 0, "{label}: {output:?}");
    assert!(output.stdout.is_empty(), "{label}: {output:?}");
    assert!(output.stderr.is_empty(), "{label}: {output:?}");
}

fn rejecting(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        _ => (400, r#"{"code":"VALIDATION","message":"No."}"#),
    }
}

fn posted(requests: &Requests) -> Vec<(String, String)> {
    requests.lock().expect("request log").drain(..).collect()
}

#[test]
fn with_no_daemon_it_still_exits_zero() {
    let root = tempfile::tempdir().expect("tempdir");
    silent(&marketrig_stdin(root.path(), &args(), HOOK), "no daemon");
}

#[test]
fn a_rejecting_daemon_is_swallowed_and_the_body_rides_unchanged() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(rejecting);
    write_endpoint(root.path(), port);

    silent(&marketrig_stdin(root.path(), &args(), HOOK), "rejected");
    let seen = posted(&requests);
    assert_eq!(
        seen.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(),
        [
            "GET /health",
            &format!("POST /desks/{DESK}/session/hook")[..],
        ]
    );
    assert_eq!(seen[1].1.as_bytes(), HOOK, "the body rides unchanged");
}

#[test]
fn an_oversize_body_is_dropped_and_still_exits_zero() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(rejecting);
    write_endpoint(root.path(), port);

    let big = vec![b'x'; 64 * 1024 + 1];
    silent(&marketrig_stdin(root.path(), &args(), &big), "oversize");
    assert!(posted(&requests).is_empty(), "nothing is posted");

    // Exactly at the cap the body still rides.
    let mut edge = br#"{"hook_event_name":"Stop","pad":""#.to_vec();
    edge.resize(64 * 1024 - 2, b'p');
    edge.extend_from_slice(br#""}"#);
    assert_eq!(edge.len(), 64 * 1024);
    silent(&marketrig_stdin(root.path(), &args(), &edge), "at the cap");
    assert_eq!(posted(&requests).len(), 2);
}

#[test]
fn hook_without_a_desk_is_a_usage_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let output = marketrig_stdin(root.path(), &["session", "hook"], HOOK);
    assert_eq!(code(&output), 2, "{output:?}");
}
