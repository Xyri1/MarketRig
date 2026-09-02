//! The acceptance experiment: E1, E2, and E3, the attended scenarios.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §10.3,
//! `sdd/features/r2-scheduled-triggers/SPEC.md` §10.3, and root `sdd/SPEC.md` §17,
//! per D75. One operator-attended run per platform-and-runtime cell, on real
//! Yahoo and a real runtime CLI, with MCP registration performed by hand (R1
//! keeps it operator-performed, feature SPEC §8).
//!
//! **Operator variable:** `MARKETRIG_EXPERIMENT` selects the cell — `codex` runs
//! E1 and E3, `claude` runs E2 and E3. Unset or anything else skips them all
//! cleanly, which is what CI and every unattended `cargo test` do. A cell's two
//! scenarios run one after the other on their own daemons, desks, and bundles:
//! they share the operator's terminal, so the harness serializes them. The
//! instructions are printed, so run the selected cell with output:
//!
//! ```text
//! MARKETRIG_EXPERIMENT=codex cargo test -p marketrig-acceptance --test experiment -- --nocapture
//! ```
//!
//! What fails and what does not (root §17): the mechanical legs — the daemon, the
//! desk, the real feed, and the shape of any row the session did produce — fail
//! the cell. The legs that wait on the agent to act end **inconclusive**, with
//! their evidence in the bundle, and the operator decides whether to rerun. The
//! session's two quote reads leave no durable trace at all (observations are
//! never persisted, feature SPEC §2.3), so that aspect is inconclusive by
//! construction and is recorded as such.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use marketrig_acceptance::{Harness, parse, waited};
use serde_json::json;

/// The operator variable and its two cells.
const CELL: &str = "MARKETRIG_EXPERIMENT";

/// A cell's scenarios share one operator, one terminal, and one pair of hands,
/// so they hold this in turn instead of running side by side the way `cargo
/// test` would otherwise start them.
static TERMINAL: Mutex<()> = Mutex::new(());

/// How long an attended cell waits on the operator and the agent before an
/// aspect ends inconclusive. The operator can stop the run sooner.
const PATIENCE: Duration = Duration::from_secs(900);

/// How long a durable consequence of an action the session already took may
/// take to land. This one is mechanical: the daemon writes it synchronously.
const SETTLES: Duration = Duration::from_secs(60);

#[test]
fn e1_codex_cli() {
    attended("E1", "codex", "Codex CLI");
}

#[test]
fn e2_claude_code() {
    attended("E2", "claude", "Claude Code");
}

fn attended(scenario: &str, cell: &str, runtime: &str) {
    if std::env::var(CELL).unwrap_or_default() != cell {
        eprintln!(
            "{scenario} ({runtime}) skipped: set {CELL}={cell} to run this cell attended, \
             and pass `-- --nocapture` so its instructions are visible."
        );
        return;
    }
    let _terminal = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);

    let mut g = Harness::new(&format!("experiment-{cell}"));
    // Real Yahoo and a real runtime: neither feed seam is set. The data root is
    // still relocated into the run's evidence directory (root §17).
    g.real_feed();
    let daemon = g.spawn(scenario);
    let endpoint = daemon.endpoint.clone();

    // Run-stamped, so a cell never collides with an earlier one (root §17).
    let desk = format!("{cell}-{}", marketrig_acceptance::now_secs());
    let (exit, created) = g.cli_json(scenario, &["--json", "desk", "create", &desk]);
    assert_eq!(exit, 0, "the cell's desk must be created: {created}");
    assert_eq!(created["state"], "READY", "{created}");
    let desk_id = created["id"].as_str().expect("id").to_owned();

    // Mechanical: the market plane answers on the real feed before an operator is
    // asked to do anything with it.
    let quotes = format!("/desks/{desk_id}/market/quotes");
    let (status, body) = g.api(scenario, &endpoint, "GET", &quotes, None);
    assert_eq!(status, 200, "{body}");
    assert!(
        waited(SETTLES, "a live observation from Yahoo", || {
            g.call(&endpoint, "GET", &quotes, None).1["quotes"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|quote| quote["health"] == "LIVE")
        }),
        "the real feed never produced an observation; this is a mechanical failure"
    );

    let instructions = format!(
        "\n\
         ===========================================================================\n\
         {scenario} — {runtime} on MarketRig's market plane (feature SPEC §10.3)\n\
         ===========================================================================\n\
         \n\
         Desk:      {desk}\n\
         Data root: {root}\n\
         Adapter:   {mcp}\n\
         Evidence:  {root}\n\
         \n\
         1. Register the adapter with {runtime} by hand — R1 keeps registration\n\
         \x20  operator-performed (feature SPEC §8):\n\
         \n\
         \x20      command: {mcp}\n\
         \x20      args:    [\"--desk\", \"{desk}\"]\n\
         \x20      env:     MARKETRIG_TEST_DATA_ROOT={root}\n\
         \n\
         \x20  Codex CLI:   codex mcp add marketrig --env MARKETRIG_TEST_DATA_ROOT={root} -- {mcp} --desk {desk}\n\
         \x20  Claude Code: claude mcp add-json marketrig '{{\"command\":\"{mcp}\",\"args\":[\"--desk\",\"{desk}\"],\"env\":{{\"MARKETRIG_TEST_DATA_ROOT\":\"{root}\"}}}}'\n\
         \n\
         2. Start a {runtime} session in that desk's workspace:\n\
         \x20      {workspace}\n\
         \n\
         3. Ask the session to read the desk's quote resource\n\
         \x20  (marketrig://desk/{desk}/quotes), wait a little, and read it again.\n\
         \x20  A second read must show a fresher observation — the adapter caches\n\
         \x20  nothing. Only you can see this: observations are never persisted, so\n\
         \x20  the harness records this aspect INCONCLUSIVE either way.\n\
         \n\
         4. Ask the session to submit one small paper order through the\n\
         \x20  `submit_order` tool — a LIMIT buy well below the last price, so it\n\
         \x20  rests instead of filling.\n\
         \n\
         5. Ask the session to cancel it through the `cancel_order` tool, naming\n\
         \x20  the client order id the submit answered with.\n\
         \n\
         The harness now watches the daemon's own durable rows and reports what it\n\
         sees. It waits up to {patience} minutes per step; stop it whenever you like.\n\
         ===========================================================================\n",
        root = g.out.display(),
        mcp = g.mcp.display(),
        workspace = g.workspace(&desk).display(),
        patience = PATIENCE.as_secs() / 60,
    );
    println!("{instructions}");
    g.write_evidence("instructions.txt", &instructions);
    g.note(
        scenario,
        "attended cell prepared; instructions issued to the operator",
        json!({ "desk": desk, "desk_id": desk_id, "adapter": g.mcp.display().to_string() }),
    );

    // The two quote reads leave no durable trace, by design (feature SPEC §2.3).
    g.inconclusive(
        scenario,
        "the session's two quote-resource reads are witnessed by the operator only: \
         observations are never persisted, so no side effect can carry them",
        json!({ "resource": format!("marketrig://desk/{desk}/quotes") }),
    );

    // --- Verified by side effects alone (root §17) --------------------------
    let submitted = waited(PATIENCE, "a submitted order from the session", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM trading_actions \
             WHERE desk_id = ?1 AND kind = 'SUBMIT' AND outcome IS NOT NULL",
            &[&desk_id],
        ) > 0
    });
    if !submitted {
        g.inconclusive(
            scenario,
            "no order was submitted through the tool within the cell's patience",
            json!({ "waited_secs": PATIENCE.as_secs() }),
        );
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }

    // The row exists, so its shape is mechanical (§5, §6).
    let (action_id, source, outcome): (String, String, String) = g
        .db()
        .query_row(
            "SELECT action_id, source, outcome FROM trading_actions \
             WHERE desk_id = ?1 AND kind = 'SUBMIT' AND outcome IS NOT NULL \
             ORDER BY created_at_ns DESC LIMIT 1",
            [&desk_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the submitted action's row");
    assert_eq!(source, "SESSION", "every R1 action is session-sourced (§6)");
    let outcome = parse(&outcome);
    assert_eq!(
        outcome["client_order_id"],
        action_id.as_str(),
        "the client order id is the submit's own action id (§4.2)"
    );
    g.note(
        scenario,
        "the session's order reached the daemon's durable record",
        json!({ "action_id": action_id, "outcome": outcome }),
    );

    // Its lifecycle is the sandbox's own, captured verbatim (§5).
    let lifecycle = |g: &Harness| -> Vec<String> {
        g.column(
            "SELECT kind FROM order_events WHERE desk_id = ?1 AND client_order_id = ?2 \
             ORDER BY occurred_at_ns, id",
            &[&desk_id, &action_id],
        )
    };
    assert!(
        waited(SETTLES, "the order's lifecycle events", || {
            lifecycle(&g).len() >= 2
        }),
        "an answered submit must have left its order events behind (§5)"
    );
    g.note(
        scenario,
        "the order's lifecycle, in the sandbox's own event names",
        json!({ "client_order_id": action_id, "events": lifecycle(&g) }),
    );

    let cancelled = waited(PATIENCE, "a cancel from the session", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM trading_actions \
             WHERE desk_id = ?1 AND kind = 'CANCEL' AND outcome IS NOT NULL",
            &[&desk_id],
        ) > 0
    });
    if cancelled {
        assert!(
            waited(SETTLES, "the cancelled order's terminal event", || {
                lifecycle(&g).iter().any(|kind| kind == "OrderCanceled")
            }),
            "an answered cancel must have left its OrderCanceled behind (§5)"
        );
        g.note(
            scenario,
            "the session cancelled its resting order and the sandbox closed it",
            json!({ "client_order_id": action_id, "events": lifecycle(&g) }),
        );
    } else {
        g.inconclusive(
            scenario,
            "the order was submitted but never cancelled within the cell's patience",
            json!({ "client_order_id": action_id, "waited_secs": PATIENCE.as_secs() }),
        );
    }

    finish(&mut g, scenario, daemon, &desk_id);
}

#[test]
fn e3_codex_cli() {
    scheduled("E3", "codex", "Codex CLI");
}

#[test]
fn e3_claude_code() {
    scheduled("E3", "claude", "Claude Code");
}

/// **E3 — a real session defines a trigger whose code trades** (R2 feature SPEC
/// §10.3). Same cell variable as E1/E2 and the same patience, on its own daemon,
/// desk, and bundle. What the session does is the agent's; what the daemon does
/// once a firing exists is mechanical and asserted.
fn scheduled(scenario: &str, cell: &str, runtime: &str) {
    if std::env::var(CELL).unwrap_or_default() != cell {
        eprintln!(
            "{scenario} ({runtime}) skipped: set {CELL}={cell} to run this cell attended, \
             and pass `-- --nocapture` so its instructions are visible."
        );
        return;
    }
    let _terminal = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);

    let mut g = Harness::new(&format!("experiment-e3-{cell}"));
    g.real_feed();
    let daemon = g.spawn(scenario);
    let endpoint = daemon.endpoint.clone();

    let desk = format!("{cell}-e3-{}", marketrig_acceptance::now_secs());
    let (exit, created) = g.cli_json(scenario, &["--json", "desk", "create", &desk]);
    assert_eq!(exit, 0, "the cell's desk must be created: {created}");
    assert_eq!(created["state"], "READY", "{created}");
    let desk_id = created["id"].as_str().expect("id").to_owned();

    // Mechanical, and it also picks the instrument the operator names: the
    // trigger's order should be for something the real feed is observing.
    let quotes = format!("/desks/{desk_id}/market/quotes");
    let (status, body) = g.api(scenario, &endpoint, "GET", &quotes, None);
    assert_eq!(status, 200, "{body}");
    let live = |g: &Harness| -> Option<String> {
        g.call(&endpoint, "GET", &quotes, None).1["quotes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|quote| quote["health"] == "LIVE")
            .and_then(|quote| quote["instrument_id"].as_str().map(str::to_owned))
    };
    assert!(
        waited(SETTLES, "a live observation from Yahoo", || {
            live(&g).is_some()
        }),
        "the real feed never produced an observation; this is a mechanical failure"
    );
    let instrument = live(&g).expect("a live instrument");

    // Two minutes: long enough for the session to write the file and issue the
    // command, short enough that the operator watches it fire.
    let at = format!(
        "{}Z",
        marketrig_acceptance::utc(marketrig_acceptance::now_secs() as i64 + 120)
    );
    let instructions = format!(
        "\n\
         ===========================================================================\n\
         {scenario} — {runtime} defines a trigger whose code trades (feature SPEC §10.3)\n\
         ===========================================================================\n\
         \n\
         Desk:         {desk}\n\
         Data root:    {root}\n\
         Adapter:      {mcp}\n\
         CLI:          {cli}\n\
         trigger-code: {runner}\n\
         Instrument:   {instrument}   (LIVE on the real feed right now)\n\
         Evidence:     {root}\n\
         \n\
         1. Register the adapter with {runtime} exactly as in E1/E2 — the trigger's\n\
         \x20  code reaches the same daemon through it:\n\
         \n\
         \x20  Codex CLI:   codex mcp add marketrig --env MARKETRIG_TEST_DATA_ROOT={root} -- {mcp} --desk {desk}\n\
         \x20  Claude Code: claude mcp add-json marketrig '{{\"command\":\"{mcp}\",\"args\":[\"--desk\",\"{desk}\"],\"env\":{{\"MARKETRIG_TEST_DATA_ROOT\":\"{root}\"}}}}'\n\
         \n\
         2. Start a {runtime} session in that desk's workspace:\n\
         \x20      {workspace}\n\
         \n\
         3. Ask the session to write a one-line script file in the workspace, say\n\
         \x20  `job.txt`, whose only content is:\n\
         \n\
         \x20      order {instrument} BUY 1\n\
         \n\
         4. Ask it to define a one-off trigger due in about two minutes that runs\n\
         \x20  that script through the trigger-code helper. The CLI needs the data\n\
         \x20  root in its environment, so give the session the whole line:\n\
         \n\
         \x20  macOS / Linux:\n\
         \x20      MARKETRIG_TEST_DATA_ROOT={root} \\\n\
         \x20        {cli} trigger create {desk} \\\n\
         \x20        --name e3-order --brief 'place one lot through a scheduled trigger' \\\n\
         \x20        --at {at} \\\n\
         \x20        --code job.txt --arg {runner} --arg '{{script}}'\n\
         \n\
         \x20  Windows PowerShell:\n\
         \x20      $env:MARKETRIG_TEST_DATA_ROOT = '{root}'\n\
         \x20      & '{cli}' trigger create {desk} --name e3-order --brief 'place one lot through a scheduled trigger' --at {at} --code job.txt --arg '{runner}' --arg '{{script}}'\n\
         \n\
         \x20  `--at` above is two minutes from when these instructions printed; if\n\
         \x20  the session takes longer, have it pick a fresh instant a couple of\n\
         \x20  minutes ahead in the same RFC 3339 UTC form.\n\
         \n\
         5. Nothing else. No session need be alive when it fires: the daemon runs\n\
         \x20  the code itself, the order is attributed to the firing, and the\n\
         \x20  result is queued back as a TRIGGER_RESULT prompt. Have the session\n\
         \x20  read it afterwards with:\n\
         \n\
         \x20      MARKETRIG_TEST_DATA_ROOT={root} {cli} prompt list {desk}\n\
         \n\
         The harness now watches the daemon's own durable rows. It waits up to\n\
         {patience} minutes per step; stop it whenever you like.\n\
         ===========================================================================\n",
        root = g.out.display(),
        mcp = g.mcp.display(),
        cli = g.cli.display(),
        runner = g.trigger_code.display(),
        workspace = g.workspace(&desk).display(),
        patience = PATIENCE.as_secs() / 60,
    );
    println!("{instructions}");
    g.write_evidence("instructions-e3.txt", &instructions);
    g.note(
        scenario,
        "attended cell prepared; instructions issued to the operator",
        json!({
            "desk": desk, "desk_id": desk_id, "instrument": instrument,
            "adapter": g.mcp.display().to_string(),
            "trigger_code": g.trigger_code.display().to_string(),
        }),
    );

    // --- The session's own step ---------------------------------------------
    let defined = waited(PATIENCE, "a trigger defined by the session", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM triggers WHERE desk_id = ?1",
            &[&desk_id],
        ) > 0
    });
    if !defined {
        g.inconclusive(
            scenario,
            "the session defined no trigger within the cell's patience",
            json!({ "waited_secs": PATIENCE.as_secs() }),
        );
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    let (trigger_id, trigger_name, schedule): (String, String, String) = g
        .db()
        .query_row(
            "SELECT id, name, coalesce(at_ns, 0) || ' ' || coalesce(rrule, '') FROM triggers \
             WHERE desk_id = ?1 ORDER BY created_at_ns DESC LIMIT 1",
            [&desk_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the session's trigger row");
    g.note(
        scenario,
        "the session's trigger reached the daemon's durable rows",
        json!({ "trigger_id": trigger_id, "name": trigger_name, "schedule": schedule }),
    );

    // The schedule is the session's, so waiting on the firing still waits on it.
    let fired = waited(PATIENCE, "the trigger's first firing", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM firings WHERE trigger_id = ?1",
            &[&trigger_id],
        ) > 0
    });
    if !fired {
        g.inconclusive(
            scenario,
            "the session's trigger never came due within the cell's patience",
            json!({ "trigger_id": trigger_id, "waited_secs": PATIENCE.as_secs() }),
        );
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    let (firing_id, code_snapshot_id): (String, Option<String>) = g
        .db()
        .query_row(
            "SELECT id, code_snapshot_id FROM firings WHERE trigger_id = ?1 \
             ORDER BY accepted_at_ns, id LIMIT 1",
            [&trigger_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the firing row");
    g.note(
        scenario,
        "the daemon accepted the occurrence with no session in the loop",
        json!({ "firing_id": firing_id, "code_snapshot_id": code_snapshot_id }),
    );

    if code_snapshot_id.is_none() {
        g.inconclusive(
            scenario,
            "the session's trigger carried no code, so nothing ran: E3 needs `--code`",
            json!({ "trigger_id": trigger_id, "firing_id": firing_id }),
        );
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }

    // --- Mechanical from here: the firing exists, so the daemon owns the rest -
    assert!(
        waited(SETTLES, "the execution to complete", || {
            g.scalar::<i64>(
                "SELECT count(*) FROM executions WHERE firing_id = ?1 AND state = 'COMPLETE'",
                &[&firing_id],
            ) == 1
        }),
        "a code-bearing firing must leave exactly one completed execution (§4.3, §4.4)"
    );
    let (outcome, exit_code, stdout): (String, Option<i64>, Option<Vec<u8>>) = g
        .db()
        .query_row(
            "SELECT outcome, exit_code, stdout FROM executions WHERE firing_id = ?1",
            [&firing_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the execution row");
    let captured = String::from_utf8_lossy(&stdout.unwrap_or_default()).into_owned();
    g.note(
        scenario,
        "the daemon ran the session's code and recorded one outcome",
        json!({ "outcome": outcome, "exit_code": exit_code, "stdout": captured }),
    );

    // The prompt is the daemon's own half of the loop, queued in the same unit.
    let queued: Vec<String> = g.column(
        "SELECT state FROM prompts WHERE desk_id = ?1 AND kind = 'TRIGGER_RESULT' \
         AND payload LIKE '%' || ?2 || '%'",
        &[&desk_id, &firing_id],
    );
    // R3: the dispatcher may already have failed it RUNTIME_UNAVAILABLE (the
    // experiment discovers no runtime before E4); the row itself is the evidence.
    assert_eq!(
        queued.len(),
        1,
        "one TRIGGER_RESULT per completed execution (§5)"
    );
    assert!(
        matches!(queued[0].as_str(), "QUEUED" | "FAILED"),
        "{queued:?}"
    );

    // Whether the code placed an order is the session's script; the attribution
    // on the row, once there is one, is the daemon's.
    let placed = waited(SETTLES, "an order attributed to the firing", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM trading_actions WHERE desk_id = ?1 AND firing_id = ?2",
            &[&desk_id, &firing_id],
        ) > 0
    });
    if !placed {
        g.inconclusive(
            scenario,
            "the trigger's code placed no order attributed to its firing",
            json!({ "firing_id": firing_id, "outcome": outcome, "stdout": captured }),
        );
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    let (action_id, source, action_trigger, action_outcome): (
        String,
        String,
        Option<String>,
        Option<String>,
    ) = g
        .db()
        .query_row(
            "SELECT action_id, source, trigger_id, outcome FROM trading_actions \
             WHERE desk_id = ?1 AND firing_id = ?2 ORDER BY created_at_ns LIMIT 1",
            [&desk_id, &firing_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the attributed action row");
    assert_eq!(
        source, "TRIGGER",
        "a firing-attributed action is TRIGGER-sourced (§6)"
    );
    assert_eq!(
        action_trigger.as_deref(),
        Some(trigger_id.as_str()),
        "the row names the trigger the firing belongs to (§6)"
    );
    g.note(
        scenario,
        "the scheduled code placed an attributable paper action with no agent alive",
        json!({
            "action_id": action_id, "source": source, "trigger_id": action_trigger,
            "firing_id": firing_id,
            "outcome": action_outcome.as_deref().map(parse),
        }),
    );

    finish(&mut g, scenario, daemon, &desk_id);
}

/// Closes the cell: the daemon stops cleanly and the bundle names itself.
fn finish(g: &mut Harness, scenario: &str, daemon: marketrig_acceptance::Daemon, desk_id: &str) {
    let actions = g.column(
        "SELECT kind || ' ' || action_id FROM trading_actions WHERE desk_id = ?1 \
         ORDER BY created_at_ns",
        &[&desk_id],
    );
    g.stop(scenario, daemon);
    let evidence = g.out.display().to_string();
    println!("{scenario} evidence bundle: {evidence}");
    g.note(
        scenario,
        "attended cell complete",
        json!({ "evidence": evidence, "actions": actions }),
    );
}

// ---------------------------------------------------------------------------
// E4 — the runtime plane, attended (R3 feature SPEC §9.3)
// ---------------------------------------------------------------------------

#[test]
fn e4_codex_cli() {
    delivery("E4", "codex", "Codex CLI", "claude");
}

#[test]
fn e4_claude_code() {
    delivery("E4", "claude", "Claude Code", "codex");
}

/// **E4 — MarketRig starts the real runtime and delivers to it** (R3 feature
/// SPEC §9.3). The operator's own console *is* the desk's terminal for the whole
/// cell — raw, with the window size relayed — so the trust and channel
/// confirmations the real CLI asks for are answered by hand. What the daemon
/// records is mechanical and asserted; what the agent does is inconclusive.
fn delivery(scenario: &str, cell: &str, runtime: &str, other: &str) {
    if std::env::var(CELL).unwrap_or_default() != cell {
        eprintln!(
            "{scenario} ({runtime}) skipped: set {CELL}={cell} to run this cell attended, \
             and pass `-- --nocapture` so its instructions are visible."
        );
        return;
    }
    let _terminal = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);

    let mut g = Harness::new(&format!("experiment-e4-{cell}"));
    g.real_feed();
    let daemon = g.spawn(scenario);
    let endpoint = daemon.endpoint.clone();

    // Startup discovery is skipped under the test seam (§2), so the cell asks
    // for it: the operator's own installation, resolved from the login PATH.
    let (status, row) = g.api(
        scenario,
        &endpoint,
        "POST",
        &format!("/runtimes/{cell}/discover"),
        Some("{}"),
    );
    assert_eq!(status, 200, "{row}");
    assert_eq!(
        row["state"], "AVAILABLE",
        "the cell's runtime must be installed and discoverable: {row}"
    );

    let desk = format!("{cell}-e4-{}", marketrig_acceptance::now_secs());
    let (status, created) = g.api(
        scenario,
        &endpoint,
        "POST",
        "/desks",
        Some(&json!({ "name": desk, "runtime": cell }).to_string()),
    );
    assert_eq!(status, 201, "{created}");
    let desk_id = created["id"].as_str().expect("id").to_owned();

    let at = format!(
        "{}Z",
        marketrig_acceptance::utc(marketrig_acceptance::now_secs() as i64 + 120)
    );
    let instructions = format!(
        "\n\
         ===========================================================================\n\
         {scenario} — MarketRig starts {runtime} and delivers to it (feature SPEC §9.3)\n\
         ===========================================================================\n\
         \n\
         Desk:      {desk}\n\
         Runtime:   {runtime} {version} at {path}\n\
         Data root: {root}\n\
         Evidence:  {root}\n\
         \n\
         Nothing to register and nothing to start: MarketRig launches the runtime\n\
         itself, in the desk's workspace, with the adapter already registered.\n\
         \n\
         1. This console becomes the desk's terminal in a moment. Everything you\n\
         \x20  type goes to the session; everything it prints appears here.\n\
         \n\
         2. Answer whatever {runtime} asks on first launch — trust, permissions,\n\
         \x20  the development channel — and nothing else. Do not send the session\n\
         \x20  work of your own: the point of the cell is what MarketRig delivers.\n\
         \n\
         3. A one-off trigger is due at {at} (about two minutes out). When it\n\
         \x20  fires you must see its result arrive as the session's own input,\n\
         \x20  after the orientation MarketRig sends first. Read it and say so.\n\
         \n\
         4. When the harness says so, keep the session busy — ask it something\n\
         \x20  that takes a moment — and a second trigger's result will be waiting\n\
         \x20  behind it, delivered when the turn ends.\n\
         \n\
         5. The harness then switches the desk to {other} and stops. The desk's\n\
         \x20  history and pointers must survive that.\n\
         \n\
         The harness waits up to {patience} minutes per step; ^C stops the run.\n\
         ===========================================================================\n",
        version = row["version"].as_str().unwrap_or_default(),
        path = row["executable_path"].as_str().unwrap_or_default(),
        root = g.out.display(),
        patience = PATIENCE.as_secs() / 60,
    );
    println!("{instructions}");
    g.write_evidence("instructions-e4.txt", &instructions);
    g.note(
        scenario,
        "attended cell prepared; the console is about to become the desk's terminal",
        json!({ "desk": desk, "desk_id": desk_id, "runtime": row }),
    );

    // The console is the terminal from here until the cell ends.
    let console = console::attach(&endpoint, &desk_id);

    let (exit, trigger) = g.cli_json(
        scenario,
        &[
            "--json",
            "trigger",
            "create",
            &desk,
            "--name",
            "e4-first",
            "--brief",
            "the first delivery",
            "--at",
            &at,
        ],
    );
    assert_eq!(exit, 0, "{trigger}");
    let first = trigger["id"].as_str().expect("id").to_owned();

    let started = waited(PATIENCE, "MarketRig to start the runtime", || {
        !kinds(&g, &desk_id, "SESSION_STARTED").is_empty()
    });
    if !started {
        g.inconclusive(
            scenario,
            "no session was started within the cell's patience",
            json!({ "waited_secs": PATIENCE.as_secs() }),
        );
        console.detach();
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    let activation = kinds(&g, &desk_id, "SESSION_STARTED").remove(0);
    assert_eq!(activation["runtime"], cell, "{activation}");
    g.note(
        scenario,
        "MarketRig started the runtime itself",
        json!({ "activation": activation }),
    );

    let ready = waited(PATIENCE, "the session to become ready", || {
        !kinds(&g, &desk_id, "SESSION_READY").is_empty()
    });
    if !ready {
        g.inconclusive(
            scenario,
            "the launch never reached readiness — the operator may not have answered its first-launch questions",
            json!({ "waited_secs": PATIENCE.as_secs() }),
        );
        console.detach();
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    g.note(
        scenario,
        "the launch reached readiness",
        json!({ "process": g.column(
            "SELECT id || ' ' || runtime || ' ' || coalesce(native_session_id, '-') \
             FROM agent_processes WHERE desk_id = ?1 ORDER BY started_at_ns",
            &[&desk_id],
        ) }),
    );

    // Mechanical from here: whatever the daemon says it delivered, its own rows
    // must agree with (root §17).
    let delivered = |g: &Harness| -> i64 {
        g.scalar(
            "SELECT count(*) FROM prompts WHERE desk_id = ?1 AND kind = 'TRIGGER_RESULT' \
             AND state = 'DELIVERED'",
            &[&desk_id],
        )
    };
    let fired = waited(
        PATIENCE,
        "the first trigger's result to be delivered",
        || delivered(&g) >= 1,
    );
    if !fired {
        g.inconclusive(
            scenario,
            "the first result was never delivered within the cell's patience",
            json!({ "trigger_id": first, "prompts": prompt_states(&g, &desk_id) }),
        );
        console.detach();
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    assert_delivery(&g, &desk_id);
    g.note(
        scenario,
        "the trigger fired with nobody home, MarketRig delivered its result to the session it started, and the rows agree",
        json!({ "prompts": prompt_states(&g, &desk_id) }),
    );
    g.inconclusive(
        scenario,
        "whether the result appeared as the session's own input is the operator's to confirm on the console",
        json!({ "expect": "MarketRig TRIGGER_RESULT <id>: followed by the firing's JSON" }),
    );

    // The second one, deliberately queued behind whatever the operator has the
    // session doing.
    println!(
        "\r\n{scenario}: give the session something to chew on now — the next result is due in two minutes.\r\n"
    );
    let at = format!(
        "{}Z",
        marketrig_acceptance::utc(marketrig_acceptance::now_secs() as i64 + 120)
    );
    let (exit, trigger) = g.cli_json(
        scenario,
        &[
            "--json",
            "trigger",
            "create",
            &desk,
            "--name",
            "e4-second",
            "--brief",
            "queued behind a turn",
            "--at",
            &at,
        ],
    );
    assert_eq!(exit, 0, "{trigger}");
    if waited(PATIENCE, "the second result to be delivered", || {
        delivered(&g) >= 2
    }) {
        assert_delivery(&g, &desk_id);
        g.note(
            scenario,
            "a second result was delivered while the session was in the operator's hands",
            json!({ "prompts": prompt_states(&g, &desk_id) }),
        );
    } else {
        g.inconclusive(
            scenario,
            "the second result was not delivered within the cell's patience",
            json!({ "prompts": prompt_states(&g, &desk_id) }),
        );
    }

    // The switch, and what it must not move.
    let before = (
        g.scalar::<i64>(
            "SELECT count(*) FROM firings f JOIN triggers t ON t.id = f.trigger_id \
             WHERE t.desk_id = ?1",
            &[&desk_id],
        ),
        g.scalar::<i64>(
            "SELECT count(*) FROM prompts WHERE desk_id = ?1",
            &[&desk_id],
        ),
        pointer(&g, &desk_id, cell),
    );
    let (status, other_row) = g.api(
        scenario,
        &endpoint,
        "POST",
        &format!("/runtimes/{other}/discover"),
        Some("{}"),
    );
    assert_eq!(status, 200, "{other_row}");
    if other_row["state"] == "AVAILABLE" {
        let (status, switched) = g.api(
            scenario,
            &endpoint,
            "POST",
            &format!("/desks/{desk_id}/session/switch"),
            Some(&json!({ "runtime": other }).to_string()),
        );
        assert_eq!(status, 200, "{switched}");
        assert_eq!(switched["selected_runtime"], other);
        assert_eq!(
            switched["pointers"][cell].as_str().map(str::to_owned),
            before.2,
            "a switch keeps the runtime's pointer (§7)"
        );
        let after = (
            g.scalar::<i64>(
                "SELECT count(*) FROM firings f JOIN triggers t ON t.id = f.trigger_id \
                 WHERE t.desk_id = ?1",
                &[&desk_id],
            ),
            g.scalar::<i64>(
                "SELECT count(*) FROM prompts WHERE desk_id = ?1",
                &[&desk_id],
            ),
        );
        assert_eq!(
            (before.0, before.1),
            after,
            "the switch moved the desk's history"
        );
        g.note(
            scenario,
            "the desk switched runtimes with its pointers and history intact",
            json!({ "switched": switched }),
        );
    } else {
        g.inconclusive(
            scenario,
            "the other runtime is not installed on this machine, so the switch leg was not run",
            json!({ "runtime": other_row }),
        );
    }

    console.detach();
    finish(&mut g, scenario, daemon, &desk_id);
}

/// One desk's events of a kind, oldest first, as payloads.
fn kinds(g: &Harness, desk_id: &str, kind: &str) -> Vec<serde_json::Value> {
    g.events()
        .into_iter()
        .filter(|e| e.desk_id.as_deref() == Some(desk_id) && e.kind == kind)
        .map(|e| e.payload)
        .collect()
}

fn prompt_states(g: &Harness, desk_id: &str) -> Vec<String> {
    g.column(
        "SELECT kind || ' ' || state || ' ' || coalesce(failure_code, '-') FROM prompts \
         WHERE desk_id = ?1 ORDER BY created_at_ns, id",
        &[&desk_id],
    )
}

fn pointer(g: &Harness, desk_id: &str, runtime: &str) -> Option<String> {
    g.db()
        .query_row(
            "SELECT native_session_id FROM native_sessions WHERE desk_id = ?1 AND runtime = ?2",
            [desk_id, runtime],
            |row| row.get(0),
        )
        .ok()
}

/// Every delivered prompt must name the runtime and the native session it was
/// handed to, and a live process must carry the same pointer: a delivery the
/// daemon's own rows contradict fails the cell (§9.3).
fn assert_delivery(g: &Harness, desk_id: &str) {
    let rows = g.column(
        "SELECT coalesce(runtime, '-') || ' ' || coalesce(native_session_id, '-') FROM prompts \
         WHERE desk_id = ?1 AND state = 'DELIVERED' ORDER BY resolved_at_ns",
        &[&desk_id],
    );
    for row in &rows {
        let mut parts = row.split(' ');
        assert_ne!(
            parts.next(),
            Some("-"),
            "a delivered prompt names no runtime"
        );
        assert_ne!(
            parts.next(),
            Some("-"),
            "a delivered prompt names no native session"
        );
    }
    assert_eq!(
        rows.len() as i64,
        g.scalar::<i64>(
            "SELECT count(*) FROM prompts WHERE desk_id = ?1 AND state = 'DELIVERED'",
            &[&desk_id],
        )
    );
}

/// The operator's console as the desk's terminal (§9.3): raw, with the window
/// size relayed, for as long as the cell lasts. It reconnects on its own, so an
/// attachment taken before the session exists — or across a switch — still
/// lands. No terminal library: `termios` on Unix and the console API on
/// Windows are the whole of it.
mod console {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use marketrig_acceptance::Endpoint;

    pub struct Console {
        stop: Arc<AtomicBool>,
        relay: Option<std::thread::JoinHandle<()>>,
        saved: Saved,
    }

    impl Console {
        /// Restores the console and lets the relay finish.
        pub fn detach(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(relay) = self.relay.take() {
                let _ = relay.join();
            }
            self.saved.restore();
            println!("\r");
        }
    }

    pub fn attach(endpoint: &Endpoint, desk_id: &str) -> Console {
        let saved = Saved::raw();
        let stop = Arc::new(AtomicBool::new(false));
        let url = format!("ws://127.0.0.1:{}/desks/{desk_id}/terminal", endpoint.port);
        let credential = endpoint.credential.clone();
        let flag = stop.clone();
        let relay = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for the console attachment");
            runtime.block_on(relay(url, credential, flag));
        });
        Console {
            stop,
            relay: Some(relay),
            saved,
        }
    }

    async fn relay(url: String, credential: String, stop: Arc<AtomicBool>) {
        use futures_util::{SinkExt as _, StreamExt as _};
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

        let (keys, mut typed) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buffer = [0u8; 1024];
            while let Ok(read) = stdin.read(&mut buffer) {
                if read == 0 || keys.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        });

        while !stop.load(Ordering::SeqCst) {
            let mut request = match url.clone().into_client_request() {
                Ok(request) => request,
                Err(_) => return,
            };
            if let Ok(header) = format!("Bearer {credential}").parse() {
                request.headers_mut().insert("authorization", header);
            }
            let Ok((mut socket, _)) = tokio_tungstenite::connect_async(request).await else {
                // No terminal yet, or none any more: ask again in a moment.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            let mut size = (0u16, 0u16);
            loop {
                if stop.load(Ordering::SeqCst) {
                    let _ = socket.close(None).await;
                    return;
                }
                let now = Saved::size();
                if now != size && now != (0, 0) {
                    size = now;
                    let resize = format!(r#"{{"resize":{{"cols":{},"rows":{}}}}}"#, size.0, size.1);
                    if socket.send(Message::Text(resize.into())).await.is_err() {
                        break;
                    }
                }
                tokio::select! {
                    keys = typed.recv() => match keys {
                        Some(bytes) => {
                            if socket.send(Message::Binary(bytes.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    frame = socket.next() => match frame {
                        Some(Ok(Message::Binary(bytes))) => {
                            let mut out = std::io::stdout();
                            let _ = out.write_all(&bytes);
                            let _ = out.flush();
                        }
                        Some(Ok(Message::Text(text))) => {
                            let mut out = std::io::stdout();
                            let _ = write!(out, "\r\n{text}\r\n");
                            let _ = out.flush();
                        }
                        Some(Ok(_)) => {}
                        _ => break,
                    },
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                }
            }
        }
    }

    #[cfg(unix)]
    pub struct Saved(libc::termios);

    #[cfg(unix)]
    impl Saved {
        fn raw() -> Saved {
            unsafe {
                let mut saved: libc::termios = std::mem::zeroed();
                libc::tcgetattr(libc::STDIN_FILENO, &mut saved);
                let mut raw = saved;
                libc::cfmakeraw(&mut raw);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
                Saved(saved)
            }
        }

        fn restore(&mut self) {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0);
            }
        }

        fn size() -> (u16, u16) {
            unsafe {
                let mut window: libc::winsize = std::mem::zeroed();
                if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut window) != 0 {
                    return (0, 0);
                }
                (window.ws_col, window.ws_row)
            }
        }
    }

    #[cfg(windows)]
    pub struct Saved(u32, u32);

    #[cfg(windows)]
    impl Saved {
        fn raw() -> Saved {
            use windows::Win32::System::Console::*;
            unsafe {
                let (mut input, mut output) = (CONSOLE_MODE(0), CONSOLE_MODE(0));
                let stdin = GetStdHandle(STD_INPUT_HANDLE).unwrap_or_default();
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE).unwrap_or_default();
                let _ = GetConsoleMode(stdin, &mut input);
                let _ = GetConsoleMode(stdout, &mut output);
                let raw = CONSOLE_MODE(
                    (input.0
                        & !(ENABLE_LINE_INPUT.0 | ENABLE_ECHO_INPUT.0 | ENABLE_PROCESSED_INPUT.0))
                        | ENABLE_VIRTUAL_TERMINAL_INPUT.0,
                );
                let _ = SetConsoleMode(stdin, raw);
                let _ = SetConsoleMode(
                    stdout,
                    CONSOLE_MODE(output.0 | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0),
                );
                Saved(input.0, output.0)
            }
        }

        fn restore(&mut self) {
            use windows::Win32::System::Console::*;
            unsafe {
                if let Ok(stdin) = GetStdHandle(STD_INPUT_HANDLE) {
                    let _ = SetConsoleMode(stdin, CONSOLE_MODE(self.0));
                }
                if let Ok(stdout) = GetStdHandle(STD_OUTPUT_HANDLE) {
                    let _ = SetConsoleMode(stdout, CONSOLE_MODE(self.1));
                }
            }
        }

        fn size() -> (u16, u16) {
            use windows::Win32::System::Console::*;
            unsafe {
                let Ok(stdout) = GetStdHandle(STD_OUTPUT_HANDLE) else {
                    return (0, 0);
                };
                let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
                if GetConsoleScreenBufferInfo(stdout, &mut info).is_err() {
                    return (0, 0);
                }
                let window = info.srWindow;
                (
                    (window.Right - window.Left + 1).max(0) as u16,
                    (window.Bottom - window.Top + 1).max(0) as u16,
                )
            }
        }
    }
}
