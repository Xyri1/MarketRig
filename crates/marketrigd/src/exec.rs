//! Trigger-code execution: the containment primitive, the executor task, the
//! per-firing run, and recovery's execution step.
//!
//! Contract: `sdd/features/r2-scheduled-triggers/SPEC.md` §4.2–§4.5 and §5,
//! root `sdd/SPEC.md` §9 and §15, per R2-3, R2-4, R2-5.

use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;

use crate::store::{Roots, Store, StoreError, now_ns};
use crate::trigger::{ExecutionSummary, FiringRow, insert_result_prompt, load_firing};

/// The captured-stream caps (§4.3). More than this is the `OUTPUT_LIMIT`
/// outcome; the prefix is kept and the truncation flag set.
const STDOUT_CAP: usize = 1024 * 1024;
const STDERR_CAP: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Containment (§4.5)
// ---------------------------------------------------------------------------

/// One spawned child and everything it leads: a `setsid` session and process
/// group on Unix, a Job Object on Windows. The one spawn-and-terminate path for
/// every managed child MarketRig owns (per D73, R2-3), so it exposes the handle
/// §4.5 words and nothing wider.
pub struct Contained {
    child: Box<dyn ChildWrapper>,
}

/// Spawns `command` inside a fresh group. The caller has already set the stdio,
/// the working directory, and the environment.
pub fn spawn(command: Command) -> io::Result<Contained> {
    let mut wrap = CommandWrap::from(command);
    #[cfg(unix)]
    wrap.wrap(ProcessSession);
    #[cfg(windows)]
    wrap.wrap(JobObject);
    Ok(Contained {
        child: wrap.spawn()?,
    })
}

impl Contained {
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin().take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout().take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr().take()
    }

    /// Waits for the whole group to end and reports the leader's status.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().await
    }

    /// Kills the whole group (or ends the job) and reaps it. A termination
    /// failure is a log line, never a caller's problem: the run is over either
    /// way and its outcome is already decided.
    pub async fn terminate(&mut self) {
        if let Err(e) = Box::into_pin(self.child.kill()).await {
            tracing::warn!(error = %e, "terminating a contained process group failed");
        }
    }
}

// ---------------------------------------------------------------------------
// One run (§4.2–§4.4)
// ---------------------------------------------------------------------------

/// Everything one run needs, read once from the rows the claim named.
#[derive(Debug)]
struct Plan {
    source: String,
    suffix: String,
    argv: Vec<String>,
    timeout_secs: u64,
    trigger_name: String,
    recurrence: String,
    desk_name: String,
    workspace_path: String,
    started_at_ns: i64,
}

/// What the run ended as, ready for the completion unit.
struct Finish {
    outcome: &'static str,
    exit_code: Option<i64>,
    error: Option<String>,
    executable: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl Finish {
    fn spawn_failed(executable: &str, error: String) -> Finish {
        Finish {
            outcome: "SPAWN_FAILED",
            exit_code: None,
            error: Some(error),
            executable: executable.to_string(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }
}

/// Runs one claimed firing to its single completion unit (§4.3, steps 1–7).
/// Nothing here ever reruns a firing: the `executions` row is already the
/// at-most-once fact, and every path below ends in exactly one `UPDATE` plus its
/// `TRIGGER_RESULT` prompt.
///
/// `quit` is this run's cancel channel: when it flips the group is terminated
/// and the outcome is `QUIT` instead of whatever it would have been.
pub async fn execute(
    store: &Store,
    roots: &Roots,
    daemon_uuid: &str,
    firing: FiringRow,
    quit: watch::Receiver<bool>,
) {
    // A plan that cannot be read is a database failure, not an execution one:
    // the rows are guaranteed by the claim and by foreign keys. The row stays
    // RUNNING and the next daemon's recovery settles it as DAEMON_LOST.
    let plan = match plan(store, &firing) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::error!(firing_id = firing.id, error = %e, "reading the execution plan failed");
            return;
        }
    };

    let script = roots
        .runtime()
        .join("scripts")
        .join(format!("{}{}", firing.id, plan.suffix));
    let finish = attempt(&plan, &script, &firing, quit).await;
    let finished_at_ns = now_ns();

    let summary = ExecutionSummary {
        outcome: finish.outcome.to_string(),
        exit_code: finish.exit_code,
        error: finish.error.clone(),
        stdout_bytes: finish.stdout.len() as u64,
        stderr_bytes: finish.stderr.len() as u64,
        stdout_truncated: finish.stdout_truncated,
        stderr_truncated: finish.stderr_truncated,
        started_at_ns: plan.started_at_ns,
        finished_at_ns,
    };
    let trigger_name = plan.trigger_name.clone();
    let daemon_uuid = daemon_uuid.to_string();
    let completion = store.unit(move |tx| {
        // The claim is ours and RUNNING, so this matches exactly once; a row
        // some other daemon's recovery already settled is left alone, prompt
        // included.
        let changed = tx.execute(
            "UPDATE executions SET state = 'COMPLETE', outcome = ?1, exit_code = ?2, \
             error = ?3, executable = ?4, stdout = ?5, stderr = ?6, \
             stdout_truncated = ?7, stderr_truncated = ?8, finished_at_ns = ?9 \
             WHERE firing_id = ?10 AND daemon_uuid = ?11 AND state = 'RUNNING'",
            params![
                finish.outcome,
                finish.exit_code,
                finish.error,
                finish.executable,
                finish.stdout,
                finish.stderr,
                finish.stdout_truncated as i64,
                finish.stderr_truncated as i64,
                finished_at_ns,
                firing.id,
                daemon_uuid,
            ],
        )?;
        if changed == 0 {
            return Ok(());
        }
        insert_result_prompt(tx, &firing, &trigger_name, Some(&summary), finished_at_ns)?;
        Ok(())
    });
    if let Err(e) = completion {
        tracing::error!(error = %e, "persisting an execution outcome failed");
    }
    // Step 7's tail: the script is scratch, and a leftover is harmless.
    let _ = std::fs::remove_file(&script);
}

/// Steps 1–6: write the script, spawn contained, feed the document, capture the
/// streams under their caps, and end on exit, cap breach, timeout, or quit.
async fn attempt(
    plan: &Plan,
    script: &Path,
    firing: &FiringRow,
    mut quit: watch::Receiver<bool>,
) -> Finish {
    let executable = plan.argv[0].clone();

    // 1. the source, beside the runtime files and never in the workspace.
    if let Err(e) = write_script(script, &plan.source) {
        return Finish::spawn_failed(&executable, e.to_string());
    }

    // 2. the whole-argument placeholder becomes that absolute path.
    let script_arg = script.to_string_lossy().to_string();
    let argv: Vec<String> = plan
        .argv
        .iter()
        .map(|a| {
            if a == "{script}" {
                script_arg.clone()
            } else {
                a.clone()
            }
        })
        .collect();

    // 3. the daemon's own environment plus the four identifiers, in the desk's
    //    workspace, contained, with every stream piped.
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&plan.workspace_path)
        .env("MARKETRIG_DESK_ID", &firing.desk_id)
        .env("MARKETRIG_DESK_NAME", &plan.desk_name)
        .env("MARKETRIG_TRIGGER_ID", &firing.trigger_id)
        .env("MARKETRIG_FIRING_ID", &firing.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match spawn(command) {
        Ok(child) => child,
        Err(e) => return Finish::spawn_failed(&executable, e.to_string()),
    };

    // 4. the version-1 document, then EOF. Written from its own task so a child
    //    that never reads its input cannot deadlock a document larger than the
    //    pipe buffer.
    if let Some(mut stdin) = child.take_stdin() {
        let document = document(plan, firing).to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(document.as_bytes()).await;
            let _ = stdin.flush().await;
        });
    }

    // 5. both streams, concurrently, under their caps.
    let (breached, mut breach) = mpsc::channel(2);
    let stdout = tokio::spawn(pump(
        child.take_stdout(),
        STDOUT_CAP,
        "stdout",
        breached.clone(),
    ));
    let stderr = tokio::spawn(pump(child.take_stderr(), STDERR_CAP, "stderr", breached));

    // 6. the first of: a cap breach, the group's end, the timeout, a quit.
    let end = tokio::select! {
        biased;
        Some(which) = breach.recv() => End::Limit(which),
        status = child.wait() => match status {
            Ok(status) => End::Exited(status),
            Err(e) => End::WaitFailed(e.to_string()),
        },
        _ = tokio::time::sleep(Duration::from_secs(plan.timeout_secs)) => End::TimedOut,
        _ = quit_requested(&mut quit) => End::Quit,
    };
    if !matches!(end, End::Exited(_) | End::WaitFailed(_)) {
        child.terminate().await;
    }
    let (stdout, stdout_truncated) = stdout.await.unwrap_or_default();
    let (stderr, stderr_truncated) = stderr.await.unwrap_or_default();

    let (outcome, exit_code, error) = match end {
        End::Exited(status) => ("EXITED", status.code().map(i64::from), signal_name(&status)),
        End::WaitFailed(e) => ("EXITED", None, Some(e)),
        End::Limit(which) => ("OUTPUT_LIMIT", None, Some(which.to_string())),
        End::TimedOut => ("TIMED_OUT", None, None),
        End::Quit => ("QUIT", None, None),
    };
    Finish {
        outcome,
        exit_code,
        error,
        executable,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    }
}

/// How a run stopped, before it becomes an outcome.
enum End {
    Exited(ExitStatus),
    /// Waiting itself failed; reported as `EXITED` with the OS error, since the
    /// process did end and no other outcome describes it.
    WaitFailed(String),
    Limit(&'static str),
    TimedOut,
    Quit,
}

/// The version-1 firing document (§4.2), field for field.
fn document(plan: &Plan, firing: &FiringRow) -> Value {
    json!({
        "version": 1,
        "firing": {
            "id": firing.id,
            "occurrence_ns": firing.occurrence_ns,
            "accepted_at_ns": firing.accepted_at_ns,
        },
        "trigger": {
            "id": firing.trigger_id,
            "name": plan.trigger_name,
            "revision": firing.trigger_revision,
            "recurrence": plan.recurrence,
        },
        "desk": {
            "id": firing.desk_id,
            "name": plan.desk_name,
            "workspace_path": plan.workspace_path,
        },
        "brief": firing.brief,
        "context": firing.context,
        "code_snapshot_id": firing.code_snapshot_id,
    })
}

fn write_script(script: &Path, source: &str) -> io::Result<()> {
    if let Some(parent) = script.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(script, source.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Reads one stream to EOF, keeping the first `cap` bytes. More than `cap` is a
/// breach: the prefix is kept, the flag returned, and the run told which stream.
async fn pump<R: AsyncRead + Unpin>(
    reader: Option<R>,
    cap: usize,
    which: &'static str,
    breached: mpsc::Sender<&'static str>,
) -> (Vec<u8>, bool) {
    let Some(mut reader) = reader else {
        return (Vec::new(), false);
    };
    let mut kept = Vec::new();
    let mut chunk = vec![0u8; 8192];
    let mut total = 0usize;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => return (kept, false),
            Ok(n) => {
                total += n;
                if kept.len() < cap {
                    let take = (cap - kept.len()).min(n);
                    kept.extend_from_slice(&chunk[..take]);
                }
                if total > cap {
                    let _ = breached.send(which).await;
                    return (kept, true);
                }
            }
            Err(e) => {
                tracing::warn!(stream = which, error = %e, "reading a child stream failed");
                return (kept, false);
            }
        }
    }
}

/// Resolves only when a quit is actually asked for; a dropped sender means
/// nothing will ever ask.
async fn quit_requested(quit: &mut watch::Receiver<bool>) {
    let gone = quit.wait_for(|q| *q).await.is_err();
    if gone {
        std::future::pending::<()>().await
    }
}

#[cfg(unix)]
fn signal_name(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| {
        match signal {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            4 => "SIGILL",
            6 => "SIGABRT",
            8 => "SIGFPE",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            13 => "SIGPIPE",
            14 => "SIGALRM",
            15 => "SIGTERM",
            other => return format!("SIG{other}"),
        }
        .to_string()
    })
}

#[cfg(not(unix))]
fn signal_name(_status: &ExitStatus) -> Option<String> {
    None
}

/// The snapshot, the trigger, the desk, and the claim instant, in one read.
fn plan(store: &Store, firing: &FiringRow) -> Result<Plan, StoreError> {
    let firing_id = firing.id.clone();
    store.call(move |c| {
        c.query_row(
            "SELECT c.source, c.suffix, c.argv, c.timeout_secs, t.name, t.recurrence, \
             d.name, d.workspace_path, e.started_at_ns \
             FROM firings f \
             JOIN code_snapshots c ON c.id = f.code_snapshot_id \
             JOIN triggers t ON t.id = f.trigger_id \
             JOIN desks d ON d.id = f.desk_id \
             JOIN executions e ON e.firing_id = f.id \
             WHERE f.id = ?1",
            params![firing_id],
            |r| {
                let argv: String = r.get(2)?;
                Ok(Plan {
                    source: r.get(0)?,
                    suffix: r.get(1)?,
                    argv: serde_json::from_str(&argv).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    timeout_secs: r.get::<_, i64>(3)?.max(1) as u64,
                    trigger_name: r.get(4)?,
                    recurrence: r.get(5)?,
                    desk_name: r.get(6)?,
                    workspace_path: r.get(7)?,
                    started_at_ns: r.get(8)?,
                })
            },
        )
    })
}

// ---------------------------------------------------------------------------
// The executor task (§4.3, per R2-4)
// ---------------------------------------------------------------------------

/// The claim unit: for every desk with nothing running, the oldest code-bearing
/// firing that has never been claimed becomes a `RUNNING` execution row under
/// this daemon. The row is the at-most-once fact, so this is the only place a
/// firing is ever attempted.
fn claim(store: &Store, daemon_uuid: &str) -> Result<Vec<FiringRow>, StoreError> {
    let daemon_uuid = daemon_uuid.to_string();
    store.unit(move |tx| {
        let rows: Vec<FiringRow> = {
            let mut stmt = tx.prepare(
                "SELECT id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, context, code_snapshot_id FROM ( \
                   SELECT f.*, row_number() OVER ( \
                     PARTITION BY f.desk_id ORDER BY f.accepted_at_ns, f.id) AS rn \
                   FROM firings f \
                   WHERE f.code_snapshot_id IS NOT NULL \
                     AND NOT EXISTS (SELECT 1 FROM executions e WHERE e.firing_id = f.id) \
                     AND NOT EXISTS (SELECT 1 FROM executions r \
                                     WHERE r.desk_id = f.desk_id AND r.state = 'RUNNING')) \
                 WHERE rn = 1",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FiringRow {
                    id: r.get(0)?,
                    desk_id: r.get(1)?,
                    trigger_id: r.get(2)?,
                    occurrence_ns: r.get(3)?,
                    accepted_at_ns: r.get(4)?,
                    trigger_revision: r.get(5)?,
                    brief: r.get(6)?,
                    context: r.get(7)?,
                    code_snapshot_id: r.get(8)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let started_at_ns = now_ns();
        for firing in &rows {
            tx.execute(
                "INSERT INTO executions (firing_id, desk_id, daemon_uuid, state, started_at_ns) \
                 VALUES (?1, ?2, ?3, 'RUNNING', ?4)",
                params![firing.id, firing.desk_id, daemon_uuid, started_at_ns],
            )?;
        }
        Ok(rows)
    })
}

/// The executor task: claim at start, on every wake, and after every completion;
/// run one firing per desk at a time and different desks concurrently. Returns
/// once `shutdown` has flipped and every running group has been terminated and
/// recorded `QUIT`.
///
/// ponytail: each run blocks its worker on the database thread for the claim and
/// the completion, exactly like the routes do (`api`'s note); a `spawn_blocking`
/// hop belongs here only if a unit ever grows long enough to starve one.
pub async fn run(
    store: Store,
    roots: Roots,
    daemon_uuid: String,
    wake: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut runs = JoinSet::new();
    while !*shutdown.borrow() {
        match claim(&store, &daemon_uuid) {
            Ok(claimed) => {
                for firing in claimed {
                    let store = store.clone();
                    let roots = roots.clone();
                    let daemon_uuid = daemon_uuid.clone();
                    let wake = wake.clone();
                    let quit = shutdown.clone();
                    runs.spawn(async move {
                        execute(&store, &roots, &daemon_uuid, firing, quit).await;
                        // A completed run frees its desk for the next firing.
                        wake.notify_one();
                    });
                }
            }
            Err(e) => tracing::error!(error = %e, "claiming trigger executions failed"),
        }
        tokio::select! {
            _ = wake.notified() => {}
            changed = shutdown.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            Some(_) = runs.join_next() => {}
        }
    }
    // Every run watches the same flag, so they are already terminating; QUIT is
    // persisted before this returns, inside the caller's shutdown bound.
    while runs.join_next().await.is_some() {}
}

// ---------------------------------------------------------------------------
// Recovery (§4.4)
// ---------------------------------------------------------------------------

/// Recovery's execution step (root §15): every execution still `RUNNING` under a
/// daemon that is not this one completes as `DAEMON_LOST` with its result
/// prompt, inside the recovery transaction. Returns one entry per settled row
/// for the `RECOVERY` payload's `executions_lost`. Firings that were never
/// claimed are left alone — never attempted is not an attempt lost.
pub fn recovery_step(
    tx: &Transaction<'_>,
    daemon_uuid: &str,
    now_ns: i64,
) -> rusqlite::Result<Vec<Value>> {
    let lost: Vec<(String, String, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT firing_id, desk_id, daemon_uuid, started_at_ns FROM executions \
             WHERE state = 'RUNNING' AND daemon_uuid <> ?1",
        )?;
        let rows = stmt.query_map(params![daemon_uuid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut settled = Vec::new();
    for (firing_id, desk_id, dead, started_at_ns) in lost {
        tx.execute(
            "UPDATE executions SET state = 'COMPLETE', outcome = 'DAEMON_LOST', \
             exit_code = NULL, error = ?1, stdout = NULL, stderr = NULL, \
             stdout_truncated = 0, stderr_truncated = 0, finished_at_ns = ?2 \
             WHERE firing_id = ?3",
            params![dead, now_ns, firing_id],
        )?;
        let firing =
            load_firing(tx, &desk_id, &firing_id)?.expect("an execution row references its firing");
        let trigger_name: String = tx.query_row(
            "SELECT name FROM triggers WHERE id = ?1",
            params![firing.trigger_id],
            |r| r.get(0),
        )?;
        let summary = ExecutionSummary {
            outcome: "DAEMON_LOST".to_string(),
            exit_code: None,
            error: Some(dead.clone()),
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            started_at_ns,
            finished_at_ns: now_ns,
        };
        insert_result_prompt(tx, &firing, &trigger_name, Some(&summary), now_ns)?;
        settled.push(json!({
            "firing_id": firing_id,
            "desk_id": desk_id,
            "daemon_uuid": dead,
        }));
    }
    Ok(settled)
}

// ---------------------------------------------------------------------------
// Tests (R2 feature SPEC §11)
// ---------------------------------------------------------------------------

/// A store and roots under one scratch directory, with the roots created.
#[cfg(test)]
fn scratch() -> (tempfile::TempDir, Store, Roots) {
    let dir = tempfile::tempdir().unwrap();
    let roots = Roots::resolve(Some(dir.path())).unwrap();
    roots.create_dirs().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    (dir, store, roots)
}

/// The suffix and argv of the shell each MVP platform ships. The trigger
/// definition chooses its own executable; the daemon spawns it without a shell.
#[cfg(test)]
fn shell() -> (&'static str, Vec<&'static str>) {
    if cfg!(windows) {
        (
            ".ps1",
            vec![
                "powershell",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "{script}",
            ],
        )
    } else {
        (".sh", vec!["sh", "{script}"])
    }
}

/// A READY desk with a real workspace directory and one trigger `t-<desk_id>`.
#[cfg(test)]
fn seed_desk(store: &Store, roots: &Roots, desk_id: &str, name: &str) -> String {
    let workspace = roots.desks.join(name);
    std::fs::create_dir_all(&workspace).unwrap();
    let path = workspace.to_string_lossy().to_string();
    let (sql_path, desk_id, name) = (path.clone(), desk_id.to_string(), name.to_string());
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES (?1, ?2, 'READY', ?3, 1, 2)",
                params![desk_id, name, sql_path],
            )?;
            tx.execute(
                "INSERT INTO triggers (id, desk_id, name, source, recurrence, brief, at_ns, \
                 enabled, revision, created_at_ns, updated_at_ns) \
                 VALUES (?1, ?2, 'nightly', 'SCHEDULED', 'ONE_OFF', 'look at AAPL', 50, 1, 7, 1, 1)",
                params![format!("t-{desk_id}"), desk_id],
            )
        })
        .unwrap();
    path
}

/// One code-bearing firing plus its snapshot.
#[cfg(test)]
fn seed_firing(
    store: &Store,
    desk_id: &str,
    firing_id: &str,
    source: &str,
    argv: &[&str],
    timeout_secs: i64,
    accepted_at_ns: i64,
) {
    let (suffix, _) = shell();
    let source = source.to_string();
    let argv = serde_json::to_string(&argv).unwrap();
    let snapshot = format!("c-{firing_id}");
    let (desk_id, firing_id) = (desk_id.to_string(), firing_id.to_string());
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, timeout_secs, \
                 fingerprint, approved_at_ns, created_at_ns) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'fp', 1, 1)",
                params![snapshot, desk_id, source, suffix, argv, timeout_secs],
            )?;
            tx.execute(
                "INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, context, code_snapshot_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 7, 'look at AAPL', 'since open', ?6)",
                params![
                    firing_id,
                    desk_id,
                    format!("t-{desk_id}"),
                    // One occurrence per firing: the §7 unique index is the
                    // duplicate-wake guard, so two firings of one trigger can
                    // never share an instant.
                    accepted_at_ns * 4,
                    accepted_at_ns,
                    snapshot
                ],
            )
        })
        .unwrap();
}

/// The completed execution row: outcome, exit code, error, streams, flags.
#[cfg(test)]
#[allow(clippy::type_complexity)]
fn execution(
    store: &Store,
    firing_id: &str,
) -> (
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Vec<u8>,
    Vec<u8>,
    i64,
    i64,
) {
    let firing_id = firing_id.to_string();
    store
        .call(move |c| {
            c.query_row(
                "SELECT state, outcome, exit_code, error, executable, \
                 coalesce(stdout, x''), coalesce(stderr, x''), \
                 stdout_truncated, stderr_truncated FROM executions WHERE firing_id = ?1",
                params![firing_id],
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
        .unwrap()
}

/// Every `TRIGGER_RESULT` payload naming this firing.
#[cfg(test)]
fn prompts_for(store: &Store, firing_id: &str) -> Vec<Value> {
    let firing_id = firing_id.to_string();
    store
        .call(move |c| {
            c.prepare(
                "SELECT payload FROM prompts WHERE kind = 'TRIGGER_RESULT' \
                 AND json_extract(payload, '$.firing_id') = ?1 ORDER BY id",
            )?
            .query_map(params![firing_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .iter()
        .map(|p| serde_json::from_str(p).unwrap())
        .collect()
}

/// A quit channel whose sender the caller keeps: nothing quits until it flips.
#[cfg(test)]
fn no_quit() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

/// §4.2: the child runs in the desk's workspace with the four identifiers in its
/// environment and the version-1 document on its standard input.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread")]
async fn document_and_environment() {
    let (_dir, store, roots) = scratch();
    let workspace = seed_desk(&store, &roots, "d1", "alpha");
    let source = if cfg!(windows) {
        "Write-Output $env:MARKETRIG_DESK_ID\n\
         Write-Output $env:MARKETRIG_DESK_NAME\n\
         Write-Output $env:MARKETRIG_TRIGGER_ID\n\
         Write-Output $env:MARKETRIG_FIRING_ID\n\
         Write-Output (Get-Location).Path\n\
         Write-Output ([Console]::In.ReadToEnd())\n"
    } else {
        "echo \"$MARKETRIG_DESK_ID\"\n\
         echo \"$MARKETRIG_DESK_NAME\"\n\
         echo \"$MARKETRIG_TRIGGER_ID\"\n\
         echo \"$MARKETRIG_FIRING_ID\"\n\
         pwd\n\
         cat\n"
    };
    seed_firing(&store, "d1", "f1", source, &shell().1, 30, 10);

    let claimed = claim(&store, "daemon-1").unwrap();
    assert_eq!(claimed.len(), 1);
    let (_keep, quit) = no_quit();
    execute(&store, &roots, "daemon-1", claimed[0].clone(), quit).await;

    let (state, outcome, exit_code, _, _, stdout, stderr, _, _) = execution(&store, "f1");
    let text = String::from_utf8(stdout).unwrap();
    assert_eq!(
        (state.as_str(), outcome.as_str(), exit_code),
        ("COMPLETE", "EXITED", Some(0)),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
    assert_eq!(&lines[..4], ["d1", "alpha", "t-d1", "f1"]);
    // `pwd` reports the physical directory, which on macOS resolves the
    // temporary root's symlink; the document still carries the stored path.
    let physical = std::fs::canonicalize(&workspace).unwrap();
    let physical = physical
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    assert_eq!(lines[4], physical);

    let document: Value = serde_json::from_str(lines[5..].join("\n").trim()).unwrap();
    assert_eq!(
        document,
        json!({
            "version": 1,
            "firing": { "id": "f1", "occurrence_ns": 40, "accepted_at_ns": 10 },
            "trigger": { "id": "t-d1", "name": "nightly", "revision": 7, "recurrence": "ONE_OFF" },
            "desk": { "id": "d1", "name": "alpha", "workspace_path": workspace },
            "brief": "look at AAPL",
            "context": "since open",
            "code_snapshot_id": "c-f1",
        })
    );
}

/// §4.4: every outcome writes exactly one completed row and one result prompt,
/// in one unit, and leaves no script behind.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::type_complexity)]
async fn outcomes_one_record_one_prompt() {
    let (_dir, store, roots) = scratch();
    seed_desk(&store, &roots, "d1", "alpha");
    let windows = cfg!(windows);
    let (suffix, argv) = shell();

    // `exit 0` is the same line in both shells.
    let quiet_exit = "exit 0\n";
    let noisy_exit = if windows {
        "[Console]::Error.WriteLine('oops')\nexit 3\n"
    } else {
        "echo oops >&2\nexit 3\n"
    };
    let slow = if windows {
        "Start-Sleep 30\n"
    } else {
        "sleep 30\n"
    };
    let flood = if windows {
        "while ($true) { Write-Output ('x' * 1000) }\n"
    } else {
        "yes 0123456789012345678901234567890123456789\n"
    };

    // (firing, source, argv, timeout, accepted, outcome, exit code)
    let cases: [(&str, &str, Vec<&str>, i64, i64, &str, Option<i64>); 5] = [
        ("f1", quiet_exit, argv.clone(), 30, 10, "EXITED", Some(0)),
        ("f2", noisy_exit, argv.clone(), 30, 20, "EXITED", Some(3)),
        ("f3", slow, argv.clone(), 1, 30, "TIMED_OUT", None),
        ("f4", flood, argv.clone(), 30, 40, "OUTPUT_LIMIT", None),
        (
            "f5",
            quiet_exit,
            vec!["marketrig-no-such-executable", "{script}"],
            30,
            50,
            "SPAWN_FAILED",
            None,
        ),
    ];
    for (firing, source, argv, timeout, accepted, _, _) in &cases {
        seed_firing(&store, "d1", firing, source, argv, *timeout, *accepted);
    }

    for (firing, _, argv, _, _, expected, exit_code) in &cases {
        // One desk runs one firing at a time, so each claim yields exactly the
        // next in acceptance order.
        let claimed = claim(&store, "daemon-1").unwrap();
        assert_eq!(claimed.len(), 1, "{firing}");
        assert_eq!(&claimed[0].id, firing);
        let (_keep, quit) = no_quit();
        execute(&store, &roots, "daemon-1", claimed[0].clone(), quit).await;

        let (state, outcome, code, error, executable, stdout, stderr, out_cut, err_cut) =
            execution(&store, firing);
        assert_eq!((state.as_str(), outcome.as_str()), ("COMPLETE", *expected));
        assert_eq!(code, *exit_code, "{firing}");
        assert_eq!(executable.as_deref(), Some(argv[0]));
        match *expected {
            "EXITED" if *exit_code == Some(3) => {
                assert!(error.is_none());
                assert!(String::from_utf8_lossy(&stderr).contains("oops"));
            }
            "OUTPUT_LIMIT" => {
                assert_eq!(error.as_deref(), Some("stdout"));
                assert_eq!(stdout.len(), STDOUT_CAP);
                assert_eq!((out_cut, err_cut), (1, 0));
            }
            "SPAWN_FAILED" => {
                assert!(error.is_some_and(|e| !e.is_empty()));
                assert!(stdout.is_empty() && stderr.is_empty());
            }
            _ => {}
        }
        if *expected != "OUTPUT_LIMIT" {
            assert_eq!((out_cut, err_cut), (0, 0), "{firing}");
        }

        // Exactly one prompt, whose summary is the row's own story.
        let prompts = prompts_for(&store, firing);
        assert_eq!(prompts.len(), 1, "{firing}");
        assert_eq!(prompts[0]["execution"]["outcome"], json!(*expected));
        assert_eq!(
            prompts[0]["execution"]["exit_code"],
            exit_code.map_or(Value::Null, |c| json!(c))
        );
        assert_eq!(prompts[0]["execution"]["stdout_bytes"], json!(stdout.len()));
        assert_eq!(prompts[0]["trigger_name"], json!("nightly"));

        // The scratch script is gone.
        assert!(
            !roots
                .runtime()
                .join("scripts")
                .join(format!("{firing}{suffix}"))
                .exists(),
            "{firing}"
        );
    }

    // Nothing reran: one row per firing, five prompts in all.
    let (executions, prompts): (i64, i64) = store
        .call(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM executions), \
                 (SELECT count(*) FROM prompts WHERE kind = 'TRIGGER_RESULT')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!((executions, prompts), (5, 5));
}

/// §4.5: terminating ends the whole group, not just the leader.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread")]
async fn group_terminated_on_timeout() {
    use sysinfo::{Pid, System};

    let (_dir, store, roots) = scratch();
    seed_desk(&store, &roots, "d1", "alpha");
    let source = if cfg!(windows) {
        "$p = Start-Process ping -ArgumentList '-n 90 127.0.0.1' -PassThru -WindowStyle Hidden\n\
         Write-Output $p.Id\n\
         Start-Sleep 90\n"
    } else {
        "sleep 60 &\necho $!\nwait\n"
    };
    seed_firing(&store, "d1", "f1", source, &shell().1, 1, 10);

    let claimed = claim(&store, "daemon-1").unwrap();
    let (_keep, quit) = no_quit();
    execute(&store, &roots, "daemon-1", claimed[0].clone(), quit).await;

    let (_, outcome, _, _, _, stdout, _, _, _) = execution(&store, "f1");
    assert_eq!(outcome, "TIMED_OUT");
    let pid: u32 = String::from_utf8_lossy(&stdout)
        .trim()
        .parse()
        .expect("the script prints its grandchild's pid");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if System::new_all().process(Pid::from_u32(pid)).is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "grandchild {pid} outlived the terminated group"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// §4.3, R2-4: one desk at a time, in acceptance order; desks run concurrently.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifo_per_desk_concurrent_across_desks() {
    let (_dir, store, roots) = scratch();
    seed_desk(&store, &roots, "d1", "alpha");
    seed_desk(&store, &roots, "d2", "beta");
    let sleeper = if cfg!(windows) {
        "Start-Sleep 1\n"
    } else {
        "sleep 1\n"
    };
    seed_firing(&store, "d1", "a1", sleeper, &shell().1, 30, 10);
    seed_firing(&store, "d1", "a2", sleeper, &shell().1, 30, 20);
    seed_firing(&store, "d2", "b1", sleeper, &shell().1, 30, 15);

    let wake = Arc::new(Notify::new());
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(run(
        store.clone(),
        roots.clone(),
        "daemon-1".to_string(),
        wake.clone(),
        shutdown,
    ));

    let span = |id: &str| {
        let id = id.to_string();
        store.call(move |c| {
            c.query_row(
                "SELECT started_at_ns, finished_at_ns FROM executions \
                 WHERE firing_id = ?1 AND state = 'COMPLETE'",
                params![id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let done: i64 = store
            .call(|c| {
                c.query_row(
                    "SELECT count(*) FROM executions WHERE state = 'COMPLETE'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        if done == 3 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "runs never finished");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.send(true).unwrap();
    task.await.unwrap();

    let (a1_start, a1_end) = span("a1").unwrap();
    let (a2_start, a2_end) = span("a2").unwrap();
    let (b1_start, b1_end) = span("b1").unwrap();
    assert!(a1_start < a1_end && a2_start < a2_end && b1_start < b1_end);
    // Acceptance order, one at a time.
    assert!(
        a1_end <= a2_start,
        "a1 [{a1_start}, {a1_end}] must finish before a2 starts at {a2_start}"
    );
    // The other desk overlaps the first run.
    assert!(
        b1_start < a1_end && a1_start < b1_end,
        "b1 [{b1_start}, {b1_end}] must overlap a1 [{a1_start}, {a1_end}]"
    );
}

/// §4.4: a daemon that stops while code runs terminates the group and records
/// `QUIT` for it, inside the shutdown bound.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_records_quit() {
    let (_dir, store, roots) = scratch();
    seed_desk(&store, &roots, "d1", "alpha");
    let sleeper = if cfg!(windows) {
        "Start-Sleep 60\n"
    } else {
        "sleep 60\n"
    };
    seed_firing(&store, "d1", "f1", sleeper, &shell().1, 300, 10);

    let wake = Arc::new(Notify::new());
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(run(
        store.clone(),
        roots.clone(),
        "daemon-1".to_string(),
        wake,
        shutdown,
    ));

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let running: i64 = store
            .call(|c| {
                c.query_row(
                    "SELECT count(*) FROM executions WHERE state = 'RUNNING'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        if running == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the firing was never claimed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the executor stops inside the 5-second shutdown bound")
        .unwrap();

    let (state, outcome, exit_code, error, _, _, _, _, _) = execution(&store, "f1");
    assert_eq!(
        (state.as_str(), outcome.as_str(), exit_code, error),
        ("COMPLETE", "QUIT", None, None)
    );
    assert_eq!(prompts_for(&store, "f1").len(), 1);
}

/// §4.4: recovery settles a dead daemon's running executions and leaves this
/// daemon's alone; a never-claimed firing is still pending afterwards.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread")]
async fn recovery_marks_daemon_lost() {
    let (_dir, store, roots) = scratch();
    seed_desk(&store, &roots, "d1", "alpha");
    seed_desk(&store, &roots, "d2", "beta");
    let source = "exit 0\n";
    seed_firing(&store, "d1", "f1", source, &shell().1, 30, 10);
    seed_firing(&store, "d1", "f3", source, &shell().1, 30, 30);
    seed_firing(&store, "d2", "f2", source, &shell().1, 30, 20);
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO executions (firing_id, desk_id, daemon_uuid, state, started_at_ns) \
                 VALUES ('f1','d1','dead-daemon','RUNNING',100)",
                [],
            )?;
            tx.execute(
                "INSERT INTO executions (firing_id, desk_id, daemon_uuid, state, started_at_ns) \
                 VALUES ('f2','d2','live-daemon','RUNNING',110)",
                [],
            )
        })
        .unwrap();

    let settled = store
        .unit(|tx| recovery_step(tx, "live-daemon", 999))
        .unwrap();
    assert_eq!(
        settled,
        vec![json!({
            "firing_id": "f1", "desk_id": "d1", "daemon_uuid": "dead-daemon",
        })]
    );

    let (state, outcome, code, error, _, _, _, out_cut, err_cut) = execution(&store, "f1");
    assert_eq!(
        (state.as_str(), outcome.as_str(), code, error.as_deref()),
        ("COMPLETE", "DAEMON_LOST", None, Some("dead-daemon"))
    );
    assert_eq!((out_cut, err_cut), (0, 0));
    let prompts = prompts_for(&store, "f1");
    assert_eq!(prompts.len(), 1);
    assert_eq!(
        prompts[0]["execution"],
        json!({
            "outcome": "DAEMON_LOST", "exit_code": null, "error": "dead-daemon",
            "stdout_bytes": 0, "stderr_bytes": 0,
            "stdout_truncated": false, "stderr_truncated": false,
            "started_at_ns": 100, "finished_at_ns": 999,
        })
    );
    // This daemon's own run is untouched, and its desk is still busy.
    let state: String = store
        .call(|c| {
            c.query_row(
                "SELECT state FROM executions WHERE firing_id = 'f2'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(state, "RUNNING");
    assert!(prompts_for(&store, "f2").is_empty());

    // The never-claimed firing is claimed now that its desk is free.
    let claimed = claim(&store, "live-daemon").unwrap();
    assert_eq!(
        claimed.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["f3"]
    );
}

/// The `RECOVERY` event carries the settled executions beside the reaped
/// children (§4.4, root §15).
#[cfg(test)]
#[test]
fn recovery_payload_lists_executions_lost() {
    let (_dir, store, roots) = scratch();
    seed_desk(&store, &roots, "d1", "alpha");
    seed_firing(&store, "d1", "f1", "exit 0\n", &shell().1, 30, 10);
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO executions (firing_id, desk_id, daemon_uuid, state, started_at_ns) \
                 VALUES ('f1','d1','dead-daemon','RUNNING',100)",
                [],
            )
        })
        .unwrap();

    crate::daemon::recover(&store, "live-daemon", Vec::new()).unwrap();

    let payload: Value = store
        .call(|c| {
            c.query_row(
                "SELECT payload FROM operational_events WHERE kind = 'RECOVERY'",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .map(|p| serde_json::from_str(&p).unwrap())
        .unwrap();
    assert_eq!(payload["children"], json!([]));
    assert_eq!(
        payload["executions_lost"],
        json!([{ "firing_id": "f1", "desk_id": "d1", "daemon_uuid": "dead-daemon" }])
    );
    assert_eq!(execution(&store, "f1").1, "DAEMON_LOST");
}
