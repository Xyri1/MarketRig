//! Data roots and the durable store.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §2 and §3,
//! root `sdd/SPEC.md` §15.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fmt, io, thread};

use rusqlite::{Connection, Transaction, TransactionBehavior};

/// The one clock behind every `*_ns` column: UTC Unix nanoseconds (root §15).
pub fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

/// Relocates all three roots into one scratch directory (feature SPEC §2).
pub const TEST_DATA_ROOT_ENV: &str = "MARKETRIG_TEST_DATA_ROOT";

/// The three per-user roots (feature SPEC §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// Application data: the database and `runtime/`.
    pub data: PathBuf,
    /// Desks home: one workspace directory per desk.
    pub desks: PathBuf,
    /// Application logs.
    pub logs: PathBuf,
}

impl Roots {
    /// Resolves the roots, relocating all three under `test_data_root` when given.
    pub fn resolve(test_data_root: Option<&Path>) -> io::Result<Roots> {
        match test_data_root {
            Some(dir) => Ok(Roots {
                data: dir.join("data"),
                desks: dir.join("desks"),
                logs: dir.join("logs"),
            }),
            None => native(),
        }
    }

    /// Reads `MARKETRIG_TEST_DATA_ROOT` once; call this from `main` and pass the
    /// result down so nothing else depends on process environment.
    pub fn from_env() -> io::Result<Roots> {
        Roots::resolve(
            env::var_os(TEST_DATA_ROOT_ENV)
                .map(PathBuf::from)
                .as_deref(),
        )
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("marketrig.sqlite3")
    }

    pub fn runtime(&self) -> PathBuf {
        self.data.join("runtime")
    }

    /// Creates every root plus `data/runtime` (startup step 1, feature SPEC §4.1).
    pub fn create_dirs(&self) -> io::Result<()> {
        for dir in [&self.runtime(), &self.desks, &self.logs] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

fn native() -> io::Result<Roots> {
    #[cfg(target_os = "macos")]
    {
        let home = home()?;
        Ok(Roots {
            data: home.join("Library/Application Support/MarketRig"),
            desks: home.join(".marketrig/desks"),
            logs: home.join("Library/Logs/MarketRig"),
        })
    }
    #[cfg(target_os = "windows")]
    {
        let local = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
        Ok(Roots {
            data: local.join("MarketRig"),
            desks: home()?.join(".marketrig").join("desks"),
            logs: local.join("MarketRig").join("logs"),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MarketRig runs on macOS and Windows",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn home() -> io::Result<PathBuf> {
    let var = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    env::var_os(var)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{var} is not set")))
}

/// The ordered, embedded migration list (R0-3). `PRAGMA user_version` holds the
/// count applied; migrations are forward-only and never edited once released.
const MIGRATIONS: &[&str] = &[
    include_str!("store/001_r0.sql"),
    include_str!("store/002_r1.sql"),
    include_str!("store/003_r2.sql"),
    include_str!("store/004_r3.sql"),
    include_str!("store/005_r4.sql"),
    include_str!("store/006_r5.sql"),
    include_str!("store/007_openviking.sql"),
    include_str!("store/008_embedding_dimension.sql"),
    include_str!("store/009_hithink.sql"),
    include_str!("store/010_invocation.sql"),
];

/// A store failure carrying a stable SCREAMING_SNAKE code.
#[derive(Debug)]
pub enum StoreError {
    /// The database was written by a newer MarketRig.
    DatabaseNewer {
        found: i64,
        newest: i64,
    },
    Sqlite(rusqlite::Error),
    Io(io::Error),
    /// The database thread is gone.
    Closed,
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::DatabaseNewer { .. } => "DATABASE_NEWER",
            _ => "INTERNAL",
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::DatabaseNewer { found, newest } => write!(
                f,
                "Database schema version {found} is newer than this MarketRig, which knows {newest}."
            ),
            StoreError::Sqlite(e) => write!(f, "Database error: {e}"),
            StoreError::Io(e) => write!(f, "Database file error: {e}"),
            StoreError::Closed => write!(f, "The database thread is no longer running."),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Handle on the one database thread that owns the one connection (root §15).
/// Cloning shares the thread; no connection, transaction, or statement escapes it.
/// The thread ends — draining by construction, since every call is synchronous —
/// when the last handle drops.
#[derive(Debug, Clone)]
pub struct Store {
    jobs: mpsc::Sender<Job>,
    /// Pulsed after every committed unit; the events publisher wakes on it
    /// (R5 feature SPEC §4.1, per R5-5). Shared by every clone.
    commits: Arc<tokio::sync::Notify>,
}

impl Store {
    /// Opens `path`, sets WAL and foreign keys, and applies pending migrations.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let path = path.to_path_buf();
        let (jobs, rx) = mpsc::channel::<Job>();
        let (ready, opened) = mpsc::channel::<Result<(), StoreError>>();
        thread::Builder::new()
            .name("marketrig-db".into())
            .spawn(move || {
                let mut conn = match prepare(&path) {
                    Ok(conn) => {
                        let _ = ready.send(Ok(()));
                        conn
                    }
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })?;
        opened.recv().map_err(|_| StoreError::Closed)??;
        Ok(Store {
            jobs,
            commits: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// The post-commit signal (R5 feature SPEC §4.1).
    pub fn commits(&self) -> Arc<tokio::sync::Notify> {
        self.commits.clone()
    }

    /// Runs a read or single statement on the database thread.
    pub fn call<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.submit(move |conn| f(conn))
    }

    /// Runs a closure inside one `BEGIN IMMEDIATE` … `COMMIT` unit; an error
    /// rolls the whole unit back.
    pub fn unit<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> rusqlite::Result<T> + Send + 'static,
    {
        let value = self.submit(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })?;
        // Waiters first, then a permit for a publisher that is between waits,
        // so no commit can pass unnoticed (R5 feature SPEC §4.1).
        self.commits.notify_waiters();
        self.commits.notify_one();
        Ok(value)
    }

    fn submit<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let (reply, answer) = mpsc::channel();
        self.jobs
            .send(Box::new(move |conn| {
                let _ = reply.send(f(conn));
            }))
            .map_err(|_| StoreError::Closed)?;
        Ok(answer.recv().map_err(|_| StoreError::Closed)??)
    }
}

fn prepare(path: &Path) -> Result<Connection, StoreError> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    // SQLite's documented ALTER TABLE procedure: enforcement stays off for the
    // migration window, so a migration can rebuild a table other tables
    // reference (migration 6 rebuilds `code_snapshots`). The pragma is a no-op
    // inside a transaction, which is why it is set here and not in the SQL.
    conn.pragma_update(None, "foreign_keys", false)?;

    let applied: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let newest = MIGRATIONS.len() as i64;
    if applied > newest {
        return Err(StoreError::DatabaseNewer {
            found: applied,
            newest,
        });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

#[cfg(test)]
pub(crate) fn open_temp() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("marketrig.sqlite3")).unwrap();
    (dir, store)
}

#[cfg(test)]
#[test]
fn roots_test_seam_relocates() {
    let dir = tempfile::tempdir().unwrap();
    let roots = Roots::resolve(Some(dir.path())).unwrap();
    assert_eq!(roots.data, dir.path().join("data"));
    assert_eq!(roots.desks, dir.path().join("desks"));
    assert_eq!(roots.logs, dir.path().join("logs"));
    assert_eq!(roots.database(), dir.path().join("data/marketrig.sqlite3"));
    assert_eq!(roots.runtime(), dir.path().join("data/runtime"));
    roots.create_dirs().unwrap();
    assert!(roots.runtime().is_dir());
    assert!(roots.desks.is_dir());
    assert!(roots.logs.is_dir());
}

#[cfg(test)]
#[test]
fn migrations_apply_and_stamp() {
    let (dir, store) = open_temp();
    let pragmas = |store: &Store| {
        store
            .call(|c| {
                Ok((
                    c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
                    c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?,
                    c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
                ))
            })
            .unwrap()
    };
    assert_eq!(
        pragmas(&store),
        (MIGRATIONS.len() as i64, "wal".to_string(), 1)
    );

    let tables = store
        .call(|c| {
            c.prepare(
                "SELECT name, strict FROM pragma_table_list \
                 WHERE schema = 'main' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(
        tables,
        [
            "agent_processes",
            "book_snapshots",
            "code_snapshots",
            "desks",
            "executions",
            "fills",
            "firings",
            "hithink_provider",
            "installation_settings",
            "memory_provider",
            "native_sessions",
            "openviking_setup",
            "operational_events",
            "order_events",
            "position_cycles",
            "position_events",
            "prompts",
            "runtimes",
            "trading_actions",
            "triggers",
        ]
        .map(|name| (name.to_string(), 1))
    );

    let index: String = store
        .call(|c| {
            c.query_row(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'operational_events' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(index, "operational_events_tail");

    // Reopening an up-to-date database applies nothing and keeps the stamp.
    drop(store);
    let store = Store::open(&dir.path().join("marketrig.sqlite3")).unwrap();
    assert_eq!(
        pragmas(&store),
        (MIGRATIONS.len() as i64, "wal".to_string(), 1)
    );
}

#[cfg(test)]
#[test]
fn trading_migration_applies() {
    let user_version = |store: &Store| {
        store.call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
    };

    // A fresh database carries every R1 table, present and STRICT.
    let (_dir, store) = open_temp();
    assert_eq!(user_version(&store).unwrap(), MIGRATIONS.len() as i64);
    for name in [
        "book_snapshots",
        "fills",
        "order_events",
        "position_cycles",
        "position_events",
        "prompts",
        "trading_actions",
    ] {
        let strict = store
            .call(move |c| {
                c.query_row(
                    "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [name],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or_else(|e| panic!("{name} must exist: {e}"));
        assert_eq!(strict, 1, "{name} must be STRICT");
    }
    drop(store);

    // A migration-1 database upgrades in place, carrying its rows.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('0199','alpha','READY','/desks/alpha',1000,2000,NULL,NULL);
             INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('019a','beta','CREATING','/desks/beta',1000,NULL,NULL,NULL);
             INSERT INTO operational_events VALUES ('01a0','RECOVERY',NULL,1500,'{\"a\":1}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1i64).unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store).unwrap(), MIGRATIONS.len() as i64);
    let carried: (i64, String, String) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM desks), kind, payload FROM operational_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!(
        carried,
        (2, "RECOVERY".to_string(), "{\"a\":1}".to_string())
    );

    // The rebuilt table keeps its tail index, its payload default, and the
    // widened vocabulary — and still refuses an unknown kind.
    let index: String = store
        .call(|c| {
            c.query_row(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'operational_events' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(index, "operational_events_tail");
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO operational_events (id, kind, desk_id, occurred_at_ns) \
                 VALUES ('01a1','TRADING_NODE_STARTED','0199',1600)",
                [],
            )
        })
        .expect("TRADING_NODE_STARTED must be accepted");
    let defaulted: String = store
        .call(|c| {
            c.query_row(
                "SELECT payload FROM operational_events WHERE id = '01a1'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(defaulted, "{}");
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO operational_events VALUES ('01a2','NODE_WOBBLED',NULL,1700,'{}')",
                [],
            ))
            .is_err(),
        "an unknown kind must still be rejected"
    );
}

// ---------------------------------------------------------------------------
// store::trigger_migration_applies (R2 feature SPEC §11)
// ---------------------------------------------------------------------------

/// Migration 3 (R2 feature SPEC §7): a fresh database carries the whole trigger
/// schema, and a migration-2 database upgrades in place with its rows intact.
#[cfg(test)]
#[test]
fn trigger_migration_applies() {
    // A fresh database lands on migration 3 with every §7 table STRICT and every
    // §7 index present.
    let (_dir, store) = open_temp();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64
    );
    for name in ["code_snapshots", "triggers", "firings", "executions"] {
        let strict = store
            .call(move |c| {
                c.query_row(
                    "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [name],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or_else(|e| panic!("{name} must exist: {e}"));
        assert_eq!(strict, 1, "{name} must be STRICT");
    }
    let indexes: Vec<String> = store
        .call(|c| {
            c.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL \
                 ORDER BY name",
            )?
            .query_map([], |r| r.get(0))?
            .collect()
        })
        .unwrap();
    for index in [
        "executions_running",
        "firings_by_trigger",
        "triggers_due",
        "triggers_live_name",
    ] {
        assert!(indexes.contains(&index.to_string()), "{index}: {indexes:?}");
    }
    // The partial unique index frees a deleted trigger's name (R2-7), and the
    // rebuilt vocabularies accept the new words.
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('0199','alpha','READY','/desks/alpha',1,2,NULL,NULL)",
                [],
            )?;
            for (id, deleted) in [("t1", "9"), ("t2", "NULL")] {
                tx.execute(
                    &format!(
                        "INSERT INTO triggers (id, desk_id, name, recurrence, brief, \
                         at_ns, enabled, revision, created_at_ns, updated_at_ns, deleted_at_ns) \
                         VALUES ('{id}','0199','nightly','ONE_OFF','b',5,1,1,1,1,{deleted})"
                    ),
                    [],
                )?;
            }
            tx.execute(
                "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) VALUES ('p1','0199','TRIGGER_RESULT','QUEUED','{}',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO operational_events VALUES ('e1','TRIGGER_MISSED','0199',1,'{}')",
                [],
            )
        })
        .expect("the widened vocabularies and the partial name index");
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO triggers (id, desk_id, name, recurrence, brief, at_ns, \
                 enabled, revision, created_at_ns, updated_at_ns) \
                 VALUES ('t3','0199','nightly','ONE_OFF','b',5,1,1,1,1)",
                [],
            ))
            .is_err(),
        "a second live trigger of the same name must be rejected"
    );
    drop(store);

    // A migration-2 database upgrades in place, carrying its rows; the new
    // attribution columns arrive NULL.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('0199','alpha','READY','/desks/alpha',1000,2000,NULL,NULL);
             INSERT INTO prompts VALUES ('p0','0199','EVALUATION','QUEUED','{\"a\":1}',1100);
             INSERT INTO trading_actions VALUES \
               ('0199','buy-1','a0','SUBMIT','SESSION','{\"q\":1}','{\"o\":2}',1200);
             INSERT INTO operational_events VALUES ('e0','RECOVERY',NULL,1300,'{\"b\":3}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64
    );
    let prompt: (String, String, String) = store
        .call(|c| {
            c.query_row("SELECT kind, state, payload FROM prompts", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .unwrap();
    assert_eq!(
        prompt,
        ("EVALUATION".into(), "QUEUED".into(), "{\"a\":1}".into())
    );
    let action: (String, String, String, Option<String>, Option<String>) = store
        .call(|c| {
            c.query_row(
                "SELECT action_id, source, outcome, trigger_id, firing_id FROM trading_actions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .unwrap();
    assert_eq!(
        action,
        (
            "buy-1".into(),
            "SESSION".into(),
            "{\"o\":2}".into(),
            None,
            None
        )
    );
    let event: (String, String) = store
        .call(|c| {
            c.query_row("SELECT kind, payload FROM operational_events", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        })
        .unwrap();
    assert_eq!(event, ("RECOVERY".into(), "{\"b\":3}".into()));
    // The rebuilt CHECK ties TRIGGER to a firing.
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO trading_actions \
                 (desk_id, action_id, id, kind, source, request, created_at_ns) \
                 VALUES ('0199','x','a1','SUBMIT','TRIGGER','{}',1)",
                [],
            ))
            .is_err(),
        "a TRIGGER action without a firing_id must be rejected"
    );
}

#[cfg(test)]
#[test]
fn newer_database_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    let store = Store::open(&path).unwrap();
    store
        .call(|c| c.pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1))
        .unwrap();
    drop(store);

    let err = Store::open(&path).unwrap_err();
    assert_eq!(err.code(), "DATABASE_NEWER");
}

#[cfg(test)]
#[test]
fn desk_row_checks() {
    let (_dir, store) = open_temp();
    let insert = |sql: &'static str| store.unit(move |tx| tx.execute(sql, []));

    // CREATING: no ready_at_ns, no failure.
    insert(
        "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES \
         ('0199','alpha','CREATING','/desks/alpha',1000,NULL,NULL,NULL)",
    )
    .expect("valid CREATING row");
    // READY carries ready_at_ns.
    insert(
        "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES \
         ('019a','beta','READY','/desks/beta',1000,2000,NULL,NULL)",
    )
    .expect("valid READY row");
    // FAILED carries a failure code and no ready_at_ns.
    insert(
        "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES \
         ('019b','gamma','FAILED','/desks/gamma',1000,NULL,'BOOTSTRAP_FAILED','Path is a file.')",
    )
    .expect("valid FAILED row");

    for (label, sql) in [
        (
            "READY without ready_at_ns",
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('019c','d1','READY','/p',1000,NULL,NULL,NULL)",
        ),
        (
            "CREATING with ready_at_ns",
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('019d','d2','CREATING','/p',1000,2000,NULL,NULL)",
        ),
        (
            "FAILED without failure_code",
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('019e','d3','FAILED','/p',1000,NULL,NULL,'why')",
        ),
        (
            "FAILED with ready_at_ns",
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('019f','d4','FAILED','/p',1000,2000,'X','why')",
        ),
        (
            "unknown state",
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('01a0','d5','RUNNING','/p',1000,NULL,NULL,NULL)",
        ),
        (
            "non-integer created_at_ns",
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns, failure_code, failure_message) VALUES ('01a1','d6','CREATING','/p','soon',NULL,NULL,NULL)",
        ),
    ] {
        assert!(insert(sql).is_err(), "{label} must be rejected");
    }

    let names: Vec<String> = store
        .call(|c| {
            c.prepare("SELECT name FROM desks ORDER BY name")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .unwrap();
    assert_eq!(names, ["alpha", "beta", "gamma"]);
}

// ---------------------------------------------------------------------------
// store::session_migration_applies (R3 feature SPEC §10 check 7)
// ---------------------------------------------------------------------------

/// Migration 4 (R3 feature SPEC §8): a fresh database carries the runtime,
/// pointer, and process schema, and a migration-3 database upgrades in place
/// with every prompt row intact.
#[cfg(test)]
#[test]
fn session_migration_applies() {
    let (_dir, store) = open_temp();
    for name in ["runtimes", "native_sessions", "agent_processes"] {
        let strict = store
            .call(move |c| {
                c.query_row(
                    "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [name],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or_else(|e| panic!("{name} must exist: {e}"));
        assert_eq!(strict, 1, "{name} must be STRICT");
    }
    // Both runtimes start UNDISCOVERED (§8).
    let seeded: Vec<(String, String)> = store
        .call(|c| {
            c.prepare("SELECT runtime, state FROM runtimes ORDER BY runtime")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .unwrap();
    assert_eq!(
        seeded,
        [
            ("claude".to_string(), "UNDISCOVERED".to_string()),
            ("codex".to_string(), "UNDISCOVERED".to_string()),
        ]
    );
    drop(store);

    // A migration-3 database upgrades in place: every prompt row survives, the
    // QUEUED state carries over unchanged, and the attempt columns arrive NULL.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
               VALUES ('0199','alpha','READY','/desks/alpha',1000,2000);
             INSERT INTO prompts VALUES ('p0','0199','EVALUATION','QUEUED','{\"a\":1}',1100);
             INSERT INTO prompts VALUES ('p1','0199','TRIGGER_RESULT','QUEUED','{\"b\":2}',1200);
             INSERT INTO operational_events VALUES ('e0','TRIGGER_MISSED','0199',1300,'{}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3i64).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64
    );
    #[allow(clippy::type_complexity)]
    let prompts: Vec<(String, String, String, String, Option<i64>, Option<String>)> = store
        .call(|c| {
            c.prepare(
                "SELECT id, kind, state, payload, attempted_at_ns, failure_code FROM prompts \
                 ORDER BY id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect()
        })
        .unwrap();
    assert_eq!(
        prompts,
        [
            (
                "p0".to_string(),
                "EVALUATION".to_string(),
                "QUEUED".to_string(),
                "{\"a\":1}".to_string(),
                None,
                None
            ),
            (
                "p1".to_string(),
                "TRIGGER_RESULT".to_string(),
                "QUEUED".to_string(),
                "{\"b\":2}".to_string(),
                None,
                None
            ),
        ]
    );
    // The existing desks keep the default runtime, and the R2 event survives.
    let carried: (String, String) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT selected_runtime FROM desks), (SELECT kind FROM operational_events)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(carried, ("codex".to_string(), "TRIGGER_MISSED".to_string()));

    // The rebuilt vocabularies accept the R3 words and still refuse the rest.
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns, \
                 resolved_at_ns, runtime) \
                 VALUES ('p2','0199','ORIENTATION','DELIVERED','{}',1400,1500,'claude')",
                [],
            )?;
            tx.execute(
                "INSERT INTO operational_events VALUES ('e1','PROMPT_DELIVERED','0199',1500,'{}')",
                [],
            )
        })
        .expect("the R3 vocabularies");
    for (label, sql) in [
        (
            "DELIVERED without resolved_at_ns",
            "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
             VALUES ('p3','0199','ORIENTATION','DELIVERED','{}',1)",
        ),
        (
            "FAILED without failure_code",
            "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns, \
             resolved_at_ns) VALUES ('p4','0199','DISCLOSURE','FAILED','{}',1,2)",
        ),
        (
            "an unknown event kind",
            "INSERT INTO operational_events VALUES ('e2','SESSION_WOBBLED','0199',1,'{}')",
        ),
        (
            "an ended process without a reason",
            "INSERT INTO agent_processes (id, desk_id, runtime, pid, daemon_uuid, \
             started_at_ns, ended_at_ns) VALUES ('a1','0199','codex',1,'d',1,2)",
        ),
    ] {
        assert!(
            store.unit(move |tx| tx.execute(sql, [])).is_err(),
            "{label} must be rejected"
        );
    }

    // Only one process per desk may be open at a time.
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO agent_processes (id, desk_id, runtime, pid, daemon_uuid, \
                 started_at_ns) VALUES ('a2','0199','codex',1,'d',1)",
                [],
            )
        })
        .expect("one open process");
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO agent_processes (id, desk_id, runtime, pid, daemon_uuid, \
                 started_at_ns) VALUES ('a3','0199','claude',2,'d',2)",
                [],
            ))
            .is_err(),
        "a second open process on the same desk must be rejected"
    );
}
// ---------------------------------------------------------------------------
// store (feature SPEC `openviking-continuity` §9 check 9)
// ---------------------------------------------------------------------------

/// Migration 7 (feature SPEC `openviking-continuity` §6, per OV-6): a fresh
/// database carries the seeded `openviking_setup` row and no `memory_child`,
/// and a schema-6 database carrying `MEMORY_*` events and a `memory_child` row
/// upgrades in place — the memory history deleted, everything else intact, and
/// the vocabulary now naming the eight OpenViking kinds.
#[cfg(test)]
#[test]
fn openviking_migration_applies() {
    let (_dir, store) = open_temp();
    let strict = store
        .call(|c| {
            c.query_row(
                "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                ["openviking_setup"],
                |r| r.get::<_, i64>(0),
            )
        })
        .expect("openviking_setup must exist");
    assert_eq!(strict, 1, "openviking_setup must be STRICT");
    assert!(
        store
            .call(
                |c| c.query_row("SELECT count(*) FROM memory_child", [], |r| r
                    .get::<_, i64>(0))
            )
            .is_err(),
        "memory_child is gone"
    );
    // One seeded row each; `memory_provider` keeps its shape (§6).
    let seeded: (i64, String, i64, i64) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM openviking_setup), \
                 (SELECT state FROM openviking_setup), \
                 (SELECT count(*) FROM memory_provider), \
                 (SELECT updated_at_ns FROM memory_provider)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
        })
        .unwrap();
    assert_eq!(seeded, (1, "UNCONFIGURED".to_string(), 1, 0));
    // A second row and an unknown state are both refused; the four states are not.
    for sql in [
        "INSERT INTO openviking_setup (id, state) VALUES (2, 'UNCONFIGURED')",
        "UPDATE openviking_setup SET state = 'WOBBLED' WHERE id = 1",
    ] {
        assert!(
            store.unit(move |tx| tx.execute(sql, [])).is_err(),
            "{sql} must be rejected"
        );
    }
    for state in ["PROVISIONING", "AVAILABLE", "UNAVAILABLE", "UNCONFIGURED"] {
        let sql = format!("UPDATE openviking_setup SET state = '{state}' WHERE id = 1");
        store
            .unit(move |tx| tx.execute(&sql, []))
            .unwrap_or_else(|e| panic!("{state} must be accepted: {e}"));
    }
    drop(store);

    // A migration-6 database upgrades in place: the memory child row and every
    // MEMORY_ event go, and nothing else does (check 9).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "UPDATE memory_child SET state = 'AVAILABLE', executable_path = '/x/hindsight', \
               validated_at_ns = 1000 WHERE id = 1;
             UPDATE memory_provider SET base_url = 'http://127.0.0.1:9/v1', \
               llm_model = 'llm-1', embedding_model = 'emb-1', \
               embedding_locked_at_ns = 1100, updated_at_ns = 1100 WHERE id = 1;
             INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
               VALUES ('0199','alpha','READY','/desks/alpha',1000,2000);
             INSERT INTO operational_events VALUES ('e0','SESSION_STARTED','0199',1300,'{}');
             INSERT INTO operational_events VALUES ('m0','MEMORY_CONFIGURED',NULL,1400,'{}');
             INSERT INTO operational_events VALUES ('m1','MEMORY_RETAINED','0199',1500,'{}');
             INSERT INTO operational_events VALUES ('e1','RUNTIME_SWITCHED','0199',1600,'{}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 6i64).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64,
        "migration 7 applied"
    );
    let carried: (String, String, String, i64, String) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT name FROM desks), (SELECT state FROM openviking_setup), \
                 (SELECT base_url FROM memory_provider), \
                 (SELECT count(*) FROM operational_events), \
                 (SELECT group_concat(kind, ',') FROM (SELECT kind FROM operational_events \
                    ORDER BY occurred_at_ns))",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .unwrap();
    assert_eq!(
        carried,
        (
            "alpha".to_string(),
            "UNCONFIGURED".to_string(),
            "http://127.0.0.1:9/v1".to_string(),
            2,
            "SESSION_STARTED,RUNTIME_SWITCHED".to_string(),
        ),
        "the memory events are gone and the provider row is not"
    );
    assert!(
        store
            .call(
                |c| c.query_row("SELECT count(*) FROM memory_child", [], |r| r
                    .get::<_, i64>(0))
            )
            .is_err(),
        "memory_child is dropped by the upgrade too"
    );
    // Nothing dangles: the desk the surviving events reference is still there.
    let violations: i64 = store
        .call(|c| {
            c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(violations, 0, "no dangling reference after the rebuild");

    // The rebuilt vocabulary accepts the eight OpenViking kinds and refuses the
    // six that went with Hindsight.
    for (n, kind) in [
        "OPENVIKING_CONFIGURED",
        "OPENVIKING_PROVISIONED",
        "OPENVIKING_STARTED",
        "OPENVIKING_LOST",
        "OPENVIKING_UNAVAILABLE",
        "DESK_MEMORY_PROVISIONED",
        "SKILLS_PROJECTED",
        "SKILLS_PROJECTION_FAILED",
    ]
    .into_iter()
    .enumerate()
    {
        store
            .unit(move |tx| {
                tx.execute(
                    "INSERT INTO operational_events VALUES (?1, ?2, NULL, ?3, '{}')",
                    rusqlite::params![format!("o{n}"), kind, 2000 + n as i64],
                )
            })
            .unwrap_or_else(|e| panic!("{kind} must be accepted: {e}"));
    }
    for (n, kind) in [
        "MEMORY_CONFIGURED",
        "MEMORY_STARTED",
        "MEMORY_LOST",
        "MEMORY_UNAVAILABLE",
        "MEMORY_RETAINED",
        "MEMORY_RECALLED",
        "OPENVIKING_WOBBLED",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            store
                .unit(move |tx| {
                    tx.execute(
                        "INSERT INTO operational_events VALUES (?1, ?2, NULL, ?3, '{}')",
                        rusqlite::params![format!("x{n}"), kind, 3000 + n as i64],
                    )
                })
                .is_err(),
            "{kind} must be rejected"
        );
    }
    // The tail index is back on the rebuilt table.
    let index: String = store
        .call(|c| {
            c.query_row(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'operational_events' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(index, "operational_events_tail");
}

// ---------------------------------------------------------------------------
// store::migration_9_applies (feature SPEC `hithink-a-share` §7)
// ---------------------------------------------------------------------------

/// Migration 9 (feature SPEC `hithink-a-share` §1.1, per HT-1): a fresh
/// database carries the seeded `hithink_provider` row with its two CHECKs, the
/// widened event vocabulary takes `HITHINK_PROVIDER_CHANGED`, and a schema-8
/// database upgrades in place with every event row intact.
#[cfg(test)]
#[test]
fn migration_9_applies() {
    let (_dir, store) = open_temp();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64
    );
    let seeded: (i64, String, String, i64, Option<String>) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT strict FROM pragma_table_list WHERE schema = 'main' \
                    AND name = 'hithink_provider'), \
                 state, a_share_feed, updated_at_ns, failure_code FROM hithink_provider",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .expect("hithink_provider must exist");
    assert_eq!(
        seeded,
        (1, "UNCONFIGURED".to_string(), "YAHOO".to_string(), 0, None)
    );

    // One row, a closed vocabulary, and no HiThink feed without a key (§1.1).
    for sql in [
        "INSERT INTO hithink_provider (id, state, a_share_feed, updated_at_ns) \
         VALUES (2,'UNCONFIGURED','YAHOO',1)",
        "UPDATE hithink_provider SET state = 'WOBBLED' WHERE id = 1",
        "UPDATE hithink_provider SET a_share_feed = 'BLOOMBERG' WHERE id = 1",
        "UPDATE hithink_provider SET a_share_feed = 'HITHINK' WHERE id = 1",
        "UPDATE hithink_provider SET updated_at_ns = NULL WHERE id = 1",
    ] {
        assert!(
            store.unit(move |tx| tx.execute(sql, [])).is_err(),
            "{sql} must be rejected"
        );
    }
    // A key present is what unlocks the toggle.
    store
        .unit(|tx| {
            tx.execute(
                "UPDATE hithink_provider SET state = 'AVAILABLE', a_share_feed = 'HITHINK', \
                 validated_at_ns = 5, updated_at_ns = 5 WHERE id = 1",
                [],
            )
        })
        .expect("AVAILABLE with HITHINK is the configured shape");
    drop(store);

    // A migration-8 database upgrades in place: the event row survives and the
    // rebuilt vocabulary takes the new kind and still refuses an unknown one.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..8] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
               VALUES ('0199','alpha','READY','/desks/alpha',1000,2000);
             INSERT INTO operational_events VALUES ('e0','SESSION_STARTED','0199',1300,'{}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 8i64).unwrap();
    }
    let store = Store::open(&path).unwrap();
    let carried: (String, i64, String) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT kind FROM operational_events), \
                 (SELECT count(*) FROM operational_events), \
                 (SELECT state FROM hithink_provider)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!(
        carried,
        ("SESSION_STARTED".to_string(), 1, "UNCONFIGURED".to_string())
    );
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO operational_events \
                 VALUES ('h0','HITHINK_PROVIDER_CHANGED',NULL,2000,'{}')",
                [],
            )
        })
        .expect("HITHINK_PROVIDER_CHANGED must be accepted");
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO operational_events VALUES ('h1','HITHINK_WOBBLED',NULL,2100,'{}')",
                [],
            ))
            .is_err(),
        "an unknown kind must still be rejected"
    );
    let index: String = store
        .call(|c| {
            c.query_row(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'operational_events' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(index, "operational_events_tail");
    let violations: i64 = store
        .call(|c| {
            c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(violations, 0, "no dangling reference after the rebuild");
}

// ---------------------------------------------------------------------------
// store (R5 feature SPEC §8 check 7)
// ---------------------------------------------------------------------------

/// Migration 6 (feature SPEC `r5-desktop-approval-controls` §2, §3): a fresh
/// database carries the seeded settings row and the approval vocabulary, and a
/// migration-5 database upgrades in place — every row intact, both gated tables
/// backfilled `ALWAYS_ALLOW` with `decided_at_ns = created_at_ns`,
/// `approved_at_ns` gone, and the three new event kinds accepted.
#[cfg(test)]
#[test]
fn approval_migration_applies() {
    let (_dir, store) = open_temp();
    let strict: i64 = store
        .call(|c| {
            c.query_row(
                "SELECT strict FROM pragma_table_list WHERE schema = 'main' \
                 AND name = 'installation_settings'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(strict, 1, "installation_settings must be STRICT");
    let seeded: (i64, String, String, String, i64) = store
        .call(|c| {
            c.query_row(
                "SELECT count(*), trigger_code_policy, paper_order_policy, delivery_mode, \
                 updated_at_ns FROM installation_settings",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .unwrap();
    assert_eq!(
        seeded,
        (
            1,
            "REQUIRE_APPROVAL".to_string(),
            "ALWAYS_ALLOW".to_string(),
            "QUEUE".to_string(),
            0
        )
    );
    // The row is one row, and steering is refused by the column itself.
    for sql in [
        "INSERT INTO installation_settings VALUES (2,'ALWAYS_ALLOW','ALWAYS_ALLOW','QUEUE',1)",
        "UPDATE installation_settings SET delivery_mode = 'STEER' WHERE id = 1",
        "UPDATE installation_settings SET trigger_code_policy = 'MAYBE' WHERE id = 1",
    ] {
        assert!(
            store.unit(move |tx| tx.execute(sql, [])).is_err(),
            "{sql} must be rejected"
        );
    }
    drop(store);

    // A migration-5 database upgrades in place. Its snapshot is referenced by a
    // trigger and by a firing, which is what the rebuild has to keep valid.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
               VALUES ('0199','alpha','READY','/desks/alpha',1000,2000);
             INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, timeout_secs, \
                 fingerprint, approved_at_ns, created_at_ns) \
               VALUES ('s0','0199','print(1)','.py','[\"{script}\"]',300,'ff',1100,1100);
             INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                 enabled, revision, code_snapshot_id, next_occurrence_ns, created_at_ns, \
                 updated_at_ns) \
               VALUES ('t0','0199','morning','SCHEDULED','ONE_OFF','Check the tape.',9000, \
                 1,1,'s0',9000,1100,1100);
             INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, code_snapshot_id) \
               VALUES ('f0','0199','t0',9000,9001,1,'Check the tape.','s0');
             INSERT INTO trading_actions (desk_id, action_id, id, kind, source, trigger_id, \
                 firing_id, request, outcome, created_at_ns) \
               VALUES ('0199','a0','i0','SUBMIT','TRIGGER','t0','f0','{\"q\":\"1\"}', \
                 '{\"status\":\"FILLED\"}',1200);
             INSERT INTO operational_events VALUES ('e0','SESSION_STARTED','0199',1300,'{}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5i64).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64,
        "migration 6 applied"
    );

    // Every row survives, both references still resolve, and the backfill is
    // ALWAYS_ALLOW with decided_at_ns = created_at_ns.
    #[allow(clippy::type_complexity)]
    let carried: (
        String,
        String,
        String,
        String,
        i64,
        String,
        i64,
        String,
        i64,
    ) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT name FROM desks), \
                 (SELECT source FROM code_snapshots WHERE id = 's0'), \
                 (SELECT code_snapshot_id FROM triggers WHERE id = 't0'), \
                 (SELECT code_snapshot_id FROM firings WHERE id = 'f0'), \
                 (SELECT count(*) FROM operational_events), \
                 (SELECT approval FROM code_snapshots WHERE id = 's0'), \
                 (SELECT decided_at_ns FROM code_snapshots WHERE id = 's0'), \
                 (SELECT approval FROM trading_actions WHERE id = 'i0'), \
                 (SELECT decided_at_ns FROM trading_actions WHERE id = 'i0')",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
        })
        .unwrap();
    assert_eq!(
        carried,
        (
            "alpha".to_string(),
            "print(1)".to_string(),
            "s0".to_string(),
            "s0".to_string(),
            1,
            "ALWAYS_ALLOW".to_string(),
            1100,
            "ALWAYS_ALLOW".to_string(),
            1200,
        )
    );
    // The trading action kept its firing attribution and its outcome.
    let action: (String, String, String, String) = store
        .call(|c| {
            c.query_row(
                "SELECT source, trigger_id, firing_id, outcome FROM trading_actions \
                 WHERE id = 'i0'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
        })
        .unwrap();
    assert_eq!(
        action,
        (
            "TRIGGER".to_string(),
            "t0".to_string(),
            "f0".to_string(),
            "{\"status\":\"FILLED\"}".to_string(),
        )
    );
    // Nothing dangles: the rebuild re-pointed both referrers at the new table.
    let violations: i64 = store
        .call(|c| {
            c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(violations, 0, "no dangling reference after the rebuild");

    // R2's approved_at_ns is gone from both tables; the two new columns are there.
    let columns = |table: &'static str| -> Vec<String> {
        store
            .call(move |c| {
                c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?
                    .query_map([table], |r| r.get::<_, String>(0))?
                    .collect()
            })
            .unwrap()
    };
    for table in ["code_snapshots", "trading_actions"] {
        let names = columns(table);
        assert!(
            !names.iter().any(|n| n == "approved_at_ns"),
            "{table} still carries approved_at_ns: {names:?}"
        );
        for column in ["approval", "decided_at_ns"] {
            assert!(names.iter().any(|n| n == column), "{table} lacks {column}");
        }
    }

    // The state vocabulary and its one invariant hold on both tables.
    for table in ["code_snapshots", "trading_actions"] {
        for (approval, decided) in [("BOGUS", "1"), ("PENDING", "1"), ("APPROVED", "NULL")] {
            let sql =
                format!("UPDATE {table} SET approval = '{approval}', decided_at_ns = {decided}");
            assert!(
                store.unit(move |tx| tx.execute(&sql, [])).is_err(),
                "{table} must reject {approval} with decided_at_ns {decided}"
            );
        }
        let sql = format!("UPDATE {table} SET approval = 'PENDING', decided_at_ns = NULL");
        store.unit(move |tx| tx.execute(&sql, [])).unwrap();
    }

    // The widened vocabulary accepts the three R5 kinds and still refuses the rest.
    for (n, kind) in ["POLICY_CHANGED", "APPROVAL_REQUESTED", "APPROVAL_DECIDED"]
        .into_iter()
        .enumerate()
    {
        store
            .unit(move |tx| {
                tx.execute(
                    "INSERT INTO operational_events VALUES (?1, ?2, NULL, ?3, '{}')",
                    rusqlite::params![format!("p{n}"), kind, 2000 + n as i64],
                )
            })
            .unwrap_or_else(|e| panic!("{kind} must be accepted: {e}"));
    }
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO operational_events VALUES ('p9','APPROVAL_WOBBLED',NULL,3000,'{}')",
                [],
            ))
            .is_err(),
        "an unknown approval kind must be rejected"
    );
    // The tail index is back on the rebuilt table.
    let index: String = store
        .call(|c| {
            c.query_row(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'operational_events' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(index, "operational_events_tail");
}

// ---------------------------------------------------------------------------
// store::invocation_migration_applies (`event-triggers` §8)
// ---------------------------------------------------------------------------

/// Migration 10 (`event-triggers` §4): a fresh database carries the rebuilt
/// `triggers` and `firings`, and a migration-9 database upgrades in place with
/// every trigger, firing, execution, and trading action intact.
#[cfg(test)]
#[test]
fn invocation_migration_applies() {
    let user_version = |store: &Store| {
        store.call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
    };
    let columns = |store: &Store, table: &'static str| {
        store
            .call(move |c| {
                c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY name")?
                    .query_map([table], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
    };
    let indexes = |store: &Store| {
        store
            .call(|c| {
                c.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL \
                     ORDER BY name",
                )?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
    };

    // A fresh database: `source` is gone, the two input columns are there, and
    // the two partial unique indexes replace the table UNIQUE.
    let (_dir, store) = open_temp();
    assert_eq!(user_version(&store).unwrap(), 10);
    assert_eq!(MIGRATIONS.len(), 10);
    assert!(!columns(&store, "triggers").contains(&"source".to_string()));
    for column in ["request_id", "input"] {
        assert!(columns(&store, "firings").contains(&column.to_string()));
    }
    for index in ["firings_by_trigger", "firings_invoked", "firings_scheduled"] {
        assert!(indexes(&store).contains(&index.to_string()), "{index}");
    }

    // The relaxed CHECKs: either recurrence may carry its shape or nothing, and
    // the two shapes never mix.
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES ('d1','alpha','READY','/desks/alpha',1,2)",
                [],
            )
        })
        .unwrap();
    // `shape` is the schedule columns and their values; every row is otherwise
    // enabled, revision 1, created and updated at 1.
    let insert = |id: &str, recurrence: &str, columns: &str, values: &str| {
        let sql = format!(
            "INSERT INTO triggers (id, desk_id, name, recurrence, brief{columns}, enabled, \
             revision, created_at_ns, updated_at_ns) \
             VALUES ('{id}','d1','{id}','{recurrence}','b'{values},1,1,1,1)"
        );
        store.unit(move |tx| tx.execute(&sql, []))
    };
    let rule = (
        ", rrule, dtstart, tz",
        ",'FREQ=DAILY','2026-09-03T09:30:00','UTC'",
    );
    for (id, recurrence, columns, values) in [
        ("s1", "ONE_OFF", ", at_ns", ",5"),
        ("s2", "ONE_OFF", "", ""),
        ("s3", "RECURRING", "", ""),
        ("s4", "RECURRING", rule.0, rule.1),
    ] {
        insert(id, recurrence, columns, values)
            .unwrap_or_else(|e| panic!("{id} must be accepted: {e}"));
    }
    for (id, recurrence, columns, values) in [
        // a one-off carrying a rule, and a recurring carrying an instant
        ("x1", "ONE_OFF", rule.0, rule.1),
        ("x2", "RECURRING", ", at_ns", ",5"),
        // half the recurring trio
        ("x3", "RECURRING", ", rrule", ",'FREQ=DAILY'"),
    ] {
        assert!(
            insert(id, recurrence, columns, values).is_err(),
            "{id} must be refused"
        );
    }
    // An input without a request id is not a firing.
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, input) VALUES ('f9','d1','s1',1,1,1,'b','hello')",
                [],
            ))
            .is_err(),
        "an input without a request id must be refused"
    );
    drop(store);

    // A migration-9 database upgrades in place, every row intact.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..9] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
               VALUES ('0199','alpha','READY','/desks/alpha',1000,2000);
             INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, timeout_secs, \
                 fingerprint, approval, decided_at_ns, created_at_ns) \
               VALUES ('s0','0199','print(1)','.py','[\"{script}\"]',300,'ff','APPROVED',1100,1100);
             INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                 enabled, revision, code_snapshot_id, next_occurrence_ns, created_at_ns, \
                 updated_at_ns) \
               VALUES ('t0','0199','morning','SCHEDULED','ONE_OFF','Check the tape.',9000, \
                 1,3,'s0',9000,1100,1100);
             INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, code_snapshot_id) \
               VALUES ('f0','0199','t0',9000,9001,3,'Check the tape.','s0');
             INSERT INTO executions (firing_id, desk_id, daemon_uuid, state, outcome, \
                 exit_code, started_at_ns, finished_at_ns) \
               VALUES ('f0','0199','dae','COMPLETE','EXITED',0,9002,9003);
             INSERT INTO trading_actions (desk_id, action_id, id, kind, source, trigger_id, \
                 firing_id, request, outcome, approval, decided_at_ns, created_at_ns) \
               VALUES ('0199','a0','i0','SUBMIT','TRIGGER','t0','f0','{\"q\":\"1\"}', \
                 '{\"status\":\"FILLED\"}','ALWAYS_ALLOW',1200,1200);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 9i64).unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store).unwrap(), 10);
    assert!(!columns(&store, "triggers").contains(&"source".to_string()));
    let carried: (String, i64, i64, String, i64, String, String) = store
        .call(|c| {
            c.query_row(
                "SELECT t.name, t.revision, t.next_occurrence_ns, f.brief, f.accepted_at_ns, \
                 e.outcome, a.action_id \
                 FROM triggers t JOIN firings f ON f.trigger_id = t.id \
                 JOIN executions e ON e.firing_id = f.id \
                 JOIN trading_actions a ON a.firing_id = f.id",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
        })
        .unwrap();
    assert_eq!(
        carried,
        (
            "morning".into(),
            3,
            9000,
            "Check the tape.".into(),
            9001,
            "EXITED".into(),
            "a0".into()
        )
    );
    // The two rebuilt tables are still the ones `executions` and
    // `trading_actions` name, and nothing dangles.
    for (table, parent) in [("executions", "firings"), ("trading_actions", "firings")] {
        let names: Vec<String> = store
            .call(move |c| {
                c.prepare("SELECT \"table\" FROM pragma_foreign_key_list(?1)")?
                    .query_map([table], |r| r.get::<_, String>(0))?
                    .collect()
            })
            .unwrap();
        assert!(names.contains(&parent.to_string()), "{table}: {names:?}");
    }
    let dangling: i64 = store
        .call(|c| {
            c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(dangling, 0, "PRAGMA foreign_key_check must be empty");
    for index in ["firings_invoked", "firings_scheduled"] {
        assert!(indexes(&store).contains(&index.to_string()), "{index}");
    }
}
