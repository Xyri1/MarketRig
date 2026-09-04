//! The Hindsight memory child, its provider settings, and the desk-scoped
//! operations.
//!
//! Contract: `sdd/features/r4-memory-skills-loop/SPEC.md` §2 (the child, per
//! R4-1), §3 (the provider, per R4-2), §4 (banks and operations, per R4-3),
//! root `sdd/SPEC.md` §16.
//!
//! The desk-scoped routes and the Hindsight request mappings are C31's. This
//! module owns the two installation rows, the discovery probe, the credential
//! seam, the provider routes, and the child's launch, readiness, and loss.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

use crate::desk::append_event;
use crate::store::{Roots, Store, StoreError, now_ns};

/// The credential store's service and account (§3, per R4-2, D49).
const SERVICE: &str = "marketrig";
const ACCOUNT: &str = "hindsight-provider";

/// The opaque marker `memory_provider.key_ref` carries once a key is stored (§6).
const KEY_REF: &str = "marketrig/hindsight-provider";

/// The seam credential store, inside the relocated runtime directory (§3).
const CREDENTIALS: &str = "credentials.json";

/// `<executable> --help` must name this on standard output (§2.1). Standard
/// error is ignored on purpose: the real launcher prints an unrelated
/// missing-`sentence-transformers` warning there on every valid run.
const PROBE_MARKER: &str = "HINDSIGHT_API_PORT";

/// The provider model list's own bound (§3).
const MODELS_TIMEOUT: Duration = Duration::from_secs(15);

/// What the daemon puts in place of the stored key in anything it lifts from
/// the child (§4.3): Hindsight's `detail` quotes the provider's text, which
/// quotes the key back.
const REDACTED: &str = "<redacted>";

/// The child's liveness (§6): memory only, `NOT_STARTED` after every start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LiveState {
    #[default]
    NotStarted,
    Starting,
    Ready,
    Lost,
}

/// The one 4 KiB tail the daemon keeps of the child's standard output (§2.2).
const TAIL: usize = 4096;

/// The readiness deadline (§2.2): measured headroom over the 12.6 s cold start
/// Spike H timed, not a guess. A [`Memory`] field so a check can shorten it.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// The `/health` poll and each attempt's own bound (§2.2).
const HEALTH_POLL: Duration = Duration::from_millis(500);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(1);

/// How often the supervisor looks for the child's exit, and how long a caller
/// that arrived during `STARTING` sleeps between looks (§2.2, §2.3).
const WATCH_POLL: Duration = Duration::from_millis(250);

/// The child's `HOME`, under the data root (§2.2): pg0 puts its PostgreSQL
/// instance there, so the daemon owns it rather than the user's home.
const HINDSIGHT_HOME: &str = "hindsight";

/// What a caller is told when the start it waited for ended (§2.3, §4.3). The
/// child's own last line is in `MEMORY_LOST` and, on the second loss, the row.
const LOST_MESSAGE: &str = "The memory child stopped before it could answer.";
const SLOW_MESSAGE: &str = "The memory child did not become ready in time.";

/// Everything about the live child, behind one mutex (slice §2): operations are
/// seconds long and one per installation, so there is no finer lock.
#[derive(Default)]
pub struct Live {
    pub state: LiveState,
    pub pid: Option<u32>,
    pub child: Option<crate::exec::Contained>,
    pub port: Option<u16>,
    /// The per-start bearer; memory only, never stored, logged, or answered.
    pub bearer: Option<String>,
    /// The child's standard output, newest [`TAIL`] bytes, raw and never parsed
    /// — the launcher's banner carries ANSI escapes even off a terminal (§2.2).
    pub output_tail: Vec<u8>,
    /// Losses since the last readiness; the second one is `CHILD_FAILED` (§2.3).
    pub losses_since_ready: u8,
    /// Bumped by every start and every stop, so a supervisor whose child is
    /// already gone cannot report a loss against the one that replaced it.
    pub generation: u64,
}

impl Live {
    /// Appends to the tail, keeping the newest [`TAIL`] bytes.
    pub fn push_output(&mut self, bytes: &[u8]) {
        self.output_tail.extend_from_slice(bytes);
        if self.output_tail.len() > TAIL {
            self.output_tail
                .drain(..self.output_tail.len() - TAIL)
                .for_each(drop);
        }
    }
}

/// The memory subsystem: the two installation rows, the credential seam, the
/// one HTTP client, and the live child. One per daemon, in `ApiState`.
pub struct Memory {
    pub store: Store,
    pub roots: Roots,
    /// True under `MARKETRIG_TEST_DATA_ROOT`: the credential store is
    /// `runtime/credentials.json` in the relocated root instead of the platform
    /// store (§3, per R4-2). It is the harness seam, never a fallback when the
    /// native store fails.
    pub seam: bool,
    /// The provider fetch and the child's routes: never through a machine proxy
    /// (the child is on loopback), never following a redirect. Timeouts are per
    /// request, since §3 and §4.3 bound them differently.
    pub http: reqwest::Client,
    /// This daemon's uuid, for the `children.json` record of every start (§2.2).
    pub daemon_uuid: String,
    /// [`READY_DEADLINE`] in a daemon; a check shortens it rather than waiting
    /// out two minutes for the loss path.
    pub ready_deadline: Duration,
    /// Shared with each start's supervisor task, which outlives the call that
    /// started the child.
    pub live: Arc<Mutex<Live>>,
}

impl Memory {
    pub fn new(store: Store, roots: Roots, daemon_uuid: String) -> io::Result<Memory> {
        Ok(Memory {
            store,
            roots,
            seam: std::env::var_os(crate::store::TEST_DATA_ROOT_ENV).is_some(),
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(io::Error::other)?,
            daemon_uuid,
            ready_deadline: READY_DEADLINE,
            live: Arc::new(Mutex::new(Live::default())),
        })
    }
}

/// A memory failure carrying a stable SCREAMING_SNAKE code (§3, §4.3).
#[derive(Debug)]
pub enum MemoryError {
    /// No `AVAILABLE` child row, or no provider base URL, key, or models.
    Unconfigured,
    /// The child row is `UNAVAILABLE`, or it never became ready in time.
    Unavailable(String),
    /// A Hindsight 4xx, its `detail` string as the message.
    Rejected(String),
    Timeout,
    /// A Hindsight 5xx or a transport failure.
    Error(String),
    Validation(String),
    EmbeddingModelLocked,
    CredentialStoreUnavailable(String),
    ProviderUnreachable(String),
}

impl MemoryError {
    pub fn code(&self) -> &'static str {
        match self {
            MemoryError::Unconfigured => "MEMORY_UNCONFIGURED",
            MemoryError::Unavailable(_) => "MEMORY_UNAVAILABLE",
            MemoryError::Rejected(_) => "MEMORY_REJECTED",
            MemoryError::Timeout => "MEMORY_TIMEOUT",
            MemoryError::Error(_) => "MEMORY_ERROR",
            MemoryError::Validation(_) => "VALIDATION",
            MemoryError::EmbeddingModelLocked => "EMBEDDING_MODEL_LOCKED",
            MemoryError::CredentialStoreUnavailable(_) => "CREDENTIAL_STORE_UNAVAILABLE",
            MemoryError::ProviderUnreachable(_) => "PROVIDER_UNREACHABLE",
        }
    }
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Unconfigured => write!(
                f,
                "Memory is not configured: discover a Hindsight launcher and set the provider first."
            ),
            MemoryError::Unavailable(m) | MemoryError::Rejected(m) | MemoryError::Error(m) => {
                write!(f, "{m}")
            }
            MemoryError::Timeout => write!(f, "The memory child did not answer in time."),
            MemoryError::Validation(m) => write!(f, "{m}"),
            MemoryError::EmbeddingModelLocked => write!(
                f,
                "The embedding model was locked by the first retain and cannot change."
            ),
            MemoryError::CredentialStoreUnavailable(m) => {
                write!(f, "The credential store is unavailable: {m}")
            }
            MemoryError::ProviderUnreachable(m) => write!(f, "The provider did not answer: {m}"),
        }
    }
}

impl std::error::Error for MemoryError {}

/// ponytail: a database failure surfaces as `MEMORY_ERROR`, because §4.3 gives
/// this group no internal code and the daemon's SQLite is in-process and
/// single-writer, so this path is unreachable in practice. Give it its own
/// variant the day a memory route can genuinely fail on the store.
impl From<StoreError> for MemoryError {
    fn from(e: StoreError) -> Self {
        MemoryError::Error(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The two installation rows (§6)
// ---------------------------------------------------------------------------

/// The `memory_child` row plus the liveness that is never durable (§3).
#[derive(Debug, Clone, Serialize)]
pub struct Child {
    /// `UNCONFIGURED` | `AVAILABLE` | `UNAVAILABLE`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    pub live: LiveState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// `GET /memory` (§3): both installation rows in the order the CLI prints them.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub child: Child,
    pub provider: Provider,
}

/// The `memory_provider` row, secrets-free by construction (§3).
#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    pub api_key_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_locked_at_ns: Option<i64>,
}

const CHILD_SELECT: &str = "SELECT state, executable_path, validated_at_ns, failure_code, \
                            failure_message FROM memory_child WHERE id = 1";

fn read_child(row: &rusqlite::Row<'_>) -> rusqlite::Result<Child> {
    Ok(Child {
        state: row.get(0)?,
        executable_path: row.get(1)?,
        validated_at_ns: row.get(2)?,
        failure_code: row.get(3)?,
        failure_message: row.get(4)?,
        live: LiveState::NotStarted,
        pid: None,
    })
}

const PROVIDER_SELECT: &str = "SELECT base_url, llm_model, embedding_model, key_ref, \
                               embedding_locked_at_ns FROM memory_provider WHERE id = 1";

fn read_provider(row: &rusqlite::Row<'_>) -> rusqlite::Result<Provider> {
    Ok(Provider {
        base_url: row.get(0)?,
        llm_model: row.get(1)?,
        embedding_model: row.get(2)?,
        api_key_present: row.get::<_, Option<String>>(3)?.is_some(),
        embedding_locked_at_ns: row.get(4)?,
    })
}

/// The durable half of the child row; the liveness is the caller's to fill.
pub fn child_row(store: &Store) -> Result<Child, StoreError> {
    store.call(|c| c.query_row(CHILD_SELECT, [], read_child))
}

pub fn provider_row(store: &Store) -> Result<Provider, StoreError> {
    store.call(|c| c.query_row(PROVIDER_SELECT, [], read_provider))
}

// ---------------------------------------------------------------------------
// Discovery (§2.1)
// ---------------------------------------------------------------------------

enum Outcome {
    Available,
    Failed { code: &'static str, message: String },
}

/// `<executable> --help` inside the shared ten-second probe bound: exit `0` and
/// [`PROBE_MARKER`] on standard output, nothing else (§2.1).
fn validate(executable: &Path) -> Outcome {
    if !executable.is_file() {
        return Outcome::Failed {
            code: "NOT_FOUND",
            message: format!("No executable at {}.", executable.display()),
        };
    }
    let Some((ok, out, _)) = crate::runtime::run(crate::runtime::probe(executable, &["--help"]))
    else {
        return Outcome::Failed {
            code: "PROBE_FAILED",
            message: format!("{} --help did not answer.", executable.display()),
        };
    };
    if !ok {
        return Outcome::Failed {
            code: "PROBE_FAILED",
            message: format!("{} --help exited non-zero.", executable.display()),
        };
    }
    if !out.contains(PROBE_MARKER) {
        return Outcome::Failed {
            code: "CAPABILITY_MISSING",
            message: format!(
                "{} --help does not name {PROBE_MARKER}, so it is not a Hindsight launcher.",
                executable.display()
            ),
        };
    }
    Outcome::Available
}

/// `POST /memory/discover` (§2.1). The path is recorded whatever the outcome, so
/// `retry` has something to re-validate.
pub fn discover(store: &Store, executable: &Path) -> Result<Child, StoreError> {
    let outcome = validate(executable);
    let path = executable.to_string_lossy().into_owned();
    let at_ns = now_ns();
    store.unit(move |tx| {
        let state = match &outcome {
            Outcome::Available => {
                tx.execute(
                    "UPDATE memory_child SET state = 'AVAILABLE', executable_path = ?1, \
                     validated_at_ns = ?2, failure_code = NULL, failure_message = NULL \
                     WHERE id = 1",
                    params![path, at_ns],
                )?;
                "AVAILABLE"
            }
            Outcome::Failed { code, message } => {
                tx.execute(
                    "UPDATE memory_child SET state = 'UNAVAILABLE', executable_path = ?1, \
                     failure_code = ?2, failure_message = ?3 WHERE id = 1",
                    params![path, code, message],
                )?;
                "UNAVAILABLE"
            }
        };
        append_event(
            tx,
            "MEMORY_CONFIGURED",
            None,
            at_ns,
            json!({ "what": "child", "executable_path": &path, "state": state }),
        )?;
        tx.query_row(CHILD_SELECT, [], read_child)
    })
}

/// `POST /memory/retry` (§2.1): re-validate the recorded launcher, which clears
/// whatever failure the row holds. A row naming no launcher answers unchanged.
pub fn retry(store: &Store) -> Result<Child, StoreError> {
    let row = child_row(store)?;
    match &row.executable_path {
        Some(path) => discover(store, Path::new(path)),
        None => Ok(row),
    }
}

/// Startup step 6b (§2.1): re-validate an `AVAILABLE` launcher before the
/// listener binds. A failure is a row, never a startup failure. The caller skips
/// this under `MARKETRIG_TEST_DATA_ROOT`, exactly as it skips step 6a.
pub fn revalidate_available(store: &Store) -> Result<(), StoreError> {
    let row = child_row(store)?;
    if row.state != "AVAILABLE" {
        return Ok(());
    }
    if let Some(path) = row.executable_path {
        discover(store, Path::new(&path))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The credential seam (§3, per R4-2)
// ---------------------------------------------------------------------------

/// Names the platform credential store once, at daemon start, before any route
/// can reach it (slice §2). A failure is a log line, not a startup failure:
/// `PUT /memory/provider` then answers `CREDENTIAL_STORE_UNAVAILABLE`.
pub fn set_platform_store() {
    #[cfg(any(target_os = "macos", windows))]
    {
        #[cfg(target_os = "macos")]
        let built = apple_native_keyring_store::keychain::Store::new();
        #[cfg(windows)]
        let built = windows_native_keyring_store::Store::new();
        match built {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => tracing::warn!(error = %e, "the platform credential store is unavailable"),
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    tracing::warn!("this platform has no MarketRig credential store");
}

impl Memory {
    fn credentials_path(&self) -> PathBuf {
        self.roots.runtime().join(CREDENTIALS)
    }

    /// Writes the provider key. Under the seam it is `runtime/credentials.json`
    /// (0600, one JSON object keyed by account); otherwise the platform store.
    pub fn store_key(&self, key: &str) -> Result<(), MemoryError> {
        if self.seam {
            let path = self.credentials_path();
            let mut map = seam_map(&path)?;
            map.insert(ACCOUNT.to_string(), key.to_string());
            return seam_write(&path, &map)
                .map_err(|e| MemoryError::CredentialStoreUnavailable(e.to_string()));
        }
        keyring_core::Entry::new(SERVICE, ACCOUNT)
            .and_then(|entry| entry.set_password(key))
            .map_err(|e| MemoryError::CredentialStoreUnavailable(e.to_string()))
    }

    /// Reads the provider key back, `None` when none was ever stored.
    pub fn load_key(&self) -> Result<Option<String>, MemoryError> {
        if self.seam {
            return Ok(seam_map(&self.credentials_path())?.remove(ACCOUNT));
        }
        match keyring_core::Entry::new(SERVICE, ACCOUNT).and_then(|entry| entry.get_password()) {
            Ok(key) => Ok(Some(key)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(MemoryError::CredentialStoreUnavailable(e.to_string())),
        }
    }

    /// Whether the store actually holds a key. `Provider::api_key_present`
    /// answers from `key_ref` instead, which a status read must not pay a
    /// credential-store round trip for.
    pub fn key_present(&self) -> bool {
        matches!(self.load_key(), Ok(Some(_)))
    }

    /// Replaces every occurrence of the stored key in a message the daemon
    /// lifted from the child (§4.3). A message with no key in it, and a store
    /// with no key in it, both come back unchanged.
    pub fn redact(&self, message: &str) -> String {
        match self.load_key() {
            Ok(Some(key)) => redact_key(&key, message),
            _ => message.to_string(),
        }
    }
}

/// [`Memory::redact`] against a key already in hand — the child's supervisor
/// holds the key its own child was launched with, so a provider change under it
/// cannot leave the child's last line unredacted.
fn redact_key(key: &str, message: &str) -> String {
    if key.is_empty() {
        message.to_string()
    } else {
        message.replace(key, REDACTED)
    }
}

fn seam_map(path: &Path) -> Result<BTreeMap<String, String>, MemoryError> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(MemoryError::CredentialStoreUnavailable(e.to_string())),
    };
    serde_json::from_slice(&raw).map_err(|e| MemoryError::CredentialStoreUnavailable(e.to_string()))
}

/// Restricts the file before the secret reaches it, like the endpoint pointer.
/// Windows relies on the per-user directory ACL instead (root §4.3).
fn seam_write(path: &Path, map: &BTreeMap<String, String>) -> io::Result<()> {
    let mut file = File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec(map)?)?;
    file.sync_all()
}

// ---------------------------------------------------------------------------
// The provider routes (§3)
// ---------------------------------------------------------------------------

/// `PUT /memory/provider`'s body (§3).
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderRequest {
    pub base_url: String,
    /// Omitted keeps whatever key is already stored.
    #[serde(default)]
    pub api_key: Option<String>,
    pub llm_model: String,
    pub embedding_model: String,
}

/// An absolute `http`/`https` URL with no userinfo, query, or fragment, its
/// trailing slash stripped (§3).
fn validate_base_url(raw: &str) -> Result<String, MemoryError> {
    let invalid = |why: &str| {
        Err(MemoryError::Validation(format!(
            "base_url must be an absolute http or https URL {why}."
        )))
    };
    let raw = raw.trim();
    let Some(rest) = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))
    else {
        return invalid("beginning http:// or https://");
    };
    if rest.is_empty() || rest.starts_with('/') {
        return invalid("naming a host");
    }
    if rest.contains('@') {
        return invalid("carrying no credentials");
    }
    if rest.contains('?') || rest.contains('#') {
        return invalid("carrying no query or fragment");
    }
    if rest.contains(char::is_whitespace) {
        return invalid("carrying no whitespace");
    }
    Ok(raw.trim_end_matches('/').to_string())
}

/// A non-empty model name of at most 128 characters (§3).
fn validate_model(field: &str, raw: &str) -> Result<String, MemoryError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().count() > 128 {
        return Err(MemoryError::Validation(format!(
            "{field} must be 1 to 128 characters."
        )));
    }
    Ok(raw.to_string())
}

impl Memory {
    /// The child row with its liveness (§3).
    pub async fn child(&self) -> Result<Child, MemoryError> {
        let mut child = child_row(&self.store)?;
        let live = self.live.lock().await;
        child.live = live.state;
        child.pid = live.pid;
        Ok(child)
    }

    /// `GET /memory` (§3).
    pub async fn status(&self) -> Result<Status, MemoryError> {
        Ok(Status {
            child: self.child().await?,
            provider: provider_row(&self.store)?,
        })
    }

    /// `POST /memory/discover` (§2.1).
    pub async fn discover(&self, executable: &Path) -> Result<Child, MemoryError> {
        discover(&self.store, executable)?;
        self.child().await
    }

    /// `POST /memory/retry` (§2.1): the failure is cleared, the launcher
    /// re-validated, and the loss counter starts again.
    pub async fn retry(&self) -> Result<Child, MemoryError> {
        retry(&self.store)?;
        self.live.lock().await.losses_since_ready = 0;
        self.child().await
    }

    /// `PUT /memory/provider` (§3): the key reaches the credential store first,
    /// so a store failure writes nothing at all; the row and the event then
    /// commit in one unit, and a live child is stopped so the next operation
    /// starts it with the new environment (§2.3).
    pub async fn put_provider(&self, request: ProviderRequest) -> Result<Provider, MemoryError> {
        let base_url = validate_base_url(&request.base_url)?;
        let llm_model = validate_model("llm_model", &request.llm_model)?;
        let embedding_model = validate_model("embedding_model", &request.embedding_model)?;
        if request.api_key.as_deref().is_some_and(str::is_empty) {
            return Err(MemoryError::Validation(
                "api_key must not be empty; omit it to keep the stored key.".to_string(),
            ));
        }

        let current = provider_row(&self.store)?;
        if current.embedding_locked_at_ns.is_some()
            && current.embedding_model.as_deref() != Some(embedding_model.as_str())
        {
            return Err(MemoryError::EmbeddingModelLocked);
        }

        if let Some(key) = &request.api_key {
            self.store_key(key)?;
        }
        let key_ref = (request.api_key.is_some() || current.api_key_present).then_some(KEY_REF);

        let at_ns = now_ns();
        let provider = self.store.unit(move |tx| {
            tx.execute(
                "UPDATE memory_provider SET base_url = ?1, llm_model = ?2, embedding_model = ?3, \
                 key_ref = ?4, updated_at_ns = ?5 WHERE id = 1",
                params![base_url, llm_model, embedding_model, key_ref, at_ns],
            )?;
            append_event(
                tx,
                "MEMORY_CONFIGURED",
                None,
                at_ns,
                json!({
                    "what": "provider",
                    "base_url": &base_url,
                    "llm_model": &llm_model,
                    "embedding_model": &embedding_model,
                }),
            )?;
            tx.query_row(PROVIDER_SELECT, [], read_provider)
        })?;

        self.stop_child().await;
        Ok(provider)
    }

    /// `GET /memory/provider/models` (§3): fetched live at request time, never
    /// cached, and nothing it answers is persisted.
    pub async fn models(&self) -> Result<Vec<String>, MemoryError> {
        let row = provider_row(&self.store)?;
        let (Some(base_url), Some(key)) = (row.base_url, self.load_key()?) else {
            return Err(MemoryError::Unconfigured);
        };
        let unreachable = |why: String| MemoryError::ProviderUnreachable(first_line(&why));
        let response = self
            .http
            .get(format!("{base_url}/models"))
            .bearer_auth(&key)
            .timeout(MODELS_TIMEOUT)
            .send()
            .await
            .map_err(|e| unreachable(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(MemoryError::ProviderUnreachable(format!("HTTP {status}")));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| unreachable(e.to_string()))?;
        let Some(data) = body.get("data").and_then(Value::as_array) else {
            return Err(MemoryError::ProviderUnreachable(
                "the answer carried no data array".to_string(),
            ));
        };
        Ok(data
            .iter()
            .filter_map(|model| Some(model.get("id")?.as_str()?.to_string()))
            .collect())
    }

    /// The provider half of the child's environment (§2.2); C30 composes the
    /// platform half around it.
    pub fn provider_env(&self) -> Result<Vec<(String, String)>, MemoryError> {
        let row = provider_row(&self.store)?;
        let (Some(base_url), Some(llm_model), Some(embedding_model), Some(key)) = (
            row.base_url,
            row.llm_model,
            row.embedding_model,
            self.load_key()?,
        ) else {
            return Err(MemoryError::Unconfigured);
        };
        Ok(vec![
            ("HINDSIGHT_API_LLM_PROVIDER".into(), "openai".into()),
            ("HINDSIGHT_API_LLM_BASE_URL".into(), base_url.clone()),
            ("HINDSIGHT_API_LLM_API_KEY".into(), key.clone()),
            ("HINDSIGHT_API_LLM_MODEL".into(), llm_model),
            ("HINDSIGHT_API_EMBEDDINGS_PROVIDER".into(), "openai".into()),
            ("HINDSIGHT_API_EMBEDDINGS_OPENAI_BASE_URL".into(), base_url),
            ("HINDSIGHT_API_EMBEDDINGS_OPENAI_API_KEY".into(), key),
            (
                "HINDSIGHT_API_EMBEDDINGS_OPENAI_MODEL".into(),
                embedding_model,
            ),
        ])
    }
}

// ---------------------------------------------------------------------------
// The child's lifecycle (§2.2, §2.3)
// ---------------------------------------------------------------------------

/// Everything one start needs, read before the live state is claimed so that a
/// row or a provider that forbids the start costs no process at all.
struct Launch {
    executable: PathBuf,
    port: u16,
    /// Minted per start, held in memory only (§2.2).
    bearer: String,
    /// The provider key this child is launched with, kept for redaction (§4.3).
    key: String,
    env: Vec<(String, String)>,
}

/// 32 random bytes as hex — the child's `HINDSIGHT_API_TENANT_API_KEY` (§2.2).
fn mint_bearer() -> Result<String, MemoryError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| MemoryError::Error(e.to_string()))?;
    Ok(bytes.iter().fold(String::new(), |mut hex, byte| {
        use fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

/// The variables the child's own `HOME` replaces, whatever case the daemon's
/// environment spells them in.
fn redirected(key: &str) -> bool {
    ["HOME", "USERPROFILE", "LOCALAPPDATA"]
        .iter()
        .any(|name| key.eq_ignore_ascii_case(name))
}

/// The last non-empty line of the child's tail, lossily decoded and never
/// parsed further — the banner carries ANSI escapes even off a terminal (§2.2).
fn last_line(tail: &[u8]) -> String {
    String::from_utf8_lossy(tail)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

impl Memory {
    /// §2.2's environment and nothing else of the daemon's own: the R3 §4.2
    /// platform set with the child's `HOME` in place of the daemon's, plus
    /// every `HINDSIGHT_API_*` variable.
    fn child_env(
        &self,
        home: &Path,
        port: u16,
        bearer: &str,
    ) -> Result<Vec<(String, String)>, MemoryError> {
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
        env.extend([
            ("HINDSIGHT_API_HOST".to_string(), "127.0.0.1".to_string()),
            ("HINDSIGHT_API_PORT".to_string(), port.to_string()),
            ("HINDSIGHT_API_WORKERS".to_string(), "1".to_string()),
            ("HINDSIGHT_API_LOG_LEVEL".to_string(), "warning".to_string()),
            (
                "HINDSIGHT_API_DATABASE_URL".to_string(),
                "pg0://marketrig".to_string(),
            ),
            (
                "HINDSIGHT_API_TENANT_EXTENSION".to_string(),
                "hindsight_api.extensions.builtin.tenant:ApiKeyTenantExtension".to_string(),
            ),
            (
                "HINDSIGHT_API_TENANT_API_KEY".to_string(),
                bearer.to_string(),
            ),
            ("HINDSIGHT_API_MCP_ENABLED".to_string(), "false".to_string()),
            (
                "HINDSIGHT_API_OTEL_TRACES_ENABLED".to_string(),
                "false".to_string(),
            ),
            (
                "HINDSIGHT_API_RERANKER_PROVIDER".to_string(),
                "rrf".to_string(),
            ),
        ]);
        env.extend(self.provider_env()?);
        Ok(env)
    }

    /// The rows decide before anything is spawned (§2.3).
    fn plan(&self) -> Result<Launch, MemoryError> {
        let row = child_row(&self.store)?;
        match row.state.as_str() {
            "AVAILABLE" => {}
            "UNAVAILABLE" => {
                return Err(MemoryError::Unavailable(
                    row.failure_message
                        .unwrap_or_else(|| "The memory child is unavailable.".to_string()),
                ));
            }
            _ => return Err(MemoryError::Unconfigured),
        }
        let Some(executable) = row.executable_path else {
            return Err(MemoryError::Unconfigured);
        };
        let home = self.roots.data.join(HINDSIGHT_HOME);
        fs::create_dir_all(&home).map_err(|e| MemoryError::Error(e.to_string()))?;
        let port = crate::codex::free_port().map_err(MemoryError::Error)?;
        let bearer = mint_bearer()?;
        let env = self.child_env(&home, port, &bearer)?;
        Ok(Launch {
            executable: PathBuf::from(executable),
            port,
            bearer,
            key: self.load_key()?.unwrap_or_default(),
            env,
        })
    }

    /// §2.2: start the child if none is live, wait for `/health`, and answer the
    /// port and the per-start bearer. A start already in flight is the one to
    /// wait for; the caller's own timeout (§4.3) bounds that wait from outside,
    /// this deadline bounds only the start itself.
    pub async fn ensure_ready(&self) -> Result<(u16, String), MemoryError> {
        let deadline = tokio::time::Instant::now() + self.ready_deadline;
        loop {
            let state = {
                let live = self.live.lock().await;
                if live.state == LiveState::Ready {
                    return Ok((
                        live.port.unwrap_or_default(),
                        live.bearer.clone().unwrap_or_default(),
                    ));
                }
                live.state
            };
            if state == LiveState::Starting {
                if tokio::time::Instant::now() >= deadline {
                    return Err(MemoryError::Unavailable(SLOW_MESSAGE.to_string()));
                }
                tokio::time::sleep(WATCH_POLL).await;
                continue;
            }

            // `NOT_STARTED` or `LOST`: this caller starts it, once.
            let launch = self.plan()?;
            let generation = {
                let mut live = self.live.lock().await;
                if matches!(live.state, LiveState::Starting | LiveState::Ready) {
                    continue; // another caller claimed the start first
                }
                live.generation += 1;
                live.state = LiveState::Starting;
                live.port = Some(launch.port);
                live.bearer = Some(launch.bearer.clone());
                live.output_tail.clear();
                live.generation
            };
            return self.start(launch, generation, deadline).await;
        }
    }

    /// Spawns the claimed start, records it, and hands the child to its
    /// supervisor before polling `/health` (§2.2).
    async fn start(
        &self,
        launch: Launch,
        generation: u64,
        deadline: tokio::time::Instant,
    ) -> Result<(u16, String), MemoryError> {
        let mut command = tokio::process::Command::new(&launch.executable);
        command
            // The data root carries no `.env` for the launcher's dotenv loader.
            .current_dir(&self.roots.data)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Spike H: the launcher logs to standard output and leaves standard
            // error empty, so there is nothing here worth a pipe and a task.
            .stderr(Stdio::null());
        command.env_clear();
        for (key, value) in &launch.env {
            command.env(key, value);
        }
        let mut child = match crate::exec::spawn(command) {
            Ok(child) => child,
            Err(e) => {
                let mut live = self.live.lock().await;
                if live.generation == generation {
                    *live = Live {
                        generation,
                        ..Live::default()
                    };
                }
                return Err(MemoryError::Unavailable(format!(
                    "The memory child could not be started: {e}"
                )));
            }
        };
        let pid = child.id().unwrap_or_default();
        let stdout = child.take_stdout();
        let mut live = self.live.lock().await;
        // A stop that landed while the spawn was in flight ends this start
        // before it is recorded or supervised, so nothing is orphaned (§2.3).
        if live.generation != generation {
            drop(live);
            child.terminate().await;
            return Err(MemoryError::Unavailable(LOST_MESSAGE.to_string()));
        }
        live.pid = Some(pid);
        live.child = Some(child);
        drop(live);
        crate::daemon::record_child(
            &self.roots,
            crate::daemon::ChildRecord {
                pid,
                kind: "memory".to_string(),
                args: vec![launch.executable.to_string_lossy().into_owned()],
                daemon_uuid: self.daemon_uuid.clone(),
                launched_at_ns: now_ns(),
            },
        );
        tokio::spawn(supervise(
            Supervisor {
                store: self.store.clone(),
                roots: self.roots.clone(),
                live: self.live.clone(),
                key: launch.key.clone(),
                generation,
            },
            stdout,
        ));
        self.await_ready(launch, generation, deadline).await
    }

    /// `GET /health` until `200`, the deadline, or the child's end (§2.2).
    async fn await_ready(
        &self,
        launch: Launch,
        generation: u64,
        deadline: tokio::time::Instant,
    ) -> Result<(u16, String), MemoryError> {
        let health = format!("http://127.0.0.1:{}/health", launch.port);
        loop {
            {
                let live = self.live.lock().await;
                if live.generation != generation || live.state != LiveState::Starting {
                    return Err(MemoryError::Unavailable(LOST_MESSAGE.to_string()));
                }
            }
            let answered = matches!(
                self.http.get(&health).timeout(HEALTH_TIMEOUT).send().await,
                Ok(response) if response.status() == reqwest::StatusCode::OK
            );
            if answered {
                let pid = {
                    let mut live = self.live.lock().await;
                    if live.generation != generation || live.state != LiveState::Starting {
                        return Err(MemoryError::Unavailable(LOST_MESSAGE.to_string()));
                    }
                    live.state = LiveState::Ready;
                    live.losses_since_ready = 0;
                    live.pid.unwrap_or_default()
                };
                let at_ns = now_ns();
                self.store.unit(move |tx| {
                    append_event(tx, "MEMORY_STARTED", None, at_ns, json!({ "pid": pid }))
                })?;
                return Ok((launch.port, launch.bearer));
            }
            if tokio::time::Instant::now() >= deadline {
                lose(
                    &self.store,
                    &self.roots,
                    &self.live,
                    generation,
                    &launch.key,
                    None,
                )
                .await;
                return Err(MemoryError::Unavailable(SLOW_MESSAGE.to_string()));
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    }

    /// §2.3: stop a live child. A provider change and Quit both call it; ending
    /// the generation is what keeps the supervisor from calling this a loss.
    pub async fn stop_child(&self) {
        let (child, pid) = {
            let mut live = self.live.lock().await;
            if live.state == LiveState::NotStarted && live.child.is_none() {
                return;
            }
            let generation = live.generation + 1;
            let losses_since_ready = live.losses_since_ready;
            let child = live.child.take();
            let pid = live.pid;
            *live = Live {
                generation,
                losses_since_ready,
                ..Live::default()
            };
            (child, pid)
        };
        if let Some(mut child) = child {
            child.terminate().await;
        }
        if let Some(pid) = pid {
            crate::daemon::forget_child(&self.roots, pid);
        }
    }
}

/// Everything one start's supervisor needs; it outlives the call that started
/// the child, so it carries clones rather than a borrow of [`Memory`].
struct Supervisor {
    store: Store,
    roots: Roots,
    live: Arc<Mutex<Live>>,
    key: String,
    generation: u64,
}

/// One task per start: the child's standard output into the tail, and the
/// child's exit into a loss (§2.2, §2.3).
///
/// ponytail: a quarter-second poll for the exit rather than an owned `wait()`,
/// so the handle can stay in [`Live`] where [`Memory::stop_child`] reaches it
/// without a second channel. Give the child its own watch channel the day a
/// quarter second of loss latency matters.
async fn supervise(context: Supervisor, stdout: Option<tokio::process::ChildStdout>) {
    let mut stdout = stdout;
    let mut buffer = [0u8; 1024];
    loop {
        if let Some(reader) = stdout.as_mut() {
            match tokio::time::timeout(WATCH_POLL, reader.read(&mut buffer)).await {
                Ok(Ok(0)) | Ok(Err(_)) => stdout = None,
                Ok(Ok(read)) => {
                    let mut live = context.live.lock().await;
                    if live.generation != context.generation {
                        return;
                    }
                    live.push_output(&buffer[..read]);
                    continue;
                }
                Err(_elapsed) => {}
            }
        } else {
            tokio::time::sleep(WATCH_POLL).await;
        }
        let exit_code = {
            let mut live = context.live.lock().await;
            if live.generation != context.generation {
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
        lose(
            &context.store,
            &context.roots,
            &context.live,
            context.generation,
            &context.key,
            exit_code,
        )
        .await;
        return;
    }
}

/// §2.3: the attempt is over. The tree is terminated, the record dropped,
/// `MEMORY_LOST` appended, and a second loss with no readiness between makes
/// the row `UNAVAILABLE CHILD_FAILED`.
async fn lose(
    store: &Store,
    roots: &Roots,
    live: &Arc<Mutex<Live>>,
    generation: u64,
    key: &str,
    exit_code: Option<i64>,
) {
    let (child, pid, last, failed) = {
        let mut live = live.lock().await;
        if live.generation != generation
            || !matches!(live.state, LiveState::Starting | LiveState::Ready)
        {
            return;
        }
        let pid = live.pid.unwrap_or_default();
        let last = redact_key(key, &last_line(&live.output_tail));
        let losses = live.losses_since_ready.saturating_add(1);
        let child = live.child.take();
        let output_tail = std::mem::take(&mut live.output_tail);
        *live = Live {
            state: LiveState::Lost,
            output_tail,
            losses_since_ready: losses,
            generation,
            ..Live::default()
        };
        (child, pid, last, losses >= 2)
    };
    if let Some(mut child) = child {
        child.terminate().await;
    }
    crate::daemon::forget_child(roots, pid);

    let at_ns = now_ns();
    let recorded = store.unit(move |tx| {
        append_event(
            tx,
            "MEMORY_LOST",
            None,
            at_ns,
            json!({ "pid": pid, "exit_code": exit_code, "output_tail_last_line": &last }),
        )?;
        if failed {
            tx.execute(
                "UPDATE memory_child SET state = 'UNAVAILABLE', failure_code = 'CHILD_FAILED', \
                 failure_message = ?1 WHERE id = 1",
                params![last],
            )?;
            append_event(
                tx,
                "MEMORY_UNAVAILABLE",
                None,
                at_ns,
                json!({ "failure_code": "CHILD_FAILED", "failure_message": &last }),
            )?;
        }
        Ok(())
    });
    if let Err(e) = recorded {
        tracing::error!(error = %e, "recording the memory child's loss failed");
    }
}

/// The first line of a message the daemon reports (§4.3).
fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Banks and the operations (§4)
// ---------------------------------------------------------------------------

/// A desk's bank (§4.1): `desk-` plus the UUID without hyphens. Computed per
/// request, stored nowhere, and answered nowhere.
pub fn bank(desk_id: &str) -> String {
    format!("desk-{}", desk_id.replace('-', ""))
}

/// The stubbed half of the seam: C31 fills the three Hindsight calls.
/// Answering `MEMORY_UNCONFIGURED` is what a daemon that cannot start a child
/// would answer anyway.
fn pending<T>() -> Result<T, MemoryError> {
    Err(MemoryError::Unconfigured)
}

impl Memory {
    /// C31 (§4.2): `POST /v1/default/banks/<bank>/memories`.
    pub async fn retain(&self, bank: &str, body: Value) -> Result<Value, MemoryError> {
        let _ = (bank, body);
        pending()
    }

    /// C31 (§4.2): `POST /v1/default/banks/<bank>/memories/recall`.
    pub async fn recall(&self, bank: &str, body: Value) -> Result<Value, MemoryError> {
        let _ = (bank, body);
        pending()
    }

    /// C31 (§4.2): `POST /v1/default/banks/<bank>/reflect`.
    pub async fn reflect(&self, bank: &str, body: Value) -> Result<Value, MemoryError> {
        let _ = (bank, body);
        pending()
    }
}

// ---------------------------------------------------------------------------
// memory::discover and memory::provider (feature SPEC §8 checks 1 and 3)
// ---------------------------------------------------------------------------

/// A [`Memory`] on a scratch root, always on the file credential seam so no test
/// can reach this machine's keychain.
#[cfg(test)]
pub(crate) fn seam_memory(store: Store, roots: Roots) -> Memory {
    Memory {
        store,
        roots,
        seam: true,
        http: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        daemon_uuid: "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".to_string(),
        ready_deadline: READY_DEADLINE,
        live: Arc::new(Mutex::new(Live::default())),
    }
}

#[cfg(test)]
fn scratch() -> (tempfile::TempDir, Memory) {
    let dir = tempfile::tempdir().unwrap();
    let roots = Roots::resolve(Some(dir.path())).unwrap();
    roots.create_dirs().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    (dir, seam_memory(store, roots))
}

/// An executable that exits `code` after printing `out` on standard output and a
/// warning on standard error — the shape the real launcher's `--help` has.
#[cfg(test)]
fn probe_standin(dir: &Path, name: &str, code: i32, out: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join(format!("{name}.cmd"));
        fs::write(
            &path,
            format!("@echo off\r\necho {out}\r\necho a warning 1>&2\r\nexit /b {code}\r\n"),
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\necho '{out}'\necho 'a warning' >&2\nexit {code}\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

#[cfg(test)]
fn events(store: &Store) -> Vec<(String, Value)> {
    store
        .call(|c| {
            c.prepare(
                "SELECT kind, payload FROM operational_events WHERE kind LIKE 'MEMORY_%' \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .into_iter()
        .map(|(kind, payload)| (kind, serde_json::from_str(&payload).unwrap()))
        .collect()
}

#[cfg(test)]
#[test]
fn discovery_outcomes() {
    let (_dir, memory) = scratch();
    let store = memory.store.clone();
    let bin = tempfile::tempdir().unwrap();

    // The row starts UNCONFIGURED and nothing is live (§2.1, §6).
    let row = child_row(&store).unwrap();
    assert_eq!(row.state, "UNCONFIGURED");
    assert_eq!(row.live, LiveState::NotStarted);
    assert!(row.executable_path.is_none() && row.failure_code.is_none());

    // The marker on standard output with exit 0 is the whole capability check;
    // the warning on standard error must not fail it (§2.1).
    let good = probe_standin(
        bin.path(),
        "hindsight-api",
        0,
        "--port INTEGER [env var: HINDSIGHT_API_PORT]",
    );
    let row = discover(&store, &good).unwrap();
    assert_eq!(row.state, "AVAILABLE");
    assert_eq!(row.executable_path.as_deref(), Some(good.to_str().unwrap()));
    assert!(row.validated_at_ns.is_some());
    assert!(row.failure_code.is_none() && row.failure_message.is_none());

    // NOT_FOUND: nothing at the named path.
    let missing = bin.path().join("nothing-here");
    let row = discover(&store, &missing).unwrap();
    assert_eq!(
        (row.state.as_str(), row.failure_code.as_deref()),
        ("UNAVAILABLE", Some("NOT_FOUND"))
    );

    // CAPABILITY_MISSING: it answers, but names no HINDSIGHT_API_PORT.
    let thin = probe_standin(bin.path(), "thin", 0, "--port INTEGER");
    let row = discover(&store, &thin).unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("CAPABILITY_MISSING"));

    // PROBE_FAILED: it prints the marker but exits non-zero.
    let broken = probe_standin(bin.path(), "broken", 3, "HINDSIGHT_API_PORT");
    let row = discover(&store, &broken).unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("PROBE_FAILED"));

    // Retry re-validates the recorded launcher and clears a failure the child's
    // own loss wrote (§2.1, §2.3).
    discover(&store, &good).unwrap();
    store
        .unit(|tx| {
            tx.execute(
                "UPDATE memory_child SET state = 'UNAVAILABLE', failure_code = 'CHILD_FAILED', \
                 failure_message = 'it exited' WHERE id = 1",
                [],
            )
        })
        .unwrap();
    let row = retry(&store).unwrap();
    assert_eq!(row.state, "AVAILABLE");
    assert!(row.failure_code.is_none() && row.failure_message.is_none());

    // Step 6b re-validates an AVAILABLE row: the launcher is gone, so the row
    // goes UNAVAILABLE without failing the start (§2.1).
    fs::remove_file(&good).unwrap();
    revalidate_available(&store).unwrap();
    assert_eq!(
        child_row(&store).unwrap().failure_code.as_deref(),
        Some("NOT_FOUND")
    );

    // Every change left one event naming what changed, and no answer or event
    // carries anything but the row.
    let seen = events(&store);
    assert_eq!(seen.len(), 7);
    assert!(
        seen.iter()
            .all(|(kind, payload)| kind == "MEMORY_CONFIGURED"
                && payload["what"] == "child"
                && payload.get("executable_path").is_some())
    );
    assert_eq!(
        seen.iter()
            .map(|(_, p)| p["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "AVAILABLE",
            "UNAVAILABLE",
            "UNAVAILABLE",
            "UNAVAILABLE",
            "AVAILABLE",
            "AVAILABLE",
            "UNAVAILABLE",
        ]
    );
}

/// Step 6b leaves a row it does not own alone: an `UNCONFIGURED` or
/// `UNAVAILABLE` row is never re-probed at startup (§2.1).
#[cfg(test)]
#[test]
fn revalidation_touches_only_available() {
    let (_dir, memory) = scratch();
    revalidate_available(&memory.store).unwrap();
    assert_eq!(child_row(&memory.store).unwrap().state, "UNCONFIGURED");
    assert!(events(&memory.store).is_empty());

    let bin = tempfile::tempdir().unwrap();
    let missing = bin.path().join("gone");
    discover(&memory.store, &missing).unwrap();
    revalidate_available(&memory.store).unwrap();
    assert_eq!(events(&memory.store).len(), 1, "no second probe");
}

#[cfg(test)]
#[test]
fn bank_is_the_desk_uuid_without_hyphens() {
    assert_eq!(
        bank("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"),
        "desk-0199a1b2c3d47e5f8a9b0c1d2e3f4a5b"
    );
    assert_eq!(bank("0199a1b2c3d4").len(), "desk-".len() + 12);
}

#[cfg(test)]
const FAKE_KEY: &str = "sk-marketrig-fake-0123456789abcdef";

#[cfg(test)]
fn request(base_url: &str, api_key: Option<&str>, llm: &str, embedding: &str) -> ProviderRequest {
    ProviderRequest {
        base_url: base_url.to_string(),
        api_key: api_key.map(str::to_string),
        llm_model: llm.to_string(),
        embedding_model: embedding.to_string(),
    }
}

#[cfg(test)]
#[tokio::test]
async fn provider_settings() {
    let (_dir, memory) = scratch();

    // Validation refuses every shape §3 names, and writes nothing.
    for bad in [
        "/relative",
        "ftp://host",
        "http://",
        "http:///v1",
        "http://user:pw@host",
        "http://host/v1?key=1",
        "http://host/v1#f",
        "http://ho st",
    ] {
        let err = memory
            .put_provider(request(bad, Some(FAKE_KEY), "llm-1", "emb-1"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "VALIDATION", "{bad} must be refused");
    }
    for (llm, embedding) in [
        ("", "emb-1"),
        ("llm-1", ""),
        ("x".repeat(129).as_str(), "e"),
    ] {
        let err = memory
            .put_provider(request("http://host/v1", Some(FAKE_KEY), llm, embedding))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "VALIDATION");
    }
    let err = memory
        .put_provider(request("http://host/v1", Some(""), "llm-1", "emb-1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "VALIDATION");
    assert!(provider_row(&memory.store).unwrap().base_url.is_none());
    assert!(events(&memory.store).is_empty());
    assert!(memory.load_key().unwrap().is_none());

    // A good PUT: the trailing slash goes, the key reaches the store, the row
    // and the event follow, and the key is in neither.
    let provider = memory
        .put_provider(request(
            "http://127.0.0.1:9/v1/",
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
    assert_eq!(provider.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
    assert_eq!(provider.llm_model.as_deref(), Some("llm-1"));
    assert!(provider.api_key_present);
    assert!(provider.embedding_locked_at_ns.is_none());
    assert_eq!(memory.load_key().unwrap().as_deref(), Some(FAKE_KEY));
    assert!(memory.key_present());
    assert_eq!(
        serde_json::to_value(&provider).unwrap()["api_key"],
        Value::Null
    );

    // The seam file is the only place the key lands, and it is 0600.
    let credentials = memory.roots.runtime().join(CREDENTIALS);
    assert!(fs::read_to_string(&credentials).unwrap().contains(FAKE_KEY));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&credentials).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // api_key omitted keeps the stored key and still changes the models.
    let provider = memory
        .put_provider(request("http://127.0.0.1:9/v1", None, "llm-2", "emb-1"))
        .await
        .unwrap();
    assert!(provider.api_key_present);
    assert_eq!(provider.llm_model.as_deref(), Some("llm-2"));
    assert_eq!(memory.load_key().unwrap().as_deref(), Some(FAKE_KEY));

    // The embedding lock: stamped by the first retain (§4.2, C31), enforced here.
    memory
        .store
        .unit(|tx| {
            tx.execute(
                "UPDATE memory_provider SET embedding_locked_at_ns = 42 WHERE id = 1",
                [],
            )
        })
        .unwrap();
    let err = memory
        .put_provider(request("http://127.0.0.1:9/v1", None, "llm-3", "emb-2"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "EMBEDDING_MODEL_LOCKED");
    assert_eq!(
        provider_row(&memory.store)
            .unwrap()
            .llm_model
            .as_deref()
            .unwrap(),
        "llm-2",
        "a refused PUT changes nothing"
    );
    // The same embedding model with a new LLM model still passes.
    let provider = memory
        .put_provider(request("http://127.0.0.1:9/v1", None, "llm-3", "emb-1"))
        .await
        .unwrap();
    assert_eq!(provider.llm_model.as_deref(), Some("llm-3"));

    // Three accepted PUTs, three events, and the key in none of them.
    let seen = events(&memory.store);
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|(kind, payload)| {
        kind == "MEMORY_CONFIGURED"
            && payload["what"] == "provider"
            && payload["base_url"] == "http://127.0.0.1:9/v1"
            && !payload.to_string().contains(FAKE_KEY)
    }));

    // Redaction (§4.3): what the child quotes back never leaves as the key.
    assert_eq!(
        memory.redact(&format!("Incorrect API key provided: {FAKE_KEY}.")),
        format!("Incorrect API key provided: {REDACTED}.")
    );
    assert_eq!(memory.redact("nothing to hide"), "nothing to hide");
}

/// A credential store that cannot take the key writes nothing at all (§3).
#[cfg(test)]
#[tokio::test]
async fn credential_store_unavailable_writes_nothing() {
    let (_dir, memory) = scratch();
    // A directory where the file belongs: the write fails on both platforms.
    fs::create_dir_all(memory.roots.runtime().join(CREDENTIALS)).unwrap();

    let err = memory
        .put_provider(request(
            "http://127.0.0.1:9/v1",
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CREDENTIAL_STORE_UNAVAILABLE");

    let row = provider_row(&memory.store).unwrap();
    assert!(row.base_url.is_none() && !row.api_key_present);
    assert!(events(&memory.store).is_empty());
}

/// The model list is fetched at request time and never cached (§3).
#[cfg(test)]
#[tokio::test]
async fn models_are_live_and_never_cached() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_dir, memory) = scratch();

    // A provider stand-in: the bearer is required, and `failing` switches it
    // between 500 and the list.
    let failing = Arc::new(AtomicBool::new(true));
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get({
            let failing = failing.clone();
            move |headers: axum::http::HeaderMap| {
                let failing = failing.clone();
                async move {
                    use axum::response::IntoResponse;
                    let presented = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok());
                    if presented != Some(&format!("Bearer {FAKE_KEY}")) {
                        return (axum::http::StatusCode::UNAUTHORIZED, "no key").into_response();
                    }
                    if failing.load(Ordering::Relaxed) {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom")
                            .into_response();
                    }
                    axum::Json(
                        json!({"data": [{"id": "stand-in-llm"}, {"id": "stand-in-embedding"}]}),
                    )
                    .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // No provider at all, then a provider with no key: both MEMORY_UNCONFIGURED.
    assert_eq!(
        memory.models().await.unwrap_err().code(),
        "MEMORY_UNCONFIGURED"
    );
    memory
        .put_provider(request(
            &format!("http://127.0.0.1:{port}/v1"),
            None,
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
    assert_eq!(
        memory.models().await.unwrap_err().code(),
        "MEMORY_UNCONFIGURED"
    );

    // With the key: the stand-in's failure is PROVIDER_UNREACHABLE, and the
    // next call after it recovers answers the fresh list in the provider's order.
    memory
        .put_provider(request(
            &format!("http://127.0.0.1:{port}/v1"),
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
    let err = memory.models().await.unwrap_err();
    assert_eq!(err.code(), "PROVIDER_UNREACHABLE");
    assert!(err.to_string().contains("500"), "{err}");
    failing.store(false, Ordering::Relaxed);
    assert_eq!(
        memory.models().await.unwrap(),
        ["stand-in-llm", "stand-in-embedding"]
    );
    // And nothing about the list was persisted.
    assert!(
        provider_row(&memory.store)
            .unwrap()
            .embedding_locked_at_ns
            .is_none()
    );

    // A dead port is a transport failure, not a status (§3).
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    memory
        .put_provider(request(
            &format!("http://127.0.0.1:{dead_port}/v1"),
            None,
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
    assert_eq!(
        memory.models().await.unwrap_err().code(),
        "PROVIDER_UNREACHABLE"
    );
}

/// The provider half of the child's environment (§2.2), and the stubs C30 and
/// C31 fill in.
#[cfg(test)]
#[tokio::test]
async fn provider_env_and_pending_operations() {
    let (_dir, memory) = scratch();
    assert!(matches!(
        memory.provider_env().unwrap_err(),
        MemoryError::Unconfigured
    ));

    memory
        .put_provider(request(
            "http://127.0.0.1:9/v1",
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
    let env = memory.provider_env().unwrap();
    let value = |name: &str| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("{name} must be composed"))
            .to_string()
    };
    assert_eq!(value("HINDSIGHT_API_LLM_PROVIDER"), "openai");
    assert_eq!(value("HINDSIGHT_API_LLM_BASE_URL"), "http://127.0.0.1:9/v1");
    assert_eq!(value("HINDSIGHT_API_LLM_API_KEY"), FAKE_KEY);
    assert_eq!(value("HINDSIGHT_API_LLM_MODEL"), "llm-1");
    assert_eq!(value("HINDSIGHT_API_EMBEDDINGS_PROVIDER"), "openai");
    assert_eq!(
        value("HINDSIGHT_API_EMBEDDINGS_OPENAI_BASE_URL"),
        "http://127.0.0.1:9/v1"
    );
    assert_eq!(value("HINDSIGHT_API_EMBEDDINGS_OPENAI_API_KEY"), FAKE_KEY);
    assert_eq!(value("HINDSIGHT_API_EMBEDDINGS_OPENAI_MODEL"), "emb-1");
    assert_eq!(env.len(), 8);

    // The seam C30 and C31 build on, answering rather than panicking until then.
    assert!(memory.ensure_ready().await.is_err());
    memory.stop_child().await;
    for pending in [
        memory.retain("desk-1", json!({})).await,
        memory.recall("desk-1", json!({})).await,
        memory.reflect("desk-1", json!({})).await,
    ] {
        assert_eq!(pending.unwrap_err().code(), "MEMORY_UNCONFIGURED");
    }

    // The output tail keeps only the newest 4 KiB (§2.2).
    let mut live = memory.live.lock().await;
    live.push_output(&vec![b'a'; TAIL]);
    live.push_output(b"tail");
    assert_eq!(live.output_tail.len(), TAIL);
    assert!(live.output_tail.ends_with(b"tail"));
}

// ---------------------------------------------------------------------------
// memory::child (feature SPEC §8 check 2)
// ---------------------------------------------------------------------------

/// The fake `hindsight-api`'s env dump, written into the child's own `HOME` so
/// the check reads it back and sees `HOME` was redirected at the same time.
#[cfg(test)]
const FAKE_ENV: &str = "env.json";

/// The last line the fake prints before exiting, so `MEMORY_LOST` has something
/// exact to carry (§2.3).
#[cfg(test)]
const FAKE_LAST_LINE: &str = "hindsight-api stopping";

/// The in-process fake `hindsight-api` (§8 check 2). This is a test only when a
/// launcher re-executes this binary as the memory child: a plain suite run has
/// no `HINDSIGHT_API_PORT` on its environment and it returns at once.
#[cfg(test)]
#[test]
fn fake_hindsight_main() {
    let Ok(port) = std::env::var("HINDSIGHT_API_PORT") else {
        return;
    };
    let host = std::env::var("HINDSIGHT_API_HOST").expect("HINDSIGHT_API_HOST");
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let dump: BTreeMap<String, String> = std::env::vars().collect();
    fs::write(home.join(FAKE_ENV), serde_json::to_vec(&dump).unwrap()).unwrap();

    // The two knobs the launcher script sets for itself, so the environment the
    // daemon composed stays exactly §2.2's.
    let ms = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let healthy_after = Duration::from_millis(ms("MARKETRIG_FAKE_HEALTH_AFTER_MS").unwrap_or(0));
    let exit_after = ms("MARKETRIG_FAKE_EXIT_AFTER_MS");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            if let Some(after) = exit_after {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(after)).await;
                    println!("{FAKE_LAST_LINE}");
                    let _ = io::stdout().flush();
                    // Straight out, so the harness's own summary is not the
                    // last line of the tail.
                    std::process::exit(1);
                });
            }
            let started = tokio::time::Instant::now();
            let app = axum::Router::new().route(
                "/health",
                axum::routing::get(move || async move {
                    if started.elapsed() < healthy_after {
                        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "starting")
                    } else {
                        (axum::http::StatusCode::OK, "{\"status\":\"healthy\"}")
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
                .await
                .unwrap();
            println!("hindsight-api listening on {host}:{port}");
            let _ = io::stdout().flush();
            let _ = axum::serve(listener, app).await;
        });
}

/// A launcher that answers the `--help` probe the way the real one does and
/// otherwise re-executes this test binary as [`fake_hindsight_main`].
#[cfg(test)]
fn fake_launcher(
    dir: &Path,
    name: &str,
    health_after_ms: u64,
    exit_after_ms: Option<u64>,
) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let exe = exe.display().to_string();
    let exit = exit_after_ms.map(|ms| ms.to_string()).unwrap_or_default();
    #[cfg(windows)]
    {
        let path = dir.join(format!("{name}.cmd"));
        fs::write(
            &path,
            format!(
                "@echo off\r\n\
                 if \"%~1\"==\"--help\" (\r\n\
                 echo --port INTEGER [env var: HINDSIGHT_API_PORT]\r\n\
                 exit /b 0\r\n\
                 )\r\n\
                 set MARKETRIG_FAKE_HEALTH_AFTER_MS={health_after_ms}\r\n\
                 set MARKETRIG_FAKE_EXIT_AFTER_MS={exit}\r\n\
                 \"{exe}\" \"memory::fake_hindsight_main\" --exact --nocapture\r\n"
            ),
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"--help\" ]; then\n\
                 echo '--port INTEGER [env var: HINDSIGHT_API_PORT]'\n\
                 echo 'sentence-transformers is not installed' >&2\n\
                 exit 0\n\
                 fi\n\
                 MARKETRIG_FAKE_HEALTH_AFTER_MS={health_after_ms} \
                 MARKETRIG_FAKE_EXIT_AFTER_MS={exit} \
                 exec '{exe}' 'memory::fake_hindsight_main' --exact --nocapture\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

/// An `AVAILABLE` child row naming `launcher` and a complete provider.
#[cfg(test)]
async fn configured(memory: &Memory, launcher: &Path) {
    discover(&memory.store, launcher).unwrap();
    memory
        .put_provider(request(
            "http://127.0.0.1:9/v1",
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
}

#[cfg(test)]
async fn wait_for(memory: &Memory, state: LiveState) {
    for _ in 0..100 {
        if memory.live.lock().await.state == state {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the memory child never reached {state:?}");
}

#[cfg(test)]
fn children(memory: &Memory) -> Vec<Value> {
    let raw = match fs::read(crate::daemon::children_path(&memory.roots)) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    serde_json::from_slice::<Value>(&raw).unwrap()["children"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The composed environment, readiness, the per-start bearer, the record, and
/// the two ways a live child stops (§2.2, §2.3).
#[cfg(test)]
#[tokio::test]
async fn child_launch_environment_and_stop() {
    let (_dir, mut memory) = scratch();
    memory.ready_deadline = Duration::from_secs(20);
    let bin = tempfile::tempdir().unwrap();
    configured(
        &memory,
        &fake_launcher(bin.path(), "hindsight-api", 0, None),
    )
    .await;

    // Two operations arriving together share one start (§2.2).
    let (first, second) = tokio::join!(memory.ensure_ready(), memory.ensure_ready());
    let (port, bearer) = first.unwrap();
    assert_eq!(second.unwrap(), (port, bearer.clone()));
    assert_eq!(bearer.len(), 64, "32 random bytes as hex");
    let child = memory.child().await.unwrap();
    assert_eq!(child.live, LiveState::Ready);
    assert!(child.pid.is_some_and(|pid| pid > 0));

    // §2.2's environment, variable for variable.
    let home = memory.roots.data.join(HINDSIGHT_HOME);
    let read_env = || -> BTreeMap<String, String> {
        serde_json::from_slice(&fs::read(home.join(FAKE_ENV)).unwrap()).unwrap()
    };
    let env = read_env();
    for (key, value) in [
        ("HINDSIGHT_API_HOST", "127.0.0.1"),
        ("HINDSIGHT_API_PORT", &port.to_string()),
        ("HINDSIGHT_API_WORKERS", "1"),
        ("HINDSIGHT_API_LOG_LEVEL", "warning"),
        ("HINDSIGHT_API_DATABASE_URL", "pg0://marketrig"),
        (
            "HINDSIGHT_API_TENANT_EXTENSION",
            "hindsight_api.extensions.builtin.tenant:ApiKeyTenantExtension",
        ),
        ("HINDSIGHT_API_TENANT_API_KEY", &bearer),
        ("HINDSIGHT_API_MCP_ENABLED", "false"),
        ("HINDSIGHT_API_OTEL_TRACES_ENABLED", "false"),
        ("HINDSIGHT_API_RERANKER_PROVIDER", "rrf"),
        ("HINDSIGHT_API_LLM_PROVIDER", "openai"),
        ("HINDSIGHT_API_LLM_BASE_URL", "http://127.0.0.1:9/v1"),
        ("HINDSIGHT_API_LLM_API_KEY", FAKE_KEY),
        ("HINDSIGHT_API_LLM_MODEL", "llm-1"),
        ("HINDSIGHT_API_EMBEDDINGS_PROVIDER", "openai"),
        (
            "HINDSIGHT_API_EMBEDDINGS_OPENAI_BASE_URL",
            "http://127.0.0.1:9/v1",
        ),
        ("HINDSIGHT_API_EMBEDDINGS_OPENAI_API_KEY", FAKE_KEY),
        ("HINDSIGHT_API_EMBEDDINGS_OPENAI_MODEL", "emb-1"),
        ("HOME", home.to_str().unwrap()),
    ] {
        assert_eq!(env.get(key).map(String::as_str), Some(value), "{key}");
    }
    assert!(env.contains_key("PATH"));
    // Nothing of the daemon's own rides along: what this process carries and
    // §2.2 does not name is absent from the child's.
    for canary in ["CARGO_MANIFEST_DIR", "CARGO_PKG_NAME", "RUSTUP_HOME"] {
        if std::env::var_os(canary).is_some() {
            assert!(!env.contains_key(canary), "{canary} leaked into the child");
        }
    }
    assert!(!env.contains_key("MARKETRIG_DESK_ID"));

    // The record lives as long as the child does (§2.2, per D73).
    let recorded = children(&memory);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["kind"], "memory");
    assert_eq!(recorded[0]["pid"].as_u64(), child.pid.map(u64::from));

    // A provider change stops the live child so the next start carries the new
    // environment (§2.3), and mints a fresh bearer on a fresh port (§2.2).
    memory
        .put_provider(request("http://127.0.0.1:9/v1", None, "llm-2", "emb-1"))
        .await
        .unwrap();
    assert_eq!(memory.child().await.unwrap().live, LiveState::NotStarted);
    assert!(children(&memory).is_empty());

    let (next_port, next_bearer) = memory.ensure_ready().await.unwrap();
    assert_ne!(next_bearer, bearer, "the bearer is per start");
    assert_ne!(next_port, port);
    let env = read_env();
    assert_eq!(
        env.get("HINDSIGHT_API_TENANT_API_KEY").map(String::as_str),
        Some(next_bearer.as_str())
    );
    assert_eq!(
        env.get("HINDSIGHT_API_LLM_MODEL").map(String::as_str),
        Some("llm-2")
    );

    memory.stop_child().await;
    assert_eq!(memory.child().await.unwrap().live, LiveState::NotStarted);
    assert!(children(&memory).is_empty());

    // Two starts, no loss, and no secret in any event.
    let seen = events(&memory.store);
    let started: Vec<_> = seen
        .iter()
        .filter(|(kind, _)| kind == "MEMORY_STARTED")
        .collect();
    assert_eq!(
        started.len(),
        2,
        "one start each, and none for a live child"
    );
    assert!(
        started
            .iter()
            .all(|(_, p)| p["pid"].as_u64().is_some_and(|pid| pid > 0))
    );
    assert!(!seen.iter().any(|(kind, _)| kind == "MEMORY_LOST"));
    let payloads = serde_json::to_string(&seen).unwrap();
    assert!(!payloads.contains(&bearer) && !payloads.contains(&next_bearer));
    assert!(!payloads.contains(FAKE_KEY));
    // The bearer is held in memory only: no row of the database file carries it.
    for entry in fs::read_dir(&memory.roots.data).unwrap() {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().contains("marketrig.sqlite3") {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        for secret in [&bearer, &next_bearer] {
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                "the bearer must never reach {}",
                path.display()
            );
        }
    }
}

/// A child that never answers `/health` is lost at the deadline, not left
/// running (§2.2, §2.3).
#[cfg(test)]
#[tokio::test]
async fn readiness_deadline_is_a_loss() {
    let (_dir, mut memory) = scratch();
    memory.ready_deadline = Duration::from_millis(900);
    let bin = tempfile::tempdir().unwrap();
    configured(&memory, &fake_launcher(bin.path(), "slow", 60_000, None)).await;

    let err = memory.ensure_ready().await.unwrap_err();
    assert_eq!(err.code(), "MEMORY_UNAVAILABLE");
    assert_eq!(memory.child().await.unwrap().live, LiveState::Lost);
    assert!(children(&memory).is_empty());

    let seen = events(&memory.store);
    assert!(!seen.iter().any(|(kind, _)| kind == "MEMORY_STARTED"));
    let (_, lost) = seen
        .iter()
        .find(|(kind, _)| kind == "MEMORY_LOST")
        .expect("MEMORY_LOST");
    assert!(lost["pid"].as_u64().is_some_and(|pid| pid > 0));
    assert!(lost["exit_code"].is_null(), "the deadline killed it");
    assert!(lost["output_tail_last_line"].is_string());
    // One loss is never the row's failure (§2.3).
    assert_eq!(child_row(&memory.store).unwrap().state, "AVAILABLE");
}

/// Loss, the one restart, `UNAVAILABLE CHILD_FAILED`, and retry (§2.3).
#[cfg(test)]
#[tokio::test]
async fn loss_then_one_restart_then_child_failed() {
    let (_dir, mut memory) = scratch();
    memory.ready_deadline = Duration::from_secs(20);
    let bin = tempfile::tempdir().unwrap();
    // Long enough to answer `/health` on the poll after the first (§2.2), then
    // exit under the daemon.
    let dying = fake_launcher(bin.path(), "dying", 0, Some(2_000));
    let healthy = fake_launcher(bin.path(), "healthy", 0, None);
    let doomed = fake_launcher(bin.path(), "doomed", 60_000, Some(0));
    configured(&memory, &dying).await;

    // Ready, then the child exits under it.
    memory.ensure_ready().await.unwrap();
    let pid = memory.child().await.unwrap().pid.unwrap();
    wait_for(&memory, LiveState::Lost).await;
    let (_, lost) = events(&memory.store)
        .into_iter()
        .find(|(kind, _)| kind == "MEMORY_LOST")
        .expect("MEMORY_LOST");
    assert_eq!(lost["pid"].as_u64(), Some(u64::from(pid)));
    assert_eq!(lost["exit_code"].as_i64(), Some(1));
    assert_eq!(lost["output_tail_last_line"], FAKE_LAST_LINE);
    assert!(children(&memory).is_empty());
    assert_eq!(child_row(&memory.store).unwrap().state, "AVAILABLE");

    // The next operation starts it again, and a readiness clears the count.
    discover(&memory.store, &healthy).unwrap();
    memory.ensure_ready().await.unwrap();
    assert_eq!(memory.live.lock().await.losses_since_ready, 0);
    memory.stop_child().await;

    // Two losses with no readiness between: the row carries the child's own
    // last line and every operation answers MEMORY_UNAVAILABLE (§2.3).
    discover(&memory.store, &doomed).unwrap();
    memory.ready_deadline = Duration::from_secs(5);
    assert_eq!(
        memory.ensure_ready().await.unwrap_err().code(),
        "MEMORY_UNAVAILABLE"
    );
    assert_eq!(child_row(&memory.store).unwrap().state, "AVAILABLE");
    assert_eq!(
        memory.ensure_ready().await.unwrap_err().code(),
        "MEMORY_UNAVAILABLE"
    );
    let row = child_row(&memory.store).unwrap();
    assert_eq!(
        (row.state.as_str(), row.failure_code.as_deref()),
        ("UNAVAILABLE", Some("CHILD_FAILED"))
    );
    assert_eq!(row.failure_message.as_deref(), Some(FAKE_LAST_LINE));
    assert!(
        events(&memory.store)
            .iter()
            .any(|(kind, _)| kind == "MEMORY_UNAVAILABLE")
    );
    // The row now answers before anything is spawned.
    let err = memory.ensure_ready().await.unwrap_err();
    assert_eq!(err.code(), "MEMORY_UNAVAILABLE");
    assert_eq!(err.to_string(), FAKE_LAST_LINE);

    // Retry clears CHILD_FAILED, re-validates, and starts the count again.
    memory.retry().await.unwrap();
    let row = child_row(&memory.store).unwrap();
    assert_eq!(row.state, "AVAILABLE");
    assert!(row.failure_code.is_none() && row.failure_message.is_none());
    assert_eq!(memory.live.lock().await.losses_since_ready, 0);
}
