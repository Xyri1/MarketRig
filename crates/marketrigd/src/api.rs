//! The loopback REST surface.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §6 (routes,
//! `Desk` resource, error envelope), §4.2 (`POST /quit`), §5.2 (`GET /health`
//! serves client verification); `sdd/features/r1-equity-paper-trading/SPEC.md`
//! §7 (the market-plane and history additions).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::desk::{self, Desk, DeskError};
use crate::memory::{self, MemoryError};
use crate::policy::{self, PolicyError};
use crate::store::{self, Store};
use crate::trade::{self, TradeError};
use crate::trigger::{self, TriggerError};

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
    /// Wakes the scheduler after a trigger mutation (R2 feature SPEC §3.1).
    pub scheduler_wake: Arc<tokio::sync::Notify>,
    /// The `PATH` runtime discovery searches, captured once at daemon start
    /// (R3 feature SPEC §2).
    pub search_path: String,
    /// The one terminal manager both adapters spawn through (R3 feature SPEC §3).
    pub terminals: Arc<crate::terminal::Manager>,
    /// The desks' Claude Code channel connections (R3 feature SPEC §5.3).
    pub channels: Arc<crate::claude::Channels>,
    /// Activation policy and the delivery queue, shared with the dispatcher
    /// task so a route and the queue start sessions exactly one way (§6, §7).
    pub dispatch: Arc<crate::dispatch::Dispatcher>,
    /// The memory child, its provider settings, and the desk-scoped operations
    /// (R4 feature SPEC §2, §3, §4).
    pub memory: Arc<crate::memory::Memory>,
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
        .route(
            "/desks/{desk_id}/triggers",
            get(list_triggers).post(create_trigger),
        )
        .route(
            "/desks/{desk_id}/triggers/{trigger_id}",
            get(show_trigger)
                .patch(patch_trigger)
                .delete(delete_trigger),
        )
        .route(
            "/desks/{desk_id}/triggers/{trigger_id}/firings",
            get(trigger_firings),
        )
        .route("/desks/{desk_id}/firings/{firing_id}", get(show_firing))
        .route("/desks/{desk_id}/session", get(session))
        .route("/desks/{desk_id}/session/activate", post(session_activate))
        .route(
            "/desks/{desk_id}/session/interrupt",
            post(session_interrupt),
        )
        .route("/desks/{desk_id}/session/exit", post(session_exit))
        .route("/desks/{desk_id}/session/switch", post(session_switch))
        .route("/desks/{desk_id}/terminal", get(terminal))
        .route("/desks/{desk_id}/channel", get(channel))
        .route("/desks/{desk_id}/session/hook", post(session_hook))
        .route("/runtimes", get(runtimes))
        .route("/runtimes/{runtime}/discover", post(runtime_discover))
        .route("/runtimes/{runtime}/retry", post(runtime_retry))
        .route("/memory", get(memory_status))
        .route("/memory/provider", put(memory_provider))
        .route("/memory/provider/models", get(memory_models))
        .route("/memory/discover", post(memory_discover))
        .route("/memory/retry", post(memory_retry))
        .route("/desks/{desk_id}/memory", get(desk_memory))
        .route("/desks/{desk_id}/memory/retain", post(memory_retain))
        .route("/desks/{desk_id}/memory/recall", post(memory_recall))
        .route("/desks/{desk_id}/memory/reflect", post(memory_reflect))
        .route("/desks/{desk_id}/prompts", get(list_prompts))
        .route("/desks/{desk_id}/prompts/{prompt_id}", get(show_prompt))
        .route("/settings/policies", get(policies).put(put_policies))
        .route("/quit", post(quit))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

/// A JSON request body's content type, checked only after the body has been
/// consumed — answering with bytes still unread makes the close a reset, which
/// Windows reports to the client in place of the envelope.
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|m| {
            m == "application/json" || (m.starts_with("application/") && m.ends_with("+json"))
        })
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
            TradeError::Invalid(_) | TradeError::Attribution(_) => StatusCode::BAD_REQUEST,
            TradeError::InstrumentUnknown(_) | TradeError::OrderNotFound(_) => {
                StatusCode::NOT_FOUND
            }
            TradeError::Rejected(_) | TradeError::NotReady(_) => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        envelope(status, self.code(), self.to_string())
    }
}

/// The R2 §8 code-to-status map, appended the same way. `TriggerError::code()`
/// owns the code; this owns the status.
impl IntoResponse for TriggerError {
    fn into_response(self) -> Response {
        // A desk lookup or store failure keeps R0's own mapping.
        if let TriggerError::Desk(e) = self {
            return e.into_response();
        }
        let status = match &self {
            TriggerError::Invalid(_) => StatusCode::BAD_REQUEST,
            TriggerError::NameTaken(_) | TriggerError::NotReady(_) => StatusCode::CONFLICT,
            _ => StatusCode::NOT_FOUND,
        };
        envelope(status, self.code(), self.to_string())
    }
}

/// The R4 §3 and §4.3 code-to-status map, appended the same way.
/// `MemoryError::code()` owns the code; this owns the status.
impl IntoResponse for MemoryError {
    fn into_response(self) -> Response {
        // A desk lookup, a desk's state, or a bad attribution keeps R1's map.
        if let MemoryError::Desk(e) = self {
            return e.into_response();
        }
        let status = match &self {
            MemoryError::Validation(_) => StatusCode::BAD_REQUEST,
            MemoryError::Unconfigured | MemoryError::EmbeddingModelLocked => StatusCode::CONFLICT,
            MemoryError::Rejected(_) => StatusCode::UNPROCESSABLE_ENTITY,
            MemoryError::Unavailable(_) | MemoryError::CredentialStoreUnavailable(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            MemoryError::Timeout => StatusCode::GATEWAY_TIMEOUT,
            MemoryError::Error(_) | MemoryError::ProviderUnreachable(_) => StatusCode::BAD_GATEWAY,
            MemoryError::Desk(_) => unreachable!("answered above"),
        };
        envelope(status, self.code(), self.to_string())
    }
}

/// The R5 §2 code-to-status map, appended the same way. `PolicyError::code()`
/// owns the code; this owns the status.
impl IntoResponse for PolicyError {
    fn into_response(self) -> Response {
        let status = match &self {
            PolicyError::Validation(_) => StatusCode::BAD_REQUEST,
            PolicyError::SteerDisabled => StatusCode::CONFLICT,
            PolicyError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
    /// The runtime the desk activates on; `codex` by omission (R3 §7).
    #[serde(default)]
    runtime: Option<String>,
}

async fn create(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, DeskError> {
    // The body is consumed before any refusal: answering with bytes still
    // unread makes the close a reset, which Windows reports to the client in
    // place of the envelope.
    let json = is_json(&headers);
    // ponytail: an unusable body reuses DESK_NAME_INVALID because R0's only
    // request body is a desk name and §6 documents no generic bad-request code.
    // The first non-name POST body needs its own code, added by decision.
    let Some(body) = json
        .then(|| serde_json::from_str::<NewDesk>(&body).ok())
        .flatten()
    else {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "DESK_NAME_INVALID",
            r#"The request body must be a JSON object with a "name" string."#.to_string(),
        ));
    };
    // Creation is synchronous (§7.2): the row exists either way, so a bootstrap
    // failure is a 201 FAILED desk, not an envelope.
    let selected_runtime = body.runtime.as_deref().unwrap_or("codex");
    if !crate::runtime::known(selected_runtime) {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "VALIDATION",
            format!("Unknown runtime {selected_runtime:?}; MarketRig knows codex and claude."),
        ));
    }
    let desk = desk::create(
        &state.store,
        &state.desks_home,
        &body.name,
        selected_runtime,
    )?;
    Ok((StatusCode::CREATED, Json(desk)).into_response())
}

async fn show(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<Desk>, DeskError> {
    let mut desk = desk::get(&state.store, &desk_id)?;
    desk.native_sessions = Some(crate::session::pointers(&state.store, &desk.id)?);
    Ok(Json(desk))
}

// The R3 session and runtime surface (R3 feature SPEC §5.2, §7). The lifecycle
// controls — activate, interrupt, exit, switch — arrive with the dispatcher.

async fn session(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, DeskError> {
    let desk = desk::get(&state.store, &desk_id)?;
    let process = crate::session::live_process(&state.store, &desk.id)?;
    Ok(Json(serde_json::json!({ "process": process })))
}

/// §7's lifecycle controls. Activation itself lives in `dispatch` so the route
/// and the dispatcher share one path.

#[derive(Deserialize)]
struct ActivateBody {
    mode: String,
}

async fn session_activate(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, DeskError> {
    let mode = match is_json(&headers)
        .then(|| serde_json::from_str::<ActivateBody>(&body).ok())
        .flatten()
        .as_ref()
        .map(|b| b.mode.as_str())
    {
        Some("CONTINUE") => crate::dispatch::Mode::Continue,
        Some("NEW") => crate::dispatch::Mode::New,
        _ => {
            return Ok(envelope(
                StatusCode::BAD_REQUEST,
                "VALIDATION",
                r#"The body must be {"mode":"CONTINUE"|"NEW"}."#.to_string(),
            ));
        }
    };
    use crate::dispatch::ActivateError;
    Ok(match state.dispatch.activate(&desk_id, mode).await {
        Ok(process) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "process": process })),
        )
            .into_response(),
        Err(ActivateError::Desk(e)) => e.into_response(),
        Err(ActivateError::SessionLive) => envelope(
            StatusCode::CONFLICT,
            "SESSION_LIVE",
            "The desk already has a live session.".to_string(),
        ),
        Err(ActivateError::NoNativeSession) => envelope(
            StatusCode::CONFLICT,
            "NO_NATIVE_SESSION",
            "The desk has no session to continue on this runtime.".to_string(),
        ),
        Err(ActivateError::RuntimeUnavailable(runtime)) => envelope(
            StatusCode::CONFLICT,
            "RUNTIME_UNAVAILABLE",
            format!("The {runtime} runtime is not available."),
        ),
        Err(ActivateError::Spawn(message)) => envelope(
            StatusCode::CONFLICT,
            "RUNTIME_UNAVAILABLE",
            format!("The runtime could not be started: {message}"),
        ),
    })
}

async fn session_interrupt(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Response, DeskError> {
    let desk = desk::get(&state.store, &desk_id)?;
    if crate::session::live_process(&state.store, &desk.id)?.is_none() {
        return Ok(envelope(
            StatusCode::CONFLICT,
            "NO_LIVE_SESSION",
            "The desk has no live session.".to_string(),
        ));
    }
    let adapter = state.dispatch.adapter(&desk.selected_runtime);
    Ok(match adapter.interrupt(&desk.id).await {
        Ok(turn_id) => {
            let (id, turn) = (desk.id.clone(), turn_id.clone());
            state.store.unit(move |tx| {
                desk::append_event(
                    tx,
                    "SESSION_INTERRUPTED",
                    Some(&id),
                    store::now_ns(),
                    serde_json::json!({ "turn_id": turn }),
                )
            })?;
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "turn_id": turn_id })),
            )
                .into_response()
        }
        Err(("RUNTIME_ERROR", message)) => {
            envelope(StatusCode::BAD_GATEWAY, "RUNTIME_ERROR", message)
        }
        Err((code, message)) => envelope(StatusCode::CONFLICT, code, message),
    })
}

async fn session_exit(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Response, DeskError> {
    let desk = desk::get(&state.store, &desk_id)?;
    if crate::session::live_process(&state.store, &desk.id)?.is_none() {
        return Ok(envelope(
            StatusCode::CONFLICT,
            "NO_LIVE_SESSION",
            "The desk has no live session.".to_string(),
        ));
    }
    state.dispatch.exit(&desk.id).await?;
    Ok(if state.dispatch.await_closed(&desk.id).await {
        (StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response()
    } else {
        envelope(
            StatusCode::BAD_GATEWAY,
            "RUNTIME_ERROR",
            "The session did not end within five seconds; its shutdown continues.".to_string(),
        )
    })
}

#[derive(Deserialize)]
struct SwitchBody {
    runtime: String,
}

async fn session_switch(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, DeskError> {
    let desk = desk::get(&state.store, &desk_id)?;
    let Some(target) = is_json(&headers)
        .then(|| serde_json::from_str::<SwitchBody>(&body).ok())
        .flatten()
        .map(|b| b.runtime)
        .filter(|runtime| crate::runtime::known(runtime))
    else {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "VALIDATION",
            r#"The body must be {"runtime":"codex"|"claude"}."#.to_string(),
        ));
    };
    // §7: everything is validated before anything is stopped.
    if target == desk.selected_runtime {
        return Ok(envelope(
            StatusCode::CONFLICT,
            "SAME_RUNTIME",
            format!("The desk is already on {target}."),
        ));
    }
    match crate::runtime::get(&state.store, &target)? {
        Some(row) if row.state == "AVAILABLE" => {}
        _ => {
            return Ok(envelope(
                StatusCode::CONFLICT,
                "RUNTIME_UNAVAILABLE",
                format!("The {target} runtime is not available."),
            ));
        }
    }
    if crate::session::live_process(&state.store, &desk.id)?.is_some() {
        state.dispatch.exit(&desk.id).await?;
        if !state.dispatch.await_closed(&desk.id).await {
            return Ok(envelope(
                StatusCode::BAD_GATEWAY,
                "RUNTIME_ERROR",
                "The session did not end within five seconds; its shutdown continues.".to_string(),
            ));
        }
    }
    let (id, from, to) = (
        desk.id.clone(),
        desk.selected_runtime.clone(),
        target.clone(),
    );
    state.store.unit(move |tx| {
        tx.execute(
            "UPDATE desks SET selected_runtime = ?2 WHERE id = ?1",
            rusqlite::params![id, to],
        )?;
        desk::append_event(
            tx,
            "RUNTIME_SWITCHED",
            Some(&id),
            store::now_ns(),
            serde_json::json!({ "from": from, "to": to }),
        )
    })?;
    Ok(Json(serde_json::json!({
        "selected_runtime": target,
        "pointers": crate::session::pointers(&state.store, &desk.id)?,
    }))
    .into_response())
}

/// Claude Code's hook ingress (§5.2). Well-formed objects are always `202`;
/// only an unparseable body is refused, and the CLI swallows that too.
async fn session_hook(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, DeskError> {
    if !is_json(&headers) {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "VALIDATION",
            "The hook body must be a JSON object.".to_string(),
        ));
    }
    Ok(
        match crate::session::hook(&state.store, &desk_id, &body, state.channels.events())? {
            crate::session::Hook::Accepted => {
                (StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response()
            }
            crate::session::Hook::Unparseable => envelope(
                StatusCode::BAD_REQUEST,
                "VALIDATION",
                "The hook body must be a JSON object.".to_string(),
            ),
        },
    )
}

/// `GET /desks/{desk_id}/channel` (R3 feature SPEC §5.3): the Claude Code
/// bridge's socket. The connection itself is the session's readiness, so a
/// connection with no open process row is closed `4002` rather than served.
async fn channel(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    request: Request,
) -> Result<Response, DeskError> {
    use axum::extract::FromRequestParts;
    let desk = desk::get(&state.store, &desk_id)?;
    let (mut parts, _) = request.into_parts();
    let Ok(upgrade) =
        axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &()).await
    else {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "VALIDATION",
            "This route serves a WebSocket upgrade.".to_string(),
        ));
    };
    Ok(upgrade.on_upgrade(move |socket| bridged(socket, state, desk.id)))
}

/// One bridge connection: readiness, then the desk's frames out, until the
/// connection is superseded (`4001`) or either side goes away.
async fn bridged(mut socket: axum::extract::ws::WebSocket, state: Arc<ApiState>, desk_id: String) {
    use axum::extract::ws::{CloseFrame, Message};

    // §5.3: no open process, no session to be ready for — except while the
    // launch is still in flight, when the row is moments away (§6.1).
    match crate::session::live_process(&state.store, &desk_id) {
        Ok(Some(_)) => {}
        _ if state.channels.is_spawning(&desk_id) => {}
        _ => {
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 4002,
                    reason: "no live session".into(),
                })))
                .await;
            return;
        }
    }
    let (generation, mut frames) = state.channels.connect(&desk_id);
    loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // Superseded: the newer connection owns the desk now.
                None => {
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 4001,
                            reason: "superseded".into(),
                        })))
                        .await;
                    return;
                }
            },
            // The bridge sends nothing; its socket closing is the signal.
            client = socket.recv() => match client {
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
    state.channels.disconnect(&desk_id, generation);
}

/// `GET /desks/{desk_id}/terminal` (R3 feature SPEC §3): the attachment socket.
/// The bearer is the same header every route takes — checked by `authorize`
/// before the upgrade — and `Sec-WebSocket-Protocol` is not used.
async fn terminal(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    request: Request,
) -> Result<Response, DeskError> {
    use axum::extract::FromRequestParts;
    desk::get(&state.store, &desk_id)?;
    // Answered before the attachment is taken, so a request that never upgrades
    // cannot supersede a live one (§3).
    if state.terminals.size(&desk_id).is_none() {
        return Ok(envelope(
            StatusCode::CONFLICT,
            "NO_LIVE_SESSION",
            format!("Desk {desk_id:?} has no live terminal."),
        ));
    }
    // The upgrade is extracted by hand rather than in the signature so that the
    // two answers above still cross as the one envelope (root §4.3) instead of
    // the framework's own rejection.
    let (mut parts, _) = request.into_parts();
    let Ok(upgrade) =
        axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &()).await
    else {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "VALIDATION",
            "This route serves a WebSocket upgrade.".to_string(),
        ));
    };
    let Some(attachment) = state.terminals.attach(&desk_id) else {
        return Ok(envelope(
            StatusCode::CONFLICT,
            "NO_LIVE_SESSION",
            format!("Desk {desk_id:?} has no live terminal."),
        ));
    };
    Ok(upgrade.on_upgrade(move |socket| attached(socket, state, desk_id, attachment)))
}

/// One attachment's life: the ring as one binary frame, then live bytes out and
/// input and resizes in, until the child exits (`1000`), a newer attachment
/// supersedes this one (`4001`), or either side goes away.
async fn attached(
    mut socket: axum::extract::ws::WebSocket,
    state: Arc<ApiState>,
    desk_id: String,
    mut attachment: crate::terminal::Attachment,
) {
    use crate::terminal::Frame;
    use axum::extract::ws::{CloseFrame, Message};

    let generation = attachment.generation;
    if !attachment.replay.is_empty() {
        let replay = std::mem::take(&mut attachment.replay);
        if socket.send(Message::Binary(replay.into())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            frame = attachment.frames.recv() => match frame {
                Some(Frame::Bytes(bytes)) => {
                    let len = bytes.len();
                    let sent = socket.send(Message::Binary(bytes.into())).await;
                    attachment.consumed(len);
                    if sent.is_err() {
                        return;
                    }
                }
                Some(Frame::Exited { reason, code }) => {
                    let exited = serde_json::json!({"exited": {"reason": reason, "code": code}});
                    let _ = socket.send(Message::Text(exited.to_string().into())).await;
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 1000,
                            reason: "exited".into(),
                        })))
                        .await;
                    return;
                }
                // Superseded, or dropped as a slow consumer: the newer
                // attachment owns the terminal now.
                _ => {
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 4001,
                            reason: "superseded".into(),
                        })))
                        .await;
                    return;
                }
            },
            client = socket.recv() => match client {
                Some(Ok(Message::Binary(bytes))) => {
                    state.terminals.write(&desk_id, generation, bytes.into())
                }
                Some(Ok(Message::Text(text))) => {
                    if let Some((cols, rows)) = parse_resize(&text) {
                        state.terminals.resize(&desk_id, generation, cols, rows);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return,
            },
        }
    }
}

/// `{"resize":{"cols":n,"rows":n}}`; anything else is ignored.
fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let resize = value.get("resize")?;
    let cols = u16::try_from(resize.get("cols")?.as_u64()?).ok()?;
    let rows = u16::try_from(resize.get("rows")?.as_u64()?).ok()?;
    Some((cols, rows))
}

async fn runtimes(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, DeskError> {
    let runtimes = crate::runtime::rows(&state.store)?;
    Ok(Json(serde_json::json!({ "runtimes": runtimes })))
}

#[derive(Deserialize)]
struct DiscoverRequest {
    executable: Option<PathBuf>,
}

async fn runtime_discover(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, DeskError> {
    if !crate::runtime::known(&name) {
        return Ok(unknown_runtime(&name));
    }
    // The body is optional; when one is sent it must be a JSON object.
    let request = if body.trim().is_empty() {
        DiscoverRequest { executable: None }
    } else {
        match is_json(&headers)
            .then(|| serde_json::from_str::<DiscoverRequest>(&body).ok())
            .flatten()
        {
            Some(request) => request,
            None => {
                return Ok(envelope(
                    StatusCode::BAD_REQUEST,
                    "VALIDATION",
                    r#"The request body must be a JSON object with an optional "executable" path."#
                        .to_string(),
                ));
            }
        }
    };
    if let Some(executable) = &request.executable
        && !executable.is_absolute()
    {
        return Ok(envelope(
            StatusCode::BAD_REQUEST,
            "VALIDATION",
            format!(
                "The executable path {} must be absolute.",
                executable.display()
            ),
        ));
    }
    let row = crate::runtime::discover(
        &state.store,
        &name,
        request.executable.as_deref(),
        &state.search_path,
    )?;
    Ok(Json(row).into_response())
}

async fn runtime_retry(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Response, DeskError> {
    if !crate::runtime::known(&name) {
        return Ok(unknown_runtime(&name));
    }
    // §4.1: the adapter's own failure count goes with the row's failure.
    state.dispatch.adapter(&name).reset_failures();
    let row = crate::runtime::retry(&state.store, &name, &state.search_path)?;
    Ok(Json(row).into_response())
}

// The memory installation routes (R4 feature SPEC §2.1, §3). The desk-scoped
// operations are C31's; nothing here starts the child.

/// One request body, or the §4.3 `VALIDATION` that describes the shape.
fn memory_request<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: &str,
    shape: &str,
) -> Result<T, MemoryError> {
    is_json(headers)
        .then(|| serde_json::from_str::<T>(body).ok())
        .flatten()
        .ok_or_else(|| {
            MemoryError::Validation(format!(
                "The request body must be a JSON object with {shape}."
            ))
        })
}

async fn memory_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<memory::Status>, MemoryError> {
    Ok(Json(state.memory.status().await?))
}

#[derive(Deserialize)]
struct MemoryDiscoverRequest {
    executable: PathBuf,
}

async fn memory_discover(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, MemoryError> {
    let request: MemoryDiscoverRequest =
        memory_request(&headers, &body, r#"an "executable" path"#)?;
    if !request.executable.is_absolute() {
        return Err(MemoryError::Validation(format!(
            "The executable path {} must be absolute.",
            request.executable.display()
        )));
    }
    Ok(Json(state.memory.discover(&request.executable).await?).into_response())
}

async fn memory_retry(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<memory::Child>, MemoryError> {
    Ok(Json(state.memory.retry().await?))
}

async fn memory_provider(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<memory::Provider>, MemoryError> {
    let request = memory_request(
        &headers,
        &body,
        "base_url, llm_model, embedding_model, and an optional api_key",
    )?;
    Ok(Json(state.memory.put_provider(request).await?))
}

async fn memory_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, MemoryError> {
    Ok(Json(
        serde_json::json!({ "models": state.memory.models().await? }),
    ))
}

// The desk-scoped memory operations (R4 feature SPEC §4.2). The desk's own
// refusals — `DESK_NOT_FOUND`, `DESK_NOT_READY`, and `ATTRIBUTION_INVALID` —
// are the order routes' own checks, carried into `MemoryError` so both shapes
// answer through the maps they already have.

async fn desk_memory(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, MemoryError> {
    trade::require_ready(&state.store, &desk_id)?;
    Ok(Json(state.memory.desk_status(&desk_id).await?))
}

async fn memory_retain(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<serde_json::Value>, MemoryError> {
    // Attribution before the desk's own state, as the order routes order it.
    let source = attribution(&state, &headers, &desk_id)?;
    trade::require_ready(&state.store, &desk_id)?;
    let request = memory_request(
        &headers,
        &body,
        r#""content", and optionally "context" and an array of "tags""#,
    )?;
    Ok(Json(
        state.memory.retain_op(&desk_id, request, &source).await?,
    ))
}

async fn memory_recall(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<serde_json::Value>, MemoryError> {
    trade::require_ready(&state.store, &desk_id)?;
    let request = memory_request(
        &headers,
        &body,
        r#""query", and optionally a "budget" and an array of "tags""#,
    )?;
    Ok(Json(state.memory.recall_op(&desk_id, request).await?))
}

async fn memory_reflect(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<serde_json::Value>, MemoryError> {
    trade::require_ready(&state.store, &desk_id)?;
    let request = memory_request(&headers, &body, r#""query" and optionally a "budget""#)?;
    Ok(Json(state.memory.reflect_op(&desk_id, request).await?))
}

fn unknown_runtime(name: &str) -> Response {
    envelope(
        StatusCode::NOT_FOUND,
        "RUNTIME_NOT_FOUND",
        format!("No runtime is named {name:?}; MarketRig knows codex and claude."),
    )
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

/// The two attribution headers (R2 feature SPEC §6). `HeaderMap` lookup is
/// case-insensitive, as HTTP is.
const TRIGGER_HEADER: &str = "x-marketrig-trigger-id";
const FIRING_HEADER: &str = "x-marketrig-firing-id";

/// Derives an action's source from those headers, *before* the desk's node does
/// any work, so a bad attribution never reaches the sandbox (§6). Neither header
/// is a session; both, naming a firing of this desk under that trigger, is a
/// trigger; anything else is `ATTRIBUTION_INVALID` and records nothing.
fn attribution(
    state: &ApiState,
    headers: &HeaderMap,
    desk_id: &str,
) -> Result<trade::Source, TradeError> {
    let read = |name: &'static str| match headers.get(name) {
        None => Ok(None),
        Some(value) => value
            .to_str()
            .map(|v| Some(v.to_owned()))
            .map_err(|_| TradeError::Attribution(format!("{name} is not text"))),
    };
    let (trigger_id, firing_id) = match (read(TRIGGER_HEADER)?, read(FIRING_HEADER)?) {
        (None, None) => return Ok(trade::Source::Session),
        (Some(trigger_id), Some(firing_id)) => (trigger_id, firing_id),
        _ => {
            return Err(TradeError::Attribution(format!(
                "{TRIGGER_HEADER} and {FIRING_HEADER} go together, and only one was sent"
            )));
        }
    };
    let (desk, firing) = (desk_id.to_owned(), firing_id.clone());
    match state
        .store
        .call(move |conn| crate::trigger::load_firing(conn, &desk, &firing))?
    {
        Some(row) if row.trigger_id == trigger_id => Ok(trade::Source::Trigger {
            trigger_id,
            firing_id,
        }),
        Some(_) => Err(TradeError::Attribution(format!(
            "firing {firing_id:?} is not a firing of trigger {trigger_id:?}"
        ))),
        None => Err(TradeError::Attribution(format!(
            "this desk has no firing {firing_id:?}"
        ))),
    }
}

async fn submit_order(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, TradeError> {
    let source = attribution(&state, &headers, &desk_id)?;
    let (record, replayed) =
        trade::submit(&state.store, &state.registry, &desk_id, &body, &source)?;
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
    headers: HeaderMap,
    body: String,
) -> Result<Json<trade::ActionRecord>, TradeError> {
    let source = attribution(&state, &headers, &desk_id)?;
    Ok(Json(trade::cancel(
        &state.store,
        &state.registry,
        &desk_id,
        &client_order_id,
        &body,
        &source,
    )?))
}

// The trigger, firing, and prompt group (R2 feature SPEC §8). Daemon-local —
// SQLite only, no node — and every body arrives as text, so an unusable one is
// `TRIGGER_INVALID` like any other form failure. Every accepted mutation wakes
// the scheduler once its unit has committed (§3.1).

async fn create_trigger(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
    body: String,
) -> Result<Response, TriggerError> {
    let created = trigger::create(&state.store, &desk_id, &body, store::now_ns())?;
    state.scheduler_wake.notify_one();
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

async fn list_triggers(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    let triggers = trigger::list(&state.store, &desk_id)?;
    Ok(Json(serde_json::json!({ "triggers": triggers })))
}

async fn show_trigger(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, trigger_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    Ok(Json(trigger::get(&state.store, &desk_id, &trigger_id)?))
}

async fn patch_trigger(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, trigger_id)): Path<(String, String)>,
    body: String,
) -> Result<Json<serde_json::Value>, TriggerError> {
    let patched = trigger::patch(&state.store, &desk_id, &trigger_id, &body, store::now_ns())?;
    state.scheduler_wake.notify_one();
    Ok(Json(patched))
}

async fn delete_trigger(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, trigger_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    let deleted = trigger::delete(&state.store, &desk_id, &trigger_id, store::now_ns())?;
    state.scheduler_wake.notify_one();
    Ok(Json(deleted))
}

async fn trigger_firings(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, trigger_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    let firings = trigger::firings(&state.store, &desk_id, &trigger_id)?;
    Ok(Json(serde_json::json!({ "firings": firings })))
}

async fn show_firing(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, firing_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    Ok(Json(trigger::firing(&state.store, &desk_id, &firing_id)?))
}

async fn list_prompts(
    State(state): State<Arc<ApiState>>,
    Path(desk_id): Path<String>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    let prompts = trigger::prompts(&state.store, &desk_id)?;
    Ok(Json(serde_json::json!({ "prompts": prompts })))
}

async fn show_prompt(
    State(state): State<Arc<ApiState>>,
    Path((desk_id, prompt_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, TriggerError> {
    Ok(Json(trigger::prompt(&state.store, &desk_id, &prompt_id)?))
}

// The installation policies (R5 feature SPEC §2). Nothing here decides an
// approval; a policy change affects only records created after it.

async fn policies(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<policy::Resource>, PolicyError> {
    Ok(Json(policy::get(&state.store)?))
}

async fn put_policies(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<policy::Resource>, PolicyError> {
    let body: serde_json::Value = is_json(&headers)
        .then(|| serde_json::from_str(&body).ok())
        .flatten()
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(policy::put(&state.store, &body, store::now_ns())?))
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
    channels: Arc<crate::claude::Channels>,
    memory: Arc<crate::memory::Memory>,
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
    let roots = crate::store::Roots::resolve(Some(dir.path())).unwrap();
    roots.create_dirs().unwrap();
    let (quit, quit_rx) = tokio::sync::mpsc::channel(1);
    let registry = Arc::new(crate::node::Registry::new(
        store.clone(),
        Arc::new(crate::feed::MarketState::new()),
        feed_base,
    ));
    let channels = Arc::new(crate::claude::Channels::default());
    let memory = Arc::new(crate::memory::seam_memory(store.clone(), roots));
    let state = ApiState {
        search_path: String::new(),
        terminals: crate::terminal::Manager::new().0,
        channels: channels.clone(),
        store: store.clone(),
        desks_home: desks_home.clone(),
        daemon_uuid: DAEMON_UUID.to_string(),
        credential: CREDENTIAL.to_string(),
        started_at_ns: 1_700_000_000_000_000_000,
        quit,
        registry: registry.clone(),
        scheduler_wake: Arc::new(tokio::sync::Notify::new()),
        dispatch: crate::dispatch::fake::dispatcher(store.clone(), DAEMON_UUID),
        memory: memory.clone(),
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
        channels,
        memory,
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

    // The single read adds the desk's native-session pointers (R3 §7); the
    // listing does not.
    let (status, body) = call_get(url(&format!("/desks/{alpha_id}")), ok);
    assert_eq!(status, 200);
    let mut with_pointers = alpha.clone();
    with_pointers["native_sessions"] = serde_json::json!({});
    assert_eq!(json(&body), with_pointers);

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
        [
            "action_id",
            "created_at_ns",
            "id",
            "kind",
            "outcome",
            "source"
        ],
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
    // The route reports an observation the moment the feed accepts it; the
    // node processes the same tick on its own thread, so an order that needs
    // a price waits on the node's cache, not on the route (§4.3).
    let node = served.registry.ensure(&alpha).expect("the node is started");
    let aapl_id = nautilus_model::identifiers::InstrumentId::from("AAPL.XNAS");
    crate::node::within(10, "the node's cache holds the first tick", || {
        node.call(move |ctx| ctx.cache.borrow().quote(&aapl_id).is_some())
            .unwrap_or(false)
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

// ---------------------------------------------------------------------------
// api::action_attribution (R2 feature SPEC §11)
// ---------------------------------------------------------------------------

/// A POST with headers and a body, so the attribution pair can ride along.
#[cfg(test)]
fn call_post_attributed(url: String, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let mut request = agent()
        .post(url)
        .header("Authorization", format!("Bearer {CREDENTIAL}"))
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    read(request.send(body))
}

/// The two attribution headers decide `trading_actions.source` and nothing else
/// does (R2 feature SPEC §6): a firing of this desk under that trigger attributes
/// the action, a replay answers the stored record whatever the caller now sends,
/// and every other shape answers `ATTRIBUTION_INVALID` having recorded nothing.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn action_attribution() {
    let aapl = crate::catalog::find("AAPL.XNAS").unwrap();
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
        json(&body)["id"].as_str().unwrap().to_string()
    };
    let alpha = create("alpha");
    let beta = create("beta");

    // One trigger and one firing per desk, planted directly: this check is about
    // the routes, not about how a firing comes to exist (§3 owns that).
    let (a, b) = (alpha.clone(), beta.clone());
    served
        .store
        .unit(move |tx| {
            for (desk, trigger, firing) in [(&a, "t-alpha", "f-alpha"), (&b, "t-beta", "f-beta")] {
                tx.execute(
                    "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                     enabled, revision, created_at_ns, updated_at_ns) \
                     VALUES (?1, ?2, 'nightly', 'SCHEDULED', 'ONE_OFF', 'trade', 50, 1, 1, 1, 1)",
                    rusqlite::params![trigger, desk],
                )?;
                tx.execute(
                    "INSERT INTO firings VALUES (?1, ?2, ?3, 50, 60, 1, 'trade', NULL, NULL)",
                    rusqlite::params![firing, desk, trigger],
                )?;
            }
            Ok(())
        })
        .unwrap();

    // The node before the first order, so a 201 means the sandbox answered.
    served.registry.ensure(&alpha).expect("the node starts");
    let market = std::sync::Arc::clone(served.registry.market());
    crate::node::within(10, "the first observation", || {
        market.read(aapl, crate::store::now_ns()).sequence == 1
    });

    let orders = url(&format!("/desks/{alpha}/orders"));
    let body = |action_id: &str| {
        format!(
            r#"{{"action_id":"{action_id}","instrument_id":"AAPL.XNAS",
                 "side":"BUY","type":"LIMIT","quantity":"1","price":"200.00"}}"#
        )
    };
    let attributed = [
        ("X-MarketRig-Trigger-Id", "t-alpha"),
        ("X-MarketRig-Firing-Id", "f-alpha"),
    ];
    let row = |action_id: &'static str| {
        served
            .store
            .call(move |c| {
                c.query_row(
                    "SELECT source, trigger_id, firing_id FROM trading_actions \
                     WHERE action_id = ?1",
                    [action_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
            })
            .unwrap()
    };
    let actions = || {
        served
            .store
            .call(|c| {
                c.query_row("SELECT count(*) FROM trading_actions", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .unwrap()
    };

    // --- Both headers, naming this desk's firing: TRIGGER, on row and record --
    let (status, answer) = call_post_attributed(orders.clone(), &attributed, &body("buy-1"));
    assert_eq!(status, 201, "{answer}");
    let record = json(&answer);
    assert_eq!(record["source"], "TRIGGER");
    assert_eq!(record["trigger_id"], "t-alpha");
    assert_eq!(record["firing_id"], "f-alpha");
    assert_eq!(
        row("buy-1"),
        (
            "TRIGGER".to_string(),
            Some("t-alpha".to_string()),
            Some("f-alpha".to_string())
        )
    );

    // --- A replay answers the stored record, headers or none ----------------
    let (status, replay) = call_post_attributed(orders.clone(), &[], &body("buy-1"));
    assert_eq!(status, 200, "{replay}");
    assert_eq!(
        json(&replay),
        record,
        "the stored TRIGGER record, byte for byte"
    );

    // --- Every other shape: 400, and nothing recorded -----------------------
    let placed = actions();
    let refusals = [
        ("one header alone", &attributed[..1]),
        ("the other header alone", &attributed[1..]),
        (
            "a firing of another desk",
            &[
                ("X-MarketRig-Trigger-Id", "t-beta"),
                ("X-MarketRig-Firing-Id", "f-beta"),
            ][..],
        ),
        (
            "an unknown firing",
            &[
                ("X-MarketRig-Trigger-Id", "t-alpha"),
                ("X-MarketRig-Firing-Id", "f-nowhere"),
            ][..],
        ),
        (
            "a firing of another trigger",
            &[
                ("X-MarketRig-Trigger-Id", "t-beta"),
                ("X-MarketRig-Firing-Id", "f-alpha"),
            ][..],
        ),
    ];
    for (label, headers) in refusals {
        expect_envelope(
            call_post_attributed(orders.clone(), headers, &body("refused")),
            400,
            "ATTRIBUTION_INVALID",
        );
        assert_eq!(actions(), placed, "{label} recorded nothing");
    }
    // The same refusal on the cancel route, and before any order lookup.
    expect_envelope(
        call_post_attributed(
            url(&format!("/desks/{alpha}/orders/buy-1/cancel")),
            &[("X-MarketRig-Firing-Id", "f-alpha")],
            r#"{"action_id":"cancel-1"}"#,
        ),
        400,
        "ATTRIBUTION_INVALID",
    );
    assert_eq!(actions(), placed);

    // --- No headers at all: SESSION ----------------------------------------
    let (status, answer) = call_post_attributed(orders, &[], &body("buy-2"));
    assert_eq!(status, 201, "{answer}");
    let record = json(&answer);
    assert_eq!(record["source"], "SESSION");
    assert!(
        record["trigger_id"].is_null() && record["firing_id"].is_null(),
        "nulls are omitted: {record}"
    );
    assert_eq!(row("buy-2"), ("SESSION".to_string(), None, None));

    served.registry.stop_all();
}

// ---------------------------------------------------------------------------
// api::trigger_codes (R2 feature SPEC §11)
// ---------------------------------------------------------------------------

#[cfg(test)]
fn call_patch(url: String, bearer: Option<&str>, body: &str) -> (u16, String) {
    let mut request = agent()
        .patch(url)
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    read(request.send(body))
}

#[cfg(test)]
fn call_delete(url: String, bearer: Option<&str>) -> (u16, String) {
    let mut request = agent().delete(url);
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    read(request.call())
}

/// The §8 group end to end: the documented body on every success and the one
/// envelope with the documented code on every refusal. Daemon-local routes, so
/// no node and no feed take part.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trigger_codes() {
    let served = serve().await;
    // R2's group under R2's policy: the daemon now ships **Require approval**
    // for trigger code, whose own scenarios are `trigger`'s (R5 feature SPEC
    // §3.2). Nothing else here is about approval.
    crate::policy::put(
        &served.store,
        &serde_json::json!({ "trigger_code_policy": "ALWAYS_ALLOW" }),
        crate::store::now_ns(),
    )
    .unwrap();
    let base = served.base.clone();
    let url = |path: &str| format!("{base}{path}");
    let ok = Some(CREDENTIAL);
    let create_desk = |name: &str| {
        let (status, body) = call_post(
            url("/desks"),
            ok,
            Some(("application/json", &format!(r#"{{"name":"{name}"}}"#))),
        );
        assert_eq!(status, 201, "{body}");
        json(&body)
    };

    let alpha = create_desk("alpha")["id"].as_str().unwrap().to_string();
    // A planted file leaves this desk FAILED: not READY, so no definitions.
    std::fs::write(served.desks_home.join("beta"), "not a directory").unwrap();
    let beta = create_desk("beta");
    assert_eq!(beta["state"], "FAILED");
    let beta = beta["id"].as_str().unwrap().to_string();

    // Every route of the group, for the sweeps that do not care about the body.
    let routes = |desk: &str| -> Vec<(&'static str, String)> {
        vec![
            ("GET", url(&format!("/desks/{desk}/triggers"))),
            ("POST", url(&format!("/desks/{desk}/triggers"))),
            ("GET", url(&format!("/desks/{desk}/triggers/t-1"))),
            ("PATCH", url(&format!("/desks/{desk}/triggers/t-1"))),
            ("DELETE", url(&format!("/desks/{desk}/triggers/t-1"))),
            ("GET", url(&format!("/desks/{desk}/triggers/t-1/firings"))),
            ("GET", url(&format!("/desks/{desk}/firings/f-1"))),
            ("GET", url(&format!("/desks/{desk}/prompts"))),
            ("GET", url(&format!("/desks/{desk}/prompts/p-1"))),
        ]
    };
    let send = |method: &str, route: String, bearer: Option<&str>| match method {
        "GET" => call_get(route, bearer),
        "POST" => call_post(route, bearer, Some(("application/json", "{}"))),
        "PATCH" => call_patch(route, bearer, "{}"),
        _ => call_delete(route, bearer),
    };

    // --- Every new route sits behind the bearer ----------------------------
    for (method, route) in routes(&alpha) {
        for bearer in [None, Some("wrong-credential")] {
            expect_envelope(send(method, route.clone(), bearer), 401, "UNAUTHORIZED");
        }
    }

    // --- An unknown desk is R0's own refusal, on every route ---------------
    for id in ["01999999-0000-7000-8000-0000000000ff", "not-a-uuid"] {
        for (method, route) in routes(id) {
            expect_envelope(send(method, route, ok), 404, "DESK_NOT_FOUND");
        }
    }

    // --- Definitions need a READY desk (§8) --------------------------------
    let instant = |offset_secs: i64| {
        chrono::DateTime::from_timestamp_nanos(crate::store::now_ns() + offset_secs * 1_000_000_000)
            .to_rfc3339()
    };
    let triggers = url(&format!("/desks/{alpha}/triggers"));
    let define = |desk: &str, body: &str| {
        call_post(
            url(&format!("/desks/{desk}/triggers")),
            ok,
            Some(("application/json", body)),
        )
    };
    let one_off = |name: &str, seconds: i64| {
        format!(
            r#"{{"name":"{name}","brief":"look at AAPL","schedule":{{"at":"{}"}}}}"#,
            instant(seconds)
        )
    };
    expect_envelope(
        define(&beta, &one_off("nightly", 3_600)),
        409,
        "DESK_NOT_READY",
    );

    // --- Every §2 and §4.1 form failure is one code ------------------------
    let recurring = |rule: &str, tz: &str| {
        format!(
            r#"{{"name":"hourly","brief":"watch","schedule":
                 {{"rrule":"{rule}","dtstart":"2026-09-03T09:30:00","tz":"{tz}"}}}}"#
        )
    };
    // One byte past §8's 16,384-byte brief.
    let long_brief = "x".repeat(16_385);
    for (label, body) in [
        ("malformed JSON", "{".to_string()),
        ("a body that is not an object", "[]".to_string()),
        ("a name outside the grammar", one_off("Bad--Name", 60)),
        (
            "no schedule",
            r#"{"name":"nightly","brief":"b"}"#.to_string(),
        ),
        (
            "a brief past its bound",
            format!(
                r#"{{"name":"nightly","brief":"{long_brief}","schedule":{{"at":"{}"}}}}"#,
                instant(60)
            ),
        ),
        ("a past instant", one_off("nightly", -60)),
        ("sub-minute recurrence", recurring("FREQ=SECONDLY", "UTC")),
        ("a bounded rule", recurring("FREQ=DAILY;COUNT=3", "UTC")),
        ("an unknown zone", recurring("FREQ=DAILY", "Mars/Olympus")),
        (
            "a snapshot with no {script}",
            format!(
                r#"{{"name":"nightly","brief":"b","schedule":{{"at":"{}"}},
                     "code":{{"source":"print(1)","argv":["python3"]}}}}"#,
                instant(60)
            ),
        ),
        (
            "a snapshot with an unusable suffix",
            format!(
                r#"{{"name":"nightly","brief":"b","schedule":{{"at":"{}"}},
                     "code":{{"source":"print(1)","suffix":"py"}}}}"#,
                instant(60)
            ),
        ),
        (
            "a snapshot with a timeout outside its bound",
            format!(
                r#"{{"name":"nightly","brief":"b","schedule":{{"at":"{}"}},
                     "code":{{"source":"print(1)","timeout_secs":0}}}}"#,
                instant(60)
            ),
        ),
    ] {
        expect_envelope(define(&alpha, &body), 400, "TRIGGER_INVALID");
        assert_eq!(
            json(&call_get(triggers.clone(), ok).1)["triggers"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "{label} defined nothing"
        );
    }

    // --- Creation: the §8 resource, revision 1, armed and fingerprinted -----
    let code = r#""code":{"source":"print(1)","suffix":".py",
                          "argv":["python3","{script}"],"timeout_secs":300}"#;
    let (status, body) = define(
        &alpha,
        &format!(
            r#"{{"name":"nightly","brief":"look at AAPL","context":"since open",
                 "schedule":{{"at":"{}"}},{code}}}"#,
            instant(3_600)
        ),
    );
    assert_eq!(status, 201, "{body}");
    let nightly = json(&body);
    let nightly_id = nightly["id"].as_str().unwrap().to_string();
    assert_eq!(nightly["desk_id"], alpha.as_str());
    assert_eq!(nightly["name"], "nightly");
    assert_eq!(nightly["source"], "SCHEDULED");
    assert_eq!(nightly["recurrence"], "ONE_OFF");
    assert_eq!(nightly["brief"], "look at AAPL");
    assert_eq!(nightly["context"], "since open");
    assert_eq!(nightly["revision"], 1);
    assert_eq!(nightly["enabled"], true);
    assert!(
        nightly["deleted_at_ns"].is_null(),
        "nulls are omitted: {nightly}"
    );
    let armed = nightly["next_occurrence_ns"].as_i64().expect("armed");
    assert!(armed > crate::store::now_ns(), "the projection is ahead");
    assert_eq!(nightly["schedule"], serde_json::json!({ "at_ns": armed }));
    let argv = ["python3".to_string(), "{script}".to_string()];
    assert_eq!(
        nightly["code"],
        serde_json::json!({
            "snapshot_id": nightly["code"]["snapshot_id"],
            "suffix": ".py",
            "argv": ["python3", "{script}"],
            "timeout_secs": 300,
            "fingerprint": crate::trigger::fingerprint("print(1)", ".py", &argv, 300),
            "approval": "ALWAYS_ALLOW",
            "decided_at_ns": nightly["created_at_ns"],
            "approved_at_ns": nightly["created_at_ns"],
            "source_bytes": 8,
            "source": "print(1)",
        }),
        "the §4.1 snapshot, approved on creation under Always allow"
    );

    // --- The live name is taken until the row is deleted --------------------
    expect_envelope(
        define(&alpha, &one_off("nightly", 3_600)),
        409,
        "TRIGGER_NAME_TAKEN",
    );

    // --- The listing omits the source and reports its size ------------------
    let listing = || json(&call_get(triggers.clone(), ok).1)["triggers"].clone();
    let listed = listing();
    assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");
    assert!(
        listed[0]["code"].get("source").is_none(),
        "the listing omits the source: {listed}"
    );
    assert_eq!(listed[0]["code"]["source_bytes"], 8);

    let one = url(&format!("/desks/{alpha}/triggers/{nightly_id}"));
    let (status, body) = call_get(one.clone(), ok);
    assert_eq!(status, 200);
    assert_eq!(json(&body), nightly, "the single read carries the source");

    // --- Patch: one revision each, the projection recomputed ---------------
    let patch = |body: &str| {
        let (status, answer) = call_patch(one.clone(), ok, body);
        assert_eq!(status, 200, "{answer}");
        json(&answer)
    };
    let patched = patch(r#"{"brief":"look at MSFT"}"#);
    assert_eq!(patched["brief"], "look at MSFT");
    assert_eq!(patched["revision"], 2);
    assert!(patched["updated_at_ns"].as_i64() >= nightly["updated_at_ns"].as_i64());
    assert_eq!(
        patched["code"], nightly["code"],
        "the snapshot is untouched"
    );

    let disabled = patch(r#"{"enabled":false}"#);
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["revision"], 3);
    assert!(
        disabled["next_occurrence_ns"].is_null(),
        "a disabled trigger is never due: {disabled}"
    );
    let enabled = patch(r#"{"enabled":true}"#);
    assert_eq!(enabled["revision"], 4);
    assert_eq!(
        enabled["next_occurrence_ns"].as_i64(),
        Some(armed),
        "enable projects from the definition's own anchor"
    );

    let cleared = patch(r#"{"context":null,"code":null}"#);
    assert!(
        cleared["context"].is_null() && cleared["code"].is_null(),
        "context clears and code detaches: {cleared}"
    );
    let rescheduled = patch(
        r#"{"schedule":{"rrule":"FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
             "dtstart":"2026-09-03T09:30:00","tz":"America/New_York"}}"#,
    );
    assert_eq!(rescheduled["recurrence"], "RECURRING");
    assert_eq!(
        rescheduled["schedule"],
        serde_json::json!({
            "rrule": "FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
            "dtstart": "2026-09-03T09:30:00",
            "tz": "America/New_York",
        })
    );
    assert!(rescheduled["next_occurrence_ns"].as_i64().unwrap() > crate::store::now_ns());

    for (label, body) in [
        ("an empty patch", "{}"),
        ("a patch naming nothing", r#"{"nothing":1}"#),
        ("malformed JSON", "{"),
        (
            "a past instant",
            r#"{"schedule":{"at":"2020-01-01T00:00:00Z"}}"#,
        ),
        ("an unusable enablement", r#"{"enabled":"yes"}"#),
    ] {
        expect_envelope(call_patch(one.clone(), ok, body), 400, "TRIGGER_INVALID");
        assert_eq!(
            json(&call_get(one.clone(), ok).1)["revision"],
            rescheduled["revision"],
            "{label} changed nothing"
        );
    }

    // --- Delete is soft: hidden from the listing, still readable by id ------
    let (status, body) = call_delete(one.clone(), ok);
    assert_eq!(status, 200, "{body}");
    let deleted = json(&body);
    assert!(deleted["deleted_at_ns"].as_i64().is_some(), "{deleted}");
    assert!(
        deleted["next_occurrence_ns"].is_null(),
        "a deleted trigger is never due: {deleted}"
    );
    assert_eq!(listing().as_array().map(Vec::len), Some(0));
    assert_eq!(json(&call_get(one.clone(), ok).1), deleted);
    expect_envelope(
        call_patch(one.clone(), ok, r#"{"brief":"again"}"#),
        404,
        "TRIGGER_NOT_FOUND",
    );
    expect_envelope(call_delete(one.clone(), ok), 404, "TRIGGER_NOT_FOUND");
    // Its name is free again, since the unique index covers undeleted rows only.
    let (status, body) = define(&alpha, &one_off("nightly", 3_600));
    assert_eq!(status, 201, "{body}");
    let reborn = json(&body)["id"].as_str().unwrap().to_string();

    // --- Firings and prompts, over rows planted by raw SQL ------------------
    let (a, b, t) = (alpha.clone(), beta.clone(), reborn.clone());
    served
        .store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO firings VALUES ('f-1', ?1, ?2, 100, 100, 1, 'early', NULL, NULL)",
                rusqlite::params![a, t],
            )?;
            tx.execute(
                "INSERT INTO firings VALUES ('f-2', ?1, ?2, 200, 200, 2, 'late', 'ctx', NULL)",
                rusqlite::params![a, t],
            )?;
            tx.execute(
                "INSERT INTO executions (firing_id, desk_id, daemon_uuid, state, outcome, \
                 exit_code, executable, error, stdout, stderr, stdout_truncated, \
                 stderr_truncated, started_at_ns, finished_at_ns) \
                 VALUES ('f-2', ?1, 'daemon-1', 'COMPLETE', 'EXITED', 0, '/bin/echo', NULL, \
                 CAST('out' AS BLOB), CAST('err' AS BLOB), 0, 1, 300, 400)",
                rusqlite::params![a],
            )?;
            tx.execute(
                "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
                 VALUES ('p-1', ?1, 'EVALUATION', 'QUEUED', '{\"kind\":\"EVALUATION\"}', 500), \
                        ('p-2', ?1, 'TRIGGER_RESULT', 'QUEUED', \
                         '{\"kind\":\"TRIGGER_RESULT\",\"execution\":null}', 600)",
                rusqlite::params![a],
            )?;
            // One trigger, firing, and prompt on the other desk: nothing of
            // theirs is ever found under this one.
            tx.execute(
                "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                 enabled, revision, created_at_ns, updated_at_ns) \
                 VALUES ('t-beta', ?1, 'nightly', 'SCHEDULED', 'ONE_OFF', 'trade', 50, 1, 1, 1, 1)",
                rusqlite::params![b],
            )?;
            tx.execute(
                "INSERT INTO firings VALUES ('f-beta', ?1, 't-beta', 50, 60, 1, 'trade', NULL, NULL)",
                rusqlite::params![b],
            )?;
            tx.execute(
                "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
                 VALUES ('p-beta', ?1, 'TRIGGER_RESULT', 'QUEUED', '{}', 1)",
                rusqlite::params![b],
            )
        })
        .unwrap();

    let (status, body) = call_get(
        url(&format!("/desks/{alpha}/triggers/{reborn}/firings")),
        ok,
    );
    assert_eq!(status, 200, "{body}");
    let firings = json(&body)["firings"].clone();
    assert_eq!(firings.as_array().map(Vec::len), Some(2), "{firings}");
    assert_eq!(firings[0]["id"], "f-2", "newest first: {firings}");
    assert_eq!(firings[1]["id"], "f-1");
    assert!(
        firings[1]["execution"].is_null() && firings[1]["context"].is_null(),
        "a firing with no run carries no execution: {firings}"
    );
    assert_eq!(
        firings[0],
        serde_json::json!({
            "id": "f-2", "desk_id": alpha, "trigger_id": reborn,
            "occurrence_ns": 200, "accepted_at_ns": 200, "trigger_revision": 2,
            "brief": "late", "context": "ctx",
            "execution": {
                "state": "COMPLETE", "daemon_uuid": "daemon-1", "outcome": "EXITED",
                "exit_code": 0, "executable": "/bin/echo",
                "stdout_bytes": 3, "stderr_bytes": 3,
                "stdout_truncated": false, "stderr_truncated": true,
                "started_at_ns": 300, "finished_at_ns": 400,
            },
        }),
        "the listing carries the summary without the streams"
    );

    let (status, body) = call_get(url(&format!("/desks/{alpha}/firings/f-2")), ok);
    assert_eq!(status, 200, "{body}");
    let firing = json(&body);
    assert_eq!(firing["execution"]["stdout"], "out");
    assert_eq!(firing["execution"]["stderr"], "err");
    assert_eq!(firing["execution"]["stdout_bytes"], 3);
    assert_eq!(firing["execution"]["stderr_bytes"], 3);

    let (status, body) = call_get(url(&format!("/desks/{alpha}/prompts")), ok);
    assert_eq!(status, 200, "{body}");
    let prompts = json(&body)["prompts"].clone();
    assert_eq!(
        prompts,
        serde_json::json!([
            { "id": "p-2", "desk_id": alpha, "kind": "TRIGGER_RESULT",
              "state": "QUEUED", "created_at_ns": 600, "attempted_at_ns": null,
              "resolved_at_ns": null, "runtime": null, "native_session_id": null,
              "failure_code": null },
            { "id": "p-1", "desk_id": alpha, "kind": "EVALUATION",
              "state": "QUEUED", "created_at_ns": 500, "attempted_at_ns": null,
              "resolved_at_ns": null, "runtime": null, "native_session_id": null,
              "failure_code": null },
        ]),
        "newest first, every delivery fact and no payload"
    );
    let (status, body) = call_get(url(&format!("/desks/{alpha}/prompts/p-2")), ok);
    assert_eq!(status, 200, "{body}");
    let prompt = json(&body);
    assert_eq!(
        prompt["payload"],
        serde_json::json!({ "kind": "TRIGGER_RESULT", "execution": null }),
        "the stored payload, verbatim"
    );

    // --- Unknown, and another desk's, on every read ------------------------
    for (path, code) in [
        (
            format!("/desks/{alpha}/triggers/t-nowhere"),
            "TRIGGER_NOT_FOUND",
        ),
        (
            format!("/desks/{alpha}/triggers/t-beta"),
            "TRIGGER_NOT_FOUND",
        ),
        (
            format!("/desks/{alpha}/triggers/t-nowhere/firings"),
            "TRIGGER_NOT_FOUND",
        ),
        (
            format!("/desks/{alpha}/triggers/t-beta/firings"),
            "TRIGGER_NOT_FOUND",
        ),
        (
            format!("/desks/{alpha}/firings/f-nowhere"),
            "FIRING_NOT_FOUND",
        ),
        (format!("/desks/{alpha}/firings/f-beta"), "FIRING_NOT_FOUND"),
        (
            format!("/desks/{alpha}/prompts/p-nowhere"),
            "PROMPT_NOT_FOUND",
        ),
        (format!("/desks/{alpha}/prompts/p-beta"), "PROMPT_NOT_FOUND"),
    ] {
        expect_envelope(call_get(url(&path), ok), 404, code);
    }
    expect_envelope(
        call_patch(
            url(&format!("/desks/{alpha}/triggers/t-beta")),
            ok,
            r#"{"enabled":false}"#,
        ),
        404,
        "TRIGGER_NOT_FOUND",
    );
    expect_envelope(
        call_delete(url(&format!("/desks/{alpha}/triggers/t-beta")), ok),
        404,
        "TRIGGER_NOT_FOUND",
    );
}

#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_route_answers_before_any_upgrade() {
    let served = serve().await;
    let base = served.base.clone();
    let ok = Some(CREDENTIAL);
    expect_envelope(
        call_get(
            format!("{base}/desks/{}/terminal", uuid::Uuid::now_v7()),
            ok,
        ),
        404,
        "DESK_NOT_FOUND",
    );
    let created = call_post(
        format!("{base}/desks"),
        ok,
        Some(("application/json", r#"{"name":"terminal-desk"}"#)),
    );
    let desk_id = json(&created.1)["id"].as_str().unwrap().to_string();
    expect_envelope(
        call_get(format!("{base}/desks/{desk_id}/terminal"), ok),
        409,
        "NO_LIVE_SESSION",
    );
    expect_envelope(
        call_get(format!("{base}/desks/{desk_id}/terminal"), None),
        401,
        "UNAUTHORIZED",
    );
}

/// The channel socket's two answers before a frame is ever written: no open
/// process is `4002`, and a second bridge supersedes the first `4001`
/// (R3 feature SPEC §5.3).
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_socket_needs_an_open_process_and_supersedes() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let served = serve().await;
    let base = served.base.clone();
    let ok = Some(CREDENTIAL);
    let created = call_post(
        format!("{base}/desks"),
        ok,
        Some(("application/json", r#"{"name":"channel-desk"}"#)),
    );
    let desk_id = json(&created.1)["id"].as_str().unwrap().to_string();
    let url = format!("{base}/desks/{desk_id}/channel").replacen("http://", "ws://", 1);
    let connect = || {
        let url = url.clone();
        async move {
            let mut request = url.into_client_request().unwrap();
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {CREDENTIAL}").parse().unwrap(),
            );
            tokio_tungstenite::connect_async(request).await.unwrap().0
        }
    };
    let closed = |message: Option<Result<Message, _>>| match message {
        Some(Ok(Message::Close(Some(frame)))) => u16::from(frame.code),
        other => panic!("expected a close frame, got {other:?}"),
    };

    // No process row: the connection is refused as soon as it is made.
    let mut early = connect().await;
    assert_eq!(closed(early.next().await), 4002);

    // …unless the launch is still in flight, when the bridge may well beat the
    // row it is the readiness of (§6.1).
    served.channels.spawning(&desk_id, true);
    let mut racing = connect().await;
    racing.send(Message::Ping(Vec::new().into())).await.unwrap();
    assert!(
        matches!(racing.next().await, Some(Ok(Message::Pong(_)))),
        "a bridge that beats the row is served"
    );
    served.channels.spawning(&desk_id, false);
    drop(racing);

    served
        .store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO agent_processes (id, desk_id, runtime, pid, daemon_uuid, \
                 started_at_ns) VALUES ('p1', (SELECT id FROM desks), 'claude', 1, 'd', 1)",
                [],
            )
        })
        .unwrap();
    let mut first = connect().await;
    let mut second = connect().await;
    assert_eq!(closed(first.next().await), 4001);
    // The survivor is live: it stays open until it goes away itself.
    second.send(Message::Ping(Vec::new().into())).await.unwrap();
    assert!(matches!(second.next().await, Some(Ok(Message::Pong(_)))));
}

// ---------------------------------------------------------------------------
// api::memory_routes (R4 feature SPEC §8 check 4, the route half)
// ---------------------------------------------------------------------------

/// The four desk-scoped memory routes against the in-process fake child
/// `memory` owns: the desk's own refusals, the attribution the retain metadata
/// carries, the three answer shapes, and the events (§4.2, §4.3).
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_routes() {
    let served = serve().await;
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
        json(&body)["id"].as_str().unwrap().to_string()
    };
    let alpha = create("alpha");
    let beta = create("beta");

    // One firing on alpha, planted: §3 owns how a firing comes to exist.
    let (a, b) = (alpha.clone(), beta.clone());
    served
        .store
        .unit(move |tx| {
            for (desk, trigger, firing) in [(&a, "t-alpha", "f-alpha"), (&b, "t-beta", "f-beta")] {
                tx.execute(
                    "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                     enabled, revision, created_at_ns, updated_at_ns) \
                     VALUES (?1, ?2, 'nightly', 'SCHEDULED', 'ONE_OFF', 'learn', 50, 1, 1, 1, 1)",
                    rusqlite::params![trigger, desk],
                )?;
                tx.execute(
                    "INSERT INTO firings VALUES (?1, ?2, ?3, 50, 60, 1, 'learn', NULL, NULL)",
                    rusqlite::params![firing, desk, trigger],
                )?;
            }
            Ok(())
        })
        .unwrap();

    // --- No child yet: the installation's own answer, desk-scoped -----------
    let (status, body) = call_get(url(&format!("/desks/{alpha}/memory")), ok);
    assert_eq!(status, 200, "{body}");
    let status_body = json(&body);
    assert_eq!(status_body["desk_id"], Value::String(alpha.clone()));
    assert_eq!(status_body["child"]["state"], "UNCONFIGURED");
    assert_eq!(status_body["child"]["live"], "NOT_STARTED");
    assert_eq!(status_body["provider"]["api_key_present"], false);

    // Nothing is live, so an operation answers the installation's state (§4.3).
    expect_envelope(
        call_post_attributed(
            url(&format!("/desks/{alpha}/memory/retain")),
            &[],
            r#"{"content":"before any child"}"#,
        ),
        409,
        "MEMORY_UNCONFIGURED",
    );

    // --- With the fake child live -------------------------------------------
    let fake = crate::memory::fake_child().await;
    crate::memory::set_ready(&served.memory, &fake).await;

    let retain = url(&format!("/desks/{alpha}/memory/retain"));
    let (status, body) = call_post_attributed(retain.clone(), &[], r#"{"content":"a lesson"}"#);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body), serde_json::json!({ "items_count": 1 }));
    assert_eq!(
        fake.last().1["items"][0]["metadata"],
        serde_json::json!({ "source": "INTERACTIVE", "desk_id": alpha })
    );

    // A firing of this desk under that trigger: TRIGGER, with both ids.
    let (status, body) = call_post_attributed(
        retain.clone(),
        &[
            ("X-MarketRig-Trigger-Id", "t-alpha"),
            ("X-MarketRig-Firing-Id", "f-alpha"),
        ],
        r#"{"content":"from a trigger","tags":["lesson"]}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        fake.last().1["items"][0]["metadata"],
        serde_json::json!({
            "source": "TRIGGER", "desk_id": alpha,
            "trigger_id": "t-alpha", "firing_id": "f-alpha",
        })
    );

    // Any other attribution shape is refused before the child is called (§4.2).
    for headers in [
        &[("X-MarketRig-Trigger-Id", "t-alpha")][..],
        &[
            ("X-MarketRig-Trigger-Id", "t-beta"),
            ("X-MarketRig-Firing-Id", "f-beta"),
        ][..],
        &[
            ("X-MarketRig-Trigger-Id", "t-alpha"),
            ("X-MarketRig-Firing-Id", "f-nowhere"),
        ][..],
    ] {
        expect_envelope(
            call_post_attributed(retain.clone(), headers, r#"{"content":"refused"}"#),
            400,
            "ATTRIBUTION_INVALID",
        );
    }
    assert!(fake.drain().is_empty(), "a refused retain never leaves");

    // --- recall and reflect answer §4.2's shapes ----------------------------
    let (status, body) = call_post_attributed(
        url(&format!("/desks/{alpha}/memory/recall")),
        &[],
        r#"{"query":"what did I learn","budget":"low"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let results = json(&body)["results"].clone();
    assert_eq!(
        results[0]
            .as_object()
            .expect("a result object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "context",
            "id",
            "mentioned_at",
            "metadata",
            "occurred_start",
            "tags",
            "text",
            "type"
        ],
        "exactly §4.2's eight fields"
    );

    let (status, body) = call_post_attributed(
        url(&format!("/desks/{alpha}/memory/reflect")),
        &[],
        r#"{"query":"what did I learn"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let reflection = json(&body);
    assert_eq!(reflection["text"], "a reflection");
    assert_eq!(
        reflection["based_on"],
        serde_json::json!([{ "id": "m-1", "text": "a lesson", "type": "experience" }])
    );

    // --- The desk segment and the desk's state (§4.3) -----------------------
    let nowhere = uuid::Uuid::now_v7();
    expect_envelope(
        call_get(url(&format!("/desks/{nowhere}/memory")), ok),
        404,
        "DESK_NOT_FOUND",
    );
    expect_envelope(
        call_post_attributed(
            url(&format!("/desks/{nowhere}/memory/recall")),
            &[],
            r#"{"query":"q"}"#,
        ),
        404,
        "DESK_NOT_FOUND",
    );
    served
        .store
        .unit(|tx| {
            tx.execute(
                "UPDATE desks SET state = 'CREATING', ready_at_ns = NULL WHERE name = 'beta'",
                [],
            )
        })
        .unwrap();
    expect_envelope(
        call_post_attributed(
            url(&format!("/desks/{beta}/memory/retain")),
            &[],
            r#"{"content":"c"}"#,
        ),
        409,
        "DESK_NOT_READY",
    );

    // --- Bodies and limits both answer VALIDATION (§4.3) --------------------
    expect_envelope(
        call_post(
            retain.clone(),
            ok,
            Some(("text/plain", r#"{"content":"c"}"#)),
        ),
        400,
        "VALIDATION",
    );
    expect_envelope(
        call_post_attributed(retain.clone(), &[], r#"{"content":""}"#),
        400,
        "VALIDATION",
    );
    expect_envelope(
        call_post_attributed(
            url(&format!("/desks/{alpha}/memory/recall")),
            &[],
            r#"{"query":"q","budget":"enormous"}"#,
        ),
        400,
        "VALIDATION",
    );

    // --- The child's own failures, through §4.3's map -----------------------
    fake.arm("reject");
    expect_envelope(
        call_post_attributed(retain.clone(), &[], r#"{"content":"c"}"#),
        422,
        "MEMORY_REJECTED",
    );
    fake.arm("boom");
    let (status, body) = call_post_attributed(retain.clone(), &[], r#"{"content":"c"}"#);
    assert_eq!(status, 502, "{body}");
    assert_eq!(json(&body)["code"], "MEMORY_ERROR");
    fake.arm("ok");

    // --- The events: counts and attribution, never a word of content --------
    let seen = served
        .store
        .call(|c| {
            c.prepare(
                "SELECT kind, desk_id, payload FROM operational_events \
                 WHERE kind LIKE 'MEMORY_R%' ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(
        seen.iter()
            .map(|(kind, desk, _)| (kind.as_str(), desk.as_str()))
            .collect::<Vec<_>>(),
        [
            ("MEMORY_RETAINED", alpha.as_str()),
            ("MEMORY_RETAINED", alpha.as_str()),
            ("MEMORY_RECALLED", alpha.as_str()),
            ("MEMORY_RECALLED", alpha.as_str()),
        ]
    );
    assert_eq!(
        json(&seen[1].2),
        serde_json::json!({
            "source": "TRIGGER", "trigger_id": "t-alpha", "firing_id": "f-alpha",
            "items_count": 1, "tags": ["lesson"],
        })
    );
    assert_eq!(
        json(&seen[2].2),
        serde_json::json!({ "op": "recall", "results": 1 })
    );
    let written: String = seen
        .iter()
        .map(|(_, _, payload)| payload.as_str())
        .collect();
    for secret in ["a lesson", "from a trigger", "what did I learn"] {
        assert!(!written.contains(secret), "{secret:?} reached an event");
    }
}

// ---------------------------------------------------------------------------
// api::policy_routes (R5 feature SPEC §8 check 1, the route half)
// ---------------------------------------------------------------------------

#[cfg(test)]
fn call_put(url: String, bearer: Option<&str>, body: &str) -> (u16, String) {
    let mut request = agent().put(url).header("content-type", "application/json");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    read(request.send(body))
}

/// `GET`/`PUT /settings/policies` (§2): both sit behind the bearer, and
/// `PolicyError` reaches the wire as the one envelope with its own status.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_routes() {
    let served = serve().await;
    let policies = format!("{}/settings/policies", served.base);
    let ok = Some(CREDENTIAL);

    // --- Every new route sits behind the bearer ----------------------------
    for bearer in [None, Some("wrong-credential")] {
        expect_envelope(call_get(policies.clone(), bearer), 401, "UNAUTHORIZED");
        expect_envelope(
            call_put(policies.clone(), bearer, "{}"),
            401,
            "UNAUTHORIZED",
        );
    }

    let (status, body) = call_get(policies.clone(), ok);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        json(&body),
        serde_json::json!({
            "trigger_code_policy": "REQUIRE_APPROVAL",
            "paper_order_policy": "ALWAYS_ALLOW",
            "delivery_mode": "QUEUE",
            "steer_available": false,
            "updated_at_ns": 0,
        })
    );

    // The three refusals of §2, each with its own status.
    expect_envelope(call_put(policies.clone(), ok, "{}"), 400, "VALIDATION");
    expect_envelope(
        call_put(policies.clone(), ok, r#"{"trigger_code_policy":"MAYBE"}"#),
        400,
        "VALIDATION",
    );
    expect_envelope(
        call_put(
            policies.clone(),
            ok,
            r#"{"delivery_mode":"QUEUE","nope":"x"}"#,
        ),
        400,
        "VALIDATION",
    );
    expect_envelope(
        call_put(policies.clone(), ok, r#"{"delivery_mode":"STEER"}"#),
        409,
        "STEER_DISABLED",
    );

    let (status, body) = call_put(
        policies.clone(),
        ok,
        r#"{"trigger_code_policy":"ALWAYS_ALLOW"}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["trigger_code_policy"], "ALWAYS_ALLOW");
    assert!(json(&body)["updated_at_ns"].as_i64().unwrap() > 0);
    assert_eq!(
        json(&call_get(policies, ok).1)["trigger_code_policy"],
        "ALWAYS_ALLOW"
    );
}
