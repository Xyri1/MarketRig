//! `memory-standin` — the gate's stand-in Hindsight child, and its provider.
//!
//! Contract: `sdd/features/r4-memory-skills-loop/SPEC.md` §7.1, per R4-6. It
//! speaks exactly the subset the daemon consumes (§2.2, §3, §4.2) and nothing
//! more: `--help` with the probe marker, `/health`, the three bank routes with
//! an in-memory per-bank store, and — invoked as `--models <port>` — the
//! provider's own model list, which the gate starts itself.
//!
//! Every knob comes from the `memory` object of the one JSON file
//! `MARKETRIG_STANDIN_SCRIPT` names, the same file `runtime-standin` reads. The
//! child reads it once at start, because the daemon starts the child and only a
//! restart can re-read it; the provider half reads it per request, so the gate
//! can turn `models_error` on and off around a live server.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

/// The discovery probe requires exit `0` and this marker on standard output
/// (§2.1); the real launcher's own `--help` names it the same way.
const HELP: &str = "\
memory-standin — the MarketRig acceptance stand-in memory child

USAGE:
  memory-standin
      Serve the Hindsight subset on HINDSIGHT_API_HOST:HINDSIGHT_API_PORT,
      with every route but /health authenticated by HINDSIGHT_API_TENANT_API_KEY.

  memory-standin --models <port>
      Serve only GET /v1/models on that port, unauthenticated: the provider
      stand-in the gate starts for itself.
";

/// What `--models` answers when the script names nothing (§7.1).
const MODELS: [&str; 2] = ["stand-in-llm", "stand-in-embedding"];

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    match args.first().map(String::as_str) {
        // The daemon starts the child with no arguments at all (§2.2).
        None => child().await,
        Some("--models") => {
            let port: u16 = args
                .get(1)
                .and_then(|port| port.parse().ok())
                .expect("--models takes a port");
            models(port).await;
        }
        Some(other) => {
            eprintln!("memory-standin: unknown argument {other}\n{HELP}");
            std::process::exit(2);
        }
    }
}

/// The `memory` object of the run's script, or null when there is none.
fn knobs() -> Value {
    std::env::var_os("MARKETRIG_STANDIN_SCRIPT")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|script| script["memory"].clone())
        .unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// The child (§7.1)
// ---------------------------------------------------------------------------

struct Child {
    /// `HINDSIGHT_API_TENANT_API_KEY`, minted per start by the daemon (§2.2).
    bearer: String,
    /// When `/health` starts answering `200` (`health_after_ms`).
    ready_at: Instant,
    reject_retain: bool,
    /// Bank name to the memories it holds, each already in the shape §4.2's
    /// recall answers: the store is the answer, so nothing is mapped twice.
    banks: Mutex<HashMap<String, Vec<Value>>>,
    next: AtomicU64,
}

impl Child {
    fn banks(&self) -> MutexGuard<'_, HashMap<String, Vec<Value>>> {
        self.banks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `401` unless the daemon's per-start bearer arrived (§7.1).
    fn denied(&self, headers: &HeaderMap) -> Option<Response> {
        let sent = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        (sent != format!("Bearer {}", self.bearer)).then(|| {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({ "detail": "Invalid API key" })),
            )
                .into_response()
        })
    }
}

async fn child() {
    let script = knobs();
    let bearer = std::env::var("HINDSIGHT_API_TENANT_API_KEY").unwrap_or_default();
    let host = std::env::var("HINDSIGHT_API_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("HINDSIGHT_API_PORT").unwrap_or_default();

    // The gate learns the bearers this run minted from here: the daemon holds
    // them in memory only, and G33's secrets check needs the real strings.
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("bearers.txt"))
    {
        let _ = writeln!(file, "{bearer}");
    }

    if let Some(ms) = script["exit_after_ms"].as_u64() {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            println!("memory-standin: exiting 1 as scripted");
            let _ = std::io::stdout().flush();
            std::process::exit(1);
        });
    }

    let state = Arc::new(Child {
        bearer,
        ready_at: Instant::now()
            + Duration::from_millis(script["health_after_ms"].as_u64().unwrap_or(0)),
        reject_retain: script["reject_retain"].as_bool() == Some(true),
        banks: Mutex::new(HashMap::new()),
        next: AtomicU64::new(0),
    });

    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
        .await
        .expect("bind the stand-in memory child");
    // The daemon keeps a tail of standard output and reports its last line when
    // the child is lost (§2.2, §2.3), so the banner is what G37 reads back.
    println!("memory-standin: listening on {host}:{port}");
    let _ = std::io::stdout().flush();

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/default/banks/{bank}/memories", post(retain))
        .route("/v1/default/banks/{bank}/memories/recall", post(recall))
        .route("/v1/default/banks/{bank}/reflect", post(reflect))
        .with_state(state);
    let _ = axum::serve(listener, app).await;
}

async fn health(State(state): State<Arc<Child>>) -> Response {
    if Instant::now() >= state.ready_at {
        return axum::Json(json!({ "status": "ok" })).into_response();
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(json!({ "status": "starting" })),
    )
        .into_response()
}

async fn retain(
    State(state): State<Arc<Child>>,
    Path(bank): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(denied) = state.denied(&headers) {
        return denied;
    }
    if state.reject_retain {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({ "detail": "rejected by script" })),
        )
            .into_response();
    }
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let now = format!(
        "{}Z",
        marketrig_acceptance::utc(marketrig_acceptance::now_secs() as i64)
    );
    let mut banks = state.banks();
    let held = banks.entry(bank.clone()).or_default();
    let mut items_count = 0;
    for item in body["items"].as_array().into_iter().flatten() {
        let id = state.next.fetch_add(1, Ordering::SeqCst) + 1;
        held.push(json!({
            "id": format!("00000000-0000-4000-8000-{id:012}"),
            "text": item["content"],
            "type": "experience",
            "context": item["context"],
            "tags": item["tags"].as_array().cloned().unwrap_or_default(),
            "metadata": item["metadata"],
            "occurred_start": now,
            "mentioned_at": now,
        }));
        items_count += 1;
    }
    axum::Json(json!({
        "success": true, "bank_id": bank, "items_count": items_count, "async": false,
    }))
    .into_response()
}

async fn recall(
    State(state): State<Arc<Child>>,
    Path(bank): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(denied) = state.denied(&headers) {
        return denied;
    }
    axum::Json(json!({ "results": matching(&state, &bank, &body) })).into_response()
}

async fn reflect(
    State(state): State<Arc<Child>>,
    Path(bank): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(denied) = state.denied(&headers) {
        return denied;
    }
    let matched = matching(&state, &bank, &body);
    let text = matched
        .iter()
        .filter_map(|memory| memory["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let memories: Vec<Value> = matched
        .iter()
        .map(|memory| json!({ "id": memory["id"], "text": memory["text"], "type": "experience" }))
        .collect();
    // The child nests its citations under `based_on.memories` (§4.2).
    axum::Json(json!({ "text": text, "based_on": { "memories": memories } })).into_response()
}

/// One bank's memories matching the request: every word of the query a
/// case-insensitive substring of the text, and — when the body carries a tag
/// list — at least one of those tags (`tags_match: "any"`, §4.2).
fn matching(state: &Child, bank: &str, body: &str) -> Vec<Value> {
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let query = body["query"].as_str().unwrap_or_default().to_lowercase();
    let wanted: Vec<&str> = body["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    state
        .banks()
        .get(bank)
        .into_iter()
        .flatten()
        .filter(|memory| {
            let text = memory["text"].as_str().unwrap_or_default().to_lowercase();
            let words = || query.split_whitespace();
            words().next().is_some() && words().all(|word| text.contains(word))
        })
        .filter(|memory| {
            wanted.is_empty()
                || memory["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|tag| wanted.contains(&tag))
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// The provider (§7.1)
// ---------------------------------------------------------------------------

/// `GET /v1/models` and nothing else, on the port the gate names. The knobs are
/// read per request so `models_error` can be turned on and off under a server
/// that is already listening (§3's stale-list scenario).
async fn models(port: u16) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind the stand-in provider");
    let app = Router::new().route("/v1/models", get(model_list));
    let _ = axum::serve(listener, app).await;
}

async fn model_list() -> Response {
    let script = knobs();
    if script["models_error"].as_bool() == Some(true) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({ "error": "scripted models failure" })),
        )
            .into_response();
    }
    let ids = match script["models"].as_array() {
        Some(ids) => ids.clone(),
        None => MODELS.iter().map(|id| json!(id)).collect(),
    };
    let data: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "object": "model" }))
        .collect();
    axum::Json(json!({ "data": data })).into_response()
}
