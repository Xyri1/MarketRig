//! Trigger rows shared across the scheduler, the executor, and the routes: the
//! firing a run is about, and the `TRIGGER_RESULT` prompt its result queues.
//!
//! Contract: `sdd/features/r2-scheduled-triggers/SPEC.md` §5, §7, per R2-5.

use std::fmt;

use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::desk::{self, DeskError};
use crate::policy::{self, DecideError, Decision};
use crate::schedule::Schedule;
use crate::store::{Store, StoreError};

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
    /// The caller's request identity on an invoked firing, `None` on a
    /// scheduled one (`event-triggers` §2.1).
    pub request_id: Option<String>,
    /// The invocation's raw input, stored verbatim and never parsed (§2.1).
    pub input: Option<String>,
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
         brief, context, code_snapshot_id, request_id, input \
         FROM firings WHERE desk_id = ?1 AND id = ?2",
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
                request_id: r.get(9)?,
                input: r.get(10)?,
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
        "invocation": prompt_invocation(firing),
        "execution": execution,
    });
    tx.execute(
        "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
         VALUES (?1, ?2, 'TRIGGER_RESULT', 'QUEUED', ?3, ?4)",
        params![id, firing.desk_id, payload.to_string(), now_ns],
    )?;
    Ok(id)
}

/// The `TRIGGER_RESULT` payload's `invocation` (`event-triggers` §3): the
/// request identity, the input's size, and the input itself only while it fits
/// the brief's own bound, so a prompt stays one bounded text. A scheduled firing
/// carries a literal `null`, as `execution` does on a code-free one.
fn prompt_invocation(firing: &FiringRow) -> Value {
    let Some(request_id) = firing.request_id.as_deref() else {
        return Value::Null;
    };
    let mut invocation = serde_json::Map::new();
    invocation.insert("request_id".to_string(), json!(request_id));
    invocation.insert(
        "input_bytes".to_string(),
        json!(firing.input.as_deref().map_or(0, str::len)),
    );
    if let Some(input) = firing.input.as_deref().filter(|i| i.len() <= INPUT_INLINE) {
        invocation.insert("input".to_string(), json!(input));
    }
    Value::Object(invocation)
}

// ---------------------------------------------------------------------------
// Code snapshots (§4.1, per R2-3)
// ---------------------------------------------------------------------------

/// §4.1 bounds. `source` is UTF-8 by the time it is JSON, so the byte count is
/// the only thing left to check.
const SOURCE_MAX: usize = 262_144;
const ARGV_MAX: usize = 64;
const ARG_MAX: usize = 4_096;
const BRIEF_MAX: usize = 16_384;
const CONTEXT_MAX: usize = 65_536;
/// The invocation's own bounds (`event-triggers` §2.1, §3): the request
/// identity, the input, and how much of the input a result prompt carries
/// inline — the brief's bound, so the prompt stays one bounded text.
const REQUEST_ID_MAX: usize = 128;
const INPUT_MAX: usize = 262_144;
const INPUT_INLINE: usize = BRIEF_MAX;
const TIMEOUT_DEFAULT: i64 = 300;
/// The whole argument the script's absolute path replaces at spawn (§4.3).
const SCRIPT: &str = "{script}";

/// The snapshot's identity (§4.1): lowercase hex SHA-256 over `source`, `\0`,
/// `suffix`, `\0`, the argv as a JSON array, `\0`, and the timeout in decimal.
pub fn fingerprint(source: &str, suffix: &str, argv: &[String], timeout_secs: i64) -> String {
    let argv = serde_json::to_string(argv).unwrap_or_default();
    let mut bytes = Vec::with_capacity(source.len() + suffix.len() + argv.len() + 16);
    for part in [source.as_bytes(), suffix.as_bytes(), argv.as_bytes()] {
        bytes.extend_from_slice(part);
        bytes.push(0);
    }
    bytes.extend_from_slice(timeout_secs.to_string().as_bytes());
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A validated `code` object, on its way to an immutable `code_snapshots` row.
struct Snapshot {
    source: String,
    suffix: String,
    argv: Vec<String>,
    timeout_secs: i64,
}

impl Snapshot {
    /// Every §4.1 bound. `Err` is the English clause `TRIGGER_INVALID` reports.
    fn parse(value: &Value) -> Result<Snapshot, String> {
        let object = value
            .as_object()
            .ok_or("`code` must be a JSON object.".to_string())?;
        let source = object
            .get("source")
            .and_then(Value::as_str)
            .ok_or("`code.source` must be a string.".to_string())?
            .to_string();
        if source.is_empty() || source.len() > SOURCE_MAX {
            return Err(format!("`code.source` must be 1-{SOURCE_MAX} bytes."));
        }
        let suffix = match object.get("suffix") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(suffix)) => suffix.clone(),
            Some(_) => return Err("`code.suffix` must be a string.".to_string()),
        };
        let alphanumeric = suffix.len() > 1
            && suffix.starts_with('.')
            && suffix.len() <= 16
            && suffix[1..].bytes().all(|b| b.is_ascii_alphanumeric());
        if !suffix.is_empty() && !alphanumeric {
            return Err("`code.suffix` is empty, or `.` and 1-15 letters or digits.".to_string());
        }
        let argv = match object.get("argv") {
            None | Some(Value::Null) => vec![SCRIPT.to_string()],
            Some(Value::Array(argv)) => argv
                .iter()
                .map(|a| {
                    a.as_str()
                        .map(str::to_string)
                        .ok_or("`code.argv` must be an array of strings.".to_string())
                })
                .collect::<Result<Vec<String>, String>>()?,
            Some(_) => return Err("`code.argv` must be an array of strings.".to_string()),
        };
        if argv.is_empty() || argv.len() > ARGV_MAX {
            return Err(format!("`code.argv` must be 1-{ARGV_MAX} arguments."));
        }
        if argv.iter().any(|a| a.is_empty() || a.len() > ARG_MAX) {
            return Err(format!(
                "every `code.argv` argument must be 1-{ARG_MAX} bytes."
            ));
        }
        if argv.iter().filter(|a| a.as_str() == SCRIPT).count() != 1 {
            return Err(format!(
                "exactly one `code.argv` argument must be `{SCRIPT}`."
            ));
        }
        let timeout_secs = match object.get("timeout_secs") {
            None | Some(Value::Null) => TIMEOUT_DEFAULT,
            Some(value) => value
                .as_i64()
                .ok_or("`code.timeout_secs` must be an integer.".to_string())?,
        };
        if !(1..=3_600).contains(&timeout_secs) {
            return Err("`code.timeout_secs` must be 1-3600.".to_string());
        }
        Ok(Snapshot {
            source,
            suffix,
            argv,
            timeout_secs,
        })
    }

    /// One immutable row (§7), gated by the installation's trigger-code policy
    /// read in this same unit (R5 feature SPEC §3.2): **Always allow** decides
    /// it on the spot, **Require approval** leaves it `PENDING` with no
    /// decision instant and asks for one. Returns the row's id and its state.
    fn insert(
        &self,
        tx: &Transaction<'_>,
        desk_id: &str,
        trigger_id: &str,
        now_ns: i64,
    ) -> rusqlite::Result<(String, String)> {
        let pending = policy::read(tx)?.trigger_code_policy == policy::REQUIRE_APPROVAL;
        let (approval, decided_at_ns) = policy::stamp(pending, now_ns);
        let id = Uuid::now_v7().to_string();
        tx.execute(
            "INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, timeout_secs, \
             fingerprint, approval, decided_at_ns, created_at_ns) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                desk_id,
                self.source,
                self.suffix,
                serde_json::to_string(&self.argv).unwrap_or_default(),
                self.timeout_secs,
                fingerprint(&self.source, &self.suffix, &self.argv, self.timeout_secs),
                approval,
                decided_at_ns,
                now_ns,
            ],
        )?;
        if pending {
            desk::append_event(
                tx,
                "APPROVAL_REQUESTED",
                Some(desk_id),
                now_ns,
                json!({ "kind": "TRIGGER_CODE", "id": id, "trigger_id": trigger_id }),
            )?;
        }
        Ok((id, approval.to_string()))
    }
}

// ---------------------------------------------------------------------------
// The projection rule (R5 feature SPEC §3.2, per R5-3)
// ---------------------------------------------------------------------------

/// The one place the projection rule lives: a trigger has a next occurrence
/// only while it is enabled, undeleted, and its code snapshot — when it has one
/// — is decided in its favour. `candidate` is R2's scan (§2), taken lazily so a
/// site that already holds the scan's answer passes it and one that does not is
/// spared running it. Every site that computes a projection goes through here:
/// create, patch (enable and disable among them), the decision below, and the
/// scheduler's advance. Delete writes its `NULL` directly, which is this rule
/// with nothing left to ask.
pub fn projection(
    enabled: bool,
    approval: Option<&str>,
    candidate: impl FnOnce() -> Option<i64>,
) -> Option<i64> {
    match approval {
        Some("PENDING" | "DENIED") => None,
        _ => enabled.then(candidate).flatten(),
    }
}

/// The trigger-code half of `POST /desks/{d}/approvals/{id}` (§3.2): the
/// decision, its instant, the owning trigger's projection recomputed from that
/// instant, and `APPROVAL_DECIDED`, in one unit. The route wakes the scheduler
/// after this returns; nothing here does.
pub fn decide(
    store: &Store,
    desk_id: &str,
    snapshot_id: &str,
    decision: Decision,
    now_ns: i64,
) -> Result<(), DecideError> {
    let (desk, snapshot) = (desk_id.to_string(), snapshot_id.to_string());
    store.unit(move |tx| {
        let Some(approval) = tx
            .query_row(
                "SELECT approval FROM code_snapshots WHERE desk_id = ?1 AND id = ?2",
                params![desk, snapshot],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(Err(DecideError::NotFound(snapshot)));
        };
        if approval != policy::PENDING {
            return Ok(Err(DecideError::AlreadyDecided { approval }));
        }
        let decided = decision.decided();
        tx.execute(
            "UPDATE code_snapshots SET approval = ?3, decided_at_ns = ?4 \
             WHERE desk_id = ?1 AND id = ?2",
            params![desk, snapshot, decided, now_ns],
        )?;
        // The owning trigger — at most one, since every attachment inserts its
        // own row — projects again from the decision instant, so an elapsed
        // one-off stays NULL and a recurring one skips the boundaries it spent
        // waiting (§3.2). A denial projects nothing, which it already had.
        let owner = tx
            .query_row(
                "SELECT id, at_ns, rrule, dtstart, tz, enabled, deleted_at_ns \
                 FROM triggers WHERE code_snapshot_id = ?1",
                params![snapshot],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Schedule::from_row(r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
                        r.get::<_, i64>(5)? == 1 && r.get::<_, Option<i64>>(6)?.is_none(),
                    ))
                },
            )
            .optional()?;
        if let Some((trigger_id, schedule, due)) = owner {
            let next = projection(due, Some(decided), || {
                schedule.and_then(|s| s.next_after(now_ns))
            });
            tx.execute(
                "UPDATE triggers SET next_occurrence_ns = ?2 WHERE id = ?1",
                params![trigger_id, next],
            )?;
        }
        desk::append_event(
            tx,
            "APPROVAL_DECIDED",
            Some(&desk),
            now_ns,
            json!({ "kind": "TRIGGER_CODE", "id": snapshot, "decision": decision.as_str() }),
        )?;
        Ok(Ok(()))
    })?
}

// ---------------------------------------------------------------------------
// Failures (§8)
// ---------------------------------------------------------------------------

/// A trigger-surface failure carrying a stable code the REST layer maps to the
/// R0 envelope (§8). `Desk` passes R0's own codes through unchanged.
#[derive(Debug)]
pub enum TriggerError {
    /// Any §2 or §4.1 form failure, and an unusable request body.
    Invalid(String),
    NameTaken(String),
    NotFound(String),
    FiringNotFound(String),
    PromptNotFound(String),
    /// Creation needs a `READY` desk (§8).
    NotReady(String),
    /// An invocation's own form failure (`event-triggers` §2.1).
    InvocationInvalid(String),
    /// The target is disabled; a consumed one-off names the firing that
    /// consumed it (`event-triggers` §2.2).
    Disabled {
        consumed_by: Option<String>,
    },
    /// The target's code snapshot is `PENDING` or `DENIED` (§2.2).
    Unapproved,
    /// A scheduled one-off whose instant has passed: the schedule owns it (§2.2).
    Elapsed,
    Desk(DeskError),
}

impl TriggerError {
    pub fn code(&self) -> &'static str {
        match self {
            TriggerError::Invalid(_) => "TRIGGER_INVALID",
            TriggerError::NameTaken(_) => "TRIGGER_NAME_TAKEN",
            TriggerError::NotFound(_) => "TRIGGER_NOT_FOUND",
            TriggerError::FiringNotFound(_) => "FIRING_NOT_FOUND",
            TriggerError::PromptNotFound(_) => "PROMPT_NOT_FOUND",
            TriggerError::NotReady(_) => "DESK_NOT_READY",
            TriggerError::InvocationInvalid(_) => "INVOCATION_INVALID",
            TriggerError::Disabled { .. } => "TRIGGER_DISABLED",
            TriggerError::Unapproved => "TRIGGER_UNAPPROVED",
            TriggerError::Elapsed => "TRIGGER_ELAPSED",
            TriggerError::Desk(e) => e.code(),
        }
    }
}

impl fmt::Display for TriggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Every clause is written to end the envelope's sentence.
            TriggerError::Invalid(what) => write!(f, "The trigger is not well formed: {what}"),
            TriggerError::NameTaken(name) => {
                write!(f, "This desk already has a trigger named {name:?}.")
            }
            TriggerError::NotFound(id) => write!(f, "This desk has no trigger {id:?}."),
            TriggerError::FiringNotFound(id) => write!(f, "This desk has no firing {id:?}."),
            TriggerError::PromptNotFound(id) => write!(f, "This desk has no prompt {id:?}."),
            TriggerError::NotReady(state) => write!(
                f,
                "Only a READY desk can define triggers; this desk is {state}."
            ),
            TriggerError::InvocationInvalid(what) => {
                write!(f, "The invocation is not well formed: {what}")
            }
            TriggerError::Disabled { consumed_by: None } => {
                write!(f, "This trigger is disabled.")
            }
            TriggerError::Disabled {
                consumed_by: Some(firing),
            } => write!(f, "This trigger is consumed by firing {firing}."),
            TriggerError::Unapproved => write!(
                f,
                "This trigger's code is not approved, so it cannot be invoked."
            ),
            TriggerError::Elapsed => write!(
                f,
                "This one-off trigger's scheduled instant has passed; reschedule it \
                 or detach its schedule."
            ),
            TriggerError::Desk(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TriggerError {}

impl From<DeskError> for TriggerError {
    fn from(e: DeskError) -> Self {
        TriggerError::Desk(e)
    }
}

impl From<StoreError> for TriggerError {
    fn from(e: StoreError) -> Self {
        TriggerError::Desk(DeskError::Store(e))
    }
}

// ---------------------------------------------------------------------------
// Resources (§8)
// ---------------------------------------------------------------------------

/// R0's resource convention: a nullable field is omitted when it is null.
/// Never applied to a stored prompt payload, which is answered verbatim.
fn compact(value: Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, compact(v)))
                .collect(),
        ),
        other => other,
    }
}

/// The Trigger columns; the source text itself is selected only for the single
/// read, so a listing never materializes every snapshot.
fn trigger_select(with_source: bool) -> String {
    format!(
        "SELECT t.id, t.desk_id, t.name, t.recurrence, t.brief, t.context, \
         t.at_ns, t.rrule, t.dtstart, t.tz, t.enabled, t.revision, t.next_occurrence_ns, \
         t.created_at_ns, t.updated_at_ns, t.deleted_at_ns, c.id, {}, c.suffix, c.argv, \
         c.timeout_secs, c.fingerprint, c.decided_at_ns, length(CAST(c.source AS BLOB)), \
         c.approval \
         FROM triggers t LEFT JOIN code_snapshots c ON c.id = t.code_snapshot_id",
        if with_source { "c.source" } else { "NULL" }
    )
}

/// The §8 Trigger. `with_source` is the single read; the listing reports the
/// snapshot's size instead (§4.1).
fn trigger_resource(r: &Row<'_>, with_source: bool) -> rusqlite::Result<Value> {
    let schedule = Schedule::from_row(r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?);
    let code = match r.get::<_, Option<String>>(16)? {
        None => Value::Null,
        Some(snapshot_id) => {
            let source: Option<String> = r.get(17)?;
            let argv: String = r.get(19)?;
            let decided_at_ns: Option<i64> = r.get(22)?;
            let approval: String = r.get(24)?;
            json!({
                "snapshot_id": snapshot_id,
                "suffix": r.get::<_, String>(18)?,
                "argv": serde_json::from_str::<Value>(&argv).unwrap_or(Value::Null),
                "timeout_secs": r.get::<_, i64>(20)?,
                "fingerprint": r.get::<_, String>(21)?,
                // R2's field, now derived: the decision instant while the
                // snapshot is allowed, absent while it is PENDING or DENIED
                // (R5 feature SPEC §3.2).
                "approved_at_ns": matches!(approval.as_str(), "ALWAYS_ALLOW" | "APPROVED")
                    .then_some(decided_at_ns)
                    .flatten(),
                "approval": approval,
                "decided_at_ns": decided_at_ns,
                "source_bytes": r.get::<_, i64>(23)?,
                "source": with_source.then_some(source).flatten(),
            })
        }
    };
    Ok(compact(json!({
        "id": r.get::<_, String>(0)?,
        "desk_id": r.get::<_, String>(1)?,
        "name": r.get::<_, String>(2)?,
        "recurrence": r.get::<_, String>(3)?,
        "brief": r.get::<_, String>(4)?,
        "context": r.get::<_, Option<String>>(5)?,
        // Omitted when the trigger has no schedule (`event-triggers` §1).
        "schedule": schedule.map(|s| s.to_json()),
        "enabled": r.get::<_, i64>(10)? == 1,
        "revision": r.get::<_, i64>(11)?,
        "next_occurrence_ns": r.get::<_, Option<i64>>(12)?,
        "code": code,
        "created_at_ns": r.get::<_, i64>(13)?,
        "updated_at_ns": r.get::<_, i64>(14)?,
        "deleted_at_ns": r.get::<_, Option<i64>>(15)?,
    })))
}

/// One Trigger by id, deleted included; `None` when this desk has no such row.
fn read_trigger(
    conn: &Connection,
    desk_id: &str,
    trigger_id: &str,
    with_source: bool,
) -> rusqlite::Result<Option<Value>> {
    conn.query_row(
        &format!(
            "{} WHERE t.desk_id = ?1 AND t.id = ?2",
            trigger_select(with_source)
        ),
        params![desk_id, trigger_id],
        |r| trigger_resource(r, with_source),
    )
    .optional()
}

/// The Firing columns; the captured streams are selected only for the single
/// read, so a listing never materializes every execution's output.
fn firing_select(with_streams: bool) -> String {
    format!(
        "SELECT f.id, f.desk_id, f.trigger_id, f.occurrence_ns, \
         f.accepted_at_ns, f.trigger_revision, f.brief, f.context, f.code_snapshot_id, \
         e.state, e.daemon_uuid, e.outcome, e.exit_code, e.error, e.executable, {}, \
         e.stdout_truncated, e.stderr_truncated, e.started_at_ns, e.finished_at_ns, \
         length(e.stdout), length(e.stderr), f.request_id, \
         length(CAST(f.input AS BLOB)), {} \
         FROM firings f LEFT JOIN executions e ON e.firing_id = f.id",
        if with_streams {
            "e.stdout, e.stderr"
        } else {
            "NULL, NULL"
        },
        // The input rides with the streams: the single read carries it, the
        // per-trigger listing reports its size alone (`event-triggers` §2.4).
        if with_streams { "f.input" } else { "NULL" }
    )
}

/// The §8 Firing. `with_streams` is the single read; the per-trigger listing
/// carries the same execution object without `stdout` and `stderr`.
fn firing_resource(r: &Row<'_>, with_streams: bool) -> rusqlite::Result<Value> {
    let execution = match r.get::<_, Option<String>>(9)? {
        None => Value::Null,
        Some(state) => {
            // The captured bytes, exactly as produced (per R2-5); JSON carries
            // them lossily with the true byte counts beside them.
            let stdout: Option<Vec<u8>> = r.get(15)?;
            let stderr: Option<Vec<u8>> = r.get(16)?;
            let lossy = |bytes: Option<Vec<u8>>| {
                String::from_utf8_lossy(&bytes.unwrap_or_default()).into_owned()
            };
            json!({
                "state": state,
                "daemon_uuid": r.get::<_, String>(10)?,
                "outcome": r.get::<_, Option<String>>(11)?,
                "exit_code": r.get::<_, Option<i64>>(12)?,
                "error": r.get::<_, Option<String>>(13)?,
                "executable": r.get::<_, Option<String>>(14)?,
                "stdout": with_streams.then(|| lossy(stdout)),
                "stderr": with_streams.then(|| lossy(stderr)),
                "stdout_bytes": r.get::<_, Option<i64>>(21)?.unwrap_or(0),
                "stderr_bytes": r.get::<_, Option<i64>>(22)?.unwrap_or(0),
                "stdout_truncated": r.get::<_, Option<i64>>(17)?.map(|v| v == 1),
                "stderr_truncated": r.get::<_, Option<i64>>(18)?.map(|v| v == 1),
                "started_at_ns": r.get::<_, i64>(19)?,
                "finished_at_ns": r.get::<_, Option<i64>>(20)?,
            })
        }
    };
    Ok(compact(json!({
        "id": r.get::<_, String>(0)?,
        "desk_id": r.get::<_, String>(1)?,
        "trigger_id": r.get::<_, String>(2)?,
        "occurrence_ns": r.get::<_, i64>(3)?,
        "accepted_at_ns": r.get::<_, i64>(4)?,
        "trigger_revision": r.get::<_, i64>(5)?,
        "brief": r.get::<_, String>(6)?,
        "context": r.get::<_, Option<String>>(7)?,
        "code_snapshot_id": r.get::<_, Option<String>>(8)?,
        // Absent on a scheduled firing, which has no request identity and no
        // input (`event-triggers` §2.4).
        "request_id": r.get::<_, Option<String>>(23)?,
        "input_bytes": r.get::<_, Option<i64>>(24)?,
        "input": r.get::<_, Option<String>>(25)?,
        "execution": execution,
    })))
}

// ---------------------------------------------------------------------------
// Definitions (§8)
// ---------------------------------------------------------------------------

/// The body of a create or patch, validated. `Err` is the clause the envelope
/// carries.
fn body_object(body: &str) -> Result<serde_json::Map<String, Value>, TriggerError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| TriggerError::Invalid(format!("the request body is not JSON: {e}.")))?;
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(TriggerError::Invalid(
            "the request body must be a JSON object.".to_string(),
        )),
    }
}

fn brief_of(value: &Value) -> Result<String, String> {
    let brief = value
        .as_str()
        .ok_or("`brief` must be a string.".to_string())?;
    if brief.is_empty() || brief.len() > BRIEF_MAX {
        return Err(format!("`brief` must be 1-{BRIEF_MAX} bytes."));
    }
    Ok(brief.to_string())
}

fn context_of(value: &Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(context) if context.len() <= CONTEXT_MAX => Ok(Some(context.clone())),
        Value::String(_) => Err(format!("`context` must be at most {CONTEXT_MAX} bytes.")),
        _ => Err("`context` must be a string or null.".to_string()),
    }
}

/// The schedule's four columns (§7); the row's CHECKs want at most one shape,
/// and a trigger with no schedule carries none of them (`event-triggers` §4).
fn schedule_columns(
    schedule: Option<&Schedule>,
) -> (Option<i64>, Option<String>, Option<String>, Option<String>) {
    match schedule {
        None => (None, None, None, None),
        Some(Schedule::OneOff { at_ns }) => (Some(*at_ns), None, None, None),
        Some(Schedule::Recurring { rrule, dtstart, tz }) => (
            None,
            Some(rrule.clone()),
            Some(dtstart.clone()),
            Some(tz.clone()),
        ),
    }
}

/// `POST /desks/{desk_id}/triggers` (§8): one trigger, its optional snapshot,
/// and its first projection, in one unit. Creation needs a `READY` desk.
pub fn create(
    store: &Store,
    desk_id: &str,
    body: &str,
    now_ns: i64,
) -> Result<Value, TriggerError> {
    let desk = desk::get(store, desk_id)?;
    if desk.state != "READY" {
        return Err(TriggerError::NotReady(desk.state));
    }
    let fields = body_object(body)?;
    let invalid = TriggerError::Invalid;
    let name = fields
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`name` must be a string.".to_string()))?
        .to_string();
    if !desk::valid_name(&name) {
        return Err(invalid(format!(
            "`name` {name:?} is not 1-40 characters of lowercase letters, digits, \
             and single interior hyphens."
        )));
    }
    let brief = fields
        .get("brief")
        .ok_or_else(|| invalid("`brief` is required.".to_string()))
        .and_then(|v| brief_of(v).map_err(invalid))?;
    let context = match fields.get("context") {
        None => None,
        Some(value) => context_of(value).map_err(invalid)?,
    };
    // Optional: a trigger with no schedule is invocable and never due
    // (`event-triggers` §1), and with none given it is RECURRING — the
    // recurrence a producer can invoke more than once.
    let schedule = match fields.get("schedule") {
        None => None,
        Some(value) => Schedule::parse(value, now_ns).map_err(invalid)?,
    };
    let recurrence = schedule.as_ref().map_or("RECURRING", Schedule::recurrence);
    let code = match fields.get("code") {
        None | Some(Value::Null) => None,
        Some(value) => Some(Snapshot::parse(value).map_err(invalid)?),
    };

    let id = Uuid::now_v7().to_string();
    let (read_id, desk_owned, taken) = (id.clone(), desk_id.to_string(), name.clone());
    // Projected before the unit: the candidate scan (§2) never holds the write
    // transaction on the create path.
    let next = schedule.as_ref().and_then(|s| s.next_after(now_ns));
    let created = store.unit(move |tx| {
        let snapshot = match &code {
            None => None,
            Some(code) => Some(code.insert(tx, &desk_owned, &id, now_ns)?),
        };
        let (snapshot_id, approval) = match &snapshot {
            None => (None, None),
            Some((id, approval)) => (Some(id.as_str()), Some(approval.as_str())),
        };
        // A fresh trigger is enabled and undeleted; only its snapshot can
        // withhold the projection (R5 feature SPEC §3.2).
        let next = projection(true, approval, || next);
        let (at_ns, rrule, dtstart, tz) = schedule_columns(schedule.as_ref());
        tx.execute(
            "INSERT INTO triggers (id, desk_id, name, recurrence, brief, context, \
             at_ns, rrule, dtstart, tz, enabled, revision, code_snapshot_id, \
             next_occurrence_ns, created_at_ns, updated_at_ns) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, 1, ?11, ?12, ?13, ?13)",
            params![
                id,
                desk_owned,
                name,
                recurrence,
                brief,
                context,
                at_ns,
                rrule,
                dtstart,
                tz,
                snapshot_id,
                next,
                now_ns,
            ],
        )?;
        read_trigger(tx, &desk_owned, &read_id, true)
    });
    match created {
        Ok(trigger) => Ok(trigger.unwrap_or(Value::Null)),
        // The desk exists and the schedule columns are built consistent, so the
        // only constraint a fresh insert can violate is the live-name index.
        Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)))
            if f.code == ErrorCode::ConstraintViolation =>
        {
            Err(TriggerError::NameTaken(taken))
        }
        Err(e) => Err(e.into()),
    }
}

/// `GET /desks/{desk_id}/triggers` (§8): undeleted, creation order, snapshots
/// without their source.
pub fn list(store: &Store, desk_id: &str) -> Result<Vec<Value>, TriggerError> {
    desk::get(store, desk_id)?;
    let desk = desk_id.to_string();
    Ok(store.call(move |conn| {
        conn.prepare(&format!(
            "{} WHERE t.desk_id = ?1 AND t.deleted_at_ns IS NULL \
             ORDER BY t.created_at_ns, t.id",
            trigger_select(false)
        ))?
        .query_map(params![desk], |r| trigger_resource(r, false))?
        .collect()
    })?)
}

/// `GET /desks/{desk_id}/triggers/{trigger_id}` (§8): one, deleted included,
/// with the snapshot's source.
pub fn get(store: &Store, desk_id: &str, trigger_id: &str) -> Result<Value, TriggerError> {
    desk::get(store, desk_id)?;
    let (desk, id) = (desk_id.to_string(), trigger_id.to_string());
    store
        .call(move |conn| read_trigger(conn, &desk, &id, true))?
        .ok_or_else(|| TriggerError::NotFound(trigger_id.to_string()))
}

/// A validated patch body: the outer `Option` is "named", the inner one is the
/// value (`context: null` clears, `code: null` detaches).
#[derive(Default)]
struct Patch {
    brief: Option<String>,
    context: Option<Option<String>>,
    /// `schedule: null` detaches one (`event-triggers` §1).
    schedule: Option<Option<Schedule>>,
    enabled: Option<bool>,
    code: Option<Option<Snapshot>>,
}

/// `PATCH /desks/{desk_id}/triggers/{trigger_id}` (§8): any subset of the five
/// fields, one revision, one recomputed projection.
pub fn patch(
    store: &Store,
    desk_id: &str,
    trigger_id: &str,
    body: &str,
    now_ns: i64,
) -> Result<Value, TriggerError> {
    desk::get(store, desk_id)?;
    let fields = body_object(body)?;
    let invalid = TriggerError::Invalid;
    let mut patch = Patch::default();
    if let Some(value) = fields.get("brief") {
        patch.brief = Some(brief_of(value).map_err(invalid)?);
    }
    if let Some(value) = fields.get("context") {
        patch.context = Some(context_of(value).map_err(invalid)?);
    }
    if let Some(value) = fields.get("schedule") {
        patch.schedule = Some(Schedule::parse(value, now_ns).map_err(invalid)?);
    }
    if let Some(value) = fields.get("enabled") {
        patch.enabled = Some(
            value
                .as_bool()
                .ok_or_else(|| invalid("`enabled` must be true or false.".to_string()))?,
        );
    }
    if let Some(value) = fields.get("code") {
        patch.code = Some(match value {
            Value::Null => None,
            value => Some(Snapshot::parse(value).map_err(invalid)?),
        });
    }
    if patch.brief.is_none()
        && patch.context.is_none()
        && patch.schedule.is_none()
        && patch.enabled.is_none()
        && patch.code.is_none()
    {
        return Err(invalid(
            "a patch names at least one of `brief`, `context`, `schedule`, `enabled`, \
             and `code`."
                .to_string(),
        ));
    }

    let (desk, id) = (desk_id.to_string(), trigger_id.to_string());
    store
        .unit(move |tx| {
            let Some(current) = tx
                .query_row(
                    "SELECT t.recurrence, t.brief, t.context, t.at_ns, t.rrule, t.dtstart, \
                     t.tz, t.enabled, t.code_snapshot_id, c.fingerprint, c.approval \
                     FROM triggers t LEFT JOIN code_snapshots c ON c.id = t.code_snapshot_id \
                     WHERE t.desk_id = ?1 AND t.id = ?2 AND t.deleted_at_ns IS NULL",
                    params![desk, id],
                    |r| {
                        Ok((
                            Schedule::from_row(r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, i64>(7)? == 1,
                            r.get::<_, Option<String>>(8)?,
                            r.get::<_, Option<String>>(9)?,
                            r.get::<_, Option<String>>(10)?,
                        ))
                    },
                )
                .optional()?
            else {
                return Ok(None);
            };
            let (
                row_schedule,
                row_recurrence,
                row_brief,
                row_context,
                row_enabled,
                row_snapshot,
                row_fingerprint,
                row_approval,
            ) = current;
            let schedule = patch.schedule.unwrap_or(row_schedule);
            // Attaching a schedule names the recurrence; detaching one keeps it
            // (`event-triggers` §1).
            let recurrence = schedule
                .as_ref()
                .map_or(row_recurrence, |s| s.recurrence().to_string());
            let enabled = patch.enabled.unwrap_or(row_enabled);
            let same_code = |code: &Snapshot| {
                row_fingerprint.as_deref()
                    == Some(
                        fingerprint(&code.source, &code.suffix, &code.argv, code.timeout_secs)
                            .as_str(),
                    )
            };
            let (snapshot_id, approval) = match &patch.code {
                None => (row_snapshot, row_approval),
                // Code that fingerprints identical to the current snapshot is
                // the same code: the row and its decision are kept, because
                // only a change needs approving again (root §8.3, R5 feature
                // SPEC §3.2).
                Some(Some(code)) if same_code(code) => (row_snapshot, row_approval),
                Some(None) => (None, None),
                Some(Some(code)) => {
                    let (snapshot_id, approval) = code.insert(tx, &desk, &id, now_ns)?;
                    (Some(snapshot_id), Some(approval))
                }
            };
            let (at_ns, rrule, dtstart, tz) = schedule_columns(schedule.as_ref());
            // Disabled, undecided, or schedule-less is never due; otherwise the
            // projection is recomputed from the definition's own anchor against
            // now (§2).
            // ponytail: the scan runs inside the unit because the row's schedule
            // is read here; it is bounded at 100,000 candidates (R2-1's ceiling,
            // a persisted cursor is the upgrade).
            let next = projection(enabled, approval.as_deref(), || {
                schedule.as_ref().and_then(|s| s.next_after(now_ns))
            });
            tx.execute(
                "UPDATE triggers SET brief = ?3, context = ?4, recurrence = ?5, at_ns = ?6, \
                 rrule = ?7, dtstart = ?8, tz = ?9, enabled = ?10, code_snapshot_id = ?11, \
                 next_occurrence_ns = ?12, revision = revision + 1, updated_at_ns = ?13 \
                 WHERE desk_id = ?1 AND id = ?2",
                params![
                    desk,
                    id,
                    patch.brief.unwrap_or(row_brief),
                    patch.context.unwrap_or(row_context),
                    recurrence,
                    at_ns,
                    rrule,
                    dtstart,
                    tz,
                    enabled as i64,
                    snapshot_id,
                    next,
                    now_ns,
                ],
            )?;
            read_trigger(tx, &desk, &id, true)
        })?
        .ok_or_else(|| TriggerError::NotFound(trigger_id.to_string()))
}

/// `DELETE /desks/{desk_id}/triggers/{trigger_id}` (§8): soft, so the firings
/// that reference it stay readable (per R2-7). A second delete is not found.
pub fn delete(
    store: &Store,
    desk_id: &str,
    trigger_id: &str,
    now_ns: i64,
) -> Result<Value, TriggerError> {
    desk::get(store, desk_id)?;
    let (desk, id) = (desk_id.to_string(), trigger_id.to_string());
    store
        .unit(move |tx| {
            let deleted = tx.execute(
                "UPDATE triggers SET deleted_at_ns = ?3, next_occurrence_ns = NULL \
                 WHERE desk_id = ?1 AND id = ?2 AND deleted_at_ns IS NULL",
                params![desk, id, now_ns],
            )?;
            if deleted == 0 {
                return Ok(None);
            }
            read_trigger(tx, &desk, &id, true)
        })?
        .ok_or_else(|| TriggerError::NotFound(trigger_id.to_string()))
}

// ---------------------------------------------------------------------------
// Invocation (`event-triggers` §2)
// ---------------------------------------------------------------------------

/// What one accepted or replayed invocation was (`event-triggers` §2.4). The
/// route turns `accepted` into `201 ACCEPTED` or `200 DUPLICATE` and wakes the
/// executor or the dispatcher exactly as the scheduler's pass does.
#[derive(Debug)]
pub struct Invocation {
    pub accepted: bool,
    pub has_code: bool,
    pub firing: Value,
}

/// §2.1's request form. `Err` is the clause `INVOCATION_INVALID` carries;
/// nothing here touches the database, so a malformed request writes nothing.
fn invocation_form(body: &str) -> Result<(String, Option<String>), TriggerError> {
    let invalid = TriggerError::InvocationInvalid;
    let value: Value = serde_json::from_str(body)
        .map_err(|e| invalid(format!("the request body is not JSON: {e}.")))?;
    let fields = value
        .as_object()
        .ok_or_else(|| invalid("the request body must be a JSON object.".to_string()))?;
    let request_id = fields
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`request_id` must be a string.".to_string()))?
        .to_string();
    if request_id.is_empty() || request_id.len() > REQUEST_ID_MAX {
        return Err(invalid(format!(
            "`request_id` must be 1-{REQUEST_ID_MAX} bytes."
        )));
    }
    if !request_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(invalid(
            "`request_id` is letters, digits, and `. _ : -`.".to_string(),
        ));
    }
    let input = match fields.get("input") {
        None | Some(Value::Null) => None,
        Some(Value::String(input)) if input.len() <= INPUT_MAX => Some(input.clone()),
        Some(Value::String(_)) => {
            return Err(invalid(format!(
                "`input` must be at most {INPUT_MAX} bytes."
            )));
        }
        Some(_) => return Err(invalid("`input` must be a string or null.".to_string())),
    };
    Ok((request_id, input))
}

/// `POST /desks/{desk_id}/triggers/{trigger_id}/invocations` (§2.2): duplicate,
/// target, eligibility, firing, advance — one `BEGIN IMMEDIATE` unit with
/// `now_ns` read once by the caller. A refusal writes nothing and buffers
/// nothing, and no backlog is ever read (ET-4, ET-6).
pub fn invoke(
    store: &Store,
    desk_id: &str,
    trigger_id: &str,
    body: &str,
    now_ns: i64,
) -> Result<Invocation, TriggerError> {
    desk::get(store, desk_id)?;
    let (request_id, input) = invocation_form(body)?;
    let (desk, id, not_found) = (
        desk_id.to_string(),
        trigger_id.to_string(),
        trigger_id.to_string(),
    );
    store.unit(move |tx| {
        // 1. Duplicate first: an accepted request keeps its answer whatever the
        //    trigger has become since (ET-4).
        if let Some(firing_id) = tx
            .query_row(
                "SELECT id FROM firings WHERE desk_id = ?1 AND trigger_id = ?2 \
                 AND request_id = ?3",
                params![desk, id, request_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            let firing = read_firing(tx, &desk, &firing_id)?;
            return Ok(Ok(Invocation {
                accepted: false,
                has_code: firing["code_snapshot_id"].is_string(),
                firing,
            }));
        }
        // 2. Target.
        let Some((name, recurrence, at_ns, enabled, brief, context, revision, snapshot, approval)) =
            tx.query_row(
                "SELECT t.name, t.recurrence, t.at_ns, t.enabled, t.brief, t.context, \
                 t.revision, t.code_snapshot_id, c.approval \
                 FROM triggers t LEFT JOIN code_snapshots c ON c.id = t.code_snapshot_id \
                 WHERE t.desk_id = ?1 AND t.id = ?2 AND t.deleted_at_ns IS NULL",
                params![desk, id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, i64>(3)? == 1,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(Err(TriggerError::NotFound(not_found)));
        };
        let one_off = recurrence == "ONE_OFF";
        // The one-off's own firing, read once: it names the consuming firing in
        // a refusal, and it is what says the schedule is already spent.
        let consumed_by = match one_off {
            true => tx
                .query_row(
                    "SELECT id FROM firings WHERE desk_id = ?1 AND trigger_id = ?2 \
                     ORDER BY accepted_at_ns, id LIMIT 1",
                    params![desk, id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?,
            false => None,
        };
        // 3. Eligibility, on the scheduler's own join.
        if !enabled {
            return Ok(Err(TriggerError::Disabled { consumed_by }));
        }
        if matches!(approval.as_deref(), Some("PENDING" | "DENIED")) {
            return Ok(Err(TriggerError::Unapproved));
        }
        // ET-7's deadline: while a scheduled one-off's instant has passed and
        // the schedule has not resolved it, the schedule owns it — only an
        // explicit reschedule or detach makes it invocable again. A one-off the
        // schedule already fired is past that: re-enabling it is the explicit
        // statement that it may fire once more (§1).
        if one_off && consumed_by.is_none() && at_ns.is_some_and(|at| at <= now_ns) {
            return Ok(Err(TriggerError::Elapsed));
        }
        // 4. The firing, with the request's identity and its input.
        let firing = FiringRow {
            id: Uuid::now_v7().to_string(),
            desk_id: desk.clone(),
            trigger_id: id.clone(),
            occurrence_ns: now_ns,
            accepted_at_ns: now_ns,
            trigger_revision: revision,
            brief,
            context,
            code_snapshot_id: snapshot,
            request_id: Some(request_id.clone()),
            input,
        };
        tx.execute(
            "INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
             trigger_revision, brief, context, code_snapshot_id, request_id, input) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                firing.id,
                firing.desk_id,
                firing.trigger_id,
                firing.occurrence_ns,
                firing.accepted_at_ns,
                firing.trigger_revision,
                firing.brief,
                firing.context,
                firing.code_snapshot_id,
                firing.request_id,
                firing.input,
            ],
        )?;
        let has_code = firing.code_snapshot_id.is_some();
        if !has_code {
            insert_result_prompt(tx, &firing, &name, None, now_ns)?;
        }
        // 5. Advance: a one-off is consumed here, a recurring trigger keeps its
        //    schedule, projection, and anchor (ET-7).
        if one_off {
            tx.execute(
                "UPDATE triggers SET enabled = 0, next_occurrence_ns = NULL WHERE id = ?1",
                params![id],
            )?;
        }
        let firing = read_firing(tx, &desk, &firing.id)?;
        Ok(Ok(Invocation {
            accepted: true,
            has_code,
            firing,
        }))
    })?
}

/// The §8 Firing with its input, read back inside the unit that wrote it.
fn read_firing(conn: &Connection, desk_id: &str, firing_id: &str) -> rusqlite::Result<Value> {
    conn.query_row(
        &format!("{} WHERE f.desk_id = ?1 AND f.id = ?2", firing_select(true)),
        params![desk_id, firing_id],
        |r| firing_resource(r, true),
    )
}

// ---------------------------------------------------------------------------
// Firings and prompts (§8)
// ---------------------------------------------------------------------------

/// `GET /desks/{desk_id}/triggers/{trigger_id}/firings` (§8): newest first,
/// each with its execution summary, streams excluded.
pub fn firings(store: &Store, desk_id: &str, trigger_id: &str) -> Result<Vec<Value>, TriggerError> {
    desk::get(store, desk_id)?;
    let (desk, id) = (desk_id.to_string(), trigger_id.to_string());
    store
        .call(move |conn| {
            // A deleted trigger still answers its firings; an unknown one does not.
            if conn
                .query_row(
                    "SELECT 1 FROM triggers WHERE desk_id = ?1 AND id = ?2",
                    params![desk, id],
                    |_| Ok(()),
                )
                .optional()?
                .is_none()
            {
                return Ok(None);
            }
            let firings = conn
                .prepare(&format!(
                    "{} WHERE f.desk_id = ?1 AND f.trigger_id = ?2 \
                     ORDER BY f.accepted_at_ns DESC, f.id DESC",
                    firing_select(false)
                ))?
                .query_map(params![desk, id], |r| firing_resource(r, false))?
                .collect::<rusqlite::Result<Vec<Value>>>()?;
            Ok(Some(firings))
        })?
        .ok_or_else(|| TriggerError::NotFound(trigger_id.to_string()))
}

/// `GET /desks/{desk_id}/firings/{firing_id}` (§8): one, with the captured
/// streams rendered lossily beside their byte counts.
pub fn firing(store: &Store, desk_id: &str, firing_id: &str) -> Result<Value, TriggerError> {
    desk::get(store, desk_id)?;
    let (desk, id) = (desk_id.to_string(), firing_id.to_string());
    store
        .call(move |conn| {
            conn.query_row(
                &format!("{} WHERE f.desk_id = ?1 AND f.id = ?2", firing_select(true)),
                params![desk, id],
                |r| firing_resource(r, true),
            )
            .optional()
        })?
        .ok_or_else(|| TriggerError::FiringNotFound(firing_id.to_string()))
}

/// `GET /desks/{desk_id}/prompts` (§8, root §11.1): the desk's queue, newest
/// first, every delivery fact and no payload.
pub fn prompts(store: &Store, desk_id: &str) -> Result<Vec<Value>, TriggerError> {
    desk::get(store, desk_id)?;
    let desk = desk_id.to_string();
    Ok(store.call(move |conn| {
        conn.prepare(
            "SELECT id, desk_id, kind, state, created_at_ns, attempted_at_ns, resolved_at_ns, \
             runtime, native_session_id, failure_code FROM prompts WHERE desk_id = ?1 \
             ORDER BY created_at_ns DESC, id DESC",
        )?
        .query_map(params![desk], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "desk_id": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "state": r.get::<_, String>(3)?,
                "created_at_ns": r.get::<_, i64>(4)?,
                "attempted_at_ns": r.get::<_, Option<i64>>(5)?,
                "resolved_at_ns": r.get::<_, Option<i64>>(6)?,
                "runtime": r.get::<_, Option<String>>(7)?,
                "native_session_id": r.get::<_, Option<String>>(8)?,
                "failure_code": r.get::<_, Option<String>>(9)?,
            }))
        })?
        .collect()
    })?)
}

/// `GET /desks/{desk_id}/prompts/{prompt_id}` (§8): the same fields plus the
/// stored payload, parsed and answered verbatim.
pub fn prompt(store: &Store, desk_id: &str, prompt_id: &str) -> Result<Value, TriggerError> {
    desk::get(store, desk_id)?;
    let (desk, id) = (desk_id.to_string(), prompt_id.to_string());
    store
        .call(move |conn| {
            conn.query_row(
                "SELECT id, desk_id, kind, state, created_at_ns, payload, attempted_at_ns, \
                 resolved_at_ns, runtime, native_session_id, failure_code FROM prompts \
                 WHERE desk_id = ?1 AND id = ?2",
                params![desk, id],
                |r| {
                    let payload: String = r.get(5)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "desk_id": r.get::<_, String>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "state": r.get::<_, String>(3)?,
                        "created_at_ns": r.get::<_, i64>(4)?,
                        "attempted_at_ns": r.get::<_, Option<i64>>(6)?,
                        "resolved_at_ns": r.get::<_, Option<i64>>(7)?,
                        "runtime": r.get::<_, Option<String>>(8)?,
                        "native_session_id": r.get::<_, Option<String>>(9)?,
                        "failure_code": r.get::<_, Option<String>>(10)?,
                        "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
                    }))
                },
            )
            .optional()
        })?
        .ok_or_else(|| TriggerError::PromptNotFound(prompt_id.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The §4.1 fingerprint over a fixed snapshot, pinned here; every field feeds
/// it, and the `\0` separators keep neighbouring fields from running together.
#[cfg(test)]
#[test]
fn fingerprint_stable() {
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<String>>();
    let python = argv(&["python3", "{script}"]);
    let pinned = fingerprint("print(1)", ".py", &python, 300);
    assert_eq!(
        pinned,
        "b001ff0533ce151237eb2ba0791a9648a52260d8f46a6d3965c9df4946d06d58"
    );

    // One field at a time, each a different snapshot.
    for other in [
        fingerprint("print(2)", ".py", &python, 300),
        fingerprint("print(1)", ".sh", &python, 300),
        fingerprint("print(1)", "", &python, 300),
        fingerprint(
            "print(1)",
            ".py",
            &argv(&["python3", "-u", "{script}"]),
            300,
        ),
        fingerprint("print(1)", ".py", &argv(&["{script}"]), 300),
        fingerprint("print(1)", ".py", &python, 301),
    ] {
        assert_ne!(pinned, other);
    }
    // The separators are not ambiguous: a byte moved across a boundary is a
    // different snapshot, not the same one.
    assert_ne!(
        fingerprint("a", "b", &argv(&["{script}"]), 1),
        fingerprint("ab", "", &argv(&["{script}"]), 1)
    );
}

/// The §5 payload, round-tripped from real rows: both shapes of `execution`,
/// and `load_firing` scoped to its desk.
#[cfg(test)]
#[test]
fn result_prompt_payload() {
    let (_dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('d1','alpha','READY','/desks/alpha',1,2,NULL,NULL)",
                [],
            )?;
            tx.execute(
                "INSERT INTO triggers (id, desk_id, name, recurrence, brief, at_ns, \
                 enabled, revision, created_at_ns, updated_at_ns) \
                 VALUES ('t1','d1','nightly','ONE_OFF','look at AAPL',50,1,1,1,1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO firings VALUES \
                 ('f1','d1','t1',50,60,1,'look at AAPL',NULL,NULL,NULL,NULL)",
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
            "invocation": null,
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

// ---------------------------------------------------------------------------
// Trigger code approval (R5 feature SPEC §3.2, §8 check 2)
// ---------------------------------------------------------------------------

/// A store with one `READY` desk, on the shipped defaults — the trigger-code
/// policy is **Require approval** until a test says otherwise.
#[cfg(test)]
fn desk_store() -> (tempfile::TempDir, Store) {
    let (dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES ('d1','alpha','READY','/desks/alpha',1,1)",
                [],
            )
        })
        .unwrap();
    (dir, store)
}

#[cfg(test)]
fn set_policy(store: &Store, value: &'static str) {
    store
        .unit(move |tx| {
            tx.execute(
                "UPDATE installation_settings SET trigger_code_policy = ?1 WHERE id = 1",
                params![value],
            )
        })
        .unwrap();
}

/// The desk's operational events, oldest first.
#[cfg(test)]
fn desk_events(store: &Store) -> Vec<(String, Value)> {
    store
        .call(|c| {
            c.prepare(
                "SELECT kind, payload FROM operational_events WHERE desk_id = 'd1' \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                ))
            })?
            .collect()
        })
        .unwrap()
}

#[cfg(test)]
fn snapshot_count(store: &Store) -> i64 {
    store
        .call(|c| c.query_row("SELECT count(*) FROM code_snapshots", [], |r| r.get(0)))
        .unwrap()
}

#[cfg(test)]
fn rfc3339(at_ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(at_ns).to_rfc3339()
}

/// A one-off create body carrying code.
#[cfg(test)]
fn code_body(name: &str, at_ns: i64, source: &str) -> String {
    format!(
        r#"{{"name":"{name}","brief":"look at AAPL","schedule":{{"at":"{}"}},
             "code":{{"source":"{source}","suffix":".py"}}}}"#,
        rfc3339(at_ns)
    )
}

/// 2026-09-03T12:00:00Z, so a minute rule's boundaries are known.
#[cfg(test)]
const T0: i64 = 1_788_436_800_000_000_000;
#[cfg(test)]
const SECOND: i64 = 1_000_000_000;

/// The rule itself (§3.2): the scan runs only when the trigger is enabled and
/// its snapshot is decided in its favour.
#[cfg(test)]
#[test]
fn the_projection_rule() {
    let candidate = || Some(7);
    assert_eq!(projection(true, None, candidate), Some(7));
    assert_eq!(projection(true, Some("ALWAYS_ALLOW"), candidate), Some(7));
    assert_eq!(projection(true, Some("APPROVED"), candidate), Some(7));
    assert_eq!(projection(false, None, candidate), None);
    assert_eq!(projection(false, Some("APPROVED"), candidate), None);
    for state in ["PENDING", "DENIED"] {
        assert_eq!(
            projection(true, Some(state), || panic!("the scan never runs")),
            None
        );
    }
}

/// **Never due** (§3.2): a pending one-off is undue through disable, enable,
/// and an approval that arrives after its instant; only a reschedule arms it.
#[cfg(test)]
#[test]
fn a_pending_trigger_is_never_due() {
    let (_dir, store) = desk_store();
    let created = create(
        &store,
        "d1",
        &code_body("nightly", T0 + 2 * SECOND, "print(1)"),
        T0,
    )
    .expect("the create is well formed");
    let id = created["id"].as_str().unwrap().to_string();
    let snapshot = created["code"]["snapshot_id"].as_str().unwrap().to_string();
    assert_eq!(created["code"]["approval"], "PENDING");
    assert!(
        created["code"]["decided_at_ns"].is_null() && created["code"]["approved_at_ns"].is_null(),
        "an undecided snapshot has no instant: {created}"
    );
    assert!(
        created["next_occurrence_ns"].is_null(),
        "pending is never due: {created}"
    );
    assert_eq!(
        desk_events(&store),
        vec![(
            "APPROVAL_REQUESTED".to_string(),
            json!({ "kind": "TRIGGER_CODE", "id": snapshot, "trigger_id": id })
        )]
    );

    // Disable and enable are patches; the rule outranks both.
    for body in [r#"{"enabled":false}"#, r#"{"enabled":true}"#] {
        let patched = patch(&store, "d1", &id, body, T0 + SECOND).unwrap();
        assert_eq!(patched["code"]["approval"], "PENDING");
        assert!(patched["next_occurrence_ns"].is_null(), "{body}: {patched}");
    }

    // Approved after its instant: decided, and still never due.
    let decided = T0 + 5 * SECOND;
    decide(&store, "d1", &snapshot, Decision::Approve, decided).unwrap();
    let after = get(&store, "d1", &id).unwrap();
    assert_eq!(after["code"]["approval"], "APPROVED");
    assert_eq!(after["code"]["decided_at_ns"], decided);
    assert_eq!(after["code"]["approved_at_ns"], decided);
    assert!(
        after["next_occurrence_ns"].is_null(),
        "an elapsed one-off stays NULL: {after}"
    );

    // A reschedule is what arms it, and the approved snapshot is kept.
    let armed = decided + 2 * SECOND;
    let moved = patch(
        &store,
        "d1",
        &id,
        &format!(r#"{{"schedule":{{"at":"{}"}}}}"#, rfc3339(armed)),
        decided,
    )
    .unwrap();
    assert_eq!(moved["next_occurrence_ns"], armed);
    assert_eq!(moved["code"]["snapshot_id"], snapshot.as_str());
    assert_eq!(moved["code"]["approval"], "APPROVED");
    assert_eq!(snapshot_count(&store), 1);
}

/// **Recurring approved** (§3.2): the next boundary after the decision, not
/// after creation.
#[cfg(test)]
#[test]
fn approval_projects_a_recurring_trigger_from_the_decision() {
    let (_dir, store) = desk_store();
    let body = r#"{"name":"tick","brief":"watch","schedule":
        {"rrule":"FREQ=MINUTELY","dtstart":"2026-09-03T12:00:00","tz":"UTC"},
        "code":{"source":"print(1)","suffix":".py"}}"#;
    let created = create(&store, "d1", body, T0).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let snapshot = created["code"]["snapshot_id"].as_str().unwrap().to_string();
    assert!(created["next_occurrence_ns"].is_null(), "{created}");

    // 12:01:30 — the boundaries at 12:00 and 12:01 are spent waiting.
    let decided = T0 + 90 * SECOND;
    decide(&store, "d1", &snapshot, Decision::Approve, decided).unwrap();
    let after = get(&store, "d1", &id).unwrap();
    assert_eq!(after["next_occurrence_ns"], T0 + 120 * SECOND);
}

/// **Denied leaves nothing** (§3.2): the trigger keeps the denied snapshot and
/// stays undue; different code asks again.
#[cfg(test)]
#[test]
fn a_denied_snapshot_stays_and_new_code_asks_again() {
    let (_dir, store) = desk_store();
    let created = create(
        &store,
        "d1",
        &code_body("nightly", T0 + 3_600 * SECOND, "print(1)"),
        T0,
    )
    .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let snapshot = created["code"]["snapshot_id"].as_str().unwrap().to_string();

    let decided = T0 + SECOND;
    decide(&store, "d1", &snapshot, Decision::Deny, decided).unwrap();
    let after = get(&store, "d1", &id).unwrap();
    assert_eq!(after["code"]["approval"], "DENIED");
    assert_eq!(after["code"]["decided_at_ns"], decided);
    assert!(
        after["code"]["approved_at_ns"].is_null(),
        "denial derives no approval instant: {after}"
    );
    assert!(
        after["next_occurrence_ns"].is_null(),
        "denied is never due, though its instant is ahead: {after}"
    );
    assert_eq!(
        desk_events(&store)[1],
        (
            "APPROVAL_DECIDED".to_string(),
            json!({ "kind": "TRIGGER_CODE", "id": snapshot, "decision": "DENY" })
        )
    );

    // Different code: a second snapshot, pending, and a second request. The
    // denied row stays.
    let repatched = patch(
        &store,
        "d1",
        &id,
        r#"{"code":{"source":"print(2)","suffix":".py"}}"#,
        decided,
    )
    .unwrap();
    let second = repatched["code"]["snapshot_id"].as_str().unwrap();
    assert_ne!(second, snapshot);
    assert_eq!(repatched["code"]["approval"], "PENDING");
    assert!(repatched["next_occurrence_ns"].is_null(), "{repatched}");
    assert_eq!(snapshot_count(&store), 2);
    assert_eq!(
        desk_events(&store)
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "APPROVAL_REQUESTED",
            "APPROVAL_DECIDED",
            "APPROVAL_REQUESTED"
        ]
    );
}

/// **Same code, no reapproval** (§3.2): a brief patch and a byte-identical code
/// patch both keep the snapshot and its decision.
#[cfg(test)]
#[test]
fn identical_code_keeps_its_decision() {
    let (_dir, store) = desk_store();
    let armed = T0 + 3_600 * SECOND;
    let created = create(&store, "d1", &code_body("nightly", armed, "print(1)"), T0).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let snapshot = created["code"]["snapshot_id"].as_str().unwrap().to_string();
    decide(&store, "d1", &snapshot, Decision::Approve, T0 + SECOND).unwrap();

    for body in [
        r#"{"brief":"look at MSFT"}"#,
        r#"{"code":{"source":"print(1)","suffix":".py"}}"#,
    ] {
        let patched = patch(&store, "d1", &id, body, T0 + 2 * SECOND).unwrap();
        assert_eq!(patched["code"]["snapshot_id"], snapshot.as_str(), "{body}");
        assert_eq!(patched["code"]["approval"], "APPROVED", "{body}");
        assert_eq!(patched["next_occurrence_ns"], armed, "{body}");
    }
    assert_eq!(snapshot_count(&store), 1);
    assert_eq!(desk_events(&store).len(), 2, "one request, one decision");
}

/// **Always allow** (§3.2): R2's behaviour unchanged, and nothing to decide.
#[cfg(test)]
#[test]
fn always_allow_keeps_r2_behaviour() {
    let (_dir, store) = desk_store();
    set_policy(&store, "ALWAYS_ALLOW");
    let armed = T0 + 3_600 * SECOND;
    let created = create(&store, "d1", &code_body("nightly", armed, "print(1)"), T0).unwrap();
    assert_eq!(created["code"]["approval"], "ALWAYS_ALLOW");
    assert_eq!(created["code"]["decided_at_ns"], T0);
    assert_eq!(created["code"]["approved_at_ns"], T0);
    assert_eq!(created["next_occurrence_ns"], armed);
    assert!(desk_events(&store).is_empty(), "nothing to approve");

    let snapshot = created["code"]["snapshot_id"].as_str().unwrap().to_string();
    let e = decide(&store, "d1", &snapshot, Decision::Approve, T0 + SECOND).unwrap_err();
    assert_eq!(e.code(), "APPROVAL_DECIDED");
}

/// The decision is desk-scoped and taken once (§3.1).
#[cfg(test)]
#[test]
fn a_decision_is_scoped_and_once() {
    let (_dir, store) = desk_store();
    let created = create(
        &store,
        "d1",
        &code_body("nightly", T0 + 3_600 * SECOND, "print(1)"),
        T0,
    )
    .unwrap();
    let snapshot = created["code"]["snapshot_id"].as_str().unwrap().to_string();

    for (desk, id) in [("d2", snapshot.as_str()), ("d1", "01997f00-nope")] {
        let e = decide(&store, desk, id, Decision::Approve, T0 + SECOND).unwrap_err();
        assert_eq!(e.code(), "APPROVAL_NOT_FOUND", "{desk}/{id}");
    }
    decide(&store, "d1", &snapshot, Decision::Approve, T0 + SECOND).unwrap();
    match decide(&store, "d1", &snapshot, Decision::Deny, T0 + 2 * SECOND).unwrap_err() {
        DecideError::AlreadyDecided { approval } => assert_eq!(approval, "APPROVED"),
        other => panic!("a decided snapshot is a conflict, not {other:?}"),
    }
    assert_eq!(
        desk_events(&store)
            .iter()
            .filter(|(kind, _)| kind == "APPROVAL_DECIDED")
            .count(),
        1,
        "a refused decision writes nothing"
    );
}

// ---------------------------------------------------------------------------
// Trigger invocation (`event-triggers` §8)
// ---------------------------------------------------------------------------

/// A create body with no schedule at all.
#[cfg(test)]
fn free_body(name: &str) -> String {
    format!(r#"{{"name":"{name}","brief":"look at AAPL"}}"#)
}

/// §1: a trigger may carry a schedule or none, `schedule: null` detaches one,
/// and every R2 rejection still answers `TRIGGER_INVALID` when one is given.
#[cfg(test)]
#[test]
fn schedule_optional() {
    let (_dir, store) = desk_store();

    // Created with no schedule: RECURRING, never due, no `schedule` key.
    let free = create(&store, "d1", &free_body("review"), T0).unwrap();
    let free_id = free["id"].as_str().unwrap().to_string();
    assert_eq!(free["recurrence"], "RECURRING");
    assert!(
        free.get("schedule").is_none() && free.get("next_occurrence_ns").is_none(),
        "no schedule, no projection: {free}"
    );
    assert!(
        free.get("source").is_none(),
        "a trigger has no source: {free}"
    );

    // Attaching one names the recurrence and arms the projection.
    let armed = T0 + 3_600 * SECOND;
    let attached = patch(
        &store,
        "d1",
        &free_id,
        &format!(r#"{{"schedule":{{"at":"{}"}}}}"#, rfc3339(armed)),
        T0,
    )
    .unwrap();
    assert_eq!(attached["recurrence"], "ONE_OFF");
    assert_eq!(attached["schedule"], json!({ "at_ns": armed }));
    assert_eq!(attached["next_occurrence_ns"], armed);

    // Detaching keeps the recurrence and takes the projection back to NULL.
    let detached = patch(&store, "d1", &free_id, r#"{"schedule":null}"#, T0).unwrap();
    assert_eq!(detached["recurrence"], "ONE_OFF", "{detached}");
    assert!(
        detached.get("schedule").is_none() && detached.get("next_occurrence_ns").is_none(),
        "detaching recomputes the projection as NULL: {detached}"
    );

    // A one-off created with its instant, and a recurring one created with its
    // rule, both still work; so does a schedule-less one-off by detachment.
    let once = create(
        &store,
        "d1",
        &format!(
            r#"{{"name":"once","brief":"b","schedule":{{"at":"{}"}}}}"#,
            rfc3339(armed)
        ),
        T0,
    )
    .unwrap();
    assert_eq!(once["recurrence"], "ONE_OFF");
    let ticking = create(
        &store,
        "d1",
        r#"{"name":"tick","brief":"b","schedule":
            {"rrule":"FREQ=MINUTELY","dtstart":"2026-09-03T12:00:00","tz":"UTC"}}"#,
        T0,
    )
    .unwrap();
    assert_eq!(ticking["recurrence"], "RECURRING");
    assert_eq!(ticking["next_occurrence_ns"], T0 + 60 * SECOND);

    // Every R2 rejection still answers TRIGGER_INVALID when a schedule is given.
    for (label, body) in [
        (
            "a past instant",
            r#"{"name":"bad","brief":"b","schedule":{"at":"2020-01-01T00:00:00Z"}}"#.to_string(),
        ),
        (
            "sub-minute recurrence",
            r#"{"name":"bad","brief":"b","schedule":
                {"rrule":"FREQ=SECONDLY","dtstart":"2026-09-03T12:00:00","tz":"UTC"}}"#
                .to_string(),
        ),
        (
            "a bounded rule",
            r#"{"name":"bad","brief":"b","schedule":
                {"rrule":"FREQ=DAILY;COUNT=3","dtstart":"2026-09-03T12:00:00","tz":"UTC"}}"#
                .to_string(),
        ),
        (
            "an unknown zone",
            r#"{"name":"bad","brief":"b","schedule":
                {"rrule":"FREQ=DAILY","dtstart":"2026-09-03T12:00:00","tz":"Mars/Olympus"}}"#
                .to_string(),
        ),
        (
            "a non-object schedule",
            r#"{"name":"bad","brief":"b","schedule":5}"#.to_string(),
        ),
    ] {
        let e = create(&store, "d1", &body, T0).expect_err(label);
        assert_eq!(e.code(), "TRIGGER_INVALID", "{label}");
    }
    assert_eq!(
        patch(
            &store,
            "d1",
            &free_id,
            r#"{"schedule":{"at":"2020-01-01T00:00:00Z"}}"#,
            T0
        )
        .unwrap_err()
        .code(),
        "TRIGGER_INVALID"
    );
}

/// Every firing as `(id, trigger_id, request_id)`, acceptance order — what the
/// invocation scenarios count and identify rows by.
#[cfg(test)]
fn firing_rows(store: &Store) -> Vec<(String, String, Option<String>)> {
    store
        .call(|c| {
            c.prepare(
                "SELECT id, trigger_id, request_id FROM firings \
                 ORDER BY accepted_at_ns, id",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
        })
        .unwrap()
}

#[cfg(test)]
fn prompt_count(store: &Store) -> i64 {
    store
        .call(|c| c.query_row("SELECT count(*) FROM prompts", [], |r| r.get(0)))
        .unwrap()
}

#[cfg(test)]
fn trigger_state(store: &Store, id: &str) -> (bool, Option<i64>) {
    let id = id.to_string();
    store
        .call(move |c| {
            c.query_row(
                "SELECT enabled, next_occurrence_ns FROM triggers WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, i64>(0)? == 1, r.get(1)?)),
            )
        })
        .unwrap()
}

#[cfg(test)]
fn body_of(request_id: &str, input: Option<&str>) -> String {
    match input {
        None => format!(r#"{{"request_id":"{request_id}"}}"#),
        Some(input) => json!({ "request_id": request_id, "input": input }).to_string(),
    }
}

/// §2.3's table against a fixed clock: outcomes, rows, projection, `enabled`,
/// and that a refusal writes nothing.
#[cfg(test)]
#[test]
fn invoke_unit() {
    let (_dir, store) = desk_store();
    set_policy(&store, "ALWAYS_ALLOW");
    let id_of = |value: &Value| value["id"].as_str().unwrap().to_string();

    // --- recurring, no schedule: accepted, replayed, burst ------------------
    let free = id_of(&create(&store, "d1", &free_body("review"), T0).unwrap());
    let first = invoke(&store, "d1", &free, &body_of("r1", Some("hello")), T0).unwrap();
    assert!(first.accepted && !first.has_code);
    assert_eq!(first.firing["request_id"], "r1");
    assert_eq!(first.firing["input"], "hello");
    assert_eq!(first.firing["input_bytes"], 5);
    assert_eq!(first.firing["occurrence_ns"], T0);
    assert_eq!(first.firing["accepted_at_ns"], T0);
    assert_eq!(prompt_count(&store), 1, "a code-free firing queues here");

    // The same request id again, whatever the content: the stored firing.
    for body in [body_of("r1", Some("hello")), body_of("r1", Some("other"))] {
        let again = invoke(&store, "d1", &free, &body, T0 + SECOND).unwrap();
        assert!(!again.accepted);
        assert_eq!(again.firing["id"], first.firing["id"]);
        assert_eq!(again.firing["input"], "hello", "the stored input answers");
    }
    assert_eq!(firing_rows(&store).len(), 1);
    assert_eq!(prompt_count(&store), 1);

    // A burst of distinct requests while the first prompt is still queued: no
    // backlog is ever read (ET-6).
    for n in 0..50 {
        let done = invoke(
            &store,
            "d1",
            &free,
            &body_of(&format!("burst-{n}"), None),
            T0 + 2 * SECOND + n,
        )
        .unwrap();
        assert!(done.accepted, "burst-{n}");
        assert!(
            done.firing.get("input").is_none() && done.firing.get("input_bytes").is_none(),
            "no input, no keys: {:?}",
            done.firing
        );
    }
    assert_eq!(firing_rows(&store).len(), 51);
    assert_eq!(prompt_count(&store), 51);
    // A recurring trigger keeps everything: enabled, and never due.
    assert_eq!(trigger_state(&store, &free), (true, None));

    // --- a one-off with a future instant, invoked now ------------------------
    let armed = T0 + 60 * SECOND;
    let once = id_of(
        &create(
            &store,
            "d1",
            &format!(
                r#"{{"name":"once","brief":"b","schedule":{{"at":"{}"}}}}"#,
                rfc3339(armed)
            ),
            T0,
        )
        .unwrap(),
    );
    assert_eq!(trigger_state(&store, &once), (true, Some(armed)));
    assert!(
        invoke(&store, "d1", &once, &body_of("r-once", None), T0)
            .unwrap()
            .accepted
    );
    assert_eq!(
        trigger_state(&store, &once),
        (false, None),
        "the invocation consumes the one-off: disabled, never due"
    );
    // Consumed: the refusal names the firing, and the accepted request stays a
    // duplicate forever.
    match invoke(&store, "d1", &once, &body_of("r-other", None), T0).unwrap_err() {
        e @ TriggerError::Disabled { .. } => {
            assert_eq!(e.code(), "TRIGGER_DISABLED");
            let firing = firing_rows(&store)
                .into_iter()
                .find(|row| row.1 == once)
                .unwrap();
            assert!(e.to_string().contains(&firing.0), "{e}");
        }
        other => panic!("a consumed one-off is TRIGGER_DISABLED, not {other:?}"),
    }
    assert!(
        !invoke(&store, "d1", &once, &body_of("r-once", None), T0)
            .unwrap()
            .accepted,
        "the accepted request is a duplicate after consumption"
    );
    // Re-enabling is the explicit statement that it may fire once more, so a
    // new distinct request is accepted although its `at` is long past (§1).
    patch(&store, "d1", &once, r#"{"enabled":true}"#, armed + SECOND).unwrap();
    assert!(
        invoke(&store, "d1", &once, &body_of("r-new", None), armed + SECOND)
            .unwrap()
            .accepted
    );
    assert_eq!(
        trigger_state(&store, &once),
        (false, None),
        "consumed again"
    );

    // --- the deadline: a one-off the schedule has not resolved --------------
    // Disabled before its instant, then re-enabled after it: the schedule never
    // fired it, so the schedule still owns it (ET-7).
    let held = id_of(
        &create(
            &store,
            "d1",
            &format!(
                r#"{{"name":"held","brief":"b","schedule":{{"at":"{}"}}}}"#,
                rfc3339(T0 + 2 * SECOND)
            ),
            T0,
        )
        .unwrap(),
    );
    patch(&store, "d1", &held, r#"{"enabled":false}"#, T0).unwrap();
    patch(&store, "d1", &held, r#"{"enabled":true}"#, T0 + 5 * SECOND).unwrap();
    let rows = firing_rows(&store).len();
    assert_eq!(
        invoke(
            &store,
            "d1",
            &held,
            &body_of("r-held", None),
            T0 + 5 * SECOND
        )
        .unwrap_err()
        .code(),
        "TRIGGER_ELAPSED"
    );
    assert_eq!(firing_rows(&store).len(), rows, "a refusal writes nothing");
    // Detaching the schedule makes it invocable again.
    patch(&store, "d1", &held, r#"{"schedule":null}"#, T0 + 5 * SECOND).unwrap();
    assert!(
        invoke(
            &store,
            "d1",
            &held,
            &body_of("r-held", None),
            T0 + 5 * SECOND
        )
        .unwrap()
        .accepted
    );

    // --- a recurring trigger with a schedule keeps it ------------------------
    let ticking = id_of(
        &create(
            &store,
            "d1",
            r#"{"name":"tick","brief":"b","schedule":
                {"rrule":"FREQ=MINUTELY","dtstart":"2026-09-03T12:00:00","tz":"UTC"}}"#,
            T0,
        )
        .unwrap(),
    );
    let projected = trigger_state(&store, &ticking).1;
    assert!(
        invoke(&store, "d1", &ticking, &body_of("t1", None), T0)
            .unwrap()
            .accepted
    );
    assert_eq!(trigger_state(&store, &ticking), (true, projected));

    // --- refusals write nothing ---------------------------------------------
    let before = firing_rows(&store).len();
    let prompts = prompt_count(&store);
    let disabled = id_of(&create(&store, "d1", &free_body("off"), T0).unwrap());
    patch(&store, "d1", &disabled, r#"{"enabled":false}"#, T0).unwrap();
    match invoke(&store, "d1", &disabled, &body_of("r-off", None), T0).unwrap_err() {
        e @ TriggerError::Disabled { .. } => assert_eq!(e.to_string(), "This trigger is disabled."),
        other => panic!("a disabled trigger says disabled, not {other:?}"),
    }
    // An unknown id, and a deleted trigger reached by a new request id.
    assert_eq!(
        invoke(&store, "d1", "t-nowhere", &body_of("r-x", None), T0)
            .unwrap_err()
            .code(),
        "TRIGGER_NOT_FOUND"
    );
    let gone = id_of(&create(&store, "d1", &free_body("gone"), T0).unwrap());
    assert!(
        invoke(&store, "d1", &gone, &body_of("r-gone", None), T0)
            .unwrap()
            .accepted
    );
    delete(&store, "d1", &gone, T0).unwrap();
    assert!(
        !invoke(&store, "d1", &gone, &body_of("r-gone", None), T0)
            .unwrap()
            .accepted,
        "an old request stays a duplicate on a deleted trigger"
    );
    assert_eq!(
        invoke(&store, "d1", &gone, &body_of("r-after", None), T0)
            .unwrap_err()
            .code(),
        "TRIGGER_NOT_FOUND"
    );
    // Pending code: refused, nothing buffered, and after APPROVE the same
    // request id is a new request, because nothing was stored.
    set_policy(&store, "REQUIRE_APPROVAL");
    let gated = create(
        &store,
        "d1",
        &code_body("gated", T0 + 3_600 * SECOND, "print(1)"),
        T0,
    )
    .unwrap();
    let gated_id = id_of(&gated);
    let snapshot = gated["code"]["snapshot_id"].as_str().unwrap().to_string();
    assert_eq!(
        invoke(&store, "d1", &gated_id, &body_of("r-gated", None), T0)
            .unwrap_err()
            .code(),
        "TRIGGER_UNAPPROVED"
    );
    assert_eq!(
        firing_rows(&store).len(),
        before + 1,
        "only `gone` was added"
    );
    decide(&store, "d1", &snapshot, Decision::Approve, T0 + SECOND).unwrap();
    let approved = invoke(
        &store,
        "d1",
        &gated_id,
        &body_of("r-gated", None),
        T0 + SECOND,
    )
    .unwrap();
    assert!(
        approved.accepted && approved.has_code,
        "{:?}",
        approved.firing
    );
    assert_eq!(approved.firing["code_snapshot_id"], snapshot.as_str());
    assert_eq!(
        prompt_count(&store),
        prompts + 1,
        "a code-bearing firing queues no prompt here; only `gone` did"
    );

    // A desk that has no such trigger, and a desk that does not exist.
    assert_eq!(
        invoke(&store, "d2", &free, &body_of("r-elsewhere", None), T0)
            .unwrap_err()
            .code(),
        "DESK_NOT_FOUND"
    );
}

/// §2.1's request form at both bounds.
#[cfg(test)]
#[test]
fn invoke_request_form() {
    let (_dir, store) = desk_store();
    let target = create(&store, "d1", &free_body("review"), T0).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refuse = |body: String| {
        let e = invoke(&store, "d1", &target, &body, T0).unwrap_err();
        assert_eq!(e.code(), "INVOCATION_INVALID", "{body}: {e}");
    };

    // The grammar, at both bounds.
    let longest = "a".repeat(REQUEST_ID_MAX);
    assert!(
        invoke(&store, "d1", &target, &body_of(&longest, None), T0)
            .unwrap()
            .accepted
    );
    assert!(
        invoke(
            &store,
            "d1",
            &target,
            &body_of("Ab9._:-", Some("")),
            T0 + SECOND
        )
        .unwrap()
        .accepted,
        "every allowed byte, and an empty input"
    );
    refuse(body_of(&"a".repeat(REQUEST_ID_MAX + 1), None));
    refuse(body_of("", None));
    for bad in ["has space", "has/slash", "has\u{e9}accent", "has+plus"] {
        refuse(body_of(bad, None));
    }
    refuse(r#"{}"#.to_string());
    refuse(r#"{"request_id":5}"#.to_string());
    refuse("[]".to_string());
    refuse("not json".to_string());

    // The input bound, at both sides of it, and a non-string input.
    let at_bound = "x".repeat(INPUT_MAX);
    let accepted = invoke(
        &store,
        "d1",
        &target,
        &body_of("at-bound", Some(&at_bound)),
        T0 + 2 * SECOND,
    )
    .unwrap();
    assert_eq!(accepted.firing["input_bytes"], INPUT_MAX as i64);
    refuse(body_of("over-bound", Some(&"x".repeat(INPUT_MAX + 1))));
    refuse(r#"{"request_id":"r","input":5}"#.to_string());
    refuse(r#"{"request_id":"r","input":{"a":1}}"#.to_string());

    // Every refusal wrote nothing: two accepted plus the bound one.
    assert_eq!(firing_rows(&store).len(), 3);
}

/// ET-7: a one-off is consumed through either door, and the loser sees it.
#[cfg(test)]
#[test]
fn one_off_consumed_by_either_door() {
    let started = T0 - 60 * SECOND;
    let one_off = |store: &Store, name: &str, at_ns: i64| {
        create(
            store,
            "d1",
            &format!(
                r#"{{"name":"{name}","brief":"b","schedule":{{"at":"{}"}}}}"#,
                rfc3339(at_ns)
            ),
            T0,
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // (a) The schedule accepts first: the invocation is TRIGGER_DISABLED,
    //     naming the scheduled firing, and no second firing is made.
    let (_dir, store) = desk_store();
    let scheduled = one_off(&store, "once", T0 + SECOND);
    let due = T0 + 2 * SECOND;
    let pass = store
        .unit(move |tx| crate::schedule::accept_or_miss(tx, due, started))
        .unwrap();
    assert_eq!(pass.accepted.len(), 1);
    assert_eq!(trigger_state(&store, &scheduled), (false, None));
    let e = invoke(&store, "d1", &scheduled, &body_of("r-a", None), due).unwrap_err();
    assert_eq!(e.code(), "TRIGGER_DISABLED");
    assert!(e.to_string().contains(&pass.accepted[0].firing_id), "{e}");
    assert_eq!(firing_rows(&store).len(), 1);
    // Enable, then the refused request id is a new request: nothing was stored.
    patch(&store, "d1", &scheduled, r#"{"enabled":true}"#, due).unwrap();
    assert!(
        invoke(&store, "d1", &scheduled, &body_of("r-a", None), due)
            .unwrap()
            .accepted
    );
    assert!(
        !invoke(&store, "d1", &scheduled, &body_of("r-a", None), due)
            .unwrap()
            .accepted,
        "the third call is the duplicate"
    );
    assert_eq!(trigger_state(&store, &scheduled), (false, None));
    let rows = firing_rows(&store);
    assert_eq!(rows.len(), 2, "one scheduled, one invoked: {rows:?}");
    assert_eq!(rows[0].2, None);
    assert_eq!(rows[1].2.as_deref(), Some("r-a"));

    // (b) The invocation accepts first: the scheduled deadline never fires it,
    //     and no miss is recorded — the row left the due index.
    let (_dir, store) = desk_store();
    let invoked = one_off(&store, "once", T0 + SECOND);
    assert!(
        invoke(&store, "d1", &invoked, &body_of("r-b", None), T0)
            .unwrap()
            .accepted
    );
    assert_eq!(trigger_state(&store, &invoked), (false, None));
    let after = T0 + 3 * SECOND;
    let pass = store
        .unit(move |tx| crate::schedule::accept_or_miss(tx, after, started))
        .unwrap();
    assert_eq!(pass.accepted.len(), 0);
    assert_eq!(
        pass.missed, 0,
        "a consumed one-off is not due, so not missed"
    );
    assert_eq!(firing_rows(&store).len(), 1);
}

/// §3: the result prompt carries the input inline up to the brief's bound and
/// its size alone above it.
#[cfg(test)]
#[test]
fn result_prompt_input_bound() {
    let (_dir, store) = desk_store();
    let target = create(&store, "d1", &free_body("review"), T0).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let payload = |firing_id: &str| {
        let firing_id = firing_id.to_string();
        store
            .call(move |c| {
                c.query_row(
                    "SELECT payload FROM prompts \
                     WHERE json_extract(payload, '$.firing_id') = ?1",
                    params![firing_id],
                    |r| Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap()),
                )
            })
            .unwrap()
    };

    let inline = "x".repeat(INPUT_INLINE);
    let done = invoke(
        &store,
        "d1",
        &target,
        &body_of("r-inline", Some(&inline)),
        T0,
    )
    .unwrap();
    assert_eq!(
        payload(done.firing["id"].as_str().unwrap())["invocation"],
        json!({ "request_id": "r-inline", "input_bytes": INPUT_INLINE, "input": inline })
    );

    let over = "x".repeat(INPUT_INLINE + 1);
    let done = invoke(
        &store,
        "d1",
        &target,
        &body_of("r-over", Some(&over)),
        T0 + SECOND,
    )
    .unwrap();
    let invocation = payload(done.firing["id"].as_str().unwrap())["invocation"].clone();
    assert_eq!(
        invocation,
        json!({ "request_id": "r-over", "input_bytes": INPUT_INLINE + 1 }),
        "over the bound the payload carries the size alone"
    );
    // The whole input is still readable through the firing route.
    assert_eq!(
        firing(&store, "d1", done.firing["id"].as_str().unwrap()).unwrap()["input"],
        over.as_str()
    );
}
