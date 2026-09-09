//! `openviking-standin` — the gate's stand-in OpenViking child.
//!
//! Contract: `sdd/features/openviking-continuity/SPEC.md` §7.1, per OV-7. It
//! speaks exactly the subset the daemon and the vendored plugins consume (§2.2,
//! §3.2, §5.2, §5.3) and nothing more: `--version`, `/health`, `/ready`, the
//! three admin routes with the seeded key rule, the skills routes with content
//! and files, the temporary upload the seed archive goes through,
//! `content/read`, sessions with messages and a scripted commit task, and
//! substring `find` — all behind `api_key` auth over a per-user store. It runs
//! no MCP, no Node, and no Python.
//!
//! Every knob comes from the `openviking` object of the one JSON file
//! `MARKETRIG_STANDIN_SCRIPT` names, the same file `runtime-standin` reads. It
//! is read once at start, because the daemon starts the child and only a Retry
//! can re-read it.
//!
//! The root key is taken the way the real server takes it: from the config
//! file's `server.root_api_key`, with a `${VAR}` reference expanded from the
//! environment — which is how the harness learns the daemon rendered §2.1's
//! reference and set `MARKETRIG_OV_ROOT_KEY` correctly.

// The auth helpers carry a ready `Response` as their error, which is an axum
// `Response` and therefore large; boxing it would only add a deref per refusal.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::io::{self, Read as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

/// What `openviking-server --version` prints on the pinned line (§1.2).
const VERSION: &str = "openviking-server 0.4.17.1";

/// The one root every listing merges beside the user's own (§5.2's filter).
const AGENT_ROOT: &str = "viking://agent/skills";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("{VERSION}");
        return;
    }
    let config = flag(&args, "--config")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let script = std::env::var_os("MARKETRIG_STANDIN_SCRIPT")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|script| script["openviking"].clone())
        .unwrap_or(Value::Null);

    let host = flag(&args, "--host").unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = flag(&args, "--port").unwrap_or_default();
    let ready_after = Duration::from_millis(script["ready_after_ms"].as_u64().unwrap_or(0));

    // The store outlives one child: a Retry or a reprovision restarts the
    // process, and §7.2's O5 and O6 both expect what was written before it to
    // still be there. ponytail: one JSON file rewritten whole under the lock —
    // the gate writes tens of documents, not thousands.
    let workspace = config
        .as_ref()
        .and_then(|config| config["storage"]["workspace"].as_str())
        .map(PathBuf::from);
    if let Some(workspace) = &workspace {
        let _ = std::fs::create_dir_all(workspace);
    }
    let store_path = workspace
        .as_ref()
        .map(|workspace| workspace.join("standin-store.json"));
    let root_key = root_key(config.as_ref());
    // The daemon mints the root key per start and keeps it in memory alone
    // (§3.1), so this file beside the store is the only way an acceptance mode
    // learns which string §7.2's O1 must find nowhere else.
    if let Some(workspace) = &workspace {
        let _ = std::fs::write(workspace.join("standin-root-key"), &root_key);
    }
    let store = store_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(
            || json!({"accounts": {}, "keys": {}, "users": {}, "tasks": {}, "uploads": {}}),
        );

    let state = Arc::new(Ov {
        root_key,
        ready_at: Instant::now() + ready_after,
        commit_task_status: script["commit_task_status"]
            .as_str()
            .unwrap_or("completed")
            .to_owned(),
        store_path,
        store: Mutex::new(store),
    });

    // A scripted loss (§2.3): the exit is counted from readiness, so the daemon
    // sees `/ready` answer once before the child goes.
    if let Some(ms) = script["exit_after_ready_ms"].as_u64() {
        let code = script["exit_code"].as_i64().unwrap_or(1) as i32;
        tokio::spawn(async move {
            tokio::time::sleep(ready_after + Duration::from_millis(ms)).await;
            println!("openviking-standin: exiting {code} as scripted");
            let _ = std::io::stdout().flush();
            std::process::exit(code);
        });
    }

    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
        .await
        .expect("bind the stand-in OpenViking child");
    // The daemon keeps a 4 KiB tail of both streams and reports its last line
    // when the child is lost (§2.3), so this banner is what O5 reads back.
    println!("openviking-standin: listening on {host}:{port}");
    let _ = std::io::stdout().flush();

    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/api/v1/admin/accounts", post(create_account))
        .route("/api/v1/admin/accounts/{account}/users", post(create_user))
        .route(
            "/api/v1/admin/accounts/{account}/users/{user}/key",
            post(issue_key),
        )
        .route("/api/v1/resources/temp_upload", post(temp_upload))
        .route("/api/v1/skills", get(list_skills).post(create_skill))
        .route(
            "/api/v1/skills/{name}",
            get(read_skill).put(put_skill).delete(delete_skill),
        )
        .route("/api/v1/content/read", get(content_read))
        .route("/api/v1/sessions/{id}", get(read_session))
        .route("/api/v1/sessions/{id}/messages", post(add_message))
        .route("/api/v1/sessions/{id}/commit", post(commit))
        .route("/api/v1/tasks/{id}", get(read_task))
        .route("/api/v1/search/find", post(find))
        .fallback(not_found)
        .with_state(state);
    let _ = axum::serve(listener, app).await;
}

/// `--flag value` anywhere in the command line.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .cloned()
}

/// The root key, read the way the real server reads it: the config's
/// `server.root_api_key` with `${VAR}` expanded from the environment (§2.1).
/// An empty result is a rendering the daemon got wrong, and the child refuses
/// to start on it rather than authenticating nobody.
fn root_key(config: Option<&Value>) -> String {
    let key = match config.and_then(|config| config["server"]["root_api_key"].as_str()) {
        Some(raw) => match raw.strip_prefix("${").and_then(|raw| raw.strip_suffix('}')) {
            Some(name) => std::env::var(name).unwrap_or_default(),
            None => raw.to_owned(),
        },
        None => std::env::var("MARKETRIG_OV_ROOT_KEY").unwrap_or_default(),
    };
    assert!(
        !key.is_empty(),
        "openviking-standin: server.root_api_key expanded to nothing — \
         the config reference or MARKETRIG_OV_ROOT_KEY is wrong"
    );
    key
}

// ---------------------------------------------------------------------------
// Keys (§3.2)
// ---------------------------------------------------------------------------

/// The key for one user: OpenViking's documented seeded rule when a seed is
/// given (`openviking/server/api_keys/legacy.py`, and the shared harness's
/// `openviking_key`), and the same rule over the clock when none is — which is
/// what the real server does at user creation, before the daemon asks for the
/// seeded one (§3.2's three calls).
fn user_key(account: &str, user: &str, seed: Option<&str>) -> String {
    let seed = match seed {
        Some(seed) => seed.to_owned(),
        None => format!(
            "{:?}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
        ),
    };
    marketrig_acceptance::openviking_key(account, user, &seed)
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct Ov {
    root_key: String,
    ready_at: Instant,
    commit_task_status: String,
    store_path: Option<PathBuf>,
    /// `{accounts: {id: {admin_user_id, users: [id]}}, keys: {key: user},
    ///   users: {id: {skills: {name: {content, files: {path: text}}},
    ///                sessions: {id: [message]}}}, tasks: {id: status},
    ///   uploads: {temp_file_id: {path: text}}}`
    store: Mutex<Value>,
}

impl Ov {
    fn store(&self) -> MutexGuard<'_, Value> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn persist(&self, store: &Value) {
        if let Some(path) = &self.store_path {
            // Replaced whole, never truncated in place: the gate reads this file
            // while the child is writing it, and a torn read is not a store.
            let scratch = path.with_extension("writing");
            if std::fs::write(&scratch, store.to_string()).is_ok() {
                let _ = std::fs::rename(&scratch, path);
            }
        }
    }

    /// The bearer's identity: `None` is ROOT, `Some(user)` is that user's key.
    /// `/health` and `/ready` never ask (§7.1).
    fn principal(&self, headers: &HeaderMap) -> Result<Option<String>, Response> {
        let sent = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        if !sent.is_empty() && sent == self.root_key {
            return Ok(None);
        }
        match self.store()["keys"][sent].as_str() {
            Some(user) => Ok(Some(user.to_owned())),
            None => Err(error(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "invalid or missing API key",
            )),
        }
    }

    /// The tenant behind the bearer. ROOT has no user binding, so every user
    /// route refuses it — which is the whole point of `api_key` mode (OV-3).
    fn user(&self, headers: &HeaderMap) -> Result<String, Response> {
        match self.principal(headers)? {
            Some(user) => Ok(user),
            None => Err(error(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "ROOT has no user binding",
            )),
        }
    }

    /// ROOT, or the account's own admin user.
    fn admin(&self, headers: &HeaderMap, account: &str) -> Result<(), Response> {
        match self.principal(headers)? {
            None => Ok(()),
            Some(user) if self.store()["accounts"][account]["admin_user_id"] == json!(user) => {
                Ok(())
            }
            Some(_) => Err(error(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "admin privileges required",
            )),
        }
    }

    /// Records a user under an account, mints its key, and answers it.
    fn provision(&self, account: &str, user: &str, seed: Option<&str>) -> String {
        let key = user_key(account, user, seed);
        let mut store = self.store();
        store["accounts"][account]["users"][user] = json!(true);
        store["keys"][&key] = json!(user);
        if !store["users"][user].is_object() {
            store["users"][user] = json!({"skills": {}, "sessions": {}});
        }
        self.persist(&store);
        key
    }
}

fn ok(result: Value) -> Response {
    axum::Json(json!({"status": "ok", "result": result})).into_response()
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(json!({"status": "error", "error": {"code": code, "message": message}})),
    )
        .into_response()
}

fn body(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "NOT_FOUND", "no such route")
}

// ---------------------------------------------------------------------------
// Liveness and readiness (§2.2)
// ---------------------------------------------------------------------------

async fn health() -> Response {
    ok(json!({"status": "ok"}))
}

async fn ready(State(state): State<Arc<Ov>>) -> Response {
    if Instant::now() >= state.ready_at {
        return axum::Json(json!({"status": "ready"})).into_response();
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(json!({"status": "not_ready", "reason": "initializing"})),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// The admin routes (§3.2)
// ---------------------------------------------------------------------------

async fn create_account(
    State(state): State<Arc<Ov>>,
    headers: HeaderMap,
    text: String,
) -> Response {
    match state.principal(&headers) {
        Err(denied) => return denied,
        Ok(Some(_)) => {
            return error(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "account creation is ROOT only",
            );
        }
        Ok(None) => {}
    }
    let request = body(&text);
    let (Some(account), Some(admin)) = (
        request["account_id"].as_str(),
        request["admin_user_id"].as_str(),
    ) else {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "account_id and admin_user_id are required",
        );
    };
    if state.store()["accounts"][account].is_object() {
        return error(
            StatusCode::CONFLICT,
            "ALREADY_EXISTS",
            &format!("account {account} already exists"),
        );
    }
    {
        let mut store = state.store();
        store["accounts"][account] = json!({"admin_user_id": admin, "users": {}});
        state.persist(&store);
    }
    let key = state.provision(account, admin, request["seed"].as_str());
    ok(json!({"account_id": account, "admin_user_id": admin, "user_key": key}))
}

async fn create_user(
    State(state): State<Arc<Ov>>,
    Path(account): Path<String>,
    headers: HeaderMap,
    text: String,
) -> Response {
    if let Err(denied) = state.admin(&headers, &account) {
        return denied;
    }
    let request = body(&text);
    let Some(user) = request["user_id"].as_str() else {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "user_id is required",
        );
    };
    if !state.store()["accounts"][&account].is_object() {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no account {account}"),
        );
    }
    if state.store()["accounts"][&account]["users"][user].is_boolean() {
        return error(
            StatusCode::CONFLICT,
            "ALREADY_EXISTS",
            &format!("user {user} already exists"),
        );
    }
    let key = state.provision(&account, user, request["seed"].as_str());
    ok(json!({"account_id": account, "user_id": user, "user_key": key}))
}

/// The seeded key: deterministic in the seed, which is why the daemon asks for
/// it again after a Retry instead of remembering it (OV-3).
async fn issue_key(
    State(state): State<Arc<Ov>>,
    Path((account, user)): Path<(String, String)>,
    headers: HeaderMap,
    text: String,
) -> Response {
    if let Err(denied) = state.admin(&headers, &account) {
        return denied;
    }
    if !state.store()["accounts"][&account]["users"][&user].is_boolean() {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no user {user} under {account}"),
        );
    }
    let key = state.provision(&account, &user, body(&text)["seed"].as_str());
    ok(json!({"user_key": key}))
}

// ---------------------------------------------------------------------------
// Skills (§5.2, §5.3)
// ---------------------------------------------------------------------------

fn root_uri(user: &str) -> String {
    format!("viking://user/{user}/skills")
}

/// One value of the SKILL.md YAML frontmatter.
fn front(markdown: &str, key: &str) -> String {
    let mut lines = markdown.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return value.trim().trim_matches('"').to_owned();
        }
    }
    String::new()
}

/// The listing shape (§5.2's first call). `root_uris` carries the agent root
/// beside the user's own, exactly as the real listing does, so the projection's
/// `viking://user/` filter has something to filter.
async fn list_skills(State(state): State<Arc<Ov>>, headers: HeaderMap) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let root = root_uri(&user);
    let store = state.store();
    let skills: Vec<Value> = store["users"][&user]["skills"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, skill)| {
            let content = skill["content"].as_str().unwrap_or_default();
            json!({
                "type": "skill",
                "name": name,
                "uri": format!("{root}/{name}"),
                "root_uri": root,
                "skill_md_uri": format!("{root}/{name}/SKILL.md"),
                "description": front(content, "description"),
                "tags": [],
                "allowed_tools": [],
            })
        })
        .collect();
    ok(json!({
        "root_uris": [root, AGENT_ROOT],
        "total": skills.len(),
        "skills": skills,
    }))
}

/// One skill with its content and its auxiliary files (§5.2's per-name call).
async fn read_skill(
    State(state): State<Arc<Ov>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let root = root_uri(&user);
    let uri = format!("{root}/{name}");
    let store = state.store();
    let skill = &store["users"][&user]["skills"][&name];
    if !skill.is_object() {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no skill {name}"),
        );
    }
    let content = skill["content"].as_str().unwrap_or_default();
    let files: Vec<Value> = skill["files"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(path, _)| {
            json!({"path": path, "uri": format!("{uri}/{path}"), "is_dir": false, "kind": "file"})
        })
        .collect();
    ok(json!({
        "name": name,
        "uri": uri,
        "root_uri": root,
        "skill_md_uri": format!("{uri}/SKILL.md"),
        "description": front(content, "description"),
        "content": content,
        "content_sha256": marketrig_acceptance::sha256_hex(content.as_bytes()),
        "files": files,
    }))
}

/// `POST /api/v1/resources/temp_upload` — the multipart half of the multi-file
/// seed upload (`hithink-a-share` §5.3). The daemon sends one `file` part
/// holding a ZIP, so the archive is found by its own signature rather
/// than by parsing the envelope, unpacked, and kept under the id the create
/// consumes.
async fn temp_upload(
    State(state): State<Arc<Ov>>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    if let Err(denied) = state.user(&headers) {
        return denied;
    }
    let Some(at) = bytes.windows(4).position(|four| four == b"PK\x03\x04") else {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "the upload carries no ZIP archive",
        );
    };
    let files: Value = unzip(&bytes[at..]).into_iter().collect();
    let id = format!(
        "temp-{:?}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
    );
    let mut store = state.store();
    store["uploads"][&id] = files;
    state.persist(&store);
    drop(store);
    ok(json!({"temp_file_id": id}))
}

/// A ZIP read back as `path -> text`, stored or deflated alike — the reader
/// upstream's skill processor stands in for.
fn unzip(bytes: &[u8]) -> Vec<(String, Value)> {
    let mut archive = match zip::ZipArchive::new(io::Cursor::new(bytes)) {
        Ok(archive) => archive,
        Err(_) => return Vec::new(),
    };
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        let name = entry.name().to_owned();
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_ok() {
            files.push((name, json!(text)));
        }
    }
    files
}

/// `POST /api/v1/skills` — the seed uploads (§5.3, `hithink-a-share` §5.3) and
/// the harness's own writes. `data` is one `SKILL.md`; `temp_file_id` names an
/// unpacked archive whose root `SKILL.md` is the skill and whose other files
/// are its auxiliary ones, which is what upstream's skill processor does with
/// a directory. The name comes from the frontmatter either way.
async fn create_skill(State(state): State<Arc<Ov>>, headers: HeaderMap, text: String) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let request = body(&text);
    let (data, files) = match request["temp_file_id"].as_str() {
        None => (
            request["data"].as_str().unwrap_or_default().to_owned(),
            request["files"].clone(),
        ),
        Some(id) => {
            let mut archive = state.store()["uploads"][id].clone();
            let Some(unpacked) = archive.as_object_mut() else {
                return error(
                    StatusCode::NOT_FOUND,
                    "NOT_FOUND",
                    &format!("no upload {id}"),
                );
            };
            let skill_md = unpacked.remove("SKILL.md").unwrap_or(Value::Null);
            (
                skill_md.as_str().unwrap_or_default().to_owned(),
                Value::Object(unpacked.clone()),
            )
        }
    };
    let name = front(&data, "name");
    if name.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "data carries no frontmatter name",
        );
    }
    if state.store()["users"][&user]["skills"][&name].is_object() {
        return error(
            StatusCode::CONFLICT,
            "ALREADY_EXISTS",
            &format!("skill {name} already exists"),
        );
    }
    write_skill(&state, &user, &name, &data, &files);
    let root = root_uri(&user);
    ok(json!({"name": name, "uri": format!("{root}/{name}"), "root_uri": root}))
}

async fn put_skill(
    State(state): State<Arc<Ov>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    text: String,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let request = body(&text);
    write_skill(
        &state,
        &user,
        &name,
        request["data"].as_str().unwrap_or_default(),
        &request["files"],
    );
    let root = root_uri(&user);
    ok(json!({"name": name, "uri": format!("{root}/{name}"), "root_uri": root}))
}

async fn delete_skill(
    State(state): State<Arc<Ov>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let mut store = state.store();
    let removed = store["users"][&user]["skills"]
        .as_object_mut()
        .and_then(|skills| skills.remove(&name));
    if removed.is_none() {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no skill {name}"),
        );
    }
    state.persist(&store);
    ok(json!({"name": name}))
}

fn write_skill(state: &Ov, user: &str, name: &str, data: &str, files: &Value) {
    let mut store = state.store();
    store["users"][user]["skills"][name] = json!({
        "content": data,
        "files": files.as_object().cloned().map(Value::Object).unwrap_or(json!({})),
    });
    state.persist(&store);
}

/// `GET /api/v1/content/read?uri=…` — the SKILL.md and auxiliary-file text
/// behind the URIs the listing handed out, and never another user's.
async fn content_read(
    State(state): State<Arc<Ov>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let uri = query.get("uri").cloned().unwrap_or_default();
    let Some(rest) = uri.strip_prefix(&format!("{}/", root_uri(&user))) else {
        return error(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "that URI is not under this user's root",
        );
    };
    let Some((name, path)) = rest.split_once('/') else {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND", "no such content");
    };
    let store = state.store();
    let skill = &store["users"][&user]["skills"][name];
    let found = if path == "SKILL.md" {
        skill["content"].as_str()
    } else {
        skill["files"][path].as_str()
    };
    match found {
        Some(text) => ok(json!(text)),
        None => error(StatusCode::NOT_FOUND, "NOT_FOUND", "no such content"),
    }
}

// ---------------------------------------------------------------------------
// Sessions, the commit task, and find (§7.1)
// ---------------------------------------------------------------------------

async fn add_message(
    State(state): State<Arc<Ov>>,
    Path(session): Path<String>,
    headers: HeaderMap,
    text: String,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let request = body(&text);
    let mut store = state.store();
    let messages = &mut store["users"][&user]["sessions"][&session];
    if !messages.is_array() {
        *messages = json!([]);
    }
    if let Some(messages) = messages.as_array_mut() {
        messages.push(json!({
            "role": request["role"].as_str().unwrap_or("user"),
            "content": request["content"].as_str().unwrap_or_default(),
        }));
    }
    let count = store["users"][&user]["sessions"][&session]
        .as_array()
        .map_or(0, Vec::len);
    state.persist(&store);
    ok(json!({"session_id": session, "message_count": count}))
}

async fn read_session(
    State(state): State<Arc<Ov>>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let store = state.store();
    let messages = &store["users"][&user]["sessions"][&session];
    if !messages.is_array() {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no session {session}"),
        );
    }
    ok(json!({"session_id": session, "messages": messages}))
}

async fn commit(
    State(state): State<Arc<Ov>>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let mut store = state.store();
    if !store["users"][&user]["sessions"][&session].is_array() {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no session {session}"),
        );
    }
    let task = format!(
        "task-{}",
        store["tasks"].as_object().map_or(0, serde_json::Map::len) + 1
    );
    store["tasks"][&task] = json!(state.commit_task_status);
    state.persist(&store);
    ok(json!({"task_id": task, "session_id": session}))
}

async fn read_task(
    State(state): State<Arc<Ov>>,
    Path(task): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denied) = state.user(&headers) {
        return denied;
    }
    match state.store()["tasks"][&task].as_str() {
        Some(status) => ok(json!({"task_id": task, "status": status})),
        None => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("no task {task}"),
        ),
    }
}

/// `POST /api/v1/search/find` — a case-insensitive substring over this user's
/// skill contents and session messages, and nothing of anyone else's.
async fn find(State(state): State<Arc<Ov>>, headers: HeaderMap, text: String) -> Response {
    let user = match state.user(&headers) {
        Ok(user) => user,
        Err(denied) => return denied,
    };
    let query = body(&text)["query"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let root = root_uri(&user);
    let store = state.store();
    let mut results = Vec::new();
    if !query.is_empty() {
        for (name, skill) in store["users"][&user]["skills"]
            .as_object()
            .into_iter()
            .flatten()
        {
            let content = skill["content"].as_str().unwrap_or_default();
            if content.to_lowercase().contains(&query) {
                results.push(json!({
                    "uri": format!("{root}/{name}/SKILL.md"),
                    "abstract": abstract_of(content),
                }));
            }
        }
        for (session, messages) in store["users"][&user]["sessions"]
            .as_object()
            .into_iter()
            .flatten()
        {
            for message in messages.as_array().into_iter().flatten() {
                let content = message["content"].as_str().unwrap_or_default();
                if content.to_lowercase().contains(&query) {
                    results.push(json!({
                        "uri": format!("viking://user/{user}/sessions/{session}"),
                        "abstract": abstract_of(content),
                    }));
                }
            }
        }
    }
    ok(json!({"results": results}))
}

fn abstract_of(text: &str) -> String {
    text.chars().take(200).collect()
}
