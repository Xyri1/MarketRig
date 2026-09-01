//! The acceptance gate: scenarios G1–G20, in order, in one test.
//!
//! Contract: `sdd/features/r0-workspace-desk-identity/SPEC.md` §10 (G1–G11 and
//! the evidence bundle) and `sdd/features/r1-equity-paper-trading/SPEC.md` §10
//! (the stand-in feed and G12–G20), per D75, R0-7, R1-9. The harness drives
//! public surfaces only — the real binaries, `marketrig --json`, the loopback
//! API, the desk's MCP surface through the harness's own MCP client, workspace
//! files, and read-only SQLite. It never links `marketrigd` or `marketrig` as
//! libraries, so the R0 §7.6 seed, the R0 §5.1 endpoint shape, the R1 §7 element
//! shapes, and the chart-endpoint body are all re-stated here from the SPEC on
//! purpose.
//!
//! State carries across scenarios: the chain is one run against one data root,
//! which is also the evidence directory. G12 onwards trade on one desk the
//! earlier scenarios created, on the stand-in feed.

use std::fs::{self, File};
use std::process::{Command, Stdio};
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
                         AND p.kind = 'EVALUATION' AND p.state = 'QUEUED' \
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

    let evidence = g.out.display().to_string();
    g.note("gate", "G1-G20 complete", json!({ "evidence": evidence }));
}
