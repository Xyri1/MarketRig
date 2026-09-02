//! The acceptance harness both modes share.
//!
//! Contract: root `sdd/SPEC.md` §17 (the rules the gate and the experiment share)
//! and `sdd/features/r0-workspace-desk-identity/SPEC.md` §10, extended by
//! `sdd/features/r1-equity-paper-trading/SPEC.md` §10, per D75, D77, R0-7, R1-9.
//!
//! The harness drives **public surfaces only**: the real binaries, `marketrig`'s
//! machine output, the loopback API, the desk's MCP surface through the harness's
//! own MCP client, workspace files, and read-only SQLite. It never links
//! `marketrigd` or `marketrig` as libraries, which is why every shape it asserts
//! is re-stated in the test targets from the SPEC on purpose.
//!
//! Both modes relocate the whole data root into the run's evidence directory, so
//! the daemon's log, the desk workspaces, and the database land in the bundle by
//! construction and no run touches the per-user root.

pub mod standin;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::OpenFlags;
use serde_json::{Value, json};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `YYYY-MM-DDTHH:MM:SS` in UTC for a Unix second. A schedule is given as text
/// (R2 feature SPEC §2) and both shapes need it — `--at` appends `Z`, `--dtstart`
/// takes it as the naive wall clock — and the harness carries no date library,
/// on purpose: it depends on nothing the daemon depends on.
pub fn utc(unix_s: i64) -> String {
    // Howard Hinnant's civil_from_days, the shortest correct proleptic
    // Gregorian conversion; the era arithmetic is exact for every i64 day.
    let days = unix_s.div_euclid(86_400);
    let seconds = unix_s.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
    )
}

#[test]
fn utc_formats_the_schedule_text() {
    assert_eq!(utc(0), "1970-01-01T00:00:00");
    // A leap day, the last second of a year, and the stand-in feed's own base.
    assert_eq!(utc(1_709_209_845), "2024-02-29T12:30:45");
    assert_eq!(utc(1_798_761_599), "2026-12-31T23:59:59");
    assert_eq!(utc(1_788_206_401), "2026-08-31T20:00:01");
}

#[track_caller]
pub fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// Every wait in either mode is bounded (root SPEC §17: unattended and
/// deterministic; attended and patient).
#[track_caller]
pub fn await_exit(child: &mut Child, bound: Duration) -> ExitStatus {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "process {} did not exit within {bound:?}",
            child.id()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A bounded wait on a condition. A mechanical assertion uses this and fails.
#[track_caller]
pub fn within(bound: Duration, what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + bound;
    loop {
        if check() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within {bound:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The same wait, answering whether it happened instead of failing: the
/// experiment's agent-behavior waits end **inconclusive**, never as a product
/// defect (root SPEC §17). It says where it is, because an operator is watching.
pub fn waited(bound: Duration, what: &str, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    let mut said = Instant::now();
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if said.elapsed() >= Duration::from_secs(30) {
            said = Instant::now();
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            eprintln!("waiting for {what} ({left}s left)");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `runtime/endpoint.json` (R0 feature SPEC §5.1) — all five fields, or this
/// panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub credential: String,
    pub daemon_uuid: String,
    pub pid: u32,
    pub started_at_ns: i64,
}

impl Endpoint {
    pub fn parse(raw: &str) -> Endpoint {
        let value = parse(raw);
        let field = |name: &str| -> Value {
            value
                .get(name)
                .unwrap_or_else(|| panic!("endpoint.json lacks {name}: {value}"))
                .clone()
        };
        Endpoint {
            port: u16::try_from(field("port").as_u64().expect("port")).expect("port fits"),
            credential: field("credential")
                .as_str()
                .expect("credential")
                .to_string(),
            daemon_uuid: field("daemon_uuid")
                .as_str()
                .expect("daemon_uuid")
                .to_string(),
            pid: u32::try_from(field("pid").as_u64().expect("pid")).expect("pid fits"),
            started_at_ns: field("started_at_ns").as_i64().expect("started_at_ns"),
        }
    }
}

/// One `operational_events` row (R0 feature SPEC §3.3), read through read-only
/// SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: String,
    pub desk_id: Option<String>,
    pub payload: Value,
}

/// A running `marketrigd`. Dropping one kills it, so a panicking scenario still
/// leaves no stray daemon; the clean path always goes through [`Harness::stop`].
pub struct Daemon {
    pub child: Child,
    pub endpoint: Endpoint,
    /// `MARKETRIG_TEST_DATA_ROOT`, so a dropped daemon can reap what it left
    /// behind: the app-server it records in `runtime/children.json` and the
    /// setsid-detached terminal children its `agent_processes` rows name.
    pub data_root: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // A clean stop already took them; this is the panicking path's net. On
        // Windows the job object holds the whole tree, so there is nothing to do.
        #[cfg(unix)]
        {
            let kill = |pid: i32, group: bool| unsafe {
                libc::kill(if group { -pid } else { pid }, libc::SIGKILL);
            };
            let children = self
                .data_root
                .join("data")
                .join("runtime")
                .join("children.json");
            if let Ok(text) = fs::read_to_string(&children)
                && let Ok(value) = serde_json::from_str::<Value>(&text)
            {
                for pid in value["children"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["pid"].as_i64())
                {
                    kill(pid as i32, false);
                }
            }
            if let Ok(db) = rusqlite::Connection::open_with_flags(
                self.data_root.join("data").join("marketrig.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) && let Ok(mut statement) =
                db.prepare("SELECT pid FROM agent_processes WHERE ended_at_ns IS NULL")
            {
                let pids: Vec<i64> = statement
                    .query_map([], |row| row.get(0))
                    .map(|rows| rows.flatten().collect())
                    .unwrap_or_default();
                // Each terminal child is its own session leader (`setsid`), so
                // the group carries the runtime and whatever it spawned.
                for pid in pids {
                    kill(pid as i32, true);
                }
            }
        }
    }
}

pub struct Harness {
    /// The evidence directory, which *is* `MARKETRIG_TEST_DATA_ROOT`.
    pub out: PathBuf,
    pub daemond: PathBuf,
    pub cli: PathBuf,
    pub mcp: PathBuf,
    /// The acceptance modes' own trigger script runner (R2 feature SPEC §10.1),
    /// which every code-bearing trigger names as `argv[0]`.
    pub trigger_code: PathBuf,
    /// The stand-in runtime (R3 feature SPEC §9.1), which the gate registers by
    /// explicit path and scripts through [`Harness::script`].
    pub standin: PathBuf,
    observations: File,
    step: u32,
    daemons: u32,
    agent: ureq::Agent,
    /// The stand-in feed seam (R1 feature SPEC §10.1), honored only alongside the
    /// data root; `None` on the attended experiment, which polls real Yahoo.
    quote_url: Option<String>,
    /// Keeps the daemon off the public feed (root SPEC §17). Set in the gate,
    /// cleared by [`Harness::real_feed`].
    no_trading: bool,
    /// The stand-in runtime's script (R3 feature SPEC §9.1). It is one file per
    /// run, set on the daemon's own environment and inherited by every child it
    /// launches, which is how a launch that the daemon spawns with an exact
    /// environment still finds its knobs.
    standin_script: Option<PathBuf>,
}

impl Harness {
    /// A run's evidence directory: `target/acceptance/<prefix>-<stamp>/`, or
    /// `MARKETRIG_ACCEPTANCE_OUT` when it is set.
    pub fn new(prefix: &str) -> Harness {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root")
            .to_path_buf();
        let out = match std::env::var_os("MARKETRIG_ACCEPTANCE_OUT") {
            Some(dir) => PathBuf::from(dir),
            None => workspace
                .join("target")
                .join("acceptance")
                .join(format!("{prefix}-{}", now_secs())),
        };
        fs::create_dir_all(&out).expect("evidence directory");
        let (daemond, cli, mcp, trigger_code, standin) = build(&workspace);
        eprintln!("acceptance evidence: {}", out.display());
        Harness {
            observations: File::create(out.join("observations.jsonl")).expect("observations"),
            out,
            daemond,
            cli,
            mcp,
            trigger_code,
            standin,
            step: 0,
            daemons: 0,
            // Same discipline as the CLI (R0 feature SPEC §8): no proxy, no
            // redirects, short bounded timeouts, statuses are data.
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .timeout_connect(Some(Duration::from_secs(2)))
                    .timeout_global(Some(Duration::from_secs(30)))
                    .max_redirects(0)
                    .proxy(None)
                    .http_status_as_error(false)
                    .build(),
            ),
            quote_url: None,
            no_trading: true,
            standin_script: None,
        }
    }

    /// Writes the stand-in runtime's script (R3 feature SPEC §9.1) and points
    /// every binary this harness starts at it. One path for the whole run: a
    /// launch reads it when it starts, so rewriting it arms the next launch,
    /// and the `<script>.sessions` ledger beside it spans the run's launches.
    pub fn script(&mut self, script: Value) {
        let path = self.out.join("standin-script.json");
        fs::write(&path, script.to_string()).expect("write the stand-in script");
        self.standin_script = Some(path);
    }

    /// Points the next daemon at the gate's stand-in feed (R1 feature SPEC
    /// §10.1). `MARKETRIG_TEST_NO_TRADING` stays set and a stand-in outranks it.
    pub fn standin_feed(&mut self, base: &str) {
        self.quote_url = Some(base.to_owned());
    }

    /// The attended experiment's feed: real Yahoo, so neither seam is set — only
    /// the data root, which both modes always relocate (root SPEC §17).
    pub fn real_feed(&mut self) {
        self.quote_url = None;
        self.no_trading = false;
    }

    /// One greppable JSON line per step; the harness deletes nothing.
    pub fn note(&mut self, scenario: &str, note: &str, data: Value) {
        self.record(scenario, "OK", note, data);
    }

    /// An aspect that waits on the agent and did not happen: evidence, not a
    /// product defect (root SPEC §17).
    pub fn inconclusive(&mut self, scenario: &str, note: &str, data: Value) {
        eprintln!("INCONCLUSIVE {scenario}: {note}");
        self.record(scenario, "INCONCLUSIVE", note, data);
    }

    fn record(&mut self, scenario: &str, outcome: &str, note: &str, data: Value) {
        self.step += 1;
        let line = json!({
            "scenario": scenario,
            "step": self.step,
            "ok": outcome == "OK",
            "outcome": outcome,
            "note": note,
            "data": data,
        });
        writeln!(self.observations, "{line}").expect("observation");
    }

    /// Anything else worth keeping in the bundle, by name.
    pub fn write_evidence(&self, name: &str, contents: &str) {
        fs::write(self.out.join(name), contents).expect("evidence file");
    }

    pub fn endpoint_path(&self) -> PathBuf {
        self.out.join("data").join("runtime").join("endpoint.json")
    }

    pub fn children_path(&self) -> PathBuf {
        self.out.join("data").join("runtime").join("children.json")
    }

    pub fn workspace(&self, name: &str) -> PathBuf {
        self.out.join("desks").join(name)
    }

    /// Never invoke a binary without the test seam (AGENTS.md).
    pub fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command.env("MARKETRIG_TEST_DATA_ROOT", &self.out);
        if self.no_trading {
            command.env("MARKETRIG_TEST_NO_TRADING", "1");
        } else {
            command.env_remove("MARKETRIG_TEST_NO_TRADING");
        }
        match &self.quote_url {
            Some(url) => command.env("MARKETRIG_TEST_QUOTE_URL", url),
            None => command.env_remove("MARKETRIG_TEST_QUOTE_URL"),
        };
        match &self.standin_script {
            Some(path) => command.env("MARKETRIG_STANDIN_SCRIPT", path),
            None => command.env_remove("MARKETRIG_STANDIN_SCRIPT"),
        };
        command
    }

    pub fn read_endpoint(&self) -> Option<Endpoint> {
        fs::read_to_string(self.endpoint_path())
            .ok()
            .map(|raw| Endpoint::parse(&raw))
    }

    /// `None` when the daemon is unreachable; otherwise the status and body.
    pub fn request(
        &self,
        method: &str,
        port: u16,
        path: &str,
        credential: &str,
        body: Option<&str>,
    ) -> Option<(u16, String)> {
        let url = format!("http://127.0.0.1:{port}{path}");
        let bearer = format!("Bearer {credential}");
        let sent = match method {
            "GET" => self
                .agent
                .get(url.as_str())
                .header("Authorization", bearer)
                .call(),
            _ => {
                let request = self
                    .agent
                    .post(url.as_str())
                    .header("Authorization", bearer);
                match body {
                    Some(body) => request
                        .header("content-type", "application/json")
                        .send(body),
                    None => request.send_empty(),
                }
            }
        };
        let mut response = sent.ok()?;
        let status = response.status().as_u16();
        Some((status, response.body_mut().read_to_string().ok()?))
    }

    /// One authenticated call on the loopback API: the status and the parsed
    /// body, which is either the resource or the one error envelope.
    #[track_caller]
    pub fn call(
        &self,
        endpoint: &Endpoint,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, Value) {
        let (status, text) = self
            .request(method, endpoint.port, path, &endpoint.credential, body)
            .unwrap_or_else(|| panic!("the daemon did not answer {method} {path}"));
        (status, parse(&text))
    }

    /// [`Harness::call`], recorded in the bundle. Polling loops use `call`, so a
    /// bounded wait does not bury the evidence in its own retries.
    #[track_caller]
    pub fn api(
        &mut self,
        scenario: &str,
        endpoint: &Endpoint,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, Value) {
        let (status, value) = self.call(endpoint, method, path, body);
        self.note(
            scenario,
            "loopback API",
            json!({ "method": method, "path": path, "request": body, "status": status, "body": value }),
        );
        (status, value)
    }

    /// R0 feature SPEC §5.2: authenticated health plus daemon-UUID equality.
    pub fn verify(&self, endpoint: &Endpoint) -> bool {
        match self.request("GET", endpoint.port, "/health", &endpoint.credential, None) {
            Some((200, body)) => parse(&body)["daemon_uuid"] == endpoint.daemon_uuid.as_str(),
            _ => false,
        }
    }

    pub fn spawn(&mut self, scenario: &str) -> Daemon {
        self.daemons += 1;
        let stderr = self.out.join(format!("marketrigd-{}.stderr", self.daemons));
        let mut child = self
            .command(&self.daemond.clone())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&stderr).expect("daemon stderr"))
            .spawn()
            .expect("spawn marketrigd");

        let deadline = Instant::now() + Duration::from_secs(10);
        let endpoint = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                panic!(
                    "marketrigd exited {status} before becoming ready; stderr:\n{}",
                    fs::read_to_string(&stderr).unwrap_or_default()
                );
            }
            if let Some(endpoint) = self.read_endpoint()
                && self.verify(&endpoint)
            {
                break endpoint;
            }
            assert!(
                Instant::now() < deadline,
                "marketrigd was not verifiable within 10s; stderr:\n{}",
                fs::read_to_string(&stderr).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        self.note(
            scenario,
            "daemon started and verified",
            json!({
                "daemon_uuid": endpoint.daemon_uuid,
                "port": endpoint.port,
                "pid": endpoint.pid,
                "quote_url": self.quote_url,
                "no_trading": self.no_trading,
            }),
        );
        Daemon {
            child,
            endpoint,
            data_root: self.out.clone(),
        }
    }

    /// The clean stop (R0 feature SPEC §4.2): `POST /quit`, bounded exit 0,
    /// pointer removed.
    pub fn stop(&mut self, scenario: &str, mut daemon: Daemon) {
        let endpoint = daemon.endpoint.clone();
        let (status, body) = self
            .request("POST", endpoint.port, "/quit", &endpoint.credential, None)
            .expect("/quit answered");
        assert_eq!(status, 202, "{body}");
        assert_eq!(parse(&body), json!({}));
        // §4.2 bounds shutdown at 5 seconds; the slack is process teardown.
        let exit = await_exit(&mut daemon.child, Duration::from_secs(8));
        assert_eq!(exit.code(), Some(0), "clean shutdown exits 0");
        assert!(
            !self.endpoint_path().exists(),
            "a clean stop removes endpoint.json"
        );
        self.note(
            scenario,
            "daemon stopped cleanly",
            json!({ "daemon_uuid": endpoint.daemon_uuid }),
        );
    }

    pub fn kill(&mut self, scenario: &str, mut daemon: Daemon) {
        let endpoint = daemon.endpoint.clone();
        daemon.child.kill().expect("kill marketrigd");
        let exit = await_exit(&mut daemon.child, Duration::from_secs(5));
        self.note(
            scenario,
            "daemon hard-killed",
            json!({ "daemon_uuid": endpoint.daemon_uuid, "exit": format!("{exit}") }),
        );
    }

    /// Bounded: a killed daemon's pointer must stop verifying (R0 §5.2, §6).
    pub fn await_unverifiable(&self, endpoint: &Endpoint) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.verify(endpoint) {
            assert!(
                Instant::now() < deadline,
                "the killed daemon's endpoint still verifies"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Bounded by construction: the CLI's own global HTTP timeout is 10s (§8).
    pub fn cli(&self, args: &[&str]) -> (i32, String, String) {
        let output = self
            .command(&self.cli.clone())
            .args(args)
            .output()
            .expect("run marketrig");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// `marketrig --json …`: the exit code and the JSON on standard output — the
    /// resource on success, the §6 envelope on a daemon error.
    pub fn cli_json(&mut self, scenario: &str, args: &[&str]) -> (i32, Value) {
        let (exit, stdout, stderr) = self.cli(args);
        let body = parse(stdout.trim());
        self.note(
            scenario,
            "marketrig --json",
            json!({ "args": args, "exit": exit, "body": body, "stderr": stderr }),
        );
        (exit, body)
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.out.join("data").join("marketrig.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open the database read-only")
    }

    /// One read-only scalar, the shape most durable assertions need.
    pub fn scalar<T: rusqlite::types::FromSql>(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> T {
        self.db()
            .query_row(sql, params, |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// One read-only single-column listing, in the query's own order.
    pub fn column(&self, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Vec<String> {
        let db = self.db();
        let mut statement = db.prepare(sql).expect("prepare");
        let rows = statement
            .query_map(params, |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<String>>>();
        rows.unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    pub fn events(&self) -> Vec<Event> {
        let db = self.db();
        let mut statement = db
            .prepare(
                "SELECT kind, desk_id, payload FROM operational_events \
                 ORDER BY occurred_at_ns, id",
            )
            .expect("prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(Event {
                    kind: row.get(0)?,
                    desk_id: row.get(1)?,
                    payload: parse(&row.get::<_, String>(2)?),
                })
            })
            .expect("query events");
        rows.collect::<rusqlite::Result<Vec<_>>>().expect("events")
    }

    pub fn event_kinds(&self) -> Vec<String> {
        self.events().into_iter().map(|e| e.kind).collect()
    }

    pub fn recoveries(&self) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|e| e.kind == "RECOVERY")
            .map(|e| e.payload)
            .collect()
    }

    pub fn desk_rows(&self) -> Vec<Value> {
        let db = self.db();
        let mut statement = db
            .prepare(
                "SELECT id, name, state, workspace_path, created_at_ns, ready_at_ns, \
                 failure_code FROM desks ORDER BY created_at_ns, id",
            )
            .expect("prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "state": row.get::<_, String>(2)?,
                    "workspace_path": row.get::<_, String>(3)?,
                    "created_at_ns": row.get::<_, i64>(4)?,
                    "ready_at_ns": row.get::<_, Option<i64>>(5)?,
                    "failure_code": row.get::<_, Option<String>>(6)?,
                }))
            })
            .expect("query desks");
        rows.collect::<rusqlite::Result<Vec<_>>>().expect("desks")
    }

    pub fn kinds_for(&self, desk_id: &str) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|e| e.desk_id.as_deref() == Some(desk_id))
            .map(|e| e.kind)
            .collect()
    }
}

/// The real binaries, located through Cargo's own artifact messages so the
/// harness is correct under `CARGO_TARGET_DIR` and on Windows. `trigger-code`
/// is built with them and lands beside them, which is how it finds the adapter
/// (R2 feature SPEC §10.1).
fn build(workspace: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args([
            "build",
            "-p",
            "marketrigd",
            "-p",
            "marketrig",
            "-p",
            "marketrig-mcp",
            "-p",
            "marketrig-acceptance",
            "--bins",
            "--message-format=json",
        ])
        .stderr(Stdio::inherit())
        .output()
        .expect("run cargo build");
    assert!(
        output.status.success(),
        "cargo build failed: {}",
        output.status
    );

    let mut daemond = None;
    let mut cli = None;
    let mut mcp = None;
    let mut trigger_code = None;
    let mut standin = None;
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let (Some(name), Some(executable)) = (
            message["target"]["name"].as_str(),
            message["executable"].as_str(),
        ) else {
            continue;
        };
        match name {
            "marketrigd" => daemond = Some(PathBuf::from(executable)),
            "marketrig" => cli = Some(PathBuf::from(executable)),
            "marketrig-mcp" => mcp = Some(PathBuf::from(executable)),
            "trigger-code" => trigger_code = Some(PathBuf::from(executable)),
            "runtime-standin" => standin = Some(PathBuf::from(executable)),
            _ => {}
        }
    }
    (
        daemond.expect("marketrigd executable"),
        cli.expect("marketrig executable"),
        mcp.expect("marketrig-mcp executable"),
        trigger_code.expect("trigger-code executable"),
        standin.expect("runtime-standin executable"),
    )
}
