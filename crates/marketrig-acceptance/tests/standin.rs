//! The stand-ins' own checks: the halves the gate drives, end to end.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §9.1 (per R3-8) for
//! `runtime-standin` and `sdd/features/r4-memory-skills-loop/SPEC.md` §7.1 (per
//! R4-6) for `memory-standin`. The gate scenarios G27–G37 exercise both through
//! the daemon; these checks are the smaller thing that fails first when a
//! stand-in's own wire drifts from what `marketrigd` sends.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const STANDIN: &str = env!("CARGO_BIN_EXE_runtime-standin");
const MEMORY: &str = env!("CARGO_BIN_EXE_memory-standin");

/// A scratch directory of this test's own, removed when it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("marketrig-standin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch(path)
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).expect("a scratch file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child killed when it goes out of scope, however the test ends.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a loopback port")
        .local_addr()
        .expect("its address")
        .port()
}

/// The next line the child prints, or a panic when it stops printing.
fn next_line(reader: &mut BufReader<std::process::ChildStdout>) -> String {
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("the child's stdout");
    assert!(read > 0, "the child stopped printing");
    line.trim_end().to_string()
}

#[test]
fn discovery_reads_the_version_and_both_capability_strings() {
    let version = Command::new(STANDIN)
        .arg("--version")
        .output()
        .expect("--version");
    let printed = String::from_utf8_lossy(&version.stdout).trim().to_string();
    assert_eq!(printed, "runtime-standin 99.0.0");

    let help = Command::new(STANDIN)
        .arg("--help")
        .output()
        .expect("--help");
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    for needle in [
        "app-server",
        "--dangerously-load-development-channels",
        "--settings",
    ] {
        assert!(help.contains(needle), "--help omits {needle}: {help}");
    }

    // The scripted version is what §9.2's G27 turns into VERSION_UNSUPPORTED.
    let scratch = Scratch::new("version");
    let script = scratch.write("script.json", r#"{"version":"0.1.0"}"#);
    let old = Command::new(STANDIN)
        .arg("--version")
        .env("MARKETRIG_STANDIN_SCRIPT", &script)
        .output()
        .expect("--version");
    assert_eq!(
        String::from_utf8_lossy(&old.stdout).trim(),
        "runtime-standin 0.1.0"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_codex_halves_start_a_thread_and_echo_a_turn() {
    let scratch = Scratch::new("codex");
    let token = "0123456789abcdef";
    let token_file = scratch.write("codex-ws-token", token);
    let script = scratch.write("script.json", r#"{"active_after_input_ms":150}"#);
    let workspace = scratch.0.join("workspace");
    std::fs::create_dir_all(&workspace).expect("a workspace");

    let port = free_port();
    let url = format!("ws://127.0.0.1:{port}");
    let _server = Child(
        Command::new(STANDIN)
            .args([
                "app-server",
                "--listen",
                &url,
                "--ws-auth",
                "capability-token",
                "--ws-token-file",
                &token_file.display().to_string(),
            ])
            .env("MARKETRIG_STANDIN_SCRIPT", &script)
            .spawn()
            .expect("the app-server starts"),
    );

    // The daemon's own connection: the bearer, `initialize`, then broadcasts.
    let mut socket = connect(&url, token).await;
    let initialize = call(&mut socket, 1, "initialize", json!({"clientInfo": {}})).await;
    assert!(initialize.get("result").is_some(), "{initialize}");

    // The TUI half, spawned the way the terminal manager spawns it.
    let mut tui = Child(
        Command::new(STANDIN)
            .args([
                "--remote",
                &url,
                "--remote-auth-token-env",
                "MARKETRIG_CODEX_WS_TOKEN",
                "-C",
                &workspace.display().to_string(),
            ])
            .env("MARKETRIG_CODEX_WS_TOKEN", token)
            .env("MARKETRIG_STANDIN_SCRIPT", &script)
            .stdout(Stdio::piped())
            .spawn()
            .expect("the TUI starts"),
    );
    let mut out = BufReader::new(tui.0.stdout.take().expect("a piped stdout"));

    // Pointer discovery: `thread/started`, non-ephemeral, the workspace cwd,
    // and the inline `idle` that is a new session's only readiness (§4.2).
    let started = broadcast(&mut socket, "thread/started").await;
    let thread = &started["params"]["thread"];
    assert_eq!(thread["ephemeral"], json!(false));
    assert_eq!(thread["cwd"], json!(workspace.display().to_string()));
    assert_eq!(thread["status"]["type"], json!("idle"));
    let thread_id = thread["id"].as_str().expect("a thread id").to_string();

    // Delivery: the gate is open, the turn starts, the TUI echoes the text.
    let turns = call(
        &mut socket,
        2,
        "thread/turns/list",
        json!({"threadId": thread_id, "limit": 1}),
    )
    .await;
    assert_eq!(turns["result"]["data"], json!([]));
    let start = call(
        &mut socket,
        3,
        "turn/start",
        json!({"threadId": thread_id, "input": [{"type":"text","text":"hello desk"}]}),
    )
    .await;
    assert!(start["result"]["turn"]["id"].is_string(), "{start}");
    assert_eq!(
        tokio::task::block_in_place(|| next_line(&mut out)),
        "INPUT 1: hello desk"
    );
    assert_eq!(
        broadcast(&mut socket, "thread/status/changed").await["params"]["status"]["type"],
        json!("active")
    );
    assert_eq!(
        broadcast(&mut socket, "thread/status/changed").await["params"]["status"]["type"],
        json!("idle")
    );

    // Every other method is an error, and an unknown thread cannot be resumed.
    let unknown = call(&mut socket, 4, "thread/spork", json!({})).await;
    assert!(unknown.get("error").is_some(), "{unknown}");
    let resume = call(
        &mut socket,
        5,
        "thread/resume",
        json!({"threadId": "th-nope"}),
    )
    .await;
    assert!(resume.get("error").is_some(), "{resume}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_claude_half_reads_the_channel_and_runs_its_hooks() {
    let scratch = Scratch::new("claude");
    let hook_log = scratch.0.join("hooks.jsonl");
    let script = scratch.write("script.json", r#"{"hooks":true}"#);

    // `mcp.json` as the daemon writes it (§5.1), pointing the channel at the
    // stand-in's own tiny server: the real bridge would need a daemon.
    let mcp = scratch.write(
        "mcp.json",
        &json!({"mcpServers": {
            "marketrig-channel": {"command": STANDIN,
                                  "args": ["fake-channel", "MarketRig TRIGGER_RESULT p-1"],
                                  "env": {}},
        }})
        .to_string(),
    );
    // `settings.json` as the daemon writes it: one command per event, reading
    // the hook input object on standard input (§5.2).
    let hook = if cfg!(windows) {
        format!(
            "more >> \"{}\"",
            hook_log.display().to_string().replace('\\', "\\\\")
        )
    } else {
        format!("cat >> {}", hook_log.display())
    };
    let settings = scratch.write(
        "settings.json",
        &json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": hook}]}],
            "Notification": [{"hooks": [{"type": "command", "command": hook}]}],
            "Stop":         [{"hooks": [{"type": "command", "command": hook}]}],
        }})
        .to_string(),
    );

    let mut claude = Child(
        Command::new(STANDIN)
            .args([
                "--session-id",
                "11111111-2222-4333-8444-555555555555",
                "--mcp-config",
                &mcp.display().to_string(),
                "--settings",
                &settings.display().to_string(),
                "--dangerously-load-development-channels",
                "server:marketrig-channel",
            ])
            .env("MARKETRIG_STANDIN_SCRIPT", &script)
            .stdout(Stdio::piped())
            .spawn()
            .expect("the claude half starts"),
    );
    let mut out = BufReader::new(claude.0.stdout.take().expect("a piped stdout"));
    assert_eq!(
        tokio::task::block_in_place(|| next_line(&mut out)),
        "INPUT 1: MarketRig TRIGGER_RESULT p-1"
    );

    // The `Stop` hook follows the turn; `SessionStart` opened it.
    let mut hooks = Vec::new();
    for _ in 0..40 {
        // The hook commands append their input objects back to back, so the
        // log is a JSON stream rather than one object per line.
        let log = std::fs::read_to_string(&hook_log).unwrap_or_default();
        hooks = serde_json::Deserializer::from_str(&log)
            .into_iter::<Value>()
            .map_while(Result::ok)
            .collect();
        if hooks.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(hooks[0]["hook_event_name"], json!("SessionStart"));
    assert_eq!(hooks[0]["source"], json!("startup"));
    assert_eq!(
        hooks[0]["session_id"],
        json!("11111111-2222-4333-8444-555555555555")
    );
    assert_eq!(hooks[1]["hook_event_name"], json!("Stop"));

    // A resume of a session id the stand-in has never seen is exit 1 (§9.1).
    let unknown = Command::new(STANDIN)
        .args([
            "--resume",
            "99999999-2222-4333-8444-555555555555",
            "--mcp-config",
            &mcp.display().to_string(),
        ])
        .env("MARKETRIG_STANDIN_SCRIPT", &script)
        .status()
        .expect("the resume runs");
    assert_eq!(unknown.code(), Some(1));
}

// ---------------------------------------------------------------------------
// `memory-standin` (R4 feature SPEC §7.1)
// ---------------------------------------------------------------------------

/// The probe marker, the bearer, and one retain-then-recall round trip — the
/// whole of what the daemon asks of the child.
#[test]
fn the_memory_child_names_the_marker_and_holds_one_bank() {
    let help = Command::new(MEMORY).arg("--help").output().expect("--help");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    assert!(help.contains("HINDSIGHT_API_PORT"), "{help}");

    let scratch = Scratch::new("memory");
    let bearer = "0123456789abcdef";
    let port = free_port();
    let _child = Child(
        Command::new(MEMORY)
            .env("HINDSIGHT_API_HOST", "127.0.0.1")
            .env("HINDSIGHT_API_PORT", port.to_string())
            .env("HINDSIGHT_API_TENANT_API_KEY", bearer)
            .env("HOME", &scratch.0)
            .stdout(Stdio::null())
            .spawn()
            .expect("the memory child starts"),
    );
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if get(&format!("{base}/health"), None).0 == 200 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(get(&format!("{base}/health"), None).0, 200);
    // The bearer it was launched with, where the gate's secrets check finds it.
    assert_eq!(
        std::fs::read_to_string(scratch.0.join("bearers.txt"))
            .expect("bearers.txt")
            .trim(),
        bearer
    );

    let bank = format!("{base}/v1/default/banks/desk-abc/memories");
    let item = json!({"items": [{"content": "the desk waited out the open",
                                 "metadata": {"source": "INTERACTIVE"}}],
                      "async": false})
    .to_string();
    let (status, body) = post(&bank, None, &item);
    assert_eq!(status, 401, "{body}");
    assert_eq!(parse(&body)["detail"], json!("Invalid API key"));
    assert_eq!(post(&bank, Some("wrong"), &item).0, 401);

    let (status, body) = post(&bank, Some(bearer), &item);
    assert_eq!(status, 200, "{body}");
    assert_eq!(parse(&body)["items_count"], json!(1));

    let query = json!({"query": "waited open", "budget": "mid"}).to_string();
    let (status, body) = post(&format!("{bank}/recall"), Some(bearer), &query);
    assert_eq!(status, 200, "{body}");
    let first = parse(&body)["results"][0].clone();
    assert_eq!(first["text"], json!("the desk waited out the open"));
    assert_eq!(first["type"], json!("experience"));
    assert_eq!(first["metadata"]["source"], json!("INTERACTIVE"));
    assert!(first["id"].is_string() && !first["occurred_start"].is_null());

    // Another bank holds nothing of it, and reflect nests its citations.
    let elsewhere = format!("{base}/v1/default/banks/desk-def/memories/recall");
    assert_eq!(
        parse(&post(&elsewhere, Some(bearer), &query).1)["results"],
        json!([])
    );
    let (status, body) = post(
        &format!("{base}/v1/default/banks/desk-abc/reflect"),
        Some(bearer),
        &query,
    );
    assert_eq!(status, 200, "{body}");
    let reflected = parse(&body);
    assert_eq!(reflected["text"], json!("the desk waited out the open"));
    assert_eq!(reflected["based_on"]["memories"][0]["id"], first["id"]);

    assert_eq!(get(&format!("{base}/v1/nothing"), Some(bearer)).0, 404);
}

/// The provider half: the scripted list, and `models_error` flipped under a
/// server that is already listening (§3's stale-list scenario).
#[test]
fn the_provider_half_serves_a_live_model_list() {
    let scratch = Scratch::new("models");
    let script = scratch.write("script.json", &json!({"memory": {}}).to_string());
    let port = free_port();
    let _child = Child(
        Command::new(MEMORY)
            .args(["--models", &port.to_string()])
            .env("MARKETRIG_STANDIN_SCRIPT", &script)
            .spawn()
            .expect("the provider stand-in starts"),
    );
    let url = format!("http://127.0.0.1:{port}/v1/models");
    let mut answered = (0, String::new());
    for _ in 0..100 {
        answered = get(&url, None);
        if answered.0 == 200 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(answered.0, 200, "{}", answered.1);
    assert_eq!(
        parse(&answered.1)["data"][0]["id"],
        json!("stand-in-llm"),
        "{}",
        answered.1
    );

    std::fs::write(
        &script,
        json!({"memory": {"models_error": true}}).to_string(),
    )
    .expect("arm the failure");
    assert_eq!(get(&url, None).0, 500, "the knob is read per request");
    std::fs::write(
        &script,
        json!({"memory": {"models": ["only-one"]}}).to_string(),
    )
    .expect("disarm it");
    let (status, body) = get(&url, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        parse(&body)["data"],
        json!([{"id": "only-one", "object": "model"}])
    );
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// The gate reaches the daemon with `ureq`; these two reach the stand-in the
/// same way, statuses as data.
fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .http_status_as_error(false)
            .build(),
    )
}

fn get(url: &str, bearer: Option<&str>) -> (u16, String) {
    let agent = agent();
    let mut request = agent.get(url);
    if let Some(bearer) = bearer {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    match request.call() {
        Ok(mut response) => (
            response.status().as_u16(),
            response.body_mut().read_to_string().unwrap_or_default(),
        ),
        Err(e) => (0, e.to_string()),
    }
}

fn post(url: &str, bearer: Option<&str>, body: &str) -> (u16, String) {
    let agent = agent();
    let mut request = agent.post(url).header("content-type", "application/json");
    if let Some(bearer) = bearer {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    match request.send(body) {
        Ok(mut response) => (
            response.status().as_u16(),
            response.body_mut().read_to_string().unwrap_or_default(),
        ),
        Err(e) => (0, e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// The daemon's side of the app-server wire, restated (slice §2)
// ---------------------------------------------------------------------------

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, token: &str) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    for _ in 0..60 {
        let mut request = url.into_client_request().expect("a ws url");
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}").parse().expect("a header"),
        );
        if let Ok((socket, _)) = tokio_tungstenite::connect_async(request).await {
            return socket;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the stand-in app-server never accepted a connection");
}

/// The next frame carrying `id`, skipping broadcasts.
async fn call(socket: &mut Socket, id: i64, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .expect("the request goes");
    loop {
        let frame = frame_of(socket).await;
        if frame["id"].as_i64() == Some(id) {
            return frame;
        }
    }
}

/// The next broadcast of `method`, skipping everything else.
async fn broadcast(socket: &mut Socket, method: &str) -> Value {
    loop {
        let frame = frame_of(socket).await;
        if frame["method"] == method {
            return frame;
        }
    }
}

async fn frame_of(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("the app-server answers within ten seconds")
            .expect("the socket stays open")
            .expect("a websocket frame");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("a JSON frame");
        }
    }
}
