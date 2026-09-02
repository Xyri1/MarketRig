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
// Code snapshots (§4.1, per R2-3)
// ---------------------------------------------------------------------------

/// §4.1 bounds. `source` is UTF-8 by the time it is JSON, so the byte count is
/// the only thing left to check.
const SOURCE_MAX: usize = 262_144;
const ARGV_MAX: usize = 64;
const ARG_MAX: usize = 4_096;
const BRIEF_MAX: usize = 16_384;
const CONTEXT_MAX: usize = 65_536;
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

    /// One immutable row (§7). R2's approval is the fixed **Always allow**, so
    /// `approved_at_ns` is `created_at_ns` (per R2-3).
    fn insert(&self, tx: &Transaction<'_>, desk_id: &str, now_ns: i64) -> rusqlite::Result<String> {
        let id = Uuid::now_v7().to_string();
        tx.execute(
            "INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, timeout_secs, \
             fingerprint, approved_at_ns, created_at_ns) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                id,
                desk_id,
                self.source,
                self.suffix,
                serde_json::to_string(&self.argv).unwrap_or_default(),
                self.timeout_secs,
                fingerprint(&self.source, &self.suffix, &self.argv, self.timeout_secs),
                now_ns,
            ],
        )?;
        Ok(id)
    }
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
         c.timeout_secs, c.fingerprint, c.approved_at_ns, length(CAST(c.source AS BLOB)) \
         FROM triggers t LEFT JOIN code_snapshots c ON c.id = t.code_snapshot_id",
        if with_source { "c.source" } else { "NULL" }
    )
}

/// The §8 Trigger. `with_source` is the single read; the listing reports the
/// snapshot's size instead (§4.1).
fn trigger_resource(r: &Row<'_>, with_source: bool) -> rusqlite::Result<Value> {
    let schedule = Schedule::from_row(
        &r.get::<_, String>(3)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    );
    let code = match r.get::<_, Option<String>>(16)? {
        None => Value::Null,
        Some(snapshot_id) => {
            let source: Option<String> = r.get(17)?;
            let argv: String = r.get(19)?;
            json!({
                "snapshot_id": snapshot_id,
                "suffix": r.get::<_, String>(18)?,
                "argv": serde_json::from_str::<Value>(&argv).unwrap_or(Value::Null),
                "timeout_secs": r.get::<_, i64>(20)?,
                "fingerprint": r.get::<_, String>(21)?,
                "approved_at_ns": r.get::<_, Option<i64>>(22)?,
                "source_bytes": r.get::<_, i64>(23)?,
                "source": with_source.then_some(source).flatten(),
            })
        }
    };
    Ok(compact(json!({
        "id": r.get::<_, String>(0)?,
        "desk_id": r.get::<_, String>(1)?,
        "name": r.get::<_, String>(2)?,
        "source": "SCHEDULED",
        "recurrence": schedule.recurrence(),
        "brief": r.get::<_, String>(4)?,
        "context": r.get::<_, Option<String>>(5)?,
        "schedule": schedule.to_json(),
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
         length(e.stdout), length(e.stderr) \
         FROM firings f LEFT JOIN executions e ON e.firing_id = f.id",
        if with_streams {
            "e.stdout, e.stderr"
        } else {
            "NULL, NULL"
        }
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

/// The schedule's four columns (§7); the row's CHECKs want exactly one shape.
fn schedule_columns(
    schedule: &Schedule,
) -> (Option<i64>, Option<String>, Option<String>, Option<String>) {
    match schedule {
        Schedule::OneOff { at_ns } => (Some(*at_ns), None, None, None),
        Schedule::Recurring { rrule, dtstart, tz } => (
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
    let schedule = fields
        .get("schedule")
        .ok_or_else(|| invalid("`schedule` is required.".to_string()))
        .and_then(|v| Schedule::parse(v, now_ns).map_err(invalid))?;
    let code = match fields.get("code") {
        None | Some(Value::Null) => None,
        Some(value) => Some(Snapshot::parse(value).map_err(invalid)?),
    };

    let id = Uuid::now_v7().to_string();
    let (read_id, desk_owned, taken) = (id.clone(), desk_id.to_string(), name.clone());
    // Projected before the unit: the candidate scan (§2) never holds the write
    // transaction on the create path.
    let next = schedule.next_after(now_ns);
    let created = store.unit(move |tx| {
        let snapshot_id = match &code {
            None => None,
            Some(code) => Some(code.insert(tx, &desk_owned, now_ns)?),
        };
        let (at_ns, rrule, dtstart, tz) = schedule_columns(&schedule);
        tx.execute(
            "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, context, \
             at_ns, rrule, dtstart, tz, enabled, revision, code_snapshot_id, \
             next_occurrence_ns, created_at_ns, updated_at_ns) \
             VALUES (?1, ?2, ?3, 'SCHEDULED', ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, 1, ?11, ?12, ?13, ?13)",
            params![
                id,
                desk_owned,
                name,
                schedule.recurrence(),
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
    schedule: Option<Schedule>,
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
                    "SELECT recurrence, brief, context, at_ns, rrule, dtstart, tz, enabled, \
                     code_snapshot_id FROM triggers \
                     WHERE desk_id = ?1 AND id = ?2 AND deleted_at_ns IS NULL",
                    params![desk, id],
                    |r| {
                        Ok((
                            Schedule::from_row(
                                &r.get::<_, String>(0)?,
                                r.get(3)?,
                                r.get(4)?,
                                r.get(5)?,
                                r.get(6)?,
                            ),
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, i64>(7)? == 1,
                            r.get::<_, Option<String>>(8)?,
                        ))
                    },
                )
                .optional()?
            else {
                return Ok(None);
            };
            let (row_schedule, row_brief, row_context, row_enabled, row_snapshot) = current;
            let schedule = patch.schedule.unwrap_or(row_schedule);
            let enabled = patch.enabled.unwrap_or(row_enabled);
            let snapshot_id = match &patch.code {
                None => row_snapshot,
                Some(None) => None,
                Some(Some(code)) => Some(code.insert(tx, &desk, now_ns)?),
            };
            let (at_ns, rrule, dtstart, tz) = schedule_columns(&schedule);
            // Disabled is never due; otherwise the projection is recomputed from
            // the definition's own anchor against now (§2).
            // ponytail: the scan runs inside the unit because the row's schedule
            // is read here; it is bounded at 100,000 candidates (R2-1's ceiling,
            // a persisted cursor is the upgrade).
            let next = enabled.then(|| schedule.next_after(now_ns)).flatten();
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
                    schedule.recurrence(),
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
            "SELECT id, desk_id, kind, state, created_at_ns FROM prompts WHERE desk_id = ?1 \
             ORDER BY created_at_ns DESC, id DESC",
        )?
        .query_map(params![desk], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "desk_id": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "state": r.get::<_, String>(3)?,
                "created_at_ns": r.get::<_, i64>(4)?,
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
                "SELECT id, desk_id, kind, state, created_at_ns, payload FROM prompts \
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
