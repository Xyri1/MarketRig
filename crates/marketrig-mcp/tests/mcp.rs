//! `mcp::resources_and_freshness`, `mcp::server_side_validation`, and
//! `mcp::pending_order_is_a_tool_result` — feature SPEC
//! `r1-equity-paper-trading` §8 driven end to end, with R5 §3.3's gated answer.
//!
//! The real adapter binary is spawned over stdio and driven by rmcp's own MCP
//! client; behind it a plain `TcpListener` plays the daemon, exactly as
//! `crates/marketrig/tests/cli_exit_codes.rs` does for the CLI.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};

const DAEMON_UUID: &str = "01997f00-0000-7000-8000-000000000001";
const DESK_ID: &str = "01997f00-0000-7000-8000-00000000000a";

/// Serve canned responses until the test process exits.
///
/// ponytail: the thread is never joined — the listener dies with the test
/// binary, as in the CLI's fake endpoint.
fn fake_daemon(respond: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> u16 {
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
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            if length > 0 && reader.read_exact(&mut body).is_err() {
                continue;
            }
            let mut parts = request_line.split_whitespace();
            let route = format!(
                "{} {}",
                parts.next().unwrap_or(""),
                parts.next().unwrap_or("")
            );
            let (status, body) = respond(&route, &String::from_utf8_lossy(&body));
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

fn health_ok() -> String {
    format!(r#"{{"daemon_uuid":"{DAEMON_UUID}","version":"0.1.0","started_at_ns":1}}"#)
}

fn desks() -> String {
    format!(
        r#"{{"desks":[{{"id":"{DESK_ID}","name":"alpha","state":"READY","workspace_path":"/w/alpha","workspace_status":"OK","created_at_ns":1,"ready_at_ns":2}}]}}"#
    )
}

fn write_endpoint(root: &Path, port: u16) {
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

/// Spawn the real adapter for desk `alpha` and complete the MCP handshake.
async fn adapter(root: &Path) -> RunningService<RoleClient, ()> {
    let command =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_marketrig-mcp")).configure(|command| {
            command
                .arg("--desk")
                .arg("alpha")
                .env("MARKETRIG_TEST_DATA_ROOT", root);
        });
    ().serve(TokioChildProcess::new(command).expect("spawn marketrig-mcp"))
        .await
        .expect("initialize the MCP session")
}

fn text_of(contents: &ResourceContents) -> &str {
    match contents {
        ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected a text resource, got {other:?}"),
    }
}

#[tokio::test]
async fn resources_and_freshness() {
    let root = tempfile::tempdir().expect("tempdir");
    let reads = AtomicUsize::new(0);
    let port = fake_daemon(move |route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, desks()),
        _ if route == format!("GET /desks/{DESK_ID}/market/quotes") => {
            let sequence = reads.fetch_add(1, Ordering::SeqCst);
            (
                200,
                format!(
                    r#"{{"quotes":[{{"instrument_id":"AAPL.XNAS","last":"{}.00","sequence":{sequence}}}]}}"#,
                    100 + sequence
                ),
            )
        }
        _ => (
            404,
            r#"{"code":"NOT_FOUND","message":"Unexpected route."}"#.to_string(),
        ),
    });
    write_endpoint(root.path(), port);
    let service = adapter(root.path()).await;

    let listed = service
        .list_resources(None)
        .await
        .expect("list the resources");
    let uris: Vec<&str> = listed.resources.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        [
            "marketrig://desk/alpha/quotes",
            "marketrig://desk/alpha/book",
            "marketrig://desk/alpha/positions",
            "marketrig://desk/alpha/orders",
            "marketrig://desk/alpha/instruments",
        ],
        "resources/list names exactly the five concrete URIs"
    );

    // Two reads straddling a daemon-side tick: the adapter caches nothing, so
    // the second read must carry the daemon's new body.
    let quotes = "marketrig://desk/alpha/quotes";
    let first = service
        .read_resource(ReadResourceRequestParams::new(quotes))
        .await
        .expect("first read");
    let second = service
        .read_resource(ReadResourceRequestParams::new(quotes))
        .await
        .expect("second read");
    let first = text_of(&first.contents[0]).to_string();
    let second = text_of(&second.contents[0]).to_string();
    assert!(
        first.contains(r#""last":"100.00""#) && first.contains(r#""sequence":0"#),
        "the first read is the daemon's body verbatim: {first}"
    );
    assert!(
        second.contains(r#""last":"101.00""#) && second.contains(r#""sequence":1"#),
        "the second read must be re-fetched, not cached: {second}"
    );

    let _ = service.cancel().await;
}

#[tokio::test]
async fn server_side_validation() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, body| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, desks()),
        // The daemon — not the adapter — judges the body (§4.2, per D4).
        _ if route == format!("POST /desks/{DESK_ID}/orders") => {
            let received: serde_json::Value =
                serde_json::from_str(body).expect("the adapter forwards JSON");
            let mut fields: Vec<&str> = received
                .as_object()
                .expect("a JSON object body")
                .keys()
                .map(String::as_str)
                .collect();
            fields.sort_unstable();
            (
                400,
                format!(
                    r#"{{"code":"ORDER_INVALID","message":"An order needs six fields; received: {}."}}"#,
                    fields.join(",")
                ),
            )
        }
        _ => (
            404,
            r#"{"code":"NOT_FOUND","message":"Unexpected route."}"#.to_string(),
        ),
    });
    write_endpoint(root.path(), port);
    let service = adapter(root.path()).await;

    let listed = service.list_tools(None).await.expect("list the tools");
    let names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["submit_order", "cancel_order"]);

    // A two-field body against the six-field schema: the adapter forwards it
    // untouched and hands back the daemon's refusal.
    let arguments = serde_json::json!({ "action_id": "buy-tencent-1", "side": "BUY" })
        .as_object()
        .expect("object")
        .clone();
    let result = service
        .call_tool(CallToolRequestParams::new("submit_order").with_arguments(arguments))
        .await
        .expect("the call itself is routed");
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let text = result.content[0]
        .as_text()
        .expect("a text content block")
        .text
        .clone();
    assert!(
        text.contains("ORDER_INVALID"),
        "the tool error carries the envelope's code: {text}"
    );
    assert!(
        text.contains("An order needs six fields; received: action_id,side."),
        "the tool error carries the daemon's own message, proving the two \
         fields were forwarded verbatim: {text}"
    );

    let _ = service.cancel().await;
}

/// `mcp::pending_order_is_a_tool_result` — a gated order answers `202`, which
/// the adapter hands back as the tool's successful record, not an error (R5
/// feature SPEC §3.3).
#[tokio::test]
async fn pending_order_is_a_tool_result() {
    const PENDING: &str = r#"{"action_id":"buy-tencent-1","id":"01997f00-0000-7000-8000-0000000000c1","kind":"SUBMIT","source":"SESSION","approval":"PENDING","created_at_ns":300}"#;

    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, desks()),
        _ if route == format!("POST /desks/{DESK_ID}/orders") => (202, PENDING.to_string()),
        _ => (
            404,
            r#"{"code":"NOT_FOUND","message":"Unexpected route."}"#.to_string(),
        ),
    });
    write_endpoint(root.path(), port);
    let service = adapter(root.path()).await;

    let arguments = serde_json::json!({
        "action_id": "buy-tencent-1", "instrument_id": "0700.XHKG",
        "side": "BUY", "type": "MARKET", "quantity": "100"
    })
    .as_object()
    .expect("object")
    .clone();
    let result = service
        .call_tool(CallToolRequestParams::new("submit_order").with_arguments(arguments))
        .await
        .expect("the call itself is routed");
    assert_ne!(result.is_error, Some(true), "{result:?}");
    assert_eq!(
        result.content[0]
            .as_text()
            .expect("a text content block")
            .text,
        PENDING,
        "the pending record reaches the agent verbatim"
    );

    let _ = service.cancel().await;
}
