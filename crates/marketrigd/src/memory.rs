//! The memory provider settings and the credential seam.
//!
//! Contract: `sdd/features/openviking-continuity/SPEC.md` §1.1 and §2.1, root
//! `sdd/SPEC.md` §16.
//!
//! `PUT /memory/provider` is R4 §3's route unchanged except that the embedding
//! lock is dropped: OpenViking's local vector store carries its own dimension
//! check. Everything Hindsight owned — the child row, the launcher probe, the
//! derived banks, and the pass-through operations — is gone with migration 7.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::desk::append_event;
use crate::store::{Roots, Store, StoreError, now_ns};

/// The credential store's service and accounts (§2.1, per D49). The seed is the
/// one installation secret desk keys derive from (§3.1), stored beside the
/// provider key and never rotated.
const SERVICE: &str = "marketrig";
const ACCOUNT: &str = "hindsight-provider";
pub const SEED_ACCOUNT: &str = "openviking-seed";

/// The opaque marker `memory_provider.key_ref` carries once a key is stored.
const KEY_REF: &str = "marketrig/hindsight-provider";

/// The seam credential store, inside the relocated runtime directory.
const CREDENTIALS: &str = "credentials.json";

/// The provider model list's and the dimension probe's own bound.
const MODELS_TIMEOUT: Duration = Duration::from_secs(15);

/// What the daemon puts in place of the stored key in anything it lifts from a
/// child or a plugin (§9 check 10): a provider's own text quotes the key back.
const REDACTED: &str = "<redacted>";

/// The provider settings: the row, the credential seam, and the one HTTP
/// client. One per daemon, in `ApiState`.
pub struct Memory {
    pub store: Store,
    pub roots: Roots,
    /// True under `MARKETRIG_TEST_DATA_ROOT`: the credential store is
    /// `runtime/credentials.json` in the relocated root instead of the platform
    /// store. It is the harness seam, never a fallback when the native store
    /// fails.
    pub seam: bool,
    /// The provider fetch: never through a machine proxy, never following a
    /// redirect, bounded per request.
    pub http: reqwest::Client,
    /// The secrets that live in memory only — the child's per-start root key and
    /// each desk's key (§3.1) — registered here as they are minted so that
    /// [`Memory::redact`] covers all four of §9 check 10's.
    ///
    /// ponytail: an append-only list, one entry per start and per desk; it is
    /// bounded by the desk count and never read on a hot path.
    held: std::sync::Mutex<Vec<String>>,
}

impl Memory {
    pub fn new(store: Store, roots: Roots) -> io::Result<Memory> {
        Ok(Memory {
            store,
            roots,
            seam: std::env::var_os(crate::store::TEST_DATA_ROOT_ENV).is_some(),
            http: client(),
            held: std::sync::Mutex::new(Vec::new()),
        })
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the provider HTTP client builds from constants")
}

/// A provider failure carrying a stable SCREAMING_SNAKE code (§1.1).
#[derive(Debug)]
pub enum MemoryError {
    /// No provider base URL or key.
    Unconfigured,
    Validation(String),
    CredentialStoreUnavailable(String),
    ProviderUnreachable(String),
    /// The embedding model would not answer with a vector (§2.1): the row is
    /// not written, so `ov.conf` never carries a guessed dimension.
    ProviderRejected(String),
    /// A store failure; the daemon's SQLite is in-process and single-writer, so
    /// this is unreachable in practice.
    Error(String),
}

impl MemoryError {
    pub fn code(&self) -> &'static str {
        match self {
            MemoryError::Unconfigured => "MEMORY_UNCONFIGURED",
            MemoryError::Validation(_) => "VALIDATION",
            MemoryError::CredentialStoreUnavailable(_) => "CREDENTIAL_STORE_UNAVAILABLE",
            MemoryError::ProviderUnreachable(_) => "PROVIDER_UNREACHABLE",
            MemoryError::ProviderRejected(_) => "PROVIDER_REJECTED",
            MemoryError::Error(_) => "MEMORY_ERROR",
        }
    }
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Unconfigured => {
                write!(f, "Memory is not configured: set the provider first.")
            }
            MemoryError::Validation(m) | MemoryError::Error(m) => write!(f, "{m}"),
            MemoryError::CredentialStoreUnavailable(m) => {
                write!(f, "The credential store is unavailable: {m}")
            }
            MemoryError::ProviderUnreachable(m) => write!(f, "The provider did not answer: {m}"),
            MemoryError::ProviderRejected(m) => write!(
                f,
                "The embedding model answered with no vector, so its dimension is unknown: {m}"
            ),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<StoreError> for MemoryError {
    fn from(e: StoreError) -> Self {
        MemoryError::Error(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The provider row (§6)
// ---------------------------------------------------------------------------

/// The `memory_provider` row, secrets-free by construction.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Provider {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    /// Measured once at save time and written into `ov.conf` (§2.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_dimension: Option<i64>,
    pub api_key_present: bool,
}

impl Provider {
    /// Whether the child may start on this row (§2.2): every field `ov.conf`
    /// needs, and a key in the credential store behind `key_ref`.
    pub fn complete(&self) -> bool {
        self.base_url.is_some()
            && self.llm_model.is_some()
            && self.embedding_model.is_some()
            && self.embedding_dimension.is_some()
            && self.api_key_present
    }
}

const PROVIDER_SELECT: &str = "SELECT base_url, llm_model, embedding_model, key_ref, \
                               embedding_dimension FROM memory_provider WHERE id = 1";

fn read_provider(row: &rusqlite::Row<'_>) -> rusqlite::Result<Provider> {
    Ok(Provider {
        base_url: row.get(0)?,
        llm_model: row.get(1)?,
        embedding_model: row.get(2)?,
        api_key_present: row.get::<_, Option<String>>(3)?.is_some(),
        embedding_dimension: row.get(4)?,
    })
}

pub fn provider_row(store: &Store) -> Result<Provider, StoreError> {
    store.call(|c| c.query_row(PROVIDER_SELECT, [], read_provider))
}

// ---------------------------------------------------------------------------
// The credential seam (§2.1, per D49)
// ---------------------------------------------------------------------------

/// Names the platform credential store once, at daemon start, before any route
/// can reach it. A failure is a log line, not a startup failure:
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

    /// Writes one secret. Under the seam the store is `runtime/credentials.json`
    /// (0600, one JSON object keyed by account); otherwise the platform store.
    pub fn store_secret(&self, account: &str, secret: &str) -> Result<(), MemoryError> {
        if self.seam {
            let path = self.credentials_path();
            let mut map = seam_map(&path)?;
            map.insert(account.to_string(), secret.to_string());
            return seam_write(&path, &map)
                .map_err(|e| MemoryError::CredentialStoreUnavailable(e.to_string()));
        }
        keyring_core::Entry::new(SERVICE, account)
            .and_then(|entry| entry.set_password(secret))
            .map_err(|e| MemoryError::CredentialStoreUnavailable(e.to_string()))
    }

    /// Reads one secret back, `None` when none was ever stored.
    pub fn load_secret(&self, account: &str) -> Result<Option<String>, MemoryError> {
        if self.seam {
            return Ok(seam_map(&self.credentials_path())?.remove(account));
        }
        match keyring_core::Entry::new(SERVICE, account).and_then(|entry| entry.get_password()) {
            Ok(key) => Ok(Some(key)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(MemoryError::CredentialStoreUnavailable(e.to_string())),
        }
    }

    /// Writes the provider key.
    pub fn store_key(&self, key: &str) -> Result<(), MemoryError> {
        self.store_secret(ACCOUNT, key)
    }

    /// Reads the provider key back, `None` when none was ever stored.
    pub fn load_key(&self) -> Result<Option<String>, MemoryError> {
        self.load_secret(ACCOUNT)
    }

    /// Registers a memory-only secret — the child's root key, a desk key — so
    /// [`Memory::redact`] covers it too (§9 check 10).
    pub fn hold_secret(&self, secret: &str) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if !secret.is_empty() && !held.iter().any(|s| s == secret) {
            held.push(secret.to_string());
        }
    }

    /// Replaces every occurrence of the four secrets of §9 check 10 — the
    /// provider key, the installation seed, the child's root key, and the desk
    /// keys — in a message the daemon lifted from the child or a plugin. A
    /// message carrying none of them comes back unchanged.
    pub fn redact(&self, message: &str) -> String {
        let mut message = message.to_string();
        for account in [ACCOUNT, SEED_ACCOUNT] {
            if let Ok(Some(secret)) = self.load_secret(account) {
                message = redact_key(&secret, &message);
            }
        }
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        for secret in held.iter() {
            message = redact_key(secret, &message);
        }
        message
    }
}

/// [`Memory::redact`] against a key already in hand — a child's supervisor holds
/// the key its own child was launched with, so a provider change under it cannot
/// leave the child's last line unredacted.
pub fn redact_key(key: &str, message: &str) -> String {
    if key.is_empty() {
        return message.to_string();
    }
    let message = message.replace(key, REDACTED);
    // A provider quotes the key masked (`sk-smoke**********-123`): any token
    // opening with the key's first characters goes the same way. Eight is short
    // enough to catch a masked prefix and long enough not to hit plain words.
    let prefix: String = key.chars().take(8).collect();
    if key.chars().count() < 12 {
        return message;
    }
    message
        .split_inclusive(|c: char| c.is_whitespace() || "'\"`,;()[]{}".contains(c))
        .map(|token| {
            let word = token.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '*');
            if word.starts_with(&prefix) && word != REDACTED {
                token.replacen(word, REDACTED, 1)
            } else {
                token.to_string()
            }
        })
        .collect()
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
// The provider routes (§1.1)
// ---------------------------------------------------------------------------

/// `PUT /memory/provider`'s body.
#[derive(Deserialize)]
pub struct ProviderRequest {
    pub base_url: String,
    /// Omitted keeps whatever key is already stored.
    #[serde(default)]
    pub api_key: Option<String>,
    pub llm_model: String,
    pub embedding_model: String,
}

/// An absolute `http`/`https` URL with no userinfo, query, or fragment, its
/// trailing slash stripped.
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

/// A non-empty model name of at most 128 characters.
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
    /// `GET /memory/provider` (§1.1).
    pub fn provider(&self) -> Result<Provider, MemoryError> {
        Ok(provider_row(&self.store)?)
    }

    /// One embeddings request for the string `marketrig`, whose vector's length
    /// is the dimension `ov.conf` writes (§2.1). OpenViking would otherwise
    /// silently assume 2048 for a model outside OpenAI's three named ones, so a
    /// provider that will not answer with a vector is `PROVIDER_REJECTED`.
    async fn measure_dimension(
        &self,
        base_url: &str,
        model: &str,
        key: Option<&str>,
    ) -> Result<i64, MemoryError> {
        let mut request = self
            .http
            .post(format!("{base_url}/embeddings"))
            .timeout(MODELS_TIMEOUT)
            .json(&json!({ "model": model, "input": "marketrig" }));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let rejected = |why: String| MemoryError::ProviderRejected(first_line(&why));
        let response = request
            .send()
            .await
            .map_err(|e| rejected(self.redact(&e.to_string())))?;
        let status = response.status();
        if !status.is_success() {
            return Err(MemoryError::ProviderRejected(format!("HTTP {status}")));
        }
        let body: Value = response.json().await.map_err(|e| rejected(e.to_string()))?;
        body["data"][0]["embedding"]
            .as_array()
            .filter(|vector| !vector.is_empty())
            .map(|vector| vector.len() as i64)
            .ok_or_else(|| rejected("the answer carried no embedding".to_string()))
    }

    /// `PUT /memory/provider` (§1.1): the dimension is measured first, so a
    /// provider that cannot serve embeddings writes nothing; then the key
    /// reaches the credential store, so a store failure writes nothing either;
    /// the row and the event then commit in one unit. There is no embedding lock.
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
        let key = match &request.api_key {
            Some(key) => Some(key.clone()),
            None => self.load_key()?,
        };
        let dimension = self
            .measure_dimension(&base_url, &embedding_model, key.as_deref())
            .await?;
        if let Some(key) = &request.api_key {
            self.store_key(key)?;
        }
        let key_ref = (request.api_key.is_some() || current.api_key_present).then_some(KEY_REF);

        let at_ns = now_ns();
        Ok(self.store.unit(move |tx| {
            tx.execute(
                "UPDATE memory_provider SET base_url = ?1, llm_model = ?2, embedding_model = ?3, \
                 key_ref = ?4, updated_at_ns = ?5, embedding_dimension = ?6 WHERE id = 1",
                params![
                    base_url,
                    llm_model,
                    embedding_model,
                    key_ref,
                    at_ns,
                    dimension
                ],
            )?;
            append_event(
                tx,
                "OPENVIKING_CONFIGURED",
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
        })?)
    }

    /// `GET /memory/provider/models`: fetched live at request time, never
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
}

/// The first line of a message the daemon reports.
fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// memory::provider (feature SPEC §9, the provider half of check 3)
// ---------------------------------------------------------------------------

/// A [`Memory`] on a scratch root, always on the file credential seam so no test
/// can reach this machine's keychain.
#[cfg(test)]
pub(crate) fn seam_memory(store: Store, roots: Roots) -> Memory {
    Memory {
        store,
        roots,
        seam: true,
        http: client(),
        held: std::sync::Mutex::new(Vec::new()),
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

#[cfg(test)]
fn events(store: &Store) -> Vec<(String, Value)> {
    store
        .call(|c| {
            c.prepare(
                "SELECT kind, payload FROM operational_events WHERE kind LIKE 'OPENVIKING_%' \
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
const FAKE_KEY: &str = "sk-marketrig-fake-0123456789abcdef";

/// A provider that answers `/embeddings` with a three-element vector, so
/// `PUT /memory/provider` can measure the dimension (§2.1). The port is the
/// caller's `base_url`; nothing else of the provider is faked here.
#[cfg(test)]
pub(crate) async fn fake_embeddings() -> u16 {
    let app = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(|| async {
            axum::Json(json!({"data": [{"embedding": [0.1, 0.2, 0.3]}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

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
    let port = fake_embeddings().await;
    let base = format!("http://127.0.0.1:{port}/v1");

    // Validation refuses every shape §1.1 keeps from R4 §3, and writes nothing.
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
    assert!(memory.provider().unwrap().base_url.is_none());
    assert!(events(&memory.store).is_empty());
    assert!(memory.load_key().unwrap().is_none());

    // A good PUT: the trailing slash goes, the key reaches the store, the row
    // and the event follow, and the key is in neither.
    let provider = memory
        .put_provider(request(
            &format!("{base}/"),
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap();
    assert_eq!(provider.base_url.as_deref(), Some(base.as_str()));
    assert_eq!(provider.llm_model.as_deref(), Some("llm-1"));
    // The dimension is measured, not declared (§2.1).
    assert_eq!(provider.embedding_dimension, Some(3));
    assert!(provider.complete());
    assert!(provider.api_key_present);
    assert_eq!(memory.load_key().unwrap().as_deref(), Some(FAKE_KEY));
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
        .put_provider(request(&base, None, "llm-2", "emb-1"))
        .await
        .unwrap();
    assert!(provider.api_key_present);
    assert_eq!(provider.llm_model.as_deref(), Some("llm-2"));
    assert_eq!(memory.load_key().unwrap().as_deref(), Some(FAKE_KEY));

    // The embedding lock is gone (§1.1): a new embedding model is just a save.
    let provider = memory
        .put_provider(request(&base, None, "llm-3", "emb-2"))
        .await
        .unwrap();
    assert_eq!(provider.embedding_model.as_deref(), Some("emb-2"));

    // A provider that will not serve embeddings is PROVIDER_REJECTED and the
    // row keeps what it had (§2.1).
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let err = memory
        .put_provider(request(
            &format!("http://127.0.0.1:{dead_port}/v1"),
            None,
            "llm-4",
            "emb-3",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "PROVIDER_REJECTED");
    assert_eq!(
        memory.provider().unwrap().llm_model.as_deref(),
        Some("llm-3")
    );

    // Three accepted PUTs, three events, and the key in none of them.
    let seen = events(&memory.store);
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|(kind, payload)| {
        kind == "OPENVIKING_CONFIGURED"
            && payload["what"] == "provider"
            && payload["base_url"] == base
            && !payload.to_string().contains(FAKE_KEY)
    }));

    // Redaction (§9 check 10): what a child quotes back never leaves as the key.
    assert_eq!(
        memory.redact(&format!("Incorrect API key provided: {FAKE_KEY}.")),
        format!("Incorrect API key provided: {REDACTED}.")
    );
    assert_eq!(memory.redact("nothing to hide"), "nothing to hide");
    // A provider masks the middle of the key; the masked form is still the key.
    let masked = format!(
        "{}**********{}",
        &FAKE_KEY[..8],
        &FAKE_KEY[FAKE_KEY.len() - 4..]
    );
    assert_eq!(
        memory.redact(&format!("provided: {masked}. Next")),
        format!("provided: {REDACTED}. Next")
    );
}

/// A credential store that cannot take the key writes nothing at all (§1.1).
#[cfg(test)]
#[tokio::test]
async fn credential_store_unavailable_writes_nothing() {
    let (_dir, memory) = scratch();
    let port = fake_embeddings().await;
    // A directory where the file belongs: the write fails on both platforms.
    fs::create_dir_all(memory.roots.runtime().join(CREDENTIALS)).unwrap();

    let err = memory
        .put_provider(request(
            &format!("http://127.0.0.1:{port}/v1"),
            Some(FAKE_KEY),
            "llm-1",
            "emb-1",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CREDENTIAL_STORE_UNAVAILABLE");

    let row = memory.provider().unwrap();
    assert!(row.base_url.is_none() && !row.api_key_present);
    assert!(events(&memory.store).is_empty());
}

/// The model list is fetched at request time and never cached (§1.1).
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
    // Every save measures the dimension first (§2.1), key or no key.
    let app = app.route(
        "/v1/embeddings",
        axum::routing::post(|| async {
            axum::Json(json!({"data": [{"embedding": [0.1, 0.2, 0.3]}]}))
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

    // A dead port is a transport failure, not a status (§1.1). The row is
    // written behind the route, which would refuse a base URL serving no
    // embeddings (§2.1) long before the model list is ever asked for.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    memory
        .store
        .unit(move |tx| {
            tx.execute(
                "UPDATE memory_provider SET base_url = ?1 WHERE id = 1",
                params![format!("http://127.0.0.1:{dead_port}/v1")],
            )
        })
        .unwrap();
    assert_eq!(
        memory.models().await.unwrap_err().code(),
        "PROVIDER_UNREACHABLE"
    );
}
