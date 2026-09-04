//! Diagnostics (feature SPEC §9, per D51): JSON Lines to the log root with
//! daily rotation and the newest 7 files kept, plus a human layer on standard
//! error when it is a terminal. Log lines never carry the bearer credential or
//! any other secret (per D49).

use std::io::IsTerminal;
use std::path::Path;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;

/// The newest files kept; rotation is time-based, so boundedness is file count
/// (per R0-8).
const KEEP: usize = 7;

/// Installs the process-wide subscriber. Call this once, first, from the binary.
///
/// Only the `tracing` global default is claimed, never the `log` crate's global
/// logger: a NautilusTrader kernel installs its own on the first node build and
/// permanently disables its logging if that fails, which would make every desk's
/// node unstartable (verified against the pinned 0.62.0 kernel). That is why this
/// is `set_global_default` and not `SubscriberInitExt::try_init`, which would
/// also install the `tracing-log` bridge.
///
/// ponytail: the file appender writes straight through — a hard-killed daemon
/// must leave its last lines on disk, since the log root is gate evidence — so
/// there is no background worker and no guard to hold; move to
/// `tracing_appender::non_blocking` only if log volume ever measurably hurts.
pub fn init(logs: &Path) -> std::io::Result<()> {
    let file = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("marketrigd")
        .filename_suffix("jsonl")
        .max_log_files(KEEP)
        .build(logs)
        .map_err(std::io::Error::other)?;

    let human = std::io::stderr()
        .is_terminal()
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));

    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_writer(file),
        )
        .with(human);

    tracing::subscriber::set_global_default(subscriber).map_err(std::io::Error::other)
}

#[cfg(test)]
use crate::daemon;
#[cfg(test)]
use crate::store::Roots;

/// The whole log root, greppable.
#[cfg(test)]
fn log_text(logs: &Path) -> String {
    std::fs::read_dir(logs)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect()
}

#[cfg(test)]
#[test]
fn secret_free() {
    let dir = tempfile::tempdir().unwrap();
    let roots = Roots::resolve(Some(dir.path())).unwrap();
    roots.create_dirs().unwrap();
    init(&roots.logs).unwrap();

    let startup = daemon::start(roots.clone()).unwrap();
    tracing::info!(port = startup.port, "a later milestone's log line");
    tracing::warn!(credential_length = startup.credential.len(), "and a field");

    let text = log_text(&roots.logs);
    assert!(
        text.contains("a later milestone's log line"),
        "logging is live"
    );
    assert!(text.contains(&startup.daemon_uuid), "startup is evidenced");
    assert!(
        !text.contains(&startup.credential),
        "the live credential must never reach the log root"
    );
    // The pointer file holds it; the log root never does.
    assert!(
        std::fs::read_to_string(daemon::endpoint_path(&roots))
            .unwrap()
            .contains(&startup.credential)
    );

    // R4 feature SPEC §8 check 7: the provider key reaches the credential store
    // and nothing else — not the database file, not the log root, not an event.
    const PROVIDER_KEY: &str = "sk-marketrig-fake-secrets-check-0123456789";
    let memory = crate::memory::seam_memory(startup.store.clone(), roots.clone());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(memory.put_provider(crate::memory::ProviderRequest {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            api_key: Some(PROVIDER_KEY.to_string()),
            llm_model: "llm-1".to_string(),
            embedding_model: "emb-1".to_string(),
        }))
        .unwrap();
    tracing::info!("the provider was configured");

    assert!(
        !log_text(&roots.logs).contains(PROVIDER_KEY),
        "the provider key must never reach the log root"
    );
    for entry in std::fs::read_dir(&roots.data).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("marketrig.sqlite3") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(PROVIDER_KEY.len())
                .any(|w| w == PROVIDER_KEY.as_bytes()),
            "the provider key must never reach {name}"
        );
    }
    let payloads: String = startup
        .store
        .call(|c| {
            c.prepare("SELECT payload FROM operational_events")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .concat();
    assert!(
        !payloads.contains(PROVIDER_KEY),
        "no event payload may carry the provider key"
    );

    // The seam credential store is the one place it lives (per D49, R4-2).
    assert!(
        std::fs::read_to_string(roots.runtime().join("credentials.json"))
            .unwrap()
            .contains(PROVIDER_KEY)
    );
}
