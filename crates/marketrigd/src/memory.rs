//! The Hindsight memory child, its provider settings, and the desk-scoped
//! operations.
//!
//! Contract: `sdd/features/r4-memory-skills-loop/SPEC.md` §2 (the child, per
//! R4-1), §3 (the provider, per R4-2), §4 (banks and operations, per R4-3),
//! root `sdd/SPEC.md` §16.
//!
//! The child's launch, readiness, and loss are C30's; the desk-scoped routes
//! and the Hindsight request mappings are C31's. This module owns the two
//! installation rows, the discovery probe, the credential seam, and the
//! provider routes, and holds the seam every other memory chunk builds on.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
    pub live: tokio::sync::Mutex<Live>,
    /// §4.3's per-operation budgets, which a `STARTING` child's readiness wait
    /// spends too: retain and reflect share [`LONG_TIMEOUT`], recall has
    /// [`RECALL_TIMEOUT`]. Fields rather than constants so a check can reach the
    /// timeout path in milliseconds instead of a minute.
    pub long_timeout: Duration,
    pub recall_timeout: Duration,
}

/// Retain and reflect (§4.3).
const LONG_TIMEOUT: Duration = Duration::from_secs(180);
/// Recall (§4.3).
const RECALL_TIMEOUT: Duration = Duration::from_secs(60);

impl Memory {
    pub fn new(store: Store, roots: Roots) -> io::Result<Memory> {
        Ok(Memory {
            store,
            roots,
            seam: std::env::var_os(crate::store::TEST_DATA_ROOT_ENV).is_some(),
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(io::Error::other)?,
            live: tokio::sync::Mutex::new(Live::default()),
            long_timeout: LONG_TIMEOUT,
            recall_timeout: RECALL_TIMEOUT,
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
    /// The desk's own refusal on a desk-scoped route — `DESK_NOT_FOUND`,
    /// `DESK_NOT_READY`, or `ATTRIBUTION_INVALID` (§4.3), all decided by the
    /// order routes' own checks and answered through their own map.
    Desk(crate::trade::TradeError),
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
            MemoryError::Desk(e) => e.code(),
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
            MemoryError::Desk(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for MemoryError {}

/// ponytail: a database failure surfaces as `MEMORY_ERROR`, because §4.3 gives
/// this group no internal code and the daemon's SQLite is in-process and
/// single-writer, so this path is unreachable in practice. Give it its own
/// variant the day a memory route can genuinely fail on the store.
impl From<crate::trade::TradeError> for MemoryError {
    fn from(e: crate::trade::TradeError) -> Self {
        MemoryError::Desk(e)
    }
}

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
            Ok(Some(key)) if !key.is_empty() => message.replace(&key, REDACTED),
            _ => message.to_string(),
        }
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

/// The stubbed half of the seam: C30 fills the child's lifecycle. Answering
/// `MEMORY_UNCONFIGURED` is what a daemon that cannot start a child would
/// answer anyway.
fn pending<T>() -> Result<T, MemoryError> {
    Err(MemoryError::Unconfigured)
}

/// The §4.3 limits. `content`, `context`, and `query` are byte bounds; a tag is
/// counted in characters.
const CONTENT_MAX: usize = 64 * 1024;
const CONTEXT_MAX: usize = 4 * 1024;
const QUERY_MAX: usize = 8 * 1024;
const TAGS_MAX: usize = 16;
const TAG_MAX: usize = 64;

/// `POST /desks/{d}/memory/retain`'s body (§4.2).
#[derive(Debug, Default, Deserialize)]
pub struct RetainRequest {
    pub content: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// `POST /desks/{d}/memory/recall`'s body (§4.2).
#[derive(Debug, Default, Deserialize)]
pub struct RecallRequest {
    pub query: String,
    #[serde(default)]
    pub budget: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// `POST /desks/{d}/memory/reflect`'s body (§4.2).
#[derive(Debug, Default, Deserialize)]
pub struct ReflectRequest {
    pub query: String,
    #[serde(default)]
    pub budget: Option<String>,
}

/// One bounded text field (§4.3); `min` is `1` for a field that must carry
/// something and `0` for one that may be empty.
fn bounded(field: &str, value: &str, min: usize, max: usize) -> Result<(), MemoryError> {
    if value.len() < min || value.len() > max {
        return Err(MemoryError::Validation(format!(
            "The field {field} must be {min} to {max} bytes; this one is {}.",
            value.len()
        )));
    }
    Ok(())
}

/// At most sixteen tags of at most sixty-four characters each, or none (§4.3).
fn checked_tags(raw: Option<Vec<String>>) -> Result<Vec<String>, MemoryError> {
    let tags = raw.unwrap_or_default();
    if tags.len() > TAGS_MAX {
        return Err(MemoryError::Validation(format!(
            "At most {TAGS_MAX} tags are allowed; this request carries {}.",
            tags.len()
        )));
    }
    if tags
        .iter()
        .any(|tag| tag.is_empty() || tag.chars().count() > TAG_MAX)
    {
        return Err(MemoryError::Validation(format!(
            "Each tag must be 1 to {TAG_MAX} characters."
        )));
    }
    Ok(tags)
}

/// `low | mid | high`, `mid` by omission (§4.3).
fn checked_budget(raw: Option<String>) -> Result<String, MemoryError> {
    let budget = raw.unwrap_or_else(|| "mid".to_string());
    if !["low", "mid", "high"].contains(&budget.as_str()) {
        return Err(MemoryError::Validation(
            "The budget must be low, mid, or high.".to_string(),
        ));
    }
    Ok(budget)
}

/// What the daemon reads back from the child. Only the fields §4.2 answers are
/// named, so Hindsight's own API never becomes product contract (root §3);
/// every one defaults to `null`, so a thinner answer is still readable.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RetainAnswer {
    items_count: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RecallAnswer {
    results: Vec<Remembered>,
}

/// One recall result, exactly the eight fields §4.2 answers.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct Remembered {
    id: Value,
    text: Value,
    #[serde(rename = "type")]
    kind: Value,
    context: Value,
    tags: Value,
    metadata: Value,
    occurred_start: Value,
    mentioned_at: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReflectAnswer {
    text: Value,
    based_on: BasedOn,
}

/// The child nests its citations under `memories`; §4.2 answers the list itself.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BasedOn {
    memories: Vec<Cited>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct Cited {
    id: Value,
    text: Value,
    #[serde(rename = "type")]
    kind: Value,
}

/// One English sentence carrying the child's own words, whose full stop the
/// child may already have written.
fn sentence(prefix: &str, detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        return format!("{prefix}.");
    }
    let stop = if detail.ends_with(['.', '!', '?']) {
        ""
    } else {
        "."
    };
    format!("{prefix}: {detail}{stop}")
}

/// Who a retain is attributed to (§4.2). The order routes' own header
/// validation decides; a session's retain is called `INTERACTIVE`.
fn attribution(source: &crate::trade::Source) -> (&'static str, Option<&str>, Option<&str>) {
    match source {
        crate::trade::Source::Session => ("INTERACTIVE", None, None),
        crate::trade::Source::Trigger {
            trigger_id,
            firing_id,
        } => ("TRIGGER", Some(trigger_id), Some(firing_id)),
    }
}

/// The child's own route (§4.2): one tenant, the derived bank, the suffix.
fn route(port: u16, bank: &str, suffix: &str) -> String {
    format!("http://127.0.0.1:{port}/v1/default/banks/{bank}{suffix}")
}

impl Memory {
    /// C30 (§2.2): start the child if none is live, wait for `/health`, and
    /// answer the port and the per-start bearer.
    pub async fn ensure_ready(&self) -> Result<(u16, String), MemoryError> {
        pending()
    }

    /// C30 (§2.3): stop a live child. A provider change and Quit both call it.
    pub async fn stop_child(&self) {}

    /// The endpoint one operation talks to. A child that is already ready
    /// answers straight away and a row the loss rule has failed refuses here
    /// (§4.3); starting one is [`Self::ensure_ready`]'s, inside the caller's own
    /// budget.
    async fn endpoint(&self) -> Result<(u16, String), MemoryError> {
        {
            let live = self.live.lock().await;
            if live.state == LiveState::Ready
                && let (Some(port), Some(bearer)) = (live.port, live.bearer.clone())
            {
                return Ok((port, bearer));
            }
        }
        let row = child_row(&self.store)?;
        if row.state == "UNAVAILABLE" {
            return Err(MemoryError::Unavailable(sentence(
                "The memory child is unavailable",
                row.failure_message
                    .as_deref()
                    .or(row.failure_code.as_deref())
                    .unwrap_or("no reason was recorded"),
            )));
        }
        self.ensure_ready().await
    }

    /// One call to the child (§4.2). The readiness wait and the request share
    /// the operation's own budget (§4.3), and every message lifted out of the
    /// answer is redacted before it can leave.
    async fn call(
        &self,
        bank: &str,
        suffix: &str,
        body: Value,
        budget: Duration,
    ) -> Result<Value, MemoryError> {
        let attempt = async {
            let (port, bearer) = self.endpoint().await?;
            let response = self
                .http
                .post(route(port, bank, suffix))
                .bearer_auth(bearer)
                .timeout(budget)
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        MemoryError::Timeout
                    } else {
                        MemoryError::Error(self.redact(&sentence(
                            "The memory child could not be reached",
                            &first_line(&e.to_string()),
                        )))
                    }
                })?;
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status.is_success() {
                return serde_json::from_str(&text).map_err(|e| {
                    MemoryError::Error(sentence(
                        "The memory child answered a body MarketRig cannot read",
                        &e.to_string(),
                    ))
                });
            }
            // FastAPI's own failure shape; anything else leaves the status.
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|body| Some(body.get("detail")?.as_str()?.to_string()));
            Err(if status.is_client_error() {
                MemoryError::Rejected(self.redact(&sentence(
                    "The memory child refused the operation",
                    &detail.unwrap_or_else(|| format!("HTTP {status}")),
                )))
            } else {
                MemoryError::Error(self.redact(&sentence(
                    &format!("The memory child answered HTTP {status}"),
                    &first_line(detail.as_deref().unwrap_or(&text)),
                )))
            })
        };
        tokio::time::timeout(budget, attempt)
            .await
            .unwrap_or(Err(MemoryError::Timeout))
    }

    /// C31 (§4.2): `POST /v1/default/banks/<bank>/memories`.
    pub async fn retain(&self, bank: &str, body: Value) -> Result<Value, MemoryError> {
        self.call(bank, "/memories", body, self.long_timeout).await
    }

    /// C31 (§4.2): `POST /v1/default/banks/<bank>/memories/recall`.
    pub async fn recall(&self, bank: &str, body: Value) -> Result<Value, MemoryError> {
        self.call(bank, "/memories/recall", body, self.recall_timeout)
            .await
    }

    /// C31 (§4.2): `POST /v1/default/banks/<bank>/reflect`.
    pub async fn reflect(&self, bank: &str, body: Value) -> Result<Value, MemoryError> {
        self.call(bank, "/reflect", body, self.long_timeout).await
    }

    /// `GET /desks/{d}/memory` (§4.2): the installation status plus the desk.
    pub async fn desk_status(&self, desk_id: &str) -> Result<Value, MemoryError> {
        let status = self.status().await?;
        Ok(json!({
            "child": status.child,
            "provider": status.provider,
            "desk_id": desk_id,
        }))
    }

    /// `POST /desks/{d}/memory/retain` (§4.2): one item, synchronous, the
    /// agent's own `context` and `tags` verbatim, and the attribution metadata
    /// the request's headers decided. The answer's unit appends
    /// `MEMORY_RETAINED` and, on the first retain ever, locks the embedding
    /// model (§3).
    pub async fn retain_op(
        &self,
        desk_id: &str,
        request: RetainRequest,
        source: &crate::trade::Source,
    ) -> Result<Value, MemoryError> {
        bounded("content", &request.content, 1, CONTENT_MAX)?;
        if let Some(context) = &request.context {
            bounded("context", context, 0, CONTEXT_MAX)?;
        }
        let tags = checked_tags(request.tags)?;
        let (source_name, trigger_id, firing_id) = attribution(source);

        let mut item = Map::new();
        item.insert("content".to_string(), json!(request.content));
        if let Some(context) = &request.context {
            item.insert("context".to_string(), json!(context));
        }
        if !tags.is_empty() {
            item.insert("tags".to_string(), json!(tags));
        }
        let mut metadata = Map::new();
        metadata.insert("source".to_string(), json!(source_name));
        metadata.insert("desk_id".to_string(), json!(desk_id));
        if let (Some(trigger_id), Some(firing_id)) = (trigger_id, firing_id) {
            metadata.insert("trigger_id".to_string(), json!(trigger_id));
            metadata.insert("firing_id".to_string(), json!(firing_id));
        }
        item.insert("metadata".to_string(), Value::Object(metadata));
        let answer: RetainAnswer = read(
            self.retain(
                &bank(desk_id),
                json!({ "items": [Value::Object(item)], "async": false }),
            )
            .await?,
        )?;

        let at_ns = now_ns();
        let desk = desk_id.to_string();
        let payload = json!({
            "source": source_name,
            "trigger_id": trigger_id,
            "firing_id": firing_id,
            "items_count": answer.items_count,
            "tags": tags,
        });
        self.store.unit(move |tx| {
            tx.execute(
                "UPDATE memory_provider SET embedding_locked_at_ns = ?1 \
                 WHERE id = 1 AND embedding_locked_at_ns IS NULL",
                params![at_ns],
            )?;
            append_event(tx, "MEMORY_RETAINED", Some(&desk), at_ns, payload)
        })?;
        Ok(json!({ "items_count": answer.items_count }))
    }

    /// `POST /desks/{d}/memory/recall` (§4.2).
    pub async fn recall_op(
        &self,
        desk_id: &str,
        request: RecallRequest,
    ) -> Result<Value, MemoryError> {
        bounded("query", &request.query, 1, QUERY_MAX)?;
        let tags = checked_tags(request.tags)?;
        let budget = checked_budget(request.budget)?;

        let mut body = Map::new();
        body.insert("query".to_string(), json!(request.query));
        body.insert("budget".to_string(), json!(budget));
        if !tags.is_empty() {
            body.insert("tags".to_string(), json!(tags));
            body.insert("tags_match".to_string(), json!("any"));
        }
        let answer: RecallAnswer = read(self.recall(&bank(desk_id), Value::Object(body)).await?)?;
        self.recalled(desk_id, "recall", answer.results.len())?;
        Ok(json!({ "results": answer.results }))
    }

    /// `POST /desks/{d}/memory/reflect` (§4.2).
    pub async fn reflect_op(
        &self,
        desk_id: &str,
        request: ReflectRequest,
    ) -> Result<Value, MemoryError> {
        bounded("query", &request.query, 1, QUERY_MAX)?;
        let budget = checked_budget(request.budget)?;
        let answer: ReflectAnswer = read(
            self.reflect(
                &bank(desk_id),
                json!({ "query": request.query, "budget": budget }),
            )
            .await?,
        )?;
        self.recalled(desk_id, "reflect", answer.based_on.memories.len())?;
        Ok(json!({ "text": answer.text, "based_on": answer.based_on.memories }))
    }

    /// `MEMORY_RECALLED {op, results}` (§4.2): the count, never a word of what
    /// was asked or answered.
    fn recalled(&self, desk_id: &str, op: &'static str, results: usize) -> Result<(), MemoryError> {
        let desk = desk_id.to_string();
        let at_ns = now_ns();
        self.store.unit(move |tx| {
            append_event(
                tx,
                "MEMORY_RECALLED",
                Some(&desk),
                at_ns,
                json!({ "op": op, "results": results }),
            )
        })?;
        Ok(())
    }
}

/// The child's answer as the subset §4.2 needs. An answer MarketRig cannot read
/// is the child's failure, so it carries `MEMORY_ERROR` like any other (§4.3).
fn read<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, MemoryError> {
    serde_json::from_value(body).map_err(|e| {
        MemoryError::Error(sentence(
            "The memory child answered a body MarketRig cannot read",
            &e.to_string(),
        ))
    })
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
        live: tokio::sync::Mutex::new(Live::default()),
        long_timeout: LONG_TIMEOUT,
        recall_timeout: RECALL_TIMEOUT,
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
// memory::ops (feature SPEC §8 check 4)
// ---------------------------------------------------------------------------

/// An in-process `hindsight-api`: §4.2's three routes on the paths and field
/// names the daemon consumes, the bearer rule of §2.2, and one knob for the
/// failure shapes §4.3 maps. Shared with `api`'s own route check.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct Fake {
    pub port: u16,
    pub bearer: String,
    /// `("<op> <bank>", request body)` for every call that carried the bearer.
    seen: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    mode: std::sync::Arc<std::sync::Mutex<&'static str>>,
}

/// A 5xx `detail` shaped like the real one: two lines, the first quoting the
/// provider's own text, which quotes the key back (§4.3).
#[cfg(test)]
pub(crate) fn boom_detail(key: &str) -> String {
    format!(
        "Fact extraction failed: AuthenticationError: Error code: 401 - Incorrect API key \
         provided: {key}.\nsecond line the caller never sees"
    )
}

#[cfg(test)]
impl Fake {
    /// `ok` | `reject` (422) | `boom` (500) | `sleep` (past any budget).
    pub(crate) fn arm(&self, mode: &'static str) {
        *self.mode.lock().expect("fake mode") = mode;
    }

    pub(crate) fn drain(&self) -> Vec<(String, Value)> {
        self.seen.lock().expect("fake log").drain(..).collect()
    }

    #[track_caller]
    pub(crate) fn last(&self) -> (String, Value) {
        self.drain().pop().expect("the daemon called the child")
    }
}

#[cfg(test)]
async fn fake_answer(
    fake: Fake,
    headers: axum::http::HeaderMap,
    call: String,
    body: Value,
    ok: Value,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    if headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {}", fake.bearer))
    {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "detail": "Invalid API key" })),
        )
            .into_response();
    }
    fake.seen.lock().expect("fake log").push((call, body));
    // Read the knob out before any await: a guard held across one is not `Send`.
    let mode = *fake.mode.lock().expect("fake mode");
    match mode {
        "reject" => (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({ "detail": "rejected by script" })),
        )
            .into_response(),
        "boom" => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({ "detail": boom_detail(FAKE_KEY) })),
        )
            .into_response(),
        "sleep" => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            axum::Json(ok).into_response()
        }
        _ => axum::Json(ok).into_response(),
    }
}

/// The one recall result the fake holds, carrying two fields §4.2 does not
/// answer so the projection is visible.
#[cfg(test)]
pub(crate) fn fake_result() -> Value {
    json!({
        "id": "m-1",
        "text": "a lesson",
        "type": "experience",
        "context": "cycle 7",
        "tags": ["lesson"],
        "metadata": { "source": "INTERACTIVE" },
        "occurred_start": "2026-09-04T00:00:00Z",
        "mentioned_at": "2026-09-04T00:00:01Z",
        "score": 0.91,
        "document_id": "d-9"
    })
}

#[cfg(test)]
pub(crate) async fn fake_child() -> Fake {
    use axum::extract::{Path as Segment, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};

    async fn retain(
        State(fake): State<Fake>,
        Segment(bank): Segment<String>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        let ok = json!({ "success": true, "bank_id": bank, "items_count": 1, "async": false });
        fake_answer(fake, headers, format!("retain {bank}"), body, ok).await
    }

    async fn recall(
        State(fake): State<Fake>,
        Segment(bank): Segment<String>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        let ok = json!({ "results": [fake_result()], "elapsed_ms": 3 });
        fake_answer(fake, headers, format!("recall {bank}"), body, ok).await
    }

    async fn reflect(
        State(fake): State<Fake>,
        Segment(bank): Segment<String>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        let ok = json!({
            "text": "a reflection",
            "based_on": { "memories": [{ "id": "m-1", "text": "a lesson",
                                         "type": "experience", "weight": 2 }] },
            "elapsed_ms": 4
        });
        fake_answer(fake, headers, format!("reflect {bank}"), body, ok).await
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake = Fake {
        port: listener.local_addr().unwrap().port(),
        bearer: "fake-per-start-bearer".to_string(),
        seen: Default::default(),
        mode: std::sync::Arc::new(std::sync::Mutex::new("ok")),
    };
    let app = axum::Router::new()
        .route(
            "/health",
            get(|| async { axum::Json(json!({"status": "ok"})) }),
        )
        .route("/v1/default/banks/{bank}/memories", post(retain))
        .route("/v1/default/banks/{bank}/memories/recall", post(recall))
        .route("/v1/default/banks/{bank}/reflect", post(reflect))
        .with_state(fake.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    fake
}

/// Puts the fake in the live slot C30's launch would fill, so this check drives
/// §4 without any of §2.2's start code.
#[cfg(test)]
pub(crate) async fn set_ready(memory: &Memory, fake: &Fake) {
    let mut live = memory.live.lock().await;
    live.state = LiveState::Ready;
    live.port = Some(fake.port);
    live.bearer = Some(fake.bearer.clone());
    live.pid = Some(4242);
}

#[cfg(test)]
const DESK: &str = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
#[cfg(test)]
const DESK_BANK: &str = "desk-0199a1b2c3d47e5f8a9b0c1d2e3f4a5b";

/// The desk an event's `desk_id` references; this check is about §4, not about
/// how a desk comes to exist.
#[cfg(test)]
fn plant_desk(store: &Store) {
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES (?1, 'alpha', 'READY', '/desks/alpha', 1, 1)",
                params![DESK],
            )
        })
        .unwrap();
}

#[cfg(test)]
fn retain_request(content: &str, context: Option<&str>, tags: &[&str]) -> RetainRequest {
    RetainRequest {
        content: content.to_string(),
        context: context.map(str::to_string),
        tags: (!tags.is_empty()).then(|| tags.iter().map(|t| t.to_string()).collect()),
    }
}

/// The three request mappings field for field, the three answer shapes, the
/// limits, every §4.3 code, the two event payloads, and no content anywhere
/// MarketRig writes (§4.2, §4.3).
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_operations() {
    use crate::trade::Source;

    let (_dir, mut memory) = scratch();
    memory.long_timeout = Duration::from_secs(10);
    memory.recall_timeout = Duration::from_secs(10);
    plant_desk(&memory.store);
    let fake = fake_child().await;
    set_ready(&memory, &fake).await;
    // The key the child quotes back at us (§4.3); stored, never sent anywhere.
    memory.store_key(FAKE_KEY).unwrap();

    // --- retain: one item, synchronous, the agent's own fields verbatim ------
    let answer = memory
        .retain_op(
            DESK,
            retain_request("a lesson", Some("cycle 7"), &["lesson", "AAPL.XNAS"]),
            &Source::Session,
        )
        .await
        .unwrap();
    assert_eq!(answer, json!({ "items_count": 1 }));
    assert_eq!(
        fake.last(),
        (
            format!("retain {DESK_BANK}"),
            json!({
                "items": [{
                    "content": "a lesson",
                    "context": "cycle 7",
                    "tags": ["lesson", "AAPL.XNAS"],
                    "metadata": { "source": "INTERACTIVE", "desk_id": DESK },
                }],
                "async": false,
            })
        )
    );
    // The first retain ever locks the embedding model (§3, §4.2).
    assert!(
        provider_row(&memory.store)
            .unwrap()
            .embedding_locked_at_ns
            .is_some()
    );

    // A trigger's retain carries both ids in the metadata, and nothing else.
    memory
        .retain_op(
            DESK,
            retain_request("from a trigger", None, &[]),
            &Source::Trigger {
                trigger_id: "t-1".to_string(),
                firing_id: "f-1".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.last().1,
        json!({
            "items": [{
                "content": "from a trigger",
                "metadata": {
                    "source": "TRIGGER", "desk_id": DESK,
                    "trigger_id": "t-1", "firing_id": "f-1",
                },
            }],
            "async": false,
        }),
        "an omitted context or tag list is left out, never sent null"
    );

    // --- recall: the query, the budget, the tag filter, the eight fields -----
    let answer = memory
        .recall_op(
            DESK,
            RecallRequest {
                query: "what did AAPL teach".to_string(),
                budget: Some("high".to_string()),
                tags: Some(vec!["lesson".to_string()]),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.last(),
        (
            format!("recall {DESK_BANK}"),
            json!({
                "query": "what did AAPL teach",
                "budget": "high",
                "tags": ["lesson"],
                "tags_match": "any",
            })
        )
    );
    assert_eq!(
        answer,
        json!({ "results": [{
            "id": "m-1",
            "text": "a lesson",
            "type": "experience",
            "context": "cycle 7",
            "tags": ["lesson"],
            "metadata": { "source": "INTERACTIVE" },
            "occurred_start": "2026-09-04T00:00:00Z",
            "mentioned_at": "2026-09-04T00:00:01Z",
        }] }),
        "exactly §4.2's eight fields; the child's score and document_id are dropped"
    );

    // No tags means no filter at all, and `mid` is the default budget (§4.3).
    memory
        .recall_op(
            DESK,
            RecallRequest {
                query: "anything".to_string(),
                budget: None,
                tags: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.last().1,
        json!({ "query": "anything", "budget": "mid" })
    );

    // --- reflect: the citations come out of `based_on.memories` -------------
    let answer = memory
        .reflect_op(
            DESK,
            ReflectRequest {
                query: "what have I learned".to_string(),
                budget: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.last(),
        (
            format!("reflect {DESK_BANK}"),
            json!({ "query": "what have I learned", "budget": "mid" })
        )
    );
    assert_eq!(
        answer,
        json!({
            "text": "a reflection",
            "based_on": [{ "id": "m-1", "text": "a lesson", "type": "experience" }],
        })
    );

    // --- The events: the counts and the attribution, never a word of content -
    let seen = events(&memory.store);
    assert_eq!(
        seen.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "MEMORY_RETAINED",
            "MEMORY_RETAINED",
            "MEMORY_RECALLED",
            "MEMORY_RECALLED",
            "MEMORY_RECALLED",
        ]
    );
    assert_eq!(
        seen[0].1,
        json!({
            "source": "INTERACTIVE", "trigger_id": null, "firing_id": null,
            "items_count": 1, "tags": ["lesson", "AAPL.XNAS"],
        })
    );
    assert_eq!(
        seen[1].1,
        json!({
            "source": "TRIGGER", "trigger_id": "t-1", "firing_id": "f-1",
            "items_count": 1, "tags": [],
        })
    );
    assert_eq!(seen[2].1, json!({ "op": "recall", "results": 1 }));
    assert_eq!(seen[4].1, json!({ "op": "reflect", "results": 1 }));
    let written = seen
        .iter()
        .map(|(kind, payload)| format!("{kind}{payload}"))
        .collect::<String>();
    for secret in [
        "a lesson",
        "from a trigger",
        "cycle 7",
        "what did AAPL teach",
        "what have I learned",
        "a reflection",
        FAKE_KEY,
    ] {
        assert!(
            !written.contains(secret),
            "{secret:?} is in an event payload"
        );
    }

    // --- The limits, none of which reaches the child (§4.3) -----------------
    let big = "x".repeat(CONTENT_MAX + 1);
    for (label, request) in [
        ("empty content", retain_request("", None, &[])),
        ("content over 64 KiB", retain_request(&big, None, &[])),
        (
            "context over 4 KiB",
            retain_request("c", Some(&"x".repeat(CONTEXT_MAX + 1)), &[]),
        ),
        (
            "seventeen tags",
            RetainRequest {
                content: "c".to_string(),
                context: None,
                tags: Some((0..=TAGS_MAX).map(|n| n.to_string()).collect()),
            },
        ),
        (
            "a 65-character tag",
            retain_request("c", None, &[&"t".repeat(TAG_MAX + 1)]),
        ),
        ("an empty tag", retain_request("c", None, &[""])),
    ] {
        let err = memory.retain_op(DESK, request, &Source::Session).await;
        assert_eq!(err.unwrap_err().code(), "VALIDATION", "{label}");
    }
    for (label, request) in [
        (
            "empty query",
            RecallRequest {
                query: String::new(),
                budget: None,
                tags: None,
            },
        ),
        (
            "query over 8 KiB",
            RecallRequest {
                query: "x".repeat(QUERY_MAX + 1),
                budget: None,
                tags: None,
            },
        ),
        (
            "an unknown budget",
            RecallRequest {
                query: "q".to_string(),
                budget: Some("enormous".to_string()),
                tags: None,
            },
        ),
    ] {
        let err = memory.recall_op(DESK, request).await;
        assert_eq!(err.unwrap_err().code(), "VALIDATION", "{label}");
    }
    let err = memory
        .reflect_op(
            DESK,
            ReflectRequest {
                query: String::new(),
                budget: None,
            },
        )
        .await;
    assert_eq!(err.unwrap_err().code(), "VALIDATION");
    assert!(fake.drain().is_empty(), "a refused request never leaves");

    // --- 422: the child's own detail, as MEMORY_REJECTED --------------------
    fake.arm("reject");
    let err = memory
        .retain_op(DESK, retain_request("c", None, &[]), &Source::Session)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_REJECTED");
    assert!(err.to_string().contains("rejected by script"), "{err}");

    // --- 500: the status and the first line, with the key redacted ----------
    fake.arm("boom");
    let err = memory
        .recall_op(
            DESK,
            RecallRequest {
                query: "q".to_string(),
                budget: None,
                tags: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_ERROR");
    let message = err.to_string();
    assert!(message.contains("500"), "{message}");
    assert!(!message.contains(FAKE_KEY), "the key leaked: {message}");
    assert!(message.contains(REDACTED), "{message}");
    assert!(
        !message.contains("second line"),
        "only the first line: {message}"
    );

    // --- The budget the operation waits inside, spent (§4.3) ----------------
    fake.arm("sleep");
    memory.recall_timeout = Duration::from_millis(50);
    let err = memory
        .recall_op(
            DESK,
            RecallRequest {
                query: "q".to_string(),
                budget: None,
                tags: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_TIMEOUT");
    memory.recall_timeout = Duration::from_secs(10);
    fake.arm("ok");

    // --- A child that is not there at all: transport, then the two rows -----
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    set_ready(
        &memory,
        &Fake {
            port: dead_port,
            ..fake.clone()
        },
    )
    .await;
    let err = memory
        .retain_op(DESK, retain_request("c", None, &[]), &Source::Session)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_ERROR");

    // No live child and an unconfigured row: MEMORY_UNCONFIGURED (C30 owns the
    // start that would follow).
    *memory.live.lock().await = Live::default();
    let err = memory
        .retain_op(DESK, retain_request("c", None, &[]), &Source::Session)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_UNCONFIGURED");

    // A failed row refuses before anything is started at all.
    memory
        .store
        .unit(|tx| {
            tx.execute(
                "UPDATE memory_child SET state = 'UNAVAILABLE', failure_code = 'CHILD_FAILED', \
                 failure_message = 'it exited' WHERE id = 1",
                [],
            )
        })
        .unwrap();
    let err = memory
        .recall_op(
            DESK,
            RecallRequest {
                query: "q".to_string(),
                budget: None,
                tags: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_UNAVAILABLE");
    assert!(err.to_string().contains("it exited"), "{err}");
}

/// A wrong bearer is the child's own refusal, and it never becomes a MarketRig
/// answer that quotes it (§2.2, §4.3).
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_bearer_is_the_childs_refusal() {
    let (_dir, memory) = scratch();
    let fake = fake_child().await;
    set_ready(
        &memory,
        &Fake {
            bearer: "not-the-bearer".to_string(),
            ..fake.clone()
        },
    )
    .await;
    let err = memory
        .retain_op(
            DESK,
            retain_request("c", None, &[]),
            &crate::trade::Source::Session,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "MEMORY_REJECTED");
    assert!(err.to_string().contains("Invalid API key"), "{err}");
    assert!(fake.drain().is_empty(), "the child logged nothing");
    assert!(
        events(&memory.store).is_empty(),
        "and neither did MarketRig"
    );
}
