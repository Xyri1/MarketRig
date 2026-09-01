//! `cli::exit_codes` — the 0/1/2/3 mapping of feature SPEC §8 against a fake endpoint.
//!
//! The fake endpoint is a plain `TcpListener` serving canned responses by
//! "METHOD /path", so the CLI is exercised through its real binary over real
//! HTTP with only `MARKETRIG_TEST_DATA_ROOT` pointing at a scratch root.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

/// Serve canned responses until the test process exits.
///
/// ponytail: the thread is never joined — the listener dies with the test
/// binary. Add a shutdown signal only if a test ever needs the port back.
fn fake_daemon(respond: impl Fn(&str, &str) -> (u16, &'static str) + Send + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake endpoint");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut length = 0usize;
            let mut authorization = String::new();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("authorization:") {
                    authorization = header["authorization:".len()..].trim().to_string();
                }
            }
            if length > 0 {
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
            }
            let mut parts = request_line.split_whitespace();
            let route = format!(
                "{} {}",
                parts.next().unwrap_or(""),
                parts.next().unwrap_or("")
            );
            let (status, body) = respond(&route, &authorization);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        }
    });
    port
}

const DAEMON_UUID: &str = "01997f00-0000-7000-8000-000000000001";

fn health_ok() -> &'static str {
    r#"{"daemon_uuid":"01997f00-0000-7000-8000-000000000001","version":"0.1.0","started_at_ns":1}"#
}

fn write_endpoint(root: &Path, port: u16, daemon_uuid: &str) {
    let runtime = root.join("data").join("runtime");
    std::fs::create_dir_all(&runtime).expect("create runtime dir");
    std::fs::write(
        runtime.join("endpoint.json"),
        format!(
            r#"{{"port":{port},"credential":"{}","daemon_uuid":"{daemon_uuid}","pid":1,"started_at_ns":1}}"#,
            "a".repeat(64)
        ),
    )
    .expect("write endpoint.json");
}

fn marketrig(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_marketrig"))
        .args(args)
        .env("MARKETRIG_TEST_DATA_ROOT", root)
        .output()
        .expect("run marketrig")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
    write_endpoint(root.path(), port, DAEMON_UUID);

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
