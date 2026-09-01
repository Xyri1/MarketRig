//! The acceptance experiment: E1 and E2, the first attended scenarios.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §10.3 and root
//! `sdd/SPEC.md` §17, per D75. One operator-attended run per platform-and-runtime
//! cell, on real Yahoo and a real runtime CLI, with MCP registration performed by
//! hand (R1 keeps it operator-performed, feature SPEC §8).
//!
//! **Operator variable:** `MARKETRIG_EXPERIMENT` selects the cell — `codex` runs
//! E1, `claude` runs E2. Unset or anything else skips both cleanly, which is what
//! CI and every unattended `cargo test` do. The instructions are printed, so run
//! the selected cell with output:
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

use std::time::Duration;

use marketrig_acceptance::{Harness, parse, waited};
use serde_json::json;

/// The operator variable and its two cells.
const CELL: &str = "MARKETRIG_EXPERIMENT";

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
