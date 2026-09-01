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
    conn.pragma_update(None, "foreign_keys", true)?;

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
    assert_eq!(pragmas(&store), (2, "wal".to_string(), 1));

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
            "book_snapshots",
            "desks",
            "fills",
            "operational_events",
            "order_events",
            "position_cycles",
            "position_events",
            "prompts",
            "trading_actions",
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
    assert_eq!(pragmas(&store), (2, "wal".to_string(), 1));
}

#[cfg(test)]
#[test]
fn trading_migration_applies() {
    let user_version = |store: &Store| {
        store.call(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
    };

    // A fresh database lands on migration 2 with every R1 table present and STRICT.
    let (_dir, store) = open_temp();
    assert_eq!(user_version(&store).unwrap(), 2);
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
            "INSERT INTO desks VALUES ('0199','alpha','READY','/desks/alpha',1000,2000,NULL,NULL);
             INSERT INTO desks VALUES ('019a','beta','CREATING','/desks/beta',1000,NULL,NULL,NULL);
             INSERT INTO operational_events VALUES ('01a0','RECOVERY',NULL,1500,'{\"a\":1}');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1i64).unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store).unwrap(), 2);
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

#[cfg(test)]
#[test]
fn newer_database_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketrig.sqlite3");
    let store = Store::open(&path).unwrap();
    store
        .call(|c| c.pragma_update(None, "user_version", 3i64))
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
        "INSERT INTO desks VALUES \
         ('0199','alpha','CREATING','/desks/alpha',1000,NULL,NULL,NULL)",
    )
    .expect("valid CREATING row");
    // READY carries ready_at_ns.
    insert(
        "INSERT INTO desks VALUES \
         ('019a','beta','READY','/desks/beta',1000,2000,NULL,NULL)",
    )
    .expect("valid READY row");
    // FAILED carries a failure code and no ready_at_ns.
    insert(
        "INSERT INTO desks VALUES \
         ('019b','gamma','FAILED','/desks/gamma',1000,NULL,'BOOTSTRAP_FAILED','Path is a file.')",
    )
    .expect("valid FAILED row");

    for (label, sql) in [
        (
            "READY without ready_at_ns",
            "INSERT INTO desks VALUES ('019c','d1','READY','/p',1000,NULL,NULL,NULL)",
        ),
        (
            "CREATING with ready_at_ns",
            "INSERT INTO desks VALUES ('019d','d2','CREATING','/p',1000,2000,NULL,NULL)",
        ),
        (
            "FAILED without failure_code",
            "INSERT INTO desks VALUES ('019e','d3','FAILED','/p',1000,NULL,NULL,'why')",
        ),
        (
            "FAILED with ready_at_ns",
            "INSERT INTO desks VALUES ('019f','d4','FAILED','/p',1000,2000,'X','why')",
        ),
        (
            "unknown state",
            "INSERT INTO desks VALUES ('01a0','d5','RUNNING','/p',1000,NULL,NULL,NULL)",
        ),
        (
            "non-integer created_at_ns",
            "INSERT INTO desks VALUES ('01a1','d6','CREATING','/p','soon',NULL,NULL,NULL)",
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
