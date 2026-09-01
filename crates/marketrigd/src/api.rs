//! The loopback REST surface.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §6 (routes,
//! `Desk` resource, error envelope), §4.2 (`POST /quit`), §5.2 (`GET /health`
//! serves client verification); `sdd/features/r1-equity-paper-trading/SPEC.md`
//! §7 (the market-plane and history additions).

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
use crate::store::{self, Store};
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
        .route("/desks/{desk_id}/market/instruments", get(instruments))
        .route("/desks/{desk_id}/market/quotes", get(quotes))
        .route("/desks/{desk_id}/market/book", get(book))
        .route("/desks/{desk_id}/positions", get(positions))
        .route(
            "/desks/{desk_id}/orders",
            get(open_orders).post(submit_order),
        )
        .route(
            "/desks/{desk_id}/orders/{client_order_id}/cancel",
            post(cancel_order),
        )
        .route("/desks/{desk_id}/history/orders", get(history_orders))
        .route("/desks/{desk_id}/history/fills", get(history_fills))
        .route("/desks/{desk_id}/history/cycles", get(history_cycles))
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

// The market-plane reads (R1 feature SPEC §7). The catalog is compiled in and the
// history tables are the daemon's own, so those routes start no node; every
// live-state read starts the desk's node lazily (§4.3) and answers
// `MARKET_UNAVAILABLE` when it cannot. The market plane needs a READY desk, the
// same rule the order routes carry (§4.2).

async fn instruments(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TradeError> {
    trade::require_ready(&state.store, &desk_id)?;
    Ok(Json(
        serde_json::json!({ "instruments": crate::catalog::ENTRIES }),
    ))
}

async fn quotes(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TradeError> {
    trade::require_ready(&state.store, &desk_id)?;
    state.registry.ensure(&desk_id)?;
    let quotes = state.registry.market().read_all(store::now_ns());
    Ok(Json(serde_json::json!({ "quotes": quotes })))
}

async fn book(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TradeError> {
    trade::require_ready(&state.store, &desk_id)?;
    state.registry.ensure(&desk_id)?;
    let book = state.registry.market().book_all(store::now_ns());
    Ok(Json(serde_json::json!({ "book": book })))
}

async fn positions(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TradeError> {
    trade::require_ready(&state.store, &desk_id)?;
    let node = state.registry.ensure(&desk_id)?;
    Ok(Json(
        serde_json::json!({ "positions": trade::open_positions(&node)? }),
    ))
}

async fn open_orders(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TradeError> {
    trade::require_ready(&state.store, &desk_id)?;
    let node = state.registry.ensure(&desk_id)?;
    Ok(Json(
        serde_json::json!({ "orders": trade::open_orders(&node)? }),
    ))
}

// The history group (§7): complete newest-first projections over the desk's own
// event tables, which exist whatever the desk's state.

async fn history_orders(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, DeskError> {
    desk::get(&state.store, &desk_id)?;
    let orders = trade::history_orders(&state.store, &desk_id)?;
    Ok(Json(serde_json::json!({ "orders": orders })))
}

async fn history_fills(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, DeskError> {
    desk::get(&state.store, &desk_id)?;
    let fills = trade::history_fills(&state.store, &desk_id)?;
    Ok(Json(serde_json::json!({ "fills": fills })))
}

async fn history_cycles(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, DeskError> {
    desk::get(&state.store, &desk_id)?;
    let cycles = trade::history_cycles(&state.store, &desk_id)?;
    Ok(Json(serde_json::json!({ "cycles": cycles })))
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
async fn serve_with(feed_base: Option<crate::feed::FeedBase>) -> Served {
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
    let served = serve_with(Some(crate::feed::FeedBase::standin(feed))).await;
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

// ---------------------------------------------------------------------------
// api::market_codes (R1 feature SPEC §11)
// ---------------------------------------------------------------------------

/// The §7 reads: each answers its documented body key, and every documented
/// error path answers the one envelope with its documented code. Driven through
/// the real routes against one real node on the scripted feed.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn market_codes() {
    let (feed, _hits) = crate::feed::scripted_server(vec![(
        200,
        crate::feed::chart_body("AAPL", "USD", "316.85", 1_788_206_401),
    )]);
    let served = serve_with(Some(crate::feed::FeedBase::standin(feed))).await;
    let base = served.base.clone();
    let url = |path: &str| format!("{base}{path}");
    let ok = Some(CREDENTIAL);
    let create = |name: &str| {
        let (status, body) = call_post(
            url("/desks"),
            ok,
            Some(("application/json", &format!(r#"{{"name":"{name}"}}"#))),
        );
        assert_eq!(status, 201, "{body}");
        json(&body)
    };

    let alpha = create("alpha")["id"].as_str().unwrap().to_string();
    // A planted file leaves this desk FAILED: not READY, so not tradable.
    std::fs::write(served.desks_home.join("beta"), "not a directory").unwrap();
    let beta = create("beta");
    assert_eq!(beta["state"], "FAILED");
    let beta = beta["id"].as_str().unwrap().to_string();
    // READY, but its book snapshot carries a payload version this build cannot
    // restore, so its node never starts (the §4.3 start failure).
    let gamma = create("gamma")["id"].as_str().unwrap().to_string();
    let poisoned = gamma.clone();
    served
        .store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO book_snapshots VALUES (?1, 99, '{}', 3000)",
                [poisoned],
            )
        })
        .unwrap();

    // The node-backed reads (§7) and the daemon-local ones.
    let live = ["/market/quotes", "/market/book", "/positions", "/orders"];
    let local = [
        "/market/instruments",
        "/history/orders",
        "/history/fills",
        "/history/cycles",
    ];
    let paths: Vec<&str> = live.iter().chain(local.iter()).copied().collect();

    // --- Every new route sits behind the bearer ----------------------------
    for path in &paths {
        for bearer in [None, Some("wrong-credential")] {
            expect_envelope(
                call_get(url(&format!("/desks/{alpha}{path}")), bearer),
                401,
                "UNAUTHORIZED",
            );
        }
    }

    // --- An unknown desk is R0's own refusal, on every route ---------------
    for path in &paths {
        for id in ["01999999-0000-7000-8000-0000000000ff", "not-a-uuid"] {
            expect_envelope(
                call_get(url(&format!("/desks/{id}{path}")), ok),
                404,
                "DESK_NOT_FOUND",
            );
        }
    }

    // --- A desk that is not READY refuses the market plane (§4.2) ----------
    for path in live.iter().chain(["/market/instruments"].iter()) {
        expect_envelope(
            call_get(url(&format!("/desks/{beta}{path}")), ok),
            409,
            "DESK_NOT_READY",
        );
    }
    expect_envelope(
        call_post(
            url(&format!("/desks/{beta}/orders")),
            ok,
            Some((
                "application/json",
                r#"{"action_id":"a-1","instrument_id":"AAPL.XNAS",
                    "side":"BUY","type":"MARKET","quantity":"1","price":null}"#,
            )),
        ),
        409,
        "DESK_NOT_READY",
    );
    // History is daemon-local and answers a FAILED desk its (empty) rows.
    for what in ["orders", "fills", "cycles"] {
        let (status, body) = call_get(url(&format!("/desks/{beta}/history/{what}")), ok);
        assert_eq!(status, 200, "{body}");
        assert_eq!(json(&body)[what].as_array().map(Vec::len), Some(0));
    }

    // --- A node that will not start is MARKET_UNAVAILABLE, and only there ---
    for path in &live {
        expect_envelope(
            call_get(url(&format!("/desks/{gamma}{path}")), ok),
            503,
            "MARKET_UNAVAILABLE",
        );
    }
    for path in &local {
        assert_eq!(call_get(url(&format!("/desks/{gamma}{path}")), ok).0, 200);
    }

    // --- The catalog (§3) ---------------------------------------------------
    let (status, body) = call_get(url(&format!("/desks/{alpha}/market/instruments")), ok);
    assert_eq!(status, 200);
    let instruments = json(&body)["instruments"].clone();
    assert_eq!(
        instruments.as_array().map(Vec::len),
        Some(crate::catalog::ENTRIES.len())
    );
    assert_eq!(instruments[0]["instrument_id"], "AAPL.XNAS");
    assert_eq!(instruments[0]["yahoo_symbol"], "AAPL");
    assert_eq!(instruments[0]["price_increment"], "0.01");
    assert_eq!(instruments[0]["lot_size"], 1);

    // --- Quotes: the first read starts the node (§4.3), and shows §2.3 ------
    let quotes_of = |desk: &str| {
        let (status, body) = call_get(url(&format!("/desks/{desk}/market/quotes")), ok);
        assert_eq!(status, 200, "{body}");
        json(&body)["quotes"].clone()
    };
    crate::node::within(10, "the first observation through the route", || {
        quotes_of(&alpha)[0]["sequence"] == serde_json::json!(1)
    });
    let quotes = quotes_of(&alpha);
    assert_eq!(
        quotes.as_array().map(Vec::len),
        Some(crate::catalog::ENTRIES.len())
    );
    let aapl = quotes[0].clone();
    assert_eq!(aapl["instrument_id"], "AAPL.XNAS");
    assert_eq!(aapl["provider"], "yahoo");
    assert_eq!(aapl["venue"], "XNAS");
    assert_eq!(aapl["last"], "316.85");
    assert_eq!(aapl["currency"], "USD");
    assert_eq!(aapl["health"], "LIVE");
    assert_eq!(aapl["book_synthesized"], true);
    for field in [
        "source_time_ns",
        "received_at_ns",
        "read_at_ns",
        "age_ms",
        "sequence",
        "market_phase",
    ] {
        assert!(!aapl[field].is_null(), "§2.3 field {field}: {aapl}");
    }

    // --- The synthesized book (§4.1): bid = ask = last, one lot a side ------
    let (status, body) = call_get(url(&format!("/desks/{alpha}/market/book")), ok);
    assert_eq!(status, 200);
    let top = json(&body)["book"][0].clone();
    assert_eq!(top["instrument_id"], "AAPL.XNAS");
    assert_eq!(top["bid_price"], "316.85");
    assert_eq!(top["ask_price"], "316.85");
    assert_eq!(top["bid_size"], "1");
    assert_eq!(top["ask_size"], "1");
    assert_eq!(top["book_synthesized"], true);
    // An instrument nothing has observed carries no price or size fields at
    // all, exactly as §2.3 omits them.
    let dark = crate::feed::MarketState::new().book_all(1_788_206_401_000_000_000);
    let unobserved = serde_json::to_value(&dark[0]).unwrap();
    assert_eq!(unobserved["health"], "UNAVAILABLE");
    for field in [
        "last",
        "currency",
        "bid_price",
        "ask_price",
        "bid_size",
        "ask_size",
        "age_ms",
    ] {
        assert!(unobserved.get(field).is_none(), "{field}: {unobserved}");
    }
    assert_eq!(unobserved["book_synthesized"], true);
    assert_eq!(unobserved["sequence"], 0);

    // --- Live positions and open orders (§7), through one round trip -------
    let orders_url = url(&format!("/desks/{alpha}/orders"));
    let submit =
        |body: &str| json(&call_post(orders_url.clone(), ok, Some(("application/json", body))).1);
    // One lot: the synthesized book is one lot a side (§4.1), so this fills whole.
    let filled = submit(
        r#"{"action_id":"buy-aapl-1","instrument_id":"AAPL.XNAS",
            "side":"BUY","type":"MARKET","quantity":"1","price":null}"#,
    );
    assert_eq!(filled["outcome"]["status"], "FILLED", "{filled}");
    let resting = submit(
        r#"{"action_id":"rest-aapl-1","instrument_id":"AAPL.XNAS",
            "side":"BUY","type":"LIMIT","quantity":"5","price":"200.00"}"#,
    );
    assert_eq!(resting["outcome"]["status"], "ACCEPTED", "{resting}");

    let (status, body) = call_get(url(&format!("/desks/{alpha}/positions")), ok);
    assert_eq!(status, 200);
    let positions = json(&body)["positions"].clone();
    assert_eq!(positions.as_array().map(Vec::len), Some(1), "{positions}");
    assert_eq!(positions[0]["instrument_id"], "AAPL.XNAS");
    assert_eq!(positions[0]["side"], "LONG");
    assert_eq!(positions[0]["quantity"], "1");
    assert_eq!(positions[0]["average_open_price"], "316.85");
    assert_eq!(positions[0]["currency"], "USD");
    assert!(!positions[0]["opened_at_ns"].is_null());

    let (status, body) = call_get(orders_url.clone(), ok);
    assert_eq!(status, 200);
    let open = json(&body)["orders"].clone();
    assert_eq!(open.as_array().map(Vec::len), Some(1), "{open}");
    assert_eq!(open[0]["client_order_id"], "rest-aapl-1");
    assert_eq!(open[0]["status"], "ACCEPTED");
    assert_eq!(open[0]["price"], "200.00");

    // Flat again: the closing fill closes the cycle (§6).
    let sold = submit(
        r#"{"action_id":"sell-aapl-1","instrument_id":"AAPL.XNAS",
            "side":"SELL","type":"MARKET","quantity":"1","price":null}"#,
    );
    assert_eq!(sold["outcome"]["status"], "FILLED", "{sold}");
    let positions = json(&call_get(url(&format!("/desks/{alpha}/positions")), ok).1)["positions"]
        .as_array()
        .map(Vec::len);
    assert_eq!(positions, Some(0), "the desk is flat again");

    // --- History: complete, newest first, over the event tables (§5) --------
    let history = |what: &str| {
        let (status, body) = call_get(url(&format!("/desks/{alpha}/history/{what}")), ok);
        assert_eq!(status, 200, "{body}");
        json(&body)[what].clone()
    };
    let orders = history("orders");
    assert_eq!(orders.as_array().map(Vec::len), Some(3), "{orders}");
    assert_eq!(orders[0]["client_order_id"], "sell-aapl-1");
    assert_eq!(orders[0]["status"], "FILLED");
    assert_eq!(orders[0]["side"], "SELL");
    assert_eq!(orders[0]["average_price"], "316.85");
    assert_eq!(orders[1]["client_order_id"], "rest-aapl-1");
    assert_eq!(orders[1]["status"], "ACCEPTED");
    assert_eq!(orders[2]["client_order_id"], "buy-aapl-1");
    assert_eq!(orders[2]["filled_quantity"], "1");

    let fills = history("fills");
    assert_eq!(fills.as_array().map(Vec::len), Some(2), "{fills}");
    assert_eq!(fills[0]["client_order_id"], "sell-aapl-1");
    assert_eq!(fills[0]["side"], "SELL");
    assert_eq!(fills[0]["quantity"], "1");
    assert_eq!(fills[0]["price"], "316.85");
    assert_eq!(fills[0]["currency"], "USD");
    assert_eq!(fills[0]["commission"], "0.00", "the US rate is 0 bp");
    assert!(fills[0]["trade_id"].is_string() && fills[0]["id"].is_string());
    assert!(
        fills[0]["occurred_at_ns"].as_i64() >= fills[1]["occurred_at_ns"].as_i64(),
        "newest first: {fills}"
    );

    let cycles = history("cycles");
    assert_eq!(cycles.as_array().map(Vec::len), Some(1), "{cycles}");
    assert_eq!(cycles[0]["instrument_id"], "AAPL.XNAS");
    assert_eq!(cycles[0]["currency"], "USD");
    assert_eq!(cycles[0]["realized_pnl"], "0.00", "bought and sold at last");
    assert!(cycles[0]["position_id"].is_string() && cycles[0]["id"].is_string());
    assert!(cycles[0]["closed_at_ns"].as_i64() >= cycles[0]["opened_at_ns"].as_i64());

    // --- The order path's own refusals, through the same fixture -----------
    expect_envelope(
        call_post(
            orders_url.clone(),
            ok,
            Some((
                "application/json",
                r#"{"action_id":"unknown-1","instrument_id":"NOPE.XNAS",
                    "side":"BUY","type":"MARKET","quantity":"1","price":null}"#,
            )),
        ),
        404,
        "INSTRUMENT_UNKNOWN",
    );
    expect_envelope(
        call_post(
            orders_url.clone(),
            ok,
            Some((
                "application/json",
                r#"{"action_id":"off-lot-1","instrument_id":"0700.XHKG",
                    "side":"BUY","type":"MARKET","quantity":"150","price":null}"#,
            )),
        ),
        400,
        "ORDER_INVALID",
    );
    // Beyond the desk's 100,000 USD: the sandbox's own refusal, verbatim.
    expect_envelope(
        call_post(
            orders_url,
            ok,
            Some((
                "application/json",
                r#"{"action_id":"too-big-1","instrument_id":"AAPL.XNAS",
                    "side":"BUY","type":"MARKET","quantity":"1000","price":null}"#,
            )),
        ),
        409,
        "ORDER_REJECTED",
    );
    expect_envelope(
        call_post(
            url(&format!("/desks/{alpha}/orders/no-such-order/cancel")),
            ok,
            Some(("application/json", r#"{"action_id":"cancel-unknown-1"}"#)),
        ),
        404,
        "ORDER_NOT_FOUND",
    );

    served.registry.stop_all();
}
