//! The acceptance experiment: E1 … E4, E6, E7 and E8, the attended scenarios.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §10.3,
//! `sdd/features/r2-scheduled-triggers/SPEC.md` §10.3,
//! `sdd/features/openviking-continuity/SPEC.md` §7.3,
//! `sdd/features/hithink-a-share/SPEC.md` §6.3,
//! `sdd/features/event-triggers/SPEC.md` §7.2, and root `sdd/SPEC.md`
//! §17, per D75. One operator-attended run per platform-and-runtime cell, on
//! real Yahoo, the real HiThink service, and a real runtime CLI, with MCP
//! registration performed by hand (R1 keeps it operator-performed, feature SPEC
//! §8).
//!
//! **Operator variable:** `MARKETRIG_EXPERIMENT` selects the cell — `codex` runs
//! E1, E3, E4, E6, E7, and E8; `claude` runs E2 and the same five. Unset or
//! anything else skips them all cleanly, which is what CI and every unattended
//! `cargo test` do; E6 additionally skips with evidence unless the operator's
//! prerequisites and provider are named, and E7 unless
//! `MARKETRIG_EXPERIMENT_HITHINK_API_KEY` carries a real HiThink key. A cell's
//! scenarios run one after the
//! other on their own daemons, desks, and bundles: they share the operator's
//! terminal, so the harness serializes them. The instructions are printed, so
//! run the selected cell with output:
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
//! never persisted, feature SPEC §2.3), and neither do E7's research reads (the
//! passthrough writes no row and no event), so those aspects are inconclusive by
//! construction and are recorded as such.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use marketrig_acceptance::standin::{shanghai_date, shanghai_midnight_s, weekday};
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
/// E6: the seed skill's upload summarizes and embeds through a real provider
/// behind the daemon's 90 s write bound, then projects.
const SEED_LANDS: Duration = Duration::from_secs(120);
/// E6: the offline install of the locked wheel set into a cold venv. Pip
/// unpacks about 100 k files, and on Windows each one meets NTFS and Defender:
/// 13-17 min measured on 2026-09-08 with `target/acceptance` unexcluded, past
/// the 15 min patience (the first Windows Claude cell timed out 90 s short).
const PROVISIONS: Duration = Duration::from_secs(1800);

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

    // R5's installed default gates trigger code (R5 feature SPEC §2), and E3 is
    // about the session defining a trigger that runs, not about the operator
    // approving one: the setup puts this root on Always allow, exactly as G21's
    // prologue does (R5 feature SPEC §7.1).
    let (status, allowed) = g.api(
        scenario,
        &endpoint,
        "PUT",
        "/settings/policies",
        Some(r#"{"trigger_code_policy":"ALWAYS_ALLOW"}"#),
    );
    assert_eq!(status, 200, "{allowed}");
    assert_eq!(allowed["trigger_code_policy"], "ALWAYS_ALLOW");

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
         The harness waits up to {patience} minutes per step. While this console is\n\
         \x20  the terminal, ^C goes to the session, not the harness: abort from another\n\
         \x20  terminal with `pkill -f deps/experiment-`, then `stty sane` here.\n\
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

// ---------------------------------------------------------------------------
// E6 — OpenViking memory, skills, and the closed loop, attended
// (`sdd/features/openviking-continuity/SPEC.md` §7.3)
// ---------------------------------------------------------------------------

/// The two prerequisites the operator names (§1.2) and the locked wheel
/// directory MarketRig provisions from, offline (§1.3): the wheel set
/// `node scripts/openviking-wheels.mjs --python <python3.12>` produced.
const OPENVIKING: [&str; 3] = [
    "MARKETRIG_EXPERIMENT_PYTHON",
    "MARKETRIG_EXPERIMENT_NODE",
    "MARKETRIG_EXPERIMENT_WHEELS",
];

/// The provider the real child reasons and embeds with (§2.1). The key is read,
/// never printed and never written to the bundle (root §16).
const MEMORY: [&str; 4] = [
    "MARKETRIG_EXPERIMENT_MEMORY_BASE_URL",
    "MARKETRIG_EXPERIMENT_MEMORY_API_KEY",
    "MARKETRIG_EXPERIMENT_MEMORY_LLM_MODEL",
    "MARKETRIG_EXPERIMENT_MEMORY_EMBEDDING_MODEL",
];

#[test]
fn e6_codex_cli() {
    continuity("E6", "codex", "Codex CLI", "claude");
}

#[test]
fn e6_claude_code() {
    continuity("E6", "claude", "Claude Code", "codex");
}

/// **E6 — the loop closes on real OpenViking** (§7.3). The cell provisions the
/// real environment offline from the operator's wheel set, starts the real
/// child on the real provider, and drives one session through a closed cycle to
/// a lesson and a skill, a resumed session, and a switch to the other runtime.
///
/// What MarketRig itself does is mechanical and asserted: the offline install,
/// the child's readiness, the desk's user, the projection, and the same skills
/// under the other runtime's own path. What the agent does is inconclusive.
/// Capture is inconclusive by construction: the desk key is derived from the
/// seed in the credential store and lives only in the daemon's memory and the
/// runtime's process environment (§3.1), so the harness cannot authenticate to
/// the real server the way the gate does against the stand-in — it records the
/// plugin's own state and logs as evidence and asks the operator to confirm
/// from inside the session, where that identity is.
fn continuity(scenario: &str, cell: &str, runtime: &str, other: &str) {
    if std::env::var(CELL).unwrap_or_default() != cell {
        eprintln!(
            "{scenario} ({runtime}) skipped: set {CELL}={cell} to run this cell attended, \
             and pass `-- --nocapture` so its instructions are visible."
        );
        return;
    }
    let _terminal = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);

    let mut g = Harness::new(&format!("experiment-e6-{cell}"));
    let value = |name: &str| std::env::var(name).unwrap_or_default();
    let missing: Vec<&str> = OPENVIKING
        .iter()
        .chain(MEMORY.iter())
        .copied()
        .filter(|name| value(name).trim().is_empty())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "{scenario} ({runtime}) skipped: unset — {}. E6 needs the Python 3.12 and Node paths, \
             the locked wheel directory, and the real provider; see \
             crates/marketrig-acceptance/EXPERIMENT.md §1.",
            missing.join(", ")
        );
        g.inconclusive(
            scenario,
            "the cell's OpenViking variables are unset, so E6 did not run",
            json!({ "missing": missing }),
        );
        return;
    }

    g.real_feed();
    let daemon = g.spawn(scenario);
    let endpoint = daemon.endpoint.clone();

    // The runtime, the provider, then the installation — all through REST, the
    // way the desktop's Settings tab does (§8).
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

    let (status, provider) = g.api_redacted(
        scenario,
        &endpoint,
        "PUT",
        "/memory/provider",
        &json!({
            "base_url": value(MEMORY[0]), "api_key": value(MEMORY[1]),
            "llm_model": value(MEMORY[2]), "embedding_model": value(MEMORY[3]),
        })
        .to_string(),
    );
    assert_eq!(status, 200, "the provider must answer: {provider}");
    assert_eq!(provider["api_key_present"], true, "{provider}");
    assert!(
        provider.get("api_key").is_none(),
        "the key never comes back: {provider}"
    );
    let dimension = provider["embedding_dimension"].as_i64().unwrap_or_default();
    assert!(
        dimension > 0,
        "the save measures the embedding dimension with one real request (§2.1): {provider}"
    );

    // Offline provisioning from the locked wheel set, then the real child's own
    // readiness (§1.3, §2.2). Both are minutes, and both are mechanical.
    let (status, claimed) = g.api(
        scenario,
        &endpoint,
        "PUT",
        "/openviking/setup",
        Some(
            &json!({
                "python": value(OPENVIKING[0]), "node": value(OPENVIKING[1]),
                "wheels": value(OPENVIKING[2]),
            })
            .to_string(),
        ),
    );
    assert_eq!(
        status, 202,
        "both prerequisites must validate (§1.2): {claimed}"
    );
    let installation = |g: &Harness| g.call(&endpoint, "GET", "/openviking", None).1;
    assert!(
        waited(PROVISIONS, "the offline provisioning to finish", || {
            installation(&g)["setup"]["state"] != "PROVISIONING"
        }),
        "provisioning did not finish within the cell's patience: {}",
        installation(&g)
    );
    let installed = installation(&g);
    assert_eq!(
        installed["setup"]["state"], "AVAILABLE",
        "the locked wheel set must install offline on this interpreter (§1.3): {installed}"
    );
    assert!(
        waited(PATIENCE, "the child to answer /ready", || {
            installation(&g)["child"] == "READY"
        }),
        "the real openviking-server never reached READY: {}",
        installation(&g)
    );
    assert!(
        g.event_kinds()
            .iter()
            .any(|kind| kind == "OPENVIKING_STARTED"),
        "readiness appends OPENVIKING_STARTED (§2.2)"
    );
    g.note(
        scenario,
        "MarketRig provisioned its own environment offline from the wheels and the real child answered /ready",
        json!({ "installation": installation(&g), "embedding_dimension": dimension }),
    );

    let desk = format!("{cell}-e6-{}", marketrig_acceptance::now_secs());
    let (status, created) = g.api(
        scenario,
        &endpoint,
        "POST",
        "/desks",
        Some(&json!({ "name": desk, "runtime": cell }).to_string()),
    );
    assert_eq!(status, 201, "{created}");
    let desk_id = created["id"].as_str().expect("id").to_owned();

    // A desk created while the child is READY gets its user, its key, and the
    // seeded skill at the end of creation (§3.2, §5.3) — the premise of
    // everything the session is about to be asked to do.
    assert!(
        waited(SETTLES, "the desk's OpenViking user", || {
            !kinds(&g, &desk_id, "DESK_MEMORY_PROVISIONED").is_empty()
        }),
        "a desk created while the child is READY is provisioned at creation (§3.2)"
    );

    // Mechanical, and it names the instrument the operator trades: a cycle only
    // closes on a market the real feed is observing.
    let quotes = format!("/desks/{desk_id}/market/quotes");
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

    let workspace = g.workspace(&desk);
    let home = g
        .out
        .join("data")
        .join("openviking")
        .join("plugin")
        .join(&desk_id);
    let doctor = workspace
        .join(".marketrig")
        .join("plugins")
        .join(format!("openviking-{cell}"))
        .join("scripts")
        .join("ov-memory-doctor.mjs");
    let instructions = format!(
        "\n\
         ===========================================================================\n\
         {scenario} — memory and skills on real OpenViking (feature SPEC §7.3)\n\
         ===========================================================================\n\
         \n\
         Desk:       {desk}\n\
         Runtime:    {runtime} {version} at {path}\n\
         Python:     {python}\n\
         Node:       {node}\n\
         Wheels:     {wheels}\n\
         Provider:   {base} — llm {llm}, embeddings {embedding} ({dimension} dimensions)\n\
         Instrument: {instrument}   (LIVE on the real feed right now)\n\
         Data root:  {root}\n\
         Evidence:   {root}\n\
         \n\
         MarketRig has already provisioned its own OpenViking environment offline\n\
         from those wheels, started the server, and given this desk its own user,\n\
         its key, and the seeded `desk-improvement` skill. Nothing to register and\n\
         nothing to start, as in E4: MarketRig launches the runtime itself, with\n\
         the memory plugin's hooks and the `openviking` MCP server registered, and\n\
         this console becomes the desk's terminal.\n\
         \n\
         1. Answer whatever {runtime} asks on first launch, as in E4, and nothing\n\
         \x20  more.\n\
         \n\
         2. Ask the session to buy one unit of {instrument} through `submit_order`\n\
         \x20  and then to sell that same unit. That closes one position cycle, and\n\
         \x20  MarketRig queues its EVALUATION prompt and delivers it.\n\
         \n\
         3. Let the session do what its constitution says: state the lesson plainly\n\
         \x20  in the conversation — that is what gets captured — and write it into\n\
         \x20  a skill with the `openviking` tools (`write` on\n\
         \x20  viking://~/skills/<name>/SKILL.md). Do not write either yourself, and\n\
         \x20  do not edit `.agents/skills/`: it is MarketRig's read-only projection\n\
         \x20  and an edit there must be refused.\n\
         \n\
         4. Confirm the capture yourself, from inside the session, which is where\n\
         \x20  this desk's OpenViking identity is:\n\
         \n\
         \x20      node {doctor}\n\
         \n\
         \x20  The harness holds no desk key and records this aspect INCONCLUSIVE\n\
         \x20  either way. That report, and the plugin's own state and logs under\n\
         \x20  {home}, are the evidence.\n\
         \n\
         5. When the harness says so, it ends the session and resumes the thread.\n\
         \x20  Ask the new session what this desk learned — it must reach for the\n\
         \x20  `openviking` `search` tool — and to read its own skill back. Let the\n\
         \x20  first session finish a turn before that: a Claude `--resume` only\n\
         \x20  succeeds once the earlier session ended one.\n\
         \n\
         6. The harness then switches the desk to {other} and stops. The same skill\n\
         \x20  must be readable through {other}'s own path.\n\
         \n\
         The harness waits up to {patience} minutes per step. While this console is\n\
         \x20  the terminal, ^C goes to the session, not the harness: abort from another\n\
         \x20  terminal with `pkill -f deps/experiment-`, then `stty sane` here.\n\
         ===========================================================================\n",
        version = row["version"].as_str().unwrap_or_default(),
        path = row["executable_path"].as_str().unwrap_or_default(),
        python = value(OPENVIKING[0]),
        node = value(OPENVIKING[1]),
        wheels = value(OPENVIKING[2]),
        base = value(MEMORY[0]),
        llm = value(MEMORY[2]),
        embedding = value(MEMORY[3]),
        root = g.out.display(),
        doctor = doctor.display(),
        home = home.display(),
        patience = PATIENCE.as_secs() / 60,
    );
    println!("{instructions}");
    g.write_evidence("instructions-e6.txt", &instructions);
    g.note(
        scenario,
        "attended cell prepared; the environment is provisioned and the console is about to become the desk's terminal",
        json!({ "desk": desk, "desk_id": desk_id, "instrument": instrument,
                "openviking_home": home.display().to_string() }),
    );

    let console = console::attach(&endpoint, &desk_id);

    let (status, activated) = g.api(
        scenario,
        &endpoint,
        "POST",
        &format!("/desks/{desk_id}/session/activate"),
        Some(r#"{"mode":"NEW"}"#),
    );
    assert_eq!(status, 202, "the runtime must be activatable: {activated}");

    // The seed upload runs behind desk creation and projects itself when it
    // lands, within the daemon's write bound (§5.3); the launch's own
    // projection covers it when it landed earlier (§5.2).
    let seeded = workspace
        .join(".agents")
        .join("skills")
        .join("desk-improvement")
        .join("SKILL.md");
    assert!(
        waited(SEED_LANDS, "the seeded skill's projection", || {
            seeded.is_file()
        }),
        "activation projects the desk's skills before the session starts (§5.2): {}",
        seeded.display()
    );
    let before = skills(&workspace);

    if !waited(PATIENCE, "the session to become ready", || {
        !kinds(&g, &desk_id, "SESSION_READY").is_empty()
    }) {
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
        "MarketRig started the runtime with the memory plugin registered and the launch reached readiness",
        json!({ "activation": kinds(&g, &desk_id, "SESSION_STARTED"), "skills": before }),
    );

    // --- The agent's own half (root §17) ------------------------------------
    if !waited(PATIENCE, "the session to close one position cycle", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM position_cycles WHERE desk_id = ?1",
            &[&desk_id],
        ) >= 1
    }) {
        g.inconclusive(
            scenario,
            "no position cycle was closed within the cell's patience",
            json!({ "prompts": prompt_states(&g, &desk_id) }),
        );
        console.detach();
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    let cycle: String = g.scalar(
        "SELECT id FROM position_cycles WHERE desk_id = ?1 ORDER BY closed_at_ns LIMIT 1",
        &[&desk_id],
    );
    if waited(PATIENCE, "the evaluation prompt to be delivered", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM prompts WHERE desk_id = ?1 AND kind = 'EVALUATION' \
             AND state = 'DELIVERED'",
            &[&desk_id],
        ) >= 1
    }) {
        assert_delivery(&g, &desk_id);
        g.note(
            scenario,
            "a round trip closed a cycle and MarketRig delivered its evaluation to the session",
            json!({ "cycle": cycle, "prompts": prompt_states(&g, &desk_id) }),
        );
    } else {
        g.inconclusive(
            scenario,
            "the evaluation prompt was not delivered within the cell's patience",
            json!({ "cycle": cycle, "prompts": prompt_states(&g, &desk_id) }),
        );
    }

    // (a) Capture. Inconclusive by construction: see this function's own note.
    g.inconclusive(
        scenario,
        "whether the turn was captured under this desk's OpenViking user is the operator's to confirm \
         from inside the session — the harness holds no desk key",
        json!({
            "confirm": format!("node {}", doctor.display()),
            "plugin_state": plugin_state(&home),
        }),
    );

    // (b) The skill, which reaches the workspace only through MarketRig's own
    // projection after SESSION_TURN_ENDED (§5.2).
    let learned = waited(
        PATIENCE,
        "a skill written or revised through the openviking tools",
        || skills(&workspace) != before,
    );
    let after = skills(&workspace);
    if learned {
        let changed: Vec<&String> = after
            .iter()
            .filter(|(name, files)| before.get(*name) != Some(files))
            .map(|(name, _)| name)
            .collect();
        for name in &changed {
            let skill = workspace
                .join(".agents")
                .join("skills")
                .join(name)
                .join("SKILL.md");
            assert!(
                skill.is_file(),
                "a projected skill carries its SKILL.md (§5.2): {}",
                skill.display()
            );
        }
        g.note(
            scenario,
            "the session's skill write reached the workspace through MarketRig's projection",
            json!({ "changed": changed, "skills": after,
                    "turn_ends": kinds(&g, &desk_id, "SESSION_TURN_ENDED").len() }),
        );
    } else {
        g.inconclusive(
            scenario,
            "no skill was written or revised within the cell's patience",
            json!({ "skills": after,
                    "turn_ends": kinds(&g, &desk_id, "SESSION_TURN_ENDED").len(),
                    "failures": kinds(&g, &desk_id, "SKILLS_PROJECTION_FAILED") }),
        );
    }

    // --- The later session: the same thread, the same skills (§5.2, §7.3) ---
    let (status, ended) = g.api(
        scenario,
        &endpoint,
        "POST",
        &format!("/desks/{desk_id}/session/exit"),
        None,
    );
    assert!(
        matches!(status, 202 | 409 | 502),
        "exit answers the process, says there was none, or says its shutdown continues: {ended}"
    );
    println!(
        "\r\n{scenario}: resuming the thread — ask the new session what this desk learned, and to read its own skill.\r\n"
    );
    let projections = kinds(&g, &desk_id, "SKILLS_PROJECTED").len();
    let (status, activated) = g.api(
        scenario,
        &endpoint,
        "POST",
        &format!("/desks/{desk_id}/session/activate"),
        Some(r#"{"mode":"CONTINUE"}"#),
    );
    assert_eq!(
        status, 202,
        "the desk's thread must be resumable: {activated}"
    );
    if waited(SETTLES, "the resumed activation's projection", || {
        kinds(&g, &desk_id, "SKILLS_PROJECTED").len() > projections
    }) {
        assert_eq!(
            skills(&workspace),
            after,
            "a resumed activation projects the same skills (§5.2)"
        );
        g.note(
            scenario,
            "the resumed activation projected the desk's skills again, unchanged",
            json!({ "skills": after }),
        );
    } else {
        g.inconclusive(
            scenario,
            "the resumed activation projected nothing within the cell's patience",
            json!({ "failures": kinds(&g, &desk_id, "SKILLS_PROJECTION_FAILED") }),
        );
    }
    g.inconclusive(
        scenario,
        "whether the resumed session found the lesson through `search` is the operator's to confirm on the console",
        json!({ "cycle": cycle,
                "expect": "the session reaches for the openviking `search` tool and answers from what the earlier session said" }),
    );

    // The other runtime reads the same skills through its own path (§5.1): the
    // `.claude/skills` link on Claude Code, `.agents/skills/` on Codex.
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
        let root = match other {
            "claude" => workspace.join(".claude").join("skills"),
            _ => workspace.join(".agents").join("skills"),
        };
        for name in after.keys() {
            let skill = root.join(name).join("SKILL.md");
            assert!(
                std::fs::read(&skill).is_ok_and(|bytes| !bytes.is_empty()),
                "the other runtime reads the same skill through its own path (§5.1): {}",
                skill.display()
            );
        }
        g.note(
            scenario,
            "the desk switched runtimes and the same skills are readable through the other runtime's own path",
            json!({ "switched": switched, "path": root.display().to_string(),
                    "skills": after.keys().collect::<Vec<_>>() }),
        );
    } else {
        g.inconclusive(
            scenario,
            "the other runtime is not installed on this machine, so the switch leg was not run",
            json!({ "runtime": other_row }),
        );
    }

    g.note(
        scenario,
        "the plugin's own state and logs are in the bundle",
        json!({ "openviking_home": home.display().to_string(), "files": plugin_state(&home) }),
    );
    console.detach();
    finish(&mut g, scenario, daemon, &desk_id);
}

// ---------------------------------------------------------------------------
// E7 — the A-share plane on the real HiThink service, attended
// (`sdd/features/hithink-a-share/SPEC.md` §6.3)
// ---------------------------------------------------------------------------

/// The operator's own HiThink key (`hithink-a-share` §1.2). It is read here,
/// sent once in a `PUT` body the bundle keeps redacted, and never printed: the
/// cell's last steps remove it and grep the whole bundle for it.
const HITHINK_KEY: &str = "MARKETRIG_EXPERIMENT_HITHINK_API_KEY";

/// The A-share the cell reads and trades (`hithink-a-share` §2.1): Kweichow
/// Moutai, `600519.SH` to HiThink, 100 shares to a lot.
const A_SHARE: &str = "600519.XSHG";

/// The cell's two Shanghai instants as Unix seconds (`a-share-engine` SPEC §6,
/// per AE-10): the staged trading day the buy belongs to, and the one the sell
/// happens on. Both are 10:00 Asia/Shanghai — inside the morning session, so the
/// node clock, which is frozen between advances, is in session for the whole
/// sitting whatever hour the operator starts at.
///
/// The sell day is the most recent weekday whose 09:30 Shanghai has passed, so
/// the provider already has that day's own bar — readiness after the clock move
/// needs it (§2.1). The buy day is the weekday before it. Both step over the
/// weekend, so the cell runs on a Sunday evening as readily as on a Tuesday
/// afternoon. A holiday neither step can see is left to the cell's own readiness
/// check, which prints what the provider's calendar says before the operator is
/// asked to drive anything.
fn staged_days(now_s: i64) -> (i64, i64) {
    let mut sell = shanghai_midnight_s(now_s);
    while !weekday(sell + 12 * 3_600) || now_s < sell + 9 * 3_600 + 1_800 {
        sell -= 86_400;
    }
    let mut buy = sell - 86_400;
    while !weekday(buy + 12 * 3_600) {
        buy -= 86_400;
    }
    (buy + 10 * 3_600, sell + 10 * 3_600)
}

/// The one part of E7 that is arithmetic rather than the operator's: both
/// instants are 10:00 Asia/Shanghai, both days are weekdays, and the sell day's
/// 09:30 has already passed. Thursday 2026-03-12 and Friday 2026-03-13 are
/// weekdays; the 14th and 15th are the weekend; Monday is the 16th.
#[test]
fn staged_days_land_in_session_and_step_over_the_weekend() {
    for (now_s, expected) in [
        // A weekday afternoon: today sells, the previous weekday buys.
        (1_773_244_800 + 14 * 3_600, ("20260311", "20260312")),
        (1_773_590_400 + 11 * 3_600, ("20260313", "20260316")),
        // A weekday before 09:30 has no bar of its own yet, so it steps back.
        (1_773_244_800 + 8 * 3_600, ("20260310", "20260311")),
        // The weekend, and the Monday morning after it, all sell on Friday.
        (1_773_417_600 + 11 * 3_600, ("20260312", "20260313")),
        (1_773_504_000 + 20 * 3_600, ("20260312", "20260313")),
        (1_773_590_400 + 8 * 3_600, ("20260312", "20260313")),
    ] {
        let (buy_s, sell_s) = staged_days(now_s);
        assert_eq!(
            (
                shanghai_date(buy_s).as_str(),
                shanghai_date(sell_s).as_str()
            ),
            expected
        );
        assert_eq!(buy_s - shanghai_midnight_s(buy_s), 10 * 3_600);
        assert_eq!(sell_s - shanghai_midnight_s(sell_s), 10 * 3_600);
    }
}

#[test]
fn e7_codex_cli() {
    a_share("E7", "codex", "Codex CLI");
}

#[test]
fn e7_claude_code() {
    a_share("E7", "claude", "Claude Code");
}

/// **E7 — A-share data on the real service** (`hithink-a-share` §6.3). The
/// harness saves the operator's real key the way Settings does, then MarketRig
/// launches the runtime itself as in E4 and E6 and this console becomes the
/// desk's terminal.
///
/// Mechanical and asserted: the provider row, the `CN` observation's provider,
/// calendar, and null source time, the closed cycle's queued evaluation, and
/// the key's absence from every file of the bundle. What the session read
/// through `marketrig research hithink` leaves no durable trace at all — the
/// passthrough writes no row and no event (§4.1) — so that aspect is
/// inconclusive by construction, as E1's two quote reads are.
///
/// One sitting completes the cell, at any wall-clock hour: A-share T+1 locks a
/// buy until the next trading day (`a-share-engine` SPEC §1.1), so the cell
/// stages both Shanghai days through the controlled-clock seam instead of
/// sending the operator away for a day (per AE-10). This desk's nodes open on
/// the buy day; once the buy has filled and the same-day sell has been refused,
/// the harness moves every node's clock to the sell day, which unlocks the lot
/// exactly as the rollover does in gate A1, and asks the same session for the
/// sell. Only the trading dates are staged: the provider's calendar, the day's
/// bar and every snapshot are the real service's own, and outside real Shanghai
/// hours that snapshot is simply the day's last, which a MARKET order fills
/// against. The bundle records what this cell cannot prove: the provider's
/// unknown source delay, the unadjusted corporate action, and the staged
/// calendar behind the realized figure.
fn a_share(scenario: &str, cell: &str, runtime: &str) {
    if std::env::var(CELL).unwrap_or_default() != cell {
        eprintln!(
            "{scenario} ({runtime}) skipped: set {CELL}={cell} to run this cell attended, \
             and pass `-- --nocapture` so its instructions are visible."
        );
        return;
    }
    let _terminal = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);

    let mut g = Harness::new(&format!("experiment-e7-{cell}"));
    let key = std::env::var(HITHINK_KEY)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if key.is_empty() {
        eprintln!(
            "{scenario} ({runtime}) skipped: unset — {HITHINK_KEY}. E7 needs a real HiThink key, \
             the one the operator would type into Settings; see \
             crates/marketrig-acceptance/EXPERIMENT.md §1."
        );
        g.inconclusive(
            scenario,
            "the cell's HiThink key is unset, so E7 did not run",
            json!({ "missing": [HITHINK_KEY] }),
        );
        return;
    }

    // The calendar this sitting runs on (per AE-10). The seam is honored only
    // alongside the relocated data root, which the harness always sets, and it
    // is what serves `PUT /test/clock` later; the daemon's own bookkeeping stays
    // on the wall clock, and both feeds stay real.
    let (buy_s, sell_s) = staged_days(marketrig_acceptance::now_secs() as i64);
    let (buy_day, sell_day) = (shanghai_date(buy_s), shanghai_date(sell_s));
    g.standin_clock(buy_s as u64 * 1_000_000_000);
    g.real_feed();
    let daemon = g.spawn(scenario);
    let endpoint = daemon.endpoint.clone();

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

    // The provider, through REST, exactly as the Settings block does (§1.4).
    // The save is one bounded request against the real service (§1.2), so a
    // rejected key fails the cell here, before the operator is asked anything.
    let (status, provider) = g.api_redacted(
        scenario,
        &endpoint,
        "PUT",
        "/research/hithink",
        &json!({ "api_key": key }).to_string(),
    );
    assert_eq!(
        status, 200,
        "the real service must accept the key: {provider}"
    );
    assert_eq!(provider["state"], "AVAILABLE", "{provider}");
    assert_eq!(provider["a_share_feed"], "HITHINK", "{provider}");
    assert_eq!(provider["api_key_present"], true, "{provider}");
    assert!(
        provider.get("api_key").is_none(),
        "the key never comes back (§1.2): {provider}"
    );
    let base = provider["base_url"].as_str().unwrap_or_default().to_owned();
    assert!(
        base.starts_with("https://"),
        "E7 is the real service: leave MARKETRIG_TEST_HITHINK_URL unset ({provider})"
    );
    assert!(
        g.event_kinds()
            .iter()
            .any(|kind| kind == "HITHINK_PROVIDER_CHANGED"),
        "a save appends HITHINK_PROVIDER_CHANGED (§1.2)"
    );

    // The desk is created after the save, so its node starts on the HiThink
    // leg and its first poll is HiThink's (§2.2).
    let desk = format!("{cell}-e7-{}", marketrig_acceptance::now_secs());
    let (status, created) = g.api(
        scenario,
        &endpoint,
        "POST",
        "/desks",
        Some(&json!({ "name": desk, "runtime": cell }).to_string()),
    );
    assert_eq!(status, 201, "{created}");
    let desk_id = created["id"].as_str().expect("id").to_owned();

    // Mechanical: the CN leg reads from HiThink whatever the Shanghai phase —
    // the node polls once at subscription (R1 feature SPEC §2.1).
    let quotes = format!("/desks/{desk_id}/market/quotes");
    let cn = |g: &Harness| -> serde_json::Value {
        g.call(&endpoint, "GET", &quotes, None).1["quotes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|quote| quote["instrument_id"] == A_SHARE)
            .cloned()
            .unwrap_or_default()
    };
    assert!(
        waited(SETTLES, "the A-share observation from HiThink", || {
            cn(&g)["provider"] == "hithink"
        }),
        "the real HiThink feed never produced a CN observation: {}",
        cn(&g)
    );
    let observation = cn(&g);
    assert_eq!(
        observation.get("source_time_ns"),
        Some(&serde_json::Value::Null),
        "a HiThink observation carries a null source time (§2.3): {observation}"
    );
    assert!(
        matches!(
            observation["calendar"].as_str(),
            Some("HITHINK" | "WEEKDAY")
        ),
        "every observation names the calendar rule that labeled its phase (§3): {observation}"
    );
    g.note(
        scenario,
        "the desk's CN leg reads the real HiThink service, with the calendar rule and the null source time §2.3 requires",
        json!({ "observation": observation, "base_url": base }),
    );

    // §2.1 readiness on the staged day, before the operator is asked for
    // anything: the provider's own calendar must carry that day and its own bar
    // must be dated it. A holiday the weekend step cannot see leaves the
    // instrument `UNAVAILABLE`, and the printout says so rather than letting the
    // sitting discover it fifteen minutes in.
    let executable = waited(
        SETTLES,
        "the A-share to be executable on the staged trading day",
        || cn(&g)["execution"]["availability"] == "OPEN",
    );
    let staged = if executable {
        format!("{buy_day} (staged) → {sell_day} after the refusal — execution OPEN")
    } else {
        format!(
            "{buy_day} (staged) — NOT EXECUTABLE: {}. Stop this run: this sitting \
             cannot buy, and the harness has recorded why.",
            cn(&g)["execution"]["reason"].as_str().unwrap_or("UNKNOWN")
        )
    };
    if !executable {
        g.inconclusive(
            scenario,
            "the staged trading day is not executable on the provider's real calendar, so this sitting cannot buy",
            json!({ "staged_day": buy_day, "execution": cn(&g)["execution"] }),
        );
    }

    // The two reads the session is asked for, as the operator will paste them:
    // the CLI needs the daemon's data root, and the terminal relays every
    // MARKETRIG_* variable to the launched runtime (R3 §4.2).
    let income = format!(
        "{cli} research hithink a-share/financials/income-statements \
         --param thscode=600519.SH --param period=annual --param limit=1",
        cli = g.cli.display()
    );
    let valuation = format!(
        "{cli} research hithink a-share/valuations/snapshot --param thscodes=600519.SH",
        cli = g.cli.display()
    );
    let instructions = format!(
        "\n\
         ===========================================================================\n\
         {scenario} — A-share data on real HiThink (feature SPEC §6.3)\n\
         ===========================================================================\n\
         \n\
         Desk:       {desk}\n\
         Runtime:    {runtime} {version} at {path}\n\
         Provider:   {base} — AVAILABLE, a_share_feed HITHINK\n\
         Instrument: {instrument} — HiThink 600519.SH, 100 shares to a lot\n\
         Day:        {staged}\n\
         CLI:        {cli}\n\
         Data root:  {root}\n\
         Evidence:   {root}\n\
         \n\
         MarketRig already holds your HiThink key — the harness saved it the way\n\
         the Settings block does — and this desk's CN leg is reading the real\n\
         service. Nothing to register and nothing to start, as in E4 and E6:\n\
         MarketRig launches the runtime itself and this console becomes the desk's\n\
         terminal. The session inherits MARKETRIG_TEST_DATA_ROOT, so the command\n\
         above reaches this daemon with no environment of its own.\n\
         \n\
         T+1 locks a lot until the next trading day, so this desk's own trading\n\
         day is staged at {buy_day} and the harness moves it to {sell_day} once\n\
         the refusal below has happened. Both are weekdays — the `Day:` line\n\
         above says whether the provider's calendar accepts the first — and\n\
         MarketRig's session clock is 10:00 on each, so the hour you start at is\n\
         never what you are waiting on. Everything the provider says is the real\n\
         service's: its calendar, the day's bar, every snapshot. The session may\n\
         notice `band_date` reading {buy_day}; that is the staging, and it is why\n\
         the realized figure of this cell is a harness artifact.\n\
         \n\
         Order type matters more than the hour. Outside real Shanghai trading\n\
         hours the snapshot is the day's last and does not move, so use MARKET\n\
         orders for both legs: a LIMIT would wait for volume that will not come.\n\
         Inside trading hours either works.\n\
         \n\
         1. Answer whatever {runtime} asks on first launch, as in E4, and nothing\n\
         \x20  more.\n\
         \n\
         2. Ask the session to read {instrument}'s latest annual income statement\n\
         \x20  and its current valuation through MarketRig:\n\
         \n\
         \x20      {income}\n\
         \x20      {valuation}\n\
         \n\
         \x20  Success is `code == 0` in HiThink's own envelope. The passthrough\n\
         \x20  writes no row and no event, so the harness records this aspect\n\
         \x20  INCONCLUSIVE by construction — only you can judge what came back.\n\
         \n\
         3. Ask the session to read {instrument}'s own band before it trades: the\n\
         \x20  desk's `market/quotes` resource carries `prev_close`, `limit_up`,\n\
         \x20  `limit_down` and `band_date`, and the `execution` object beside\n\
         \x20  them says `band_date_inferred: true`, `source_delay: UNKNOWN` and\n\
         \x20  `fill_policy: HITHINK_SAMPLED`. The reference is the provider's,\n\
         \x20  the date is inferred from receipt, and neither is certified: that\n\
         \x20  is the disclosed assumption every order below rests on.\n\
         \n\
         4. Ask the session to buy one lot — 100 shares — of {instrument} through\n\
         \x20  `submit_order`, as a MARKET order: it fills against the current\n\
         \x20  snapshot at once. A LIMIT waits for a later observation whose\n\
         \x20  volume has grown, which only a trading market produces. Only\n\
         \x20  {instrument} counts: the harness ignores trades of any other\n\
         \x20  instrument, and they will not move this protocol on — if the\n\
         \x20  session buys something else, steer it back to {instrument}.\n\
         \n\
         5. Ask it to sell that same lot **now**. It must be refused: the\n\
         \x20  A-share T+1 rule locks the day's buy until the next trading day,\n\
         \x20  and the refusal says so. That refusal is the point of the sitting,\n\
         \x20  not a fault.\n\
         \n\
         6. Then wait here. The harness moves this desk's trading day on to\n\
         \x20  {sell_day}, which unlocks the lot, and prints the one step left —\n\
         \x20  the sell — on this console. Do not ask for it before that.\n\
         \n\
         7. Nothing else. The harness then removes the key from the credential\n\
         \x20  store and greps the whole bundle for it, so a later research read\n\
         \x20  answers RESEARCH_UNCONFIGURED: that is the cell ending, not a fault.\n\
         \n\
         Three limitations this cell cannot remove, and records instead: the\n\
         provider publishes no source timestamp, so every observation's delay is\n\
         UNKNOWN and no fill may be read as a claim about market time; a holding\n\
         carried across a corporate action is not adjusted here, so a dividend or\n\
         split date inside the hold makes the realized figure incomparable; and\n\
         both legs trade against the same day's real quotes under two staged\n\
         trading dates, so that figure is a harness artifact either way — never\n\
         attribute any of it to strategy.\n\
         \n\
         The harness waits up to {patience} minutes per step. While this console is\n\
         \x20  the terminal, ^C goes to the session, not the harness: abort from another\n\
         \x20  terminal with `pkill -f deps/experiment-`, then `stty sane` here.\n\
         ===========================================================================\n",
        version = row["version"].as_str().unwrap_or_default(),
        path = row["executable_path"].as_str().unwrap_or_default(),
        instrument = A_SHARE,
        cli = g.cli.display(),
        root = g.out.display(),
        patience = PATIENCE.as_secs() / 60,
    );
    println!("{instructions}");
    g.write_evidence("instructions-e7.txt", &instructions);
    g.note(
        scenario,
        "attended cell prepared; the provider is saved, the nodes are staged on the buy day, and the console is about to become the desk's terminal",
        json!({ "desk": desk, "desk_id": desk_id, "instrument": A_SHARE, "runtime": row,
                "staged_day": buy_day, "sell_day": sell_day, "staged_ns": buy_s as u64 * 1_000_000_000,
                "execution": cn(&g)["execution"] }),
    );

    let console = console::attach(&endpoint, &desk_id);

    let (status, activated) = g.api(
        scenario,
        &endpoint,
        "POST",
        &format!("/desks/{desk_id}/session/activate"),
        Some(r#"{"mode":"NEW"}"#),
    );
    assert_eq!(status, 202, "the runtime must be activatable: {activated}");

    // --- The agent's own half (root §17) ------------------------------------
    if !waited(PATIENCE, "the session to become ready", || {
        !kinds(&g, &desk_id, "SESSION_READY").is_empty()
    }) {
        g.inconclusive(
            scenario,
            "the launch never reached readiness — the operator may not have answered its first-launch questions",
            json!({ "waited_secs": PATIENCE.as_secs() }),
        );
    } else if !waited(PATIENCE, "the session's buy to fill", || {
        g.scalar::<i64>(
            "SELECT count(*) FROM fills WHERE desk_id = ?1 AND instrument_id = ?2 AND side = 'BUY'",
            &[&desk_id, &A_SHARE],
        ) >= 1
    }) {
        g.inconclusive(
            scenario,
            "the session bought no lot of the named instrument within the cell's patience",
            json!({ "instrument": A_SHARE, "prompts": prompt_states(&g, &desk_id) }),
        );
    } else {
        // The buy filled on the staged day, so the sitting's first claim is the
        // T+1 refusal: a same-day sell is terminal on the action row, with no
        // order behind it (`a-share-engine` SPEC §1.2, §5.2). `trading_actions`
        // carries no instrument column, so the named instrument is matched in
        // the request the session sent, which is where its id is recorded.
        const REFUSED_SELLS: &str = "SELECT count(*) FROM trading_actions WHERE desk_id = ?1 \
             AND outcome LIKE '%ORDER_INVALID%' AND request LIKE '%SELL%' AND request LIKE ?2";
        let named = format!("%{A_SHARE}%");
        if !waited(
            PATIENCE,
            "the same-day sell to be refused by the T+1 rule",
            || g.scalar::<i64>(REFUSED_SELLS, &[&desk_id, &named]) >= 1,
        ) {
            g.inconclusive(
                scenario,
                "the session never asked for the same-day sell of the named instrument the T+1 rule must refuse",
                json!({ "instrument": A_SHARE, "prompts": prompt_states(&g, &desk_id) }),
            );
        } else {
            let refused: i64 = g.scalar(REFUSED_SELLS, &[&desk_id, &named]);
            g.note(
                scenario,
                "the staged day's buy of the named instrument filled and its same-day sell was refused terminally by the T+1 rule, with no order behind it",
                json!({ "instrument": A_SHARE, "refused_sells": refused }),
            );
        }

        // The calendar is staged, never waited for (per AE-10): moving every
        // node's clock to the sell day is the rollover gate A1 proves, and it
        // leaves the provider's calendar, that day's own bar and every snapshot
        // on the real service. The re-open needs a poll on the new day, which is
        // why the controlled clock also lifts the cadence gate (`node.rs`).
        g.advance_clock(scenario, &endpoint, sell_s as u64 * 1_000_000_000);
        let reopened = waited(SETTLES, "the A-share to re-open on the sell day", || {
            let view = cn(&g);
            view["execution"]["availability"] == "OPEN"
                && view["execution"]["band_date"] == json!(sell_day)
        });
        g.note(
            scenario,
            "the desk's trading day moved from the staged buy day to the sell day, which unlocks the lot T+1 held",
            json!({ "from": buy_day, "to": sell_day,
                    "from_ns": buy_s as u64 * 1_000_000_000,
                    "to_ns": sell_s as u64 * 1_000_000_000,
                    "reopened": reopened, "execution": cn(&g)["execution"] }),
        );

        // Printed into the raw console, so every line carries its own return.
        let sell = format!(
            "\n\
             ===========================================================================\n\
             {scenario} — the trading day moved on: ask for the sell (feature SPEC §6.3)\n\
             ===========================================================================\n\
             \n\
             This desk's trading day is now {sell_day}, and the lot bought on\n\
             {buy_day} is no longer locked by T+1. Nothing else moved: the\n\
             calendar, the day's bar and the snapshot are the real service's.\n\
             \n\
             Ask the session to sell the 100-share lot of {instrument} through\n\
             `submit_order`, as a MARKET order again: it fills against the\n\
             current snapshot at once. A LIMIT waits for a later observation\n\
             whose volume has grown, which only a trading market produces.\n\
             \n\
             Then nothing else: the harness waits up to {patience} minutes for the\n\
             native cycle to close and its EVALUATION prompt to queue, removes the\n\
             key, and ends the cell.\n\
             ===========================================================================\n",
            instrument = A_SHARE,
            patience = PATIENCE.as_secs() / 60,
        );
        println!("{}", sell.replace('\n', "\r\n"));
        g.write_evidence("instructions-e7-sell.txt", &sell);

        if !waited(PATIENCE, "the session's sell to close the position", || {
            g.scalar::<i64>(
                "SELECT count(*) FROM position_cycles WHERE desk_id = ?1 AND instrument_id = ?2",
                &[&desk_id, &A_SHARE],
            ) >= 1
        }) {
            g.inconclusive(
                scenario,
                "the session never sold the named instrument's lot on the moved-on trading day, so no cycle closed",
                json!({ "instrument": A_SHARE, "prompts": prompt_states(&g, &desk_id) }),
            );
        } else {
            // The cycle exists, so the rest is the daemon's own: its evaluation
            // is queued in the same unit (root §13.2).
            let cycle: String = g.scalar(
                "SELECT id FROM position_cycles WHERE desk_id = ?1 AND instrument_id = ?2 \
                 ORDER BY closed_at_ns LIMIT 1",
                &[&desk_id, &A_SHARE],
            );
            assert!(
                waited(SETTLES, "the cycle's evaluation prompt", || {
                    g.scalar::<i64>(
                        "SELECT count(*) FROM prompts WHERE desk_id = ?1 AND kind = 'EVALUATION'",
                        &[&desk_id],
                    ) >= 1
                }),
                "a closed cycle queues its EVALUATION prompt (root §13.2)"
            );
            g.note(
                scenario,
                "the same session sold the lot it had bought on the staged trading day: the native cycle closed and MarketRig queued its evaluation",
                json!({ "cycle": cycle, "prompts": prompt_states(&g, &desk_id) }),
            );
        }
    }

    // The three limitations the cell cannot remove, recorded in the bundle
    // rather than argued away (`a-share-engine` SPEC §2.1, §6).
    g.note(
        scenario,
        "limitations of this cell, recorded with its evidence",
        json!({
            "source_delay": "UNKNOWN — the provider publishes no source timestamp, so no fill here is a claim about market time",
            "corporate_actions": "a holding carried across a dividend or split date is not adjusted, so its realized figure is incomparable and must never be attributed to strategy",
            "staged_calendar": format!(
                "the buy was dated {buy_day} and the sell {sell_day} through the controlled clock, but both traded against the same day's real quotes minutes apart: the realized figure is a harness artifact, not a market outcome"
            ),
        }),
    );

    g.inconclusive(
        scenario,
        "what the session read through `marketrig research hithink` is the operator's to judge: \
         the passthrough writes no row and no event (§4.1)",
        json!({ "commands": [income, valuation] }),
    );

    // The key leaves the credential store before the bundle is kept, so the
    // evidence carries no secret at all (§1.2's DELETE, root §16).
    let (status, removed) = g.api(scenario, &endpoint, "DELETE", "/research/hithink", None);
    assert_eq!(status, 200, "{removed}");
    assert_eq!(removed["state"], "UNCONFIGURED", "{removed}");
    assert_eq!(removed["api_key_present"], false, "{removed}");

    console.detach();
    finish(&mut g, scenario, daemon, &desk_id);

    // Everything the run wrote is on disk now, the daemon included.
    let leaked = carrying(&g.out, &key);
    assert!(
        leaked.is_empty(),
        "the operator's key appears in the bundle, which nothing may carry (§1.2): {leaked:?}"
    );
    g.note(
        scenario,
        "the key is in no file of the bundle",
        json!({ "scanned": g.out.display().to_string() }),
    );
}

// ---------------------------------------------------------------------------
// E8 — a producer hands the desk its findings, attended
// (`sdd/features/event-triggers/SPEC.md` §7.2)
// ---------------------------------------------------------------------------

/// The request id a producer would repeat after a failure, and the three lines
/// it hands over. Both are made up: E8 proves the path, not the research.
const E8_REQUEST: &str = "e8-findings-2026-09-11";
const E8_INPUT: &str = "Toolmakers led the tape again: three of the five names we watch closed up.\n\
                        Two of them reported after the close and neither guided down.\n\
                        Nothing in the group traded on volume worth calling unusual.";

#[test]
fn e8_codex_cli() {
    handoff("E8", "codex", "Codex CLI");
}

#[test]
fn e8_claude_code() {
    handoff("E8", "claude", "Claude Code");
}

/// **E8 — a producer hands the desk its findings** (`event-triggers` §7.2). The
/// job is defined once, with no schedule, and runs only when something invokes
/// it; the harness plays the producer. As in E4, MarketRig launches the runtime
/// itself and the operator's console becomes the desk's terminal.
///
/// Mechanical and asserted: the acceptance, the firing's request identity and
/// input, the delivery the daemon's own rows must agree with, and the replay
/// that answers the original firing and queues nothing. Whether the text
/// reached the session as its own input and what the agent made of it is the
/// operator's to confirm and ends `INCONCLUSIVE`. No trade is made.
fn handoff(scenario: &str, cell: &str, runtime: &str) {
    if std::env::var(CELL).unwrap_or_default() != cell {
        eprintln!(
            "{scenario} ({runtime}) skipped: set {CELL}={cell} to run this cell attended, \
             and pass `-- --nocapture` so its instructions are visible."
        );
        return;
    }
    let _terminal = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);

    let mut g = Harness::new(&format!("experiment-e8-{cell}"));
    g.real_feed();
    let daemon = g.spawn(scenario);
    let endpoint = daemon.endpoint.clone();

    // Startup discovery is skipped under the test seam (R3 §2), so the cell
    // asks for it: the operator's own installation, resolved from the PATH.
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

    let desk = format!("{cell}-e8-{}", marketrig_acceptance::now_secs());
    let (status, created) = g.api(
        scenario,
        &endpoint,
        "POST",
        "/desks",
        Some(&json!({ "name": desk, "runtime": cell }).to_string()),
    );
    assert_eq!(status, 201, "{created}");
    let desk_id = created["id"].as_str().expect("id").to_owned();

    // The job, defined once and never again: no schedule, so it runs only when
    // a producer invokes it (§1).
    let (exit, trigger) = g.cli_json(
        scenario,
        &[
            "--json",
            "trigger",
            "create",
            &desk,
            "--name",
            "review-research",
            "--brief",
            "Read the findings handed to you and answer with a one-sentence summary of them. \
             Do not trade.",
        ],
    );
    assert_eq!(exit, 0, "{trigger}");
    let trigger_id = trigger["id"].as_str().expect("id").to_owned();
    assert_eq!(
        trigger["recurrence"], "RECURRING",
        "a create with no schedule is recurring (§1): {trigger}"
    );
    assert!(
        trigger.get("schedule").is_none(),
        "the job has no schedule (§1): {trigger}"
    );

    let instructions = format!(
        "\n\
         ===========================================================================\n\
         {scenario} — a producer hands the desk its findings (feature SPEC §7.2)\n\
         ===========================================================================\n\
         \n\
         Desk:      {desk}\n\
         Runtime:   {runtime} {version} at {path}\n\
         Trigger:   review-research (recurring, no schedule)\n\
         Data root: {root}\n\
         Evidence:  {root}\n\
         \n\
         Nothing to register and nothing to start, as in E4: MarketRig launches\n\
         the runtime itself and this console becomes the desk's terminal.\n\
         \n\
         1. Answer whatever {runtime} asks on first launch — trust, permissions,\n\
         \x20  the development channel — and nothing else.\n\
         \n\
         2. The harness now plays a producer: it invokes `review-research` once,\n\
         \x20  with a request id it could safely repeat, handing over three lines\n\
         \x20  of made-up findings. There is no schedule and nothing is due; the\n\
         \x20  invocation is the whole reason the job runs.\n\
         \n\
         3. After the orientation MarketRig sends first, you must see\n\
         \x20  `MarketRig TRIGGER_RESULT <id>:` arrive as the session's own input,\n\
         \x20  with those three lines inside it. Let the session answer with its\n\
         \x20  one-sentence summary and read what it says. Whether it did is\n\
         \x20  recorded INCONCLUSIVE — the daemon's rows are what fail the cell.\n\
         \n\
         4. The harness then repeats the same request id. Nothing must reach the\n\
         \x20  session a second time: a replay answers the first firing.\n\
         \n\
         5. No trade is made in this cell.\n\
         \n\
         The harness waits up to {patience} minutes per step. While this console is\n\
         \x20  the terminal, ^C goes to the session, not the harness: abort from another\n\
         \x20  terminal with `pkill -f deps/experiment-`, then `stty sane` here.\n\
         ===========================================================================\n",
        version = row["version"].as_str().unwrap_or_default(),
        path = row["executable_path"].as_str().unwrap_or_default(),
        root = g.out.display(),
        patience = PATIENCE.as_secs() / 60,
    );
    println!("{instructions}");
    g.write_evidence("instructions-e8.txt", &instructions);
    g.note(
        scenario,
        "attended cell prepared; the console is about to become the desk's terminal",
        json!({ "desk": desk, "desk_id": desk_id, "trigger": trigger }),
    );

    // The console is the terminal from here until the cell ends.
    let console = console::attach(&endpoint, &desk_id);

    let (exit, accepted) = g.cli_json(
        scenario,
        &[
            "--json",
            "trigger",
            "invoke",
            &desk,
            "review-research",
            "--request-id",
            E8_REQUEST,
            "--input",
            E8_INPUT,
        ],
    );
    assert_eq!(exit, 0, "{accepted}");
    assert_eq!(accepted["outcome"], "ACCEPTED", "{accepted}");
    let firing_id = accepted["firing"]["id"].as_str().expect("id").to_owned();
    assert_eq!(accepted["firing"]["trigger_id"], trigger_id.as_str());
    assert_eq!(accepted["firing"]["request_id"], E8_REQUEST);
    assert_eq!(
        accepted["firing"]["input"], E8_INPUT,
        "the firing carries the producer's text verbatim (§2.2)"
    );
    g.note(
        scenario,
        "the producer's invocation was accepted as one firing carrying its request id and its input",
        json!({ "firing": accepted["firing"] }),
    );

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

    let delivered = |g: &Harness| -> i64 {
        g.scalar(
            "SELECT count(*) FROM prompts WHERE desk_id = ?1 AND kind = 'TRIGGER_RESULT' \
             AND state = 'DELIVERED'",
            &[&desk_id],
        )
    };
    if !waited(
        PATIENCE,
        "the invoked firing's result to be delivered",
        || delivered(&g) >= 1,
    ) {
        g.inconclusive(
            scenario,
            "the invoked result was never delivered within the cell's patience",
            json!({ "firing_id": firing_id, "prompts": prompt_states(&g, &desk_id) }),
        );
        console.detach();
        finish(&mut g, scenario, daemon, &desk_id);
        return;
    }
    assert_delivery(&g, &desk_id);
    g.note(
        scenario,
        "MarketRig delivered the invoked firing's result to the session it started, and the rows agree",
        json!({ "prompts": prompt_states(&g, &desk_id) }),
    );
    g.inconclusive(
        scenario,
        "whether the producer's three lines appeared as the session's own input, and what the agent \
         made of them, is the operator's to confirm on the console",
        json!({ "expect": format!("MarketRig TRIGGER_RESULT <id>: carrying {E8_REQUEST}") }),
    );

    // The replay, in the same run: the producer repeats its request id and gets
    // the first firing back, with nothing new anywhere (§2.3).
    let prompts_before = g.scalar::<i64>(
        "SELECT count(*) FROM prompts WHERE desk_id = ?1",
        &[&desk_id],
    );
    println!(
        "\r\n{scenario}: repeating the same request id now — nothing must reach the session.\r\n"
    );
    let (exit, duplicate) = g.cli_json(
        scenario,
        &[
            "--json",
            "trigger",
            "invoke",
            &desk,
            "review-research",
            "--request-id",
            E8_REQUEST,
            "--input",
            E8_INPUT,
        ],
    );
    assert_eq!(exit, 0, "a replay is success: {duplicate}");
    assert_eq!(duplicate["outcome"], "DUPLICATE", "{duplicate}");
    assert_eq!(duplicate["firing"]["id"], firing_id.as_str());
    assert_eq!(
        g.scalar::<i64>(
            "SELECT count(*) FROM firings WHERE trigger_id = ?1",
            &[&trigger_id],
        ),
        1,
        "a replay writes no second firing (§2.2)"
    );
    assert_eq!(
        g.scalar::<i64>(
            "SELECT count(*) FROM prompts WHERE desk_id = ?1",
            &[&desk_id],
        ),
        prompts_before,
        "a replay queues no second prompt (§2.2)"
    );
    g.note(
        scenario,
        "the repeated request id answered the original firing and left no second firing and no second prompt",
        json!({ "duplicate": duplicate, "prompts": prompt_states(&g, &desk_id) }),
    );

    console.detach();
    finish(&mut g, scenario, daemon, &desk_id);
}

/// Every file under `root` whose bytes contain `needle`, named relative to it.
/// ponytail: a naive scan of a bundle of a few megabytes; a real search tool if
/// a bundle ever grows the way E6's provisioned one does.
fn carrying(root: &std::path::Path, needle: &str) -> Vec<String> {
    let needle = needle.as_bytes();
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if std::fs::read(&path)
                .unwrap_or_default()
                .windows(needle.len())
                .any(|window| window == needle)
            {
                found.push(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                );
            }
        }
    }
    found
}

/// The projected skill tree by skill name (§5.2): per file its path inside the
/// tree, its size, and a content sum, so an edit that keeps a file's length
/// still reads as a change. It is all the harness can see of the desk's
/// OpenViking skills without the desk key.
fn skills(workspace: &std::path::Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let root = workspace.join(".agents").join("skills");
    let mut tree = std::collections::BTreeMap::new();
    for skill in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        if !skill.path().is_dir() {
            continue;
        }
        let mut files = Vec::new();
        let mut dirs = vec![skill.path()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let bytes = std::fs::read(&path).unwrap_or_default();
                let sum: u64 = bytes.iter().map(|byte| u64::from(*byte)).sum();
                let named = path.strip_prefix(&root).unwrap_or(&path).display();
                files.push(format!("{named} {} {sum}", bytes.len()));
            }
        }
        files.sort();
        tree.insert(skill.file_name().to_string_lossy().into_owned(), files);
    }
    tree
}

/// The plugin's own state, pending queue, and `logs/` under `OPENVIKING_HOME`
/// (§4.3), which the bundle already holds: the data root *is* the evidence
/// directory (root §17), so this only names what landed there.
fn plugin_state(home: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut dirs = vec![home.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            found.push(
                path.strip_prefix(home)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }
    found.sort();
    found
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
            // ConPTY switched the hosting terminal into win32-input-mode
            // (`ESC[?9001h`, relayed with the session's own output) and the
            // dead session never switches it back, so this console does, or
            // every later keypress in the operator's window is echoed as a
            // key report. Harmless elsewhere; a terminal that never saw the
            // mode ignores the reset.
            {
                use std::io::Write as _;
                let mut out = std::io::stdout();
                let _ = out.write_all(b"\x1b[?9001l");
                let _ = out.flush();
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
                            if serde_json::from_str::<serde_json::Value>(&text)
                                .is_ok_and(|frame| frame.get("attached").is_some())
                            {
                                continue;
                            }
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
