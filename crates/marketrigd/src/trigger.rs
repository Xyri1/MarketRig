//! Trigger rows shared across the scheduler, the executor, and the routes: the
//! firing a run is about, and the `TRIGGER_RESULT` prompt its result queues.
//!
//! Contract: `sdd/features/r2-scheduled-triggers/SPEC.md` §5, §7, per R2-5.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

/// One `firings` row (§7). The accepted occurrence and the definition's wording
/// at that instant, which is what the run and its result speak about — never the
/// trigger's current wording.
#[derive(Debug, Clone)]
pub struct FiringRow {
    pub id: String,
    pub desk_id: String,
    pub trigger_id: String,
    pub occurrence_ns: i64,
    pub accepted_at_ns: i64,
    pub trigger_revision: i64,
    pub brief: String,
    pub context: Option<String>,
    pub code_snapshot_id: Option<String>,
}

/// The firing of this desk with this id, or `None`. Takes a `Connection`, so a
/// `Transaction` works through its `Deref` too.
pub fn load_firing(
    conn: &Connection,
    desk_id: &str,
    firing_id: &str,
) -> rusqlite::Result<Option<FiringRow>> {
    conn.query_row(
        "SELECT id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, trigger_revision, \
         brief, context, code_snapshot_id FROM firings WHERE desk_id = ?1 AND id = ?2",
        params![desk_id, firing_id],
        |r| {
            Ok(FiringRow {
                id: r.get(0)?,
                desk_id: r.get(1)?,
                trigger_id: r.get(2)?,
                occurrence_ns: r.get(3)?,
                accepted_at_ns: r.get(4)?,
                trigger_revision: r.get(5)?,
                brief: r.get(6)?,
                context: r.get(7)?,
                code_snapshot_id: r.get(8)?,
            })
        },
    )
    .optional()
}

/// What the result prompt says about a run (§5). The captured streams stay on
/// the `executions` row; this carries their sizes only (per R2-5).
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionSummary {
    pub outcome: String,
    pub exit_code: Option<i64>,
    pub error: Option<String>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub started_at_ns: i64,
    pub finished_at_ns: i64,
}

/// Queues one `TRIGGER_RESULT` prompt (§5), returning its id. `execution` is
/// `None` for a code-free firing, whose prompt commits in the acceptance unit.
/// Born `QUEUED` and left there until R3, like `EVALUATION`.
pub fn insert_result_prompt(
    tx: &Transaction<'_>,
    firing: &FiringRow,
    trigger_name: &str,
    execution: Option<&ExecutionSummary>,
    now_ns: i64,
) -> rusqlite::Result<String> {
    let id = Uuid::now_v7().to_string();
    let payload = json!({
        "kind": "TRIGGER_RESULT",
        "trigger_id": firing.trigger_id,
        "trigger_name": trigger_name,
        "firing_id": firing.id,
        "occurrence_ns": firing.occurrence_ns,
        "accepted_at_ns": firing.accepted_at_ns,
        "brief": firing.brief,
        "context": firing.context,
        "execution": execution,
    });
    tx.execute(
        "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
         VALUES (?1, ?2, 'TRIGGER_RESULT', 'QUEUED', ?3, ?4)",
        params![id, firing.desk_id, payload.to_string(), now_ns],
    )?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The §5 payload, round-tripped from real rows: both shapes of `execution`,
/// and `load_firing` scoped to its desk.
#[cfg(test)]
#[test]
fn result_prompt_payload() {
    let (_dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks VALUES ('d1','alpha','READY','/desks/alpha',1,2,NULL,NULL)",
                [],
            )?;
            tx.execute(
                "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                 enabled, revision, created_at_ns, updated_at_ns) \
                 VALUES ('t1','d1','nightly','SCHEDULED','ONE_OFF','look at AAPL',50,1,1,1,1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO firings VALUES ('f1','d1','t1',50,60,1,'look at AAPL',NULL,NULL)",
                [],
            )
        })
        .unwrap();

    let firing = store
        .call(|c| load_firing(c, "d1", "f1"))
        .unwrap()
        .expect("the firing loads");
    assert_eq!(
        (
            firing.trigger_id.as_str(),
            firing.occurrence_ns,
            firing.accepted_at_ns,
            firing.trigger_revision,
            firing.brief.as_str(),
            firing.context.as_deref(),
            firing.code_snapshot_id.as_deref(),
        ),
        ("t1", 50, 60, 1, "look at AAPL", None, None)
    );
    // Desk scoping is the row's own, not the caller's promise.
    assert!(
        store
            .call(|c| load_firing(c, "d2", "f1"))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .call(|c| load_firing(c, "d1", "f9"))
            .unwrap()
            .is_none()
    );

    let payload = |id: String| {
        store
            .call(move |c| {
                c.query_row(
                    "SELECT kind, state, payload, created_at_ns FROM prompts WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(2)?)
                                .unwrap(),
                            r.get::<_, i64>(3)?,
                        ))
                    },
                )
            })
            .unwrap()
    };

    // A code-free firing: `execution` is null.
    let free = firing.clone();
    let id = store
        .unit(move |tx| insert_result_prompt(tx, &free, "nightly", None, 70))
        .unwrap();
    let (kind, state, value, created) = payload(id);
    assert_eq!(
        (kind.as_str(), state.as_str(), created),
        ("TRIGGER_RESULT", "QUEUED", 70)
    );
    assert_eq!(
        value,
        json!({
            "kind": "TRIGGER_RESULT",
            "trigger_id": "t1", "trigger_name": "nightly",
            "firing_id": "f1", "occurrence_ns": 50, "accepted_at_ns": 60,
            "brief": "look at AAPL", "context": null,
            "execution": null,
        })
    );

    // A completed run: the summary object, field for field.
    let summary = ExecutionSummary {
        outcome: "EXITED".into(),
        exit_code: Some(0),
        error: None,
        stdout_bytes: 12,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: true,
        started_at_ns: 80,
        finished_at_ns: 90,
    };
    let id = store
        .unit(move |tx| insert_result_prompt(tx, &firing, "nightly", Some(&summary), 100))
        .unwrap();
    assert_eq!(
        payload(id).2["execution"],
        json!({
            "outcome": "EXITED", "exit_code": 0, "error": null,
            "stdout_bytes": 12, "stderr_bytes": 0,
            "stdout_truncated": false, "stderr_truncated": true,
            "started_at_ns": 80, "finished_at_ns": 90,
        })
    );
}
