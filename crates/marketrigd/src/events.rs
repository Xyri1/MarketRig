//! The installation's operational-event tail: one publisher, one socket, and
//! one listing.
//!
//! Contract: `sdd/features/r5-desktop-approval-controls/SPEC.md` §4.1 (the
//! publisher), §4.2 (`WS /events`), and §4.3 (`GET /events` and the CLI), per
//! R5-5.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::api::{ApiState, Envelope, Gate, envelope, ws_gate};
use crate::store::{Store, StoreError};

/// One page of the tail, and the listing's ceiling (§4.1, §4.3).
const PAGE: i64 = 500;
/// The listing's default page (§4.3).
const DEFAULT_LIMIT: i64 = 100;
/// One subscriber's bounded queue; a push that finds it full closes that
/// subscriber `4408` (§4.1).
const QUEUE: usize = 1_000;
/// The publisher wakes on a commit or after this, whichever comes first (§4.1).
const IDLE: Duration = Duration::from_secs(5);
/// The first frame's budget (§4.2).
const FIRST_FRAME: Duration = Duration::from_secs(5);

/// `(occurred_at_ns, id)` — the tail's order and every client's cursor (§4.2).
pub type Cursor = (i64, String);

/// One event, already serialized, on its way to one subscriber.
type Frame = String;

/// `<occurred_at_ns>:<id>`, the decimal instant and the UUID (§4.2).
fn format_cursor(cursor: &Cursor) -> String {
    format!("{}:{}", cursor.0, cursor.1)
}

fn parse_cursor(text: &str) -> Option<Cursor> {
    let (occurred_at_ns, id) = text.split_once(':')?;
    Some((occurred_at_ns.parse().ok()?, id.to_string()))
}

// ---------------------------------------------------------------------------
// The rows (§4.1, §4.3)
// ---------------------------------------------------------------------------

const SELECT: &str = "SELECT id, kind, desk_id, occurred_at_ns, payload FROM operational_events";

/// One row as its cursor and the frame body: `desk_id` is absent when null and
/// `payload` is the stored object, parsed (§4.2).
fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Cursor, Value)> {
    let id: String = row.get(0)?;
    let desk_id: Option<String> = row.get(2)?;
    let occurred_at_ns: i64 = row.get(3)?;
    let payload: String = row.get(4)?;
    let mut event = json!({
        "id": id,
        "kind": row.get::<_, String>(1)?,
        "occurred_at_ns": occurred_at_ns,
        "payload": serde_json::from_str::<Value>(&payload).unwrap_or_else(|_| json!({})),
    });
    if let Some(desk_id) = desk_id {
        event["desk_id"] = Value::String(desk_id);
    }
    Ok(((occurred_at_ns, id), event))
}

/// One page of the tail in commit order: rows after `after`, and no later than
/// `until` when the replay is bounded by the subscription's tail (§4.1, §4.2).
fn page(
    conn: &Connection,
    after: &Cursor,
    until: Option<&Cursor>,
    limit: i64,
) -> rusqlite::Result<Vec<(Cursor, Value)>> {
    let bound = match until {
        Some(_) => "AND (occurred_at_ns, id) <= (?4, ?5)",
        None => "",
    };
    let mut statement = conn.prepare(&format!(
        "{SELECT} WHERE (occurred_at_ns, id) > (?1, ?2) {bound} \
         ORDER BY occurred_at_ns, id LIMIT ?3"
    ))?;
    match until {
        Some(until) => statement
            .query_map(params![after.0, after.1, limit, until.0, until.1], read)?
            .collect(),
        None => statement
            .query_map(params![after.0, after.1, limit], read)?
            .collect(),
    }
}

/// The listing's page, newest first (§4.3). A `desk_id` filter excludes the
/// installation-wide rows by construction: `NULL = ?1` is never true.
fn listing(
    conn: &Connection,
    desk_id: Option<&str>,
    before: Option<&Cursor>,
    limit: i64,
) -> rusqlite::Result<Vec<(Cursor, Value)>> {
    conn.prepare(&format!(
        "{SELECT} WHERE (?1 IS NULL OR desk_id = ?1) \
           AND (?2 IS NULL OR (occurred_at_ns, id) < (?2, ?3)) \
         ORDER BY occurred_at_ns DESC, id DESC LIMIT ?4"
    ))?
    .query_map(
        params![
            desk_id,
            before.map(|c| c.0),
            before.map(|c| c.1.as_str()).unwrap_or_default(),
            limit
        ],
        read,
    )?
    .collect()
}

// ---------------------------------------------------------------------------
// The publisher (§4.1)
// ---------------------------------------------------------------------------

/// The one fan-out of the tail: a cursor and the live subscriber queues.
pub struct Publisher {
    store: Store,
    inner: Mutex<Inner>,
}

struct Inner {
    cursor: Cursor,
    subscribers: Vec<mpsc::Sender<Frame>>,
}

impl Publisher {
    /// The cursor starts at the table's last row, so a subscriber with no
    /// `after` sees only rows committed after it connects (§4.1).
    pub fn new(store: Store) -> Result<Arc<Publisher>, StoreError> {
        let cursor = store
            .call(|conn| {
                conn.query_row(
                    "SELECT occurred_at_ns, id FROM operational_events \
                     ORDER BY occurred_at_ns DESC, id DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
            })?
            .unwrap_or((0, String::new()));
        Ok(Arc::new(Publisher {
            store,
            inner: Mutex::new(Inner {
                cursor,
                subscribers: Vec::new(),
            }),
        }))
    }

    /// Registers a queue and reports the tail it starts from, under one lock,
    /// so the replay's upper bound and the first live frame meet exactly (§4.2).
    pub fn subscribe(&self) -> (mpsc::Receiver<Frame>, Cursor) {
        let (tx, rx) = mpsc::channel(QUEUE);
        let mut inner = self.lock();
        inner.subscribers.push(tx);
        (rx, inner.cursor.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reads every row after the cursor, in pages, until a page is short (§4.1).
    fn drain(&self) {
        loop {
            let after = self.lock().cursor.clone();
            let rows = match self.store.call(move |conn| page(conn, &after, None, PAGE)) {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("the events tail could not be read: {e}");
                    return;
                }
            };
            let short = (rows.len() as i64) < PAGE;
            self.push(rows);
            if short {
                return;
            }
        }
    }

    /// Fans one page out and advances the cursor with it, under the same lock
    /// [`subscribe`](Publisher::subscribe) takes.
    fn push(&self, rows: Vec<(Cursor, Value)>) {
        let mut inner = self.lock();
        for (cursor, event) in rows {
            let frame = event.to_string();
            // A full queue is a consumer that has stopped reading: drop its
            // sender, which its socket task closes `4408` on (§4.1).
            inner
                .subscribers
                .retain(|queue| queue.try_send(frame.clone()).is_ok());
            inner.cursor = cursor;
        }
    }
}

/// The one publisher task, spawned by `lib.rs::serve` and stopped by the
/// shutdown watch (§4.1).
pub async fn run(publisher: Arc<Publisher>, mut shutdown: watch::Receiver<bool>) {
    let commits = publisher.store.commits();
    loop {
        publisher.drain();
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = commits.notified() => {}
            _ = tokio::time::sleep(IDLE) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// `GET /events` — the socket and the listing (§4.2, §4.3)
// ---------------------------------------------------------------------------

/// One path, two answers: a request that upgrades becomes the tail socket,
/// which authenticates in its first frame; anything else is the listing, which
/// takes the header bearer like every other route (§4.2, §4.3, §4.4).
///
/// ponytail: the route sits outside `api::router`'s bearer layer because the
/// socket has no header to check; C39's `ws_gate` adds the origin allowlist in
/// front of both halves.
#[utoipa::path(
    get,
    path = "/events",
    params(
        ("desk_id" = Option<String>, Query, description = "Only this desk's rows"),
        ("before" = Option<String>, Query, description = "Page back from this cursor"),
        ("limit" = Option<i64>, Query, description = "1 to 500; 100 by omission"),
    ),
    responses(
        (status = 200, description = "One page of the tail, newest first", body = Value),
        (status = 400, body = Envelope),
        (status = 401, body = Envelope),
        (status = 403, body = Envelope),
    )
)]
pub async fn events(
    State(state): State<Arc<ApiState>>,
    query: Result<Query<HashMap<String, String>>, axum::extract::rejection::QueryRejection>,
    request: Request,
) -> Response {
    use axum::extract::FromRequestParts;
    let (mut parts, _) = request.into_parts();
    // Extracted before the gate but upgraded only after it, so a refused origin
    // is an envelope and never a socket (§4.4).
    let upgrade = axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &())
        .await
        .ok();
    // The socket half has no header to require — its credential arrives in
    // frame 1 — and the listing half takes the bearer like every other route.
    if let Gate::Refused(refused) = ws_gate(&parts.headers, &state.credential, upgrade.is_none()) {
        return refused;
    }
    if let Some(upgrade) = upgrade {
        return upgrade.on_upgrade(move |socket| tail(socket, state));
    }

    let Ok(Query(query)) = query else {
        return validation("The query string must be \"key=value\" pairs.");
    };
    let before = match query.get("before") {
        Some(text) => match parse_cursor(text) {
            Some(cursor) => Some(cursor),
            None => {
                return validation(
                    "The \"before\" cursor must be \"<occurred_at_ns>:<id>\", \
                     as a listing's \"next_before\" and a tail frame carry it.",
                );
            }
        },
        None => None,
    };
    let limit = match query.get("limit") {
        Some(text) => match text.parse::<i64>() {
            Ok(limit) if (1..=PAGE).contains(&limit) => limit,
            _ => return validation("The \"limit\" must be a whole number from 1 to 500."),
        },
        None => DEFAULT_LIMIT,
    };
    let desk_id = query.get("desk_id").cloned();
    let rows = match state
        .store
        .call(move |conn| listing(conn, desk_id.as_deref(), before.as_ref(), limit))
    {
        Ok(rows) => rows,
        Err(e) => {
            return envelope(StatusCode::INTERNAL_SERVER_ERROR, e.code(), e.to_string());
        }
    };
    // A full page means there may be more behind it; a short one is the end.
    let next_before = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|(cursor, _)| format_cursor(cursor)))
        .flatten();
    let events: Vec<Value> = rows.into_iter().map(|(_, event)| event).collect();
    let mut body = json!({ "events": events });
    if let Some(next_before) = next_before {
        body["next_before"] = Value::String(next_before);
    }
    axum::Json(body).into_response()
}

fn validation(message: &str) -> Response {
    envelope(StatusCode::BAD_REQUEST, "VALIDATION", message.to_string())
}

/// One tail connection: the first frame, the replay, the `tail` position, then
/// live frames until either side goes away (§4.2).
async fn tail(mut socket: WebSocket, state: Arc<ApiState>) {
    let Some(after) = handshake(&mut socket, &state).await else {
        return;
    };
    // Subscribed before the replay is read, so the tail the replay stops at is
    // the position the first live frame continues from (§4.2).
    let (mut queue, tail) = state.events.subscribe();
    if let Some(after) = after {
        let mut at = after;
        loop {
            let (from, until) = (at.clone(), tail.clone());
            let Ok(rows) = state
                .store
                .call(move |conn| page(conn, &from, Some(&until), PAGE))
            else {
                return;
            };
            let short = (rows.len() as i64) < PAGE;
            for (cursor, event) in rows {
                if send(&mut socket, event.to_string()).await.is_err() {
                    return;
                }
                at = cursor;
            }
            if short {
                break;
            }
        }
    }
    if send(
        &mut socket,
        json!({ "tail": format_cursor(&tail) }).to_string(),
    )
    .await
    .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            frame = queue.recv() => match frame {
                Some(frame) => if send(&mut socket, frame).await.is_err() {
                    return;
                },
                // The publisher dropped this queue because it was full: this
                // consumer stopped reading (§4.1).
                None => return close(&mut socket, 4408, "SLOW_CONSUMER").await,
            },
            // The client sends nothing after its first frame (§4.2).
            client = socket.recv() => match client {
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return,
            },
        }
    }
}

/// Frame 1's credential (§4.2, §4.4): `{"bearer": "…", …}` as the first text
/// frame within 5 s. `None` means the socket was closed with its own refusal —
/// `4401` for no credential, `4400` for a frame that is not that object — and
/// `Some(body)` is the whole frame, whose other members are the route's.
///
/// The one first-frame authentication: the tail takes its `after` from the body
/// it returns, the terminal takes nothing (§4.4).
pub(crate) async fn first_frame_auth(socket: &mut WebSocket, credential: &str) -> Option<Value> {
    let first = match tokio::time::timeout(FIRST_FRAME, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => text,
        // No frame in time, a connection that went away, or a frame that is not
        // text: no credential was presented either way.
        _ => {
            close(socket, 4401, "UNAUTHORIZED").await;
            return None;
        }
    };
    let bearer = serde_json::from_str::<Value>(&first)
        .ok()
        .and_then(|body| Some((body.get("bearer")?.as_str()?.to_string(), body)));
    let Some((bearer, body)) = bearer else {
        close(socket, 4400, "VALIDATION").await;
        return None;
    };
    if bearer != credential {
        close(socket, 4401, "UNAUTHORIZED").await;
        return None;
    }
    Some(body)
}

/// Frame 1 for the tail: the credential, then the replay position, itself
/// `None` when the client asked for none or named no row (§4.2).
async fn handshake(socket: &mut WebSocket, state: &ApiState) -> Option<Option<Cursor>> {
    let body = first_frame_auth(socket, &state.credential).await?;
    // An `after` that names no row is ignored: the connection starts at the
    // tail and the `tail` frame says so (§4.2).
    Some(
        body.get("after")
            .and_then(Value::as_str)
            .and_then(parse_cursor)
            .filter(|cursor| exists(&state.store, cursor)),
    )
}

fn exists(store: &Store, cursor: &Cursor) -> bool {
    let cursor = cursor.clone();
    store
        .call(move |conn| {
            conn.query_row(
                "SELECT 1 FROM operational_events WHERE occurred_at_ns = ?1 AND id = ?2",
                params![cursor.0, cursor.1],
                |_| Ok(()),
            )
            .optional()
        })
        .is_ok_and(|row| row.is_some())
}

async fn send(socket: &mut WebSocket, frame: String) -> Result<(), axum::Error> {
    socket.send(Message::Text(frame.into())).await
}

/// Close codes carry the same SCREAMING_SNAKE code as text (§4.4).
pub(crate) async fn close(socket: &mut WebSocket, code: u16, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

// ---------------------------------------------------------------------------
// events (feature SPEC §8 check 5)
// ---------------------------------------------------------------------------

#[cfg(test)]
use futures_util::{SinkExt, StreamExt};
#[cfg(test)]
use tokio_tungstenite::tungstenite::Message as Wire;

/// A tail client, the way a browser opens one: no header, the credential in the
/// first frame (§4.2).
#[cfg(test)]
type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[cfg(test)]
async fn dial(base: &str) -> Client {
    let url = format!("{base}/events").replacen("http://", "ws://", 1);
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

#[cfg(test)]
async fn open(base: &str, first: Value) -> Client {
    let mut socket = dial(base).await;
    socket
        .send(Wire::Text(first.to_string().into()))
        .await
        .unwrap();
    socket
}

#[cfg(test)]
#[track_caller]
fn frame(message: Option<Result<Wire, tokio_tungstenite::tungstenite::Error>>) -> Value {
    match message {
        Some(Ok(Wire::Text(text))) => serde_json::from_str(&text).expect("a JSON text frame"),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

/// The next text frame, parsed.
#[cfg(test)]
async fn next(socket: &mut Client) -> Value {
    frame(socket.next().await)
}

/// The close code, past any frames still in flight.
#[cfg(test)]
async fn closed(socket: &mut Client) -> u16 {
    loop {
        match socket.next().await {
            Some(Ok(Wire::Close(Some(close)))) => return u16::from(close.code),
            Some(Ok(_)) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

/// `count` rows in one unit — so one commit and one pulse — at consecutive
/// instants from `base_ns`, in tail order. `pad` bytes of filler in each
/// payload is how the slow-consumer flood outgrows the socket buffers that
/// would otherwise swallow it.
#[cfg(test)]
fn seed(store: &Store, desk_id: Option<&str>, count: usize, base_ns: i64) -> Vec<Cursor> {
    seed_padded(store, desk_id, count, base_ns, 0)
}

#[cfg(test)]
fn seed_padded(
    store: &Store,
    desk_id: Option<&str>,
    count: usize,
    base_ns: i64,
    pad: usize,
) -> Vec<Cursor> {
    let desk_id = desk_id.map(str::to_string);
    let filler = "x".repeat(pad);
    store
        .unit(move |tx| {
            let mut cursors = Vec::with_capacity(count);
            for n in 0..count {
                let id = uuid::Uuid::now_v7().to_string();
                let at = base_ns + n as i64;
                let payload = match pad {
                    0 => format!("{{\"n\":{n}}}"),
                    _ => format!("{{\"n\":{n},\"pad\":\"{filler}\"}}"),
                };
                tx.execute(
                    "INSERT INTO operational_events (id, kind, desk_id, occurred_at_ns, payload) \
                     VALUES (?1, 'POLICY_CHANGED', ?2, ?3, ?4)",
                    params![id, desk_id, at, payload],
                )?;
                cursors.push((at, id));
            }
            Ok(cursors)
        })
        .unwrap()
}

#[cfg(test)]
#[track_caller]
fn id_of(event: &Value) -> String {
    event["id"]
        .as_str()
        .expect("every frame carries an id")
        .to_string()
}

/// Waits for the publisher to reach `cursor`, so a reconnect's replay boundary
/// is the one the scenario describes and not a race.
#[cfg(test)]
async fn caught_up(publisher: &Publisher, cursor: &Cursor) {
    for _ in 0..1_000 {
        if publisher.lock().cursor == *cursor {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the publisher never reached {cursor:?}");
}

/// The cursor starts at the table's last row, and one commit of 1 200 rows
/// reaches every subscriber in order through three pages (§4.1).
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_starts_at_the_tail_and_pages() {
    let (_dir, store) = crate::store::open_temp();
    let before = seed(&store, None, 3, 1_000);
    let publisher = Publisher::new(store.clone()).unwrap();
    let (mut queue, tail) = publisher.subscribe();
    assert_eq!(
        &tail,
        before.last().unwrap(),
        "a fresh subscriber starts past every row that already existed"
    );

    let (_shutdown, shutdown_rx) = watch::channel(false);
    tokio::spawn(run(publisher.clone(), shutdown_rx));

    let committed = seed(&store, None, 1_200, 2_000);
    for cursor in &committed {
        let event: Value = serde_json::from_str(&queue.recv().await.expect("every row is pushed"))
            .expect("a JSON frame");
        assert_eq!(id_of(&event), cursor.1, "in commit order, page after page");
    }
    caught_up(&publisher, committed.last().unwrap()).await;
}

/// The gapless reconnect, the `tail` frame a fresh client is given first, and
/// an `after` naming no row (§4.2, §4.4 "Gapless reconnect").
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tail_socket_replays_a_gap_then_streams() {
    let served = crate::api::serve().await;
    let base = served.base.clone();
    let bearer = crate::api::CREDENTIAL;

    // A fresh client is told where it starts before anything else.
    let mut first = open(&base, json!({ "bearer": bearer })).await;
    assert_eq!(next(&mut first).await, json!({ "tail": "0:" }));

    let read = seed(&served.store, None, 20, 1_000);
    assert_eq!(
        next(&mut first).await,
        json!({
            "id": read[0].1,
            "kind": "POLICY_CHANGED",
            "occurred_at_ns": read[0].0,
            "payload": { "n": 0 },
        }),
        "an installation-wide row carries no desk_id"
    );
    for cursor in &read[1..] {
        assert_eq!(id_of(&next(&mut first).await), cursor.1);
    }
    drop(first);

    // 600 rows commit while it is away — more than one replay page (§4.1).
    let missed = seed(&served.store, None, 600, 2_000);
    caught_up(&served.events, missed.last().unwrap()).await;
    let at_tail = json!({ "tail": format_cursor(missed.last().unwrap()) });

    let mut back = open(
        &base,
        json!({ "bearer": bearer, "after": format_cursor(read.last().unwrap()) }),
    )
    .await;
    for cursor in &missed {
        assert_eq!(id_of(&next(&mut back).await), cursor.1, "exactly the gap");
    }
    assert_eq!(next(&mut back).await, at_tail);

    // A client with no `after`, and one whose `after` names no row, both start
    // at the tail and are told so.
    let mut fresh = open(&base, json!({ "bearer": bearer })).await;
    assert_eq!(next(&mut fresh).await, at_tail);
    let mut bogus = open(&base, json!({ "bearer": bearer, "after": "1:not-a-row" })).await;
    assert_eq!(next(&mut bogus).await, at_tail);

    // Live rows continue from that tail on all three.
    let live = seed(&served.store, None, 1, 3_000);
    for socket in [&mut back, &mut fresh, &mut bogus] {
        assert_eq!(id_of(&next(socket).await), live[0].1);
    }
}

/// A subscriber that stops reading is closed `4408` while another keeps
/// receiving and the commits continue (§4.1, §4.4 "Slow consumer").
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tail_socket_closes_a_slow_consumer() {
    /// A flood in paced batches: sixteen kibibytes a row so the loopback
    /// buffers cannot absorb the idle client's thousand-frame queue, and a
    /// pause between batches so the reading client stays ahead of its own.
    const BATCH: usize = 120;
    const BATCHES: usize = 12;
    const PAD: usize = 16 * 1024;
    let served = crate::api::serve().await;
    let base = served.base.clone();
    let bearer = crate::api::CREDENTIAL;

    let mut idle = open(&base, json!({ "bearer": bearer })).await;
    let mut reading = open(&base, json!({ "bearer": bearer })).await;
    assert_eq!(next(&mut idle).await, json!({ "tail": "0:" }));
    assert_eq!(next(&mut reading).await, json!({ "tail": "0:" }));

    // The reader drains everything; `idle` reads nothing from here on.
    let drain = tokio::spawn(async move {
        let mut seen = 0usize;
        while seen < BATCH * BATCHES {
            match reading.next().await {
                Some(Ok(Wire::Text(_))) => seen += 1,
                Some(Ok(_)) => {}
                other => panic!("the reading subscriber was cut off at {seen}: {other:?}"),
            }
        }
        seen
    });
    let mut last = None;
    for batch in 0..BATCHES {
        last = seed_padded(
            &served.store,
            None,
            BATCH,
            1_000 + (batch as i64) * 1_000,
            PAD,
        )
        .last()
        .cloned();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    assert_eq!(
        closed(&mut idle).await,
        4408,
        "the queue filled and was dropped"
    );
    assert_eq!(
        drain.await.unwrap(),
        BATCH * BATCHES,
        "the reader is unaffected"
    );
    caught_up(&served.events, &last.unwrap()).await;
}

/// The first frame's three refusals: no frame in the budget and a wrong bearer
/// are `4401`, anything unparseable is `4400` (§4.2).
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tail_socket_refuses_a_bad_first_frame() {
    let served = crate::api::serve().await;
    let base = served.base.clone();

    // Started first so its five-second budget elapses under the rest.
    let mut silent = dial(&base).await;

    let mut wrong = open(&base, json!({ "bearer": "0".repeat(64) })).await;
    assert_eq!(closed(&mut wrong).await, 4401);

    for first in [
        "not json at all",
        r#"{"after":"1:x"}"#,
        "[]",
        r#"{"bearer":7}"#,
    ] {
        let mut garbage = dial(&base).await;
        garbage.send(Wire::Text(first.into())).await.unwrap();
        assert_eq!(closed(&mut garbage).await, 4400, "first frame {first:?}");
    }

    // A binary first frame presents no credential either.
    let mut binary = dial(&base).await;
    binary
        .send(Wire::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(closed(&mut binary).await, 4401);

    assert_eq!(
        closed(&mut silent).await,
        4401,
        "no first frame in five seconds"
    );
}

/// `GET /events` (§4.3): the header bearer, the two validations, newest-first
/// paging through `before`, and the desk filter.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_listing_pages_and_filters() {
    use crate::api::{CREDENTIAL, call_get, expect_envelope, json as parse};

    let served = crate::api::serve().await;
    let base = served.base.clone();
    let url = |query: &str| format!("{base}/events{query}");
    let ok = Some(CREDENTIAL);

    // The listing half sits outside the bearer layer and checks the header
    // itself, so it answers the same envelope every other route does.
    expect_envelope(call_get(url(""), None), 401, "UNAUTHORIZED");
    expect_envelope(call_get(url(""), Some("wrong")), 401, "UNAUTHORIZED");
    for query in [
        "?before=nope",
        "?before=1",
        "?limit=0",
        "?limit=501",
        "?limit=x",
    ] {
        expect_envelope(call_get(url(query), ok), 400, "VALIDATION");
    }

    let wide = seed(&served.store, None, 5, 1_000);
    served
        .store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES ('d1','alpha','READY','/desks/alpha',1,1)",
                [],
            )
        })
        .unwrap();
    let scoped = seed(&served.store, Some("d1"), 3, 2_000);
    let newest_first: Vec<String> = scoped
        .iter()
        .rev()
        .chain(wide.iter().rev())
        .map(|(_, id)| id.clone())
        .collect();

    let ids = |body: &str| -> Vec<String> {
        parse(body)["events"]
            .as_array()
            .expect("an events array")
            .iter()
            .map(id_of)
            .collect()
    };

    // The default page carries everything, newest first, and nothing behind it.
    let (status, body) = call_get(url(""), ok);
    assert_eq!(status, 200);
    assert_eq!(ids(&body), newest_first);
    assert!(parse(&body).get("next_before").is_none());

    // `before` pages back until a short page ends the walk.
    let mut walked = Vec::new();
    let mut query = "?limit=3".to_string();
    loop {
        let (status, body) = call_get(url(&query), ok);
        assert_eq!(status, 200);
        walked.extend(ids(&body));
        match parse(&body).get("next_before").and_then(Value::as_str) {
            Some(next) => query = format!("?limit=3&before={next}"),
            None => break,
        }
    }
    assert_eq!(walked, newest_first);

    // The desk filter excludes the installation-wide rows.
    let (status, body) = call_get(url("?desk_id=d1"), ok);
    assert_eq!(status, 200);
    assert_eq!(
        ids(&body),
        scoped
            .iter()
            .rev()
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(parse(&body)["events"][0]["desk_id"].as_str(), Some("d1"));
    assert_eq!(
        ids(&call_get(url("?desk_id=nobody"), ok).1),
        Vec::<String>::new()
    );
}
