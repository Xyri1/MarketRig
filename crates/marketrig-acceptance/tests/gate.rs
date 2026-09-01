//! The R0 acceptance gate: scenarios G1–G11, in order, in one test.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §10 (the scenario
//! chain and the evidence bundle) and §11 (the gate), per D75 and R0-7. The
//! harness drives public surfaces only — the real binaries, `marketrig --json`,
//! the loopback API, workspace files, and read-only SQLite. It never links
//! `marketrigd` or `marketrig` as libraries, so the §7.6 seed and the §5.1
//! endpoint shape are re-stated here from the SPEC on purpose.
//!
//! State carries across scenarios: the chain is one run against one data root,
//! which is also the evidence directory (§2 relocates all three roots into it).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::OpenFlags;
use serde_json::{Value, json};

/// The R0 `AGENTS.md` seed (SPEC §7.6), with the desk name substituted.
fn agents_seed(name: &str) -> String {
    format!(
        "# {name}\n\nThis desk's constitution. MarketRig seeded it at desk creation and never\nrewrites it; its full content arrives with later MarketRig milestones.\n"
    )
}

/// The MarketRig-owned Claude Code shim (SPEC §7.2), exactly.
const SHIM: &str = "@AGENTS.md\n";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// Every wait in the gate is bounded (root SPEC §17: unattended and deterministic).
#[track_caller]
fn await_exit(child: &mut Child, bound: Duration) -> ExitStatus {
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

/// `runtime/endpoint.json` (SPEC §5.1) — all five fields, or this panics.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    port: u16,
    credential: String,
    daemon_uuid: String,
    pid: u32,
    started_at_ns: i64,
}

impl Endpoint {
    fn parse(raw: &str) -> Endpoint {
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

/// One `operational_events` row (SPEC §3.3), read through read-only SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Event {
    kind: String,
    desk_id: Option<String>,
    payload: Value,
}

/// A running `marketrigd`. Dropping one kills it, so a panicking scenario still
/// leaves no stray daemon; the clean path always goes through [`Gate::stop`].
struct Daemon {
    child: Child,
    endpoint: Endpoint,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Gate {
    /// The evidence directory, which *is* `MARKETRIG_TEST_DATA_ROOT` (SPEC §10).
    out: PathBuf,
    daemond: PathBuf,
    cli: PathBuf,
    observations: File,
    step: u32,
    daemons: u32,
    agent: ureq::Agent,
}

impl Gate {
    fn new() -> Gate {
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
                .join(format!("r0-{}", now_secs())),
        };
        fs::create_dir_all(&out).expect("evidence directory");
        let (daemond, cli) = build(&workspace);
        eprintln!("gate evidence: {}", out.display());
        Gate {
            observations: File::create(out.join("observations.jsonl")).expect("observations"),
            out,
            daemond,
            cli,
            step: 0,
            daemons: 0,
            // Same discipline as the CLI (SPEC §8): no proxy, no redirects,
            // short bounded timeouts, statuses are data.
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .timeout_connect(Some(Duration::from_secs(2)))
                    .timeout_global(Some(Duration::from_secs(10)))
                    .max_redirects(0)
                    .proxy(None)
                    .http_status_as_error(false)
                    .build(),
            ),
        }
    }

    /// One greppable JSON line per step; the harness deletes nothing (SPEC §10).
    fn note(&mut self, scenario: &str, note: &str, data: Value) {
        self.step += 1;
        let line = json!({
            "scenario": scenario,
            "step": self.step,
            "ok": true,
            "note": note,
            "data": data,
        });
        writeln!(self.observations, "{line}").expect("observation");
    }

    fn endpoint_path(&self) -> PathBuf {
        self.out.join("data").join("runtime").join("endpoint.json")
    }

    fn children_path(&self) -> PathBuf {
        self.out.join("data").join("runtime").join("children.json")
    }

    fn workspace(&self, name: &str) -> PathBuf {
        self.out.join("desks").join(name)
    }

    /// Never invoke a binary without the test seam (AGENTS.md); `NO_TRADING` is
    /// accepted and inert in R0 (SPEC §2) and set as hygiene.
    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .env("MARKETRIG_TEST_DATA_ROOT", &self.out)
            .env("MARKETRIG_TEST_NO_TRADING", "1");
        command
    }

    fn read_endpoint(&self) -> Option<Endpoint> {
        fs::read_to_string(self.endpoint_path())
            .ok()
            .map(|raw| Endpoint::parse(&raw))
    }

    /// `None` when the daemon is unreachable; otherwise the status and body.
    fn request(
        &self,
        method: &str,
        port: u16,
        path: &str,
        credential: &str,
    ) -> Option<(u16, String)> {
        let url = format!("http://127.0.0.1:{port}{path}");
        let bearer = format!("Bearer {credential}");
        let sent = match method {
            "GET" => self
                .agent
                .get(url.as_str())
                .header("Authorization", bearer)
                .call(),
            _ => self
                .agent
                .post(url.as_str())
                .header("Authorization", bearer)
                .send_empty(),
        };
        let mut response = sent.ok()?;
        let status = response.status().as_u16();
        Some((status, response.body_mut().read_to_string().ok()?))
    }

    /// SPEC §5.2: authenticated health plus daemon-UUID equality.
    fn verify(&self, endpoint: &Endpoint) -> bool {
        match self.request("GET", endpoint.port, "/health", &endpoint.credential) {
            Some((200, body)) => parse(&body)["daemon_uuid"] == endpoint.daemon_uuid.as_str(),
            _ => false,
        }
    }

    fn spawn(&mut self, scenario: &str) -> Daemon {
        self.daemons += 1;
        let stderr = self.out.join(format!("marketrigd-{}.stderr", self.daemons));
        let mut child = self
            .command(&self.daemond)
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
            json!({ "daemon_uuid": endpoint.daemon_uuid, "port": endpoint.port, "pid": endpoint.pid }),
        );
        Daemon { child, endpoint }
    }

    /// The clean stop (SPEC §4.2): `POST /quit`, bounded exit 0, pointer removed.
    fn stop(&mut self, scenario: &str, mut daemon: Daemon) {
        let endpoint = daemon.endpoint.clone();
        let (status, body) = self
            .request("POST", endpoint.port, "/quit", &endpoint.credential)
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

    fn kill(&mut self, scenario: &str, mut daemon: Daemon) {
        let endpoint = daemon.endpoint.clone();
        daemon.child.kill().expect("kill marketrigd");
        let exit = await_exit(&mut daemon.child, Duration::from_secs(5));
        self.note(
            scenario,
            "daemon hard-killed",
            json!({ "daemon_uuid": endpoint.daemon_uuid, "exit": format!("{exit}") }),
        );
    }

    /// Bounded: a killed daemon's pointer must stop verifying (SPEC §5.2, §6).
    fn await_unverifiable(&self, endpoint: &Endpoint) {
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
    fn cli(&self, args: &[&str]) -> (i32, String, String) {
        let output = self
            .command(&self.cli)
            .args(args)
            .output()
            .expect("run marketrig");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// `marketrig --json …`: the exit code and the JSON on standard output —
    /// the resource on success, the §6 envelope on a daemon error.
    fn cli_json(&mut self, scenario: &str, args: &[&str]) -> (i32, Value) {
        let (exit, stdout, stderr) = self.cli(args);
        let body = parse(stdout.trim());
        self.note(
            scenario,
            "marketrig --json",
            json!({ "args": args, "exit": exit, "body": body, "stderr": stderr }),
        );
        (exit, body)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.out.join("data").join("marketrig.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open the database read-only")
    }

    fn events(&self) -> Vec<Event> {
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

    fn event_kinds(&self) -> Vec<String> {
        self.events().into_iter().map(|e| e.kind).collect()
    }

    fn recoveries(&self) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|e| e.kind == "RECOVERY")
            .map(|e| e.payload)
            .collect()
    }

    fn desk_rows(&self) -> Vec<Value> {
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

    fn kinds_for(&self, desk_id: &str) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|e| e.desk_id.as_deref() == Some(desk_id))
            .map(|e| e.kind)
            .collect()
    }
}

/// The real binaries, located through Cargo's own artifact messages so the gate
/// is correct under `CARGO_TARGET_DIR` and on Windows.
fn build(workspace: &Path) -> (PathBuf, PathBuf) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args([
            "build",
            "-p",
            "marketrigd",
            "-p",
            "marketrig",
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
            _ => {}
        }
    }
    (
        daemond.expect("marketrigd executable"),
        cli.expect("marketrig executable"),
    )
}

#[test]
fn gate() {
    let mut g = Gate::new();
    // Lowercase letters and digits keep the §7.1 grammar; the stamp names this
    // run's desks in the evidence bundle. G1 requires a fresh root either way.
    let stamp = now_secs().to_string();
    let alpha = format!("alpha-{stamp}");
    let beta = format!("beta-{stamp}");
    let gamma = format!("gamma-{stamp}");

    // --- G1 — first start ---------------------------------------------------
    assert!(
        !g.endpoint_path().exists(),
        "the evidence root starts without a daemon pointer"
    );
    let daemon1 = g.spawn("G1");
    let first = daemon1.endpoint.clone();
    assert_eq!(first.credential.len(), 64);
    assert!(first.pid > 0 && first.started_at_ns > 0 && first.port > 0);
    let recoveries = g.recoveries();
    assert_eq!(recoveries.len(), 1, "exactly one RECOVERY event");
    assert_eq!(recoveries[0]["previous_daemon_uuid"], Value::Null);
    assert_eq!(recoveries[0]["daemon_uuid"], first.daemon_uuid.as_str());
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(g.endpoint_path())
            .expect("endpoint.json")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "endpoint.json is 0600 on macOS");
    }
    g.note(
        "G1",
        "empty root started healthy with one RECOVERY and a valid endpoint",
        json!({ "recovery": recoveries[0] }),
    );

    // --- G2 — two isolated desks --------------------------------------------
    let mut ids = Vec::new();
    for name in [alpha.as_str(), beta.as_str()] {
        let (exit, desk) = g.cli_json("G2", &["--json", "desk", "create", name]);
        assert_eq!(exit, 0, "{desk}");
        assert_eq!(desk["name"], name);
        assert_eq!(desk["state"], "READY");
        assert_eq!(desk["workspace_status"], "OK");
        let workspace = g.workspace(name);
        assert_eq!(desk["workspace_path"], workspace.to_str().expect("utf-8"));
        assert_eq!(
            fs::read_to_string(workspace.join("AGENTS.md")).expect("AGENTS.md"),
            agents_seed(name),
            "the §7.6 seed, byte for byte"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("CLAUDE.md")).expect("CLAUDE.md"),
            SHIM
        );
        ids.push(desk["id"].as_str().expect("id").to_string());
    }
    let rows = g.desk_rows();
    assert_eq!(rows.len(), 2);
    for (row, (name, id)) in rows.iter().zip([(&alpha, &ids[0]), (&beta, &ids[1])]) {
        assert_eq!(row["id"], id.as_str());
        assert_eq!(row["name"], name.as_str());
        assert_eq!(row["state"], "READY");
        assert!(!row["ready_at_ns"].is_null() && row["failure_code"].is_null());
    }
    assert_eq!(
        g.event_kinds(),
        [
            "RECOVERY",
            "DESK_CREATED",
            "DESK_READY",
            "DESK_CREATED",
            "DESK_READY"
        ]
    );
    let events = g.events();
    for (event, (name, id)) in events[1..].iter().zip([
        (&alpha, &ids[0]),
        (&alpha, &ids[0]),
        (&beta, &ids[1]),
        (&beta, &ids[1]),
    ]) {
        assert_eq!(event.desk_id.as_deref(), Some(id.as_str()));
        assert_eq!(event.payload["name"], name.as_str());
    }
    g.note(
        "G2",
        "two desks READY with exact seeds, rows, and events",
        json!({ "desks": rows }),
    );

    // --- G3 — refusals ------------------------------------------------------
    for (args, code) in [
        (
            ["--json", "desk", "create", alpha.as_str()],
            "DESK_NAME_TAKEN",
        ),
        (
            ["--json", "desk", "create", "Bad--Name"],
            "DESK_NAME_INVALID",
        ),
        (
            [
                "--json",
                "desk",
                "show",
                "01999999-0000-7000-8000-0000000000ff",
            ],
            "DESK_NOT_FOUND",
        ),
    ] {
        let (exit, envelope) = g.cli_json("G3", &args);
        assert_eq!(exit, 1, "a daemon-reported error exits 1: {envelope}");
        assert_eq!(envelope["code"], code);
        assert!(
            envelope["message"]
                .as_str()
                .is_some_and(|m| m.ends_with('.')),
            "{envelope}"
        );
    }
    assert_eq!(g.desk_rows().len(), 2, "a refusal creates nothing");
    g.note("G3", "three refusals, no state change", json!({}));

    // --- G4 — clean restart -------------------------------------------------
    let rows_before = g.desk_rows();
    let events_before = g.events();
    let seed_before = fs::read_to_string(g.workspace(&alpha).join("AGENTS.md")).expect("seed");
    g.stop("G4", daemon1);
    let daemon2 = g.spawn("G4");
    let second = daemon2.endpoint.clone();
    assert_ne!(second.daemon_uuid, first.daemon_uuid);
    assert_eq!(g.desk_rows(), rows_before, "both desks survive identically");
    assert_eq!(
        fs::read_to_string(g.workspace(&alpha).join("AGENTS.md")).expect("seed"),
        seed_before,
        "workspaces are untouched by a restart"
    );
    let events_after = g.events();
    assert_eq!(
        events_after[..events_before.len()],
        events_before[..],
        "prior events are preserved"
    );
    assert_eq!(events_after.len(), events_before.len() + 1);
    let recovery = events_after.last().expect("the new event");
    assert_eq!(recovery.kind, "RECOVERY");
    assert_eq!(
        recovery.payload["previous_daemon_uuid"],
        first.daemon_uuid.as_str()
    );
    assert_eq!(recovery.payload["daemon_uuid"], second.daemon_uuid.as_str());
    g.note(
        "G4",
        "clean restart kept desks, events, and workspaces",
        json!({ "recovery": recovery.payload }),
    );

    // --- G5 — stale credential ----------------------------------------------
    let (status, body) = g
        .request("GET", second.port, "/health", &first.credential)
        .expect("the second daemon answered");
    assert_eq!(status, 401, "{body}");
    assert_eq!(parse(&body)["code"], "UNAUTHORIZED");
    assert!(g.verify(&second), "the second endpoint file verifies");
    g.note(
        "G5",
        "the first daemon's credential is rejected 401 by the second",
        json!({ "body": parse(&body) }),
    );

    // --- G6 — hard kill -----------------------------------------------------
    g.kill("G6", daemon2);
    let stale = g.read_endpoint().expect("a hard kill leaves the pointer");
    assert_eq!(
        stale, second,
        "the stale pointer still names the dead daemon"
    );
    g.await_unverifiable(&stale);
    let daemon3 = g.spawn("G6"); // the OS released the lock: the restart proves it
    let third = daemon3.endpoint.clone();
    assert_eq!(g.desk_rows(), rows_before, "desks survive a hard kill");
    let recoveries = g.recoveries();
    assert_eq!(recoveries.len(), 3);
    assert_eq!(
        recoveries[2]["previous_daemon_uuid"],
        second.daemon_uuid.as_str()
    );
    assert_eq!(recoveries[2]["daemon_uuid"], third.daemon_uuid.as_str());
    g.note(
        "G6",
        "stale pointer failed verification, restart recovered",
        json!({ "recovery": recoveries[2] }),
    );

    // --- G7 — reaping -------------------------------------------------------
    // children.json is consumed by the *next* start, so plant it while stopped.
    g.stop("G7", daemon3);
    let record = |pid: u32, args: &[&str]| {
        json!({
            "pid": pid,
            "kind": "GATE_SLEEPER",
            "args": args,
            "daemon_uuid": third.daemon_uuid,
            "launched_at_ns": 1_000,
        })
    };

    #[cfg(target_os = "macos")]
    let (mut doomed, mut survivor) = {
        // (a) recorded args still on its command line; (b) recorded args that
        // never match, so it must survive (SPEC §4.4, per D73).
        let doomed = Command::new("/bin/sleep")
            .arg("27101")
            .spawn()
            .expect("spawn sleeper");
        let survivor = Command::new("/bin/sleep")
            .arg("27102")
            .spawn()
            .expect("spawn sleeper");
        fs::write(
            g.children_path(),
            json!({ "children": [
                record(doomed.id(), &["27101"]),
                record(survivor.id(), &["27103-not-my-argument"]),
            ] })
            .to_string(),
        )
        .expect("plant children.json");
        (doomed, survivor)
    };
    #[cfg(not(target_os = "macos"))]
    let planted = {
        // Windows discards records without a check, so no real child is needed.
        let planted = [4_294_967_294u32, 4_294_967_293];
        fs::write(
            g.children_path(),
            json!({ "children": [
                record(planted[0], &["--marker"]),
                record(planted[1], &["--marker"]),
            ] })
            .to_string(),
        )
        .expect("plant children.json");
        planted
    };

    let daemon4 = g.spawn("G7");
    assert!(
        !g.children_path().exists(),
        "every record is dropped either way"
    );
    let children = g.recoveries()[3]["children"].clone();

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::ExitStatusExt;
        let status = await_exit(&mut doomed, Duration::from_secs(5));
        assert_eq!(
            status.signal(),
            Some(9),
            "the matching child was terminated"
        );
        assert!(
            survivor.try_wait().expect("try_wait").is_none(),
            "a mismatched command line survives"
        );
        assert_eq!(
            children,
            json!([
                { "pid": doomed.id(), "kind": "GATE_SLEEPER", "outcome": "TERMINATED" },
                { "pid": survivor.id(), "kind": "GATE_SLEEPER", "outcome": "PID_RECYCLED" },
            ])
        );
        survivor.kill().expect("kill the surviving sleeper");
        survivor.wait().expect("reap the surviving sleeper");
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(
            children,
            json!([
                { "pid": planted[0], "kind": "GATE_SLEEPER", "outcome": "DISCARDED" },
                { "pid": planted[1], "kind": "GATE_SLEEPER", "outcome": "DISCARDED" },
            ])
        );
    }
    g.note("G7", "recorded children reaped and reported", children);

    // --- G8 — failed creation and retry -------------------------------------
    let obstruction = g.workspace(&gamma);
    fs::write(&obstruction, "not a directory").expect("plant the obstruction");
    let (exit, failed) = g.cli_json("G8", &["--json", "desk", "create", &gamma]);
    assert_eq!(exit, 0, "the FAILED desk is still returned: {failed}");
    assert_eq!(failed["state"], "FAILED");
    assert!(failed["failure_code"].is_string() && failed["failure_message"].is_string());
    assert!(failed["ready_at_ns"].is_null() && failed["workspace_status"].is_null());
    let gamma_id = failed["id"].as_str().expect("id").to_string();
    let gamma_path = failed["workspace_path"].as_str().expect("path").to_string();
    assert_eq!(g.kinds_for(&gamma_id), ["DESK_CREATED", "DESK_FAILED"]);
    assert_eq!(
        g.events()
            .into_iter()
            .find(|e| e.kind == "DESK_FAILED")
            .expect("DESK_FAILED")
            .payload["failure_code"],
        failed["failure_code"]
    );

    fs::remove_file(&obstruction).expect("clear the obstruction");
    let (exit, retried) = g.cli_json("G8", &["--json", "desk", "retry", &gamma]);
    assert_eq!(exit, 0, "{retried}");
    assert_eq!(retried["state"], "READY");
    assert_eq!(retried["id"], gamma_id.as_str());
    assert_eq!(retried["name"], gamma.as_str());
    assert_eq!(retried["workspace_path"], gamma_path.as_str());
    assert_eq!(retried["workspace_status"], "OK");
    assert!(retried["failure_code"].is_null() && retried["failure_message"].is_null());
    assert_eq!(
        g.kinds_for(&gamma_id),
        ["DESK_CREATED", "DESK_FAILED", "DESK_RETRIED", "DESK_READY"]
    );
    assert_eq!(
        fs::read_to_string(g.workspace(&gamma).join("AGENTS.md")).expect("seed"),
        agents_seed(&gamma)
    );
    g.note(
        "G8",
        "obstructed creation FAILED, then retried READY on the same identity",
        json!({ "id": gamma_id, "workspace_path": gamma_path }),
    );

    // --- G9 — damaged READY workspace ---------------------------------------
    fs::remove_file(g.workspace(&alpha).join("AGENTS.md")).expect("damage the workspace");
    let (exit, damaged) = g.cli_json("G9", &["--json", "desk", "show", &alpha]);
    assert_eq!(exit, 0, "{damaged}");
    assert_eq!(damaged["state"], "READY", "the durable row stays READY");
    assert_eq!(damaged["workspace_status"], "UNAVAILABLE");
    assert!(
        damaged["workspace_status_reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
    let (_, healthy) = g.cli_json("G9", &["--json", "desk", "show", &beta]);
    assert_eq!(healthy["workspace_status"], "OK");
    assert_eq!(g.desk_rows()[0]["state"], "READY");

    g.stop("G9", daemon4);
    let daemon5 = g.spawn("G9");
    let (exit, listing) = g.cli_json("G9", &["--json", "desk", "list"]);
    assert_eq!(exit, 0, "{listing}");
    let desks = listing["desks"].as_array().expect("desks");
    assert_eq!(desks.len(), 3);
    assert_eq!(desks[0]["name"], alpha.as_str());
    assert_eq!(desks[0]["state"], "READY");
    assert_eq!(desks[0]["workspace_status"], "UNAVAILABLE");
    assert_eq!(desks[1]["workspace_status"], "OK");
    assert_eq!(desks[2]["workspace_status"], "OK");
    assert!(
        !g.workspace(&alpha).join("AGENTS.md").exists(),
        "a restart never rewrites an agent-owned file"
    );
    g.note(
        "G9",
        "a damaged workspace reads UNAVAILABLE and blocks nothing",
        json!({ "desks": desks }),
    );

    // --- G10 — single instance ----------------------------------------------
    let stderr_path = g.out.join("marketrigd-second-instance.stderr");
    let mut second_instance = g
        .command(&g.daemond.clone())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(&stderr_path).expect("stderr"))
        .spawn()
        .expect("spawn a second marketrigd");
    let exit = await_exit(&mut second_instance, Duration::from_secs(10));
    assert!(!exit.success(), "a second daemon on one root must fail");
    let refusal = fs::read_to_string(&stderr_path).expect("stderr");
    assert!(refusal.contains("ALREADY_RUNNING"), "{refusal}");
    assert!(
        g.verify(&daemon5.endpoint),
        "the first daemon is undisturbed"
    );
    g.note(
        "G10",
        "the second instance refused, the first still serves",
        json!({ "exit": format!("{exit}"), "stderr": refusal.trim() }),
    );

    // --- G11 — no daemon ----------------------------------------------------
    g.stop("G11", daemon5);
    let (exit, stdout, stderr) = g.cli(&["desk", "list"]);
    assert_eq!(exit, 3, "no usable daemon exits 3: {stderr}");
    assert!(stderr.contains("error: DAEMON_UNREACHABLE:"), "{stderr:?}");
    assert!(stdout.is_empty(), "{stdout:?}");
    g.note(
        "G11",
        "no daemon: exit 3",
        json!({ "stderr": stderr.trim() }),
    );

    let evidence = g.out.display().to_string();
    g.note("gate", "G1-G11 complete", json!({ "evidence": evidence }));
}
