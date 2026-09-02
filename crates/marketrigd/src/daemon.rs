//! Daemon lifecycle: the single-instance lock, recovery, child reaping,
//! endpoint discovery, and shutdown's pointer removal.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §4 and §5.1,
//! root `sdd/SPEC.md` §15. Everything here is synchronous std code: the binary's
//! async glue calls [`start`], serves [`Startup::listener`], and calls
//! [`Startup::shutdown`] on the way out.

use std::fs::{self, File};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::{fmt, io};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::desk::{self, DeskError};
use crate::store::{Roots, Store, StoreError, now_ns};

const LOCK: &str = "daemon.lock";
const ENDPOINT: &str = "endpoint.json";
const CHILDREN: &str = "children.json";

/// A lifecycle failure carrying a stable SCREAMING_SNAKE code.
#[derive(Debug)]
pub enum DaemonError {
    /// Another daemon holds this data root's lock (R0-2).
    AlreadyRunning(PathBuf),
    Store(StoreError),
    Desk(DeskError),
    Io(io::Error),
}

impl DaemonError {
    pub fn code(&self) -> &'static str {
        match self {
            DaemonError::AlreadyRunning(_) => "ALREADY_RUNNING",
            DaemonError::Store(e) => e.code(),
            DaemonError::Desk(e) => e.code(),
            DaemonError::Io(_) => "INTERNAL",
        }
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::AlreadyRunning(path) => write!(
                f,
                "Another marketrigd already holds {}; one daemon per data root.",
                path.display()
            ),
            DaemonError::Store(e) => write!(f, "{e}"),
            DaemonError::Desk(e) => write!(f, "{e}"),
            DaemonError::Io(e) => write!(f, "Startup failed: {e}"),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<StoreError> for DaemonError {
    fn from(e: StoreError) -> Self {
        DaemonError::Store(e)
    }
}

impl From<DeskError> for DaemonError {
    fn from(e: DeskError) -> Self {
        DaemonError::Desk(e)
    }
}

impl From<io::Error> for DaemonError {
    fn from(e: io::Error) -> Self {
        DaemonError::Io(e)
    }
}

/// Everything a started daemon owns (§4.1). Holding it holds the lifetime lock,
/// so the glue keeps it alive until the process exits.
pub struct Startup {
    pub roots: Roots,
    pub store: Store,
    /// Bound on `127.0.0.1:0` and still blocking; the async glue sets
    /// `set_nonblocking(true)` before handing it to tokio.
    pub listener: TcpListener,
    pub port: u16,
    pub daemon_uuid: String,
    /// The per-start bearer, 32 CSPRNG bytes as 64 lowercase hex characters.
    /// Never log it (per D49, D51).
    pub credential: String,
    pub started_at_ns: i64,
    /// The lifetime lock (R0-2), released only when this process exits.
    _lock: File,
}

// Hand-written so a `{:?}` can never leak the credential into a log line
// (per D49, D51; `log::secret_free` greps for exactly that).
impl fmt::Debug for Startup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Startup")
            .field("roots", &self.roots)
            .field("port", &self.port)
            .field("daemon_uuid", &self.daemon_uuid)
            .field("credential", &"<redacted>")
            .field("started_at_ns", &self.started_at_ns)
            .finish_non_exhaustive()
    }
}

impl Startup {
    /// Shutdown's file half (§4.2): the daemon stops being discoverable. Draining
    /// the database thread is by construction — every [`Store`] call is
    /// synchronous, so no unit is in flight once the routes stop. Takes `&self`
    /// so the caller keeps holding the lock until the process actually exits.
    pub fn shutdown(&self) -> io::Result<()> {
        match fs::remove_file(endpoint_path(&self.roots)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

pub fn endpoint_path(roots: &Roots) -> PathBuf {
    roots.runtime().join(ENDPOINT)
}

pub fn children_path(roots: &Roots) -> PathBuf {
    roots.runtime().join(CHILDREN)
}

/// Per-desk Claude Code launch files (R3 feature SPEC §5.1), removed wholesale
/// at every startup.
pub fn launch_dir(roots: &Roots) -> PathBuf {
    roots.runtime().join("launch")
}

/// Startup steps 1–8 (§4.1), in order.
pub fn start(roots: Roots) -> Result<Startup, DaemonError> {
    // 1. roots are already resolved; create what is missing.
    roots.create_dirs()?;

    // 2. one daemon per data root, for this process's whole life (R0-2).
    let lock = acquire(&roots.runtime().join(LOCK))?;

    // 3. open the database, set WAL and foreign keys, apply migrations.
    let store = Store::open(&roots.database())?;

    // 4. the daemon's identity, minted before recovery because the RECOVERY
    //    payload names it (§3.3).
    let daemon_uuid = Uuid::now_v7().to_string();

    // 5. recovery: reap the predecessor's children, then one transaction that
    //    appends exactly one RECOVERY event (§4.3). Every record is dropped
    //    (§4.4), but only once the event evidencing the outcomes has committed.
    let children = reap(&children_path(&roots))?;
    recover(&store, &daemon_uuid, children)?;
    match fs::remove_file(children_path(&roots)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }

    // 6. finish every desk a crash left CREATING (§7.3).
    desk::complete_interrupted(&store)?;

    // 6a. discover every UNDISCOVERED runtime before the listener binds (R3
    //     feature SPEC §2), and drop the previous run's launch files, which
    //     name processes that no longer exist (R3 feature SPEC §8). Discovery
    //     is skipped under the test data root: the acceptance gate discovers
    //     its own stand-in explicitly and must not see this machine's real
    //     runtimes (root §17).
    match fs::remove_dir_all(launch_dir(&roots)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    if std::env::var_os(crate::store::TEST_DATA_ROOT_ENV).is_none() {
        crate::runtime::discover_undiscovered(&store)?;
    }

    // 7. bind and mint the per-start bearer.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let credential = mint_credential()?;
    let started_at_ns = now_ns();

    // 8. the daemon is now discoverable.
    write_endpoint(
        &endpoint_path(&roots),
        &Endpoint {
            port,
            credential: credential.clone(),
            daemon_uuid: daemon_uuid.clone(),
            pid: std::process::id(),
            started_at_ns,
        },
    )?;

    tracing::info!(
        daemon_uuid,
        port,
        pid = std::process::id(),
        "daemon started"
    );
    Ok(Startup {
        roots,
        store,
        listener,
        port,
        daemon_uuid,
        credential,
        started_at_ns,
        _lock: lock,
    })
}

/// Takes the lifetime lock (R0-2). The returned handle must outlive the daemon:
/// closing it releases the lock, which is exactly what process death does.
fn acquire(path: &Path) -> Result<File, DaemonError> {
    let file = File::create(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(DaemonError::AlreadyRunning(path.to_path_buf())),
        Err(fs::TryLockError::Error(e)) => Err(DaemonError::Io(e)),
    }
}

/// The one pre-service transaction (§4.3): reap the predecessor's children, then
/// settle the executions it left running (R2 feature SPEC §4.4), and append the
/// evidence for both.
pub(crate) fn recover(
    store: &Store,
    daemon_uuid: &str,
    children: Vec<Value>,
) -> Result<(), StoreError> {
    let uuid = daemon_uuid.to_string();
    store.unit(move |tx| {
        let previous: Option<String> = tx
            .query_row(
                "SELECT json_extract(payload, '$.daemon_uuid') FROM operational_events \
                 WHERE kind = 'RECOVERY' ORDER BY occurred_at_ns DESC, id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let at_ns = now_ns();
        let executions_lost = crate::exec::recovery_step(tx, &uuid, at_ns)?;
        let (sessions_lost, prompts_unknown) = crate::session::recovery_step(tx, &uuid, at_ns)?;
        desk::append_event(
            tx,
            "RECOVERY",
            None,
            at_ns,
            json!({
                "previous_daemon_uuid": previous,
                "daemon_uuid": uuid,
                "children": children,
                "executions_lost": executions_lost,
                "sessions_lost": sessions_lost,
                "prompts_unknown": prompts_unknown,
            }),
        )
    })
}

/// 32 bytes from the OS CSPRNG as 64 lowercase hex characters (§4.1).
fn mint_credential() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut hex, b| {
        use fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
        hex
    }))
}

/// `runtime/children.json` (§4.4), written by later milestones' launches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildRecord {
    pub pid: u32,
    pub kind: String,
    pub args: Vec<String>,
    pub daemon_uuid: String,
    pub launched_at_ns: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ChildrenFile {
    children: Vec<ChildRecord>,
}

/// Records one managed child in `runtime/children.json` (§4.4) so a crashed
/// daemon's successor can reap it. A write failure is a log line: the child is
/// already running and the daemon must not fail the launch over its bookkeeping.
pub fn record_child(roots: &Roots, record: ChildRecord) {
    let path = children_path(roots);
    let mut file: ChildrenFile = fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default();
    file.children.push(record);
    let written = serde_json::to_vec(&file)
        .map_err(io::Error::other)
        .and_then(|raw| fs::write(&path, raw));
    if let Err(e) = written {
        tracing::warn!(error = %e, "recording a managed child failed");
    }
}

/// Recovery's first step (§4.4, per D73): resolve a crashed daemon's recorded
/// children and report each outcome for the `RECOVERY` payload. A missing file
/// yields no outcomes. The file itself is removed by [`start`] only after the
/// recovery transaction commits, so a failed commit preserves the records as
/// evidence.
pub fn reap(path: &Path) -> io::Result<Vec<Value>> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let file: ChildrenFile = serde_json::from_slice(&raw).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "children.json is unreadable; dropping it unchecked");
        ChildrenFile::default()
    });

    let outcomes = file
        .children
        .iter()
        .map(|child| json!({ "pid": child.pid, "kind": child.kind, "outcome": classify(child) }))
        .collect();

    Ok(outcomes)
}

/// macOS: terminate a recorded child only when its pid is alive *and* its
/// current command line still carries the recorded args — a shim may have
/// replaced the executable path (per D73).
#[cfg(target_os = "macos")]
fn classify(child: &ChildRecord) -> &'static str {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    // No recorded args means no identity evidence, so the pid is never ours to kill.
    if child.args.is_empty() {
        return "PID_RECYCLED";
    }
    let pid = Pid::from_u32(child.pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let Some(process) = system.process(pid) else {
        return "NOT_RUNNING";
    };
    let cmd = process.cmd();
    if child
        .args
        .iter()
        .all(|arg| cmd.iter().any(|actual| actual == arg.as_str()))
    {
        process.kill();
        "TERMINATED"
    } else {
        "PID_RECYCLED"
    }
}

/// Windows: the Job Object already ended the children, so records are discarded
/// without a check (per D73).
#[cfg(not(target_os = "macos"))]
fn classify(_child: &ChildRecord) -> &'static str {
    "DISCARDED"
}

/// `runtime/endpoint.json` (§5.1): the discovery pointer, never proof of
/// liveness. No `Debug`: the credential must never reach a log line.
#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    credential: String,
    daemon_uuid: String,
    pid: u32,
    started_at_ns: i64,
}

/// Temp file plus rename in the same directory (§5.1), so a reader sees the
/// pointer whole or not at all. One fixed temp name is safe: the writer holds
/// the data root's lifetime lock.
fn write_endpoint(path: &Path, endpoint: &Endpoint) -> io::Result<()> {
    let temp = path.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    // Restrict before any secret reaches the file. Windows relies on the
    // per-user directory ACL instead (root §4.3).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec(endpoint)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)
}

#[cfg(test)]
fn scratch() -> (tempfile::TempDir, Roots) {
    let dir = tempfile::tempdir().unwrap();
    let roots = Roots::resolve(Some(dir.path())).unwrap();
    (dir, roots)
}

#[cfg(test)]
fn recovery_payloads(store: &Store) -> Vec<Value> {
    store
        .call(|c| {
            c.prepare(
                "SELECT payload FROM operational_events WHERE kind = 'RECOVERY' \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .iter()
        .map(|p| serde_json::from_str(p).unwrap())
        .collect()
}

#[cfg(test)]
#[test]
fn lock_excludes_second_start() {
    let (_dir, roots) = scratch();
    let first = start(roots.clone()).unwrap();

    let err = start(roots.clone()).unwrap_err();
    assert_eq!(err.code(), "ALREADY_RUNNING");

    // The second start wrote nothing: the first daemon still owns discovery.
    let published: Endpoint =
        serde_json::from_slice(&fs::read(endpoint_path(&roots)).unwrap()).unwrap();
    assert_eq!(published.daemon_uuid, first.daemon_uuid);

    // Closing the lock handle is what process exit does, and it is what makes
    // dropping `first` an honest stand-in for a dead daemon: prove both halves
    // of that on the bare file before relying on it.
    let path = roots.runtime().join(LOCK);
    let held = File::create(&path).unwrap();
    held.try_lock().unwrap_err(); // the live daemon's lock excludes us
    drop(held);
    drop(first);
    let free = File::create(&path).unwrap();
    free.try_lock().expect("close releases the lock");
    drop(free);

    // A restart records the predecessor and appends exactly one more RECOVERY.
    let second = start(roots.clone()).unwrap();
    let payloads = recovery_payloads(&second.store);
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0]["previous_daemon_uuid"], Value::Null);
    assert_eq!(payloads[0]["children"], json!([]));
    assert_eq!(
        payloads[1]["previous_daemon_uuid"],
        payloads[0]["daemon_uuid"]
    );
    assert_eq!(
        payloads[1]["daemon_uuid"].as_str().unwrap(),
        second.daemon_uuid
    );
}

#[cfg(test)]
#[test]
fn endpoint_write_atomic() {
    let (_dir, roots) = scratch();
    let startup = start(roots.clone()).unwrap();
    let path = endpoint_path(&roots);

    let published: Endpoint = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(published.port, startup.port);
    assert_eq!(
        published.port,
        startup.listener.local_addr().unwrap().port()
    );
    assert_eq!(published.credential, startup.credential);
    assert_eq!(published.credential.len(), 64);
    assert!(
        published
            .credential
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(published.daemon_uuid, startup.daemon_uuid);
    assert_eq!(published.pid, std::process::id());
    assert_eq!(published.started_at_ns, startup.started_at_ns);
    assert!(published.started_at_ns > 0);

    // Nothing partial survives: the runtime directory holds the lock and the
    // finished pointer, never a temp file.
    let mut names: Vec<String> = fs::read_dir(roots.runtime())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [LOCK, ENDPOINT]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "endpoint.json must be 0600");
    }

    // Rewriting over a live pointer still lands complete, never truncated.
    write_endpoint(&path, &published).unwrap();
    let again: Endpoint = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(again.credential, startup.credential);

    startup.shutdown().unwrap();
    assert!(!path.exists());
    startup.shutdown().expect("shutdown is idempotent");
}

#[cfg(test)]
#[test]
fn reap_missing_file_is_empty() {
    let (dir, _roots) = scratch();
    assert_eq!(
        reap(&dir.path().join("nothing.json")).unwrap(),
        Vec::<Value>::new()
    );
}

#[cfg(test)]
#[cfg(target_os = "macos")]
#[test]
fn reap_identity_check() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    let (dir, _roots) = scratch();
    let path = dir.path().join(CHILDREN);

    // (a) ours: a live sleeper whose recorded args are still on its command line.
    let mut matching = Command::new("/bin/sleep").arg("31517").spawn().unwrap();
    // (b) not ours: a live process whose recorded args do not match.
    let mut recycled = Command::new("/bin/sleep").arg("31518").spawn().unwrap();
    // (c) gone: a pid we owned and reaped, so nothing runs under it.
    let mut finished = Command::new("/bin/sleep").arg("0").spawn().unwrap();
    let dead_pid = finished.id();
    finished.wait().unwrap();

    let record = |pid: u32, args: &[&str]| ChildRecord {
        pid,
        kind: "TEST_SLEEPER".to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        daemon_uuid: "0199-previous".to_string(),
        launched_at_ns: 1000,
    };
    fs::write(
        &path,
        serde_json::to_vec(&ChildrenFile {
            children: vec![
                record(matching.id(), &["31517"]),
                record(recycled.id(), &["31519-not-my-argument"]),
                record(dead_pid, &["0"]),
            ],
        })
        .unwrap(),
    )
    .unwrap();

    let outcomes = reap(&path).unwrap();
    assert_eq!(outcomes[0]["pid"], matching.id());
    assert_eq!(outcomes[0]["outcome"], "TERMINATED");
    assert_eq!(outcomes[1]["pid"], recycled.id());
    assert_eq!(outcomes[1]["outcome"], "PID_RECYCLED");
    assert_eq!(outcomes[2]["pid"], dead_pid);
    assert_eq!(outcomes[2]["outcome"], "NOT_RUNNING");

    // (a) is actually dead, by signal.
    assert_eq!(matching.wait().unwrap().signal(), Some(9));
    // (b) is untouched — it is still running, so killing it is our cleanup.
    assert!(
        recycled.try_wait().unwrap().is_none(),
        "mismatched pid survives"
    );
    recycled.kill().unwrap();
    recycled.wait().unwrap();

    // reap classifies only; the records survive until the recovery
    // transaction commits (start() drops them — asserted in
    // reap_outcomes_reach_the_recovery_event).
    assert!(path.exists());
}

#[cfg(test)]
#[cfg(not(target_os = "macos"))]
#[test]
fn reap_identity_check() {
    let (dir, _roots) = scratch();
    let path = dir.path().join(CHILDREN);
    let record = |pid: u32| ChildRecord {
        pid,
        kind: "TEST_SLEEPER".to_string(),
        args: vec!["--marker".to_string()],
        daemon_uuid: "0199-previous".to_string(),
        launched_at_ns: 1000,
    };
    fs::write(
        &path,
        serde_json::to_vec(&ChildrenFile {
            children: vec![record(std::process::id()), record(4_294_967_294)],
        })
        .unwrap(),
    )
    .unwrap();

    // Windows discards without a check: the Job Object already ended them.
    let outcomes = reap(&path).unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| o["outcome"] == "DISCARDED"));
    // reap classifies only; start() drops the file after the recovery commit.
    assert!(path.exists());
}

#[cfg(test)]
#[test]
fn reap_outcomes_reach_the_recovery_event() {
    let (_dir, roots) = scratch();
    roots.create_dirs().unwrap();
    fs::write(
        children_path(&roots),
        serde_json::to_vec(&ChildrenFile {
            children: vec![ChildRecord {
                pid: 4_294_967_294,
                kind: "TEST_SLEEPER".to_string(),
                args: vec!["--marker".to_string()],
                daemon_uuid: "0199-previous".to_string(),
                launched_at_ns: 1000,
            }],
        })
        .unwrap(),
    )
    .unwrap();

    let startup = start(roots.clone()).unwrap();
    let payloads = recovery_payloads(&startup.store);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["children"].as_array().unwrap().len(), 1);
    assert_eq!(payloads[0]["children"][0]["pid"], 4_294_967_294u32);
    assert!(!children_path(&roots).exists());
}

#[cfg(test)]
#[test]
fn start_completes_interrupted_desks() {
    let (_dir, roots) = scratch();
    roots.create_dirs().unwrap();
    let workspace = roots.desks.join("gamma").to_str().unwrap().to_string();
    let store = Store::open(&roots.database()).unwrap();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns) \
                 VALUES ('0199-gamma', 'gamma', 'CREATING', ?1, 1000)",
                [workspace],
            )
        })
        .unwrap();
    drop(store);

    let startup = start(roots.clone()).unwrap();
    assert_eq!(
        desk::get(&startup.store, "0199-gamma").unwrap().state,
        "READY"
    );
    assert!(roots.desks.join("gamma/AGENTS.md").is_file());
}
