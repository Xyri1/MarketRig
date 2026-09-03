//! `runtime-standin` — the gate's stand-in for Codex CLI and Claude Code.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §9.1, per R3-8. It
//! speaks exactly the subset the daemon's two adapters consume (§4, §5) and
//! nothing more: enough wire for G27–G32, never a second runtime.
//!
//! Every knob comes from the JSON file `MARKETRIG_STANDIN_SCRIPT` names, which
//! the gate writes per launch (§9.1). The Codex halves — the app-server and the
//! `--remote` TUI — are the same binary in two modes, and they talk to each
//! other over the one non-Codex method on the wire,
//! `marketrig/standin/input`, which is how the TUI learns what to echo.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

const HELP: &str = "\
runtime-standin — the MarketRig acceptance stand-in runtime

USAGE:
  runtime-standin app-server --listen <ws-url> --ws-auth capability-token --ws-token-file <path>
  runtime-standin [resume <thread-id>] --remote <ws-url> --remote-auth-token-env <var> -C <cwd>
  runtime-standin --session-id <uuid> | --resume <uuid>
      --mcp-config <path> --settings <path>
      --dangerously-load-development-channels server:marketrig-channel
";

// ---------------------------------------------------------------------------
// The script (§9.1)
// ---------------------------------------------------------------------------

/// One launch's knobs. Every field has a default, so an absent or unreadable
/// script is the plain happy path.
struct Script(Value);

impl Script {
    fn load() -> Script {
        let value = std::env::var_os("MARKETRIG_STANDIN_SCRIPT")
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| json!({}));
        Script(value)
    }

    fn flag(&self, key: &str) -> bool {
        self.0[key].as_bool() == Some(true)
    }

    fn ms(&self, key: &str) -> Duration {
        Duration::from_millis(self.0[key].as_u64().unwrap_or(0))
    }

    fn text(&self, key: &str) -> Option<String> {
        self.0[key].as_str().map(str::to_string)
    }

    fn count(&self, key: &str) -> Option<u64> {
        self.0[key].as_u64()
    }

    /// Where the Claude half remembers the session ids it has seen, so that
    /// `--resume` can tell a real pointer from an unknown one. Beside the
    /// script, because that is the one path both launches of a gate scenario
    /// share.
    fn sessions_ledger() -> Option<PathBuf> {
        let script = PathBuf::from(std::env::var_os("MARKETRIG_STANDIN_SCRIPT")?);
        Some(script.with_extension("sessions"))
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let script = Script::load();
    if args.iter().any(|a| a == "--version") {
        println!(
            "runtime-standin {}",
            script.text("version").unwrap_or_else(|| "99.0.0".into())
        );
        return;
    }
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    if args[0] == "app-server" {
        app_server(&args, script).await;
    } else if args.iter().any(|a| a == "--remote") {
        codex_tui(&args, script).await;
    } else if args[0] == "fake-channel" {
        // Test-only: the tiny stdio MCP server this crate's own check points
        // `mcp.json` at, because the real bridge needs a daemon.
        fake_channel(args.get(1).cloned().unwrap_or_default());
    } else {
        claude(&args, script).await;
    }
}

/// `--flag value` anywhere in the command line.
fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

// ---------------------------------------------------------------------------
// Codex: the app-server (§4.1, §4.3)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Thread {
    cwd: String,
    /// Turn id and status, newest last.
    turns: Vec<(String, String)>,
}

struct Server {
    token: String,
    script: Script,
    threads: Mutex<HashMap<String, Thread>>,
    next: AtomicU64,
    push: tokio::sync::broadcast::Sender<String>,
}

impl Server {
    fn broadcast(&self, method: &str, params: Value) {
        let _ = self
            .push
            .send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    fn id(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.next.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

async fn app_server(args: &[String], script: Script) {
    let listen = value_of(args, "--listen").unwrap_or_default();
    let port: u16 = listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let token = value_of(args, "--ws-token-file")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let server = Arc::new(Server {
        token: token.trim().to_string(),
        script,
        threads: Mutex::new(HashMap::new()),
        next: AtomicU64::new(0),
        push: tokio::sync::broadcast::channel(64).0,
    });

    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::http::HeaderMap;
    use axum::routing::any;
    let app = axum::Router::new().route(
        "/",
        any(async move |headers: HeaderMap, upgrade: WebSocketUpgrade| {
            let offered = headers
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .trim_start_matches("Bearer ")
                .to_string();
            if !server.token.is_empty() && offered != server.token {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
            let server = server.clone();
            upgrade.on_upgrade(async move |mut socket| {
                let mut pushes = server.push.subscribe();
                loop {
                    let frame = tokio::select! {
                        push = pushes.recv() => {
                            let Ok(text) = push else { continue };
                            if socket.send(Message::Text(text.into())).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        frame = socket.recv() => frame,
                    };
                    let Some(Ok(Message::Text(text))) = frame else {
                        return;
                    };
                    let Ok(request) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    // A notification (`initialized`) is acknowledged by silence.
                    let Some(id) = request.get("id").cloned() else {
                        continue;
                    };
                    let method = request["method"].as_str().unwrap_or_default();
                    if method == "turn/start" && server.script.flag("drop_socket_on_turn_start") {
                        return;
                    }
                    let reply = match handle(&server, method, &request["params"]).await {
                        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                        Err(message) => json!({"jsonrpc":"2.0","id":id,
                            "error":{"code":-32600,"message":message}}),
                    };
                    if socket
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            })
        }),
    );
    use axum::response::IntoResponse as _;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("the stand-in app-server binds its listener");
    axum::serve(listener, app).await.expect("serve");
}

/// The consumed subset of the app-server's methods; everything else is an
/// error, as §9.1 requires.
async fn handle(server: &Arc<Server>, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "initialize" => Ok(json!({"userAgent": "runtime-standin"})),
        "thread/start" => {
            let cwd = params["cwd"].as_str().unwrap_or_default().to_string();
            let id = server.id("th");
            server.threads.lock().expect("threads").insert(
                id.clone(),
                Thread {
                    cwd: cwd.clone(),
                    turns: Vec::new(),
                },
            );
            let thread = json!({"id": id, "cwd": cwd, "ephemeral": false,
                                "status": {"type": "idle"}});
            server.broadcast("thread/started", json!({"thread": thread}));
            Ok(json!({"thread": thread}))
        }
        "thread/resume" => {
            let id = params["threadId"].as_str().unwrap_or_default().to_string();
            let cwd = {
                let threads = server.threads.lock().expect("threads");
                let Some(thread) = threads.get(&id) else {
                    return Err(format!("no rollout found for thread id {id}"));
                };
                thread.cwd.clone()
            };
            // A resume reaches `idle` through the status broadcast only
            // (§4.2, spike S): no `thread/started` follows it.
            server.broadcast(
                "thread/status/changed",
                json!({"threadId": id, "status": {"type": "notLoaded"}}),
            );
            server.broadcast(
                "thread/status/changed",
                json!({"threadId": id, "status": {"type": "idle"}}),
            );
            Ok(json!({"thread": {"id": id, "cwd": cwd, "ephemeral": false,
                                 "status": {"type": "idle"}}}))
        }
        "thread/turns/list" => {
            let id = params["threadId"].as_str().unwrap_or_default();
            let threads = server.threads.lock().expect("threads");
            // The real app-server refuses the listing for a thread that has not
            // taken a turn yet (§9.1); scripted, so the gate covers the fall
            // through to `turn/start` that this used to block.
            if server.script.flag("turns_list_error_before_first_input")
                && threads.get(id).is_none_or(|thread| thread.turns.is_empty())
            {
                return Err(format!("thread {id} has no first message"));
            }
            let data: Vec<Value> = threads
                .get(id)
                .map(|thread| {
                    thread
                        .turns
                        .iter()
                        .rev()
                        .map(|(id, status)| json!({"id": id, "status": status}))
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!({"data": data}))
        }
        "turn/start" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let text = params["input"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let turn_id = server.id("turn");
            {
                let mut threads = server.threads.lock().expect("threads");
                let Some(thread) = threads.get_mut(&thread_id) else {
                    return Err(format!("unknown thread {thread_id}"));
                };
                thread
                    .turns
                    .push((turn_id.clone(), "inProgress".to_string()));
            }
            server.broadcast(
                "marketrig/standin/input",
                json!({"threadId": thread_id, "text": text}),
            );
            server.broadcast(
                "thread/status/changed",
                json!({"threadId": thread_id, "status": {"type": "active"}}),
            );
            let idle = {
                let server = server.clone();
                let thread_id = thread_id.clone();
                let turn_id = turn_id.clone();
                let after = server.script.ms("active_after_input_ms");
                async move {
                    tokio::time::sleep(after).await;
                    complete(&server, &thread_id, &turn_id);
                }
            };
            tokio::spawn(idle);
            tokio::time::sleep(server.script.ms("delay_turn_start_response_ms")).await;
            Ok(json!({"turn": {"id": turn_id}}))
        }
        "turn/interrupt" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let turn_id = params["turnId"].as_str().unwrap_or_default().to_string();
            complete(server, &thread_id, &turn_id);
            Ok(json!({}))
        }
        other => Err(format!("unknown method {other}")),
    }
}

/// Ends a turn and reopens the gate.
fn complete(server: &Arc<Server>, thread_id: &str, turn_id: &str) {
    {
        let mut threads = server.threads.lock().expect("threads");
        if let Some(thread) = threads.get_mut(thread_id)
            && let Some(turn) = thread.turns.iter_mut().find(|(id, _)| id == turn_id)
        {
            turn.1 = "completed".to_string();
        }
    }
    server.broadcast(
        "thread/status/changed",
        json!({"threadId": thread_id, "status": {"type": "idle"}}),
    );
}

// ---------------------------------------------------------------------------
// Codex: the `--remote` TUI (§4.2)
// ---------------------------------------------------------------------------

async fn codex_tui(args: &[String], script: Script) {
    if script.flag("exit_before_ready") {
        eprintln!("runtime-standin: exiting before readiness");
        std::process::exit(1);
    }
    let url = value_of(args, "--remote").unwrap_or_default();
    let cwd = value_of(args, "-C").unwrap_or_default();
    let resume = (args.first().map(String::as_str) == Some("resume"))
        .then(|| args.get(1).cloned().unwrap_or_default());
    let token = value_of(args, "--remote-auth-token-env")
        .and_then(|name| std::env::var(name).ok())
        .unwrap_or_default();

    let mut socket = connect(&url, &token).await;
    tokio::time::sleep(script.ms("ready_after_ms")).await;
    let _ = call(
        &mut socket,
        1,
        "initialize",
        json!({"clientInfo": {"name": "runtime-standin"}}),
    )
    .await;
    let start = match &resume {
        Some(thread) => call(&mut socket, 2, "thread/resume", json!({"threadId": thread})).await,
        None => call(&mut socket, 2, "thread/start", json!({"cwd": cwd})).await,
    };
    let Ok(started) = start else {
        eprintln!("runtime-standin: the thread could not be started");
        std::process::exit(1);
    };
    let thread_id = started["thread"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    if let Some(uri) = script.text("mcp_read") {
        match codex_registration(Path::new(&cwd)) {
            Some((command, args, env)) => mcp_read(&command, &args, &env, &uri),
            None => println!("MCP_READ_ERROR {uri}: no .codex/config.toml registration"),
        }
    }

    // Every delivered input, in order, on the PTY (§9.1).
    use futures_util::StreamExt as _;
    let mut seen = 0u64;
    while let Some(Ok(message)) = socket.next().await {
        let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
            continue;
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if frame["method"] == "marketrig/standin/input"
            && frame["params"]["threadId"] == thread_id.as_str()
        {
            seen += 1;
            say(&format!(
                "INPUT {seen}: {}",
                frame["params"]["text"].as_str().unwrap_or_default()
            ));
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, token: &str) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    for _ in 0..60 {
        let mut request = url.into_client_request().expect("a ws url");
        if !token.is_empty() {
            request.headers_mut().insert(
                "Authorization",
                format!("Bearer {token}").parse().expect("a header"),
            );
        }
        if let Ok((socket, _)) = tokio_tungstenite::connect_async(request).await {
            return socket;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    eprintln!("runtime-standin: cannot reach {url}");
    std::process::exit(1);
}

/// One request, and the next response carrying its id; broadcasts arriving
/// meanwhile are ignored, because nothing is delivered before readiness.
async fn call(socket: &mut Socket, id: i64, method: &str, params: Value) -> Result<Value, String> {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if frame["id"].as_i64() != Some(id) {
            continue;
        }
        return match frame.get("error") {
            Some(error) => Err(error["message"].as_str().unwrap_or("error").to_string()),
            None => Ok(frame["result"].clone()),
        };
    }
    Err("the connection closed".to_string())
}

/// One registered MCP server: the command, its arguments, and its environment.
type Registration = (String, Vec<String>, Vec<(String, String)>);

/// `<workspace>/.codex/config.toml`'s `[mcp_servers.marketrig]` — the three
/// keys the daemon writes (§4.2), read back without a TOML parser because
/// nothing else is ever in that file.
fn codex_registration(workspace: &Path) -> Option<Registration> {
    let text = std::fs::read_to_string(workspace.join(".codex").join("config.toml")).ok()?;
    let mut command = None;
    let mut args = Vec::new();
    let mut env = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "command" => command = serde_json::from_str::<String>(value).ok(),
            "args" => args = serde_json::from_str::<Vec<String>>(value).unwrap_or_default(),
            "env" => {
                // `{ KEY = "value" }`, one pair, written only under the seam.
                let inner = value.trim_matches(['{', '}']).trim();
                if let Some((name, literal)) = inner.split_once('=')
                    && let Ok(literal) = serde_json::from_str::<String>(literal.trim())
                {
                    env.push((name.trim().to_string(), literal));
                }
            }
            _ => {}
        }
    }
    Some((command?, args, env))
}

// ---------------------------------------------------------------------------
// Claude Code (§5)
// ---------------------------------------------------------------------------

async fn claude(args: &[String], script: Script) {
    let hooks = value_of(args, "--settings").filter(|_| script.flag("hooks"));
    let resume = value_of(args, "--resume");
    let mut session_id = match &resume {
        Some(id) => {
            if !ledger_has(id) {
                eprintln!("runtime-standin: no conversation found with session ID {id}");
                std::process::exit(1);
            }
            id.clone()
        }
        None => value_of(args, "--session-id").unwrap_or_default(),
    };
    ledger_add(&session_id);
    if script.flag("exit_before_ready") {
        eprintln!("runtime-standin: exiting before readiness");
        std::process::exit(1);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    tokio::time::sleep(script.ms("ready_after_ms")).await;

    let source = if resume.is_some() {
        "resume"
    } else {
        "startup"
    };
    run_hook(
        hooks.as_deref(),
        "SessionStart",
        json!({"hook_event_name": "SessionStart", "session_id": session_id,
               "source": source, "cwd": cwd.display().to_string()}),
    );
    if let Some(title) = script.text("notification") {
        run_hook(
            hooks.as_deref(),
            "Notification",
            json!({"hook_event_name": "Notification", "session_id": session_id,
                   "notification_type": "permission", "title": title,
                   "message": "the stand-in is asking"}),
        );
    }

    let servers = value_of(args, "--mcp-config")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));
    let launch = |name: &str| -> Option<Registration> {
        let server = servers["mcpServers"].get(name)?;
        let env = server["env"]
            .as_object()
            .map(|env| {
                env.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Some((
            server["command"].as_str()?.to_string(),
            server["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .map(|a| a.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default(),
            env,
        ))
    };

    if let Some(uri) = script.text("mcp_read") {
        match launch("marketrig") {
            Some((command, args, env)) => mcp_read(&command, &args, &env, &uri),
            None => println!("MCP_READ_ERROR {uri}: mcp.json registers no marketrig server"),
        }
    }

    let Some((command, args, env)) = launch("marketrig-channel") else {
        eprintln!("runtime-standin: mcp.json registers no channel server");
        std::process::exit(1);
    };
    let mut child = stdio_client(&command, &args, &env, false);
    // Claude Code attaches a channel only after its own `initialized`, a beat
    // after `initialize` answers, and silently drops anything pushed in
    // between (§9.1). The stand-in reproduces that gap, so a bridge that
    // connects before `initialized` loses the first delivery here too.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut dropped = 0u64;
    while let Ok(line) = child.lines.try_recv() {
        if line.contains("notifications/claude/channel") {
            dropped += 1;
        }
    }
    if dropped > 0 {
        say(&format!("DROPPED_BEFORE_INITIALIZED {dropped}"));
    }
    child.initialized();
    let mut seen = 0u64;
    while let Some(frame) = child.next() {
        if frame["method"] != "notifications/claude/channel" {
            continue;
        }
        seen += 1;
        say(&format!(
            "INPUT {seen}: {}",
            frame["params"]["content"].as_str().unwrap_or_default()
        ));
        tokio::time::sleep(script.ms("active_after_input_ms")).await;
        run_hook(
            hooks.as_deref(),
            "Stop",
            json!({"hook_event_name": "Stop", "session_id": session_id}),
        );
        if script.count("clear_after_inputs") == Some(seen) {
            session_id = format!("{session_id}-cleared");
            ledger_add(&session_id);
            run_hook(
                hooks.as_deref(),
                "SessionStart",
                json!({"hook_event_name": "SessionStart", "session_id": session_id,
                       "source": "clear", "cwd": cwd.display().to_string()}),
            );
        }
    }
}

fn ledger_add(session_id: &str) {
    if let Some(path) = Script::sessions_ledger()
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{session_id}");
    }
}

fn ledger_has(session_id: &str) -> bool {
    Script::sessions_ledger()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| text.lines().any(|line| line == session_id))
}

/// Runs every command the settings file lists for `event`, handing it the hook
/// input object on standard input (§5.2). The command is one line, so the
/// platform shell runs it.
fn run_hook(settings: Option<&str>, event: &str, input: Value) {
    let Some(settings) = settings else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(settings) else {
        return;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(matchers) = settings["hooks"][event].as_array() else {
        return;
    };
    for matcher in matchers {
        for hook in matcher["hooks"].as_array().unwrap_or(&Vec::new()) {
            let Some(command) = hook["command"].as_str() else {
                continue;
            };
            // The platform shell, handed the command line as one string. On
            // Windows `arg` would quote it and escape its inner quotes, and
            // `cmd /C` strips only the outer pair, so a redirection target
            // like `>> "C:\…\hooks.jsonl"` arrives as `\"C:\…\"` and fails.
            #[cfg(windows)]
            let mut shell = {
                use std::os::windows::process::CommandExt as _;
                let mut shell = std::process::Command::new("cmd");
                shell.raw_arg(format!("/C {command}"));
                shell
            };
            #[cfg(not(windows))]
            let mut shell = {
                let mut shell = std::process::Command::new("/bin/sh");
                shell.arg("-c").arg(command);
                shell
            };
            let Ok(mut child) = shell.stdin(Stdio::piped()).spawn() else {
                continue;
            };
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            drop(child.stdin.take());
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// The stdio MCP client both halves read the adapter through
// ---------------------------------------------------------------------------

/// A newline-framed JSON-RPC child, initialized. Kept as a struct only so the
/// child is killed when the reader is dropped.
struct StdioClient {
    child: std::process::Child,
    /// The child's output lines, read by a thread so the gap before
    /// `initialized` can be drained without blocking on the next one.
    lines: std::sync::mpsc::Receiver<String>,
    next: i64,
}

impl StdioClient {
    fn send(&mut self, frame: Value) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, "{frame}");
            let _ = stdin.flush();
        }
    }

    /// The next frame, or `None` when the child's output ends.
    fn next(&mut self) -> Option<Value> {
        loop {
            let line = self.lines.recv().ok()?;
            if let Ok(frame) = serde_json::from_str::<Value>(&line) {
                return Some(frame);
            }
        }
    }

    fn initialized(&mut self) {
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn call(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let frame = self.next()?;
            if frame["id"].as_i64() == Some(id) {
                return Some(frame);
            }
        }
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns the server and completes MCP `initialize` — advertising nothing,
/// because the stand-in offers no client capability at all. `initialized`
/// follows at once unless the caller wants the gap Claude Code leaves.
fn stdio_client(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    initialized: bool,
) -> StdioClient {
    let mut child = std::process::Command::new(command);
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        child.env(key, value);
    }
    let mut child = child.spawn().unwrap_or_else(|error| {
        eprintln!("runtime-standin: cannot spawn {command}: {error}");
        std::process::exit(1);
    });
    let stdout = child.stdout.take().expect("a piped stdout");
    let (tx, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut client = StdioClient {
        child,
        lines,
        next: 1,
    };
    client.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "runtime-standin", "version": "99.0.0"}}),
    );
    if initialized {
        client.initialized();
    }
    client
}

/// Reads one resource through the registered adapter and echoes it (§9.1).
fn mcp_read(command: &str, args: &[String], env: &[(String, String)], uri: &str) {
    let mut client = stdio_client(command, args, env, true);
    let answer = client.call("resources/read", json!({"uri": uri}));
    match answer.as_ref().and_then(|frame| {
        frame["result"]["contents"][0]["text"]
            .as_str()
            .filter(|_| frame.get("error").is_none())
    }) {
        Some(text) => {
            let head: String = text.chars().take(80).collect();
            say(&format!("MCP_READ {uri}: {head}"));
        }
        None => say(&format!(
            "MCP_READ_ERROR {uri}: {}",
            answer
                .map(|f| f["error"]["message"].to_string())
                .unwrap_or_default()
        )),
    }
}

/// One line on the PTY, flushed: the gate reads the attachment as it arrives.
fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// Test-only: a stdio MCP server that answers `initialize` and then pushes one
/// `notifications/claude/channel` carrying `content`. The real bridge needs a
/// daemon, which this crate's own check has no business starting.
fn fake_channel(content: String) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = frame.get("id") {
            let _ = writeln!(
                out,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18",
                       "capabilities":{},"serverInfo":{"name":"fake-channel","version":"0"}}})
            );
        } else if frame["method"] == "notifications/initialized" {
            let _ = writeln!(
                out,
                "{}",
                json!({"jsonrpc":"2.0","method":"notifications/claude/channel",
                       "params":{"content": content,
                                 "meta":{"prompt_id":"p-1","kind":"TRIGGER_RESULT"}}})
            );
        }
        let _ = out.flush();
    }
}
