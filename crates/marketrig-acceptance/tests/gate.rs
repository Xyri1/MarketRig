//! The acceptance gate: scenarios G1–G26, in order, in one test.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §10 (G1–G11 and
//! the evidence bundle) and `sdd/features/r1-equity-paper-trading/SPEC.md` §10
//! (the stand-in feed and G12–G20) and `sdd/features/r2-scheduled-triggers/SPEC.md`
//! §10 (the `trigger-code` binary and G21–G26), per D75, R0-7, R1-9, R2-8. The
//! harness drives
//! public surfaces only — the real binaries, `marketrig --json`, the loopback
//! API, the desk's MCP surface through the harness's own MCP client, workspace
//! files, and read-only SQLite. It never links `marketrigd` or `marketrig` as
//! libraries, so the R0 §7.6 seed, the R0 §5.1 endpoint shape, the R1 §7 element
//! shapes, and the chart-endpoint body are all re-stated here from the SPEC on
//! purpose.
//!
//! State carries across scenarios: the chain is one run against one data root,
//! which is also the evidence directory. G12 onwards trade on one desk the
//! earlier scenarios created, on the stand-in feed; G21 onwards schedule triggers
//! on `gamma`, the desk G8 left READY and untraded.

use std::fs::{self, File};
use std::process::Stdio;
use std::time::Duration;

use marketrig_acceptance::{Harness, parse, standin, within};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

/// The R0 `AGENTS.md` seed (R0 feature SPEC §7.6), with the desk name substituted.
fn agents_seed(name: &str) -> String {
    format!(
        "# {name}\n\nThis desk's constitution. MarketRig seeded it at desk creation and never\nrewrites it; its full content arrives with later MarketRig milestones.\n"
    )
}

/// The MarketRig-owned Claude Code shim (R0 feature SPEC §7.2), exactly.
const SHIM: &str = "@AGENTS.md\n";

/// One instrument's observation out of a `market/quotes` body (R1 §2.3).
#[track_caller]
fn quote_of(body: &Value, instrument_id: &str) -> Value {
    body["quotes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|quote| quote["instrument_id"] == instrument_id)
        .unwrap_or_else(|| panic!("no quote for {instrument_id}: {body}"))
        .clone()
}

/// The numeric part of a money or price string, for arithmetic the assertions do
/// and MarketRig never does: `"48.55"`, or NautilusTrader's `"100000.00 USD"`.
#[track_caller]
fn amount(text: &str) -> f64 {
    text.split_whitespace()
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("{text:?} is not an amount"))
}

/// One order body (R1 §4.2).
fn order(action_id: &str, instrument_id: &str, side: &str, kind: &str, quantity: &str) -> String {
    json!({
        "action_id": action_id, "instrument_id": instrument_id,
        "side": side, "type": kind, "quantity": quantity, "price": null,
    })
    .to_string()
}

/// A limit order body, whose price is required (R1 §4.2).
fn limit(action_id: &str, instrument_id: &str, side: &str, quantity: &str, price: &str) -> String {
    json!({
        "action_id": action_id, "instrument_id": instrument_id,
        "side": side, "type": "LIMIT", "quantity": quantity, "price": price,
    })
    .to_string()
}

/// Every venue account's per-currency total, out of the desk's durable book
/// snapshot (R1 §5) — read through read-only SQLite, because no §7 route exposes
/// a balance in R1. The snapshot's accounts are NautilusTrader's own payloads:
/// one single-variant object each, whose `base.balances` maps currency to
/// `{total, locked, free}` as money text.
fn balances(g: &Harness, desk_id: &str) -> Vec<(String, String)> {
    let payload: String = g.scalar(
        "SELECT payload FROM book_snapshots WHERE desk_id = ?1",
        &[&desk_id],
    );
    let snapshot = parse(&payload);
    let mut totals: Vec<(String, String)> = Vec::new();
    for account in snapshot["accounts"].as_array().into_iter().flatten() {
        let base = account
            .as_object()
            .and_then(|variant| variant.values().next())
            .map(|account| &account["base"])
            .unwrap_or_else(|| panic!("not an account payload: {account}"));
        let id = base["id"].as_str().unwrap_or_default();
        for (currency, balance) in base["balances"].as_object().into_iter().flatten() {
            totals.push((
                format!("{id}-{currency}"),
                balance["total"].as_str().unwrap_or_default().to_owned(),
            ));
        }
    }
    totals.sort();
    totals
}

/// The desk's durable trading history, by size — what a restart must not move.
fn history_counts(g: &Harness, desk_id: &str) -> (i64, i64, i64) {
    (
        g.scalar(
            "SELECT count(*) FROM order_events WHERE desk_id = ?1",
            &[&desk_id],
        ),
        g.scalar("SELECT count(*) FROM fills WHERE desk_id = ?1", &[&desk_id]),
        g.scalar(
            "SELECT count(*) FROM position_cycles WHERE desk_id = ?1",
            &[&desk_id],
        ),
    )
}

/// One code-bearing trigger's snapshot source: the single line the
/// `trigger-code` binary reads back (R2 feature SPEC §10.1). The file lives in
/// the evidence bundle, so every script the gate ran is in it by construction.
fn script(g: &Harness, name: &str, line: &str) -> String {
    let dir = g.out.join("scripts");
    fs::create_dir_all(&dir).expect("the scripts directory");
    let path = dir.join(format!("{name}.txt"));
    fs::write(&path, format!("{line}\n")).expect("write the script");
    path.display().to_string()
}

/// How many firings one trigger has, read through read-only SQLite.
fn firings(g: &Harness, trigger_id: &str) -> i64 {
    g.scalar(
        "SELECT count(*) FROM firings WHERE trigger_id = ?1",
        &[&trigger_id],
    )
}

/// The trigger's firings, oldest first — the order the scenarios reason in.
fn firing_ids(g: &Harness, trigger_id: &str) -> Vec<String> {
    g.column(
        "SELECT id FROM firings WHERE trigger_id = ?1 ORDER BY accepted_at_ns, id",
        &[&trigger_id],
    )
}

/// Bounded: a trigger due seconds from now fires within one wake of its
/// instant, because every mutation wakes the scheduler (R2 feature SPEC §3.1),
/// so the first firing waits 10 s. A later one waits out a whole minutely
/// cadence, hence 75. Answers the firing's id.
#[track_caller]
fn await_firing(g: &Harness, trigger_id: &str, nth: usize) -> String {
    within(
        Duration::from_secs(if nth == 0 { 10 } else { 75 }),
        &format!("firing {} of trigger {trigger_id}", nth + 1),
        || firings(g, trigger_id) > nth as i64,
    );
    firing_ids(g, trigger_id)[nth].clone()
}

/// Bounded: the executor is woken by the acceptance that produced the firing
/// (§4.3), and every scenario's script finishes in about a second.
#[track_caller]
fn await_execution(g: &Harness, firing_id: &str, bound: Duration) {
    within(bound, &format!("execution {firing_id} completing"), || {
        g.scalar::<i64>(
            "SELECT count(*) FROM executions WHERE firing_id = ?1 AND state = 'COMPLETE'",
            &[&firing_id],
        ) == 1
    });
}

/// The `TRIGGER_RESULT` prompts naming one firing. The payload is stored
/// verbatim (§5), so the firing id inside it is what ties the two together
/// without the harness reaching for a JSON function SQLite need not have.
fn result_prompts(g: &Harness, desk_id: &str, firing_id: &str) -> Vec<String> {
    g.column(
        "SELECT id FROM prompts WHERE desk_id = ?1 AND kind = 'TRIGGER_RESULT' \
         AND payload LIKE '%' || ?2 || '%' ORDER BY created_at_ns, id",
        &[&desk_id, &firing_id],
    )
}

/// Whether a trigger is still due for anything: the projection is `NULL` for a
/// consumed one-off, a disabled trigger, and a rule with no further candidate
/// (§3.1), and the resource omits the key when it is null (§8).
fn projected(g: &Harness, trigger_id: &str) -> Option<i64> {
    g.db()
        .query_row(
            "SELECT next_occurrence_ns FROM triggers WHERE id = ?1",
            [trigger_id],
            |row| row.get(0),
        )
        .expect("the trigger row")
}

async fn resource_text(service: &RunningService<RoleClient, ()>, uri: &str) -> String {
    let result = service
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .expect("read the resource");
    match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected a text resource, got {other:?}"),
    }
}

/// One tool call: whether it answered a structured error, and its text.
async fn tool(
    service: &RunningService<RoleClient, ()>,
    name: &'static str,
    arguments: Value,
) -> (bool, String) {
    let arguments = arguments
        .as_object()
        .expect("tool arguments are an object")
        .clone();
    let result = service
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await
        .expect("the call itself is routed");
    let text = result.content[0]
        .as_text()
        .expect("a text content block")
        .text
        .clone();
    (result.is_error == Some(true), text)
}

#[test]
fn gate() {
    let mut g = Harness::new("gate");
    // Lowercase letters and digits keep the §7.1 grammar; the stamp names this
    // run's desks in the evidence bundle. G1 requires a fresh root either way.
    let stamp = marketrig_acceptance::now_secs().to_string();
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
        .request("GET", second.port, "/health", &first.credential, None)
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
        // never match, so it must survive (R0 §4.4, per D73).
        let doomed = std::process::Command::new("/bin/sleep")
            .arg("27101")
            .spawn()
            .expect("spawn sleeper");
        let survivor = std::process::Command::new("/bin/sleep")
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
        let status = marketrig_acceptance::await_exit(&mut doomed, Duration::from_secs(5));
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
    let exit = marketrig_acceptance::await_exit(&mut second_instance, Duration::from_secs(10));
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

    // ======================================================================
    // R1 (feature SPEC `r1-equity-paper-trading` §10.2). The chain continues on
    // the same root with a daemon that now points at the harness's stand-in
    // feed: the seam is honored only alongside the data root, and it outranks
    // `MARKETRIG_TEST_NO_TRADING`, which stays set (§10.1). `beta` is the
    // trading desk — READY with an intact workspace.
    // ======================================================================
    let feed = standin::Feed::start();
    g.standin_feed(feed.base());
    let daemon6 = g.spawn("G12");
    let mut endpoint = daemon6.endpoint.clone();
    let desk = ids[1].clone();
    let quotes_path = format!("/desks/{desk}/market/quotes");
    let orders_path = format!("/desks/{desk}/orders");
    let positions_path = format!("/desks/{desk}/positions");
    let started = "TRADING_NODE_STARTED".to_string();

    // --- G12 — catalog and first observation --------------------------------
    let (status, listing) = g.api(
        "G12",
        &endpoint,
        "GET",
        &format!("/desks/{desk}/market/instruments"),
        None,
    );
    assert_eq!(status, 200, "{listing}");
    let instruments = listing["instruments"].as_array().expect("instruments");
    assert_eq!(instruments.len(), 15, "the §3 starter set");
    assert_eq!(instruments[0]["instrument_id"], "AAPL.XNAS");
    assert_eq!(instruments[0]["yahoo_symbol"], "AAPL");
    assert_eq!(instruments[0]["price_increment"], "0.01");
    assert_eq!(instruments[0]["lot_size"], 1);
    assert!(
        !g.kinds_for(&desk).contains(&started),
        "the compiled-in catalog starts no node"
    );

    let (status, _) = g.api("G12", &endpoint, "GET", &quotes_path, None);
    assert_eq!(status, 200);
    assert!(
        g.kinds_for(&desk).contains(&started),
        "the first quotes read starts the desk's node lazily (§4.3)"
    );
    within(Duration::from_secs(30), "AAPL's first observation", || {
        quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS")["sequence"]
            == json!(1)
    });
    let first_quote = quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS");
    assert_eq!(first_quote["provider"], "yahoo");
    assert_eq!(first_quote["venue"], "XNAS");
    assert_eq!(first_quote["last"], feed.price("AAPL").as_str());
    assert_eq!(first_quote["currency"], "USD");
    assert_eq!(first_quote["health"], "LIVE");
    assert_eq!(first_quote["book_synthesized"], true);
    for field in [
        "source_time_ns",
        "received_at_ns",
        "read_at_ns",
        "age_ms",
        "sequence",
        "market_phase",
    ] {
        assert!(!first_quote[field].is_null(), "§2.3 field {field}");
    }

    // The stand-in ticks: a source timestamp that advances replaces the
    // observation and bumps the sequence (§2.1). AAPL is idle here — no order,
    // no position — so this waits out one 30-second tier.
    let ticked = feed.tick("AAPL");
    within(Duration::from_secs(60), "AAPL's second observation", || {
        quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS")["sequence"]
            == json!(2)
    });
    let second_quote = quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS");
    assert_eq!(second_quote["last"], ticked.as_str());
    assert!(
        second_quote["source_time_ns"].as_i64() > first_quote["source_time_ns"].as_i64(),
        "the source timestamp advanced: {second_quote}"
    );
    g.note(
        "G12",
        "the catalog lists, the first quotes read started the node, and a scripted tick advanced the observation",
        json!({ "first": first_quote, "second": second_quote }),
    );

    // --- G13 — USD round trip and the queued evaluation ---------------------
    let (status, bought) = g.api(
        "G13",
        &endpoint,
        "POST",
        &orders_path,
        Some(&order("g13-buy-aapl", "AAPL.XNAS", "BUY", "MARKET", "1")),
    );
    assert_eq!(status, 201, "{bought}");
    assert_eq!(bought["kind"], "SUBMIT");
    let filled = bought["outcome"].clone();
    assert_eq!(filled["client_order_id"], "g13-buy-aapl");
    assert_eq!(filled["status"], "FILLED");
    assert_eq!(filled["filled_quantity"], "1");
    assert_eq!(filled["time_in_force"], "GTC");
    let buy_price = filled["average_price"]
        .as_str()
        .expect("a filled order carries its average price")
        .to_owned();

    // The fill and its order events landed verbatim (§5).
    assert_eq!(
        g.column(
            "SELECT price FROM fills WHERE desk_id = ?1 AND client_order_id = ?2",
            &[&desk, &"g13-buy-aapl"],
        ),
        std::slice::from_ref(&buy_price),
    );
    let kinds = g.column(
        "SELECT kind FROM order_events WHERE desk_id = ?1 AND client_order_id = ?2 \
         ORDER BY occurred_at_ns, id",
        &[&desk, &"g13-buy-aapl"],
    );
    assert!(
        kinds.first().is_some_and(|k| k == "OrderInitialized")
            && kinds.last().is_some_and(|k| k == "OrderFilled"),
        "the sandbox's own event names, in order: {kinds:?}"
    );
    for payload in g.column(
        "SELECT payload FROM order_events WHERE desk_id = ?1 AND client_order_id = ?2",
        &[&desk, &"g13-buy-aapl"],
    ) {
        assert!(parse(&payload).is_object(), "payloads are stored verbatim");
    }

    let (_, positions) = g.api("G13", &endpoint, "GET", &positions_path, None);
    let position = positions["positions"][0].clone();
    assert_eq!(positions["positions"].as_array().map(Vec::len), Some(1));
    assert_eq!(position["instrument_id"], "AAPL.XNAS");
    assert_eq!(position["side"], "LONG");
    assert_eq!(position["quantity"], "1");
    assert_eq!(position["average_open_price"], buy_price.as_str());
    assert_eq!(position["currency"], "USD");

    let usd = balances(&g, &desk)
        .into_iter()
        .find(|(account, _)| account.ends_with("-USD"))
        .expect("the desk holds a USD account");
    assert_ne!(
        amount(&usd.1),
        100_000.0,
        "the buy moved the desk's cash: {usd:?}"
    );

    // Flat again on a moved market: the closing fill closes the cycle (§6).
    let sell_tick = feed.tick("AAPL");
    within(
        Duration::from_secs(40),
        "AAPL's observation after the buy",
        || {
            quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS")["last"]
                == sell_tick.as_str()
        },
    );
    let (status, sold) = g.api(
        "G13",
        &endpoint,
        "POST",
        &orders_path,
        Some(&order("g13-sell-aapl", "AAPL.XNAS", "SELL", "MARKET", "1")),
    );
    assert_eq!(status, 201, "{sold}");
    assert_eq!(sold["outcome"]["status"], "FILLED");
    let sell_price = sold["outcome"]["average_price"]
        .as_str()
        .expect("average price")
        .to_owned();
    let (_, positions) = g.api("G13", &endpoint, "GET", &positions_path, None);
    assert_eq!(
        positions["positions"].as_array().map(Vec::len),
        Some(0),
        "the desk is flat again"
    );

    // One read-only query: every cycle this desk owns was born with its
    // evaluation prompt, keyed by the cycle id inside the prompt's payload —
    // which only one transaction can guarantee (§6).
    let (cycles, orphans): (i64, i64) = g
        .db()
        .query_row(
            "SELECT (SELECT count(*) FROM position_cycles WHERE desk_id = ?1), \
                    (SELECT count(*) FROM position_cycles c \
                       LEFT JOIN prompts p ON p.desk_id = c.desk_id \
                         AND p.kind = 'EVALUATION' \
                         AND json_extract(p.payload, '$.cycle_id') = c.id \
                      WHERE c.desk_id = ?1 AND p.id IS NULL)",
            [&desk],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the atomicity query");
    assert_eq!(
        (cycles, orphans),
        (1, 0),
        "one cycle, and no cycle without its prompt"
    );

    let (realized, currency): (String, String) = g
        .db()
        .query_row(
            "SELECT realized_pnl, currency FROM position_cycles WHERE desk_id = ?1",
            [&desk],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the cycle row");
    assert_eq!(currency, "USD");
    // The US rate is 0 bp (§4.1), so the net figure is the price move on one
    // share. MarketRig never recomputes it; the gate only checks the arithmetic.
    assert_eq!(
        realized,
        format!("{:.2}", amount(&sell_price) - amount(&buy_price)),
        "the closing event's own net realized P&L"
    );
    g.note(
        "G13",
        "USD round trip: fill captured, position live, balance moved, cycle and prompt born together",
        json!({ "buy": buy_price, "sell": sell_price, "realized_pnl": realized }),
    );

    // --- G14 — resting, cancel, and replay ----------------------------------
    let resting = limit("g14-rest-aapl", "AAPL.XNAS", "BUY", "1", "100.00");
    let (status, rested) = g.api("G14", &endpoint, "POST", &orders_path, Some(&resting));
    assert_eq!(status, 201, "{rested}");
    assert_eq!(rested["outcome"]["status"], "ACCEPTED");
    let (_, open) = g.api("G14", &endpoint, "GET", &orders_path, None);
    assert_eq!(open["orders"].as_array().map(Vec::len), Some(1));
    assert_eq!(open["orders"][0]["client_order_id"], "g14-rest-aapl");
    assert_eq!(open["orders"][0]["price"], "100.00");

    let (status, replay) = g.api("G14", &endpoint, "POST", &orders_path, Some(&resting));
    assert_eq!(status, 200, "a repeated action_id replays: {replay}");
    assert_eq!(replay, rested, "the stored record, byte for byte");
    let (_, open) = g.call(&endpoint, "GET", &orders_path, None);
    assert_eq!(
        open["orders"].as_array().map(Vec::len),
        Some(1),
        "a replay creates no second order"
    );
    assert_eq!(
        g.scalar::<i64>(
            "SELECT count(*) FROM trading_actions WHERE desk_id = ?1 AND action_id = ?2",
            &[&desk, &"g14-rest-aapl"],
        ),
        1,
    );

    let (status, cancelled) = g.api(
        "G14",
        &endpoint,
        "POST",
        &format!("/desks/{desk}/orders/g14-rest-aapl/cancel"),
        Some(r#"{"action_id":"g14-cancel-aapl"}"#),
    );
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["kind"], "CANCEL");
    assert_eq!(cancelled["outcome"]["status"], "CANCELED");
    let (_, open) = g.call(&endpoint, "GET", &orders_path, None);
    assert_eq!(open["orders"].as_array().map(Vec::len), Some(0));

    let (status, envelope) = g.api(
        "G14",
        &endpoint,
        "POST",
        &format!("/desks/{desk}/orders/no-such-order/cancel"),
        Some(r#"{"action_id":"g14-cancel-unknown"}"#),
    );
    assert_eq!(status, 404, "{envelope}");
    assert_eq!(envelope["code"], "ORDER_NOT_FOUND");
    g.note(
        "G14",
        "a limit order rested, replayed once, cancelled, and an unknown id refused",
        json!({ "record": rested, "cancel": cancelled }),
    );

    // --- G15 — non-USD cycle ------------------------------------------------
    within(Duration::from_secs(30), "0700.XHKG's observation", || {
        quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "0700.XHKG")["health"] == "LIVE"
    });
    let (status, bought) = g.api(
        "G15",
        &endpoint,
        "POST",
        &orders_path,
        Some(&order(
            "g15-buy-tencent",
            "0700.XHKG",
            "BUY",
            "MARKET",
            "100",
        )),
    );
    assert_eq!(status, 201, "{bought}");
    assert_eq!(bought["outcome"]["status"], "FILLED");
    assert_eq!(bought["outcome"]["filled_quantity"], "100");
    let hk_buy = bought["outcome"]["average_price"]
        .as_str()
        .expect("average price")
        .to_owned();

    let hk_tick = feed.tick("0700.HK");
    within(
        Duration::from_secs(40),
        "0700.XHKG's observation after the buy",
        || {
            quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "0700.XHKG")["last"]
                == hk_tick.as_str()
        },
    );
    let (status, sold) = g.api(
        "G15",
        &endpoint,
        "POST",
        &orders_path,
        Some(&order(
            "g15-sell-tencent",
            "0700.XHKG",
            "SELL",
            "MARKET",
            "100",
        )),
    );
    assert_eq!(status, 201, "{sold}");
    assert_eq!(sold["outcome"]["status"], "FILLED");
    let hk_sell = sold["outcome"]["average_price"]
        .as_str()
        .expect("average price")
        .to_owned();

    let hk_fills: Vec<Value> = g
        .column(
            "SELECT price || ' ' || commission || ' ' || currency FROM fills \
             WHERE desk_id = ?1 AND instrument_id = '0700.XHKG' ORDER BY occurred_at_ns, id",
            &[&desk],
        )
        .into_iter()
        .map(Value::String)
        .collect();
    assert_eq!(hk_fills.len(), 2, "one fill a side: {hk_fills:?}");
    let mut fees = 0.0;
    for fill in &hk_fills {
        let fields: Vec<&str> = fill.as_str().expect("row").split(' ').collect();
        let (price, commission, currency) = (fields[0], fields[1], fields[2]);
        assert_eq!(currency, "HKD", "the fill settles in the venue's currency");
        assert!(amount(commission) > 0.0, "a nonzero commission: {fill}");
        let rate = amount(commission) / (amount(price) * 100.0);
        assert!(
            (rate - 0.0011).abs() < 1e-6,
            "the HK rate is 11 bp a side, got {rate} from {fill}"
        );
        fees += amount(commission);
    }

    let (realized, currency): (String, String) = g
        .db()
        .query_row(
            "SELECT realized_pnl, currency FROM position_cycles \
             WHERE desk_id = ?1 AND instrument_id = '0700.XHKG'",
            [&desk],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the HKD cycle row");
    assert_eq!(currency, "HKD");
    assert_eq!(
        realized,
        format!("{:.2}", (amount(&hk_sell) - amount(&hk_buy)) * 100.0 - fees),
        "net of both sides' fees (root §12.4)"
    );
    g.note(
        "G15",
        "an HKD round trip closed with realized P&L in HKD and 11 bp on both fills",
        json!({ "buy": hk_buy, "sell": hk_sell, "fees": fees, "realized_pnl": realized }),
    );

    // --- G16 — refusals -----------------------------------------------------
    for instrument in ["600519.XSHG", "000001.XSHE"] {
        within(
            Duration::from_secs(30),
            &format!("{instrument}'s observation"),
            || {
                quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, instrument)["health"]
                    == "LIVE"
            },
        );
    }
    let refusals: [(&str, String, u16, &str, &str); 4] = [
        (
            "g16-unknown",
            order("g16-unknown", "NOPE.XNAS", "BUY", "MARKET", "1"),
            404,
            "INSTRUMENT_UNKNOWN",
            "",
        ),
        (
            "g16-off-lot",
            order("g16-off-lot", "0700.XHKG", "BUY", "MARKET", "150"),
            400,
            "ORDER_INVALID",
            "",
        ),
        (
            // Far beyond the 500,000 CNY seeded at XSHG (§4.1).
            "g16-too-big",
            order("g16-too-big", "600519.XSHG", "BUY", "MARKET", "50000"),
            409,
            "ORDER_REJECTED",
            "FREE_BALANCE",
        ),
        (
            // Flat, so this sells beyond the held quantity.
            "g16-short",
            order("g16-short", "000001.XSHE", "SELL", "MARKET", "100"),
            409,
            "ORDER_REJECTED",
            "Short selling",
        ),
    ];
    for (action_id, body, status, code, reason) in &refusals {
        let (got, envelope) = g.api("G16", &endpoint, "POST", &orders_path, Some(body));
        assert_eq!(got, *status, "{action_id}: {envelope}");
        assert_eq!(envelope["code"], *code, "{action_id}");
        let message = envelope["message"].as_str().expect("a message");
        assert!(message.ends_with('.'), "{action_id}: {message}");
        assert!(
            message.contains(reason),
            "{action_id} must carry the sandbox's own reason: {message}"
        );
    }
    // A form refusal records no action at all; a sandbox refusal records one
    // whose outcome is the refused order, never an accepted one (§6).
    for action_id in ["g16-unknown", "g16-off-lot"] {
        assert_eq!(
            g.scalar::<i64>(
                "SELECT count(*) FROM trading_actions WHERE desk_id = ?1 AND action_id = ?2",
                &[&desk, &action_id],
            ),
            0,
            "{action_id} is refused before the sandbox sees anything",
        );
    }
    for action_id in ["g16-too-big", "g16-short"] {
        let outcome: String = g.scalar(
            "SELECT outcome FROM trading_actions WHERE desk_id = ?1 AND action_id = ?2",
            &[&desk, &action_id],
        );
        let status = parse(&outcome)["status"].as_str().unwrap_or("").to_owned();
        assert!(
            matches!(status.as_str(), "DENIED" | "REJECTED"),
            "{action_id} recorded {status}, not an accepted order"
        );
    }
    g.note(
        "G16",
        "four refusals, each with its documented code and the sandbox's own reason",
        json!({ "actions": refusals.iter().map(|r| r.0).collect::<Vec<_>>() }),
    );

    // --- G17 — restoration --------------------------------------------------
    let (status, held) = g.api(
        "G17",
        &endpoint,
        "POST",
        &orders_path,
        Some(&order("g17-buy-aapl", "AAPL.XNAS", "BUY", "MARKET", "1")),
    );
    assert_eq!(status, 201, "{held}");
    assert_eq!(held["outcome"]["status"], "FILLED");
    let (status, standing) = g.api(
        "G17",
        &endpoint,
        "POST",
        &orders_path,
        Some(&limit("g17-rest-aapl", "AAPL.XNAS", "BUY", "1", "100.00")),
    );
    assert_eq!(status, 201, "{standing}");
    assert_eq!(standing["outcome"]["status"], "ACCEPTED");

    let balances_before = balances(&g, &desk);
    let history_before = history_counts(&g, &desk);
    // Dark before the restart, so the restored desk has nothing to observe until
    // the feed comes back.
    feed.dark("AAPL", true);
    g.stop("G17", daemon6);
    let daemon7 = g.spawn("G17");
    endpoint = daemon7.endpoint.clone();

    let (status, open) = g.api("G17", &endpoint, "GET", &orders_path, None);
    assert_eq!(status, 200, "{open}");
    assert_eq!(open["orders"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        open["orders"][0]["client_order_id"], "g17-rest-aapl",
        "the resting order came back under its original client order id"
    );
    assert_eq!(open["orders"][0]["price"], "100.00");
    assert_eq!(open["orders"][0]["status"], "ACCEPTED");
    let (_, positions) = g.api("G17", &endpoint, "GET", &positions_path, None);
    assert_eq!(positions["positions"].as_array().map(Vec::len), Some(1));
    assert_eq!(positions["positions"][0]["instrument_id"], "AAPL.XNAS");
    assert_eq!(positions["positions"][0]["quantity"], "1");

    let dark_quote = quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS");
    assert_eq!(dark_quote["health"], "UNAVAILABLE");
    assert_eq!(dark_quote["sequence"], 0);
    for field in [
        "last",
        "currency",
        "source_time_ns",
        "received_at_ns",
        "age_ms",
    ] {
        assert!(
            dark_quote.get(field).is_none(),
            "an unobserved instrument omits {field}: {dark_quote}"
        );
    }
    assert_eq!(
        balances(&g, &desk),
        balances_before,
        "the desk's durable balances are what they were"
    );
    assert_eq!(
        history_counts(&g, &desk),
        history_before,
        "restoration replays no history"
    );

    feed.dark("AAPL", false);
    within(
        Duration::from_secs(40),
        "AAPL's observation after the feed returns",
        || {
            quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS")["health"]
                == "LIVE"
        },
    );
    g.note(
        "G17",
        "a restart restored the resting order and the position, left history and balances alone, and quotes stayed UNAVAILABLE until the stand-in answered again",
        json!({ "orders": open["orders"], "positions": positions["positions"] }),
    );

    // --- G18 — feed honesty -------------------------------------------------
    // AAPL carries a position and a resting order, so it polls on the tightened
    // 10-second tier (§2.1) and each step below waits at most one of those.
    let aapl = |g: &Harness| quote_of(&g.call(&endpoint, "GET", &quotes_path, None).1, "AAPL.XNAS");
    let sequence = aapl(&g)["sequence"].as_u64().expect("a sequence");

    // Arm between polls, never during one, so the counts below are exact.
    feed.quiet("AAPL");
    let before = feed.hits("AAPL");
    feed.burst_429("AAPL", 7);
    let accepted = feed.tick("AAPL");
    within(
        Duration::from_secs(40),
        "the observation accepted on the eighth attempt",
        || aapl(&g)["sequence"] == json!(sequence + 1),
    );
    let attempts = feed.hits("AAPL") - before;
    assert_eq!(attempts, 8, "the retry bound is eight attempts (§2.1)");
    assert_eq!(aapl(&g)["last"], accepted.as_str());
    assert_eq!(aapl(&g)["health"], "LIVE");

    feed.quiet("AAPL");
    let before = feed.hits("AAPL");
    feed.burst_429("AAPL", 9);
    feed.tick("AAPL");
    within(
        Duration::from_secs(40),
        "the exhausted retry marking the feed degraded",
        || aapl(&g)["health"] == "DEGRADED",
    );
    let exhausted = aapl(&g);
    assert_eq!(feed.hits("AAPL") - before, 8, "and never a ninth attempt");
    assert_eq!(
        exhausted["sequence"],
        json!(sequence + 1),
        "exhaustion accepts nothing"
    );
    assert_eq!(
        exhausted["last"],
        accepted.as_str(),
        "the last accepted observation stands"
    );

    // Live again, then dark: the observation stands and ages.
    feed.burst_429("AAPL", 0);
    let standing = feed.tick("AAPL");
    within(Duration::from_secs(40), "the feed recovering", || {
        aapl(&g)["health"] == "LIVE" && aapl(&g)["last"] == standing.as_str()
    });
    feed.dark("AAPL", true);
    within(Duration::from_secs(40), "the dark feed degrading", || {
        aapl(&g)["health"] == "DEGRADED"
    });
    let aged = aapl(&g);
    assert_eq!(aged["last"], standing.as_str());
    std::thread::sleep(Duration::from_millis(700));
    let older = aapl(&g);
    assert_eq!(older["last"], standing.as_str());
    assert!(
        older["age_ms"].as_i64() > aged["age_ms"].as_i64(),
        "the standing observation ages: {aged} then {older}"
    );

    // A catalog instrument the stand-in never serves was never observed at all.
    let never = quote_of(
        &g.call(&endpoint, "GET", &quotes_path, None).1,
        standin::UNSERVED_INSTRUMENT,
    );
    assert_eq!(never["health"], "UNAVAILABLE");
    assert_eq!(never["sequence"], 0);
    for field in [
        "last",
        "currency",
        "source_time_ns",
        "received_at_ns",
        "age_ms",
    ] {
        assert!(never.get(field).is_none(), "{field}: {never}");
    }
    assert!(!never["read_at_ns"].is_null() && !never["market_phase"].is_null());
    feed.dark("AAPL", false);
    g.note(
        "G18",
        "eight attempts then acceptance, nine 429s then DEGRADED with the last observation standing and ageing, and a never-served instrument UNAVAILABLE",
        json!({ "attempts": attempts, "degraded": exhausted, "unavailable": never }),
    );

    // --- G19 — the MCP plane ------------------------------------------------
    let mcp = g.mcp.clone();
    let data_root = g.out.clone();
    let desk_name = beta.clone();
    let quotes_uri = format!("marketrig://desk/{beta}/quotes");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the harness's MCP client");
    let (uris, before_read, after_read, submitted, cancelled, invalid) = runtime.block_on(async {
        let command = tokio::process::Command::new(&mcp).configure(|command| {
            command
                .arg("--desk")
                .arg(&desk_name)
                .env("MARKETRIG_TEST_DATA_ROOT", &data_root);
        });
        let service =
            ().serve(TokioChildProcess::new(command).expect("spawn marketrig-mcp"))
                .await
                .expect("initialize the MCP session");

        let listed = service
            .list_resources(None)
            .await
            .expect("list the resources");
        let uris: Vec<String> = listed
            .resources
            .iter()
            .map(|resource| resource.uri.to_string())
            .collect();

        // Two reads straddling a scripted tick. The body carries the read
        // instant, so the observation itself is what must differ.
        let before_read = quote_of(
            &parse(&resource_text(&service, &quotes_uri).await),
            "AAPL.XNAS",
        );
        feed.tick("AAPL");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
        let after_read = loop {
            let read = quote_of(
                &parse(&resource_text(&service, &quotes_uri).await),
                "AAPL.XNAS",
            );
            if read["sequence"] != before_read["sequence"] {
                break read;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the quote resource never showed a new observation"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        };

        let submitted = tool(
            &service,
            "submit_order",
            json!({
                "action_id": "g19-rest-aapl", "instrument_id": "AAPL.XNAS",
                "side": "BUY", "type": "LIMIT", "quantity": "1", "price": "100.00",
            }),
        )
        .await;
        let cancelled = tool(
            &service,
            "cancel_order",
            json!({ "client_order_id": "g19-rest-aapl", "action_id": "g19-cancel-aapl" }),
        )
        .await;
        // Two fields against the six-field schema: the daemon judges it, not the
        // adapter (per D4).
        let invalid = tool(
            &service,
            "submit_order",
            json!({ "action_id": "g19-bad", "side": "BUY" }),
        )
        .await;

        let _ = service.cancel().await;
        (uris, before_read, after_read, submitted, cancelled, invalid)
    });

    assert_eq!(
        uris,
        [
            format!("marketrig://desk/{beta}/quotes"),
            format!("marketrig://desk/{beta}/book"),
            format!("marketrig://desk/{beta}/positions"),
            format!("marketrig://desk/{beta}/orders"),
            format!("marketrig://desk/{beta}/instruments"),
        ],
        "resources/list names exactly the five concrete URIs (§8)"
    );
    assert!(
        after_read["sequence"].as_u64() > before_read["sequence"].as_u64()
            && after_read["last"] != before_read["last"],
        "the re-read straddled a tick: {before_read} then {after_read}"
    );
    assert!(
        !submitted.0,
        "submit_order answered an error: {submitted:?}"
    );
    assert_eq!(parse(&submitted.1)["outcome"]["status"], "ACCEPTED");
    assert!(
        !cancelled.0,
        "cancel_order answered an error: {cancelled:?}"
    );
    assert_eq!(parse(&cancelled.1)["outcome"]["status"], "CANCELED");
    assert!(invalid.0, "a malformed call is a structured tool error");
    assert!(
        invalid.1.contains("ORDER_INVALID"),
        "the tool error carries the daemon's code: {}",
        invalid.1
    );
    g.note(
        "G19",
        "five resources, a re-read that straddled a tick, a tool round trip, and a server-side ORDER_INVALID",
        json!({ "resources": uris, "submitted": submitted.1, "cancelled": cancelled.1, "invalid": invalid.1 }),
    );

    // --- G20 — the history group --------------------------------------------
    let (exit, orders) = g.cli_json("G20", &["--json", "history", "orders", &beta]);
    assert_eq!(exit, 0, "{orders}");
    let listed: Vec<String> = orders["orders"]
        .as_array()
        .expect("orders")
        .iter()
        .map(|order| order["client_order_id"].as_str().unwrap_or("").to_owned())
        .collect();
    let mut sorted = listed.clone();
    sorted.sort();
    let mut captured = g.column(
        "SELECT DISTINCT client_order_id FROM order_events WHERE desk_id = ?1",
        &[&desk],
    );
    captured.sort();
    assert_eq!(
        sorted, captured,
        "history orders are exactly the desk's captured orders"
    );
    assert_eq!(
        listed.first().map(String::as_str),
        g.column(
            "SELECT client_order_id FROM order_events WHERE desk_id = ?1 \
             ORDER BY occurred_at_ns DESC, id DESC LIMIT 1",
            &[&desk],
        )
        .first()
        .map(String::as_str),
        "newest first: {listed:?}"
    );

    let (exit, fills) = g.cli_json("G20", &["--json", "history", "fills", &beta]);
    assert_eq!(exit, 0, "{fills}");
    let listed: Vec<String> = fills["fills"]
        .as_array()
        .expect("fills")
        .iter()
        .map(|fill| {
            format!(
                "{} {} {} {}",
                fill["id"].as_str().unwrap_or(""),
                fill["price"].as_str().unwrap_or(""),
                fill["commission"].as_str().unwrap_or(""),
                fill["currency"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        listed,
        g.column(
            "SELECT id || ' ' || price || ' ' || commission || ' ' || currency FROM fills \
             WHERE desk_id = ?1 ORDER BY occurred_at_ns DESC, id DESC",
            &[&desk],
        ),
        "the fills rows, field for field, newest first"
    );

    let (exit, cycles) = g.cli_json("G20", &["--json", "history", "cycles", &beta]);
    assert_eq!(exit, 0, "{cycles}");
    let listed: Vec<String> = cycles["cycles"]
        .as_array()
        .expect("cycles")
        .iter()
        .map(|cycle| cycle["id"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(
        listed,
        g.column(
            "SELECT id FROM position_cycles WHERE desk_id = ?1 \
             ORDER BY closed_at_ns DESC, id DESC",
            &[&desk],
        ),
        "the cycle rows, newest first"
    );

    let (exit, envelope) = g.cli_json(
        "G20",
        &["--json", "history", "orders", &format!("no-such-{stamp}")],
    );
    assert_eq!(exit, 1, "an unknown desk exits 1: {envelope}");
    assert_eq!(envelope["code"], "DESK_NOT_FOUND");

    g.stop("G20", daemon7);
    let (exit, stdout, stderr) = g.cli(&["history", "orders", &beta]);
    assert_eq!(exit, 3, "no usable daemon exits 3: {stderr}");
    assert!(stderr.contains("error: DAEMON_UNREACHABLE:"), "{stderr:?}");
    assert!(stdout.is_empty(), "{stdout:?}");
    g.note(
        "G20",
        "history matched the durable rows, an unknown desk exited 1, and no daemon exited 3",
        json!({ "orders": orders["orders"], "cycles": cycles["cycles"] }),
    );

    // ======================================================================
    // R2 (feature SPEC `r2-scheduled-triggers` §10.2). The chain continues on
    // the same root and the same stand-in feed with a fresh daemon. `gamma` is
    // the trigger desk — READY since G8 and never traded — so G22's position
    // and its "no session ever existed" are exact facts about the desk rather
    // than differences against `beta`'s R1 history.
    //
    // Timing throughout: the scheduler rechecks every 60 s, but every trigger
    // mutation wakes it (§3.1), so an occurrence a couple of seconds out is
    // accepted within about a second of its instant and the code-bearing ones
    // start immediately. Every wait below is bounded far beyond that.
    // ======================================================================
    let daemon8 = g.spawn("G21");
    endpoint = daemon8.endpoint.clone();
    let ns = 1_000_000_000i64;
    let now = || marketrig_acceptance::now_secs() as i64;
    let trigger_route = |id: &str| format!("/desks/{gamma_id}/triggers/{id}");
    let runner = g.trigger_code.display().to_string();

    // --- G21 — definitions --------------------------------------------------
    let soon = now() + 2;
    let at = format!("{}Z", marketrig_acceptance::utc(soon));
    let dtstart = marketrig_acceptance::utc(soon);
    let (exit, once) = g.cli_json(
        "G21",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g21-once",
            "--brief",
            "the code-free one-off",
            "--at",
            &at,
        ],
    );
    assert_eq!(exit, 0, "{once}");
    let once_id = once["id"].as_str().expect("id").to_owned();
    assert_eq!(once["desk_id"], gamma_id.as_str());
    assert_eq!(once["name"], "g21-once");
    assert_eq!(once["source"], "SCHEDULED");
    assert_eq!(once["recurrence"], "ONE_OFF");
    assert_eq!(once["brief"], "the code-free one-off");
    assert_eq!(once["enabled"], true);
    assert_eq!(once["revision"], 1);
    assert_eq!(once["schedule"], json!({ "at_ns": soon * ns }));
    assert_eq!(once["next_occurrence_ns"], json!(soon * ns));
    for absent in ["context", "code", "deleted_at_ns"] {
        assert!(
            once.get(absent).is_none(),
            "a null field is omitted, never a literal null (§8): {absent} in {once}"
        );
    }

    let (exit, every) = g.cli_json(
        "G21",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g21-every",
            "--brief",
            "the code-free minutely rule",
            "--rrule",
            "FREQ=MINUTELY",
            "--dtstart",
            &dtstart,
            "--tz",
            "UTC",
        ],
    );
    assert_eq!(exit, 0, "{every}");
    let every_id = every["id"].as_str().expect("id").to_owned();
    assert_eq!(every["recurrence"], "RECURRING");
    assert_eq!(
        every["schedule"],
        json!({ "rrule": "FREQ=MINUTELY", "dtstart": dtstart, "tz": "UTC" })
    );
    assert_eq!(every["next_occurrence_ns"], json!(soon * ns));

    let (exit, listing) = g.cli_json("G21", &["--json", "trigger", "list", &gamma]);
    assert_eq!(exit, 0, "{listing}");
    let named: Vec<&str> = listing["triggers"]
        .as_array()
        .expect("triggers")
        .iter()
        .map(|trigger| trigger["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(named, ["g21-once", "g21-every"], "creation order (§8)");
    let (exit, shown) = g.cli_json("G21", &["--json", "trigger", "show", &gamma, "g21-once"]);
    assert_eq!(exit, 0, "{shown}");
    assert_eq!(shown["id"], once_id.as_str(), "name-or-id resolution (§9)");

    // The firing and its queued prompt in one read-only query: a code-free
    // firing's prompt commits in the acceptance unit itself (§3.2, §5).
    within(
        Duration::from_secs(10),
        "the one-off's firing and its TRIGGER_RESULT prompt",
        || {
            g.scalar::<i64>(
                "SELECT count(*) FROM firings f JOIN prompts p ON p.desk_id = f.desk_id \
                 WHERE f.trigger_id = ?1 AND p.kind = 'TRIGGER_RESULT' \
                 AND p.payload LIKE '%' || f.id || '%'",
                &[&once_id],
            ) == 1
        },
    );
    let once_firing = firing_ids(&g, &once_id)[0].clone();
    assert_eq!(
        g.scalar::<i64>(
            "SELECT occurrence_ns FROM firings WHERE id = ?1",
            &[&once_firing]
        ),
        soon * ns
    );
    assert_eq!(
        projected(&g, &once_id),
        None,
        "a consumed one-off is never due again"
    );
    let (status, consumed) = g.api("G21", &endpoint, "GET", &trigger_route(&once_id), None);
    assert_eq!(status, 200, "{consumed}");
    assert!(
        consumed.get("next_occurrence_ns").is_none(),
        "a null projection is omitted from the resource (§8): {consumed}"
    );

    let (exit, prompts) = g.cli_json("G21", &["--json", "prompt", "list", &gamma]);
    assert_eq!(exit, 0, "{prompts}");
    let queued = prompts["prompts"]
        .as_array()
        .expect("prompts")
        .iter()
        .find(|prompt| prompt["id"] == result_prompts(&g, &gamma_id, &once_firing)[0].as_str())
        .expect("the one-off's prompt is listed")
        .clone();
    assert_eq!(queued["kind"], "TRIGGER_RESULT");
    // R3: with both runtimes UNDISCOVERED the dispatcher fails the queued row
    // RUNTIME_UNAVAILABLE as soon as it wakes; either state proves the insert.
    assert!(
        matches!(queued["state"].as_str(), Some("QUEUED" | "FAILED")),
        "{}",
        queued
    );
    assert!(
        queued.get("payload").is_none(),
        "the listing carries no payload (§8)"
    );
    let queued_id = queued["id"].as_str().expect("id").to_owned();
    let (exit, prompt) = g.cli_json("G21", &["--json", "prompt", "show", &gamma, &queued_id]);
    assert_eq!(exit, 0, "{prompt}");
    assert_eq!(prompt["payload"]["kind"], "TRIGGER_RESULT");
    assert_eq!(prompt["payload"]["trigger_id"], once_id.as_str());
    assert_eq!(prompt["payload"]["trigger_name"], "g21-once");
    assert_eq!(prompt["payload"]["firing_id"], once_firing.as_str());
    assert_eq!(prompt["payload"]["brief"], "the code-free one-off");
    assert_eq!(
        prompt["payload"]["execution"],
        Value::Null,
        "a code-free firing has no execution (§5); the stored payload is verbatim, \
         so this one is a literal null: {prompt}"
    );

    // The rule fires once and re-projects from the occurrence, exactly a minute
    // on (§2, §3.2) — never from `now`.
    let every_firing = await_firing(&g, &every_id, 0);
    let occurrence: i64 = g.scalar(
        "SELECT occurrence_ns FROM firings WHERE id = ?1",
        &[&every_firing],
    );
    assert_eq!(occurrence, soon * ns);
    assert_eq!(projected(&g, &every_id), Some(occurrence + 60 * ns));

    let (exit, deleted) = g.cli_json("G21", &["--json", "trigger", "delete", &gamma, "g21-every"]);
    assert_eq!(exit, 0, "{deleted}");
    assert!(
        deleted["deleted_at_ns"].as_i64().is_some_and(|at| at > 0),
        "{deleted}"
    );
    assert_eq!(
        deleted["revision"], every["revision"],
        "a delete never bumps the revision (§8)"
    );
    let (_, listing) = g.cli_json("G21", &["--json", "trigger", "list", &gamma]);
    assert!(
        !listing["triggers"]
            .as_array()
            .expect("triggers")
            .iter()
            .any(|trigger| trigger["id"] == every_id.as_str()),
        "a deleted trigger leaves the listing: {listing}"
    );
    let (exit, gone) = g.cli_json("G21", &["--json", "trigger", "show", &gamma, &every_id]);
    assert_eq!(exit, 0, "a deleted trigger still answers by id: {gone}");
    assert!(
        gone["deleted_at_ns"].as_i64().is_some_and(|at| at > 0),
        "{gone}"
    );
    let (exit, kept) = g.cli_json("G21", &["--json", "trigger", "firings", &gamma, &every_id]);
    assert_eq!(exit, 0, "{kept}");
    assert_eq!(kept["firings"].as_array().map(Vec::len), Some(1));
    assert_eq!(kept["firings"][0]["id"], every_firing.as_str());
    assert_eq!(kept["firings"][0]["occurrence_ns"], json!(occurrence));

    // Three seconds, not two: creating and then disabling is two CLI
    // invocations, each a discovery plus a resolve, and the disable must commit
    // while the candidate is still ahead.
    let later = now() + 3;
    let (exit, idle) = g.cli_json(
        "G21",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g21-idle",
            "--brief",
            "disabled before its first candidate",
            "--rrule",
            "FREQ=MINUTELY",
            "--dtstart",
            &marketrig_acceptance::utc(later),
            "--tz",
            "UTC",
        ],
    );
    assert_eq!(exit, 0, "{idle}");
    let idle_id = idle["id"].as_str().expect("id").to_owned();
    let (exit, disabled) = g.cli_json("G21", &["--json", "trigger", "disable", &gamma, "g21-idle"]);
    assert_eq!(exit, 0, "{disabled}");
    assert_eq!(disabled["enabled"], false);
    assert!(
        disabled.get("next_occurrence_ns").is_none(),
        "disabling makes a trigger never due (§3.1): {disabled}"
    );
    assert!(
        now() < later,
        "the disable landed after the candidate; the gate's margin is too thin"
    );
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(firings(&g, &idle_id), 0, "a disabled trigger does not fire");
    assert!(
        !g.events().iter().any(|event| event.kind == "TRIGGER_MISSED"
            && event.payload["trigger_id"] == idle_id.as_str()),
        "an ineligible trigger is neither fired nor missed (§3.1)"
    );

    let past = format!("{}Z", marketrig_acceptance::utc(now() - 60));
    let ahead = format!("{}Z", marketrig_acceptance::utc(now() + 3_600));
    let refusals: Vec<(&str, Vec<&str>)> = vec![
        (
            "TRIGGER_INVALID",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g21-secondly",
                "--brief",
                "refused",
                "--rrule",
                "FREQ=SECONDLY",
                "--dtstart",
                &dtstart,
                "--tz",
                "UTC",
            ],
        ),
        (
            "TRIGGER_INVALID",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g21-counted",
                "--brief",
                "refused",
                "--rrule",
                "FREQ=MINUTELY;COUNT=3",
                "--dtstart",
                &dtstart,
                "--tz",
                "UTC",
            ],
        ),
        (
            "TRIGGER_INVALID",
            vec![
                "--json", "trigger", "create", &gamma, "--name", "g21-past", "--brief", "refused",
                "--at", &past,
            ],
        ),
        (
            "TRIGGER_INVALID",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g21-mars",
                "--brief",
                "refused",
                "--rrule",
                "FREQ=MINUTELY",
                "--dtstart",
                &dtstart,
                "--tz",
                "Mars/Olympus",
            ],
        ),
        (
            "TRIGGER_NAME_TAKEN",
            vec![
                "--json", "trigger", "create", &gamma, "--name", "g21-once", "--brief", "refused",
                "--at", &ahead,
            ],
        ),
        (
            "TRIGGER_NOT_FOUND",
            vec!["--json", "trigger", "show", &gamma, "nope"],
        ),
    ];
    for (code, args) in &refusals {
        let (exit, envelope) = g.cli_json("G21", args);
        assert_eq!(exit, 1, "{args:?} must be refused: {envelope}");
        assert_eq!(envelope["code"], *code, "{args:?}: {envelope}");
        assert!(
            envelope["message"].as_str().is_some_and(|m| !m.is_empty()),
            "{envelope}"
        );
    }
    g.note(
        "G21",
        "a code-free one-off fired once with its queued prompt, a minutely rule re-projected exactly 60 s on, a delete kept its firing readable, a disable fired nothing, and six refusals answered their codes",
        json!({
            "one_off": consumed, "recurring": every, "firing": every_firing,
            "refusals": refusals.iter().map(|(code, _)| *code).collect::<Vec<_>>(),
        }),
    );

    // --- G22 — code fires with no agent alive -------------------------------
    // `gamma` has never traded, so its node has never started. One quotes read
    // starts it and the stand-in gives it an observation before the trigger's
    // market order reaches the sandbox (R1 feature SPEC §4.3).
    let gamma_quotes = format!("/desks/{gamma_id}/market/quotes");
    let (status, opened) = g.api("G22", &endpoint, "GET", &gamma_quotes, None);
    assert_eq!(status, 200, "{opened}");
    within(
        Duration::from_secs(60),
        "gamma's first AAPL observation",
        || {
            quote_of(
                &g.call(&endpoint, "GET", &gamma_quotes, None).1,
                "AAPL.XNAS",
            )["health"]
                == "LIVE"
        },
    );

    let order_line = "order AAPL.XNAS BUY 1";
    let order_script = script(&g, "g22-order", order_line);
    let at = format!("{}Z", marketrig_acceptance::utc(now() + 2));
    let (exit, trading) = g.cli_json(
        "G22",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g22-order",
            "--brief",
            "buy one lot of AAPL through the desk's own adapter",
            "--at",
            &at,
            "--code",
            &order_script,
            "--arg",
            &runner,
            "--arg",
            "{script}",
        ],
    );
    assert_eq!(exit, 0, "{trading}");
    let trading_id = trading["id"].as_str().expect("id").to_owned();
    assert_eq!(trading["code"]["argv"], json!([runner, "{script}"]));
    assert_eq!(trading["code"]["source"], format!("{order_line}\n"));
    assert_eq!(trading["code"]["source_bytes"], json!(order_line.len() + 1));
    assert_eq!(
        trading["code"]["suffix"], ".txt",
        "--suffix defaults to the file's extension (§9)"
    );
    assert_eq!(
        trading["code"]["timeout_secs"], 300,
        "the daemon's default (§4.1)"
    );
    assert_eq!(
        trading["code"]["approved_at_ns"], trading["created_at_ns"],
        "R2's fixed Always allow (§4.1)"
    );
    assert!(
        trading["code"]["fingerprint"]
            .as_str()
            .is_some_and(|f| f.len() == 64
                && f.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())),
        "the fingerprint is lowercase hex SHA-256 (§4.1): {trading}"
    );

    let trading_firing = await_firing(&g, &trading_id, 0);
    await_execution(&g, &trading_firing, Duration::from_secs(15));
    let (exit, ran) = g.cli_json(
        "G22",
        &["--json", "trigger", "firing", &gamma, &trading_firing],
    );
    assert_eq!(exit, 0, "{ran}");
    assert_eq!(ran["trigger_id"], trading_id.as_str());
    assert_eq!(ran["execution"]["state"], "COMPLETE");
    assert_eq!(ran["execution"]["outcome"], "EXITED");
    assert_eq!(ran["execution"]["exit_code"], 0);
    assert_eq!(ran["execution"]["stdout_truncated"], false);
    assert_eq!(
        ran["execution"]["daemon_uuid"],
        endpoint.daemon_uuid.as_str()
    );
    let answers: Vec<Value> = ran["execution"]["stdout"]
        .as_str()
        .expect("the captured standard output")
        .lines()
        .map(parse)
        .collect();
    assert_eq!(answers.len(), 2, "one JSON line per call (§10.1): {ran}");
    for (call, answer) in answers.iter().enumerate() {
        assert_eq!(answer["action_id"], trading_firing.as_str(), "call {call}");
        assert_eq!(answer["kind"], "SUBMIT", "call {call}");
        assert_eq!(answer["source"], "TRIGGER", "call {call}");
        assert_eq!(answer["trigger_id"], trading_id.as_str(), "call {call}");
        assert_eq!(answer["firing_id"], trading_firing.as_str(), "call {call}");
        assert_eq!(answer["outcome"]["status"], "FILLED", "call {call}");
        assert_eq!(answer["outcome"]["filled_quantity"], "1", "call {call}");
    }
    assert_eq!(
        answers[0]["id"], answers[1]["id"],
        "the second call replayed the first's stored record (R1 feature SPEC §6)"
    );

    assert_eq!(
        g.scalar::<i64>(
            "SELECT count(*) FROM trading_actions WHERE desk_id = ?1",
            &[&gamma_id]
        ),
        1,
        "two calls, one action: the firing id is the action id (§10.1)"
    );
    let attributed: (String, Option<String>, Option<String>) = g
        .db()
        .query_row(
            "SELECT source, trigger_id, firing_id FROM trading_actions \
             WHERE desk_id = ?1 AND action_id = ?2",
            [&gamma_id, &trading_firing],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the attributed row");
    assert_eq!(
        (
            attributed.0.as_str(),
            attributed.1.as_deref(),
            attributed.2.as_deref()
        ),
        (
            "TRIGGER",
            Some(trading_id.as_str()),
            Some(trading_firing.as_str())
        ),
        "the row names the firing that placed it (§6)"
    );

    let (status, held) = g.api(
        "G22",
        &endpoint,
        "GET",
        &format!("/desks/{gamma_id}/positions"),
        None,
    );
    assert_eq!(status, 200, "{held}");
    assert_eq!(held["positions"].as_array().map(Vec::len), Some(1));
    assert_eq!(held["positions"][0]["instrument_id"], "AAPL.XNAS");
    assert_eq!(held["positions"][0]["quantity"], "1");

    let result = result_prompts(&g, &gamma_id, &trading_firing);
    assert_eq!(result.len(), 1, "one firing, one prompt (§5)");
    let (exit, summary) = g.cli_json("G22", &["--json", "prompt", "show", &gamma, &result[0]]);
    assert_eq!(exit, 0, "{summary}");
    // R3: with both runtimes UNDISCOVERED the dispatcher fails the queued row
    // RUNTIME_UNAVAILABLE as soon as it wakes; either state proves the insert.
    assert!(
        matches!(summary["state"].as_str(), Some("QUEUED" | "FAILED")),
        "{}",
        summary
    );
    assert_eq!(summary["payload"]["firing_id"], trading_firing.as_str());
    assert_eq!(summary["payload"]["trigger_id"], trading_id.as_str());
    assert_eq!(summary["payload"]["execution"]["outcome"], "EXITED");
    assert_eq!(summary["payload"]["execution"]["exit_code"], 0);
    assert_eq!(summary["payload"]["execution"]["stdout_truncated"], false);
    assert!(
        summary["payload"]["execution"]["stdout_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "{summary}"
    );

    // No session ever existed for this desk. R2 carries no sessions table and no
    // `SESSION_*` event kind, so the durable statements are that the desk owns
    // no session-sourced action and no event naming one.
    assert_eq!(
        g.scalar::<i64>(
            "SELECT count(*) FROM trading_actions WHERE desk_id = ?1 AND source = 'SESSION'",
            &[&gamma_id]
        ),
        0
    );
    let gamma_kinds = g.kinds_for(&gamma_id);
    assert!(
        !gamma_kinds.iter().any(|kind| kind.starts_with("SESSION")),
        "no session ever ran on this desk: {gamma_kinds:?}"
    );
    g.note(
        "G22",
        "a scheduled script with no agent alive placed one attributable, idempotent paper order and left one lot, one action, and one queued result",
        json!({ "firing": trading_firing, "execution": ran["execution"], "record": answers[0], "positions": held["positions"] }),
    );

    // --- G23 — the environment and the document -----------------------------
    let env_script = script(&g, "g23-env", "env");
    let brief = "report the environment the daemon handed the child";
    let context = "G23's context, carried into the document verbatim";
    let at = format!("{}Z", marketrig_acceptance::utc(now() + 2));
    let (exit, probe) = g.cli_json(
        "G23",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g23-env",
            "--brief",
            brief,
            "--context",
            context,
            "--at",
            &at,
            "--code",
            &env_script,
            "--arg",
            &runner,
            "--arg",
            "{script}",
        ],
    );
    assert_eq!(exit, 0, "{probe}");
    let probe_id = probe["id"].as_str().expect("id").to_owned();
    let probe_firing = await_firing(&g, &probe_id, 0);
    await_execution(&g, &probe_firing, Duration::from_secs(15));
    let (_, reported) = g.cli_json(
        "G23",
        &["--json", "trigger", "firing", &gamma, &probe_firing],
    );
    assert_eq!(reported["execution"]["outcome"], "EXITED");
    assert_eq!(reported["execution"]["exit_code"], 0);
    let seen = parse(
        reported["execution"]["stdout"]
            .as_str()
            .expect("the captured standard output")
            .trim(),
    );

    let (row_desk_id, row_desk_name, row_workspace): (String, String, String) = g
        .db()
        .query_row(
            "SELECT id, name, workspace_path FROM desks WHERE id = ?1",
            [&gamma_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("gamma's desk row");
    assert_eq!(seen["MARKETRIG_DESK_ID"], row_desk_id.as_str());
    assert_eq!(seen["MARKETRIG_DESK_NAME"], row_desk_name.as_str());
    assert_eq!(seen["MARKETRIG_TRIGGER_ID"], probe_id.as_str());
    assert_eq!(seen["MARKETRIG_FIRING_ID"], probe_firing.as_str());
    // Canonicalized on both sides: the evidence root is under a symlinked
    // temporary path on macOS, and the child reports where it actually is.
    let reported_cwd =
        fs::canonicalize(seen["cwd"].as_str().expect("cwd")).expect("the child's cwd");
    let workspace = fs::canonicalize(g.workspace(&gamma)).expect("the desk workspace");
    assert_eq!(
        reported_cwd, workspace,
        "the child runs in the desk workspace (§4.3)"
    );
    assert_eq!(seen["document"]["version"], 1);
    assert_eq!(seen["document"]["firing"]["id"], probe_firing.as_str());
    assert_eq!(seen["document"]["trigger"]["id"], probe_id.as_str());
    assert_eq!(seen["document"]["trigger"]["name"], "g23-env");
    assert_eq!(seen["document"]["trigger"]["recurrence"], "ONE_OFF");
    assert_eq!(seen["document"]["desk"]["id"], row_desk_id.as_str());
    assert_eq!(seen["document"]["desk"]["name"], row_desk_name.as_str());
    assert_eq!(
        seen["document"]["desk"]["workspace_path"],
        row_workspace.as_str()
    );
    assert_eq!(seen["document"]["brief"], brief);
    assert_eq!(seen["document"]["context"], context);

    // A patched brief reaches the next document; the firing already accepted
    // keeps the brief it was accepted with (§3.2 snapshots into the firing).
    let first_brief = "the brief the first firing snapshotted";
    let second_brief = "the brief the patch installed";
    let cycle_start = now() + 2;
    let (exit, cycle) = g.cli_json(
        "G23",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g23-cycle",
            "--brief",
            first_brief,
            "--rrule",
            "FREQ=MINUTELY",
            "--dtstart",
            &marketrig_acceptance::utc(cycle_start),
            "--tz",
            "UTC",
            "--code",
            &env_script,
            "--arg",
            &runner,
            "--arg",
            "{script}",
        ],
    );
    assert_eq!(exit, 0, "{cycle}");
    let cycle_id = cycle["id"].as_str().expect("id").to_owned();
    let cycle_first = await_firing(&g, &cycle_id, 0);
    await_execution(&g, &cycle_first, Duration::from_secs(15));
    let (exit, patched) = g.cli_json(
        "G23",
        &[
            "--json",
            "trigger",
            "update",
            &gamma,
            "g23-cycle",
            "--brief",
            second_brief,
        ],
    );
    assert_eq!(exit, 0, "{patched}");
    assert_eq!(patched["brief"], second_brief);
    assert_eq!(
        patched["revision"], 2,
        "every patch bumps the revision (§8)"
    );
    assert_eq!(
        patched["next_occurrence_ns"],
        json!(cycle_start * ns + 60 * ns),
        "a patch re-projects from the definition's own anchor (§2)"
    );

    // The next candidate is a minute after the first occurrence; awaiting the
    // second firing is bounded at 75 s.
    let cycle_second = await_firing(&g, &cycle_id, 1);
    await_execution(&g, &cycle_second, Duration::from_secs(15));
    let (_, second) = g.cli_json(
        "G23",
        &["--json", "trigger", "firing", &gamma, &cycle_second],
    );
    let second_seen = parse(
        second["execution"]["stdout"]
            .as_str()
            .expect("the captured standard output")
            .trim(),
    );
    assert_eq!(second_seen["document"]["brief"], second_brief);
    assert_eq!(second_seen["document"]["trigger"]["revision"], 2);
    assert_eq!(
        g.scalar::<String>("SELECT brief FROM firings WHERE id = ?1", &[&cycle_first]),
        first_brief,
        "the earlier firing keeps the brief it was accepted with"
    );

    // Disabled here so the rule stops firing: G24 onwards reason about one desk
    // and this one would otherwise keep claiming the executor every minute.
    let (exit, stopped) = g.cli_json(
        "G23",
        &["--json", "trigger", "disable", &gamma, "g23-cycle"],
    );
    assert_eq!(exit, 0, "{stopped}");
    g.note(
        "G23",
        "the child saw the four identifiers, the desk workspace, and a version-1 document; a patched brief reached the next document while the earlier firing kept its own",
        json!({ "environment": seen, "first": first_brief, "second": second_seen["document"]["brief"] }),
    );

    // --- G24 — every outcome is one record and one prompt --------------------
    // One instant for all four: they queue in acceptance order and the executor
    // runs them one at a time per desk (§4.3), which is what makes the pass
    // below deterministic.
    let outcomes = now() + 4;
    let at = format!("{}Z", marketrig_acceptance::utc(outcomes));
    let exit_script = script(&g, "g24-exit", "exit 3");
    let sleep_script = script(&g, "g24-sleep", "sleep 30");
    let flood_script = script(&g, "g24-flood", "flood 2000000");
    let spawn_script = script(&g, "g24-spawn", "exit 0");
    // The argv[0] no platform can launch (§4.4 `SPAWN_FAILED`).
    const MISSING: &str = "/nonexistent/marketrig-no-such-binary";
    let plans: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "g24-exit",
            "EXITED",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g24-exit",
                "--brief",
                "a nonzero exit",
                "--at",
                &at,
                "--code",
                &exit_script,
                "--arg",
                &runner,
                "--arg",
                "{script}",
            ],
        ),
        (
            "g24-timeout",
            "TIMED_OUT",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g24-timeout",
                "--brief",
                "a run past its timeout",
                "--at",
                &at,
                "--code",
                &sleep_script,
                "--arg",
                &runner,
                "--arg",
                "{script}",
                "--timeout",
                "1",
            ],
        ),
        (
            "g24-flood",
            "OUTPUT_LIMIT",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g24-flood",
                "--brief",
                "a run past the stdout cap",
                "--at",
                &at,
                "--code",
                &flood_script,
                "--arg",
                &runner,
                "--arg",
                "{script}",
            ],
        ),
        (
            "g24-spawn",
            "SPAWN_FAILED",
            vec![
                "--json",
                "trigger",
                "create",
                &gamma,
                "--name",
                "g24-spawn",
                "--brief",
                "a launch that cannot happen",
                "--at",
                &at,
                "--code",
                &spawn_script,
                "--arg",
                MISSING,
                "--arg",
                "{script}",
            ],
        ),
    ];
    let mut runs: Vec<(&str, &str, String, String)> = Vec::new();
    for (name, outcome, args) in &plans {
        let (exit, created) = g.cli_json("G24", args);
        assert_eq!(exit, 0, "{created}");
        runs.push((
            name,
            outcome,
            created["id"].as_str().expect("id").to_owned(),
            String::new(),
        ));
    }
    assert!(
        now() < outcomes,
        "the four creations must land before their shared instant"
    );
    for run in runs.iter_mut() {
        run.3 = await_firing(&g, &run.2, 0);
        // 25 s covers the whole serial pass: one 1-second timeout, one 1 MiB
        // flood, and two immediate ends, each spawned after the one before.
        await_execution(&g, &run.3, Duration::from_secs(25));
    }

    let mut settled = Vec::new();
    for (name, outcome, trigger_id, firing_id) in &runs {
        // Read straight off the route rather than through `marketrig trigger
        // firing`: the flood's capture is a megabyte, and `cli_json` would put
        // every byte of it into the observations file.
        let (status, shown) = g.call(
            &endpoint,
            "GET",
            &format!("/desks/{gamma_id}/firings/{firing_id}"),
            None,
        );
        assert_eq!(status, 200, "{name}: {shown}");
        let execution = shown["execution"].clone();
        assert_eq!(execution["outcome"], *outcome, "{name}: {shown}");
        match *outcome {
            "EXITED" => {
                assert_eq!(execution["exit_code"], 3, "{name}");
                assert!(
                    execution["stderr"]
                        .as_str()
                        .is_some_and(|e| e.contains("trigger-code exiting 3")),
                    "{name}: {execution}"
                );
            }
            "TIMED_OUT" => {
                assert!(execution.get("exit_code").is_none(), "{name}: {execution}");
                let ran_for = execution["finished_at_ns"].as_i64().unwrap_or_default()
                    - execution["started_at_ns"].as_i64().unwrap_or_default();
                assert!(
                    ran_for < 10 * ns,
                    "a one-second timeout ends the run promptly, not after the script's 30 s: {ran_for} ns"
                );
            }
            "OUTPUT_LIMIT" => {
                assert_eq!(execution["stdout_truncated"], true, "{name}: {execution}");
                assert_eq!(
                    execution["stdout_bytes"],
                    json!(1024 * 1024),
                    "the §4.3 cap"
                );
                assert!(
                    execution["error"]
                        .as_str()
                        .is_some_and(|e| e.contains("stdout")),
                    "{execution}"
                );
            }
            _ => {
                assert!(execution.get("exit_code").is_none(), "{name}: {execution}");
                assert_eq!(execution["executable"], MISSING, "{name}: {execution}");
                assert!(
                    execution["error"].as_str().is_some_and(|e| !e.is_empty()),
                    "{execution}"
                );
            }
        }
        assert_eq!(
            g.scalar::<i64>(
                "SELECT count(*) FROM executions WHERE firing_id = ?1",
                &[firing_id]
            ),
            1,
            "{name}: one record"
        );
        assert_eq!(
            result_prompts(&g, &gamma_id, firing_id).len(),
            1,
            "{name}: one prompt"
        );
        assert_eq!(
            projected(&g, trigger_id),
            None,
            "{name}: a consumed one-off"
        );
        settled.push(json!({
            "trigger": name,
            "outcome": execution["outcome"],
            "exit_code": execution["exit_code"],
            "error": execution["error"],
            "executable": execution["executable"],
            "stdout_bytes": execution["stdout_bytes"],
            "stdout_truncated": execution["stdout_truncated"],
            "stderr_bytes": execution["stderr_bytes"],
        }));
    }

    // Nothing reruns: the `executions` row is the at-most-once fact (§4.3).
    std::thread::sleep(Duration::from_secs(5));
    for (name, _, _, firing_id) in &runs {
        assert_eq!(
            g.scalar::<i64>(
                "SELECT count(*) FROM executions WHERE firing_id = ?1",
                &[firing_id]
            ),
            1,
            "{name}: still one record five seconds on"
        );
        assert_eq!(
            result_prompts(&g, &gamma_id, firing_id).len(),
            1,
            "{name}: still one prompt"
        );
    }
    g.note(
        "G24",
        "four outcomes, each exactly one execution record and one queued prompt, and nothing rerun",
        json!({ "executions": settled }),
    );

    // --- G25 — misses across downtime ---------------------------------------
    // Twelve seconds ahead: `POST /quit` is bounded at 5 s (root §4.6) and the
    // harness allows 8 for the process to go, so both candidates are still
    // ahead when the daemon is gone. Passing them while nothing runs is what
    // makes them misses rather than late acceptances (§3.2).
    let lapse = now() + 12;
    let lapse_at = format!("{}Z", marketrig_acceptance::utc(lapse));
    let lapse_dtstart = marketrig_acceptance::utc(lapse);
    let (exit, missed_once) = g.cli_json(
        "G25",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g25-once",
            "--brief",
            "due while the daemon is down",
            "--at",
            &lapse_at,
        ],
    );
    assert_eq!(exit, 0, "{missed_once}");
    let missed_once_id = missed_once["id"].as_str().expect("id").to_owned();
    let (exit, missed_every) = g.cli_json(
        "G25",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g25-every",
            "--brief",
            "a rule due while the daemon is down",
            "--rrule",
            "FREQ=MINUTELY",
            "--dtstart",
            &lapse_dtstart,
            "--tz",
            "UTC",
        ],
    );
    assert_eq!(exit, 0, "{missed_every}");
    let missed_every_id = missed_every["id"].as_str().expect("id").to_owned();
    assert_eq!(projected(&g, &missed_once_id), Some(lapse * ns));
    assert_eq!(projected(&g, &missed_every_id), Some(lapse * ns));

    g.stop("G25", daemon8);
    assert!(
        now() < lapse,
        "the daemon outlived the candidates it was meant to miss"
    );
    within(
        Duration::from_secs(30),
        "the missed instant to pass",
        || now() > lapse,
    );
    let daemon9 = g.spawn("G25");
    endpoint = daemon9.endpoint.clone();

    let missed = |g: &Harness, trigger_id: &str| -> Vec<Value> {
        g.events()
            .into_iter()
            .filter(|event| {
                event.kind == "TRIGGER_MISSED" && event.payload["trigger_id"] == trigger_id
            })
            .map(|event| event.payload)
            .collect()
    };
    within(Duration::from_secs(10), "both misses recorded", || {
        missed(&g, &missed_once_id).len() == 1 && missed(&g, &missed_every_id).len() == 1
    });
    for (trigger_id, name, recurrence) in [
        (&missed_once_id, "g25-once", "ONE_OFF"),
        (&missed_every_id, "g25-every", "RECURRING"),
    ] {
        let payload = missed(&g, trigger_id).remove(0);
        assert_eq!(payload["name"], name);
        assert_eq!(payload["recurrence"], recurrence);
        assert_eq!(
            payload["missed_from_ns"],
            json!(lapse * ns),
            "the old projection (§3.2)"
        );
        assert_eq!(payload["count"], 1);
        assert_eq!(payload["count_capped"], false);
        assert!(
            payload["missed_through_ns"].as_i64().unwrap_or_default() > lapse * ns,
            "{payload}"
        );
        assert_eq!(
            firings(&g, trigger_id),
            0,
            "{name}: a miss is never a catch-up firing"
        );
    }
    assert_eq!(projected(&g, &missed_once_id), None);
    let (exit, enabled) = g.cli_json("G25", &["--json", "trigger", "enable", &gamma, "g25-once"]);
    assert_eq!(exit, 0, "{enabled}");
    assert_eq!(enabled["enabled"], true);
    assert!(
        enabled.get("next_occurrence_ns").is_none(),
        "an elapsed one-off stays never due after enable (§3.1): {enabled}"
    );
    assert_eq!(projected(&g, &missed_once_id), None);
    let advanced = projected(&g, &missed_every_id).expect("the rule is due again");
    assert!(
        advanced > now() * ns,
        "the rule's projection is ahead: {advanced}"
    );
    let (status, unchanged) = g.api(
        "G25",
        &endpoint,
        "GET",
        &trigger_route(&missed_every_id),
        None,
    );
    assert_eq!(status, 200, "{unchanged}");
    assert_eq!(
        unchanged["schedule"],
        json!({ "rrule": "FREQ=MINUTELY", "dtstart": lapse_dtstart, "tz": "UTC" }),
        "the anchor a miss leaves alone (§3.3)"
    );
    let (exit, quiet) = g.cli_json(
        "G25",
        &["--json", "trigger", "disable", &gamma, "g25-every"],
    );
    assert_eq!(exit, 0, "{quiet}");
    g.note(
        "G25",
        "a one-off and a rule both due during downtime became one miss each with no firing, the one-off stayed consumed through enable, and the rule kept its anchor",
        json!({ "one_off": missed(&g, &missed_once_id), "recurring": missed(&g, &missed_every_id) }),
    );

    // --- G26 — a restart mid-flight -----------------------------------------
    let inflight_script = script(&g, "g26-inflight", "sleep 30");
    let at = format!("{}Z", marketrig_acceptance::utc(now() + 2));
    let (exit, inflight) = g.cli_json(
        "G26",
        &[
            "--json",
            "trigger",
            "create",
            &gamma,
            "--name",
            "g26-inflight",
            "--brief",
            "still running when the daemon dies",
            "--at",
            &at,
            "--code",
            &inflight_script,
            "--arg",
            &runner,
            "--arg",
            "{script}",
            "--timeout",
            "60",
        ],
    );
    assert_eq!(exit, 0, "{inflight}");
    let inflight_id = inflight["id"].as_str().expect("id").to_owned();
    let inflight_firing = await_firing(&g, &inflight_id, 0);
    within(
        Duration::from_secs(15),
        "the execution to be RUNNING",
        || {
            g.scalar::<i64>(
                "SELECT count(*) FROM executions WHERE firing_id = ?1 AND state = 'RUNNING'",
                &[&inflight_firing],
            ) == 1
        },
    );
    let before = g.scalar::<String>(
        "SELECT id || ' ' || trigger_id || ' ' || occurrence_ns || ' ' || accepted_at_ns \
         || ' ' || trigger_revision || ' ' || brief FROM firings WHERE id = ?1",
        &[&inflight_firing],
    );
    let dead = daemon9.endpoint.daemon_uuid.clone();
    let stale = daemon9.endpoint.clone();
    g.kill("G26", daemon9);
    g.await_unverifiable(&stale);
    let daemon10 = g.spawn("G26");
    endpoint = daemon10.endpoint.clone();

    let recoveries = g.recoveries();
    let recovery = recoveries.last().expect("a RECOVERY").clone();
    assert_eq!(recovery["daemon_uuid"], endpoint.daemon_uuid.as_str());
    assert_eq!(recovery["previous_daemon_uuid"], dead.as_str());
    assert_eq!(
        recovery["executions_lost"],
        json!([{ "firing_id": inflight_firing, "desk_id": gamma_id, "daemon_uuid": dead }]),
        "recovery names what the dead daemon was running (§4.4)"
    );
    let (exit, lost) = g.cli_json(
        "G26",
        &["--json", "trigger", "firing", &gamma, &inflight_firing],
    );
    assert_eq!(exit, 0, "{lost}");
    assert_eq!(lost["execution"]["state"], "COMPLETE");
    assert_eq!(lost["execution"]["outcome"], "DAEMON_LOST");
    assert_eq!(lost["execution"]["daemon_uuid"], dead.as_str());
    assert_eq!(lost["execution"]["error"], dead.as_str());
    assert!(lost["execution"].get("exit_code").is_none(), "{lost}");
    let queued = result_prompts(&g, &gamma_id, &inflight_firing);
    assert_eq!(
        queued.len(),
        1,
        "recovery queues the lost run's result (§4.4)"
    );
    let (_, settled_prompt) = g.cli_json("G26", &["--json", "prompt", "show", &gamma, &queued[0]]);
    // R3: with both runtimes UNDISCOVERED the dispatcher fails the queued row
    // RUNTIME_UNAVAILABLE as soon as it wakes; either state proves the insert.
    assert!(
        matches!(settled_prompt["state"].as_str(), Some("QUEUED" | "FAILED")),
        "{}",
        settled_prompt
    );
    assert_eq!(
        settled_prompt["payload"]["execution"]["outcome"],
        "DAEMON_LOST"
    );
    assert_eq!(
        g.scalar::<String>(
            "SELECT id || ' ' || trigger_id || ' ' || occurrence_ns || ' ' || accepted_at_ns \
             || ' ' || trigger_revision || ' ' || brief FROM firings WHERE id = ?1",
            &[&inflight_firing],
        ),
        before,
        "the firing row survived the kill untouched"
    );
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        g.scalar::<i64>(
            "SELECT count(*) FROM executions WHERE firing_id = ?1",
            &[&inflight_firing]
        ),
        1,
        "a lost run is settled, never re-attempted (§4.4)"
    );
    g.stop("G26", daemon10);
    g.note(
        "G26",
        "a hard kill mid-run left the firing intact, and recovery settled the execution DAEMON_LOST with its queued result and no second attempt",
        json!({ "recovery": recovery, "execution": lost["execution"] }),
    );

    let evidence = g.out.display().to_string();
    g.note("gate", "G1-G26 complete", json!({ "evidence": evidence }));
}
