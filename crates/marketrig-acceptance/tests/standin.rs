//! The stand-ins' own checks: the halves the gate drives, end to end.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §9.1 (per R3-8) for
//! `runtime-standin`. The gate scenarios G27–G32 exercise it through the
//! daemon; these checks are the smaller thing that fails first when the
//! stand-in's own wire drifts from what `marketrigd` sends.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const STANDIN: &str = env!("CARGO_BIN_EXE_runtime-standin");

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

// ---------------------------------------------------------------------------
// `openviking-standin` (OpenViking feature SPEC §7.1, per OV-7)
// ---------------------------------------------------------------------------

const OPENVIKING: &str = env!("CARGO_BIN_EXE_openviking-standin");

/// The root key the harness mints per start (§2.2); any 32 bytes of hex.
const ROOT_KEY: &str = "6f1c0a2b3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8";

/// The one installation secret (§3.2).
const SEED: &str = "the-seed";

/// The key OpenViking's documented rule yields for `desk-abc` under account
/// `marketrig` with [`SEED`]: `b64url("marketrig") . b64url("desk-abc") .
/// b64url(sha256("desk-abc\0the-seed").hexdigest())`, padding stripped. Taken
/// from the rule, not from the stand-in, so a drift in either fails here.
const DESK_ABC_KEY: &str = "bWFya2V0cmln.ZGVzay1hYmM.\
NWU2NjZjZWQ2Y2ZmMWVjYWFiOWIyOGRjMGE2NzcxMmI5NGI4Yzk3ZGFiZjExMjM2OGYyNGY3M2NlNmQ2ZGExMQ";

/// One SKILL.md, the shape §5.3's seed and §5.2's projection both carry.
const SKILL: &str = "---\nname: desk-improvement\ndescription: Improve the desk\n---\n\nBody.\n";

/// A running stand-in and its client.
struct Openviking {
    child: Child,
    agent: ureq::Agent,
    base: String,
}

impl Openviking {
    /// Starts it the way the daemon does (§2.2): the rendered config, the root
    /// key behind the config's `${…}` reference only, and an explicit port.
    fn start(scratch: &Scratch, script: &str) -> Openviking {
        let workspace = scratch.0.join("data");
        let config = scratch.write(
            "ov.conf",
            &json!({
                "server": {
                    "host": "127.0.0.1",
                    "port": 0,
                    "root_api_key": "${MARKETRIG_OV_ROOT_KEY}",
                },
                "storage": {"workspace": workspace.display().to_string()},
            })
            .to_string(),
        );
        let script = scratch.write("script.json", script);
        let port = free_port();
        let child = Child(
            Command::new(OPENVIKING)
                .args([
                    "--config",
                    &config.display().to_string(),
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                ])
                .env("MARKETRIG_OV_ROOT_KEY", ROOT_KEY)
                .env("MARKETRIG_STANDIN_SCRIPT", &script)
                .spawn()
                .expect("the stand-in starts"),
        );
        let ov = Openviking {
            child,
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(5)))
                    .http_status_as_error(false)
                    .build(),
            ),
            base: format!("http://127.0.0.1:{port}"),
        };
        // Listening is not readiness: `/health` is the liveness probe, and a
        // scripted `ready_after_ms` is still counting down behind it.
        for _ in 0..100 {
            if ov.try_call("GET", "/health", "", None).is_some() {
                return ov;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the stand-in never listened");
    }

    /// One call: the status and the parsed envelope.
    #[track_caller]
    fn call(&self, method: &str, path: &str, key: &str, body: Option<Value>) -> (u16, Value) {
        self.try_call(method, path, key, body)
            .unwrap_or_else(|| panic!("the stand-in answers {method} {path}"))
    }

    fn try_call(
        &self,
        method: &str,
        path: &str,
        key: &str,
        body: Option<Value>,
    ) -> Option<(u16, Value)> {
        let url = format!("{}{path}", self.base);
        let bearer = format!("Bearer {key}");
        let sent = match method {
            "GET" | "DELETE" => {
                let request = match method {
                    "GET" => self.agent.get(&url),
                    _ => self.agent.delete(&url),
                };
                match key.is_empty() {
                    true => request.call(),
                    false => request.header("Authorization", bearer).call(),
                }
            }
            _ => {
                let request = match method {
                    "PUT" => self.agent.put(&url),
                    _ => self.agent.post(&url),
                }
                .header("Authorization", bearer)
                .header("content-type", "application/json");
                match body {
                    Some(body) => request.send(body.to_string()),
                    None => request.send_empty(),
                }
            }
        };
        let mut response = sent.ok()?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().ok()?;
        Some((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// Blocks until `/ready` answers 200, the daemon's own gate (§2.2).
    fn wait_ready(&self) {
        for _ in 0..100 {
            if self.call("GET", "/ready", "", None).0 == 200 {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the stand-in never became ready");
    }

    /// Account `marketrig` and one desk user, the sequence of §3.2.
    fn provision(&self, user: &str) -> String {
        let (status, _) = self.call(
            "POST",
            "/api/v1/admin/accounts",
            ROOT_KEY,
            Some(json!({"account_id": "marketrig", "admin_user_id": "marketrig-admin"})),
        );
        assert!(status == 200 || status == 409, "account: {status}");
        let (status, _) = self.call(
            "POST",
            "/api/v1/admin/accounts/marketrig/users",
            ROOT_KEY,
            Some(json!({"user_id": user, "role": "user"})),
        );
        assert!(status == 200 || status == 409, "user: {status}");
        let (status, answer) = self.call(
            "POST",
            &format!("/api/v1/admin/accounts/marketrig/users/{user}/key"),
            ROOT_KEY,
            Some(json!({"seed": SEED})),
        );
        assert_eq!(status, 200, "{answer}");
        answer["result"]["user_key"]
            .as_str()
            .expect("a user key")
            .to_string()
    }
}

#[test]
fn openviking_discovery_and_the_seeded_key_rule() {
    let version = Command::new(OPENVIKING)
        .arg("--version")
        .output()
        .expect("--version");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "openviking-server 0.4.17.1"
    );

    let scratch = Scratch::new("ov-keys");
    let ov = Openviking::start(&scratch, "{}");
    ov.wait_ready();
    assert_eq!(ov.call("GET", "/health", "", None).0, 200);

    // The seeded key is the documented rule, so the daemon's Retry gets the
    // same one back and every live session keeps working (OV-3).
    assert_eq!(ov.provision("desk-abc"), DESK_ABC_KEY);
    assert_eq!(ov.provision("desk-abc"), DESK_ABC_KEY);

    // `AlreadyExists` is HTTP 409 with the envelope's own code.
    let (status, answer) = ov.call(
        "POST",
        "/api/v1/admin/accounts",
        ROOT_KEY,
        Some(json!({"account_id": "marketrig", "admin_user_id": "marketrig-admin"})),
    );
    assert_eq!(
        (status, &answer["error"]["code"]),
        (409, &json!("ALREADY_EXISTS"))
    );
    let (status, answer) = ov.call(
        "POST",
        "/api/v1/admin/accounts/marketrig/users",
        ROOT_KEY,
        Some(json!({"user_id": "desk-abc", "role": "user"})),
    );
    assert_eq!(
        (status, &answer["error"]["code"]),
        (409, &json!("ALREADY_EXISTS"))
    );

    // Auth: no key and a wrong key are both 401; a desk key is no admin; and
    // ROOT has no user binding, so it reaches no tenant data at all.
    assert_eq!(ov.call("GET", "/api/v1/skills", "", None).0, 401);
    assert_eq!(ov.call("GET", "/api/v1/skills", "not-a-key", None).0, 401);
    assert_eq!(
        ov.call(
            "POST",
            "/api/v1/admin/accounts/marketrig/users",
            DESK_ABC_KEY,
            Some(json!({"user_id": "desk-def", "role": "user"})),
        )
        .0,
        403
    );
    assert_eq!(ov.call("GET", "/api/v1/skills", ROOT_KEY, None).0, 403);
    assert_eq!(ov.call("GET", "/api/v1/nope", DESK_ABC_KEY, None).0, 404);
    drop(ov.child);
}

#[test]
fn openviking_keeps_two_desk_users_apart() {
    let scratch = Scratch::new("ov-tenancy");
    let ov = Openviking::start(&scratch, "{}");
    ov.wait_ready();
    let a = ov.provision("desk-abc");
    let b = ov.provision("desk-def");
    assert_ne!(a, b);

    // The seed upload (§5.3), with one auxiliary file beside SKILL.md.
    let (status, _) = ov.call(
        "POST",
        "/api/v1/skills",
        &a,
        Some(json!({"data": SKILL, "files": {"notes.md": "aux"}})),
    );
    assert_eq!(status, 200);
    assert_eq!(
        ov.call("POST", "/api/v1/skills", &a, Some(json!({"data": SKILL})),)
            .0,
        409
    );

    // The listing and the detail, the two calls the projection makes (§5.2).
    let (_, listed) = ov.call("GET", "/api/v1/skills", &a, None);
    assert_eq!(listed["result"]["total"], json!(1));
    assert_eq!(
        listed["result"]["root_uris"],
        json!(["viking://user/desk-abc/skills", "viking://agent/skills"])
    );
    let skill = &listed["result"]["skills"][0];
    assert_eq!(skill["name"], json!("desk-improvement"));
    assert_eq!(skill["description"], json!("Improve the desk"));
    assert_eq!(skill["type"], json!("skill"));

    let (_, detail) = ov.call(
        "GET",
        "/api/v1/skills/desk-improvement?include_content=true&include_files=true",
        &a,
        None,
    );
    let detail = &detail["result"];
    assert_eq!(detail["content"], json!(SKILL));
    assert_eq!(detail["files"][0]["path"], json!("notes.md"));
    assert_eq!(detail["files"][0]["is_dir"], json!(false));
    let (_, read) = ov.call(
        "GET",
        &format!(
            "/api/v1/content/read?uri={}",
            detail["skill_md_uri"].as_str().expect("a uri")
        ),
        &a,
        None,
    );
    assert_eq!(read["result"], json!(SKILL));
    let (_, aux) = ov.call(
        "GET",
        &format!(
            "/api/v1/content/read?uri={}",
            detail["files"][0]["uri"].as_str().expect("a uri")
        ),
        &a,
        None,
    );
    assert_eq!(aux["result"], json!("aux"));

    // Capture: messages, the commit task, and the session read back.
    let (_, _) = ov.call(
        "POST",
        "/api/v1/sessions/s1/messages",
        &a,
        Some(json!({"role": "assistant", "content": "AAPL slipped on the open"})),
    );
    let (_, committed) = ov.call("POST", "/api/v1/sessions/s1/commit", &a, None);
    let task = committed["result"]["task_id"].as_str().expect("a task id");
    let (_, done) = ov.call("GET", &format!("/api/v1/tasks/{task}"), &a, None);
    assert_eq!(done["result"]["status"], json!("completed"));
    let (_, session) = ov.call("GET", "/api/v1/sessions/s1", &a, None);
    assert_eq!(session["result"]["messages"][0]["role"], json!("assistant"));

    let (_, found) = ov.call(
        "POST",
        "/api/v1/search/find",
        &a,
        Some(json!({"query": "SLIPPED ON THE OPEN"})),
    );
    assert_eq!(found["result"]["results"].as_array().map(Vec::len), Some(1));

    // Isolation: B's key reaches nothing of A's, by any route.
    let (_, listed) = ov.call("GET", "/api/v1/skills", &b, None);
    assert_eq!(listed["result"]["total"], json!(0));
    assert_eq!(
        ov.call("GET", "/api/v1/skills/desk-improvement", &b, None)
            .0,
        404
    );
    assert_eq!(
        ov.call(
            "GET",
            "/api/v1/content/read?uri=viking://user/desk-abc/skills/desk-improvement/SKILL.md",
            &b,
            None,
        )
        .0,
        403
    );
    assert_eq!(ov.call("GET", "/api/v1/sessions/s1", &b, None).0, 404);
    let (_, found) = ov.call(
        "POST",
        "/api/v1/search/find",
        &b,
        Some(json!({"query": "slipped on the open"})),
    );
    assert_eq!(found["result"]["results"], json!([]));

    // A's own writes still land, and the delete is visible in the listing.
    assert_eq!(
        ov.call(
            "PUT",
            "/api/v1/skills/desk-improvement",
            &a,
            Some(json!({"data": SKILL.replace("Body.", "Rewritten.")})),
        )
        .0,
        200
    );
    assert_eq!(
        ov.call("DELETE", "/api/v1/skills/desk-improvement", &a, None)
            .0,
        200
    );
    let (_, listed) = ov.call("GET", "/api/v1/skills", &a, None);
    assert_eq!(listed["result"]["total"], json!(0));
    drop(ov.child);
}

#[test]
fn openviking_exits_after_readiness_as_scripted() {
    let scratch = Scratch::new("ov-exit");
    let mut ov = Openviking::start(
        &scratch,
        // Two seconds, not two hundred milliseconds: the assertion below has to
        // beat the delay even when the machine is busy compiling something else.
        r#"{"openviking":{"ready_after_ms":2000,"exit_after_ready_ms":200,"exit_code":7}}"#,
    );

    // Before the delay elapses `/ready` is the real server's own refusal.
    let (status, answer) = ov.call("GET", "/ready", "", None);
    assert_eq!(
        (status, answer),
        (
            503,
            json!({"status": "not_ready", "reason": "initializing"})
        )
    );
    ov.wait_ready();

    let status = ov.child.0.wait().expect("the scripted exit");
    assert_eq!(status.code(), Some(7));
}
