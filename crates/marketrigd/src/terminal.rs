//! The one terminal manager: PTY creation, attachment generations, the
//! reconnect ring, resize, containment, and shutdown.
//!
//! Contract: `sdd/features/r3-runtime-delivery/SPEC.md` §3 (per R3-2), root
//! `sdd/SPEC.md` §6.5. The manager owns no `agent_processes` row: the adapters
//! do (`session.rs`), and the child's exit reaches them over [`Manager::new`]'s
//! exit channel.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

/// The reconnect ring (§3): the newest 256 KiB of the child's output, replayed
/// to a new attachment before live bytes and never persisted.
const RING: usize = 256 * 1024;
/// A slow consumer is dropped once this much of its output is unsent (§3).
const SEND_BUFFER: usize = 1024 * 1024;
/// Shutdown drains for at most this long before terminating the tree (§3).
const DRAIN: Duration = Duration::from_secs(2);
/// How long a terminated tree gets to reach EOF before the reader is joined
/// anyway — the join itself would otherwise hang on a stuck read.
const REAP: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 8 * 1024;

/// What a spawn needs; everything else about the command is the adapter's.
#[derive(Debug, Clone)]
pub struct Spawn {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Applied over the daemon's own environment.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// One frame towards the current attachment.
#[derive(Debug, Clone)]
pub enum Frame {
    Bytes(Vec<u8>),
    /// `{"exited":{"reason","code"}}`, then close `1000`.
    Exited {
        reason: &'static str,
        code: Option<i64>,
    },
    /// A newer attachment took over; close `4001`.
    Superseded,
}

/// The child ended. Whoever owns the desk's `agent_processes` row closes it.
#[derive(Debug, Clone)]
pub struct TerminalExit {
    pub desk_id: String,
    /// The child that ended; a later terminal on the same desk is not this one.
    pub pid: u32,
    /// `EXITED` for the child's own exit, `INTERRUPTED` when MarketRig ended it.
    pub reason: &'static str,
    pub code: Option<i64>,
}

/// One attachment generation: the ring replay, then live frames.
pub struct Attachment {
    pub generation: u64,
    pub replay: Vec<u8>,
    pub frames: mpsc::UnboundedReceiver<Frame>,
    buffered: Arc<AtomicUsize>,
}

impl Attachment {
    /// The consumer reports every byte it has actually sent, which is what
    /// keeps the slow-consumer budget honest.
    pub fn consumed(&self, bytes: usize) {
        self.buffered.fetch_sub(bytes, Ordering::Relaxed);
    }
}

/// Ring and current attachment under one lock, so a replay can never miss or
/// duplicate a byte that arrives while an attachment is being installed.
struct Sink {
    ring: VecDeque<u8>,
    generation: u64,
    sender: Option<mpsc::UnboundedSender<Frame>>,
    buffered: Arc<AtomicUsize>,
}

impl Sink {
    fn push(&mut self, bytes: &[u8]) {
        self.ring.extend(bytes);
        while self.ring.len() > RING {
            let excess = self.ring.len() - RING;
            self.ring.drain(..excess);
        }
        self.send(Frame::Bytes(bytes.to_vec()), bytes.len());
    }

    fn send(&mut self, frame: Frame, cost: usize) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        if sender.send(frame).is_err() {
            self.sender = None;
            return;
        }
        if self.buffered.fetch_add(cost, Ordering::Relaxed) + cost > SEND_BUFFER {
            // §3: a client that never reads must not block the child.
            self.sender = None;
        }
    }
}

struct Terminal {
    master: Mutex<Box<dyn MasterPty + Send>>,
    size: Mutex<PtySize>,
    /// The bounded writer channel; dropping it stops input and EOFs the slave.
    input: Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
    sink: Arc<Mutex<Sink>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    interrupting: Arc<AtomicBool>,
    /// The session leader, and so the process group `killpg` ends on Unix and
    /// the process the Windows fallback terminates.
    pid: u32,
    /// The Job Object the child was assigned to at spawn, `0` when the
    /// assignment failed. Closing it kills the tree (`KILL_ON_JOB_CLOSE`).
    #[cfg(windows)]
    job: usize,
}

/// The daemon's single terminal manager, one live terminal per desk.
pub struct Manager {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    exits: mpsc::UnboundedSender<TerminalExit>,
}

impl Manager {
    /// The manager and the exit stream its terminals report on.
    pub fn new() -> (Arc<Manager>, mpsc::UnboundedReceiver<TerminalExit>) {
        let (exits, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Manager {
                terminals: Mutex::new(HashMap::new()),
                exits,
            }),
            rx,
        )
    }

    /// Opens a PTY, spawns the command in it, and starts the desk's terminal at
    /// generation 0 with an empty ring. Any existing terminal for the desk is
    /// shut down first. Answers the child's pid.
    pub fn spawn(&self, desk_id: &str, spawn: Spawn) -> anyhow::Result<u32> {
        self.shutdown(desk_id);
        let size = PtySize {
            rows: spawn.rows,
            cols: spawn.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system().openpty(size)?;
        let mut command =
            CommandBuilder::from_argv(spawn.argv.iter().map(std::ffi::OsString::from).collect());
        command.cwd(&spawn.cwd);
        // §4.2/§5.1: exactly the adapter's variables reach the runtime. An
        // inherited `CLAUDE_CODE_CHILD_SESSION` turns Claude's transcript off,
        // which makes every later `--resume` "no conversation found". The
        // daemon's own `MARKETRIG_*` variables ride along: root §17's test
        // seam is how the acceptance runtimes are pointed at their scratch.
        command.env_clear();
        for (key, value) in std::env::vars() {
            if key.starts_with("MARKETRIG_") {
                command.env(key, value);
            }
        }
        for (key, value) in &spawn.env {
            command.env(key, value);
        }
        let mut child = pair.slave.spawn_command(command)?;
        // The slave fd must go, or the reader never sees EOF.
        drop(pair.slave);
        let pid = child.process_id().unwrap_or_default();
        // The R2 containment mechanics, applied to a child `portable-pty`
        // spawned: on Unix its own `setsid` already made the process group the
        // tree, on Windows the job has to be assigned here so that everything
        // the session starts later is inside it (slice 004 §2).
        #[cfg(windows)]
        let job = contain(pid);
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;

        let sink = Arc::new(Mutex::new(Sink {
            ring: VecDeque::new(),
            generation: 0,
            sender: None,
            buffered: Arc::new(AtomicUsize::new(0)),
        }));
        let (input_tx, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        let interrupting = Arc::new(AtomicBool::new(false));
        let reader_thread = {
            let sink = sink.clone();
            let interrupting = interrupting.clone();
            let exits = self.exits.clone();
            let desk_id = desk_id.to_string();
            std::thread::spawn(move || {
                let mut buf = [0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => sink.lock().expect("sink").push(&buf[..n]),
                    }
                }
                let code = child.wait().ok().map(|s| i64::from(s.exit_code()));
                let reason = if interrupting.load(Ordering::SeqCst) {
                    "INTERRUPTED"
                } else {
                    "EXITED"
                };
                sink.lock()
                    .expect("sink")
                    .send(Frame::Exited { reason, code }, 0);
                let _ = exits.send(TerminalExit {
                    desk_id,
                    pid,
                    reason,
                    code,
                });
            })
        };

        self.terminals.lock().expect("terminals").insert(
            desk_id.to_string(),
            Arc::new(Terminal {
                master: Mutex::new(pair.master),
                size: Mutex::new(size),
                input: Mutex::new(Some(input_tx)),
                sink,
                reader: Mutex::new(Some(reader_thread)),
                interrupting,
                pid,
                #[cfg(windows)]
                job,
            }),
        );
        Ok(pid)
    }

    fn get(&self, desk_id: &str) -> Option<Arc<Terminal>> {
        self.terminals
            .lock()
            .expect("terminals")
            .get(desk_id)
            .cloned()
    }

    /// The next attachment generation: the previous one is closed `4001` and
    /// its input stops counting, and this one gets the ring then live bytes.
    pub fn attach(&self, desk_id: &str) -> Option<Attachment> {
        let terminal = self.get(desk_id)?;
        let mut sink = terminal.sink.lock().expect("sink");
        sink.send(Frame::Superseded, 0);
        sink.sender = None;
        sink.generation += 1;
        let (tx, rx) = mpsc::unbounded_channel();
        let buffered = Arc::new(AtomicUsize::new(0));
        sink.sender = Some(tx);
        sink.buffered = buffered.clone();
        Some(Attachment {
            generation: sink.generation,
            replay: sink.ring.iter().copied().collect(),
            frames: rx,
            buffered,
        })
    }

    fn current(&self, desk_id: &str, generation: u64) -> Option<Arc<Terminal>> {
        let terminal = self.get(desk_id)?;
        let live = terminal.sink.lock().expect("sink").generation;
        (live == generation).then_some(terminal)
    }

    /// Input from the current generation; anything else is dropped silently,
    /// as is input that outruns the bounded writer channel.
    pub fn write(&self, desk_id: &str, generation: u64, bytes: Vec<u8>) {
        let Some(terminal) = self.current(desk_id, generation) else {
            return;
        };
        let input = terminal.input.lock().expect("input");
        if let Some(sender) = input.as_ref() {
            let _ = sender.try_send(bytes);
        }
    }

    /// Resize from the current generation. Requests coalesce to the newest
    /// dimensions: the size lock serializes them and the last one is what the
    /// kernel is left holding.
    pub fn resize(&self, desk_id: &str, generation: u64, cols: u16, rows: u16) {
        let Some(terminal) = self.current(desk_id, generation) else {
            return;
        };
        let mut size = terminal.size.lock().expect("size");
        *size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let _ = terminal.master.lock().expect("master").resize(*size);
    }

    /// The desk's terminal size, for callers that need to report it.
    pub fn size(&self, desk_id: &str) -> Option<(u16, u16)> {
        let terminal = self.get(desk_id)?;
        let size = *terminal.size.lock().expect("size");
        Some((size.cols, size.rows))
    }

    /// Stops input, drains for at most 2 s, terminates the contained process
    /// tree, and joins the reader (§3). Blocking; the async paths hop through
    /// `spawn_blocking`.
    pub fn shutdown(&self, desk_id: &str) {
        let Some(terminal) = self.terminals.lock().expect("terminals").remove(desk_id) else {
            return;
        };
        terminal.interrupting.store(true, Ordering::SeqCst);
        drop(terminal.input.lock().expect("input").take());
        let reader = terminal.reader.lock().expect("reader").take();
        let Some(reader) = reader else { return };
        if !wait_for(&reader, DRAIN) {
            terminal.terminate_tree();
            wait_for(&reader, REAP);
        }
        let _ = reader.join();
    }

    /// Every live terminal, for the Quit path (root §4.2).
    /// `shutdown` only when the desk's current terminal is still the child
    /// `pid` names; a terminal started since (another runtime after a switch)
    /// is left alone.
    pub fn shutdown_pid(&self, desk_id: &str, pid: u32) {
        let current = self
            .terminals
            .lock()
            .expect("terminals")
            .get(desk_id)
            .map(|t| t.pid);
        if current == Some(pid) {
            self.shutdown(desk_id);
        }
    }

    pub fn shutdown_all(&self) {
        let desks: Vec<String> = self
            .terminals
            .lock()
            .expect("terminals")
            .keys()
            .cloned()
            .collect();
        for desk_id in desks {
            self.shutdown(&desk_id);
        }
    }
}

/// ponytail: polling `is_finished` rather than a condition variable, because
/// the only waiters are shutdown paths with a two-second budget.
fn wait_for(handle: &std::thread::JoinHandle<()>, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if handle.is_finished() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.is_finished()
}

impl Terminal {
    /// `portable-pty` already put the child in its own session with `setsid`,
    /// so the process group is the tree (`exec::spawn`'s `ProcessSession`).
    #[cfg(unix)]
    fn terminate_tree(&self) {
        let pid = self.pid as i32;
        if pid <= 0 {
            return;
        }
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(200));
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }

    /// Closing the job kills everything in it (`exec::spawn`'s `JobObject`,
    /// through `KILL_ON_JOB_CLOSE`). Without a job — the assignment failed —
    /// only the leader can be reached.
    #[cfg(windows)]
    fn terminate_tree(&self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
        unsafe {
            if self.job != 0 {
                let _ = CloseHandle(handle(self.job));
                return;
            }
            if let Ok(process) = OpenProcess(PROCESS_TERMINATE, false, self.pid) {
                let _ = TerminateProcess(process, 1);
                let _ = CloseHandle(process);
            }
        }
    }
}

#[cfg(windows)]
fn handle(raw: usize) -> windows::Win32::Foundation::HANDLE {
    windows::Win32::Foundation::HANDLE(raw as *mut core::ffi::c_void)
}

/// Puts the freshly spawned child in its own kill-on-close Job Object and
/// answers the job handle, or `0` when it could not be assigned — in which case
/// shutdown falls back to terminating the leader alone.
#[cfg(windows)]
fn contain(pid: u32) -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) else {
            return 0;
        };
        let job = match CreateJobObjectW(None, windows::core::PCWSTR::null()) {
            Ok(job) => job,
            Err(_) => {
                let _ = CloseHandle(process);
                return 0;
            }
        };
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        let contained = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_ok()
            && AssignProcessToJobObject(job, process).is_ok();
        let _ = CloseHandle(process);
        if contained {
            job.0 as usize
        } else {
            let _ = CloseHandle(job);
            0
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> (Arc<Manager>, mpsc::UnboundedReceiver<TerminalExit>) {
        Manager::new()
    }

    #[cfg(unix)]
    fn echo(text: &str) -> Spawn {
        sh(&format!("printf %s {text}"))
    }

    #[cfg(unix)]
    fn sh(script: &str) -> Spawn {
        Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }

    /// Reads frames until the predicate is satisfied or the budget runs out.
    fn drain(attachment: &mut Attachment, budget: Duration) -> Vec<Frame> {
        let deadline = Instant::now() + budget;
        let mut frames = Vec::new();
        while Instant::now() < deadline {
            match attachment.frames.try_recv() {
                Ok(frame) => {
                    if let Frame::Bytes(b) = &frame {
                        attachment.consumed(b.len());
                    }
                    frames.push(frame);
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        frames
    }

    fn bytes(frames: &[Frame]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|f| match f {
                Frame::Bytes(b) => b.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn spawn_attach_and_supersede() {
        let (manager, mut exits) = manager();
        manager
            .spawn("desk", sh("printf hello; sleep 5"))
            .expect("spawn");
        let mut first = manager.attach("desk").expect("attach");
        assert_eq!(first.generation, 1);
        let seen = drain(&mut first, Duration::from_millis(600));
        assert!(String::from_utf8_lossy(&bytes(&seen)).contains("hello"));

        let mut second = manager.attach("desk").expect("attach");
        assert_eq!(second.generation, 2);
        // The first is closed 4001 and its input no longer counts.
        let closed = drain(&mut first, Duration::from_millis(200));
        assert!(matches!(closed.first(), Some(Frame::Superseded)));
        // The ring is replayed to the new generation before live bytes.
        assert!(String::from_utf8_lossy(&second.replay).contains("hello"));
        manager.write("desk", first.generation, b"ignored\n".to_vec());

        manager.shutdown("desk");
        let exit = exits.try_recv().expect("exit notification");
        assert_eq!(exit.desk_id, "desk");
        assert_eq!(exit.reason, "INTERRUPTED");
        assert!(matches!(
            drain(&mut second, Duration::from_millis(200)).last(),
            Some(Frame::Exited { .. })
        ));
    }

    #[test]
    #[cfg(unix)]
    fn ring_keeps_exactly_the_newest_256_kib() {
        let (manager, _exits) = manager();
        // 512 KiB of output, no newline translation to worry about: `dd` from
        // /dev/zero would carry NULs, so use a repeating printable byte.
        manager
            .spawn(
                "desk",
                sh("i=0; while [ $i -lt 512 ]; do printf 'a%.0s' $(seq 1 1024); i=$((i+1)); done"),
            )
            .expect("spawn");
        let mut attachment = manager.attach("desk").expect("attach");
        // Let the child finish and the reader reach EOF.
        let frames = drain(&mut attachment, Duration::from_secs(5));
        assert!(matches!(frames.last(), Some(Frame::Exited { .. })));
        let later = manager.attach("desk").expect("attach");
        assert_eq!(later.replay.len(), RING);
        assert!(later.replay.iter().all(|b| *b == b'a'));
        manager.shutdown("desk");
    }

    #[test]
    #[cfg(unix)]
    fn resize_coalesces_and_ignores_a_stale_generation() {
        let (manager, _exits) = manager();
        manager.spawn("desk", sh("sleep 5")).expect("spawn");
        let stale = manager.attach("desk").expect("attach");
        let current = manager.attach("desk").expect("attach");
        manager.resize("desk", current.generation, 100, 40);
        manager.resize("desk", current.generation, 120, 50);
        manager.resize("desk", stale.generation, 20, 5);
        assert_eq!(manager.size("desk"), Some((120, 50)));
        manager.shutdown("desk");
    }

    #[test]
    #[cfg(unix)]
    fn a_slow_consumer_is_dropped_and_the_ring_survives() {
        let (manager, _exits) = manager();
        manager
            .spawn(
                "desk",
                sh("i=0; while [ $i -lt 2048 ]; do printf 'b%.0s' $(seq 1 1024); i=$((i+1)); done"),
            )
            .expect("spawn");
        let attachment = manager.attach("desk").expect("attach");
        // Never reading: the sender is dropped once 1 MiB is outstanding.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !attachment.frames.is_closed() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            attachment.frames.is_closed(),
            "slow consumer was not dropped"
        );
        let later = manager.attach("desk").expect("attach");
        assert_eq!(later.replay.len(), RING);
        manager.shutdown("desk");
    }

    #[test]
    #[cfg(unix)]
    fn shutdown_drains_then_terminates_the_tree() {
        let (manager, mut exits) = manager();
        // The grandchild outlives its parent's `wait` only if the group is
        // killed; `pgrep -g` proves it is gone.
        manager
            .spawn("desk", sh("printf ready; sleep 30 & wait"))
            .expect("spawn");
        let mut attachment = manager.attach("desk").expect("attach");
        let seen = drain(&mut attachment, Duration::from_millis(800));
        assert!(String::from_utf8_lossy(&bytes(&seen)).contains("ready"));
        let group = manager.get("desk").expect("terminal").pid;

        let began = Instant::now();
        manager.shutdown("desk");
        assert!(began.elapsed() < Duration::from_secs(6));
        assert_eq!(exits.try_recv().expect("exit").reason, "INTERRUPTED");
        let alive = std::process::Command::new("pgrep")
            .arg("-g")
            .arg(group.to_string())
            .output()
            .expect("pgrep");
        assert!(
            String::from_utf8_lossy(&alive.stdout).trim().is_empty(),
            "the process group survived shutdown"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_child_that_exits_on_its_own_reports_exited() {
        let (manager, mut exits) = manager();
        manager.spawn("desk", echo("bye")).expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        let exit = loop {
            if let Ok(exit) = exits.try_recv() {
                break exit;
            }
            assert!(Instant::now() < deadline, "no exit notification");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(exit.reason, "EXITED");
        assert_eq!(exit.code, Some(0));
        manager.shutdown("desk");
    }
}
