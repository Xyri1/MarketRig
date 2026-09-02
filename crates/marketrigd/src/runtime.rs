//! Runtime discovery: the two `runtimes` rows and the probes behind them.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §2 (per R3-1),
//! root `sdd/SPEC.md` §4.4.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::json;

use crate::desk::append_event;
use crate::store::{Store, StoreError, now_ns};

/// The two runtimes MarketRig knows (per D24).
pub const RUNTIMES: [&str; 2] = ["codex", "claude"];

/// The version floors verified on 2026-09-02 (§2).
const FLOORS: [(&str, (u64, u64, u64)); 2] = [("codex", (0, 152, 1)), ("claude", (2, 1, 258))];

/// Every probe is bounded at ten seconds (§2).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// One `runtimes` row (§8), secrets-free by construction.
#[derive(Debug, Clone, Serialize)]
pub struct Runtime {
    pub runtime: String,
    /// `UNDISCOVERED` | `AVAILABLE` | `UNAVAILABLE`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
}

pub fn known(runtime: &str) -> bool {
    RUNTIMES.contains(&runtime)
}

const SELECT: &str = "SELECT runtime, state, executable_path, version, validated_at_ns, \
                      failure_code, failure_message FROM runtimes";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Runtime> {
    Ok(Runtime {
        runtime: row.get(0)?,
        state: row.get(1)?,
        executable_path: row.get(2)?,
        version: row.get(3)?,
        validated_at_ns: row.get(4)?,
        failure_code: row.get(5)?,
        failure_message: row.get(6)?,
    })
}

/// Both rows in a stable order (`GET /runtimes`, §7).
pub fn rows(store: &Store) -> Result<Vec<Runtime>, StoreError> {
    store.call(|c| {
        c.prepare(&format!("{SELECT} ORDER BY runtime"))?
            .query_map([], read)?
            .collect()
    })
}

/// One row, or `None` for an unknown runtime name.
pub fn get(store: &Store, runtime: &str) -> Result<Option<Runtime>, StoreError> {
    let runtime = runtime.to_string();
    store.call(move |c| {
        c.query_row(&format!("{SELECT} WHERE runtime = ?1"), [runtime], read)
            .optional()
    })
}

/// The validation outcome discovery writes (§2).
enum Outcome {
    Available { path: String, version: String },
    Failed { code: &'static str, message: String },
}

/// `POST /runtimes/{r}/retry` (§7, §4.1): clear whatever failure the row holds —
/// a probe failure or the control-plane's `CONTROL_PLANE_FAILED` — and discover
/// again.
pub fn retry(store: &Store, runtime: &str, search_path: &str) -> Result<Runtime, StoreError> {
    let name = runtime.to_string();
    store.unit(move |tx| {
        tx.execute(
            "UPDATE runtimes SET failure_code = NULL, failure_message = NULL WHERE runtime = ?1",
            [name],
        )
    })?;
    discover(store, runtime, None, search_path)
}

/// The one discovery function (§2). `search_path` is the `PATH` the daemon
/// captured at start; `explicit` bypasses it entirely.
pub fn discover(
    store: &Store,
    runtime: &str,
    explicit: Option<&Path>,
    search_path: &str,
) -> Result<Runtime, StoreError> {
    let outcome = validate(runtime, explicit, search_path);
    let name = runtime.to_string();
    let at_ns = now_ns();
    store.unit(move |tx| {
        match &outcome {
            Outcome::Available { path, version } => {
                tx.execute(
                    "UPDATE runtimes SET state = 'AVAILABLE', executable_path = ?2, version = ?3, \
                     validated_at_ns = ?4, failure_code = NULL, failure_message = NULL \
                     WHERE runtime = ?1",
                    params![name, path, version, at_ns],
                )?;
                append_event(
                    tx,
                    "RUNTIME_DISCOVERED",
                    None,
                    at_ns,
                    json!({ "runtime": name, "executable_path": path, "version": version }),
                )?;
            }
            Outcome::Failed { code, message } => {
                tx.execute(
                    "UPDATE runtimes SET state = 'UNAVAILABLE', failure_code = ?2, \
                     failure_message = ?3 WHERE runtime = ?1",
                    params![name, code, message],
                )?;
                append_event(
                    tx,
                    "RUNTIME_UNAVAILABLE",
                    None,
                    at_ns,
                    json!({ "runtime": name, "failure_code": code, "failure_message": message }),
                )?;
            }
        }
        tx.query_row(&format!("{SELECT} WHERE runtime = ?1"), [&name], read)
    })
}

/// Startup step 6a (§2): every `UNDISCOVERED` row is discovered before the
/// listener binds. A discovery failure is a row, never a startup failure.
pub fn discover_undiscovered(store: &Store) -> Result<(), StoreError> {
    let pending: Vec<String> = store.call(|c| {
        c.prepare("SELECT runtime FROM runtimes WHERE state = 'UNDISCOVERED' ORDER BY runtime")?
            .query_map([], |r| r.get(0))?
            .collect()
    })?;
    if pending.is_empty() {
        return Ok(());
    }
    let path = search_path();
    for runtime in pending {
        discover(store, &runtime, None, &path)?;
    }
    Ok(())
}

/// The user's real `PATH` (§2): the login shell's on macOS, captured once per
/// daemon start and kept in memory only; the registry's on Windows, which is
/// cheap enough to read fresh but is cached the same way.
pub fn search_path() -> String {
    static CAPTURED: OnceLock<String> = OnceLock::new();
    CAPTURED.get_or_init(capture_path).clone()
}

#[cfg(not(windows))]
fn capture_path() -> String {
    let own = std::env::var("PATH").unwrap_or_default();
    let Some(shell) = std::env::var_os("SHELL") else {
        return own;
    };
    let mut command = Command::new(shell);
    command.args(["-l", "-c", "printf %s \"$PATH\""]);
    match run(command) {
        Some((true, out)) if !out.trim().is_empty() => out.trim().to_string(),
        _ => own,
    }
}

/// ponytail: `reg.exe query` instead of a registry crate — two reads once per
/// daemon start, and the alternative is a dependency for `RegOpenKeyExW`.
/// Swap in `windows`' registry API if this ever runs on a hot path.
#[cfg(windows)]
fn capture_path() -> String {
    let mut parts = Vec::new();
    for (key, name) in [
        ("HKCU\\Environment", "Path"),
        (
            "HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
            "Path",
        ),
    ] {
        let mut command = Command::new("reg.exe");
        command.args(["query", key, "/v", name]);
        if let Some((true, out)) = run(command) {
            // `    Path    REG_EXPAND_SZ    C:\a;C:\b`
            if let Some(value) = out
                .lines()
                .find(|l| l.trim_start().starts_with(name))
                .and_then(|l| l.split_whitespace().nth(2).map(|_| l))
                .and_then(|l| l.split_once("REG_"))
                .and_then(|(_, rest)| rest.split_once("SZ"))
                .map(|(_, value)| value.trim())
            {
                parts.push(value.to_string());
            }
        }
    }
    if parts.is_empty() {
        return std::env::var("PATH").unwrap_or_default();
    }
    parts.join(";")
}

fn validate(runtime: &str, explicit: Option<&Path>, search_path: &str) -> Outcome {
    let executable = match explicit {
        Some(path) => {
            if !path.is_file() {
                return Outcome::Failed {
                    code: "NOT_FOUND",
                    message: format!("No executable at {}.", path.display()),
                };
            }
            path.to_path_buf()
        }
        None => match resolve(runtime, search_path) {
            Some(path) => path,
            None => {
                return Outcome::Failed {
                    code: "NOT_FOUND",
                    message: format!("No {runtime} executable is on the user's PATH."),
                };
            }
        },
    };

    // The desktop bundles ship a different program behind the same name (per D3).
    if let Some(bundle) = ["Codex.app", "ChatGPT.app"]
        .into_iter()
        .find(|bundle| executable.components().any(|c| c.as_os_str() == *bundle))
    {
        return Outcome::Failed {
            code: "CAPABILITY_MISSING",
            message: format!(
                "{} is inside the {bundle} desktop bundle, which is not a MarketRig runtime target.",
                executable.display()
            ),
        };
    }

    let Some((ok, version_out)) = run(probe(&executable, "--version")) else {
        return Outcome::Failed {
            code: "PROBE_FAILED",
            message: format!("{} --version did not answer.", executable.display()),
        };
    };
    if !ok {
        return Outcome::Failed {
            code: "PROBE_FAILED",
            message: format!("{} --version exited non-zero.", executable.display()),
        };
    }
    let Some(version) = first_version(&version_out) else {
        return Outcome::Failed {
            code: "PROBE_FAILED",
            message: format!(
                "{} --version printed no dotted version.",
                executable.display()
            ),
        };
    };
    let floor = FLOORS
        .iter()
        .find(|(name, _)| *name == runtime)
        .map(|(_, floor)| *floor)
        .unwrap_or((0, 0, 0));
    if parse(&version).unwrap_or((0, 0, 0)) < floor {
        return Outcome::Failed {
            code: "VERSION_UNSUPPORTED",
            message: format!(
                "{runtime} {version} is older than the supported floor {}.{}.{}.",
                floor.0, floor.1, floor.2
            ),
        };
    }

    let Some((_, help)) = run(probe(&executable, "--help")) else {
        return Outcome::Failed {
            code: "PROBE_FAILED",
            message: format!("{} --help did not answer.", executable.display()),
        };
    };
    let required: &[&str] = match runtime {
        "codex" => &["app-server"],
        _ => &["--dangerously-load-development-channels", "--settings"],
    };
    if let Some(missing) = required.iter().find(|marker| !help.contains(**marker)) {
        return Outcome::Failed {
            code: "CAPABILITY_MISSING",
            message: format!("{runtime} {version} does not offer {missing}."),
        };
    }

    Outcome::Available {
        path: executable.to_string_lossy().into_owned(),
        version,
    }
}

/// The first entry of `search_path` carrying the bare name (§2). Windows accepts
/// `.exe`, `.cmd`, and `.bat`; the latter two are launched through `%ComSpec%`.
fn resolve(runtime: &str, search_path: &str) -> Option<PathBuf> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    search_path
        .split(separator)
        .filter(|dir| !dir.is_empty())
        .flat_map(|dir| {
            suffixes
                .iter()
                .map(move |suffix| Path::new(dir).join(format!("{runtime}{suffix}")))
        })
        .find(|candidate| candidate.is_file())
}

/// A probe command: the executable directly, or `%ComSpec% /d /c` in front of a
/// Windows batch launcher (§2).
fn probe(executable: &Path, arg: &str) -> Command {
    let batch = executable
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if batch {
        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut command = Command::new(shell);
        command.args(["/d", "/c"]);
        command.arg(executable);
        command.arg(arg);
        command
    } else {
        let mut command = Command::new(executable);
        command.arg(arg);
        command
    }
}

/// Runs a probe with the §2 timeout, answering `(exit succeeded, stdout)` or
/// `None` when it could not be started or did not finish in time.
fn run(mut command: Command) -> Option<(bool, String)> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = stdout.read_to_string(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    status.map(|status| (status.success(), out))
}

/// The first `\d+.\d+.\d+` on standard output (§2).
fn first_version(out: &str) -> Option<String> {
    let bytes = out.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() || (start > 0 && bytes[start - 1] == b'.') {
            continue;
        }
        let mut end = start;
        let mut dots = 0;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            if bytes[end] == b'.' {
                dots += 1;
                if dots > 2 {
                    break;
                }
            }
            end += 1;
        }
        let candidate = out[start..end].trim_end_matches('.');
        if parse(candidate).is_some() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let triple = (next()?, next()?, next()?);
    parts.next().is_none().then_some(triple)
}

// ---------------------------------------------------------------------------
// runtime::discovery (feature SPEC §10 check 1)
// ---------------------------------------------------------------------------

/// Writes an executable stand-in that prints `version_line` for `--version` and
/// `help` for `--help`, and returns its path.
#[cfg(test)]
fn standin(dir: &Path, name: &str, version_line: &str, help: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join(format!("{name}.cmd"));
        std::fs::write(
            &path,
            format!(
                "@echo off\r\nif \"%1\"==\"--version\" echo {version_line}\r\n\
                 if \"%1\"==\"--help\" echo {help}\r\n"
            ),
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n--version) echo '{version_line}';;\n\
                 --help) echo '{help}';;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

#[cfg(test)]
const CODEX_HELP: &str = "Usage: codex [COMMAND] app-server resume";

#[cfg(test)]
const CLAUDE_HELP: &str = "--dangerously-load-development-channels --settings";

#[cfg(test)]
#[test]
fn version_parsing_and_floor() {
    assert_eq!(
        first_version("codex-cli 0.152.1").as_deref(),
        Some("0.152.1")
    );
    assert_eq!(
        first_version("2.1.258 (Claude Code)").as_deref(),
        Some("2.1.258")
    );
    assert_eq!(first_version("no version here"), None);
    assert_eq!(first_version("1.2"), None);
    assert!(parse("0.150.1").unwrap() < (0, 152, 1));
    assert!(parse("0.152.1").unwrap() >= (0, 152, 1));
    assert!(parse("1.0.0").unwrap() >= (0, 152, 1));
}

#[cfg(test)]
#[test]
fn discovery_outcomes() {
    let (_dir, store) = crate::store::open_temp();
    let bin = tempfile::tempdir().unwrap();

    // Both rows start UNDISCOVERED (§8).
    let start = rows(&store).unwrap();
    assert_eq!(
        start.iter().map(|r| r.state.as_str()).collect::<Vec<_>>(),
        ["UNDISCOVERED", "UNDISCOVERED"]
    );

    // Explicit path wins: the executable is validated and PATH is never consulted.
    let codex = standin(bin.path(), "codex", "codex-cli 99.0.0", CODEX_HELP);
    let row = discover(&store, "codex", Some(&codex), "").unwrap();
    assert_eq!(row.state, "AVAILABLE");
    assert_eq!(row.version.as_deref(), Some("99.0.0"));
    assert_eq!(
        row.executable_path.as_deref(),
        Some(codex.to_str().unwrap())
    );
    assert!(row.validated_at_ns.is_some() && row.failure_code.is_none());

    // PATH resolution: the same stand-in found by name in the search path.
    let claude = standin(bin.path(), "claude", "2.1.258", CLAUDE_HELP);
    let path = bin.path().to_str().unwrap();
    let row = discover(&store, "claude", None, path).unwrap();
    assert_eq!(row.state, "AVAILABLE");
    assert_eq!(
        row.executable_path.as_deref(),
        Some(claude.to_str().unwrap())
    );

    // NOT_FOUND: an explicit path that is not there, and an empty search path.
    let missing = bin.path().join("nothing-here");
    let row = discover(&store, "codex", Some(&missing), path).unwrap();
    assert_eq!(
        (row.state.as_str(), row.failure_code.as_deref()),
        ("UNAVAILABLE", Some("NOT_FOUND"))
    );
    let empty = tempfile::tempdir().unwrap();
    let row = discover(&store, "codex", None, empty.path().to_str().unwrap()).unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("NOT_FOUND"));

    // VERSION_UNSUPPORTED: below the floor, whatever the help says.
    let old = standin(empty.path(), "codex", "codex-cli 0.150.1", CODEX_HELP);
    let row = discover(&store, "codex", Some(&old), "").unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("VERSION_UNSUPPORTED"));
    assert_eq!(row.state, "UNAVAILABLE");

    // CAPABILITY_MISSING: the version is fine but the flag is absent.
    let thin = standin(empty.path(), "claude", "9.0.0", "--settings only");
    let row = discover(&store, "claude", Some(&thin), "").unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("CAPABILITY_MISSING"));

    // PROBE_FAILED: `--version` prints nothing usable.
    let mute = standin(empty.path(), "mute", "nothing", CODEX_HELP);
    let row = discover(&store, "codex", Some(&mute), "").unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("PROBE_FAILED"));

    // The desktop bundle is refused before any probe runs (per D3).
    let bundle = bin.path().join("Codex.app/Contents/MacOS");
    std::fs::create_dir_all(&bundle).unwrap();
    let bundled = standin(&bundle, "codex", "codex-cli 99.0.0", CODEX_HELP);
    let row = discover(&store, "codex", Some(&bundled), "").unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("CAPABILITY_MISSING"));
    assert!(row.failure_message.unwrap().contains("Codex.app"));

    // Retry clears the failure and discovers again.
    let row = retry(&store, "codex", bin.path().to_str().unwrap()).unwrap();
    assert_eq!(row.state, "AVAILABLE");
    assert!(row.failure_code.is_none() && row.failure_message.is_none());

    // Every outcome left one event behind, and no row carries a secret.
    let kinds: Vec<String> = store
        .call(|c| {
            c.prepare(
                "SELECT kind FROM operational_events WHERE kind LIKE 'RUNTIME_%' \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| r.get(0))?
            .collect()
        })
        .unwrap();
    assert_eq!(
        kinds,
        [
            "RUNTIME_DISCOVERED",
            "RUNTIME_DISCOVERED",
            "RUNTIME_UNAVAILABLE",
            "RUNTIME_UNAVAILABLE",
            "RUNTIME_UNAVAILABLE",
            "RUNTIME_UNAVAILABLE",
            "RUNTIME_UNAVAILABLE",
            "RUNTIME_UNAVAILABLE",
            "RUNTIME_DISCOVERED",
        ]
    );
}
