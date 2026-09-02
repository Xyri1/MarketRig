//! Activation, the delivery dispatcher, and the prompt renderings.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §6 (per R3-5) and §7's
//! lifecycle controls, root `sdd/SPEC.md` §7. The adapters own the runtimes;
//! this module owns activation policy, the FIFO, the rows, and the text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, watch};
use uuid::Uuid;

use crate::desk::{self, Desk};
use crate::session::{self, Adapter, AdapterEvent, DeliverOutcome, Process};
use crate::store::{Store, StoreError, now_ns};
use crate::terminal::TerminalExit;

/// §6.1's readiness deadline and idle poll.
const READY_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_secs(30);
/// §7: `exit` and `switch` wait this long for the row to close.
pub const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// The daemon's one dispatcher wake, so a prompt insert deep inside a trading
/// node or an execution run can signal it without threading a handle through
/// every layer.
///
/// ponytail: a process-global signal — not state, and never desk-scoped. The
/// 30-second poll is the fallback if it is unset (tests) or missed; thread a
/// handle through if a second dispatcher ever exists.
static WAKE: OnceLock<Arc<Notify>> = OnceLock::new();

/// Signals the dispatcher that a prompt may be waiting. Cheap and lossless
/// enough: `Notify::notify_one` stores a permit when nobody is waiting.
pub fn wake() {
    if let Some(notify) = WAKE.get() {
        notify.notify_one();
    }
}

/// The two runtime adapters, chosen by `desks.selected_runtime`.
pub struct Adapters {
    pub codex: Arc<dyn Adapter>,
    pub claude: Arc<dyn Adapter>,
}

impl Adapters {
    fn get(&self, runtime: &str) -> Arc<dyn Adapter> {
        match runtime {
            "claude" => self.claude.clone(),
            _ => self.codex.clone(),
        }
    }
}

/// How an activation was asked for (§7's `mode`, plus the dispatcher's own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The dispatcher: resume when there is a pointer, else start new.
    Auto,
    /// `POST …/session/activate {"mode":"CONTINUE"}` — a pointer is required
    /// and a failure is never silently a new session (root §7).
    Continue,
    New,
}

/// Why an activation could not happen; the route maps these to §7's answers.
#[derive(Debug)]
pub enum ActivateError {
    SessionLive,
    NoNativeSession,
    RuntimeUnavailable(String),
    Spawn(String),
    Desk(desk::DeskError),
}

impl From<StoreError> for ActivateError {
    fn from(e: StoreError) -> Self {
        ActivateError::Desk(desk::DeskError::Store(e))
    }
}

impl From<desk::DeskError> for ActivateError {
    fn from(e: desk::DeskError) -> Self {
        ActivateError::Desk(e)
    }
}

/// One activation the dispatcher is still waiting on (§6.2).
struct Live {
    process_id: String,
    runtime: String,
    /// A resume: the unresumable rules apply to it.
    resumed: Option<String>,
    /// The explicit Continue path never becomes a new session (root §7).
    explicit: bool,
    started: tokio::time::Instant,
    ready: bool,
    /// The unresumable retry happens once.
    retried: bool,
}

/// Activation policy and the delivery loop; shared by the dispatcher task and
/// the §7 routes so both start sessions exactly one way.
pub struct Dispatcher {
    store: Store,
    adapters: Adapters,
    daemon_uuid: String,
    notify: Arc<Notify>,
    live: Mutex<HashMap<String, Live>>,
    ready_deadline: Duration,
    poll: Duration,
}

impl Dispatcher {
    pub fn new(store: Store, adapters: Adapters, daemon_uuid: String) -> Arc<Dispatcher> {
        let notify = Arc::new(Notify::new());
        let _ = WAKE.set(notify.clone());
        Arc::new(Dispatcher {
            store,
            adapters,
            daemon_uuid,
            notify,
            live: Mutex::new(HashMap::new()),
            ready_deadline: READY_DEADLINE,
            poll: POLL,
        })
    }

    /// Wakes this dispatcher (the routes hold a handle; everything else uses
    /// the free [`wake`]).
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    pub fn adapter(&self, runtime: &str) -> Arc<dyn Adapter> {
        self.adapters.get(runtime)
    }

    /// §6.1's `activate`, and §7's route. One unit opens the process row,
    /// appends `SESSION_STARTED`, and heads the FIFO with a new session's
    /// disclosure and orientation.
    pub async fn activate(&self, desk_id: &str, mode: Mode) -> Result<Process, ActivateError> {
        let desk = desk::get(&self.store, desk_id)?;
        if session::live_process(&self.store, &desk.id)?.is_some() {
            return Err(ActivateError::SessionLive);
        }
        let runtime = desk.selected_runtime.clone();
        match crate::runtime::get(&self.store, &runtime)? {
            Some(row) if row.state == "AVAILABLE" => {}
            _ => return Err(ActivateError::RuntimeUnavailable(runtime)),
        }
        let pointer = session::pointers(&self.store, &desk.id)?[&runtime]
            .as_str()
            .map(str::to_string);
        let resume = match mode {
            Mode::New => None,
            Mode::Continue => match pointer {
                Some(id) => Some(id),
                None => return Err(ActivateError::NoNativeSession),
            },
            Mode::Auto => pointer,
        };

        let adapter = self.adapters.get(&runtime);
        let activation = adapter
            .spawn(&desk.id, resume.as_deref())
            .await
            .map_err(ActivateError::Spawn)?;

        let activation = crate::session::Activation {
            pid: activation.pid,
            native_session_id: activation.native_session_id.or_else(|| resume.clone()),
        };
        let native = activation.native_session_id.clone();
        let new_session = resume.is_none();
        let desk_id_owned = desk.id.clone();
        let process = session::open_process(
            &self.store,
            &desk.id,
            &runtime,
            &activation,
            &self.daemon_uuid,
            if new_session { "NEW" } else { "RESUME" },
            move |tx, at_ns| {
                if new_session {
                    activation_prompts(tx, &desk_id_owned, at_ns)?;
                }
                Ok(())
            },
        )?;

        // A runtime that mints its own session id knows the pointer at spawn
        // (§5.1); a runtime that discovers it later sends `PointerDiscovered`.
        if let Some(id) = &native {
            let _ = session::repoint(&self.store, &desk.id, &runtime, Some(id), "started");
        }
        self.live.lock().expect("live").insert(
            desk.id.clone(),
            Live {
                process_id: process.id.clone(),
                runtime,
                resumed: resume,
                explicit: mode == Mode::Continue,
                started: tokio::time::Instant::now(),
                ready: false,
                retried: false,
            },
        );
        self.wake();
        Ok(process)
    }

    /// §7's `exit`: end the session and wait for its row to close.
    pub async fn exit(&self, desk_id: &str) -> Result<(), StoreError> {
        let desk = match desk::get(&self.store, desk_id) {
            Ok(desk) => desk,
            Err(desk::DeskError::Store(e)) => return Err(e),
            Err(_) => return Ok(()),
        };
        self.adapters
            .get(&desk.selected_runtime)
            .exit(&desk.id)
            .await;
        Ok(())
    }

    /// Waits up to [`CLOSE_WAIT`] for the desk's process row to close (§7).
    pub async fn await_closed(&self, desk_id: &str) -> bool {
        let deadline = tokio::time::Instant::now() + CLOSE_WAIT;
        loop {
            match session::live_process(&self.store, desk_id) {
                Ok(None) => return true,
                Ok(Some(_)) => {}
                Err(_) => return false,
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Quit (§7): every open row of this daemon ends `QUIT`.
    pub fn quit_rows(&self) -> Result<(), StoreError> {
        session::close_all(&self.store, &self.daemon_uuid, "QUIT")
    }

    /// One pass of §6.1's loop.
    async fn pass(&self) {
        let desks = match queued_desks(&self.store) {
            Ok(desks) => desks,
            Err(e) => {
                tracing::error!(error = %e, "reading the prompt queue failed");
                return;
            }
        };
        for desk_id in desks {
            match session::live_process(&self.store, &desk_id) {
                Ok(None) => self.activate_for_queue(&desk_id).await,
                Ok(Some(process)) if process.ready_at_ns.is_some() => {
                    self.deliver_head(&desk_id, &process).await;
                }
                Ok(Some(_)) => {}
                Err(e) => tracing::error!(error = %e, "reading the desk's session failed"),
            }
        }
        self.check_deadlines().await;
    }

    async fn activate_for_queue(&self, desk_id: &str) {
        match self.activate(desk_id, Mode::Auto).await {
            Ok(_) | Err(ActivateError::SessionLive) => {}
            Err(ActivateError::RuntimeUnavailable(_)) => {
                fail_queued(&self.store, desk_id, "RUNTIME_UNAVAILABLE", Value::Null);
            }
            Err(ActivateError::Spawn(detail)) => {
                fail_queued(
                    &self.store,
                    desk_id,
                    "ACTIVATION_FAILED",
                    Value::String(detail),
                );
            }
            Err(e) => tracing::error!("activating {desk_id} failed: {e:?}"),
        }
    }

    /// §6.2's delivery attempt: the attempt unit, the adapter call, then the
    /// resolution unit.
    async fn deliver_head(&self, desk_id: &str, process: &Process) {
        let Ok(Some(head)) = head_prompt(&self.store, desk_id) else {
            return;
        };
        let Ok(desk) = desk::get(&self.store, desk_id) else {
            return;
        };
        let disclosed = if head.kind == "DISCLOSURE" {
            undisclosed(&self.store, desk_id).unwrap_or_default()
        } else {
            Vec::new()
        };
        let text = render(&head, &desk, &disclosed);

        if mark_attempted(&self.store, &head.id, process).is_err() {
            return;
        }
        let outcome = self
            .adapters
            .get(&process.runtime)
            .deliver(desk_id, &head.id, &head.kind, &text)
            .await;
        let (state, failure_code, detail) = match outcome {
            DeliverOutcome::Delivered => ("DELIVERED", None, Value::Null),
            DeliverOutcome::Refused(message) => {
                ("FAILED", Some("DELIVERY_REFUSED"), Value::String(message))
            }
            DeliverOutcome::HandoffUnknown => ("FAILED", Some("HANDOFF_UNKNOWN"), Value::Null),
            DeliverOutcome::ChannelUnavailable => {
                ("FAILED", Some("CHANNEL_UNAVAILABLE"), Value::Null)
            }
            // The gate is closed; the prompt is untouched, attempt and all, so
            // recovery never mistakes a wait for a handoff (§6.2, §8).
            DeliverOutcome::Waiting => {
                let _ = clear_attempt(&self.store, &head.id);
                return;
            }
        };
        let resolved = Resolution {
            prompt: head,
            desk_id: desk_id.to_string(),
            runtime: process.runtime.clone(),
            native_session_id: process.native_session_id.clone(),
            state,
            failure_code,
            detail,
            disclosed,
        };
        if let Err(e) = resolve(&self.store, resolved) {
            tracing::error!(error = %e, "recording a delivery outcome failed");
        }
        self.wake();
    }

    /// §6.2's readiness deadline.
    async fn check_deadlines(&self) {
        let now = tokio::time::Instant::now();
        let expired: Vec<String> = self
            .live
            .lock()
            .expect("live")
            .iter()
            .filter(|(_, live)| {
                !live.ready && now.duration_since(live.started) >= self.ready_deadline
            })
            .map(|(desk_id, _)| desk_id.clone())
            .collect();
        for desk_id in expired {
            self.ended(&desk_id, "EXITED", None, Some("timeout".to_string()))
                .await;
        }
    }

    /// One adapter or terminal exit (§6.2). Idempotent: whichever evidence
    /// arrives first closes the row and decides the outcome.
    async fn ended(&self, desk_id: &str, reason: &str, code: Option<i64>, detail: Option<String>) {
        let live = self.live.lock().expect("live").remove(desk_id);
        let process_id = match &live {
            Some(live) => live.process_id.clone(),
            None => match session::live_process(&self.store, desk_id) {
                Ok(Some(process)) => process.id,
                _ => return,
            },
        };
        let code_i32 = code.and_then(|c| i32::try_from(c).ok());
        if let Err(e) = session::close_process(&self.store, &process_id, reason, code_i32) {
            tracing::error!(error = %e, "closing a session row failed");
        }
        let Some(live) = live else { return };
        if live.ready {
            // A ready session ending leaves its queue alone: the next pass
            // activates again (§6.2).
            self.wake();
            return;
        }
        // A resume that never reached readiness is unresumable (§4.2, §5.1).
        if live.resumed.is_some() && !live.retried {
            if live.explicit {
                // The route already answered; evidence only (§6.2).
                return;
            }
            let _ = session::repoint(&self.store, desk_id, &live.runtime, None, "unresumable");
            match self.activate(desk_id, Mode::New).await {
                Ok(_) => {
                    if let Some(entry) = self.live.lock().expect("live").get_mut(desk_id) {
                        entry.retried = true;
                    }
                    return;
                }
                Err(e) => tracing::warn!("restarting {desk_id} after an unresumable resume: {e:?}"),
            }
        }
        let detail = detail
            .map(Value::String)
            .unwrap_or_else(|| code.map_or(Value::Null, |c| json!(c)));
        fail_queued(&self.store, desk_id, "ACTIVATION_FAILED", detail);
    }

    async fn event(&self, event: AdapterEvent) {
        match event {
            AdapterEvent::Ready { desk_id } => {
                if let Some(live) = self.live.lock().expect("live").get_mut(&desk_id) {
                    live.ready = true;
                }
                if let Ok(Some(process)) = session::live_process(&self.store, &desk_id) {
                    let _ = session::mark_ready(&self.store, &process.id);
                }
                self.wake();
            }
            AdapterEvent::PointerDiscovered {
                desk_id,
                native_session_id,
            } => {
                let runtime = self
                    .live
                    .lock()
                    .expect("live")
                    .get(&desk_id)
                    .map(|live| live.runtime.clone());
                let runtime = match runtime {
                    Some(runtime) => runtime,
                    None => match desk::get(&self.store, &desk_id) {
                        Ok(desk) => desk.selected_runtime,
                        Err(_) => return,
                    },
                };
                let _ = session::repoint(
                    &self.store,
                    &desk_id,
                    &runtime,
                    Some(&native_session_id),
                    "discovered",
                );
                let _ = session::adopt_native_session(&self.store, &desk_id, &native_session_id);
            }
            AdapterEvent::Attention {
                desk_id,
                kind,
                detail,
            } => {
                let mut payload = json!({ "kind": kind });
                if let Some(fields) = detail.as_object() {
                    for (key, value) in fields {
                        payload[key] = value.clone();
                    }
                }
                let _ = self.store.unit(move |tx| {
                    desk::append_event(tx, "SESSION_ATTENTION", Some(&desk_id), now_ns(), payload)
                });
            }
            AdapterEvent::Exited {
                desk_id,
                reason,
                code,
            } => self.ended(&desk_id, reason, code, None).await,
        }
    }
}

/// §6.1's task. Shaped like `exec::run`: wake or poll, one pass, stop on the
/// shutdown watch.
pub async fn run(
    dispatcher: Arc<Dispatcher>,
    mut events: mpsc::UnboundedReceiver<AdapterEvent>,
    mut exits: mpsc::UnboundedReceiver<TerminalExit>,
    mut shutdown: watch::Receiver<bool>,
) {
    let notify = dispatcher.notify.clone();
    while !*shutdown.borrow() {
        dispatcher.pass().await;
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(dispatcher.poll) => {}
            event = events.recv() => match event {
                Some(event) => dispatcher.event(event).await,
                None => break,
            },
            exit = exits.recv() => match exit {
                Some(exit) => {
                    dispatcher
                        .ended(&exit.desk_id, exit.reason, exit.code, None)
                        .await;
                }
                None => break,
            },
            changed = shutdown.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The queue (§6.1, §6.2)
// ---------------------------------------------------------------------------

/// One queued prompt, as the FIFO reads it.
#[derive(Debug, Clone)]
struct Prompt {
    id: String,
    kind: String,
    payload: Value,
}

struct Resolution {
    prompt: Prompt,
    desk_id: String,
    runtime: String,
    native_session_id: Option<String>,
    state: &'static str,
    failure_code: Option<&'static str>,
    detail: Value,
    disclosed: Vec<Value>,
}

fn queued_desks(store: &Store) -> Result<Vec<String>, StoreError> {
    store.call(|c| {
        c.prepare(
            "SELECT desk_id, MIN(created_at_ns) AS head FROM prompts WHERE state = 'QUEUED' \
             GROUP BY desk_id ORDER BY head, desk_id",
        )?
        .query_map([], |r| r.get(0))?
        .collect()
    })
}

fn head_prompt(store: &Store, desk_id: &str) -> Result<Option<Prompt>, StoreError> {
    let desk_id = desk_id.to_string();
    store.call(move |c| {
        c.query_row(
            "SELECT id, kind, payload FROM prompts WHERE desk_id = ?1 AND state = 'QUEUED' \
             ORDER BY created_at_ns, id LIMIT 1",
            [desk_id],
            |r| {
                let payload: String = r.get(2)?;
                Ok(Prompt {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                })
            },
        )
        .optional()
    })
}

/// Every `FAILED` prompt of the desk that has not been disclosed yet (§6.3).
fn undisclosed(store: &Store, desk_id: &str) -> Result<Vec<Value>, StoreError> {
    let desk_id = desk_id.to_string();
    store.call(move |c| {
        c.prepare(
            "SELECT id, kind, failure_code FROM prompts WHERE desk_id = ?1 AND state = 'FAILED' \
             AND disclosed_at_ns IS NULL ORDER BY created_at_ns, id",
        )?
        .query_map([desk_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "failure_code": r.get::<_, String>(2)?,
            }))
        })?
        .collect()
    })
}

fn mark_attempted(store: &Store, prompt_id: &str, process: &Process) -> Result<(), StoreError> {
    let (prompt_id, runtime) = (prompt_id.to_string(), process.runtime.clone());
    let native = process.native_session_id.clone();
    store.unit(move |tx| {
        tx.execute(
            "UPDATE prompts SET attempted_at_ns = ?2, runtime = ?3, native_session_id = ?4 \
             WHERE id = ?1",
            params![prompt_id, now_ns(), runtime, native],
        )?;
        Ok(())
    })
}

fn clear_attempt(store: &Store, prompt_id: &str) -> Result<(), StoreError> {
    let prompt_id = prompt_id.to_string();
    store.unit(move |tx| {
        tx.execute(
            "UPDATE prompts SET attempted_at_ns = NULL WHERE id = ?1",
            [prompt_id],
        )?;
        Ok(())
    })
}

/// The second unit of §6.2: the outcome, its event, and — for a delivered
/// disclosure — `disclosed_at_ns` on the rows it named.
fn resolve(store: &Store, r: Resolution) -> Result<(), StoreError> {
    store.unit(move |tx| {
        let at_ns = now_ns();
        tx.execute(
            "UPDATE prompts SET state = ?2, resolved_at_ns = ?3, failure_code = ?4 WHERE id = ?1",
            params![r.prompt.id, r.state, at_ns, r.failure_code],
        )?;
        let (kind, mut payload) = match r.failure_code {
            None => ("PROMPT_DELIVERED", json!({})),
            Some(code) => ("PROMPT_FAILED", json!({ "failure_code": code })),
        };
        payload["prompt_id"] = json!(r.prompt.id);
        payload["kind"] = json!(r.prompt.kind);
        payload["runtime"] = json!(r.runtime);
        payload["native_session_id"] = json!(r.native_session_id);
        if !r.detail.is_null() {
            payload["failure_detail"] = r.detail.clone();
        }
        if r.state == "DELIVERED" {
            for row in &r.disclosed {
                tx.execute(
                    "UPDATE prompts SET disclosed_at_ns = ?2 WHERE id = ?1",
                    params![row["id"].as_str(), at_ns],
                )?;
            }
        }
        desk::append_event(tx, kind, Some(&r.desk_id), at_ns, payload)
    })
}

/// §6.1 and §6.2: every `QUEUED` prompt of one desk fails with the same code.
fn fail_queued(store: &Store, desk_id: &str, code: &'static str, detail: Value) {
    let desk_id = desk_id.to_string();
    let outcome = store.unit(move |tx| {
        let at_ns = now_ns();
        let rows: Vec<(String, String)> = tx
            .prepare(
                "SELECT id, kind FROM prompts WHERE desk_id = ?1 AND state = 'QUEUED' \
                 ORDER BY created_at_ns, id",
            )?
            .query_map([&desk_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, kind) in rows {
            tx.execute(
                "UPDATE prompts SET state = 'FAILED', resolved_at_ns = ?2, failure_code = ?3, \
                 attempted_at_ns = NULL WHERE id = ?1",
                params![id, at_ns, code],
            )?;
            let mut payload = json!({
                "prompt_id": id, "kind": kind, "failure_code": code,
            });
            if !detail.is_null() {
                payload["failure_detail"] = detail.clone();
            }
            desk::append_event(tx, "PROMPT_FAILED", Some(&desk_id), at_ns, payload)?;
        }
        Ok(())
    });
    if let Err(e) = outcome {
        tracing::error!(error = %e, "failing a desk's queued prompts");
    }
}

/// A new session's head-of-FIFO rows (§6.1): the disclosure first when there is
/// one to make, then the orientation, both older than the activation instant.
fn activation_prompts(tx: &Transaction<'_>, desk_id: &str, at_ns: i64) -> rusqlite::Result<()> {
    let has_undisclosed: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM prompts WHERE desk_id = ?1 AND state = 'FAILED' \
         AND disclosed_at_ns IS NULL)",
        [desk_id],
        |r| r.get(0),
    )?;
    // Heading the FIFO means being older than whatever is already waiting —
    // the prompt that caused this activation was created before it (§6.1).
    let head: Option<i64> = tx.query_row(
        "SELECT MIN(created_at_ns) FROM prompts WHERE desk_id = ?1 AND state = 'QUEUED'",
        [desk_id],
        |r| r.get(0),
    )?;
    let at_ns = head.unwrap_or(at_ns).min(at_ns);
    if has_undisclosed {
        insert_prompt(tx, desk_id, "DISCLOSURE", at_ns - 2)?;
    }
    insert_prompt(tx, desk_id, "ORIENTATION", at_ns - 1)
}

fn insert_prompt(
    tx: &Transaction<'_>,
    desk_id: &str,
    kind: &str,
    created_at_ns: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
         VALUES (?1, ?2, ?3, 'QUEUED', ?4, ?5)",
        params![
            Uuid::now_v7().to_string(),
            desk_id,
            kind,
            json!({ "kind": kind }).to_string(),
            created_at_ns
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Renderings (§6.3)
// ---------------------------------------------------------------------------

/// The five market-plane resources `marketrig-mcp` serves (root §7, §8).
const RESOURCES: [&str; 5] = ["quotes", "book", "positions", "orders", "instruments"];

/// One prompt as the text the runtime receives. English only, byte-identical
/// under both locales (root "Localization").
fn render(prompt: &Prompt, desk: &Desk, disclosed: &[Value]) -> String {
    match prompt.kind.as_str() {
        "ORIENTATION" => orientation(desk),
        "DISCLOSURE" => {
            let mut text = String::from(
                "MarketRig could not deliver these prompts since your last session. \
                 Their content is not repeated and they are never redelivered.\n",
            );
            for row in disclosed {
                text.push_str(&format!(
                    "{} {} {}\n",
                    row["id"].as_str().unwrap_or_default(),
                    row["kind"].as_str().unwrap_or_default(),
                    row["failure_code"].as_str().unwrap_or_default(),
                ));
            }
            text
        }
        kind => format!(
            "MarketRig {kind} {}:\n```json\n{}\n```\n",
            prompt.id,
            serde_json::to_string_pretty(&prompt.payload).unwrap_or_default(),
        ),
    }
}

/// Root §7's paragraph: the desk, its workspace, its `AGENTS.md`, the CLI, and
/// the desk's market resources, ending by asking what the user has in mind.
fn orientation(desk: &Desk) -> String {
    let resources = RESOURCES
        .iter()
        .map(|leaf| format!("marketrig://desk/{}/{leaf}", desk.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "You are the trading agent of the MarketRig desk {name}. Its workspace is {workspace}, \
         and its durable instructions are {workspace}/AGENTS.md. The marketrig command is your \
         continuity plane: records, triggers, memory, and prompts. Its market resources are \
         {resources}. Nothing has been decided for you — what do you have in mind?\n",
        name = desk.name,
        workspace = desk.workspace_path,
    )
}

// ---------------------------------------------------------------------------
// dispatch (feature SPEC §10 checks 5 and 6)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests;
