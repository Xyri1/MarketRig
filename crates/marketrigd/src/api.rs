//! The loopback REST surface.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §6 (routes,
//! `Desk` resource, error envelope), §4.2 (`POST /quit`), §5.2 (`GET /health`
//! serves client verification).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::desk::{self, Desk, DeskError};
use crate::store::Store;
use crate::trade::{self, TradeError};

/// Everything the routes need. The daemon builds one and hands it to
/// [`router`]; nothing here is process-global (root SPEC §5.1).
pub struct ApiState {
    pub store: Store,
    pub desks_home: PathBuf,
    pub daemon_uuid: String,
    pub credential: String,
    pub started_at_ns: i64,
    /// Signals the daemon's shutdown path once `POST /quit` has answered (§4.2).
    pub quit: tokio::sync::mpsc::Sender<()>,
    /// Every desk's trading node, started lazily on first market-plane use
    /// (R1 feature SPEC §4.3).
    pub registry: Arc<crate::node::Registry>,
}

/// The whole §6 surface, every route behind the bearer check.
pub fn router(state: ApiState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/health", get(health))
        .route("/desks", get(list).post(create))
        .route("/desks/{desk_id}", get(show))
        .route("/desks/{desk_id}/retry", post(retry))
        .route("/desks/{desk_id}/orders", post(submit_order))
        .route(
            "/desks/{desk_id}/orders/{client_order_id}/cancel",
            post(cancel_order),
        )
        .route("/quit", post(quit))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

/// The one error envelope (§6, per R0-5): a stable SCREAMING_SNAKE code and an
/// English sentence, and nothing else.
fn envelope(status: StatusCode, code: &str, message: String) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "message": message })),
    )
        .into_response()
}

/// The §6 code-to-status map. `DeskError::code()` owns the code; this owns the
/// status. `DATABASE_NEWER` stops startup, so a running server never sends it.
impl IntoResponse for DeskError {
    fn into_response(self) -> Response {
        let status = match self {
            DeskError::NameInvalid(_) => StatusCode::BAD_REQUEST,
            DeskError::NotFound(_) => StatusCode::NOT_FOUND,
            DeskError::NameTaken(_) | DeskError::StateInvalid(_) => StatusCode::CONFLICT,
            DeskError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        envelope(status, self.code(), self.to_string())
    }
}

/// The R1 §7 code-to-status map, appended to R0's per D68. `TradeError::code()`
/// owns the code; this owns the status.
impl IntoResponse for TradeError {
    fn into_response(self) -> Response {
        // A desk lookup or store failure keeps R0's own mapping.
        if let TradeError::Desk(e) = self {
            return e.into_response();
        }
        let status = match &self {
            TradeError::Invalid(_) => StatusCode::BAD_REQUEST,
            TradeError::InstrumentUnknown(_) | TradeError::OrderNotFound(_) => {
                StatusCode::NOT_FOUND
            }
            TradeError::Rejected(_) | TradeError::NotReady(_) => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        envelope(status, self.code(), self.to_string())
    }
}

/// Bearer authentication for every route, including `/quit` (per R0-6).
/// ponytail: plain equality — a fresh 64-hex secret per start on a loopback
/// listener has no timing oracle worth a constant-time crate; swap one in if
/// the listener ever leaves loopback.
async fn authorize(State(state): State<Arc<ApiState>>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if presented == Some(state.credential.as_str()) {
        next.run(request).await
    } else {
        envelope(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "This request needs the daemon's bearer credential from runtime/endpoint.json."
                .to_string(),
        )
    }
}

// Handlers are thin: parse, call `desk::*`, map to a status.
//
// ponytail: `desk::*` blocks the calling worker on the database thread's
// channel. Sub-millisecond SQLite plus a few local file writes for one
// single-user loopback daemon, so no `spawn_blocking` hop; add one here if a
// desk operation ever grows long enough to starve a worker.

async fn health(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "daemon_uuid": state.daemon_uuid,
        "version": env!("CARGO_PKG_VERSION"),
        "started_at_ns": state.started_at_ns,
    }))
}

async fn list(State(state): State<Arc<ApiState>>) -> Result<Json<serde_json::Value>, DeskError> {
    let desks = desk::list(&state.store)?;
    Ok(Json(serde_json::json!({ "desks": desks })))
}

#[derive(Deserialize)]
struct NewDesk {
    name: String,
}

async fn create(
    State(state): State<Arc<ApiState>>,
    body: Result<Json<NewDesk>, JsonRejection>,
) -> Result<Response, DeskError> {
    // ponytail: an unusable body reuses DESK_NAME_INVALID because R0's only
    // request body is a desk name and §6 documents no generic bad-request code.
    // The first non-name POST body needs its own code, added by decision.
    let Ok(Json(body)) = body else {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "DESK_NAME_INVALID",
            r#"The request body must be a JSON object with a "name" string."#.to_string(),
        ));
    };
    // Creation is synchronous (§7.2): the row exists either way, so a bootstrap
    // failure is a 201 FAILED desk, not an envelope.
    let desk = desk::create(&state.store, &state.desks_home, &body.name)?;
    Ok((StatusCode::CREATED, Json(desk)).into_response())
}

async fn show(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<Desk>, DeskError> {
    Ok(Json(desk::get(&state.store, &desk_id)?))
}

async fn retry(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<Desk>, DeskError> {
    Ok(Json(desk::retry(&state.store, &desk_id)?))
}

// The two mutating market-plane routes (R1 feature SPEC §7). The body arrives as
// text so the `trading_actions` row keeps the caller's own request verbatim
// (§5); `trade` owns every rule about its content.

async fn submit_order(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    body: String,
) -> Result<Response, TradeError> {
    let (record, replayed) = trade::submit(&state.store, &state.registry, &desk_id, &body)?;
    let status = if replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(record)).into_response())
}

async fn cancel_order(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, client_order_id)): Path<(String, String)>,
    body: String,
) -> Result<Json<trade::ActionRecord>, TradeError> {
    Ok(Json(trade::cancel(
        &state.store,
        &state.registry,
        &desk_id,
        &client_order_id,
        &body,
    )?))
}

/// Answers, then asks the daemon to shut down (§4.2). A full or closed channel
/// means a stop is already under way, so a second `/quit` is a no-op.
async fn quit(State(state): State<Arc<ApiState>>) -> Response {
    let _ = state.quit.try_send(());
    (StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response()
}

// ---------------------------------------------------------------------------
// api::envelope_stability (feature SPEC §11)
// ---------------------------------------------------------------------------

#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[cfg(test)]
const DAEMON_UUID: &str = "01999999-0000-7000-8000-000000000001";

#[cfg(test)]
struct Served {
    _dir: tempfile::TempDir,
    desks_home: PathBuf,
    base: String,
    quit: tokio::sync::mpsc::Receiver<()>,
    store: Store,
    registry: Arc<crate::node::Registry>,
}

#[cfg(test)]
async fn serve() -> Served {
    // No feed base: these routes never start a node, and nothing may reach the
    // public endpoint from a test.
    serve_with(None).await
}

#[cfg(test)]
async fn serve_with(feed_base: Option<String>) -> Served {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("marketrig.sqlite3")).unwrap();
    let desks_home = dir.path().join("desks");
    std::fs::create_dir_all(&desks_home).unwrap();
    let (quit, quit_rx) = tokio::sync::mpsc::channel(1);
    let registry = Arc::new(crate::node::Registry::new(
        store.clone(),
        Arc::new(crate::feed::MarketState::new()),
        feed_base,
    ));
    let state = ApiState {
        store: store.clone(),
        desks_home: desks_home.clone(),
        daemon_uuid: DAEMON_UUID.to_string(),
        credential: CREDENTIAL.to_string(),
        started_at_ns: 1_700_000_000_000_000_000,
        quit,
        registry: registry.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Served {
        _dir: dir,
        desks_home,
        base,
        quit: quit_rx,
        store,
        registry,
    }
}

#[cfg(test)]
fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build(),
    )
}

#[cfg(test)]
fn read(response: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> (u16, String) {
    let mut response = response.unwrap();
    let status = response.status().as_u16();
    (status, response.body_mut().read_to_string().unwrap())
}

#[cfg(test)]
fn call_get(url: String, bearer: Option<&str>) -> (u16, String) {
    let mut request = agent().get(url);
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    read(request.call())
}

#[cfg(test)]
fn call_post(url: String, bearer: Option<&str>, body: Option<(&str, &str)>) -> (u16, String) {
    let mut request = agent().post(url);
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    read(match body {
        Some((content_type, payload)) => request.header("content-type", content_type).send(payload),
        None => request.send_empty(),
    })
}

#[cfg(test)]
fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{body:?} is not JSON: {e}"))
}

/// Asserts the one §6 envelope: exactly `code` + `message`, a documented code,
/// and an English sentence.
#[cfg(test)]
#[track_caller]
fn expect_envelope(answer: (u16, String), status: u16, code: &str) {
    let (got, body) = answer;
    assert_eq!(got, status, "status for {code}; body {body}");
    let value = json(&body);
    let object = value.as_object().expect("envelope is a JSON object");
    assert_eq!(
        object.keys().collect::<Vec<_>>(),
        ["code", "message"],
        "envelope carries exactly code and message: {value}"
    );
    assert_eq!(object["code"].as_str(), Some(code));
    let message = object["message"].as_str().expect("message is a string");
    assert!(
        message
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
            && message.ends_with('.'),
        "message must be an English sentence: {message:?}"
    );
}

#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn envelope_stability() {
    let mut served = serve().await;
    let base = served.base.clone();
    let url = |path: &str| format!("{base}{path}");
    let ok = Some(CREDENTIAL);

    // --- Happy paths, once each -------------------------------------------
    let (status, body) = call_get(url("/health"), ok);
    assert_eq!(status, 200);
    assert_eq!(
        json(&body),
        serde_json::json!({
            "daemon_uuid": DAEMON_UUID,
            "version": env!("CARGO_PKG_VERSION"),
            "started_at_ns": 1_700_000_000_000_000_000i64,
        })
    );

    let (status, body) = call_post(
        url("/desks"),
        ok,
        Some(("application/json", r#"{"name":"alpha"}"#)),
    );
    assert_eq!(status, 201, "{body}");
    let alpha = json(&body);
    assert_eq!(alpha["state"], "READY");
    assert_eq!(alpha["name"], "alpha");
    assert_eq!(alpha["workspace_status"], "OK");
    assert!(
        alpha["failure_code"].is_null(),
        "nulls are omitted: {alpha}"
    );
    let alpha_id = alpha["id"].as_str().unwrap().to_string();

    let (status, body) = call_get(url("/desks"), ok);
    assert_eq!(status, 200);
    assert_eq!(json(&body)["desks"], serde_json::json!([alpha]));

    let (status, body) = call_get(url(&format!("/desks/{alpha_id}")), ok);
    assert_eq!(status, 200);
    assert_eq!(json(&body), alpha);

    // A planted file obstructs the workspace: creation answers 201 with the
    // FAILED desk (the row exists), and retry recovers it on the same identity.
    std::fs::write(served.desks_home.join("beta"), "not a directory").unwrap();
    let (status, body) = call_post(
        url("/desks"),
        ok,
        Some(("application/json", r#"{"name":"beta"}"#)),
    );
    assert_eq!(status, 201, "{body}");
    let beta = json(&body);
    assert_eq!(beta["state"], "FAILED");
    assert!(beta["failure_code"].is_string() && beta["failure_message"].is_string());
    let beta_id = beta["id"].as_str().unwrap().to_string();

    std::fs::remove_file(served.desks_home.join("beta")).unwrap();
    let (status, body) = call_post(url(&format!("/desks/{beta_id}/retry")), ok, None);
    assert_eq!(status, 200, "{body}");
    let retried = json(&body);
    assert_eq!(retried["state"], "READY");
    assert_eq!(retried["id"], beta["id"]);
    assert!(retried["failure_code"].is_null());

    // --- Every error path answers the one envelope -------------------------
    let routes: [(&str, String); 6] = [
        ("GET", url("/health")),
        ("GET", url("/desks")),
        ("POST", url("/desks")),
        ("GET", url(&format!("/desks/{alpha_id}"))),
        ("POST", url(&format!("/desks/{alpha_id}/retry"))),
        ("POST", url("/quit")),
    ];
    for (method, route) in &routes {
        for bearer in [None, Some("wrong-credential")] {
            let answer = match *method {
                "GET" => call_get(route.clone(), bearer),
                _ => call_post(route.clone(), bearer, None),
            };
            expect_envelope(answer, 401, "UNAUTHORIZED");
        }
    }

    expect_envelope(
        call_post(
            url("/desks"),
            ok,
            Some(("application/json", r#"{"name":"Bad--Name"}"#)),
        ),
        400,
        "DESK_NAME_INVALID",
    );
    expect_envelope(
        call_post(
            url("/desks"),
            ok,
            Some(("application/json", r#"{"name":"alpha"}"#)),
        ),
        409,
        "DESK_NAME_TAKEN",
    );
    // An unusable create body is the same refusal: R0's only POST body is a name.
    for body in [
        Some(("application/json", "{")),
        Some(("application/json", r#"{"nome":"alpha"}"#)),
        Some(("text/plain", r#"{"name":"alpha"}"#)),
        None,
    ] {
        expect_envelope(call_post(url("/desks"), ok, body), 400, "DESK_NAME_INVALID");
    }

    for id in ["01999999-0000-7000-8000-0000000000ff", "not-a-uuid", "%20"] {
        expect_envelope(
            call_get(url(&format!("/desks/{id}")), ok),
            404,
            "DESK_NOT_FOUND",
        );
        expect_envelope(
            call_post(url(&format!("/desks/{id}/retry")), ok, None),
            404,
            "DESK_NOT_FOUND",
        );
    }
    expect_envelope(
        call_post(url(&format!("/desks/{alpha_id}/retry")), ok, None),
        409,
        "DESK_STATE_INVALID",
    );

    // --- Quit answers, then signals; repeats never panic --------------------
    let (status, body) = call_post(url("/quit"), ok, None);
    assert_eq!(status, 202);
    assert_eq!(json(&body), serde_json::json!({}));
    served.quit.try_recv().expect("quit was signalled");

    // Undrained (channel fills), then closed: both stay 202.
    assert_eq!(call_post(url("/quit"), ok, None).0, 202);
    assert_eq!(call_post(url("/quit"), ok, None).0, 202);
    served.quit.close();
    assert_eq!(call_post(url("/quit"), ok, None).0, 202);
}

// ---------------------------------------------------------------------------
// api::action_replay (R1 feature SPEC §11)
// ---------------------------------------------------------------------------

/// A repeated `action_id` returns the original record and acts on nothing — the
/// whole idempotency contract (R1-8, §6). Driven through the real route against
/// a real node on the scripted feed.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn action_replay() {
    let aapl = crate::catalog::find("AAPL.XNAS").unwrap();
    let (feed, _hits) = crate::feed::scripted_server(vec![(
        200,
        crate::feed::chart_body("AAPL", "USD", "316.85", 1_788_206_401),
    )]);
    let served = serve_with(Some(feed)).await;
    let base = served.base.clone();
    let url = |path: &str| format!("{base}{path}");
    let ok = Some(CREDENTIAL);

    let (status, body) = call_post(
        url("/desks"),
        ok,
        Some(("application/json", r#"{"name":"alpha"}"#)),
    );
    assert_eq!(status, 201, "{body}");
    let desk_id = json(&body)["id"].as_str().unwrap().to_string();

    // Start the node and wait for its first observation, so the order below
    // matches against a book rather than racing the feed.
    served.registry.ensure(&desk_id).expect("the node starts");
    let market = std::sync::Arc::clone(served.registry.market());
    crate::node::within(10, "the first observation", || {
        market.read(aapl, crate::store::now_ns()).sequence == 1
    });

    let orders = url(&format!("/desks/{desk_id}/orders"));
    // One lot: the synthesized book is one lot a side (§4.1), so this is exactly
    // one order and exactly one fill.
    let request = r#"{"action_id":"buy-aapl-1","instrument_id":"AAPL.XNAS",
                      "side":"BUY","type":"MARKET","quantity":"1","price":null}"#;

    // First acceptance: 201, one order in the sandbox.
    let (status, body) = call_post(orders.clone(), ok, Some(("application/json", request)));
    assert_eq!(status, 201, "{body}");
    let record = json(&body);
    // The ActionRecord is the trading_actions row and nothing else (§7); the
    // keys come back sorted because `serde_json::Value` is a map.
    assert_eq!(
        record.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["action_id", "created_at_ns", "id", "kind", "outcome"],
        "the ActionRecord is the trading_actions row: {record}"
    );
    assert_eq!(record["action_id"], "buy-aapl-1");
    assert_eq!(record["kind"], "SUBMIT");
    assert_eq!(record["outcome"]["client_order_id"], "buy-aapl-1");
    assert_eq!(record["outcome"]["status"], "FILLED");
    assert_eq!(record["outcome"]["filled_quantity"], "1");
    assert_eq!(record["outcome"]["average_price"], "316.85");

    let orders_placed = |served: &Served| {
        served
            .store
            .call(|c| {
                c.query_row("SELECT count(*) FROM trading_actions", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .unwrap()
    };
    let fills = |served: &Served| {
        served
            .store
            .call(|c| c.query_row("SELECT count(*) FROM fills", [], |r| r.get::<_, i64>(0)))
            .unwrap()
    };
    let sandbox_orders = |served: &Served| {
        served
            .registry
            .ensure(&desk_id)
            .unwrap()
            .call(|context| {
                context
                    .cache
                    .borrow()
                    .orders(None, None, None, None, None)
                    .len()
            })
            .unwrap()
    };
    assert_eq!((orders_placed(&served), fills(&served)), (1, 1));
    assert_eq!(sandbox_orders(&served), 1, "one order reached the sandbox");

    // The same action_id again: 200, byte-identical record, no second order.
    let (status, replay) = call_post(orders.clone(), ok, Some(("application/json", request)));
    assert_eq!(status, 200, "{replay}");
    assert_eq!(json(&replay), record, "the stored record, byte for byte");
    assert_eq!((orders_placed(&served), fills(&served)), (1, 1));
    assert_eq!(sandbox_orders(&served), 1, "and created no second order");

    // A different body under the same action_id replays too: the record, not the
    // request, is the contract.
    let (status, replay) = call_post(
        orders.clone(),
        ok,
        Some((
            "application/json",
            r#"{"action_id":"buy-aapl-1","instrument_id":"MSFT.XNAS",
                "side":"SELL","type":"LIMIT","quantity":"1","price":"1.00"}"#,
        )),
    );
    assert_eq!(status, 200, "{replay}");
    assert_eq!(json(&replay), record);
    assert_eq!((orders_placed(&served), fills(&served)), (1, 1));
    assert_eq!(sandbox_orders(&served), 1);

    // And a fresh action_id still acts.
    let (status, body) = call_post(
        orders,
        ok,
        Some((
            "application/json",
            r#"{"action_id":"buy-aapl-2","instrument_id":"AAPL.XNAS",
                "side":"BUY","type":"LIMIT","quantity":"5","price":"200.00"}"#,
        )),
    );
    assert_eq!(status, 201, "{body}");
    assert_eq!(json(&body)["outcome"]["status"], "ACCEPTED");
    assert_eq!(orders_placed(&served), 2);

    served.registry.stop_all();
}
