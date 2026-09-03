//! Native-session pointers, managed-process rows, and the hook ingress.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §5.2, §6.2, §7, §8
//! (per R3-6, R3-7), root `sdd/SPEC.md` §6.2 and §15. The adapters own the
//! processes; this module owns their rows.

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::desk::{self, DeskError};
use crate::store::{Store, StoreError, now_ns};

/// One `agent_processes` row as `GET /desks/{d}/session` reports it (§7).
#[derive(Debug, Clone, Serialize)]
pub struct Process {
    pub id: String,
    pub runtime: String,
    pub native_session_id: Option<String>,
    pub pid: i64,
    pub started_at_ns: i64,
    pub ready_at_ns: Option<i64>,
}

const SELECT: &str =
    "SELECT id, runtime, native_session_id, pid, started_at_ns, ready_at_ns FROM agent_processes";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Process> {
    Ok(Process {
        id: row.get(0)?,
        runtime: row.get(1)?,
        native_session_id: row.get(2)?,
        pid: row.get(3)?,
        started_at_ns: row.get(4)?,
        ready_at_ns: row.get(5)?,
    })
}

/// The desk's open process, or `None` (§7). The partial unique index makes
/// "open" single by construction.
pub fn live_process(store: &Store, desk_id: &str) -> Result<Option<Process>, StoreError> {
    let desk_id = desk_id.to_string();
    store.call(move |c| {
        c.query_row(
            &format!("{SELECT} WHERE desk_id = ?1 AND ended_at_ns IS NULL"),
            [desk_id],
            read,
        )
        .optional()
    })
}

/// The desk's pointers as `{runtime: native_session_id}` (`GET /desks/{d}`, §7).
pub fn pointers(store: &Store, desk_id: &str) -> Result<Value, StoreError> {
    let desk_id = desk_id.to_string();
    let pairs: Vec<(String, String)> = store.call(move |c| {
        c.prepare(
            "SELECT runtime, native_session_id FROM native_sessions WHERE desk_id = ?1 \
             ORDER BY runtime",
        )?
        .query_map([desk_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
    })?;
    Ok(Value::Object(
        pairs
            .into_iter()
            .map(|(runtime, id)| (runtime, Value::String(id)))
            .collect::<Map<_, _>>(),
    ))
}

/// Opens the process row and appends `SESSION_STARTED` in one unit (§6.1).
/// `also` runs inside that same unit with the unit's instant — the dispatcher
/// heads the FIFO with a new session's orientation and disclosure there.
pub fn open_process<F>(
    store: &Store,
    desk_id: &str,
    runtime: &str,
    activation: &Activation,
    daemon_uuid: &str,
    mode: &str,
    also: F,
) -> Result<Process, StoreError>
where
    F: FnOnce(&Transaction<'_>, i64) -> rusqlite::Result<()> + Send + 'static,
{
    let process = Process {
        id: Uuid::now_v7().to_string(),
        runtime: runtime.to_string(),
        native_session_id: activation.native_session_id.clone(),
        pid: i64::from(activation.pid),
        started_at_ns: now_ns(),
        ready_at_ns: None,
    };
    let row = process.clone();
    let desk_id = desk_id.to_string();
    let mode = mode.to_string();
    let daemon_uuid = daemon_uuid.to_string();
    store.unit(move |tx| {
        tx.execute(
            "INSERT INTO agent_processes (id, desk_id, runtime, native_session_id, pid, \
             daemon_uuid, started_at_ns) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.id,
                desk_id,
                row.runtime,
                row.native_session_id,
                row.pid,
                daemon_uuid,
                row.started_at_ns
            ],
        )?;
        desk::append_event(
            tx,
            "SESSION_STARTED",
            Some(&desk_id),
            row.started_at_ns,
            json!({
                "runtime": row.runtime,
                "mode": mode,
                "native_session_id": row.native_session_id,
            }),
        )?;
        also(tx, row.started_at_ns)
    })?;
    Ok(process)
}

/// Readiness: `ready_at_ns` plus one `SESSION_READY` (§6.2). A closed row is
/// left alone and no event is appended.
pub fn mark_ready(store: &Store, process_id: &str) -> Result<(), StoreError> {
    let process_id = process_id.to_string();
    store.unit(move |tx| {
        let desk_id: Option<String> = tx
            .query_row(
                "SELECT desk_id FROM agent_processes WHERE id = ?1 AND ended_at_ns IS NULL \
                 AND ready_at_ns IS NULL",
                [&process_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(desk_id) = desk_id else {
            return Ok(());
        };
        let at_ns = now_ns();
        tx.execute(
            "UPDATE agent_processes SET ready_at_ns = ?2 WHERE id = ?1",
            params![process_id, at_ns],
        )?;
        desk::append_event(tx, "SESSION_READY", Some(&desk_id), at_ns, json!({}))
    })
}

/// Closes the row and appends `SESSION_EXITED` in the same unit (per R3-6).
/// Closing an already-closed row is a no-op.
pub fn close_process(
    store: &Store,
    process_id: &str,
    reason: &str,
    code: Option<i32>,
) -> Result<(), StoreError> {
    let process_id = process_id.to_string();
    let reason = reason.to_string();
    store.unit(move |tx| {
        let desk_id: Option<String> = tx
            .query_row(
                "SELECT desk_id FROM agent_processes WHERE id = ?1 AND ended_at_ns IS NULL",
                [&process_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(desk_id) = desk_id else {
            return Ok(());
        };
        let at_ns = now_ns();
        tx.execute(
            "UPDATE agent_processes SET ended_at_ns = ?2, exit_reason = ?3, exit_code = ?4 \
             WHERE id = ?1",
            params![process_id, at_ns, reason, code],
        )?;
        desk::append_event(
            tx,
            "SESSION_EXITED",
            Some(&desk_id),
            at_ns,
            json!({ "reason": reason, "code": code }),
        )
    })
}

/// Ends every open row of this daemon with one reason — Quit's `QUIT` (§7).
pub fn close_all(store: &Store, daemon_uuid: &str, reason: &str) -> Result<(), StoreError> {
    let (daemon_uuid, reason) = (daemon_uuid.to_string(), reason.to_string());
    store.unit(move |tx| {
        let open: Vec<(String, String)> = tx
            .prepare(
                "SELECT id, desk_id FROM agent_processes WHERE ended_at_ns IS NULL \
                 AND daemon_uuid = ?1 ORDER BY started_at_ns, id",
            )?
            .query_map([&daemon_uuid], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let at_ns = now_ns();
        for (id, desk_id) in open {
            tx.execute(
                "UPDATE agent_processes SET ended_at_ns = ?2, exit_reason = ?3 WHERE id = ?1",
                params![id, at_ns, reason],
            )?;
            desk::append_event(
                tx,
                "SESSION_EXITED",
                Some(&desk_id),
                at_ns,
                json!({ "reason": reason, "code": Value::Null }),
            )?;
        }
        Ok(())
    })
}

/// Fills in the open row's `native_session_id` once the adapter learns it
/// (Codex discovers its thread after the spawn, §4.2). Never overwrites.
pub fn adopt_native_session(
    store: &Store,
    desk_id: &str,
    native_session_id: &str,
) -> Result<(), StoreError> {
    let (desk_id, native) = (desk_id.to_string(), native_session_id.to_string());
    store.unit(move |tx| {
        tx.execute(
            "UPDATE agent_processes SET native_session_id = ?2 WHERE desk_id = ?1 \
             AND ended_at_ns IS NULL AND native_session_id IS NULL",
            params![desk_id, native],
        )?;
        Ok(())
    })
}

/// Moves the desk's pointer for one runtime and appends
/// `SESSION_POINTER_CHANGED {from, to, cause}` (§5.2, §6.2). `to` of `None`
/// clears it — the unresumable path.
pub fn repoint(
    store: &Store,
    desk_id: &str,
    runtime: &str,
    to: Option<&str>,
    cause: &str,
) -> Result<(), StoreError> {
    let desk_id = desk_id.to_string();
    let runtime = runtime.to_string();
    let to = to.map(str::to_string);
    let cause = cause.to_string();
    store.unit(move |tx| {
        let from: Option<String> = tx
            .query_row(
                "SELECT native_session_id FROM native_sessions WHERE desk_id = ?1 AND runtime = ?2",
                params![desk_id, runtime],
                |r| r.get(0),
            )
            .optional()?;
        if from.as_deref() == to.as_deref() {
            return Ok(());
        }
        let at_ns = now_ns();
        match &to {
            Some(id) => tx.execute(
                "INSERT INTO native_sessions (desk_id, runtime, native_session_id, updated_at_ns) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT (desk_id, runtime) DO UPDATE SET \
                 native_session_id = excluded.native_session_id, \
                 updated_at_ns = excluded.updated_at_ns",
                params![desk_id, runtime, id, at_ns],
            )?,
            None => tx.execute(
                "DELETE FROM native_sessions WHERE desk_id = ?1 AND runtime = ?2",
                params![desk_id, runtime],
            )?,
        };
        desk::append_event(
            tx,
            "SESSION_POINTER_CHANGED",
            Some(&desk_id),
            at_ns,
            json!({ "runtime": runtime, "from": from, "to": to, "cause": cause }),
        )
    })
}

/// Recovery's `sessions` step (§8), registered after `executions`: a crashed
/// daemon's open processes are `DAEMON_LOST`, and every prompt it had begun
/// handing off is `HANDOFF_UNKNOWN`. Returns `(sessions_lost, prompts_unknown)`
/// for the `RECOVERY` payload.
pub fn recovery_step(
    tx: &Transaction<'_>,
    daemon_uuid: &str,
    now_ns: i64,
) -> rusqlite::Result<(Vec<Value>, Vec<Value>)> {
    let lost: Vec<(String, String, String, Option<String>)> = {
        let mut stmt = tx.prepare(
            "SELECT id, desk_id, runtime, native_session_id FROM agent_processes \
             WHERE ended_at_ns IS NULL AND daemon_uuid <> ?1 ORDER BY started_at_ns, id",
        )?;
        stmt.query_map(params![daemon_uuid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut sessions_lost = Vec::new();
    for (id, desk_id, runtime, native_session_id) in lost {
        tx.execute(
            "UPDATE agent_processes SET ended_at_ns = ?2, exit_reason = 'DAEMON_LOST' \
             WHERE id = ?1",
            params![id, now_ns],
        )?;
        desk::append_event(
            tx,
            "SESSION_EXITED",
            Some(&desk_id),
            now_ns,
            json!({ "reason": "DAEMON_LOST", "code": Value::Null }),
        )?;
        sessions_lost.push(json!({
            "process_id": id,
            "desk_id": desk_id,
            "runtime": runtime,
            "native_session_id": native_session_id,
        }));
    }

    let attempted: Vec<(String, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, desk_id, kind FROM prompts \
             WHERE state = 'QUEUED' AND attempted_at_ns IS NOT NULL ORDER BY created_at_ns, id",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut prompts_unknown = Vec::new();
    for (id, desk_id, kind) in attempted {
        tx.execute(
            "UPDATE prompts SET state = 'FAILED', resolved_at_ns = ?2, \
             failure_code = 'HANDOFF_UNKNOWN' WHERE id = ?1",
            params![id, now_ns],
        )?;
        desk::append_event(
            tx,
            "PROMPT_FAILED",
            Some(&desk_id),
            now_ns,
            json!({ "prompt_id": id, "kind": kind, "failure_code": "HANDOFF_UNKNOWN" }),
        )?;
        prompts_unknown.push(json!({
            "prompt_id": id,
            "desk_id": desk_id,
            "kind": kind,
            "failure_code": "HANDOFF_UNKNOWN",
        }));
    }
    Ok((sessions_lost, prompts_unknown))
}

// ---------------------------------------------------------------------------
// Hook ingress (§5.2)
// ---------------------------------------------------------------------------

/// What the hook route answers with. Everything well-formed is `202`; only an
/// unparseable body is a refusal the CLI still swallows.
pub enum Hook {
    Accepted,
    Unparseable,
}

/// Records one Claude Code hook object against the desk (§5.2). The desk must
/// exist; every other outcome is a row or nothing.
pub fn hook(
    store: &Store,
    desk_id: &str,
    body: &str,
    events: Option<AdapterEvents>,
) -> Result<Hook, DeskError> {
    let desk = desk::get(store, desk_id)?;
    let Some(event) = serde_json::from_str::<Value>(body)
        .ok()
        .filter(Value::is_object)
    else {
        return Ok(Hook::Unparseable);
    };
    let name = event["hook_event_name"].as_str().unwrap_or_default();
    let session_id = event["session_id"].as_str();
    let source = event["source"].as_str();
    // Hooks arrive from the runtime the desk is on; the pointer is that
    // runtime's (§5.2 is Claude's, but the column is per runtime).
    let runtime = desk.selected_runtime.clone();

    // `clear` is the one transition that moves the pointer instead of being
    // judged against it: the payload carries the *new* `session_id` and names
    // no prior one, so the desk's own live Claude process row is the only
    // evidence that this clear is ours. Anything else is a foreign session.
    if name == "SessionStart" && source == Some("clear") {
        let ours = runtime == "claude"
            && live_process(store, &desk.id)?.is_some_and(|p| p.runtime == "claude");
        return match (ours, session_id) {
            (true, Some(id)) => {
                repoint(store, &desk.id, &runtime, Some(id), "clear")?;
                Ok(Hook::Accepted)
            }
            _ => {
                attention(
                    store,
                    &desk.id,
                    json!({ "kind": "foreign_session", "session_id": session_id }),
                )?;
                Ok(Hook::Accepted)
            }
        };
    }

    let pointer: Option<String> = {
        let pointers = pointers(store, &desk.id)?;
        pointers[&runtime].as_str().map(str::to_string)
    };
    if session_id.is_some() && session_id != pointer.as_deref() {
        attention(
            store,
            &desk.id,
            json!({ "kind": "foreign_session", "session_id": session_id }),
        )?;
        return Ok(Hook::Accepted);
    }

    match name {
        // Pointer confirmation only; readiness is the channel's (§5.3). The
        // dispatcher hears the confirmation so it can tell a resume that took
        // from one that never did (§6.2).
        "SessionStart" if matches!(source, Some("startup") | Some("resume")) => {
            if let (Some(events), Some(id)) = (&events, session_id) {
                let _ = events.send(AdapterEvent::PointerDiscovered {
                    desk_id: desk.id.clone(),
                    native_session_id: id.to_string(),
                });
            }
        }
        "SessionStart" => attention(
            store,
            &desk.id,
            json!({ "kind": "session_start", "source": source }),
        )?,
        "Notification" => attention(
            store,
            &desk.id,
            json!({
                "kind": event["notification_type"].as_str(),
                "title": event["title"].as_str(),
            }),
        )?,
        "Stop" => {
            let at_ns = now_ns();
            let desk_id = desk.id.clone();
            store.unit(move |tx| {
                desk::append_event(tx, "SESSION_TURN_ENDED", Some(&desk_id), at_ns, json!({}))
            })?;
        }
        _ => {}
    }
    Ok(Hook::Accepted)
}

fn attention(store: &Store, desk_id: &str, payload: Value) -> Result<(), StoreError> {
    let at_ns = now_ns();
    let desk_id = desk_id.to_string();
    store
        .unit(move |tx| desk::append_event(tx, "SESSION_ATTENTION", Some(&desk_id), at_ns, payload))
}

// ---------------------------------------------------------------------------
// session::recovery (feature SPEC §10 check 7)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[test]
fn recovery_closes_lost_sessions_and_prompts() {
    let (_dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES ('d1','alpha','READY','/desks/alpha',1,2)",
                [],
            )?;
            // One process of a dead daemon, one of ours.
            tx.execute(
                "INSERT INTO agent_processes (id, desk_id, runtime, native_session_id, pid, \
                 daemon_uuid, started_at_ns) VALUES ('p1','d1','codex','th-1',10,'dead',5)",
                [],
            )?;
            tx.execute(
                "INSERT INTO agent_processes (id, desk_id, runtime, pid, daemon_uuid, \
                 started_at_ns, ended_at_ns, exit_reason) \
                 VALUES ('p0','d1','codex',9,'dead',3,4,'EXITED')",
                [],
            )?;
            // One prompt mid-attempt, one merely queued.
            tx.execute(
                "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns, \
                 attempted_at_ns) VALUES ('q1','d1','TRIGGER_RESULT','QUEUED','{}',6,7)",
                [],
            )?;
            tx.execute(
                "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
                 VALUES ('q2','d1','EVALUATION','QUEUED','{}',8)",
                [],
            )
        })
        .unwrap();

    let (sessions_lost, prompts_unknown) =
        store.unit(|tx| recovery_step(tx, "alive", 100)).unwrap();
    assert_eq!(sessions_lost.len(), 1);
    assert_eq!(sessions_lost[0]["process_id"], "p1");
    assert_eq!(sessions_lost[0]["native_session_id"], "th-1");
    assert_eq!(prompts_unknown.len(), 1);
    assert_eq!(prompts_unknown[0]["prompt_id"], "q1");
    assert_eq!(prompts_unknown[0]["failure_code"], "HANDOFF_UNKNOWN");

    let rows: Vec<(String, Option<i64>, Option<String>)> = store
        .call(|c| {
            c.prepare("SELECT id, ended_at_ns, exit_reason FROM agent_processes ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })
        .unwrap();
    assert_eq!(
        rows,
        [
            ("p0".to_string(), Some(4), Some("EXITED".to_string())),
            ("p1".to_string(), Some(100), Some("DAEMON_LOST".to_string())),
        ]
    );
    let prompts: Vec<(String, String, Option<String>)> = store
        .call(|c| {
            c.prepare("SELECT id, state, failure_code FROM prompts ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })
        .unwrap();
    assert_eq!(
        prompts,
        [
            (
                "q1".to_string(),
                "FAILED".to_string(),
                Some("HANDOFF_UNKNOWN".to_string())
            ),
            ("q2".to_string(), "QUEUED".to_string(), None),
        ]
    );

    // A second recovery finds nothing left to settle.
    let (again, and) = store.unit(|tx| recovery_step(tx, "alive", 200)).unwrap();
    assert!(again.is_empty() && and.is_empty());
}

// ---------------------------------------------------------------------------
// session::hook_ingress (feature SPEC §5.2)
// ---------------------------------------------------------------------------

/// The mappings the ingress owns on its own: the `clear` repoint, the foreign
/// session, `Stop`, and an unparseable body. The rest of check 4 — the launch
/// files and the channel — belongs to the Claude adapter.
#[cfg(test)]
#[test]
fn hook_ingress_records_the_documented_rows() {
    let (_dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, \
                 selected_runtime) VALUES ('d1','alpha','READY','/desks/alpha',1,2,'claude')",
                [],
            )
        })
        .unwrap();
    let events = || -> Vec<(String, String)> {
        store
            .call(|c| {
                c.prepare(
                    "SELECT kind, payload FROM operational_events ORDER BY occurred_at_ns, id",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
            })
            .unwrap()
    };
    let (adapter_events, mut confirmations) = tokio::sync::mpsc::unbounded_channel();
    let post = |body: &str| hook(&store, "d1", body, Some(adapter_events.clone())).unwrap();

    // An unparseable body — and a JSON scalar — is refused; nothing is recorded.
    assert!(matches!(post("not json"), Hook::Unparseable));
    assert!(matches!(post("[1]"), Hook::Unparseable));
    assert!(events().is_empty());

    // No pointer yet, so a startup hook is foreign.
    assert!(matches!(
        post(r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"s-1"}"#),
        Hook::Accepted
    ));
    assert_eq!(events()[0].0, "SESSION_ATTENTION");
    assert!(events()[0].1.contains("foreign_session"));

    // A `clear` with no live Claude process of ours is another session's, and
    // never repoints this desk (§5.2).
    post(r#"{"hook_event_name":"SessionStart","source":"clear","session_id":"s-9"}"#);
    assert_eq!(pointers(&store, "d1").unwrap(), json!({}));
    assert_eq!(events().pop().unwrap().0, "SESSION_ATTENTION");
    assert!(events().pop().unwrap().1.contains("foreign_session"));

    // With this desk's Claude session live, `clear` moves the pointer whatever
    // it was.
    open_process(
        &store,
        "d1",
        "claude",
        &Activation {
            pid: 1,
            native_session_id: None,
        },
        "daemon",
        "NEW",
        |_, _| Ok(()),
    )
    .unwrap();
    post(r#"{"hook_event_name":"SessionStart","source":"clear","session_id":"s-2"}"#);
    assert_eq!(pointers(&store, "d1").unwrap(), json!({ "claude": "s-2" }));
    let changed = events().pop().unwrap();
    assert_eq!(changed.0, "SESSION_POINTER_CHANGED");
    assert!(changed.1.contains(r#""cause":"clear""#));

    // With the pointer in place a startup or resume hook confirms it: no row,
    // one `PointerDiscovered` for the dispatcher.
    for source in ["startup", "resume"] {
        let before = events().len();
        post(&format!(
            r#"{{"hook_event_name":"SessionStart","source":"{source}","session_id":"s-2"}}"#
        ));
        assert_eq!(events().len(), before, "confirmation records nothing");
        assert!(matches!(
            confirmations.try_recv(),
            Ok(AdapterEvent::PointerDiscovered { native_session_id, .. })
                if native_session_id == "s-2"
        ));
    }

    // Another `source` is attention only.
    post(r#"{"hook_event_name":"SessionStart","source":"compact","session_id":"s-2"}"#);
    let compaction = events().pop().unwrap();
    assert_eq!(compaction.0, "SESSION_ATTENTION");
    assert!(compaction.1.contains(r#""kind":"session_start""#));
    post(r#"{"hook_event_name":"Stop","session_id":"s-2"}"#);
    assert_eq!(events().last().unwrap().0, "SESSION_TURN_ENDED");
    post(
        r#"{"hook_event_name":"Notification","session_id":"s-2","notification_type":"permission",
            "title":"Approve?","message":"never stored"}"#,
    );
    let notification = events().pop().unwrap();
    assert_eq!(notification.0, "SESSION_ATTENTION");
    assert!(notification.1.contains(r#""kind":"permission""#));
    assert!(notification.1.contains(r#""title":"Approve?""#));
    assert!(
        !notification.1.contains("never stored"),
        "the message is not stored"
    );

    // An unknown event is accepted (the route answers 202) and recorded nowhere.
    let before = events().len();
    post(r#"{"hook_event_name":"PreToolUse","session_id":"s-2"}"#);
    assert_eq!(events().len(), before);
}

// ---------------------------------------------------------------------------
// The adapter seam (§4, §5, §6) — one trait per runtime behind one dispatcher.
// ---------------------------------------------------------------------------

/// What one delivery attempt came to (§6.2). `Waiting` is not a failure: the
/// gate is closed (a turn is active, or a channel has not connected yet) and
/// the prompt stays `QUEUED` for the next pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliverOutcome {
    Delivered,
    /// `DELIVERY_REFUSED` with the runtime's own message.
    Refused(String),
    HandoffUnknown,
    ChannelUnavailable,
    Waiting,
}

/// What an adapter tells the dispatcher out of band, over the one `mpsc` the
/// dispatcher (C27) drains. Everything here is evidence; the dispatcher writes
/// the rows.
#[derive(Debug, Clone)]
pub enum AdapterEvent {
    /// The session is live and the delivery gate is open (§6.2).
    Ready { desk_id: String },
    /// `native_sessions[desk, runtime]` learned or changed (§4.2, §5.1).
    PointerDiscovered {
        desk_id: String,
        native_session_id: String,
    },
    /// `SESSION_ATTENTION {kind, …}` (§4.1, §5.2).
    Attention {
        desk_id: String,
        kind: String,
        detail: Value,
    },
    /// The process ended; `reason` is an `agent_processes.exit_reason`.
    Exited {
        desk_id: String,
        reason: &'static str,
        code: Option<i64>,
    },
}

impl AdapterEvent {
    /// The desk every variant carries, so the dispatcher can hold an event for
    /// a desk whose row is still being opened (§6.1).
    pub fn desk_id(&self) -> &str {
        match self {
            AdapterEvent::Ready { desk_id }
            | AdapterEvent::PointerDiscovered { desk_id, .. }
            | AdapterEvent::Attention { desk_id, .. }
            | AdapterEvent::Exited { desk_id, .. } => desk_id,
        }
    }
}

/// The channel every adapter reports on, held by the dispatcher.
pub type AdapterEvents = tokio::sync::mpsc::UnboundedSender<AdapterEvent>;

/// One started session, ready for the `agent_processes` insert (§6.1).
#[derive(Debug, Clone)]
pub struct Activation {
    pub pid: u32,
    /// Known at spawn for Claude, discovered later for Codex.
    pub native_session_id: Option<String>,
}

/// One runtime's mechanics (§4, §5). The dispatcher owns activation policy,
/// the FIFO, the rows, and the renderings; an adapter owns only the runtime.
#[async_trait::async_trait]
pub trait Adapter: Send + Sync {
    /// Starts a session for the desk: `resume` carries the pointer for a resume
    /// and is `None` for a new session. `Err` is the activation failure detail.
    async fn spawn(&self, desk_id: &str, resume: Option<&str>) -> Result<Activation, String>;

    /// Hands one rendered prompt (§6.3) to the live session. `prompt_id` and
    /// `kind` ride along because Claude's channel frame carries them as the
    /// event's meta (§5.3); Codex has no use for them.
    async fn deliver(
        &self,
        desk_id: &str,
        prompt_id: &str,
        kind: &str,
        text: &str,
    ) -> DeliverOutcome;

    /// Interrupts the active turn; `Ok` carries the turn id (§4.3), `Err` a
    /// failure code from §7's answers (`NO_ACTIVE_TURN`, `INTERRUPT_UNSUPPORTED`,
    /// `RUNTIME_ERROR`) with its message.
    async fn interrupt(&self, desk_id: &str) -> Result<String, (&'static str, String)>;

    /// Ends the session's process tree; the `Exited` event follows.
    async fn exit(&self, desk_id: &str);

    /// The dispatcher has finished the spawn — the process row exists, or the
    /// spawn failed and none will (§6.1). Claude closes its spawn-in-flight
    /// window here.
    fn settled(&self, _desk_id: &str) {}

    /// The desk's process row has closed, however it ended (§6.2). Claude
    /// deletes the launch files here.
    fn closed(&self, _desk_id: &str) {}

    /// `POST /runtimes/{r}/retry` cleared the row's failure; the adapter drops
    /// whatever failure state of its own it was counting (§4.1).
    fn reset_failures(&self) {}
}

/// §4.2's launch environment, shared by both adapters: the captured login
/// `PATH`, `TERM`, the desk id, and the daemon's `HOME` and locale. Secrets are
/// each adapter's own to add.
/// Windows has no usable process without its extra variables: Winsock needs
/// `SYSTEMROOT`, the shell shims need `COMSPEC` and `PATHEXT`, and the
/// runtimes' own state lives under the profile directories (§4.2).
fn keep_env(key: &str) -> bool {
    if key == "HOME" || key == "LANG" || key.starts_with("LC_") {
        return true;
    }
    #[cfg(windows)]
    if matches!(
        key.to_ascii_uppercase().as_str(),
        "SYSTEMROOT"
            | "COMSPEC"
            | "PATHEXT"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "APPDATA"
            | "LOCALAPPDATA"
            | "TEMP"
            | "TMP"
    ) {
        return true;
    }
    false
}

pub fn base_env(search_path: &str, desk_id: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("PATH".to_string(), search_path.to_string()),
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("MARKETRIG_DESK_ID".to_string(), desk_id.to_string()),
    ];
    for (key, value) in std::env::vars() {
        if keep_env(&key) {
            env.push((key, value));
        }
    }
    env
}
