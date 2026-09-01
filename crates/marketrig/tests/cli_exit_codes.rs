//! `cli::exit_codes` — the 0/1/2/3 mapping of feature SPEC §8 against a fake endpoint.
//!
//! The fake endpoint (`common`) is a plain `TcpListener` serving canned responses
//! by "METHOD /path", so the CLI is exercised through its real binary over real
//! HTTP with only `MARKETRIG_TEST_DATA_ROOT` pointing at a scratch root.

use std::net::TcpListener;

mod common;

use common::{code, fake_daemon, health_ok, marketrig, write_endpoint};

#[test]
fn success_exits_zero() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (
            200,
            r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY","workspace_path":"/w/alpha","workspace_status":"OK","created_at_ns":1,"ready_at_ns":2}]}"#,
        ),
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    });
    write_endpoint(root.path(), port);

    let human = marketrig(root.path(), &["desk", "list"]);
    assert_eq!(code(&human), 0, "{human:?}");
    let text = String::from_utf8(human.stdout).expect("utf-8 stdout");
    assert!(text.contains("alpha"), "human list output: {text:?}");

    let json = marketrig(root.path(), &["--json", "desk", "list"]);
    assert_eq!(code(&json), 0, "{json:?}");
    let emitted = String::from_utf8(json.stdout).expect("utf-8 stdout");
    assert_eq!(
        emitted.trim(),
        r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY","workspace_path":"/w/alpha","workspace_status":"OK","created_at_ns":1,"ready_at_ns":2}]}"#,
        "--json must emit the daemon's body verbatim"
    );
}

#[test]
fn show_resolves_a_name_through_the_desk_list() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (
            200,
            r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY","workspace_path":"/w/alpha","workspace_status":"OK","created_at_ns":1,"ready_at_ns":2}]}"#,
        ),
        "GET /desks/01997f00-0000-7000-8000-00000000000a" => (
            200,
            r#"{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY","workspace_path":"/w/alpha","workspace_status":"OK","created_at_ns":1,"ready_at_ns":2}"#,
        ),
        _ => (
            404,
            r#"{"code":"DESK_NOT_FOUND","message":"No such desk."}"#,
        ),
    });
    write_endpoint(root.path(), port);

    let by_name = marketrig(root.path(), &["desk", "show", "alpha"]);
    assert_eq!(code(&by_name), 0, "{by_name:?}");
    let by_id = marketrig(
        root.path(),
        &["desk", "show", "01997f00-0000-7000-8000-00000000000a"],
    );
    assert_eq!(code(&by_id), 0, "{by_id:?}");
    assert_eq!(by_name.stdout, by_id.stdout);
}

#[test]
fn daemon_error_exits_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "POST /desks" => (
            409,
            r#"{"code":"DESK_NAME_TAKEN","message":"A desk named alpha already exists."}"#,
        ),
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    });
    write_endpoint(root.path(), port);

    let human = marketrig(root.path(), &["desk", "create", "alpha"]);
    assert_eq!(code(&human), 1, "{human:?}");
    assert_eq!(
        String::from_utf8(human.stderr).expect("utf-8 stderr"),
        "error: DESK_NAME_TAKEN: A desk named alpha already exists.\n"
    );

    let json = marketrig(root.path(), &["--json", "desk", "create", "alpha"]);
    assert_eq!(code(&json), 1, "{json:?}");
    assert_eq!(
        String::from_utf8(json.stdout).expect("utf-8 stdout").trim(),
        r#"{"code":"DESK_NAME_TAKEN","message":"A desk named alpha already exists."}"#
    );
}

#[test]
fn usage_error_exits_two() {
    let root = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["desk"],
        vec!["desk", "list", "--json"], // the global flag must precede the group
        vec!["desk", "create"],
        vec!["trigger", "list"],
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 2, "{args:?} -> {output:?}");
    }
}

#[test]
fn missing_endpoint_file_exits_three() {
    let root = tempfile::tempdir().expect("tempdir");
    let output = marketrig(root.path(), &["desk", "list"]);
    assert_eq!(code(&output), 3, "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf-8 stderr")
            .starts_with("error: DAEMON_UNREACHABLE: ")
    );
}

#[test]
fn unreachable_port_exits_three() {
    let root = tempfile::tempdir().expect("tempdir");
    // Bind then drop, so the port is almost certainly closed.
    let closed = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = closed.local_addr().expect("addr").port();
    drop(closed);
    write_endpoint(root.path(), port);

    let output = marketrig(root.path(), &["desk", "list"]);
    assert_eq!(code(&output), 3, "{output:?}");
}

#[test]
fn rejected_credential_exits_three() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|_, _| {
        (
            401,
            r#"{"code":"UNAUTHORIZED","message":"Missing or wrong credential."}"#,
        )
    });
    write_endpoint(root.path(), port);

    let output = marketrig(root.path(), &["desk", "list"]);
    assert_eq!(code(&output), 3, "{output:?}");
    let json = marketrig(root.path(), &["--json", "desk", "list"]);
    assert_eq!(code(&json), 3, "{json:?}");
    let emitted = String::from_utf8(json.stdout).expect("utf-8 stdout");
    assert!(
        emitted.contains("DAEMON_UNREACHABLE"),
        "--json fault envelope: {emitted:?}"
    );
}

#[test]
fn daemon_uuid_mismatch_exits_three() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (
            200,
            r#"{"daemon_uuid":"01997f00-0000-7000-8000-0000000000ff","version":"0.1.0","started_at_ns":1}"#,
        ),
        _ => (200, r#"{"desks":[]}"#),
    });
    write_endpoint(root.path(), port);

    let output = marketrig(root.path(), &["desk", "list"]);
    assert_eq!(code(&output), 3, "{output:?}");
}

#[test]
fn bearer_credential_is_sent_and_never_printed() {
    let root = tempfile::tempdir().expect("tempdir");
    // Every route answers only when the bearer credential arrived, so a request
    // that forgot the header turns into exit 3 instead of exit 0.
    let port = fake_daemon(|route, authorization| {
        if authorization
            != "Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        {
            return (
                401,
                r#"{"code":"UNAUTHORIZED","message":"Missing or wrong credential."}"#,
            );
        }
        match route {
            "GET /health" => (200, health_ok()),
            "GET /desks" => (200, r#"{"desks":[]}"#),
            _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
        }
    });
    write_endpoint(root.path(), port);

    let output = marketrig(root.path(), &["--json", "desk", "list"]);
    assert_eq!(code(&output), 0, "{output:?}");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !printed.contains(&"a".repeat(64)),
        "the credential must never reach CLI output"
    );
}
