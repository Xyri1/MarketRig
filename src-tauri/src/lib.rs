//! The desktop shell (R5-6).
//!
//! Contract: `sdd/features/r5-desktop-approval-controls/SPEC.md` §5, root
//! `sdd/SPEC.md` §4.3 and §14. The shell performs no HTTP: it discovers the
//! endpoint file, starts the daemon sidecar, and owns the window and the tray.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

/// The endpoint file the daemon writes (root §4.3); `credential` is the bearer.
#[derive(Deserialize)]
struct EndpointFile {
    port: u16,
    credential: String,
    daemon_uuid: String,
}

/// What the webview sees. `credential` is renamed `bearer` here and nowhere
/// else; no `Debug`, so the bearer never reaches a log line.
#[derive(Serialize, Clone)]
pub struct Endpoint {
    pub port: u16,
    pub bearer: String,
    pub daemon_uuid: String,
}

impl From<EndpointFile> for Endpoint {
    fn from(f: EndpointFile) -> Self {
        Endpoint {
            port: f.port,
            bearer: f.credential,
            daemon_uuid: f.daemon_uuid,
        }
    }
}

/// The application-data root, the same three lines `Roots::resolve` runs
/// (`crates/marketrigd/src/store.rs`). Copied rather than depended on: the
/// daemon crate drags the whole nautilus graph into the shell build.
pub fn data_root() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("MARKETRIG_TEST_DATA_ROOT") {
        return Ok(PathBuf::from(dir).join("data"));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(PathBuf::from(home).join("Library/Application Support/MarketRig"))
    }
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
        Ok(PathBuf::from(local).join("MarketRig"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("MarketRig runs on macOS and Windows".to_string())
    }
}

pub fn endpoint_path(data_root: &Path) -> PathBuf {
    data_root.join("runtime").join("endpoint.json")
}

/// `None` when the file is absent; `Err` when it is there but unreadable.
pub fn read_endpoint_at(path: &Path) -> Result<Option<Endpoint>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("ENDPOINT_MALFORMED: {e}")),
    };
    serde_json::from_slice::<EndpointFile>(&bytes)
        .map(|f| Some(f.into()))
        .map_err(|e| format!("ENDPOINT_MALFORMED: {e}"))
}

/// `marketrigd` beside the running executable. In a packaged build that is the
/// sidecar Tauri placed next to the app binary; under `tauri dev` the shell's
/// own exe already lives in `target/debug`, where `cargo build` puts the
/// daemon — so one rule covers both.
pub fn daemon_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("DAEMON_START_FAILED: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("DAEMON_START_FAILED: the executable has no directory")?;
    let name = if cfg!(windows) {
        "marketrigd.exe"
    } else {
        "marketrigd"
    };
    Ok(dir.join(name))
}

fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
}

const STDERR_TAIL: usize = 2048;

/// Spawns the daemon detached and waits for the endpoint file to name a
/// `daemon_uuid` other than `before`. The `Child` is dropped on success —
/// `std::process::Child` does not kill on drop, so the shell's exit or crash
/// never takes the daemon with it.
pub fn spawn_and_wait(
    program: &Path,
    args: &[&str],
    data_root: &Path,
    before: Option<&str>,
    timeout: Duration,
) -> Result<Endpoint, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(data_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    detach(&mut command);
    let mut child: Child = command
        .spawn()
        .map_err(|e| format!("DAEMON_START_FAILED: {e}"))?;

    // The reader keeps draining for the daemon's whole life so its stderr pipe
    // never fills; only the last 2 KiB is kept, for the failure message.
    let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    if let Some(mut err) = child.stderr.take() {
        let tail = Arc::clone(&tail);
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    return;
                }
                let mut kept = tail.lock().unwrap();
                kept.extend_from_slice(&buf[..n]);
                let over = kept.len().saturating_sub(STDERR_TAIL);
                kept.drain(..over);
            }
        });
    }
    let tail_text = || {
        String::from_utf8_lossy(&tail.lock().unwrap())
            .trim()
            .to_string()
    };

    let path = endpoint_path(data_root);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(endpoint) = read_endpoint_at(&path).ok().flatten()
            && Some(endpoint.daemon_uuid.as_str()) != before
        {
            return Ok(endpoint);
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Let the reader drain what the child wrote before it died.
            std::thread::sleep(Duration::from_millis(100));
            return Err(format!(
                "DAEMON_START_FAILED: {program:?} exited {status}: {}",
                tail_text()
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(format!(
                "DAEMON_START_FAILED: no new daemon within {}s: {}",
                timeout.as_secs(),
                tail_text()
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The tray's pending count. Slice 008 builds the tray that reads it.
#[derive(Default)]
pub struct TrayPending(pub Mutex<u32>);

pub fn pending_label(n: u32) -> String {
    format!("{n} pending approvals")
}

#[tauri::command]
fn read_endpoint() -> Result<Option<Endpoint>, String> {
    read_endpoint_at(&endpoint_path(&data_root()?))
}

#[tauri::command]
fn start_daemon() -> Result<Endpoint, String> {
    let root = data_root()?;
    std::fs::create_dir_all(root.join("runtime"))
        .map_err(|e| format!("DAEMON_START_FAILED: {e}"))?;
    let before = read_endpoint_at(&endpoint_path(&root))
        .ok()
        .flatten()
        .map(|e| e.daemon_uuid);
    spawn_and_wait(
        &daemon_path()?,
        &[],
        &root,
        before.as_deref(),
        Duration::from_secs(30),
    )
}

#[tauri::command]
fn set_tray_pending(n: u32, pending: State<'_, TrayPending>) {
    *pending.0.lock().unwrap() = n;
    // slice 008: tray — update the menu line to `pending_label(n)` and the
    // macOS title / Windows tooltip.
}

#[tauri::command]
fn exit_app(app: AppHandle) {
    app.exit(0);
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--hidden"])
                .app_name("MarketRig")
                .build(),
        )
        .plugin(
            tauri_plugin_log::Builder::new()
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("MarketRig".into()),
                    },
                ))
                .max_file_size(5 * 1024 * 1024)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .build(),
        )
        .manage(TrayPending::default())
        .invoke_handler(tauri::generate_handler![
            read_endpoint,
            start_daemon,
            set_tray_pending,
            exit_app
        ])
        .setup(|app| {
            if std::env::args().any(|a| a == "--hidden")
                && let Some(window) = app.get_webview_window("main")
            {
                window.hide()?;
            }
            // slice 008: tray, close-hides, and the prevented ExitRequested.
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the desktop shell failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_endpoint(path: &Path, uuid: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"port":51515,"credential":"secret","daemon_uuid":"{uuid}","pid":7,"started_at_ns":1}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn reads_parses_and_rejects() {
        let dir = std::env::temp_dir().join(format!("mrdesk-{}", std::process::id()));
        let path = dir.join("runtime").join("endpoint.json");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(read_endpoint_at(&path).unwrap().is_none());

        write_endpoint(&path, "uuid-a");
        let endpoint = read_endpoint_at(&path).unwrap().unwrap();
        assert_eq!(endpoint.port, 51515);
        assert_eq!(endpoint.bearer, "secret");
        assert_eq!(endpoint.daemon_uuid, "uuid-a");

        std::fs::write(&path, b"{ not json").unwrap();
        let err = read_endpoint_at(&path).err().unwrap();
        assert!(err.starts_with("ENDPOINT_MALFORMED: "), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake sidecar that writes `source` over the endpoint file after a beat.
    #[cfg(unix)]
    fn writer(source: &Path, target: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("/bin/sh"),
            vec![
                "-c".into(),
                format!("sleep 0.3; cp {} {}", source.display(), target.display()),
            ],
        )
    }
    #[cfg(windows)]
    fn writer(source: &Path, target: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from(std::env::var("COMSPEC").unwrap()),
            vec![
                "/C".into(),
                format!(
                    "ping -n 2 127.0.0.1 >NUL & copy /Y \"{}\" \"{}\"",
                    source.display(),
                    target.display()
                ),
            ],
        )
    }

    #[cfg(unix)]
    fn failer() -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("/bin/sh"),
            vec![
                "-c".into(),
                "echo boom-from-the-sidecar 1>&2; exit 1".into(),
            ],
        )
    }
    #[cfg(windows)]
    fn failer() -> (PathBuf, Vec<String>) {
        (
            PathBuf::from(std::env::var("COMSPEC").unwrap()),
            vec![
                "/C".into(),
                "echo boom-from-the-sidecar 1>&2 & exit 1".into(),
            ],
        )
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mrdesk-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("runtime")).unwrap();
        dir
    }

    #[test]
    fn start_daemon_waits_for_a_new_uuid() {
        let root = scratch("start");
        let path = endpoint_path(&root);
        write_endpoint(&path, "uuid-old");
        let staged = root.join("new.json");
        write_endpoint(&staged, "uuid-new");

        let (program, args) = writer(&staged, &path);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let endpoint = spawn_and_wait(
            &program,
            &args,
            &root,
            Some("uuid-old"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(endpoint.daemon_uuid, "uuid-new");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn start_daemon_reports_the_stderr_tail() {
        let root = scratch("fail");
        let (program, args) = failer();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let err = spawn_and_wait(&program, &args, &root, None, Duration::from_secs(10))
            .err()
            .unwrap();
        assert!(err.starts_with("DAEMON_START_FAILED: "), "{err}");
        assert!(err.contains("boom-from-the-sidecar"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tray_label() {
        assert_eq!(pending_label(0), "0 pending approvals");
        assert_eq!(pending_label(3), "3 pending approvals");
    }
}
