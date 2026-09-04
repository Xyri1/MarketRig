//! Desk identity: name grammar, creation, retry, and validation.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §7,
//! root `sdd/SPEC.md` §5.1 and §5.2.

use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use rusqlite::{ErrorCode, Row, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::store::{Store, StoreError, now_ns};

/// The MarketRig-owned Claude Code compatibility shim, exactly (§7.2, per D20).
const SHIM: &str = "@AGENTS.md\n";

/// The seeded constitution and improvement skill, byte-identical to the R4
/// feature SPEC §5.1 and §5.2 blocks. `<name>` is the desk name.
const SEED_AGENTS: &str = include_str!("../seed/AGENTS.md");
const SEED_SKILL: &str = include_str!("../seed/desk-improvement.SKILL.md");

/// The one `failure_code` R0 records: every bootstrap step fails the same way.
const BOOTSTRAP_FAILED: &str = "BOOTSTRAP_FAILED";

const SELECT: &str = "SELECT id, name, state, workspace_path, created_at_ns, \
                      ready_at_ns, failure_code, failure_message, selected_runtime FROM desks";

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
    /// `codex` | `claude`, the runtime this desk activates on (R3 §7).
    pub selected_runtime: String,
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
    /// The desk's native-session pointers, filled by `GET /desks/{id}` only
    /// (R3 feature SPEC §7); never stored on this row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_sessions: Option<Value>,
}

/// The `AGENTS.md` seed (R4 §5.1). Agent-owned from the moment it is written.
fn agents_seed(name: &str) -> String {
    SEED_AGENTS.replace("<name>", name)
}

/// The desk's one canonical skills tree (R4 §5, per D21).
fn skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".agents").join("skills")
}

/// Claude Code's view of that same tree: a relative symlink on macOS, a
/// directory junction on Windows (R4 §5, per D21).
fn skills_link(workspace: &Path) -> PathBuf {
    workspace.join(".claude").join("skills")
}

/// Creation step 2 (§7.2, R4 §5): the four artifacts in order, each only when
/// absent, so a half-written workspace completes and a retry rewrites nothing.
fn bootstrap(workspace: &Path, name: &str) -> io::Result<()> {
    fs::create_dir_all(workspace)?;
    let agents = workspace.join("AGENTS.md");
    if !agents.exists() {
        fs::write(&agents, agents_seed(name))?;
    }
    reconcile_shim(workspace)?;
    let skill = skills_dir(workspace)
        .join("desk-improvement")
        .join("SKILL.md");
    if !skill.exists() {
        fs::create_dir_all(skill.parent().expect("the seeded skill has a parent"))?;
        fs::write(&skill, SEED_SKILL)?;
    }
    reconcile_link(workspace)
}

/// The one file inside a desk workspace MarketRig owns (§7.2, per D20).
fn reconcile_shim(workspace: &Path) -> io::Result<()> {
    let shim = workspace.join("CLAUDE.md");
    if fs::read(&shim).ok().as_deref() != Some(SHIM.as_bytes()) {
        fs::write(&shim, SHIM)?;
    }
    Ok(())
}

/// Reconciles `.claude/skills` (R4 §5): a missing link is created after
/// `.agents/skills/` is created empty if absent, a link naming anything but
/// this desk's tree is replaced, and an ordinary file or directory is left in
/// place (per D21) for the read-time status to report.
fn reconcile_link(workspace: &Path) -> io::Result<()> {
    let link = skills_link(workspace);
    match skills_link_target(workspace)? {
        Some(target) if resolves_to_skills(workspace, &target) => return Ok(()),
        Some(_) => remove_link(&link)?,
        None if fs::symlink_metadata(&link).is_ok() => return Ok(()),
        None => {}
    }
    fs::create_dir_all(skills_dir(workspace))?;
    link_skills(workspace)
}

/// Whether a link target — relative to `.claude/`, or absolute — is this desk's
/// `.agents/skills`. A target that cannot be resolved counts as not it.
fn resolves_to_skills(workspace: &Path, target: &Path) -> bool {
    // `join` on an absolute target discards the prefix, so one line covers both.
    let resolved = workspace.join(".claude").join(target);
    matches!(
        (fs::canonicalize(resolved), fs::canonicalize(skills_dir(workspace))),
        (Ok(link), Ok(skills)) if link == skills
    )
}

/// Creates a desk synchronously (§7.2): the returned row is `READY` or `FAILED`.
pub fn create(
    store: &Store,
    desks_home: &Path,
    name: &str,
    selected_runtime: &str,
) -> Result<Desk, DeskError> {
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
        selected_runtime: selected_runtime.to_string(),
        ready_at_ns: None,
        failure_code: None,
        failure_message: None,
        workspace_status: None,
        workspace_status_reason: None,
        native_sessions: None,
    };

    // Step 1: the row and its event commit before the workspace is touched.
    let row = desk.clone();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, \
                 selected_runtime) VALUES (?1, ?2, 'CREATING', ?3, ?4, ?5)",
                params![
                    row.id,
                    row.name,
                    row.workspace_path,
                    row.created_at_ns,
                    row.selected_runtime
                ],
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

/// Startup validation (§7.5, R4 §5): for every `READY` desk, reconcile the two
/// things MarketRig owns — the `CLAUDE.md` shim and the `.claude/skills` link —
/// and nothing else. A workspace that will not reconcile is logged and left
/// alone: it blocks neither another desk nor startup.
pub fn validate_ready(store: &Store) -> Result<(), DeskError> {
    for desk in query(store, "WHERE state = 'READY' ORDER BY created_at_ns, id")? {
        let workspace = Path::new(&desk.workspace_path);
        if !workspace.is_dir() {
            continue;
        }
        if let Err(e) = reconcile_shim(workspace).and_then(|()| reconcile_link(workspace)) {
            tracing::warn!(desk = desk.name, "workspace reconciliation failed: {e}");
        }
    }
    Ok(())
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
    } else if let Err(e) = fs::File::open(workspace.join("AGENTS.md")) {
        (
            "UNAVAILABLE",
            Some(format!("AGENTS.md is unreadable: {e}.")),
        )
    } else {
        // An ordinary file or directory at `.claude/skills` is left in place
        // (per D21) and named here; the desk stays `OK` (R4 §5).
        let obstructed = matches!(skills_link_target(workspace), Ok(None))
            && fs::symlink_metadata(skills_link(workspace)).is_ok();
        (
            "OK",
            obstructed.then(|| {
                ".claude/skills is an ordinary file or directory, not the link to \
                 .agents/skills, so Claude Code does not see this desk's skills."
                    .to_string()
            }),
        )
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
        selected_runtime: r.get(8)?,
        workspace_status: None,
        workspace_status_reason: None,
        native_sessions: None,
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

// ---------------------------------------------------------------------------
// The `.claude/skills` link (R4 §5, per D21)
// ---------------------------------------------------------------------------

/// Points `.claude/skills` at `../.agents/skills`, exactly.
#[cfg(unix)]
pub fn link_skills(desk_dir: &Path) -> io::Result<()> {
    let link = skills_link(desk_dir);
    fs::create_dir_all(link.parent().expect("the link has a parent"))?;
    std::os::unix::fs::symlink("../.agents/skills", link)
}

/// The `.claude/skills` link's target, or `None` when nothing is there or what
/// is there is an ordinary file or directory.
#[cfg(unix)]
pub fn skills_link_target(desk_dir: &Path) -> io::Result<Option<PathBuf>> {
    match fs::read_link(skills_link(desk_dir)) {
        Ok(target) => Ok(Some(target)),
        // `EINVAL`: there is something there, and it is not a symlink.
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn remove_link(link: &Path) -> io::Result<()> {
    fs::remove_file(link)
}

/// `IO_REPARSE_TAG_MOUNT_POINT`. The `windows` crate carries it only inside the
/// whole `Win32_System_SystemServices` module, which nothing else here needs.
#[cfg(windows)]
const MOUNT_POINT: u32 = 0xa000_0003;

/// A mount-point `REPARSE_DATA_BUFFER`, sized to the largest one Windows takes.
#[cfg(windows)]
#[repr(C)]
struct MountPoint {
    tag: u32,
    data_len: u16,
    reserved: u16,
    substitute_offset: u16,
    substitute_len: u16,
    print_offset: u16,
    print_len: u16,
    path: [u16; (windows::Win32::Storage::FileSystem::MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize
        - 16)
        / 2],
}

/// Opens the link itself — never what it points at — for one reparse-point ioctl.
#[cfg(windows)]
fn open_reparse(path: &Path, access: u32) -> io::Result<windows::Win32::Foundation::HANDLE> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        CreateFileW(
            windows::core::PCWSTR(wide.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|e| io::Error::other(e.to_string()))
}

/// Makes `.claude/skills` a directory junction to the absolute `.agents/skills`.
#[cfg(windows)]
pub fn link_skills(desk_dir: &Path) -> io::Result<()> {
    use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

    // A junction is the empty directory itself carrying a reparse point.
    let link = skills_link(desk_dir);
    fs::create_dir_all(&link)?;
    let target = fs::canonicalize(skills_dir(desk_dir))?;
    let target = target.to_string_lossy();
    let target = target.strip_prefix(r"\\?\").unwrap_or(&target);

    let substitute: Vec<u16> = format!(r"\??\{target}").encode_utf16().collect();
    let print: Vec<u16> = target.encode_utf16().collect();
    let mut buffer: Box<MountPoint> = Box::new(unsafe { std::mem::zeroed() });
    let words = substitute.len() + print.len() + 2; // both names are NUL-terminated
    if words > buffer.path.len() {
        return Err(io::Error::other(format!(
            "The skills path {target} is too long for a junction."
        )));
    }
    buffer.tag = MOUNT_POINT;
    buffer.data_len = (8 + words * 2) as u16;
    buffer.substitute_offset = 0;
    buffer.substitute_len = (substitute.len() * 2) as u16;
    buffer.print_offset = (substitute.len() * 2 + 2) as u16;
    buffer.print_len = (print.len() * 2) as u16;
    buffer.path[..substitute.len()].copy_from_slice(&substitute);
    let print_at = substitute.len() + 1;
    buffer.path[print_at..print_at + print.len()].copy_from_slice(&print);

    let handle = open_reparse(&link, GENERIC_WRITE.0)?;
    let set = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            Some((&raw const *buffer).cast()),
            8 + u32::from(buffer.data_len),
            None,
            0,
            None,
            None,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    set.map_err(|e| io::Error::other(e.to_string()))
}

/// The junction's target, or `None` when nothing is there, what is there is an
/// ordinary file or directory, or the reparse point is not a mount point.
#[cfg(windows)]
pub fn skills_link_target(desk_dir: &Path) -> io::Result<Option<PathBuf>> {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

    let link = skills_link(desk_dir);
    let metadata = match fs::symlink_metadata(&link) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0 {
        return Ok(None);
    }
    let handle = open_reparse(&link, GENERIC_READ.0)?;
    let mut buffer: Box<MountPoint> = Box::new(unsafe { std::mem::zeroed() });
    let read = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_REPARSE_POINT,
            None,
            0,
            Some((&raw mut *buffer).cast()),
            MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
            None,
            None,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    read.map_err(|e| io::Error::other(e.to_string()))?;
    if buffer.tag != MOUNT_POINT {
        return Ok(None);
    }
    let at = buffer.substitute_offset as usize / 2;
    let Some(name) = buffer.path.get(at..at + buffer.substitute_len as usize / 2) else {
        return Ok(None);
    };
    let name = String::from_utf16_lossy(name);
    Ok(Some(PathBuf::from(
        name.strip_prefix(r"\??\").unwrap_or(&name),
    )))
}

#[cfg(windows)]
fn remove_link(link: &Path) -> io::Result<()> {
    // The junction is a directory; removing it leaves what it names untouched.
    fs::remove_dir(link)
}

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

/// One fenced block of the R4 feature SPEC, read at test time so the seeds
/// cannot drift from the contract (R4 §5.1, §5.2).
#[cfg(test)]
fn spec_block(heading: &str) -> String {
    let spec = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdd/features/r4-memory-skills-loop/SPEC.md"),
    )
    .unwrap();
    let section = &spec[spec.find(heading).expect("the heading")..];
    let open = "```markdown\n";
    let body = &section[section.find(open).expect("the fence") + open.len()..];
    body[..body.find("\n```").expect("the closing fence") + 1].to_string()
}

#[cfg(test)]
#[test]
fn seeds_are_the_spec_blocks() {
    assert_eq!(SEED_AGENTS, spec_block("### 5.1 `AGENTS.md`"));
    assert_eq!(
        SEED_SKILL,
        spec_block("### 5.2 `.agents/skills/desk-improvement/SKILL.md`")
    );
    assert_eq!(agents_seed("alpha"), SEED_AGENTS.replace("<name>", "alpha"));
    assert!(agents_seed("alpha").starts_with("# alpha\n"));
    assert!(!agents_seed("alpha").contains("<name>"));
}

#[cfg(test)]
#[test]
fn bootstrap_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("alpha");
    let skill = skills_dir(&workspace)
        .join("desk-improvement")
        .join("SKILL.md");

    bootstrap(&workspace, "alpha").unwrap();
    let seeded = fs::read(workspace.join("AGENTS.md")).unwrap();
    assert_eq!(
        String::from_utf8(seeded.clone()).unwrap(),
        agents_seed("alpha")
    );
    assert_eq!(
        fs::read(workspace.join("CLAUDE.md")).unwrap(),
        b"@AGENTS.md\n"
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), SEED_SKILL);
    assert!(resolves_to_skills(
        &workspace,
        &skills_link_target(&workspace).unwrap().unwrap()
    ));

    // One physical directory, reachable through both paths (R4 §5).
    assert_eq!(
        fs::read_to_string(
            skills_link(&workspace)
                .join("desk-improvement")
                .join("SKILL.md")
        )
        .unwrap(),
        SEED_SKILL
    );
    fs::write(skills_link(&workspace).join("through-the-link"), "x").unwrap();
    assert!(skills_dir(&workspace).join("through-the-link").is_file());

    // Byte-identical on a second run.
    bootstrap(&workspace, "alpha").unwrap();
    assert_eq!(fs::read(workspace.join("AGENTS.md")).unwrap(), seeded);
    assert_eq!(
        fs::read(workspace.join("CLAUDE.md")).unwrap(),
        b"@AGENTS.md\n"
    );

    // Agent-owned files survive; the MarketRig-owned shim is reconciled.
    fs::write(workspace.join("AGENTS.md"), "# alpha\n\nmine now\n").unwrap();
    fs::write(workspace.join("CLAUDE.md"), "@SOMETHING-ELSE.md\n").unwrap();
    fs::write(&skill, "mine now\n").unwrap();
    bootstrap(&workspace, "alpha").unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        "# alpha\n\nmine now\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), "mine now\n");
}

#[cfg(test)]
#[test]
fn startup_reconciles_the_shim_and_the_link() {
    let (_dir, store, desks_home) = scratch();
    let alpha = create(&store, &desks_home, "alpha", "codex").unwrap();
    let workspace = desks_home.join("alpha");
    let link = skills_link(&workspace);

    // A missing link is recreated over a recreated empty tree, and nothing else:
    // startup never re-seeds the skill.
    fs::remove_dir_all(workspace.join(".claude")).unwrap();
    fs::remove_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join("CLAUDE.md"), "@SOMETHING-ELSE.md\n").unwrap();
    validate_ready(&store).unwrap();
    assert!(skills_dir(&workspace).is_dir());
    assert!(
        !skills_dir(&workspace)
            .join("desk-improvement")
            .join("SKILL.md")
            .exists()
    );
    assert!(resolves_to_skills(
        &workspace,
        &skills_link_target(&workspace).unwrap().unwrap()
    ));
    assert_eq!(
        fs::read_to_string(workspace.join("CLAUDE.md")).unwrap(),
        SHIM
    );

    // A link naming anything but this desk's tree is replaced. On macOS that is
    // a symlink pointing elsewhere; on Windows, where MarketRig only ever makes
    // a junction, it is a junction whose target has gone.
    let elsewhere = desks_home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    #[cfg(unix)]
    {
        remove_link(&link).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    }
    #[cfg(windows)]
    fs::remove_dir_all(skills_dir(&workspace)).unwrap();
    assert!(!resolves_to_skills(
        &workspace,
        &skills_link_target(&workspace).unwrap().unwrap()
    ));
    validate_ready(&store).unwrap();
    assert!(resolves_to_skills(
        &workspace,
        &skills_link_target(&workspace).unwrap().unwrap()
    ));
    assert!(elsewhere.is_dir(), "the old target is untouched");

    // An ordinary directory is left in place and named in the status reason.
    remove_link(&link).unwrap();
    fs::create_dir_all(&link).unwrap();
    fs::write(link.join("mine"), "x").unwrap();
    validate_ready(&store).unwrap();
    assert!(link.join("mine").is_file());
    assert!(skills_link_target(&workspace).unwrap().is_none());
    let row = get(&store, &alpha.id).unwrap();
    assert_eq!(row.workspace_status, Some("OK"));
    assert!(
        row.workspace_status_reason
            .as_deref()
            .is_some_and(|reason| reason.contains(".claude/skills") && reason.ends_with('.')),
        "{:?}",
        row.workspace_status_reason
    );
}

/// A desk created before R4 keeps its constitution and gains only the tree and
/// the link (R4 §5).
#[cfg(test)]
#[test]
fn pre_r4_desk_gains_only_the_tree_and_the_link() {
    let (_dir, store, desks_home) = scratch();
    let workspace = desks_home.join("legacy");
    fs::create_dir_all(&workspace).unwrap();
    let placeholder = "# legacy\n\nseeded before R4\n";
    fs::write(workspace.join("AGENTS.md"), placeholder).unwrap();
    fs::write(workspace.join("CLAUDE.md"), SHIM).unwrap();
    let path = workspace.to_str().unwrap().to_string();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES ('0199-legacy', 'legacy', 'READY', ?1, 1000, 1000)",
                [path],
            )
        })
        .unwrap();

    validate_ready(&store).unwrap();

    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        placeholder
    );
    assert!(resolves_to_skills(
        &workspace,
        &skills_link_target(&workspace).unwrap().unwrap()
    ));
    assert_eq!(
        fs::read_dir(skills_dir(&workspace)).unwrap().count(),
        0,
        "the tree is empty: the improvement skill is never seeded here"
    );
    assert_eq!(
        get(&store, "0199-legacy").unwrap().workspace_status,
        Some("OK")
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

    let alpha = create(&store, &desks_home, "alpha", "codex").unwrap();
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
        create(&store, &desks_home, "alpha", "codex")
            .unwrap_err()
            .code(),
        "DESK_NAME_TAKEN"
    );
    assert_eq!(
        create(&store, &desks_home, "Bad--Name", "codex")
            .unwrap_err()
            .code(),
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
    let beta = create(&store, &desks_home, "beta", "codex").unwrap();
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
