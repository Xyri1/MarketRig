//! The Codex adapter: one contained app-server, one remote TUI per desk, and
//! delivery through `turn/start` behind an idle gate.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §4 (per R3-3), root
//! `sdd/SPEC.md` §6.3, §6.4, §11. The dispatcher (C27) owns the rows and the
//! FIFO; this module owns the protocol.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::desk;
use crate::session::{Activation, Adapter, AdapterEvent, AdapterEvents, DeliverOutcome};
use crate::store::{Roots, Store, now_ns};
use crate::terminal;

/// The app-server capability token, rewritten at every control-plane start (§4.1).
const TOKEN_FILE: &str = "codex-ws-token";
/// How long the daemon waits for the freshly spawned app-server to accept a
/// connection, retrying every [`CONNECT_RETRY`] (§4.1).
const CONNECT_DEADLINE: Duration = Duration::from_secs(15);
const CONNECT_RETRY: Duration = Duration::from_millis(250);
/// A request the app-server never answers is an uncertain handoff, not a hang.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The JSON-RPC connection (§4.1)
// ---------------------------------------------------------------------------

enum CallError {
    /// The connection went before the response: the handoff is unknown.
    Lost,
    /// The app-server answered with a JSON-RPC error.
    Rpc(String),
}

/// One live connection to the app-server. `serde_json::Value` frames, an
/// `AtomicI64` id, and a pending map — no JSON-RPC crate (slice §2).
struct Client {
    next_id: AtomicI64,
    out: mpsc::UnboundedSender<Message>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    alive: AtomicBool,
}

impl Client {
    async fn call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending").insert(id, tx);
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self
            .out
            .send(Message::Text(frame.to_string().into()))
            .is_err()
        {
            return Err(CallError::Lost);
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(message))) => Err(CallError::Rpc(message)),
            // The reader dropped the sender, or nothing came back at all.
            Ok(Err(_)) | Err(_) => Err(CallError::Lost),
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let frame = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = self.out.send(Message::Text(frame.to_string().into()));
    }
}

// ---------------------------------------------------------------------------
// What the adapter knows about the app-server's threads (§4.1)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Threads {
    /// The desk's thread: the pointer, known at spawn for a resume and
    /// discovered from `thread/started` for a new session.
    by_desk: HashMap<String, String>,
    /// Desks with a Codex terminal this adapter started, by child pid; the
    /// pointer map above outlives a session, this does not.
    live: HashMap<String, u32>,
    /// Workspaces whose `thread/started` has not arrived yet, by canonical path.
    awaiting: HashMap<PathBuf, String>,
    /// The thread's last broadcast status; the delivery gate reads it.
    status: HashMap<String, String>,
    /// Desks already reported ready, so readiness is announced once.
    ready: HashSet<String>,
}

impl Threads {
    fn desk_of(&self, thread_id: &str) -> Option<String> {
        self.by_desk
            .iter()
            .find(|(_, id)| id.as_str() == thread_id)
            .map(|(desk, _)| desk.clone())
    }
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

struct Inner {
    store: Store,
    roots: Roots,
    terminals: Arc<terminal::Manager>,
    events: AdapterEvents,
    /// The `PATH` the daemon captured at start, handed to every TUI.
    search_path: String,
    /// The `marketrig-mcp` binary `<workspace>/.codex/config.toml` registers.
    mcp_path: PathBuf,
    /// `MARKETRIG_TEST_DATA_ROOT`, propagated to the adapter child under the
    /// test seam only (§4.2, root §17).
    test_data_root: Option<PathBuf>,
    /// Test seam: connect here instead of spawning an app-server.
    connect_url: Option<String>,
    /// Where the live control plane listens, handed to every TUI as `--remote`.
    url: Mutex<String>,
    /// The live control plane's capability token, the one secret the TUI needs
    /// (§4.2). Written by `start_inner`; empty only under the test seam, where
    /// the socket takes no auth.
    token: Mutex<String>,
    /// This daemon's UUID, stamped on the app-server's `children.json` record.
    daemon_uuid: String,
    control: tokio::sync::Mutex<Option<Arc<Client>>>,
    child: Mutex<Option<crate::exec::Contained>>,
    threads: Mutex<Threads>,
    /// Control-plane restarts since the last `POST /runtimes/codex/retry`; the
    /// second failure is `CONTROL_PLANE_FAILED` until that retry clears both
    /// the row and this count (§4.1).
    restarts: AtomicU32,
}

/// The Codex runtime adapter. Cheap to clone; one per daemon.
#[derive(Clone)]
pub struct Codex(Arc<Inner>);

impl Codex {
    /// `mcp_path` is the `marketrig-mcp` binary beside the daemon;
    /// `search_path` is the `PATH` discovery captured (`ApiState::search_path`).
    /// The Codex executable and its availability are read from the `runtimes`
    /// row at every use, so a `retry` takes effect without a restart.
    // Eight plainly-named things the daemon has and the adapter needs; a struct
    // of them would be the same eight with one more name.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        roots: Roots,
        terminals: Arc<terminal::Manager>,
        events: AdapterEvents,
        search_path: String,
        mcp_path: PathBuf,
        test_data_root: Option<PathBuf>,
        daemon_uuid: String,
    ) -> Codex {
        Codex(Arc::new(Inner {
            store,
            roots,
            terminals,
            events,
            search_path,
            mcp_path,
            test_data_root,
            connect_url: None,
            url: Mutex::new(String::new()),
            token: Mutex::new(String::new()),
            daemon_uuid,
            control: tokio::sync::Mutex::new(None),
            child: Mutex::new(None),
            threads: Mutex::new(Threads::default()),
            restarts: AtomicU32::new(0),
        }))
    }

    /// Quit's stop of the app-server, after every terminal (§4.1).
    pub async fn stop(&self) {
        *self.0.control.lock().await = None;
        let child = self.0.child.lock().expect("child").take();
        if let Some(mut child) = child {
            child.terminate().await;
        }
    }

    async fn client(&self) -> Result<Arc<Client>, String> {
        let mut slot = self.0.control.lock().await;
        if let Some(client) = slot.as_ref()
            && client.alive.load(Ordering::SeqCst)
        {
            return Ok(client.clone());
        }
        let client = self.0.clone().start().await?;
        *slot = Some(client.clone());
        Ok(client)
    }
}

impl Inner {
    fn emit(&self, event: AdapterEvent) {
        let _ = self.events.send(event);
    }

    fn event_row(&self, kind: &str, payload: Value) {
        let kind = kind.to_string();
        let _ = self
            .store
            .unit(move |tx| desk::append_event(tx, &kind, None, now_ns(), payload));
    }

    /// The `runtimes.codex` executable, or the reason there is none.
    fn executable(&self) -> Result<PathBuf, String> {
        match crate::runtime::get(&self.store, "codex").map_err(|e| e.to_string())? {
            Some(row) if row.state == "AVAILABLE" => row
                .executable_path
                .map(PathBuf::from)
                .ok_or_else(|| "codex has no executable path".to_string()),
            Some(row) => Err(format!("codex is {}", row.state)),
            None => Err("codex is not a known runtime".to_string()),
        }
    }

    /// Starts (or, under the test seam, just connects to) the control plane and
    /// completes `initialize` / `initialized` (§4.1).
    fn start(
        self: Arc<Self>,
    ) -> futures_util::future::BoxFuture<'static, Result<Arc<Client>, String>> {
        // Boxed because the loss path restarts: `start` -> `lost` -> `start`.
        Box::pin(self.start_inner())
    }

    async fn start_inner(self: Arc<Self>) -> Result<Arc<Client>, String> {
        let (url, token, pid) = match &self.connect_url {
            Some(url) => (url.clone(), String::new(), 0),
            None => {
                let executable = self.executable()?;
                let port = free_port()?;
                let token = self.write_token()?;
                let url = format!("ws://127.0.0.1:{port}");
                let token_path = self.roots.runtime().join(TOKEN_FILE);
                let args = vec![
                    "app-server".to_string(),
                    "--listen".to_string(),
                    url.clone(),
                    "--ws-auth".to_string(),
                    "capability-token".to_string(),
                    "--ws-token-file".to_string(),
                    token_path.display().to_string(),
                ];
                let mut command = tokio::process::Command::new(&executable);
                command
                    .args(&args)
                    .current_dir(&self.roots.data)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let child = crate::exec::spawn(command).map_err(|e| e.to_string())?;
                let pid = child.id().unwrap_or_default();
                *self.child.lock().expect("child") = Some(child);
                crate::daemon::record_child(
                    &self.roots,
                    crate::daemon::ChildRecord {
                        pid,
                        kind: "codex-app-server".to_string(),
                        args,
                        daemon_uuid: self.daemon_uuid.clone(),
                        launched_at_ns: now_ns(),
                    },
                );
                (url, token, pid)
            }
        };

        *self.url.lock().expect("url") = url.clone();
        *self.token.lock().expect("token") = token.clone();
        let stream = connect(&url, &token).await?;
        let (mut sink, mut source) = {
            use futures_util::StreamExt as _;
            stream.split()
        };
        let (out, mut outbox) = mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            use futures_util::SinkExt as _;
            while let Some(message) = outbox.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
        });
        let client = Arc::new(Client {
            next_id: AtomicI64::new(1),
            out,
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
        });

        let reader = {
            let inner = self.clone();
            let client = client.clone();
            tokio::spawn(async move {
                use futures_util::StreamExt as _;
                while let Some(Ok(message)) = source.next().await {
                    let Message::Text(text) = message else {
                        continue;
                    };
                    let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    inner.frame(&client, &frame);
                }
                client.alive.store(false, Ordering::SeqCst);
                client.pending.lock().expect("pending").clear();
                inner.lost().await;
            })
        };
        drop(reader);

        client
            .call(
                "initialize",
                json!({"clientInfo": {"name": "marketrigd", "version": env!("CARGO_PKG_VERSION")}}),
            )
            .await
            .map_err(|_| "the app-server refused initialize".to_string())?;
        client.notify("initialized", json!({}));
        self.event_row(
            "CONTROL_PLANE_STARTED",
            json!({"runtime": "codex", "pid": pid, "port": port_of(&url)}),
        );
        Ok(client)
    }

    /// A fresh 0600 capability token under the data root (§4.1). The token is a
    /// secret: it reaches the file, the app-server, and the TUI's environment,
    /// and nothing else.
    fn write_token(&self) -> Result<String, String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
        let token = bytes.iter().fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        });
        let path = self.roots.runtime().join(TOKEN_FILE);
        std::fs::write(&path, &token).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        Ok(token)
    }

    /// One frame from the app-server: a response resolves its caller, anything
    /// else is one of §4.1's broadcasts.
    fn frame(&self, client: &Client, frame: &Value) {
        if let Some(id) = frame.get("id").and_then(Value::as_i64) {
            let waiting = client.pending.lock().expect("pending").remove(&id);
            if let Some(waiting) = waiting {
                let _ = waiting.send(match frame.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("error").to_string()),
                    None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                });
            }
            return;
        }
        let Some(method) = frame["method"].as_str() else {
            return;
        };
        let params = &frame["params"];
        match method {
            "thread/started" => {
                let thread = &params["thread"];
                let (Some(id), Some(cwd)) = (thread["id"].as_str(), thread["cwd"].as_str()) else {
                    return;
                };
                if thread["ephemeral"].as_bool() == Some(true) {
                    return;
                }
                let claimed = {
                    let mut threads = self.threads.lock().expect("threads");
                    threads.awaiting.remove(&canonical(Path::new(cwd)))
                };
                if let Some(desk_id) = claimed {
                    self.threads
                        .lock()
                        .expect("threads")
                        .by_desk
                        .insert(desk_id.clone(), id.to_string());
                    self.emit(AdapterEvent::PointerDiscovered {
                        desk_id,
                        native_session_id: id.to_string(),
                    });
                }
                // A new session's only `idle` is the one this broadcast carries
                // (spike S, 2026-09-03): no `thread/status/changed` follows it.
                if let Some(status) = thread["status"]["type"].as_str() {
                    self.status(id, status);
                }
            }
            "thread/status/changed" => {
                let (Some(id), Some(status)) = (
                    params["threadId"].as_str(),
                    params["status"]["type"].as_str(),
                ) else {
                    return;
                };
                self.status(id, status);
            }
            "thread/closed" => {
                if let Some(id) = params["threadId"].as_str() {
                    self.status(id, "closed");
                }
            }
            "error" => {
                if let Some(desk_id) = params["threadId"]
                    .as_str()
                    .and_then(|id| self.threads.lock().expect("threads").desk_of(id))
                {
                    self.emit(AdapterEvent::Attention {
                        desk_id,
                        kind: "error".to_string(),
                        detail: json!({"message": params["message"].as_str()}),
                    });
                }
            }
            _ => {}
        }
    }

    /// The gate's memory, and the one place readiness is decided (§4.1, §4.2).
    fn status(&self, thread_id: &str, status: &str) {
        let mut threads = self.threads.lock().expect("threads");
        threads
            .status
            .insert(thread_id.to_string(), status.to_string());
        let Some(desk_id) = threads.desk_of(thread_id) else {
            return;
        };
        let ready = status == "idle" && threads.ready.insert(desk_id.clone());
        drop(threads);
        if ready {
            self.emit(AdapterEvent::Ready {
                desk_id: desk_id.clone(),
            });
        }
        if status == "systemError" {
            self.emit(AdapterEvent::Attention {
                desk_id,
                kind: "system_error".to_string(),
                detail: json!({}),
            });
        }
    }

    /// The control plane went (§4.1): every Codex session ends
    /// `CONTROL_PLANE_LOST` with its pointer kept, and the start is retried
    /// once before the runtime is marked unavailable.
    async fn lost(self: Arc<Self>) {
        let desks: Vec<(String, u32)> = {
            let mut threads = self.threads.lock().expect("threads");
            threads.ready.clear();
            threads.awaiting.clear();
            threads.live.drain().collect()
        };
        self.event_row(
            "CONTROL_PLANE_LOST",
            json!({"runtime": "codex", "desks": desks.len()}),
        );
        for (desk_id, pid) in desks {
            self.terminals.shutdown_pid(&desk_id, pid);
            self.emit(AdapterEvent::Exited {
                desk_id,
                reason: "CONTROL_PLANE_LOST",
                code: None,
            });
        }
        let mut control = self.control.lock().await;
        *control = None;
        if self.restarts.fetch_add(1, Ordering::SeqCst) >= 1 {
            self.unavailable("the control plane was lost twice");
            return;
        }
        match self.clone().start().await {
            Ok(client) => *control = Some(client),
            Err(message) => self.unavailable(&message),
        }
    }

    fn unavailable(&self, message: &str) {
        let _ =
            crate::runtime::mark_unavailable(&self.store, "codex", "CONTROL_PLANE_FAILED", message);
    }

    /// `<workspace>/.codex/config.toml` with the bundled adapter registered:
    /// `-c mcp_servers.*` on the TUI command line does not reach the remote
    /// thread (spike S), so the app-server reads it from the workspace.
    fn write_config(&self, workspace: &Path, desk_id: &str) -> Result<(), String> {
        let dir = workspace.join(".codex");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let quote = |s: &str| Value::String(s.to_string()).to_string();
        let mut toml = format!(
            "[mcp_servers.marketrig]\ncommand = {}\nargs = [\"--desk\", {}]\n",
            quote(&self.mcp_path.display().to_string()),
            quote(desk_id),
        );
        if let Some(root) = &self.test_data_root {
            toml.push_str(&format!(
                "env = {{ MARKETRIG_TEST_DATA_ROOT = {} }}\n",
                quote(&root.display().to_string()),
            ));
        }
        std::fs::write(dir.join("config.toml"), toml).map_err(|e| e.to_string())
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn port_of(url: &str) -> u16 {
    url.rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
}

/// The port the app-server is told to listen on: bound, taken, released. A
/// bind race surfaces as the connect deadline (slice §2).
pub(crate) fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| e.to_string())
}

type Stream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connects within 15 seconds, retrying every 250 ms, carrying the capability
/// token as a bearer header (§4.1).
async fn connect(url: &str, token: &str) -> Result<Stream, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
    loop {
        let mut request = url.into_client_request().map_err(|e| e.to_string())?;
        if !token.is_empty() {
            let value = format!("Bearer {token}").parse().map_err(|_| "bad token")?;
            request.headers_mut().insert("Authorization", value);
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok((stream, _)) => return Ok(stream),
            Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.to_string()),
            Err(_) => tokio::time::sleep(CONNECT_RETRY).await,
        }
    }
}

// ---------------------------------------------------------------------------
// The Adapter surface (§4.2, §4.3)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl Adapter for Codex {
    async fn spawn(&self, desk_id: &str, resume: Option<&str>) -> Result<Activation, String> {
        let inner = &self.0;
        let desk = desk::get(&inner.store, desk_id).map_err(|e| e.to_string())?;
        let workspace = PathBuf::from(&desk.workspace_path);
        let client = self.client().await?;
        drop(client);
        let executable = inner.executable()?;
        inner.write_config(&workspace, desk_id)?;

        let url = inner.url.lock().expect("url").clone();
        let mut argv = vec![executable.display().to_string()];
        if let Some(thread) = resume {
            argv.push("resume".to_string());
            argv.push(thread.to_string());
        }
        argv.extend([
            "--remote".to_string(),
            url.clone(),
            "--remote-auth-token-env".to_string(),
            "MARKETRIG_CODEX_WS_TOKEN".to_string(),
            "-C".to_string(),
            workspace.display().to_string(),
        ]);
        // The live control plane's own token, from the start that just
        // connected — never re-read from the file (§4.2).
        let token = inner.token.lock().expect("token").clone();
        let mut env = crate::session::base_env(&inner.search_path, desk_id);
        env.push(("MARKETRIG_CODEX_WS_TOKEN".to_string(), token));

        {
            let mut threads = inner.threads.lock().expect("threads");
            threads.ready.remove(desk_id);
            match resume {
                Some(thread) => {
                    threads
                        .by_desk
                        .insert(desk_id.to_string(), thread.to_string());
                    threads.status.remove(thread);
                }
                None => {
                    threads.by_desk.remove(desk_id);
                    threads
                        .awaiting
                        .insert(canonical(&workspace), desk_id.to_string());
                }
            }
        }

        let pid = inner
            .terminals
            .spawn(
                desk_id,
                terminal::Spawn {
                    argv,
                    cwd: workspace,
                    env,
                    cols: 120,
                    rows: 40,
                },
            )
            .map_err(|e| e.to_string())?;
        inner
            .threads
            .lock()
            .expect("threads")
            .live
            .insert(desk_id.to_string(), pid);
        Ok(Activation {
            pid,
            native_session_id: resume.map(str::to_string),
        })
    }

    async fn deliver(
        &self,
        desk_id: &str,
        _prompt_id: &str,
        _kind: &str,
        text: &str,
    ) -> DeliverOutcome {
        let inner = &self.0;
        let thread = {
            let threads = inner.threads.lock().expect("threads");
            let Some(thread) = threads.by_desk.get(desk_id).cloned() else {
                return DeliverOutcome::Waiting;
            };
            if threads.status.get(&thread).map(String::as_str) != Some("idle") {
                return DeliverOutcome::Waiting;
            }
            thread
        };
        let Ok(client) = self.client().await else {
            return DeliverOutcome::Waiting;
        };
        // §4.3: the last status is not enough — a turn started elsewhere since
        // the broadcast closes the gate too.
        match client
            .call("thread/turns/list", json!({"threadId": thread, "limit": 1}))
            .await
        {
            Ok(result) if active_turn(&result).is_some() => return DeliverOutcome::Waiting,
            Ok(_) => {}
            Err(CallError::Lost) => return DeliverOutcome::Waiting,
            // A thread with no user message yet is not materialized and the
            // app-server refuses the listing (Codex 0.152.1); that is exactly
            // the no-active-turn case, so the idle gate alone decides.
            Err(CallError::Rpc(_)) => {}
        }
        match client
            .call(
                "turn/start",
                json!({"threadId": thread, "input": [{"type": "text", "text": text}]}),
            )
            .await
        {
            Ok(result) if result.get("turn").is_some() => DeliverOutcome::Delivered,
            Ok(_) => DeliverOutcome::Refused("the app-server started no turn".to_string()),
            Err(CallError::Rpc(message)) => DeliverOutcome::Refused(message),
            Err(CallError::Lost) => DeliverOutcome::HandoffUnknown,
        }
    }

    /// §4.1: `POST /runtimes/codex/retry` clears `CONTROL_PLANE_FAILED`, so it
    /// clears the restart count that failure came from too — otherwise the very
    /// next loss is a second failure again.
    fn reset_failures(&self) {
        self.0.restarts.store(0, Ordering::SeqCst);
    }

    async fn interrupt(&self, desk_id: &str) -> Result<String, (&'static str, String)> {
        let inner = &self.0;
        let thread = inner
            .threads
            .lock()
            .expect("threads")
            .by_desk
            .get(desk_id)
            .cloned()
            .ok_or(("NO_ACTIVE_TURN", "the desk has no thread".to_string()))?;
        let client = self
            .client()
            .await
            .map_err(|message| ("RUNTIME_ERROR", message))?;
        let turns = client
            .call("thread/turns/list", json!({"threadId": thread, "limit": 1}))
            .await
            .map_err(runtime_error)?;
        let turn_id =
            active_turn(&turns).ok_or(("NO_ACTIVE_TURN", "no turn is active".to_string()))?;
        client
            .call(
                "turn/interrupt",
                json!({"threadId": thread, "turnId": turn_id}),
            )
            .await
            .map_err(runtime_error)?;
        Ok(turn_id)
    }

    async fn exit(&self, desk_id: &str) {
        let inner = &self.0;
        {
            let mut threads = inner.threads.lock().expect("threads");
            threads.ready.remove(desk_id);
            threads.live.remove(desk_id);
            threads.awaiting.retain(|_, desk| desk != desk_id);
        }
        inner.terminals.shutdown(desk_id);
    }
}

fn runtime_error(error: CallError) -> (&'static str, String) {
    match error {
        CallError::Rpc(message) => ("RUNTIME_ERROR", message),
        CallError::Lost => ("RUNTIME_ERROR", "the control plane was lost".to_string()),
    }
}

/// The id of the newest turn that is still running, if any (§4.3).
fn active_turn(result: &Value) -> Option<String> {
    result["data"]
        .as_array()?
        .iter()
        .find(|turn| turn["status"] == "inProgress")
        .and_then(|turn| turn["id"].as_str())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// codex (feature SPEC §10 check 3)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The push that makes the fake app-server drop a live connection.
    const CLOSE: &str = "__close__";

    /// What the fake app-server should do next, set by the test.
    struct Script {
        /// Answer `turn/start` with this JSON-RPC error instead of a turn.
        refuse: Mutex<Option<String>>,
        /// Drop the connection instead of answering `turn/start`.
        drop_on_turn: AtomicBool,
        /// `thread/turns/list` reports an in-progress turn.
        active: AtomicBool,
        /// Connections accepted so far — the restart count.
        connections: AtomicUsize,
        /// Broadcasts to push to every connection, drained on connect.
        broadcast: tokio::sync::broadcast::Sender<String>,
    }

    impl Script {
        fn new() -> Arc<Script> {
            Arc::new(Script {
                refuse: Mutex::new(None),
                drop_on_turn: AtomicBool::new(false),
                active: AtomicBool::new(false),
                connections: AtomicUsize::new(0),
                broadcast: tokio::sync::broadcast::channel(16).0,
            })
        }
    }

    /// The in-process app-server the check speaks to: only the subset the
    /// adapter consumes.
    async fn fake_app_server(script: Arc<Script>) -> String {
        use axum::extract::ws::{Message as Ws, WebSocketUpgrade};
        use axum::routing::get;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/",
            get(async move |upgrade: WebSocketUpgrade| {
                let script = script.clone();
                upgrade.on_upgrade(async move |mut socket| {
                    script.connections.fetch_add(1, Ordering::SeqCst);
                    let mut pushes = script.broadcast.subscribe();
                    loop {
                        let message = tokio::select! {
                            push = pushes.recv() => {
                                match push {
                                    // The one way the test closes a live
                                    // connection from the server's side.
                                    Ok(text) if text == CLOSE => return,
                                    Ok(text) => {
                                        if socket.send(Ws::Text(text.into())).await.is_err() {
                                            return;
                                        }
                                        continue;
                                    }
                                    Err(_) => continue,
                                }
                            }
                            frame = socket.recv() => frame,
                        };
                        let Some(Ok(Ws::Text(text))) = message else {
                            return;
                        };
                        let frame: Value = serde_json::from_str(&text).unwrap();
                        let Some(id) = frame.get("id").and_then(Value::as_i64) else {
                            continue;
                        };
                        let method = frame["method"].as_str().unwrap_or_default();
                        let reply = match method {
                            "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
                            "thread/turns/list" => {
                                let data = if script.active.load(Ordering::SeqCst) {
                                    json!([{"id": "turn-1", "status": "inProgress"}])
                                } else {
                                    json!([{"id": "turn-0", "status": "completed"}])
                                };
                                json!({"jsonrpc":"2.0","id":id,"result":{"data": data}})
                            }
                            "turn/start" => {
                                if script.drop_on_turn.load(Ordering::SeqCst) {
                                    return;
                                }
                                match script.refuse.lock().unwrap().clone() {
                                    Some(message) => json!({"jsonrpc":"2.0","id":id,
                                        "error":{"code":-32600,"message":message}}),
                                    None => json!({"jsonrpc":"2.0","id":id,
                                        "result":{"turn":{"id":"turn-9"}}}),
                                }
                            }
                            "turn/interrupt" => {
                                json!({"jsonrpc":"2.0","id":id,"result":{}})
                            }
                            _ => json!({"jsonrpc":"2.0","id":id,
                                "error":{"code":-32601,"message":"unknown"}}),
                        };
                        if socket
                            .send(Ws::Text(reply.to_string().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                })
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("ws://127.0.0.1:{port}")
    }

    /// A desk on `codex` whose "runtime" is a command that exits at once: the
    /// adapter's own bookkeeping is what the check is about, not the TUI.
    fn fixture(
        url: String,
    ) -> (
        tempfile::TempDir,
        Codex,
        mpsc::UnboundedReceiver<AdapterEvent>,
        PathBuf,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots::resolve(Some(dir.path())).unwrap();
        roots.create_dirs().unwrap();
        let workspace = roots.desks.join("alpha");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = Store::open(&roots.database()).unwrap();
        // Absolute on both platforms, because the fixture hands the terminal
        // an empty search path, and quick to exit whatever the arguments,
        // because a child that lingers costs shutdown its 2 s drain before
        // the kill (`cmd.exe` would sit at a prompt; `whoami.exe` rejects
        // the arguments and leaves at once).
        let executable = if cfg!(windows) {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            format!(r"{root}\System32\whoami.exe")
        } else {
            "/bin/echo".to_string()
        };
        let workspace_sql = workspace.display().to_string().replace('\'', "''");
        store
            .unit(move |tx| {
                tx.execute(
                    &format!(
                        "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, \
                         ready_at_ns, selected_runtime) \
                         VALUES ('d1','alpha','READY','{workspace_sql}',1,2,'codex')"
                    ),
                    [],
                )?;
                tx.execute(
                    "UPDATE runtimes SET state = 'AVAILABLE', executable_path = ?1, \
                     version = '0.152.1', validated_at_ns = 1 WHERE runtime = 'codex'",
                    [executable],
                )
            })
            .unwrap();
        let (terminals, _exits) = terminal::Manager::new();
        let (events, rx) = mpsc::unbounded_channel();
        let mut codex = Codex::new(
            store,
            roots,
            terminals,
            events,
            String::new(),
            PathBuf::from("marketrig-mcp"),
            Some(dir.path().to_path_buf()),
            "test-daemon".to_string(),
        );
        Arc::get_mut(&mut codex.0).unwrap().connect_url = Some(url);
        (dir, codex, rx, workspace)
    }

    fn events(rx: &mut mpsc::UnboundedReceiver<AdapterEvent>) -> Vec<AdapterEvent> {
        let mut all = Vec::new();
        while let Ok(event) = rx.try_recv() {
            all.push(event);
        }
        all
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    fn started(thread: &str, cwd: &Path, ephemeral: bool, status: &str) -> String {
        json!({"method": "thread/started", "params": {"thread": {
            "id": thread, "cwd": cwd.display().to_string(),
            "ephemeral": ephemeral, "status": {"type": status}}}})
        .to_string()
    }

    #[tokio::test]
    async fn pointer_readiness_gate_and_delivery_outcomes() {
        let script = Script::new();
        let url = fake_app_server(script.clone()).await;
        let (_dir, codex, mut rx, workspace) = fixture(url);

        codex.spawn("d1", None).await.unwrap();
        // §4.1: an ephemeral thread, and a thread of another workspace, are not
        // the desk's pointer; the first non-ephemeral match is.
        script
            .broadcast
            .send(started("eph", &workspace, true, "idle"))
            .unwrap();
        script
            .broadcast
            .send(started("other", Path::new("/nowhere"), false, "idle"))
            .unwrap();
        settle().await;
        assert!(events(&mut rx).is_empty(), "no pointer from either");

        script
            .broadcast
            .send(started("th-1", &workspace, false, "idle"))
            .unwrap();
        settle().await;
        let seen = events(&mut rx);
        assert!(matches!(
            &seen[0],
            AdapterEvent::PointerDiscovered { native_session_id, .. } if native_session_id == "th-1"
        ));
        // A new session's `idle` rides on `thread/started` (spike S): readiness
        // needs no further broadcast.
        assert!(matches!(&seen[1], AdapterEvent::Ready { desk_id } if desk_id == "d1"));

        // The registration the app-server reads (§4.2).
        let config = std::fs::read_to_string(workspace.join(".codex/config.toml")).unwrap();
        assert!(config.contains("[mcp_servers.marketrig]"));
        assert!(config.contains(r#"args = ["--desk", "d1"]"#));

        // Gate: an active turn holds the prompt even while the status is idle.
        script.active.store(true, Ordering::SeqCst);
        assert_eq!(
            codex.deliver("d1", "p1", "TRIGGER_RESULT", "hello").await,
            DeliverOutcome::Waiting
        );
        script.active.store(false, Ordering::SeqCst);
        assert_eq!(
            codex.deliver("d1", "p1", "TRIGGER_RESULT", "hello").await,
            DeliverOutcome::Delivered
        );

        // Gate: a non-idle status closes it without touching the app-server.
        script
            .broadcast
            .send(
                json!({"method":"thread/status/changed",
                       "params":{"threadId":"th-1","status":{"type":"active"}}})
                .to_string(),
            )
            .unwrap();
        settle().await;
        assert_eq!(
            codex.deliver("d1", "p1", "TRIGGER_RESULT", "hello").await,
            DeliverOutcome::Waiting
        );
        script
            .broadcast
            .send(
                json!({"method":"thread/status/changed",
                       "params":{"threadId":"th-1","status":{"type":"idle"}}})
                .to_string(),
            )
            .unwrap();
        settle().await;

        // Refusal carries the app-server's own message.
        *script.refuse.lock().unwrap() = Some("no".to_string());
        assert_eq!(
            codex.deliver("d1", "p1", "TRIGGER_RESULT", "hello").await,
            DeliverOutcome::Refused("no".to_string())
        );
        *script.refuse.lock().unwrap() = None;

        // Interrupt: no active turn, then one.
        assert_eq!(codex.interrupt("d1").await.unwrap_err().0, "NO_ACTIVE_TURN");
        script.active.store(true, Ordering::SeqCst);
        assert_eq!(codex.interrupt("d1").await.unwrap(), "turn-1");
        script.active.store(false, Ordering::SeqCst);

        // A socket that goes before the response is an uncertain handoff.
        script.drop_on_turn.store(true, Ordering::SeqCst);
        assert_eq!(
            codex.deliver("d1", "p1", "TRIGGER_RESULT", "hello").await,
            DeliverOutcome::HandoffUnknown
        );
        codex.stop().await;
    }

    #[tokio::test]
    async fn control_plane_loss_restarts_once_then_fails_the_runtime() {
        let script = Script::new();
        let url = fake_app_server(script.clone()).await;
        let (_dir, codex, mut rx, workspace) = fixture(url);
        let store = codex.0.store.clone();

        codex.spawn("d1", None).await.unwrap();
        script
            .broadcast
            .send(started("th-1", &workspace, false, "idle"))
            .unwrap();
        settle().await;
        let _ = events(&mut rx);

        // The first loss ends the session, keeps the pointer, and restarts.
        script.broadcast.send(CLOSE.to_string()).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;

        let kinds = |()| -> Vec<String> {
            store
                .call(|c| {
                    c.prepare("SELECT kind FROM operational_events ORDER BY occurred_at_ns, id")?
                        .query_map([], |r| r.get(0))?
                        .collect()
                })
                .unwrap()
        };
        let seen = kinds(());
        assert!(seen.iter().any(|k| k == "CONTROL_PLANE_LOST"));
        assert_eq!(
            seen.iter()
                .filter(|k| *k == "CONTROL_PLANE_STARTED")
                .count(),
            2,
            "the start was retried exactly once: {seen:?}"
        );
        assert_eq!(script.connections.load(Ordering::SeqCst), 2);
        assert!(events(&mut rx).iter().any(
            |e| matches!(e, AdapterEvent::Exited { reason, .. } if *reason
                    == "CONTROL_PLANE_LOST")
        ));
        // The pointer survives the loss (§4.1).
        assert_eq!(
            codex.0.threads.lock().unwrap().by_desk.get("d1").unwrap(),
            "th-1"
        );
        assert_eq!(
            crate::runtime::get(&store, "codex").unwrap().unwrap().state,
            "AVAILABLE"
        );

        // The second loss is `CONTROL_PLANE_FAILED`, with no third start.
        script.broadcast.send(CLOSE.to_string()).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(script.connections.load(Ordering::SeqCst), 2);
        let row = crate::runtime::get(&store, "codex").unwrap().unwrap();
        assert_eq!(row.state, "UNAVAILABLE");
        assert_eq!(row.failure_code.as_deref(), Some("CONTROL_PLANE_FAILED"));

        // `retry` clears the count with the row, so the runtime gets its one
        // restart again instead of failing on the very next loss (§4.1).
        codex.reset_failures();
        codex.client().await.unwrap();
        let connected = script.connections.load(Ordering::SeqCst);
        script.broadcast.send(CLOSE.to_string()).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            script.connections.load(Ordering::SeqCst),
            connected + 1,
            "the retry bought another restart"
        );
        codex.stop().await;
    }
}
