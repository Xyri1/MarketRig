//! Desk identity: name grammar, creation, retry, and validation.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §7,
//! root `sdd/SPEC.md` §5.1 and §5.2.

use std::path::Path;
use std::{fmt, io};

use rusqlite::{ErrorCode, Row, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::store::{Store, StoreError, now_ns};

/// The MarketRig-owned Claude Code compatibility shim, exactly (§7.2, per D20).
const SHIM: &str = "@AGENTS.md\n";

/// The one `failure_code` R0 records: every bootstrap step fails the same way.
const BOOTSTRAP_FAILED: &str = "BOOTSTRAP_FAILED";

const SELECT: &str = "SELECT id, name, state, workspace_path, created_at_ns, \
                      ready_at_ns, failure_code, failure_message FROM desks";

/// Accepts the §7.1 grammar: 1–40 characters, lowercase `a–z`, `0–9`, and
/// single interior hyphens.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// A desk failure carrying a stable code the REST surface maps to the §6 envelope.
#[derive(Debug)]
pub enum DeskError {
    NameInvalid(String),
    NameTaken(String),
    NotFound(String),
    /// Retry attempted on a desk that is not `FAILED`.
    StateInvalid(String),
    Store(StoreError),
}

impl DeskError {
    pub fn code(&self) -> &'static str {
        match self {
            DeskError::NameInvalid(_) => "DESK_NAME_INVALID",
            DeskError::NameTaken(_) => "DESK_NAME_TAKEN",
            DeskError::NotFound(_) => "DESK_NOT_FOUND",
            DeskError::StateInvalid(_) => "DESK_STATE_INVALID",
            DeskError::Store(e) => e.code(),
        }
    }
}

impl fmt::Display for DeskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeskError::NameInvalid(name) => write!(
                f,
                "Desk name {name:?} is not 1-40 characters of lowercase letters, \
                 digits, and single interior hyphens."
            ),
            DeskError::NameTaken(name) => write!(f, "A desk named {name:?} already exists."),
            DeskError::NotFound(id) => write!(f, "No desk has id {id:?}."),
            DeskError::StateInvalid(state) => {
                write!(
                    f,
                    "Only a FAILED desk can be retried; this desk is {state}."
                )
            }
            DeskError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DeskError {}

impl From<StoreError> for DeskError {
    fn from(e: StoreError) -> Self {
        DeskError::Store(e)
    }
}

/// One desk row plus the read-time workspace derivation (§7.5). Field names and
/// omit-when-null behavior are the §6 `Desk` resource.
#[derive(Debug, Clone, Serialize)]
pub struct Desk {
    pub id: String,
    pub name: String,
    /// `CREATING` | `READY` | `FAILED`.
    pub state: String,
    pub workspace_path: String,
    pub created_at_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// `OK` | `UNAVAILABLE`, derived for `READY` desks only; never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_status_reason: Option<String>,
}

/// The R0 `AGENTS.md` seed (§7.6). Agent-owned from the moment it is written.
fn agents_seed(name: &str) -> String {
    format!(
        "# {name}\n\
         \n\
         This desk's constitution. MarketRig seeded it at desk creation and never\n\
         rewrites it; its full content arrives with later MarketRig milestones.\n"
    )
}

/// Creation step 2 (§7.2): idempotent, so a half-written workspace completes.
fn bootstrap(workspace: &Path, name: &str) -> io::Result<()> {
    std::fs::create_dir_all(workspace)?;
    let agents = workspace.join("AGENTS.md");
    if !agents.exists() {
        std::fs::write(&agents, agents_seed(name))?;
    }
    let shim = workspace.join("CLAUDE.md");
    if std::fs::read(&shim).ok().as_deref() != Some(SHIM.as_bytes()) {
        std::fs::write(&shim, SHIM)?;
    }
    Ok(())
}

/// Creates a desk synchronously (§7.2): the returned row is `READY` or `FAILED`.
pub fn create(store: &Store, desks_home: &Path, name: &str) -> Result<Desk, DeskError> {
    if !valid_name(name) {
        return Err(DeskError::NameInvalid(name.to_string()));
    }
    let workspace_path = desks_home
        .join(name)
        .into_os_string()
        .into_string()
        .map_err(|p| {
            DeskError::Store(StoreError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Workspace path {p:?} is not valid UTF-8."),
            )))
        })?;
    let desk = Desk {
        id: Uuid::now_v7().to_string(),
        name: name.to_string(),
        state: "CREATING".to_string(),
        workspace_path,
        created_at_ns: now_ns(),
        ready_at_ns: None,
        failure_code: None,
        failure_message: None,
        workspace_status: None,
        workspace_status_reason: None,
    };

    // Step 1: the row and its event commit before the workspace is touched.
    let row = desk.clone();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns) \
                 VALUES (?1, ?2, 'CREATING', ?3, ?4)",
                params![row.id, row.name, row.workspace_path, row.created_at_ns],
            )?;
            append_event(
                tx,
                "DESK_CREATED",
                Some(&row.id),
                row.created_at_ns,
                json!({ "name": row.name }),
            )
        })
        .map_err(|e| match &e {
            // The id is a fresh UUIDv7 and the name is already validated, so the
            // only constraint a fresh insert can violate is the name's UNIQUE.
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _))
                if f.code == ErrorCode::ConstraintViolation =>
            {
                DeskError::NameTaken(name.to_string())
            }
            _ => e.into(),
        })?;

    finish(store, desk)
}

/// Startup completion (§7.3): finishes every interrupted `CREATING` desk,
/// returning how many were completed.
pub fn complete_interrupted(store: &Store) -> Result<usize, DeskError> {
    let pending = query(store, "WHERE state = 'CREATING' ORDER BY created_at_ns, id")?;
    let count = pending.len();
    for desk in pending {
        finish(store, desk)?;
    }
    Ok(count)
}

/// Retry (§7.4): only a `FAILED` desk, on the same UUID, name, and path.
pub fn retry(store: &Store, desk_id: &str) -> Result<Desk, DeskError> {
    let mut desk = load(store, desk_id)?;
    if desk.state != "FAILED" {
        return Err(DeskError::StateInvalid(desk.state));
    }
    let at_ns = now_ns();
    let row = desk.clone();
    store.unit(move |tx| {
        tx.execute(
            "UPDATE desks SET state = 'CREATING', failure_code = NULL, failure_message = NULL \
             WHERE id = ?1",
            params![row.id],
        )?;
        append_event(
            tx,
            "DESK_RETRIED",
            Some(&row.id),
            at_ns,
            json!({ "name": row.name }),
        )
    })?;
    desk.state = "CREATING".to_string();
    desk.failure_code = None;
    desk.failure_message = None;
    finish(store, desk)
}

/// Every desk in creation order (§6 `GET /desks`).
pub fn list(store: &Store) -> Result<Vec<Desk>, DeskError> {
    let mut desks = query(store, "ORDER BY created_at_ns, id")?;
    for desk in &mut desks {
        derive_status(desk);
    }
    Ok(desks)
}

/// One desk by UUID (§6 `GET /desks/{desk_id}`).
pub fn get(store: &Store, desk_id: &str) -> Result<Desk, DeskError> {
    let mut desk = load(store, desk_id)?;
    derive_status(&mut desk);
    Ok(desk)
}

/// Creation steps 2 and 3, shared by create, startup completion, and retry.
fn finish(store: &Store, mut desk: Desk) -> Result<Desk, DeskError> {
    let at_ns = now_ns();
    let row = desk.clone();
    match bootstrap(Path::new(&desk.workspace_path), &desk.name) {
        Ok(()) => {
            store.unit(move |tx| {
                tx.execute(
                    "UPDATE desks SET state = 'READY', ready_at_ns = ?2 WHERE id = ?1",
                    params![row.id, at_ns],
                )?;
                append_event(
                    tx,
                    "DESK_READY",
                    Some(&row.id),
                    at_ns,
                    json!({ "name": row.name }),
                )
            })?;
            desk.state = "READY".to_string();
            desk.ready_at_ns = Some(at_ns);
        }
        Err(e) => {
            let message = format!("Workspace bootstrap failed: {e}.");
            let recorded = message.clone();
            store.unit(move |tx| {
                tx.execute(
                    "UPDATE desks SET state = 'FAILED', ready_at_ns = NULL, \
                     failure_code = ?2, failure_message = ?3 WHERE id = ?1",
                    params![row.id, BOOTSTRAP_FAILED, recorded],
                )?;
                append_event(
                    tx,
                    "DESK_FAILED",
                    Some(&row.id),
                    at_ns,
                    json!({ "name": row.name, "failure_code": BOOTSTRAP_FAILED }),
                )
            })?;
            desk.state = "FAILED".to_string();
            desk.ready_at_ns = None;
            desk.failure_code = Some(BOOTSTRAP_FAILED.to_string());
            desk.failure_message = Some(message);
        }
    }
    derive_status(&mut desk);
    Ok(desk)
}

/// `READY` validation (§7.5): a pure read-time derivation — nothing is rewritten.
fn derive_status(desk: &mut Desk) {
    if desk.state != "READY" {
        return;
    }
    let workspace = Path::new(&desk.workspace_path);
    let (status, reason) = if !workspace.is_dir() {
        (
            "UNAVAILABLE",
            Some("Workspace directory is missing.".into()),
        )
    } else if let Err(e) = std::fs::File::open(workspace.join("AGENTS.md")) {
        (
            "UNAVAILABLE",
            Some(format!("AGENTS.md is unreadable: {e}.")),
        )
    } else {
        ("OK", None)
    };
    desk.workspace_status = Some(status);
    desk.workspace_status_reason = reason;
}

fn load(store: &Store, desk_id: &str) -> Result<Desk, DeskError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    let id = desk_id.to_string();
    store
        .call(move |c| c.query_row(&sql, params![id], read_row))
        .map_err(|e| match &e {
            StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => {
                DeskError::NotFound(desk_id.to_string())
            }
            _ => e.into(),
        })
}

fn query(store: &Store, tail: &str) -> Result<Vec<Desk>, DeskError> {
    let sql = format!("{SELECT} {tail}");
    Ok(store.call(move |c| {
        c.prepare(&sql)?
            .query_map([], read_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
    })?)
}

fn read_row(r: &Row<'_>) -> rusqlite::Result<Desk> {
    Ok(Desk {
        id: r.get(0)?,
        name: r.get(1)?,
        state: r.get(2)?,
        workspace_path: r.get(3)?,
        created_at_ns: r.get(4)?,
        ready_at_ns: r.get(5)?,
        failure_code: r.get(6)?,
        failure_message: r.get(7)?,
        workspace_status: None,
        workspace_status_reason: None,
    })
}

/// Appends one `operational_events` row inside the transaction of the change it
/// evidences (§3.3). `desk_id` is `None` for installation-wide kinds.
pub(crate) fn append_event(
    tx: &Transaction<'_>,
    kind: &str,
    desk_id: Option<&str>,
    at_ns: i64,
    payload: Value,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO operational_events (id, kind, desk_id, occurred_at_ns, payload) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            Uuid::now_v7().to_string(),
            kind,
            desk_id,
            at_ns,
            payload.to_string()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
fn scratch() -> (tempfile::TempDir, Store, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("marketrig.sqlite3")).unwrap();
    let desks = dir.path().join("desks");
    fs::create_dir_all(&desks).unwrap();
    (dir, store, desks)
}

#[cfg(test)]
fn event_kinds(store: &Store) -> Vec<String> {
    store
        .call(|c| {
            c.prepare("SELECT kind FROM operational_events ORDER BY occurred_at_ns, id")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .unwrap()
}

#[cfg(test)]
#[test]
fn name_grammar() {
    let forty = "a".repeat(40);
    for name in [
        "alpha",
        "a",
        "0",
        "desk-1",
        "a-b-c",
        "btc-scalper-2",
        forty.as_str(),
    ] {
        assert!(valid_name(name), "{name:?} must be accepted");
    }
    let forty_one = "a".repeat(41);
    for name in [
        "",
        forty_one.as_str(),
        "Alpha",
        "ALPHA",
        "-alpha",
        "alpha-",
        "al--pha",
        "--",
        "-",
        "al_pha",
        "al pha",
        "café",
        "台北",
        "alpha.1",
        "alpha/beta",
    ] {
        assert!(!valid_name(name), "{name:?} must be rejected");
    }
}

#[cfg(test)]
#[test]
fn bootstrap_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("alpha");

    bootstrap(&workspace, "alpha").unwrap();
    let seeded = fs::read(workspace.join("AGENTS.md")).unwrap();
    assert_eq!(
        String::from_utf8(seeded.clone()).unwrap(),
        agents_seed("alpha")
    );
    assert!(agents_seed("alpha").starts_with("# alpha\n"));
    assert_eq!(
        fs::read(workspace.join("CLAUDE.md")).unwrap(),
        b"@AGENTS.md\n"
    );

    // Byte-identical on a second run.
    bootstrap(&workspace, "alpha").unwrap();
    assert_eq!(fs::read(workspace.join("AGENTS.md")).unwrap(), seeded);
    assert_eq!(
        fs::read(workspace.join("CLAUDE.md")).unwrap(),
        b"@AGENTS.md\n"
    );

    // The agent-owned constitution survives; the MarketRig-owned shim is reconciled.
    fs::write(workspace.join("AGENTS.md"), "# alpha\n\nmine now\n").unwrap();
    fs::write(workspace.join("CLAUDE.md"), "@SOMETHING-ELSE.md\n").unwrap();
    bootstrap(&workspace, "alpha").unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        "# alpha\n\nmine now\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );
}

#[cfg(test)]
#[test]
fn interrupted_creating_completes() {
    let (_dir, store, desks_home) = scratch();
    let workspace = desks_home.join("gamma");
    fs::create_dir_all(&workspace).unwrap(); // half-written: directory, no files
    let path = workspace.to_str().unwrap().to_string();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns) \
                 VALUES ('0199-gamma', 'gamma', 'CREATING', ?1, 1000)",
                [path],
            )
        })
        .unwrap();

    assert_eq!(complete_interrupted(&store).unwrap(), 1);

    let desk = get(&store, "0199-gamma").unwrap();
    assert_eq!(desk.state, "READY");
    assert!(desk.ready_at_ns.is_some());
    assert_eq!(desk.workspace_status, Some("OK"));
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        agents_seed("gamma")
    );
    assert_eq!(event_kinds(&store), ["DESK_READY"]);

    // Nothing left interrupted.
    assert_eq!(complete_interrupted(&store).unwrap(), 0);
}

#[cfg(test)]
#[test]
fn create_retry_and_reads() {
    let (_dir, store, desks_home) = scratch();

    let alpha = create(&store, &desks_home, "alpha").unwrap();
    assert_eq!(alpha.state, "READY");
    assert_eq!(alpha.workspace_status, Some("OK"));
    assert_eq!(
        alpha.workspace_path,
        desks_home.join("alpha").to_str().unwrap()
    );
    assert_eq!(
        fs::read_to_string(desks_home.join("alpha/CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );

    // Refusals.
    assert_eq!(
        create(&store, &desks_home, "alpha").unwrap_err().code(),
        "DESK_NAME_TAKEN"
    );
    assert_eq!(
        create(&store, &desks_home, "Bad--Name").unwrap_err().code(),
        "DESK_NAME_INVALID"
    );
    assert_eq!(
        get(&store, "no-such-desk").unwrap_err().code(),
        "DESK_NOT_FOUND"
    );
    assert_eq!(
        retry(&store, &alpha.id).unwrap_err().code(),
        "DESK_STATE_INVALID"
    );

    // Obstructed workspace fails, then retries to READY on the same identity.
    fs::write(desks_home.join("beta"), "not a directory").unwrap();
    let beta = create(&store, &desks_home, "beta").unwrap();
    assert_eq!(beta.state, "FAILED");
    assert!(beta.failure_code.is_some() && beta.failure_message.is_some());
    fs::remove_file(desks_home.join("beta")).unwrap();
    let retried = retry(&store, &beta.id).unwrap();
    assert_eq!(retried.state, "READY");
    assert_eq!(retried.id, beta.id);
    assert_eq!(retried.workspace_path, beta.workspace_path);
    assert_eq!(retried.created_at_ns, beta.created_at_ns);
    assert!(retried.failure_code.is_none() && retried.failure_message.is_none());

    // Reads: creation order, and a damaged READY workspace derives UNAVAILABLE.
    fs::remove_file(desks_home.join("alpha/AGENTS.md")).unwrap();
    let rows = list(&store).unwrap();
    let names: Vec<&str> = rows.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert_eq!(rows[0].state, "READY");
    assert_eq!(rows[0].workspace_status, Some("UNAVAILABLE"));
    assert!(rows[0].workspace_status_reason.is_some());
    assert_eq!(rows[1].workspace_status, Some("OK"));

    assert_eq!(
        event_kinds(&store),
        [
            "DESK_CREATED",
            "DESK_READY",
            "DESK_CREATED",
            "DESK_FAILED",
            "DESK_RETRIED",
            "DESK_READY"
        ]
    );
}
