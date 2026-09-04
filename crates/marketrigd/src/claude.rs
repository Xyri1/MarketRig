//! The Claude Code adapter: launch files, the session command, and delivery
//! over the development channel.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §5 (per R3-4). The
//! process itself is the terminal manager's (§3) and every row is the
//! dispatcher's (§6); this module owns the runtime's own mechanics.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use crate::session::{Activation, Adapter, AdapterEvent, AdapterEvents, DeliverOutcome};
use crate::store::Store;
use crate::terminal::{self, Spawn};

/// How long a delivery waits for a bridge that has not connected yet (§5.3).
const CHANNEL_WAIT: Duration = Duration::from_secs(30);
/// The session's PTY size until an attachment resizes it.
const COLS: u16 = 120;
const ROWS: u16 = 40;

// ---------------------------------------------------------------------------
// The channel registry (§5.3)
// ---------------------------------------------------------------------------

/// One text frame down a desk's channel: the rendering plus the meta the bridge
/// republishes as `notifications/claude/channel` (§5.3).
pub fn frame(prompt_id: &str, kind: &str, content: &str) -> String {
    json!({ "content": content, "prompt_id": prompt_id, "kind": kind }).to_string()
}

struct Connection {
    generation: u64,
    frames: mpsc::UnboundedSender<String>,
}

/// The desks' live bridge connections, shared by the adapter and the channel
/// route. It also carries the adapter event sender, because the route — not the
/// adapter — sees a connection arrive and readiness is that connection (§5.3).
#[derive(Default)]
pub struct Channels {
    live: Mutex<HashMap<String, Connection>>,
    /// Desks whose launch is in flight. The bridge can connect before the
    /// process row exists, and that connection is the session's readiness, so
    /// it is served rather than closed `4002` (§5.3, §6.1).
    spawning: Mutex<HashSet<String>>,
    events: Mutex<Option<AdapterEvents>>,
    connected: tokio::sync::Notify,
    generations: AtomicU64,
}

impl Channels {
    /// The dispatcher's event sender, installed once the adapter exists.
    pub fn attach_events(&self, events: AdapterEvents) {
        *self.events.lock().expect("events") = Some(events);
    }

    pub fn events(&self) -> Option<AdapterEvents> {
        self.events.lock().expect("events").clone()
    }

    /// Installs the desk's connection, superseding any previous one — whose
    /// receiver then ends, which is the route's cue to close it `4001`.
    /// Answers the generation and the frame stream, and reports readiness.
    pub fn connect(&self, desk_id: &str) -> (u64, mpsc::UnboundedReceiver<String>) {
        let generation = self.generations.fetch_add(1, Ordering::Relaxed) + 1;
        let (frames, rx) = mpsc::unbounded_channel();
        self.live
            .lock()
            .expect("live")
            .insert(desk_id.to_string(), Connection { generation, frames });
        if let Some(events) = self.events() {
            let _ = events.send(AdapterEvent::Ready {
                desk_id: desk_id.to_string(),
            });
        }
        self.connected.notify_waiters();
        (generation, rx)
    }

    /// Removes the desk's connection if it is still this generation's.
    pub fn disconnect(&self, desk_id: &str, generation: u64) {
        let mut live = self.live.lock().expect("live");
        if live
            .get(desk_id)
            .is_some_and(|c| c.generation == generation)
        {
            live.remove(desk_id);
        }
    }

    /// Writes one frame; `false` when there is no connection or the write
    /// fails, which is `CHANNEL_UNAVAILABLE` either way (§5.3).
    fn write(&self, desk_id: &str, frame: String) -> bool {
        let mut live = self.live.lock().expect("live");
        let Some(connection) = live.get(desk_id) else {
            return false;
        };
        if connection.frames.send(frame).is_err() {
            live.remove(desk_id);
            return false;
        }
        true
    }

    /// Opens and closes the spawn-in-flight window; the dispatcher closes it
    /// through `Adapter::settled` once the process row exists.
    pub fn spawning(&self, desk_id: &str, in_flight: bool) {
        let mut spawning = self.spawning.lock().expect("spawning");
        if in_flight {
            spawning.insert(desk_id.to_string());
        } else {
            spawning.remove(desk_id);
        }
    }

    pub fn is_spawning(&self, desk_id: &str) -> bool {
        self.spawning.lock().expect("spawning").contains(desk_id)
    }

    pub fn is_connected(&self, desk_id: &str) -> bool {
        self.live.lock().expect("live").contains_key(desk_id)
    }
}

// ---------------------------------------------------------------------------
// Launch files (§5.1)
// ---------------------------------------------------------------------------

/// Writes `mcp.json` and, when the CLI is there to run them, `settings.json`
/// into `<launch>/<desk-id>/`, both 0600 (per D69). Answers both paths.
pub fn write_launch_files(
    launch_dir: &Path,
    desk_id: &str,
    mcp_adapter: &Path,
    cli: Option<&Path>,
) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    let dir = launch_dir.join(desk_id);
    std::fs::create_dir_all(&dir)?;

    let mut env = serde_json::Map::new();
    if let Some(root) = std::env::var_os(crate::store::TEST_DATA_ROOT_ENV) {
        env.insert(
            crate::store::TEST_DATA_ROOT_ENV.to_string(),
            json!(root.to_string_lossy()),
        );
    }
    let adapter = mcp_adapter.to_string_lossy();
    let mcp = dir.join("mcp.json");
    write_private(
        &mcp,
        &json!({ "mcpServers": {
            "marketrig": {
                "command": adapter,
                "args": ["--desk", desk_id],
                "env": env,
            },
            "marketrig-channel": {
                "command": adapter,
                "args": ["--desk", desk_id, "--channel"],
                "env": env,
            },
        }})
        .to_string(),
    )?;

    let settings = match cli {
        None => None,
        Some(cli) => {
            let path = dir.join("settings.json");
            let hook = json!([{ "hooks": [{
                "type": "command",
                // Exec form: with `args` present Claude spawns `command`
                // directly, no shell on any platform. The shell form ran
                // under bash on Windows too and a backslashed path collapsed
                // into one word (the Windows E5 cell, 2026-09-04).
                "command": cli.to_string_lossy(),
                "args": ["--desk", desk_id, "session", "hook"],
            }]}]);
            write_private(
                &path,
                &json!({ "hooks": {
                    "SessionStart": hook,
                    "Notification": hook,
                    "Stop": hook,
                }})
                .to_string(),
            )?;
            Some(path)
        }
    };
    Ok((mcp, settings))
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The launch directory goes when the process row closes (§5.1).
pub fn remove_launch_files(launch_dir: &Path, desk_id: &str) {
    let _ = std::fs::remove_dir_all(launch_dir.join(desk_id));
}

/// The binary named `name` beside the running daemon, when it is there.
pub fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// A UUIDv4 for a new session (§5.1) — Claude Code mints v4s and rejects
/// anything else as a `--session-id`.
fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("getrandom");
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .as_hyphenated()
        .to_string()
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

pub struct Claude {
    store: Store,
    launch_dir: PathBuf,
    /// The `PATH` the daemon captured at start (§4.2).
    search_path: String,
    terminals: std::sync::Arc<terminal::Manager>,
    channels: std::sync::Arc<Channels>,
    /// §5.3's thirty seconds; a field only so the checks need not wait them.
    channel_wait: Duration,
}

impl Claude {
    /// The adapter, sharing the daemon's one terminal manager and the channel
    /// registry the API route also holds. `events` is the dispatcher's stream.
    pub fn new(
        store: Store,
        launch_dir: PathBuf,
        search_path: String,
        terminals: std::sync::Arc<terminal::Manager>,
        channels: std::sync::Arc<Channels>,
        events: AdapterEvents,
    ) -> Self {
        channels.attach_events(events);
        Self {
            store,
            launch_dir,
            search_path,
            terminals,
            channels,
            channel_wait: CHANNEL_WAIT,
        }
    }
}

#[async_trait::async_trait]
impl Adapter for Claude {
    async fn spawn(&self, desk_id: &str, resume: Option<&str>) -> Result<Activation, String> {
        let desk = crate::desk::get(&self.store, desk_id).map_err(|e| e.to_string())?;
        let runtime = crate::runtime::get(&self.store, "claude")
            .map_err(|e| e.to_string())?
            .filter(|r| r.state == "AVAILABLE")
            .and_then(|r| r.executable_path)
            .ok_or_else(|| "The claude runtime is not available.".to_string())?;
        let adapter = sibling("marketrig-mcp")
            .ok_or_else(|| "marketrig-mcp is not installed beside marketrigd.".to_string())?;
        let (mcp, settings) = write_launch_files(
            &self.launch_dir,
            &desk.id,
            &adapter,
            sibling("marketrig").as_deref(),
        )
        .map_err(|e| format!("Cannot write the launch files: {e}."))?;

        let session_id = resume.map_or_else(new_session_id, str::to_string);
        let mut argv = vec![
            runtime,
            if resume.is_some() {
                "--resume".to_string()
            } else {
                "--session-id".to_string()
            },
            session_id.clone(),
            "--mcp-config".to_string(),
            mcp.to_string_lossy().to_string(),
        ];
        if let Some(settings) = settings {
            argv.push("--settings".to_string());
            argv.push(settings.to_string_lossy().to_string());
        }
        argv.push("--dangerously-load-development-channels".to_string());
        argv.push("server:marketrig-channel".to_string());

        // §5.3: the bridge may connect before the dispatcher opens the row.
        self.channels.spawning(&desk.id, true);
        let pid = self
            .terminals
            .spawn(
                &desk.id,
                Spawn {
                    argv,
                    cwd: PathBuf::from(&desk.workspace_path),
                    env: crate::session::base_env(&self.search_path, &desk.id),
                    cols: COLS,
                    rows: ROWS,
                },
            )
            .map_err(|e| {
                remove_launch_files(&self.launch_dir, &desk.id);
                format!("Cannot start claude: {e}.")
            })?;
        Ok(Activation {
            pid,
            // Known at spawn on both paths: a new session's id is the daemon's
            // own, a resume's is the pointer it resumed (§5.1).
            native_session_id: Some(session_id),
        })
    }

    async fn deliver(
        &self,
        desk_id: &str,
        prompt_id: &str,
        kind: &str,
        text: &str,
    ) -> DeliverOutcome {
        let frame = frame(prompt_id, kind, text);
        if self.channels.write(desk_id, frame.clone()) {
            return DeliverOutcome::Delivered;
        }
        // §5.3: no connection is worth one wait, not a failure on sight.
        let waited = tokio::time::timeout(self.channel_wait, async {
            while !self.channels.is_connected(desk_id) {
                self.channels.connected.notified().await;
            }
        })
        .await;
        if waited.is_ok() && self.channels.write(desk_id, frame) {
            DeliverOutcome::Delivered
        } else {
            DeliverOutcome::ChannelUnavailable
        }
    }

    async fn interrupt(&self, _desk_id: &str) -> Result<String, (&'static str, String)> {
        // §5.3: nothing is touched, so nothing can be half-done.
        Err((
            "INTERRUPT_UNSUPPORTED",
            "Claude Code has no interrupt MarketRig may use.".to_string(),
        ))
    }

    async fn exit(&self, desk_id: &str) {
        let terminals = self.terminals.clone();
        let desk = desk_id.to_string();
        let _ = tokio::task::spawn_blocking(move || terminals.shutdown(&desk)).await;
        remove_launch_files(&self.launch_dir, desk_id);
    }

    fn settled(&self, desk_id: &str) {
        self.channels.spawning(desk_id, false);
    }

    /// §5.1: the launch files live exactly as long as the process row, whether
    /// it ended by exit, deadline, or Quit.
    fn closed(&self, desk_id: &str) {
        remove_launch_files(&self.launch_dir, desk_id);
    }
}

// ---------------------------------------------------------------------------
// claude (feature SPEC §10 check 4)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_files_are_private_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        let (mcp, settings) = write_launch_files(
            dir.path(),
            "d1",
            Path::new("/bin/marketrig-mcp"),
            Some(Path::new("/bin/marketrig")),
        )
        .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
        assert_eq!(
            value["mcpServers"]["marketrig-channel"]["args"],
            json!(["--desk", "d1", "--channel"])
        );
        let settings_path = settings.expect("the CLI is there, so hooks are configured");
        let hooks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        for event in ["SessionStart", "Notification", "Stop"] {
            let hook = &hooks["hooks"][event][0]["hooks"][0];
            assert_eq!(hook["type"], json!("command"));
            assert_eq!(hook["command"], json!("/bin/marketrig"));
            assert_eq!(hook["args"], json!(["--desk", "d1", "session", "hook"]));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&mcp, &settings_path] {
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "{} is 0600", path.display());
            }
        }

        // Without the CLI beside the daemon the launch carries no hooks.
        let (_, none) =
            write_launch_files(dir.path(), "d2", Path::new("/bin/marketrig-mcp"), None).unwrap();
        assert!(none.is_none());
        assert!(!dir.path().join("d2").join("settings.json").exists());

        remove_launch_files(dir.path(), "d1");
        assert!(!dir.path().join("d1").exists());
        assert!(dir.path().join("d2").exists(), "one desk at a time");

        // §5.1: the row closing takes them, however it closed — a self-exit,
        // the readiness deadline, and Quit never reach `exit`.
        let (_home, claude, _events) = adapter();
        write_launch_files(
            &claude.launch_dir,
            "d3",
            Path::new("/bin/marketrig-mcp"),
            None,
        )
        .unwrap();
        assert!(claude.launch_dir.join("d3").exists());
        claude.closed("d3");
        assert!(!claude.launch_dir.join("d3").exists());
    }

    #[test]
    fn a_new_session_id_is_a_v4() {
        let id = uuid::Uuid::parse_str(&new_session_id()).unwrap();
        assert_eq!(id.get_version_num(), 4);
        assert_ne!(new_session_id(), new_session_id());
    }

    fn adapter() -> (
        tempfile::TempDir,
        Claude,
        mpsc::UnboundedReceiver<AdapterEvent>,
    ) {
        let (dir, store) = crate::store::open_temp();
        let (events, rx) = mpsc::unbounded_channel();
        let channels = std::sync::Arc::new(Channels::default());
        let mut claude = Claude::new(
            store,
            dir.path().join("launch"),
            String::new(),
            crate::terminal::Manager::new().0,
            channels,
            events,
        );
        claude.channel_wait = Duration::from_millis(50);
        (dir, claude, rx)
    }

    #[tokio::test]
    async fn a_connection_is_readiness_and_carries_the_frame() {
        let (_dir, claude, mut events) = adapter();
        let (generation, mut frames) = claude.channels.connect("d1");
        assert!(matches!(
            events.try_recv(),
            Ok(AdapterEvent::Ready { desk_id }) if desk_id == "d1"
        ));

        assert_eq!(
            claude.deliver("d1", "p1", "TRIGGER_RESULT", "hello").await,
            DeliverOutcome::Delivered
        );
        let sent: serde_json::Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
        assert_eq!(
            sent,
            json!({"content": "hello", "prompt_id": "p1", "kind": "TRIGGER_RESULT"})
        );

        // A second connection supersedes the first, whose stream then ends.
        let (newer, _newer_frames) = claude.channels.connect("d1");
        assert!(newer > generation);
        assert!(frames.recv().await.is_none());
        // The stale generation cannot unregister the live connection.
        claude.channels.disconnect("d1", generation);
        assert!(claude.channels.is_connected("d1"));
    }

    #[tokio::test]
    async fn no_connection_and_a_write_error_are_both_channel_unavailable() {
        let (_dir, claude, _events) = adapter();
        // A write error: the bridge's socket task is gone but the registry has
        // not caught up yet.
        let (_generation, frames) = claude.channels.connect("d1");
        drop(frames);
        assert_eq!(
            claude.deliver("d1", "p1", "EVALUATION", "text").await,
            DeliverOutcome::ChannelUnavailable
        );
        assert!(!claude.channels.is_connected("d1"), "the write unregisters");

        // No connection at all: the (shortened) wait runs out.
        assert_eq!(
            claude.deliver("d2", "p2", "EVALUATION", "text").await,
            DeliverOutcome::ChannelUnavailable
        );
    }

    #[tokio::test]
    async fn a_connection_inside_the_wait_still_delivers() {
        let (_dir, claude, _events) = adapter();
        let channels = claude.channels.clone();
        let connected = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            channels.connect("d1")
        });
        let outcome = claude.deliver("d1", "p1", "ORIENTATION", "welcome").await;
        assert_eq!(outcome, DeliverOutcome::Delivered);
        let (_generation, mut frames) = connected.await.unwrap();
        assert!(frames.recv().await.unwrap().contains("welcome"));
    }

    #[tokio::test]
    async fn interrupt_is_refused_before_anything_is_touched() {
        let (_dir, claude, _events) = adapter();
        let (code, _message) = claude.interrupt("d1").await.unwrap_err();
        assert_eq!(code, "INTERRUPT_UNSUPPORTED");
    }
}
