//! OpenViking: the installation row, its provisioning, the one child, and the
//! per-desk tenancy the plugins are configured with.
//!
//! Contract: `sdd/features/openviking-continuity/SPEC.md` §1 (setup, per OV-1),
//! §2 (the child, per OV-2), §3 (tenancy, per OV-3), §7.1 (the `standin` seam);
//! root `sdd/SPEC.md` §16.
//!
//! The provider row and the credential seam stay in [`crate::memory`]; this
//! module reads them and never writes a secret anywhere but that store and the
//! child's own environment.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::desk::append_event;
use crate::memory::{Memory, MemoryError, Provider};
use crate::store::{Roots, Store, StoreError, now_ns};

/// The one release the bundled wheel set is locked to (§1.3).
const OPENVIKING_VERSION: &str = "0.4.17.1";

/// The account and the admin user §3.1 names.
const ACCOUNT: &str = "marketrig";
const ADMIN_USER: &str = "marketrig-admin";

/// Everything OpenViking owns lives under `<data root>/openviking/` (§2.1, §2.2).
const HOME: &str = "openviking";

/// The child's readiness poll and its deadline (§2.2).
const READY_POLL: Duration = Duration::from_millis(500);
const READY_TIMEOUT: Duration = Duration::from_secs(1);
const READY_DEADLINE_MS: u64 = 120_000;

/// How often the supervisor looks for the child's exit (§2.3).
const WATCH_POLL: Duration = Duration::from_millis(250);

/// `SIGTERM`, then the group kill (§2.3).
const STOP_GRACE: Duration = Duration::from_secs(3);

/// The one 4 KiB in-memory tail of the child's output (§2.2).
const TAIL: usize = 4096;

/// Every admin call's own bound (§3.2).
const ADMIN_TIMEOUT: Duration = Duration::from_secs(15);

/// `ov.conf` exactly as §2.1 shows it. The two `${…}` references are
/// OpenViking's own `os.path.expandvars` layer, so no secret is ever in the
/// file; every substituted value arrives already JSON-quoted, which is what
/// keeps a Windows path's backslashes legal here.
const CONF_TEMPLATE: &str = r#"{
  "server": {"host": "127.0.0.1", "port": @PORT@, "root_api_key": "${MARKETRIG_OV_ROOT_KEY}"},
  "storage": {"workspace": @WORKSPACE@,
              "agfs": {"backend": "local"}, "vectordb": {"backend": "local"}},
  "embedding": {"dense": {"provider": "openai", "model": @EMBEDDING@, "input": "text",
                          "dimension": @DIMENSION@,
                          "api_key": "${MARKETRIG_OV_PROVIDER_KEY}", "api_base": @BASE@}},
  "vlm": {"provider": "openai", "model": @LLM@,
          "api_key": "${MARKETRIG_OV_PROVIDER_KEY}", "api_base": @BASE@},
  "memory": {"session_skill_extraction_enabled": false}
}
"#;

// ---------------------------------------------------------------------------
// Errors (§1.2)
// ---------------------------------------------------------------------------

/// A setup failure carrying a stable SCREAMING_SNAKE code. The `{found}` of
/// §1.2's two `*_UNSUPPORTED` codes is in the message: the one error envelope
/// carries a code and an English sentence and nothing else (root §6).
#[derive(Debug)]
pub enum SetupError {
    Validation(String),
    PythonUnsupported(String),
    PythonProbeFailed(String),
    NodeUnsupported(String),
    NodeProbeFailed(String),
    /// A second `PUT` while the first is still provisioning (§1.3).
    Busy,
    /// A retry with no environment to start (§2.3).
    Unconfigured,
    Error(String),
}

impl SetupError {
    pub fn code(&self) -> &'static str {
        match self {
            SetupError::Validation(_) => "VALIDATION",
            SetupError::PythonUnsupported(_) => "PYTHON_UNSUPPORTED",
            SetupError::PythonProbeFailed(_) => "PYTHON_PROBE_FAILED",
            SetupError::NodeUnsupported(_) => "NODE_UNSUPPORTED",
            SetupError::NodeProbeFailed(_) => "NODE_PROBE_FAILED",
            SetupError::Busy => "SETUP_BUSY",
            SetupError::Unconfigured => "OPENVIKING_UNCONFIGURED",
            SetupError::Error(_) => "OPENVIKING_ERROR",
        }
    }
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupError::Validation(m)
            | SetupError::PythonUnsupported(m)
            | SetupError::PythonProbeFailed(m)
            | SetupError::NodeUnsupported(m)
            | SetupError::NodeProbeFailed(m)
            | SetupError::Error(m) => write!(f, "{m}"),
            SetupError::Busy => write!(f, "OpenViking is already being set up."),
            SetupError::Unconfigured => {
                write!(f, "OpenViking has no environment: set it up first.")
            }
        }
    }
}

impl std::error::Error for SetupError {}

impl From<StoreError> for SetupError {
    fn from(e: StoreError) -> Self {
        SetupError::Error(e.to_string())
    }
}

impl From<MemoryError> for SetupError {
    fn from(e: MemoryError) -> Self {
        SetupError::Error(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The row and the live state (§1.1)
// ---------------------------------------------------------------------------

/// The `openviking_setup` row, secrets-free by construction. The schema names
/// are qualified: `components/schemas` is one namespace for the whole document.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = OpenVikingSetup)]
pub struct Setup {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venv_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisioned_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
}

const SETUP_SELECT: &str = "SELECT state, python_path, python_version, node_path, node_version, \
                            venv_path, provisioned_at_ns, failure_code, failure_message \
                            FROM openviking_setup WHERE id = 1";

fn read_setup(row: &rusqlite::Row<'_>) -> rusqlite::Result<Setup> {
    Ok(Setup {
        state: row.get(0)?,
        python_path: row.get(1)?,
        python_version: row.get(2)?,
        node_path: row.get(3)?,
        node_version: row.get(4)?,
        venv_path: row.get(5)?,
        provisioned_at_ns: row.get(6)?,
        failure_code: row.get(7)?,
        failure_message: row.get(8)?,
    })
}

pub fn setup_row(store: &Store) -> Result<Setup, StoreError> {
    store.call(|c| c.query_row(SETUP_SELECT, [], read_setup))
}

/// The child's liveness (§1.1, §6): memory only, `NOT_STARTED` after every start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[schema(as = OpenVikingChildState)]
pub enum LiveState {
    #[default]
    NotStarted,
    Starting,
    Ready,
    Lost,
}

/// `GET /openviking` (§1.1).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = OpenVikingStatus)]
pub struct Status {
    pub setup: Setup,
    pub child: LiveState,
    /// One entry per `READY` desk: whether its OpenViking user is provisioned.
    pub desks: BTreeMap<String, bool>,
}

/// Everything about the live child, behind one lock. Nothing here is durable:
/// the root key and the desk keys are memory only (§3.1).
#[derive(Default)]
struct Live {
    state: LiveState,
    pid: Option<u32>,
    child: Option<crate::exec::Contained>,
    port: Option<u16>,
    root_key: Option<String>,
    /// Both streams, newest [`TAIL`] bytes, raw and never parsed (§2.2).
    output_tail: Vec<u8>,
    /// Desk id to that desk's user key (§3.2), dropped on a loss.
    keys: BTreeMap<String, String>,
    /// Bumped by every start and every stop, so a supervisor whose child is
    /// already gone cannot report a loss against the one that replaced it.
    generation: u64,
}

impl Live {
    fn push_output(&mut self, bytes: &[u8]) {
        self.output_tail.extend_from_slice(bytes);
        if self.output_tail.len() > TAIL {
            self.output_tail
                .drain(..self.output_tail.len() - TAIL)
                .for_each(drop);
        }
    }
}

/// The OpenViking subsystem: the setup row, the one child, and the desk keys.
/// One per daemon, in `ApiState`.
pub struct OpenViking {
    pub store: Store,
    pub roots: Roots,
    /// The provider row and the credential seam (§2.1, §3.1).
    pub memory: Arc<Memory>,
    pub http: reqwest::Client,
    daemon_uuid: String,
    /// True under `MARKETRIG_TEST_DATA_ROOT`: the `standin` seam of §7.1 is open.
    seam: bool,
    live: Mutex<Live>,
    /// `DESK_MEMORY_PROVISIONED` is appended once per daemon start per desk
    /// (§3.2), so this outlives a child restart, where [`Live`] does not.
    announced: Mutex<BTreeSet<String>>,
    /// §2.2's readiness deadline in milliseconds; a check shortens it so a
    /// deadline miss costs a second rather than two minutes.
    ready_deadline_ms: AtomicU64,
    /// Notified at every child readiness. The skills projection (§5.2) is the
    /// waiter C55 adds.
    pub ready: Arc<tokio::sync::Notify>,
}

impl OpenViking {
    pub fn new(memory: Arc<Memory>, daemon_uuid: String) -> Arc<OpenViking> {
        Arc::new(OpenViking {
            store: memory.store.clone(),
            roots: memory.roots.clone(),
            http: memory.http.clone(),
            seam: memory.seam,
            memory,
            daemon_uuid,
            live: Mutex::new(Live::default()),
            announced: Mutex::new(BTreeSet::new()),
            ready_deadline_ms: AtomicU64::new(READY_DEADLINE_MS),
            ready: Arc::new(tokio::sync::Notify::new()),
        })
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The live child's state (§1.1). C54's launch reads it to decide between
    /// `OPENVIKING_MEMORY_ENABLED` `1` and `0`.
    pub fn child_state(&self) -> LiveState {
        self.live().state
    }

    /// The port the child answers on, `None` until it has one (§4.3).
    pub fn port(&self) -> Option<u16> {
        self.live().port
    }

    /// One desk's OpenViking key, held in memory only (§3.2). `None` until the
    /// desk's user is provisioned, and again after a loss.
    pub fn desk_key(&self, desk_id: &str) -> Option<String> {
        self.live().keys.get(desk_id).cloned()
    }

    /// One desk's OpenViking user: `desk-` and the desk UUID's 32 lowercase hex
    /// characters, computed per request and stored nowhere (§3.1).
    pub fn desk_user(desk_id: &str) -> String {
        format!("desk-{}", desk_id.replace('-', "").to_ascii_lowercase())
    }

    /// `GET /openviking` (§1.1).
    pub fn status(&self) -> Result<Status, SetupError> {
        let ready = ready_desks(&self.store)?;
        let desks = {
            let live = self.live();
            ready
                .into_iter()
                .map(|id| {
                    let provisioned = live.keys.contains_key(&id);
                    (id, provisioned)
                })
                .collect()
        };
        Ok(Status {
            setup: setup_row(&self.store)?,
            child: self.child_state(),
            desks,
        })
    }
}

/// Every `READY` desk, in creation order (§3.2).
fn ready_desks(store: &Store) -> Result<Vec<String>, StoreError> {
    store.call(|c| {
        c.prepare("SELECT id FROM desks WHERE state = 'READY' ORDER BY created_at_ns, id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    })
}

// ---------------------------------------------------------------------------
// Setup: validation and provisioning (§1.2, §1.3)
// ---------------------------------------------------------------------------

/// `PUT /openviking/setup`'s body.
#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub python: String,
    pub node: String,
    /// The wheel directory; absent means the one beside the daemon (§1.3).
    #[serde(default)]
    pub wheels: Option<String>,
    /// The acceptance seam (§7.1): honored only under `MARKETRIG_TEST_DATA_ROOT`.
    #[serde(default)]
    pub standin: Option<String>,
}

fn absolute(field: &str, raw: &str) -> Result<PathBuf, SetupError> {
    let path = PathBuf::from(raw.trim());
    if raw.trim().is_empty() || !path.is_absolute() {
        return Err(SetupError::Validation(format!(
            "{field} must be an absolute path."
        )));
    }
    Ok(path)
}

/// `<python> -c "…"` must print exactly `3.12` (§1.2).
fn probe_python(python: &Path) -> Result<String, SetupError> {
    let script = "import sys;print('%d.%d'%sys.version_info[:2])";
    let Some((ok, out, err)) = crate::runtime::run(crate::runtime::probe(python, &["-c", script]))
    else {
        return Err(SetupError::PythonProbeFailed(format!(
            "{} did not answer.",
            python.display()
        )));
    };
    let found = out.trim().lines().next_back().unwrap_or_default().trim();
    if !ok || found.is_empty() {
        return Err(SetupError::PythonProbeFailed(format!(
            "{} did not report a version. {}",
            python.display(),
            first_line(if err.trim().is_empty() { &out } else { &err })
        )));
    }
    if found != "3.12" {
        return Err(SetupError::PythonUnsupported(format!(
            "{} is Python {found}; MarketRig needs exactly 3.12.",
            python.display()
        )));
    }
    Ok(found.to_string())
}

/// `<node> --version` must print `v<major>.…` with `major >= 22` (§1.2).
fn probe_node(node: &Path) -> Result<String, SetupError> {
    let Some((ok, out, err)) = crate::runtime::run(crate::runtime::probe(node, &["--version"]))
    else {
        return Err(SetupError::NodeProbeFailed(format!(
            "{} did not answer.",
            node.display()
        )));
    };
    let found = out.trim().lines().next_back().unwrap_or_default().trim();
    let major = found
        .strip_prefix('v')
        .and_then(|rest| rest.split('.').next())
        .and_then(|major| major.parse::<u32>().ok());
    let Some(major) = major.filter(|_| ok) else {
        return Err(SetupError::NodeProbeFailed(format!(
            "{} did not report a version. {}",
            node.display(),
            first_line(if err.trim().is_empty() { &out } else { &err })
        )));
    };
    if major < 22 {
        return Err(SetupError::NodeUnsupported(format!(
            "{} is Node {found}; MarketRig needs 22 or newer.",
            node.display()
        )));
    }
    Ok(found.to_string())
}

/// The locked wheel set the release unit carries beside the daemon (§1.3), with
/// the macOS bundle's `Contents/Resources/` as the second place to look.
fn default_wheels() -> PathBuf {
    let platform = if cfg!(windows) {
        "windows-x64"
    } else {
        "macos-arm64"
    };
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let beside = dir.join("openviking-wheels").join(platform);
    if beside.is_dir() || !cfg!(target_os = "macos") {
        return beside;
    }
    let bundled = dir.join("../Resources/openviking-wheels").join(platform);
    if bundled.is_dir() { bundled } else { beside }
}

/// The private environment (§1.3).
fn venv_dir(roots: &Roots) -> PathBuf {
    roots.data.join(HOME).join("venv")
}

/// One executable inside the environment. On Windows a `.cmd` beside it is
/// taken when no `.exe` is there.
///
/// ponytail: the `.cmd` fallback is what lets a fake interpreter's `venv` write
/// a runnable console script in a check; a real `pip install` writes the `.exe`.
fn venv_exe(venv: &Path, name: &str) -> PathBuf {
    let bin = venv.join(if cfg!(windows) { "Scripts" } else { "bin" });
    let exe = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let batch = bin.join(format!("{name}.cmd"));
    if cfg!(windows) && !exe.is_file() && batch.is_file() {
        return batch;
    }
    exe
}

/// The executable one start spawns (§2.2, §7.1): the venv's console script, or
/// the registered stand-in, which the row carries as `python_path` behind an
/// empty `venv_path`.
fn child_executable(row: &Setup) -> Option<PathBuf> {
    match row.venv_path.as_deref() {
        Some("") => row.python_path.clone().map(PathBuf::from),
        Some(venv) => Some(venv_exe(Path::new(venv), "openviking-server")),
        None => None,
    }
}

/// One provisioning command, run to completion with both streams captured.
/// There is no timeout: `--no-index` keeps every step off the network, so a step
/// is bounded by local disk.
///
/// ponytail: no bound at all; a per-step deadline arrives if a real machine
/// wedges inside `venv` or `pip`.
fn output(mut command: std::process::Command) -> (bool, String) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::runtime::CREATE_NO_WINDOW);
    }
    command.stdin(Stdio::null());
    match command.output() {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), text)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// The last non-empty line of a captured stream, which is what the row and the
/// events carry (§1.3, §2.3).
fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// §1.3, in order, stopping at the first failure with its last output line.
fn run_steps(python: &Path, venv: &Path, wheels: &Path) -> Result<(), String> {
    let _ = fs::remove_dir_all(venv);
    let mut create = crate::runtime::probe(python, &["-m", "venv"]);
    create.arg(venv);
    let (ok, out) = output(create);
    if !ok {
        return Err(last_line(&out));
    }
    let mut install = crate::runtime::probe(
        &venv_exe(venv, "python"),
        &["-m", "pip", "install", "--no-index", "--find-links"],
    );
    install.arg(wheels);
    install.arg(format!("openviking=={OPENVIKING_VERSION}"));
    let (ok, out) = output(install);
    if !ok {
        return Err(last_line(&out));
    }
    let (ok, out) = output(crate::runtime::probe(
        &venv_exe(venv, "openviking-server"),
        &["--version"],
    ));
    if !ok {
        return Err(last_line(&out));
    }
    Ok(())
}

/// 32 random bytes as hex: the seed and every per-start root key (§2.2, §3.1).
fn hex32() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().fold(String::new(), |mut hex, byte| {
        use fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

impl OpenViking {
    /// `PUT /openviking/setup` (§1.2, §1.3): validate, claim `PROVISIONING`,
    /// answer, and provision on one task. Under the seam a `standin` skips both
    /// validation and provisioning (§7.1).
    pub async fn setup(self: &Arc<Self>, request: SetupRequest) -> Result<Setup, SetupError> {
        if setup_row(&self.store)?.state == "PROVISIONING" {
            return Err(SetupError::Busy);
        }
        if let Some(standin) = request.standin.filter(|_| self.seam) {
            let standin = absolute("standin", &standin)?;
            let row = self.land_standin(&standin.to_string_lossy(), request.node.trim())?;
            self.ensure_seed()?;
            self.announce_provisioned(None, None);
            let this = self.clone();
            tokio::spawn(async move {
                this.stop_child().await;
                this.start().await;
            });
            return Ok(row);
        }

        let python = absolute("python", &request.python)?;
        let node = absolute("node", &request.node)?;
        let (python_version, node_version) = tokio::task::spawn_blocking(move || {
            Ok::<_, SetupError>((probe_python(&python)?, probe_node(&node)?))
        })
        .await
        .map_err(|e| SetupError::Error(e.to_string()))??;

        let wheels = match &request.wheels {
            Some(raw) => absolute("wheels", raw)?,
            None => default_wheels(),
        };
        let python = request.python.trim().to_string();
        let claimed = self.store.unit({
            let (python, node, py_v, node_v) = (
                python.clone(),
                request.node.trim().to_string(),
                python_version.clone(),
                node_version.clone(),
            );
            move |tx| {
                tx.execute(
                    "UPDATE openviking_setup SET state = 'PROVISIONING', python_path = ?1, \
                     python_version = ?2, node_path = ?3, node_version = ?4, \
                     failure_code = NULL, failure_message = NULL WHERE id = 1",
                    params![python, py_v, node, node_v],
                )?;
                tx.query_row(SETUP_SELECT, [], read_setup)
            }
        })?;

        let this = self.clone();
        tokio::spawn(async move {
            this.provision(PathBuf::from(python), wheels, python_version, node_version)
                .await;
        });
        Ok(claimed)
    }

    /// §1.3's command sequence, then the row and the child.
    async fn provision(
        self: &Arc<Self>,
        python: PathBuf,
        wheels: PathBuf,
        python_version: String,
        node_version: String,
    ) {
        // A live child holds the environment being replaced (§1.3).
        self.stop_child().await;
        let venv = venv_dir(&self.roots);
        let steps = {
            let venv = venv.clone();
            tokio::task::spawn_blocking(move || run_steps(&python, &venv, &wheels)).await
        };
        match steps.unwrap_or_else(|e| Err(e.to_string())) {
            Ok(()) => {
                // The seed is created once and never rotated (§1.3, §3.1).
                if let Err(e) = self.ensure_seed() {
                    tracing::warn!(error = %e, "storing the OpenViking seed failed");
                }
                if let Err(e) = self.land_available(&venv.to_string_lossy()) {
                    tracing::error!(error = %e, "recording the provisioned environment failed");
                    return;
                }
                self.announce_provisioned(Some(&python_version), Some(&node_version));
                self.start().await;
            }
            Err(message) => {
                let message = self.memory.redact(&message);
                let failed = self.store.unit(move |tx| {
                    tx.execute(
                        "UPDATE openviking_setup SET state = 'UNCONFIGURED', venv_path = NULL, \
                         failure_code = 'PROVISION_FAILED', failure_message = ?1 WHERE id = 1",
                        params![message],
                    )
                });
                if let Err(e) = failed {
                    tracing::error!(error = %e, "recording PROVISION_FAILED failed");
                }
            }
        }
    }

    /// The stand-in (§7.1): `AVAILABLE` at once, `venv_path` empty, and the
    /// executable in `python_path`, which is where [`child_executable`] looks
    /// when there is no environment to look in.
    fn land_standin(&self, standin: &str, node: &str) -> Result<Setup, StoreError> {
        let at_ns = now_ns();
        let (standin, node) = (standin.to_string(), node.to_string());
        self.store.unit(move |tx| {
            tx.execute(
                "UPDATE openviking_setup SET state = 'AVAILABLE', python_path = ?1, \
                 python_version = NULL, node_path = ?2, node_version = NULL, venv_path = '', \
                 provisioned_at_ns = ?3, failure_code = NULL, failure_message = NULL WHERE id = 1",
                params![standin, node, at_ns],
            )?;
            tx.query_row(SETUP_SELECT, [], read_setup)
        })
    }

    /// The completed environment (§1.3). The paths and versions were written
    /// when the row was claimed.
    fn land_available(&self, venv: &str) -> Result<Setup, StoreError> {
        let (at_ns, venv) = (now_ns(), venv.to_string());
        self.store.unit(move |tx| {
            tx.execute(
                "UPDATE openviking_setup SET state = 'AVAILABLE', venv_path = ?1, \
                 provisioned_at_ns = ?2, failure_code = NULL, failure_message = NULL WHERE id = 1",
                params![venv, at_ns],
            )?;
            tx.query_row(SETUP_SELECT, [], read_setup)
        })
    }

    fn announce_provisioned(&self, python_version: Option<&str>, node_version: Option<&str>) {
        let at_ns = now_ns();
        let payload = json!({
            "python_version": python_version,
            "node_version": node_version,
        });
        let recorded = self
            .store
            .unit(move |tx| append_event(tx, "OPENVIKING_PROVISIONED", None, at_ns, payload));
        if let Err(e) = recorded {
            tracing::warn!(error = %e, "recording OPENVIKING_PROVISIONED failed");
        }
    }

    /// The one installation secret desk keys derive from (§1.3, §3.1): created
    /// on the first successful provisioning and never rotated.
    fn ensure_seed(&self) -> Result<String, MemoryError> {
        if let Some(seed) = self.memory.load_secret(crate::memory::SEED_ACCOUNT)? {
            return Ok(seed);
        }
        let seed = hex32().map_err(MemoryError::Error)?;
        self.memory
            .store_secret(crate::memory::SEED_ACCOUNT, &seed)?;
        Ok(seed)
    }
}

// ---------------------------------------------------------------------------
// The configuration file (§2.1)
// ---------------------------------------------------------------------------

/// §2.1's file, rendered from the provider row. Both secrets are `${…}`
/// references OpenViking's loader expands from the child's environment.
pub fn render_conf(port: u16, workspace: &Path, provider: &Provider) -> String {
    let quoted = |raw: &str| Value::String(raw.to_string()).to_string();
    CONF_TEMPLATE
        .replace("@PORT@", &port.to_string())
        .replace("@WORKSPACE@", &quoted(&workspace.to_string_lossy()))
        .replace(
            "@EMBEDDING@",
            &quoted(provider.embedding_model.as_deref().unwrap_or_default()),
        )
        .replace(
            "@DIMENSION@",
            &provider.embedding_dimension.unwrap_or_default().to_string(),
        )
        .replace(
            "@LLM@",
            &quoted(provider.llm_model.as_deref().unwrap_or_default()),
        )
        .replace(
            "@BASE@",
            &quoted(provider.base_url.as_deref().unwrap_or_default()),
        )
}

/// Writes `ov.conf` 0600 before every start (§2.1). Windows relies on the
/// per-user directory ACL instead, as the credential seam does.
fn write_conf(path: &Path, body: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(body.as_bytes())?;
    file.sync_all()
}

// ---------------------------------------------------------------------------
// The child (§2.2, §2.3)
// ---------------------------------------------------------------------------

/// The variables the child's own `HOME` replaces, whatever case the daemon's
/// environment spells them in.
fn redirected(key: &str) -> bool {
    ["HOME", "USERPROFILE", "LOCALAPPDATA"]
        .iter()
        .any(|name| key.eq_ignore_ascii_case(name))
}

/// Everything one start needs, read before the live state is claimed.
struct Launch {
    executable: PathBuf,
    port: u16,
    root_key: String,
    env: Vec<(String, String)>,
    cwd: PathBuf,
    conf: PathBuf,
}

impl OpenViking {
    /// §2.2's environment: R3 §4.2's platform set with the child's own `HOME`,
    /// the daemon's `MARKETRIG_*` seam, and the four OpenViking variables.
    fn child_env(
        home: &Path,
        conf: &Path,
        root_key: &str,
        provider_key: &str,
    ) -> Vec<(String, String)> {
        let home = home.to_string_lossy().into_owned();
        let mut env: Vec<(String, String)> =
            crate::session::platform_env(&crate::runtime::search_path())
                .into_iter()
                .filter(|(key, _)| !redirected(key))
                .collect();
        #[cfg(windows)]
        {
            env.push(("USERPROFILE".to_string(), home.clone()));
            env.push(("LOCALAPPDATA".to_string(), home.clone()));
        }
        env.push(("HOME".to_string(), home));
        env.extend(std::env::vars().filter(|(key, _)| key.starts_with("MARKETRIG_")));
        env.extend([
            // Off a terminal Python encodes its output in the system code page,
            // and a GBK Windows box died on the first non-ASCII glyph (the R4
            // Windows cell, 2026-09-04). UTF-8 mode on both platforms.
            ("PYTHONUTF8".to_string(), "1".to_string()),
            (
                "OPENVIKING_CONFIG_FILE".to_string(),
                conf.to_string_lossy().into_owned(),
            ),
            ("MARKETRIG_OV_ROOT_KEY".to_string(), root_key.to_string()),
            (
                "MARKETRIG_OV_PROVIDER_KEY".to_string(),
                provider_key.to_string(),
            ),
        ]);
        env
    }

    /// The rows decide before anything is spawned (§2.2): a provisioned
    /// environment and a complete provider, or no start at all.
    fn plan(&self) -> Result<Option<Launch>, String> {
        let row = setup_row(&self.store).map_err(|e| e.to_string())?;
        if !matches!(row.state.as_str(), "AVAILABLE" | "UNAVAILABLE") {
            return Ok(None);
        }
        let provider = crate::memory::provider_row(&self.store).map_err(|e| e.to_string())?;
        if !provider.complete() {
            return Ok(None);
        }
        let Some(executable) = child_executable(&row) else {
            return Ok(None);
        };
        let provider_key = self
            .memory
            .load_key()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let cwd = self.roots.data.join(HOME);
        let home = cwd.join("home");
        let workspace = cwd.join("data");
        for dir in [&cwd, &home, &workspace] {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let port = crate::codex::free_port()?;
        let root_key = hex32()?;
        self.memory.hold_secret(&root_key);
        let conf = cwd.join("ov.conf");
        write_conf(&conf, &render_conf(port, &workspace, &provider)).map_err(|e| e.to_string())?;
        let env = Self::child_env(&home, &conf, &root_key, &provider_key);
        Ok(Some(Launch {
            executable,
            port,
            root_key,
            env,
            cwd,
            conf,
        }))
    }

    /// §2.2: start the child when the rows allow it, and return at once —
    /// readiness is a task of its own, so desks, triggers, and trading are
    /// served while the child is `STARTING`.
    pub async fn start(self: &Arc<Self>) {
        if matches!(self.child_state(), LiveState::Starting | LiveState::Ready) {
            return;
        }
        let launch = match self.plan() {
            Ok(Some(launch)) => launch,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "the OpenViking child could not be planned");
                return;
            }
        };
        let generation = {
            let mut live = self.live();
            if matches!(live.state, LiveState::Starting | LiveState::Ready) {
                return;
            }
            live.generation += 1;
            live.state = LiveState::Starting;
            live.port = Some(launch.port);
            live.root_key = Some(launch.root_key.clone());
            live.output_tail.clear();
            live.generation
        };
        if let Err(e) = self.spawn(launch, generation).await {
            tracing::warn!(error = %e, "the OpenViking child could not be started");
        }
    }

    async fn spawn(self: &Arc<Self>, launch: Launch, generation: u64) -> Result<(), String> {
        let mut command = tokio::process::Command::new(&launch.executable);
        command.args([
            "--config".as_ref(),
            launch.conf.as_os_str(),
            "--host".as_ref(),
            "127.0.0.1".as_ref(),
            "--port".as_ref(),
            launch.port.to_string().as_ref(),
        ]);
        command
            .current_dir(&launch.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env_clear();
        for (key, value) in &launch.env {
            command.env(key, value);
        }
        let mut child = match crate::exec::spawn(command) {
            Ok(child) => child,
            Err(e) => {
                let mut live = self.live();
                if live.generation == generation {
                    *live = Live {
                        generation,
                        ..Live::default()
                    };
                }
                return Err(e.to_string());
            }
        };
        let pid = child.id().unwrap_or_default();
        let stdout = child.take_stdout();
        let stderr = child.take_stderr();
        // A stop that landed while the spawn was in flight ends this start
        // before it is recorded or supervised, so nothing is orphaned.
        let mut orphan = None;
        {
            let mut live = self.live();
            if live.generation != generation {
                orphan = Some(child);
            } else {
                live.pid = Some(pid);
                live.child = Some(child);
            }
        }
        if let Some(mut child) = orphan {
            child.terminate_gracefully(STOP_GRACE).await;
            return Ok(());
        }
        crate::daemon::record_child(
            &self.roots,
            crate::daemon::ChildRecord {
                pid,
                kind: "openviking".to_string(),
                args: vec![launch.executable.to_string_lossy().into_owned()],
                daemon_uuid: self.daemon_uuid.clone(),
                launched_at_ns: now_ns(),
            },
        );
        if let Some(reader) = stdout {
            tokio::spawn(drain(self.clone(), generation, reader));
        }
        if let Some(reader) = stderr {
            tokio::spawn(drain(self.clone(), generation, reader));
        }
        tokio::spawn(supervise(self.clone(), generation));
        tokio::spawn(await_ready(self.clone(), generation, launch.port));
        Ok(())
    }

    /// `POST /openviking/retry` (§2.3): one more start, whatever is live now.
    pub async fn retry(self: &Arc<Self>) -> Result<Status, SetupError> {
        let row = setup_row(&self.store)?;
        if !matches!(row.state.as_str(), "AVAILABLE" | "UNAVAILABLE") {
            return Err(SetupError::Unconfigured);
        }
        self.stop_child().await;
        self.start().await;
        self.status()
    }

    /// §2.3: stop a live child. Reprovisioning, Retry, and Quit all call it;
    /// ending the generation is what keeps the supervisor from calling this a
    /// loss.
    pub async fn stop_child(&self) {
        let (child, pid) = {
            let mut live = self.live();
            if live.state == LiveState::NotStarted && live.child.is_none() {
                return;
            }
            let generation = live.generation + 1;
            let child = live.child.take();
            let pid = live.pid;
            *live = Live {
                generation,
                ..Live::default()
            };
            (child, pid)
        };
        if let Some(mut child) = child {
            child.terminate_gracefully(STOP_GRACE).await;
        }
        if let Some(pid) = pid {
            crate::daemon::forget_child(&self.roots, pid);
        }
    }

    fn ready_deadline(&self) -> Duration {
        Duration::from_millis(self.ready_deadline_ms.load(Ordering::Relaxed))
    }
}

/// One stream into the tail (§2.2).
async fn drain(
    context: Arc<OpenViking>,
    generation: u64,
    mut reader: impl tokio::io::AsyncRead + Unpin,
) {
    let mut buffer = [0u8; 1024];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let mut live = context.live();
                if live.generation != generation {
                    return;
                }
                live.push_output(&buffer[..read]);
            }
        }
    }
}

/// The child's exit is a loss (§2.3).
///
/// ponytail: a quarter-second poll rather than an owned `wait()`, so the handle
/// stays in [`Live`] where [`OpenViking::stop_child`] reaches it.
async fn supervise(context: Arc<OpenViking>, generation: u64) {
    loop {
        tokio::time::sleep(WATCH_POLL).await;
        let exit_code = {
            let mut live = context.live();
            if live.generation != generation {
                return;
            }
            match live.child.as_mut().map(crate::exec::Contained::try_wait) {
                Some(Ok(Some(status))) => status.code().map(i64::from),
                Some(Ok(None)) => continue,
                // A wait that fails is still an ended attempt; a taken handle
                // means the stop already happened.
                Some(Err(_)) => None,
                None => return,
            }
        };
        context.lose(generation, exit_code).await;
        return;
    }
}

/// `GET /ready` until `200`, the deadline, or the child's end (§2.2).
async fn await_ready(context: Arc<OpenViking>, generation: u64, port: u16) {
    let ready = format!("http://127.0.0.1:{port}/ready");
    let deadline = tokio::time::Instant::now() + context.ready_deadline();
    loop {
        {
            let live = context.live();
            if live.generation != generation || live.state != LiveState::Starting {
                return;
            }
        }
        let answered = matches!(
            context.http.get(&ready).timeout(READY_TIMEOUT).send().await,
            Ok(response) if response.status() == reqwest::StatusCode::OK
        );
        if answered {
            {
                let mut live = context.live();
                if live.generation != generation || live.state != LiveState::Starting {
                    return;
                }
                live.state = LiveState::Ready;
            }
            let at_ns = now_ns();
            let recorded = context.store.unit(move |tx| {
                // A Retry that reaches readiness clears the failure (§2.3).
                tx.execute(
                    "UPDATE openviking_setup SET state = 'AVAILABLE', failure_code = NULL, \
                     failure_message = NULL WHERE id = 1 AND state = 'UNAVAILABLE'",
                    [],
                )?;
                append_event(
                    tx,
                    "OPENVIKING_STARTED",
                    None,
                    at_ns,
                    json!({ "port": port }),
                )
            });
            if let Err(e) = recorded {
                tracing::warn!(error = %e, "recording OPENVIKING_STARTED failed");
            }
            context.provision_desks().await;
            context.ready.notify_waiters();
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            context.lose(generation, None).await;
            return;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

impl OpenViking {
    /// §2.3: the attempt is over. There is no automatic restart.
    async fn lose(self: &Arc<Self>, generation: u64, exit_code: Option<i64>) {
        let (child, pid, last) = {
            let mut live = self.live();
            if live.generation != generation
                || !matches!(live.state, LiveState::Starting | LiveState::Ready)
            {
                return;
            }
            let pid = live.pid.unwrap_or_default();
            let last = last_line(&String::from_utf8_lossy(&live.output_tail));
            let child = live.child.take();
            let output_tail = std::mem::take(&mut live.output_tail);
            *live = Live {
                state: LiveState::Lost,
                output_tail,
                generation,
                ..Live::default()
            };
            (child, pid, last)
        };
        // Whatever the child printed is redacted before it is stored or
        // published (§9 check 10).
        let last = self.memory.redact(&last);
        crate::daemon::forget_child(&self.roots, pid);
        let at_ns = now_ns();
        let recorded = self.store.unit(move |tx| {
            append_event(
                tx,
                "OPENVIKING_LOST",
                None,
                at_ns,
                json!({ "pid": pid, "exit_code": exit_code, "output_tail_last_line": &last }),
            )?;
            tx.execute(
                "UPDATE openviking_setup SET state = 'UNAVAILABLE', \
                 failure_code = 'CHILD_FAILED', failure_message = ?1 WHERE id = 1",
                params![last],
            )?;
            append_event(
                tx,
                "OPENVIKING_UNAVAILABLE",
                None,
                at_ns,
                json!({ "failure_code": "CHILD_FAILED", "failure_message": &last }),
            )
        });
        if let Err(e) = recorded {
            tracing::error!(error = %e, "recording the OpenViking child's loss failed");
        }
        if let Some(mut child) = child {
            child.terminate_gracefully(STOP_GRACE).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tenancy (§3.2)
// ---------------------------------------------------------------------------

impl OpenViking {
    /// The port and root key of a `READY` child, or nothing to call.
    fn admin_context(&self) -> Option<(u16, String)> {
        let live = self.live();
        if live.state != LiveState::Ready {
            return None;
        }
        Some((live.port?, live.root_key.clone()?))
    }

    /// One admin call with the root key. `409` is `AlreadyExists`, which §3.2
    /// treats as success.
    async fn admin(&self, port: u16, root: &str, path: &str, body: Value) -> Result<Value, String> {
        let response = self
            .http
            .post(format!("http://127.0.0.1:{port}/api/v1/{path}"))
            .bearer_auth(root)
            .timeout(ADMIN_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.memory.redact(&e.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::CONFLICT {
            return Ok(Value::Null);
        }
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                self.memory.redact(&first_line(&text))
            ));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| format!("{e}: {}", self.memory.redact(&first_line(&text))))?;
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// §3.2 for every `READY` desk; run at every readiness. One desk's failure
    /// is a warn and a retry at the next readiness, never a block.
    pub async fn provision_desks(self: &Arc<Self>) {
        let desks = match ready_desks(&self.store) {
            Ok(desks) => desks,
            Err(e) => {
                tracing::warn!(error = %e, "reading the desks to provision failed");
                return;
            }
        };
        for desk_id in desks {
            self.provision_desk(&desk_id).await;
        }
    }

    /// §3.2 for one desk: the account, the user, and the seeded key. Called at
    /// every readiness and at the end of creation while the child is `READY`.
    pub async fn provision_desk(self: &Arc<Self>, desk_id: &str) {
        let Some((port, root)) = self.admin_context() else {
            return;
        };
        if let Err(e) = self.provision_one(port, &root, desk_id).await {
            tracing::warn!(desk = desk_id, error = %e, "provisioning a desk's OpenViking user failed");
        }
    }

    async fn provision_one(
        self: &Arc<Self>,
        port: u16,
        root: &str,
        desk_id: &str,
    ) -> Result<(), String> {
        let seed = self
            .ensure_seed()
            .map_err(|e| self.memory.redact(&e.to_string()))?;
        let user = Self::desk_user(desk_id);
        self.admin(
            port,
            root,
            "admin/accounts",
            json!({ "account_id": ACCOUNT, "admin_user_id": ADMIN_USER }),
        )
        .await?;
        self.admin(
            port,
            root,
            &format!("admin/accounts/{ACCOUNT}/users"),
            json!({ "user_id": user, "role": "user" }),
        )
        .await?;
        let answered = self
            .admin(
                port,
                root,
                &format!("admin/accounts/{ACCOUNT}/users/{user}/key"),
                json!({ "seed": seed }),
            )
            .await?;
        let key = answered
            .get("user_key")
            .and_then(Value::as_str)
            .ok_or("the key call answered no user_key")?
            .to_string();
        self.memory.hold_secret(&key);
        {
            let mut live = self.live();
            if live.state != LiveState::Ready {
                return Ok(());
            }
            live.keys.insert(desk_id.to_string(), key.clone());
        }
        let first = self
            .announced
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(desk_id.to_string());
        if first {
            let (at_ns, desk) = (now_ns(), desk_id.to_string());
            let recorded = self.store.unit(move |tx| {
                append_event(
                    tx,
                    "DESK_MEMORY_PROVISIONED",
                    Some(&desk),
                    at_ns,
                    json!({ "desk_id": desk }),
                )
            });
            if let Err(e) = recorded {
                tracing::warn!(error = %e, "recording DESK_MEMORY_PROVISIONED failed");
            }
        }
        self.upload_seed_skill(desk_id, &key).await;
        Ok(())
    }

    /// §5.3's seed upload: `GET /api/v1/skills/desk-improvement` missing means
    /// `POST /api/v1/skills` with the seed's `SKILL.md` under the desk's key.
    /// C55 fills this in with the rewritten seed; until then a freshly
    /// provisioned desk simply starts with no skills of its own.
    async fn upload_seed_skill(&self, desk_id: &str, key: &str) {
        let _ = (desk_id, key);
    }
}

// ---------------------------------------------------------------------------
// openviking (feature SPEC §9, checks 1–5 and 10)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DESK_A: &str = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    const DESK_B: &str = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a6c";
    const PROVIDER_KEY: &str = "sk-openviking-fake-0123456789abcdef";

    /// An [`OpenViking`] on a scratch root, always on the file credential seam
    /// and always with §7.1's stand-in seam open, so no check can reach this
    /// machine's keychain or its real Python.
    fn scratch() -> (tempfile::TempDir, Arc<OpenViking>) {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots::resolve(Some(dir.path())).unwrap();
        roots.create_dirs().unwrap();
        let store = Store::open(&roots.database()).unwrap();
        let memory = Arc::new(crate::memory::seam_memory(store, roots));
        (dir, OpenViking::new(memory, "daemon-uuid".to_string()))
    }

    fn events(store: &Store) -> Vec<(String, Value)> {
        store
            .call(|c| {
                c.prepare(
                    "SELECT kind, payload FROM operational_events WHERE kind LIKE 'OPENVIKING_%' \
                     OR kind = 'DESK_MEMORY_PROVISIONED' ORDER BY occurred_at_ns, id",
                )?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .into_iter()
            .map(|(kind, payload)| (kind, serde_json::from_str(&payload).unwrap()))
            .collect()
    }

    fn count(store: &Store, kind: &str) -> usize {
        events(store).iter().filter(|(k, _)| k == kind).count()
    }

    fn payload(store: &Store, kind: &str) -> Value {
        events(store)
            .into_iter()
            .find(|(k, _)| k == kind)
            .unwrap_or_else(|| panic!("no {kind}"))
            .1
    }

    /// Writes an executable script with `body` as its whole program.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        #[cfg(windows)]
        {
            let path = dir.join(format!("{name}.cmd"));
            fs::write(&path, format!("@echo off\r\n{body}\r\n")).unwrap();
            path
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }

    fn echoing(dir: &Path, name: &str, line: &str) -> PathBuf {
        script(dir, name, &format!("echo {line}"))
    }

    /// A complete provider row, measured against a fake embeddings endpoint.
    async fn configure_provider(ov: &Arc<OpenViking>) {
        let port = crate::memory::fake_embeddings().await;
        ov.memory
            .put_provider(crate::memory::ProviderRequest {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                api_key: Some(PROVIDER_KEY.to_string()),
                llm_model: "llm-1".to_string(),
                embedding_model: "emb-1".to_string(),
            })
            .await
            .unwrap();
    }

    fn plant_desk(store: &Store, id: &str, name: &str) {
        let (id, name) = (id.to_string(), name.to_string());
        store
            .unit(move |tx| {
                let path = format!("/desks/{name}");
                tx.execute(
                    "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, \
                     ready_at_ns, selected_runtime) VALUES (?1, ?2, 'READY', ?3, 1, 1, 'codex')",
                    params![id, name, path],
                )
            })
            .unwrap();
    }

    async fn wait_for_state(ov: &Arc<OpenViking>, state: &str) {
        for _ in 0..400 {
            if setup_row(&ov.store).unwrap().state == state {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("openviking_setup never became {state}");
    }

    async fn wait_for_child(ov: &Arc<OpenViking>, state: LiveState) {
        wait_until(
            &format!("the OpenViking child never became {state:?}"),
            || ov.child_state() == state,
        )
        .await;
    }

    /// §2.2 makes the child `READY` and *then* provisions the desks, so a check
    /// that wants a key waits for the key, not for the state.
    async fn wait_until(what: &str, mut held: impl FnMut() -> bool) {
        for _ in 0..400 {
            if held() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{what}");
    }

    // -- check 1: setup validation (§1.2) -----------------------------------

    #[tokio::test]
    async fn setup_validation() {
        let (dir, ov) = scratch();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let py312 = echoing(&bin, "py312", "3.12");
        let py311 = echoing(&bin, "py311", "3.11");
        let node22 = echoing(&bin, "node22", "v22.11.0");
        let node20 = echoing(&bin, "node20", "v20.19.0");
        let missing = bin.join("not-there");
        let request = |python: &Path, node: &Path| SetupRequest {
            python: python.to_string_lossy().into_owned(),
            node: node.to_string_lossy().into_owned(),
            wheels: None,
            standin: None,
        };

        // A relative path never reaches a probe.
        let err = ov
            .setup(SetupRequest {
                python: "python3".to_string(),
                node: node22.to_string_lossy().into_owned(),
                wheels: None,
                standin: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "VALIDATION");

        for (python, node, code) in [
            (&py311, &node22, "PYTHON_UNSUPPORTED"),
            (&missing, &node22, "PYTHON_PROBE_FAILED"),
            (&py312, &node20, "NODE_UNSUPPORTED"),
            (&py312, &missing, "NODE_PROBE_FAILED"),
        ] {
            let err = ov.setup(request(python, node)).await.unwrap_err();
            assert_eq!(err.code(), code, "{err}");
        }
        // Every refusal leaves the row untouched.
        assert_eq!(setup_row(&ov.store).unwrap().state, "UNCONFIGURED");
        assert!(events(&ov.store).is_empty());

        // The found version is in the message the frontend shows (§8).
        let err = ov.setup(request(&py311, &node22)).await.unwrap_err();
        assert!(err.to_string().contains("3.11"), "{err}");
        let err = ov.setup(request(&py312, &node20)).await.unwrap_err();
        assert!(err.to_string().contains("v20.19.0"), "{err}");

        // The accepted pair claims PROVISIONING with both versions recorded…
        let row = ov.setup(request(&py312, &node22)).await.unwrap();
        assert_eq!(row.state, "PROVISIONING");
        assert_eq!(row.python_version.as_deref(), Some("3.12"));
        assert_eq!(row.node_version.as_deref(), Some("v22.11.0"));
        // …and a second one while it runs is SETUP_BUSY (§1.3).
        let err = ov.setup(request(&py312, &node22)).await.unwrap_err();
        assert_eq!(err.code(), "SETUP_BUSY");
    }

    // -- check 2: provisioning (§1.3) ---------------------------------------

    /// The record of every call the sequence made, one line per invocation.
    fn calls(log: &Path) -> Vec<String> {
        fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// A fake Python: it prints `3.12`, builds a venv whose own `python` is a
    /// copy of itself and whose `openviking-server` answers `--version`, and
    /// logs every invocation's arguments. `pip_fails` makes the install step
    /// print what pip prints for a wheel that is not in the directory.
    fn fake_python(dir: &Path, log: &Path, pip_fails: bool) -> PathBuf {
        let log = log.display().to_string();
        let pip = if pip_fails {
            "ERROR: Could not find a version that satisfies the requirement openviking==0.4.17.1"
        } else {
            "Successfully installed openviking-0.4.17.1"
        };
        let code = i32::from(pip_fails);
        #[cfg(windows)]
        let body = format!(
            "echo %*>>\"{log}\"\r\n\
             if \"%~1\"==\"-c\" goto ver\r\n\
             if \"%~2\"==\"venv\" goto mkvenv\r\n\
             if \"%~2\"==\"pip\" goto pip\r\n\
             exit /b 0\r\n\
             :ver\r\n\
             echo 3.12\r\n\
             exit /b 0\r\n\
             :mkvenv\r\n\
             mkdir \"%~3\\Scripts\"\r\n\
             copy /y \"%~f0\" \"%~3\\Scripts\\python.cmd\" >nul\r\n\
             >\"%~3\\Scripts\\openviking-server.cmd\" echo @echo off\r\n\
             >>\"%~3\\Scripts\\openviking-server.cmd\" echo echo openviking-server 0.4.17.1\r\n\
             exit /b 0\r\n\
             :pip\r\n\
             echo {pip}\r\n\
             exit /b {code}"
        );
        #[cfg(not(windows))]
        let body = format!(
            "echo \"$@\" >> '{log}'\n\
             case \"$1\" in\n\
             -c) echo 3.12 ;;\n\
             -m)\n\
             case \"$2\" in\n\
             venv) mkdir -p \"$3/bin\"; cp \"$0\" \"$3/bin/python\"; chmod 755 \"$3/bin/python\"; \
             printf '#!/bin/sh\\necho openviking-server 0.4.17.1\\n' > \"$3/bin/openviking-server\"; \
             chmod 755 \"$3/bin/openviking-server\" ;;\n\
             pip) echo '{pip}'; exit {code} ;;\n\
             esac ;;\n\
             esac\n\
             exit 0"
        );
        script(dir, "fakepy", &body)
    }

    #[tokio::test]
    async fn provisioning_runs_the_sequence_and_creates_the_seed() {
        let (dir, ov) = scratch();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("calls.log");
        let python = fake_python(&bin, &log, false);
        let node = echoing(&bin, "node22", "v22.11.0");
        let wheels = dir.path().join("wheels");
        fs::create_dir_all(&wheels).unwrap();
        let request = || SetupRequest {
            python: python.to_string_lossy().into_owned(),
            node: node.to_string_lossy().into_owned(),
            wheels: Some(wheels.to_string_lossy().into_owned()),
            standin: None,
        };

        assert_eq!(ov.setup(request()).await.unwrap().state, "PROVISIONING");
        wait_for_state(&ov, "AVAILABLE").await;
        let venv = venv_dir(&ov.roots);
        let row = setup_row(&ov.store).unwrap();
        assert_eq!(
            row.venv_path.as_deref(),
            Some(venv.to_string_lossy().as_ref())
        );
        assert!(row.provisioned_at_ns.is_some() && row.failure_code.is_none());
        assert_eq!(count(&ov.store, "OPENVIKING_PROVISIONED"), 1);
        let announced = payload(&ov.store, "OPENVIKING_PROVISIONED");
        assert_eq!(announced["python_version"], "3.12");
        assert_eq!(announced["node_version"], "v22.11.0");
        // No provider row, so nothing is started (§2.2).
        assert_eq!(ov.child_state(), LiveState::NotStarted);

        // §1.3's sequence, in order and with the exact arguments.
        let seen = calls(&log);
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[0].contains("-c"), "{seen:?}");
        assert!(
            seen[1].contains("-m")
                && seen[1].contains("venv")
                && seen[1].contains(venv.to_string_lossy().as_ref()),
            "{seen:?}"
        );
        assert!(
            seen[2].contains("pip")
                && seen[2].contains("install")
                && seen[2].contains("--no-index")
                && seen[2].contains("--find-links")
                && seen[2].contains(wheels.to_string_lossy().as_ref())
                && seen[2].contains("openviking==0.4.17.1"),
            "{seen:?}"
        );

        // The seed is created once and never rotated (§1.3).
        let seed = ov
            .memory
            .load_secret(crate::memory::SEED_ACCOUNT)
            .unwrap()
            .unwrap();
        assert_eq!(seed.len(), 64);
        ov.setup(request()).await.unwrap();
        wait_for_state(&ov, "AVAILABLE").await;
        assert_eq!(
            ov.memory
                .load_secret(crate::memory::SEED_ACCOUNT)
                .unwrap()
                .unwrap(),
            seed
        );
    }

    #[tokio::test]
    async fn a_missing_wheel_is_provision_failed() {
        let (dir, ov) = scratch();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("calls.log");
        let python = fake_python(&bin, &log, true);
        let node = echoing(&bin, "node22", "v22.11.0");
        ov.setup(SetupRequest {
            python: python.to_string_lossy().into_owned(),
            node: node.to_string_lossy().into_owned(),
            wheels: Some(dir.path().to_string_lossy().into_owned()),
            standin: None,
        })
        .await
        .unwrap();
        wait_for_state(&ov, "UNCONFIGURED").await;
        let row = setup_row(&ov.store).unwrap();
        assert_eq!(row.failure_code.as_deref(), Some("PROVISION_FAILED"));
        assert!(
            row.failure_message
                .as_deref()
                .unwrap_or_default()
                .contains("openviking==0.4.17.1"),
            "{row:?}"
        );
        assert_eq!(count(&ov.store, "OPENVIKING_PROVISIONED"), 0);
    }

    // -- check 3: ov.conf (§2.1) --------------------------------------------

    #[test]
    fn ov_conf_matches_the_spec() {
        let provider = Provider {
            base_url: Some("https://provider.example/v1".to_string()),
            llm_model: Some("gpt-4o-mini".to_string()),
            embedding_model: Some("text-embedding-3-small".to_string()),
            embedding_dimension: Some(1536),
            api_key_present: true,
        };
        let rendered = render_conf(51234, Path::new("/data/openviking/data"), &provider);
        assert_eq!(
            rendered,
            r#"{
  "server": {"host": "127.0.0.1", "port": 51234, "root_api_key": "${MARKETRIG_OV_ROOT_KEY}"},
  "storage": {"workspace": "/data/openviking/data",
              "agfs": {"backend": "local"}, "vectordb": {"backend": "local"}},
  "embedding": {"dense": {"provider": "openai", "model": "text-embedding-3-small", "input": "text",
                          "dimension": 1536,
                          "api_key": "${MARKETRIG_OV_PROVIDER_KEY}", "api_base": "https://provider.example/v1"}},
  "vlm": {"provider": "openai", "model": "gpt-4o-mini",
          "api_key": "${MARKETRIG_OV_PROVIDER_KEY}", "api_base": "https://provider.example/v1"},
  "memory": {"session_skill_extraction_enabled": false}
}
"#
        );
        // It is strict JSON and carries no secret, only the two references.
        let parsed: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["server"]["root_api_key"], "${MARKETRIG_OV_ROOT_KEY}");
        assert_eq!(
            parsed["embedding"]["dense"]["api_key"],
            "${MARKETRIG_OV_PROVIDER_KEY}"
        );
        assert_eq!(parsed["vlm"]["api_key"], "${MARKETRIG_OV_PROVIDER_KEY}");
        assert_eq!(parsed["memory"]["session_skill_extraction_enabled"], false);

        // A Windows workspace path is escaped, not broken.
        let windows = render_conf(1, Path::new(r"C:\Users\x\data"), &provider);
        let parsed: Value = serde_json::from_str(&windows).unwrap();
        assert_eq!(parsed["storage"]["workspace"], r"C:\Users\x\data");
    }

    // -- checks 4 and 5: the child and tenancy (§2.2, §2.3, §3.2) -----------

    /// The stand-in child (§7.1's seam, driven in process): it dumps its
    /// environment and its argv under the redirected `HOME`, then serves
    /// `/ready` and §3.2's three admin routes on the port it was given. A plain
    /// suite run has no `MARKETRIG_FAKE_OV_PORT` and this returns at once.
    #[test]
    fn fake_openviking_main() {
        let Ok(port) = std::env::var("MARKETRIG_FAKE_OV_PORT") else {
            return;
        };
        let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
        let dump: BTreeMap<String, String> = std::env::vars().collect();
        fs::write(home.join("env.json"), serde_json::to_vec(&dump).unwrap()).unwrap();
        let ms = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
        let ready_after = Duration::from_millis(ms("MARKETRIG_FAKE_OV_READY_MS").unwrap_or(0));
        let exit_after = ms("MARKETRIG_FAKE_OV_EXIT_MS");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                if let Some(after) = exit_after {
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(after)).await;
                        eprintln!("openviking-server stopping");
                        let _ = std::io::stderr().flush();
                        // Straight out, so the harness's own summary is not the
                        // last line of the tail.
                        std::process::exit(3);
                    });
                }
                let started = tokio::time::Instant::now();
                let app = fake_admin().route(
                    "/ready",
                    axum::routing::get(move || async move {
                        if started.elapsed() < ready_after {
                            (axum::http::StatusCode::SERVICE_UNAVAILABLE, "starting")
                        } else {
                            (axum::http::StatusCode::OK, "{\"status\":\"ok\"}")
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
                    .await
                    .unwrap();
                println!("openviking-server listening on {port}");
                let _ = std::io::stdout().flush();
                let _ = axum::serve(listener, app).await;
            });
    }

    /// §3.2's three admin routes with OpenViking's own envelope: the account and
    /// the user answer `409 AlreadyExists` after the first time they are asked
    /// for, and the key is deterministic in the user and the seed.
    #[cfg(test)]
    fn fake_admin() -> axum::Router {
        use axum::extract::Path as AxumPath;
        use axum::http::StatusCode;
        use axum::response::{IntoResponse, Response};
        use std::sync::atomic::AtomicUsize;

        fn once(seen: &AtomicUsize, result: Value) -> Response {
            if seen.fetch_add(1, Ordering::Relaxed) > 0 {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(json!({"status": "error", "error": {"code": "AlreadyExists"}})),
                )
                    .into_response();
            }
            axum::Json(json!({"status": "ok", "result": result})).into_response()
        }

        let accounts = Arc::new(AtomicUsize::new(0));
        let users = Arc::new(AtomicUsize::new(0));
        axum::Router::new()
            .route(
                "/api/v1/admin/accounts",
                axum::routing::post(move || {
                    let accounts = accounts.clone();
                    async move { once(&accounts, json!({"account_id": ACCOUNT})) }
                }),
            )
            .route(
                "/api/v1/admin/accounts/{account}/users",
                axum::routing::post(move || {
                    let users = users.clone();
                    async move { once(&users, json!({"user_id": "u"})) }
                }),
            )
            .route(
                "/api/v1/admin/accounts/{account}/users/{user}/key",
                axum::routing::post(
                    |AxumPath((_account, user)): AxumPath<(String, String)>,
                     axum::Json(body): axum::Json<Value>| async move {
                        let seed = body["seed"].as_str().unwrap_or_default();
                        axum::Json(json!({
                            "status": "ok",
                            "result": {"user_key": format!("{user}.{seed}")},
                        }))
                    },
                ),
            )
    }

    /// The stand-in the daemon spawns: it records its own argv, forwards
    /// `--port` and the two knobs to [`fake_openviking_main`] in a fresh
    /// process, and never touches a POSIX shell on Windows.
    fn standin(dir: &Path, name: &str, ready_ms: u64, exit_ms: Option<u64>) -> PathBuf {
        let exe = std::env::current_exe().unwrap().display().to_string();
        let exit = exit_ms.map(|v| v.to_string()).unwrap_or_default();
        #[cfg(windows)]
        let body = format!(
            ">\"%HOME%\\argv.txt\" echo %*\r\n\
             :loop\r\n\
             if \"%~1\"==\"--port\" set OVPORT=%~2\r\n\
             shift\r\n\
             if not \"%~1\"==\"\" goto loop\r\n\
             set MARKETRIG_FAKE_OV_PORT=%OVPORT%\r\n\
             set MARKETRIG_FAKE_OV_READY_MS={ready_ms}\r\n\
             set MARKETRIG_FAKE_OV_EXIT_MS={exit}\r\n\
             \"{exe}\" \"openviking::tests::fake_openviking_main\" --exact --nocapture"
        );
        #[cfg(not(windows))]
        let body = format!(
            "echo \"$@\" > \"$HOME/argv.txt\"\n\
             PORT=\"\"\n\
             while [ $# -gt 0 ]; do\n\
             if [ \"$1\" = \"--port\" ]; then PORT=\"$2\"; fi\n\
             shift\n\
             done\n\
             MARKETRIG_FAKE_OV_PORT=\"$PORT\" \
             MARKETRIG_FAKE_OV_READY_MS={ready_ms} \
             MARKETRIG_FAKE_OV_EXIT_MS={exit} \
             '{exe}' 'openviking::tests::fake_openviking_main' --exact --nocapture"
        );
        script(dir, name, &body)
    }

    fn children(ov: &OpenViking) -> Vec<Value> {
        let raw = match fs::read(crate::daemon::children_path(&ov.roots)) {
            Ok(raw) => raw,
            Err(_) => return Vec::new(),
        };
        serde_json::from_slice::<Value>(&raw).unwrap()["children"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    async fn register(ov: &Arc<OpenViking>, standin: &Path, node: &Path) {
        ov.setup(SetupRequest {
            python: "/absent/python".to_string(),
            node: node.to_string_lossy().into_owned(),
            wheels: None,
            standin: Some(standin.to_string_lossy().into_owned()),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn child_launch_environment_and_tenancy() {
        let (dir, ov) = scratch();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let node = echoing(&bin, "node22", "v22.11.0");
        let standin = standin(&bin, "ov-standin", 0, None);
        configure_provider(&ov).await;
        plant_desk(&ov.store, DESK_A, "alpha");
        plant_desk(&ov.store, DESK_B, "beta");

        register(&ov, &standin, &node).await;
        let row = setup_row(&ov.store).unwrap();
        assert_eq!(row.state, "AVAILABLE");
        assert_eq!(row.venv_path.as_deref(), Some(""));
        wait_for_child(&ov, LiveState::Ready).await;
        wait_until("desk B was never provisioned", || {
            ov.desk_key(DESK_B).is_some()
        })
        .await;

        // §2.2: the argv, the redirected home, and the environment.
        let ov_home = ov.roots.data.join(HOME);
        let home = ov_home.join("home");
        let conf = ov_home.join("ov.conf");
        let port = ov.port().unwrap();
        let argv = fs::read_to_string(home.join("argv.txt")).unwrap();
        for expected in [
            "--config",
            conf.to_string_lossy().as_ref(),
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ] {
            assert!(argv.contains(expected), "{argv}");
        }
        let env: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(home.join("env.json")).unwrap()).unwrap();
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(home.to_string_lossy().as_ref())
        );
        assert_eq!(env.get("PYTHONUTF8").map(String::as_str), Some("1"));
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert!(env.contains_key("PATH"));
        assert_eq!(
            env.get("OPENVIKING_CONFIG_FILE").map(String::as_str),
            Some(conf.to_string_lossy().as_ref())
        );
        assert_eq!(env.get("MARKETRIG_OV_ROOT_KEY").map(String::len), Some(64));
        assert_eq!(
            env.get("MARKETRIG_OV_PROVIDER_KEY").map(String::as_str),
            Some(PROVIDER_KEY)
        );
        #[cfg(windows)]
        {
            assert_eq!(env.get("USERPROFILE"), env.get("HOME"));
            assert_eq!(env.get("LOCALAPPDATA"), env.get("HOME"));
        }

        // `ov.conf` is on disk, 0600, and names neither secret (§2.1).
        let rendered = fs::read_to_string(&conf).unwrap();
        assert!(!rendered.contains(PROVIDER_KEY));
        assert!(!rendered.contains(env.get("MARKETRIG_OV_ROOT_KEY").unwrap()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&conf).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // §2.2: the record and OPENVIKING_STARTED with the port.
        let recorded = children(&ov);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["kind"], "openviking");
        assert_eq!(payload(&ov.store, "OPENVIKING_STARTED")["port"], port);

        // §3.2: both desks provisioned behind one account whose second creation
        // answered `409`, keys in memory only, one announcement each.
        let seed = ov
            .memory
            .load_secret(crate::memory::SEED_ACCOUNT)
            .unwrap()
            .unwrap();
        let user_a = OpenViking::desk_user(DESK_A);
        assert_eq!(user_a, "desk-0199a1b2c3d47e5f8a9b0c1d2e3f4a5b");
        let key_a = ov.desk_key(DESK_A).expect("desk A's key");
        assert_eq!(key_a, format!("{user_a}.{seed}"));
        assert!(ov.desk_key(DESK_B).is_some_and(|key| key != key_a));
        assert_eq!(count(&ov.store, "DESK_MEMORY_PROVISIONED"), 2);
        // A second round announces nothing new (§3.2).
        ov.provision_desks().await;
        assert_eq!(count(&ov.store, "DESK_MEMORY_PROVISIONED"), 2);

        let status = ov.status().unwrap();
        assert_eq!(status.child, LiveState::Ready);
        assert_eq!(status.desks.len(), 2);
        assert!(status.desks.values().all(|provisioned| *provisioned));

        // No key is anywhere durable (§3.2's third scenario, in miniature).
        let database =
            String::from_utf8_lossy(&fs::read(ov.roots.database()).unwrap()).into_owned();
        assert!(!database.contains(&key_a) && !database.contains(&seed));
        assert!(
            !events(&ov.store)
                .iter()
                .any(|(_, p)| p.to_string().contains(&key_a))
        );

        // §2.3: the stop takes the child and its record with it.
        ov.stop_child().await;
        assert_eq!(ov.child_state(), LiveState::NotStarted);
        assert!(children(&ov).is_empty());
    }

    #[tokio::test]
    async fn a_lost_child_is_unavailable_with_no_restart() {
        let (dir, ov) = scratch();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let node = echoing(&bin, "node22", "v22.11.0");
        let dying = standin(&bin, "ov-dying", 0, Some(800));
        configure_provider(&ov).await;
        plant_desk(&ov.store, DESK_A, "alpha");

        register(&ov, &dying, &node).await;
        wait_for_child(&ov, LiveState::Ready).await;
        wait_until("the desk was never provisioned", || {
            ov.desk_key(DESK_A).is_some()
        })
        .await;
        wait_for_child(&ov, LiveState::Lost).await;

        let row = setup_row(&ov.store).unwrap();
        assert_eq!(row.state, "UNAVAILABLE");
        assert_eq!(row.failure_code.as_deref(), Some("CHILD_FAILED"));
        assert_eq!(
            row.failure_message.as_deref(),
            Some("openviking-server stopping")
        );
        assert_eq!(
            payload(&ov.store, "OPENVIKING_LOST")["output_tail_last_line"],
            "openviking-server stopping"
        );
        assert_eq!(count(&ov.store, "OPENVIKING_UNAVAILABLE"), 1);
        // The desk keys are dropped and nothing restarts on its own (§2.3).
        assert!(ov.desk_key(DESK_A).is_none());
        assert!(children(&ov).is_empty());
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(ov.child_state(), LiveState::Lost);

        // Retry starts one more, clears the failure, and hands back the same
        // key: it is derived from the one seed, not from the child (§3.2).
        let living = standin(&bin, "ov-living", 0, None);
        ov.store
            .unit({
                let living = living.to_string_lossy().into_owned();
                move |tx| {
                    tx.execute(
                        "UPDATE openviking_setup SET python_path = ?1 WHERE id = 1",
                        params![living],
                    )
                }
            })
            .unwrap();
        let status = ov.retry().await.unwrap();
        assert_ne!(status.child, LiveState::Lost);
        wait_for_child(&ov, LiveState::Ready).await;
        wait_until("the desk was never re-provisioned", || {
            ov.desk_key(DESK_A).is_some()
        })
        .await;
        let row = setup_row(&ov.store).unwrap();
        assert_eq!(row.state, "AVAILABLE");
        assert!(row.failure_code.is_none());
        let seed = ov
            .memory
            .load_secret(crate::memory::SEED_ACCOUNT)
            .unwrap()
            .unwrap();
        assert_eq!(
            ov.desk_key(DESK_A),
            Some(format!("{}.{seed}", OpenViking::desk_user(DESK_A)))
        );
        ov.stop_child().await;
    }

    #[tokio::test]
    async fn a_readiness_deadline_is_a_loss() {
        let (dir, ov) = scratch();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let node = echoing(&bin, "node22", "v22.11.0");
        // Ready far past the deadline this check shortens to one second.
        let slow = standin(&bin, "ov-slow", 60_000, None);
        configure_provider(&ov).await;
        ov.ready_deadline_ms.store(1_000, Ordering::Relaxed);

        register(&ov, &slow, &node).await;
        wait_for_child(&ov, LiveState::Lost).await;
        assert_eq!(setup_row(&ov.store).unwrap().state, "UNAVAILABLE");
        assert_eq!(count(&ov.store, "OPENVIKING_LOST"), 1);
        assert!(children(&ov).is_empty());
    }

    // -- check 10: redaction (§9) -------------------------------------------

    #[tokio::test]
    async fn every_secret_is_redacted() {
        let (_dir, ov) = scratch();
        configure_provider(&ov).await;
        let seed = ov.ensure_seed().unwrap();
        let root_key = hex32().unwrap();
        let desk_key = "ZGVzay0wMTk5.bWFya2V0cmln.0123456789abcdef0123456789abcdef";
        ov.memory.hold_secret(&root_key);
        ov.memory.hold_secret(desk_key);

        let quoted = format!(
            "child said: provider {PROVIDER_KEY}, root {root_key}, seed {seed}, desk {desk_key}"
        );
        let redacted = ov.memory.redact(&quoted);
        for secret in [PROVIDER_KEY, &root_key, &seed, desk_key] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert_eq!(redacted.matches("<redacted>").count(), 4, "{redacted}");
        assert_eq!(ov.memory.redact("nothing to hide"), "nothing to hide");
    }
}
