//! Data roots and the durable store.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §2 and §3,
//! root `sdd/SPEC.md` §15.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
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
        Ok(Store { jobs })
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
        self.submit(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })
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
            "installation_settings",
            "memory_child",
            "memory_provider",
            "native_sessions",
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
                        "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, \
                         at_ns, enabled, revision, created_at_ns, updated_at_ns, deleted_at_ns) \
                         VALUES ('{id}','0199','nightly','SCHEDULED','ONE_OFF','b',5,1,1,1,1,{deleted})"
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
                "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                 enabled, revision, created_at_ns, updated_at_ns) \
                 VALUES ('t3','0199','nightly','SCHEDULED','ONE_OFF','b',5,1,1,1,1)",
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
// store (R4 feature SPEC §8 check 6)
// ---------------------------------------------------------------------------

/// Migration 5 (feature SPEC `r4-memory-skills-loop` §6): a fresh database
/// carries the two seeded memory rows, and a migration-4 database upgrades in
/// place with every row intact and the six memory event kinds accepted.
#[cfg(test)]
#[test]
fn memory_migration_applies() {
    let (_dir, store) = open_temp();
    for name in ["memory_child", "memory_provider"] {
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
    // One row each, seeded (§6).
    let seeded: (i64, String, i64, i64) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM memory_child), (SELECT state FROM memory_child), \
                 (SELECT count(*) FROM memory_provider), (SELECT updated_at_ns FROM memory_provider)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
        })
        .unwrap();
    assert_eq!(seeded, (1, "UNCONFIGURED".to_string(), 1, 0));
    // A second row of either is refused, and so is a failure code without the
    // state that must carry it.
    for sql in [
        "INSERT INTO memory_child (id, state) VALUES (2, 'UNCONFIGURED')",
        "UPDATE memory_child SET failure_code = 'NOT_FOUND' WHERE id = 1",
        "UPDATE memory_child SET state = 'UNAVAILABLE' WHERE id = 1",
        "UPDATE memory_child SET state = 'WOBBLED' WHERE id = 1",
    ] {
        assert!(
            store.unit(move |tx| tx.execute(sql, [])).is_err(),
            "{sql} must be rejected"
        );
    }
    drop(store);

    // A migration-4 database upgrades in place: every row survives.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
               VALUES ('0199','alpha','READY','/desks/alpha',1000,2000);
             INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
               VALUES ('p0','0199','EVALUATION','QUEUED','{\"a\":1}',1100);
             UPDATE runtimes SET state = 'AVAILABLE', executable_path = '/x/codex', \
               version = '99.0.0', validated_at_ns = 1200 WHERE runtime = 'codex';
             INSERT INTO operational_events VALUES ('e0','SESSION_STARTED','0199',1300,'{}');
             INSERT INTO operational_events VALUES ('e1','RUNTIME_SWITCHED','0199',1400,'{}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4i64).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        MIGRATIONS.len() as i64
    );
    let carried: (String, String, String, i64, String) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT name FROM desks), (SELECT payload FROM prompts), \
                 (SELECT executable_path FROM runtimes WHERE runtime = 'codex'), \
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
            "{\"a\":1}".to_string(),
            "/x/codex".to_string(),
            2,
            "SESSION_STARTED,RUNTIME_SWITCHED".to_string(),
        )
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

    // The widened vocabulary accepts the six memory kinds and still refuses the rest.
    for (n, kind) in [
        "MEMORY_CONFIGURED",
        "MEMORY_STARTED",
        "MEMORY_LOST",
        "MEMORY_UNAVAILABLE",
        "MEMORY_RETAINED",
        "MEMORY_RECALLED",
    ]
    .into_iter()
    .enumerate()
    {
        store
            .unit(move |tx| {
                tx.execute(
                    "INSERT INTO operational_events VALUES (?1, ?2, NULL, ?3, '{}')",
                    rusqlite::params![format!("m{n}"), kind, 2000 + n as i64],
                )
            })
            .unwrap_or_else(|e| panic!("{kind} must be accepted: {e}"));
    }
    assert!(
        store
            .unit(|tx| tx.execute(
                "INSERT INTO operational_events VALUES ('m9','MEMORY_WOBBLED',NULL,3000,'{}')",
                [],
            ))
            .is_err(),
        "an unknown memory kind must be rejected"
    );
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
             INSERT INTO operational_events VALUES ('e0','MEMORY_RETAINED','0199',1300,'{}');",
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
