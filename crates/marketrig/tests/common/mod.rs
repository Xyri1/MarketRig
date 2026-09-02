//! What every `marketrig` CLI test binary needs: a fake daemon endpoint and the
//! two lines that run the real binary against it.
//!
//! `tests/common/mod.rs` is not a test target of its own; each test binary that
//! declares `mod common;` compiles it in, which is how integration tests share
//! code.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// The daemon identity the fake endpoint and [`health_ok`] agree on.
const DAEMON_UUID: &str = "01997f00-0000-7000-8000-000000000001";

/// Every request a fake endpoint saw, in arrival order, as `"METHOD /path"`
/// and the request body — what a test asserts the CLI actually sent.
pub type Requests = Arc<Mutex<Vec<(String, String)>>>;

/// Serve canned responses by "METHOD /path" until the test process exits,
/// recording every request. The responder's second argument is the request's
/// `Authorization` header value.
///
/// ponytail: the thread is never joined — the listener dies with the test
/// binary. Add a shutdown signal only if a test ever needs the port back.
pub fn fake_daemon(
    respond: impl Fn(&str, &str) -> (u16, &'static str) + Send + 'static,
) -> (u16, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake endpoint");
    let port = listener.local_addr().expect("local addr").port();
    let requests: Requests = Requests::default();
    let recorder = Arc::clone(&requests);
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
            let mut body = vec![0u8; length];
            if length > 0 {
                let _ = reader.read_exact(&mut body);
            }
            let mut parts = request_line.split_whitespace();
            let route = format!(
                "{} {}",
                parts.next().unwrap_or(""),
                parts.next().unwrap_or("")
            );
            recorder
                .lock()
                .expect("request log")
                .push((route.clone(), String::from_utf8_lossy(&body).into_owned()));
            let (status, body) = respond(&route, &authorization);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        }
    });
    (port, requests)
}

/// The `GET /health` body every fake endpoint answers, carrying [`DAEMON_UUID`].
pub fn health_ok() -> &'static str {
    r#"{"daemon_uuid":"01997f00-0000-7000-8000-000000000001","version":"0.1.0","started_at_ns":1}"#
}

/// Points the CLI at `port` by writing the endpoint file under a scratch root.
pub fn write_endpoint(root: &Path, port: u16) {
    let runtime = root.join("data").join("runtime");
    std::fs::create_dir_all(&runtime).expect("create runtime dir");
    std::fs::write(
        runtime.join("endpoint.json"),
        format!(
            r#"{{"port":{port},"credential":"{}","daemon_uuid":"{DAEMON_UUID}","pid":1,"started_at_ns":1}}"#,
            "a".repeat(64)
        ),
    )
    .expect("write endpoint.json");
}

pub fn marketrig(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_marketrig"))
        .args(args)
        .env("MARKETRIG_TEST_DATA_ROOT", root)
        .output()
        .expect("run marketrig")
}

pub fn code(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}
