//! `cli::history_exit_codes` — the `history` group's 0/1/2/3 mapping (feature
//! SPEC `r1-equity-paper-trading` §9) against a fake endpoint.
//!
//! The fake endpoint is the R0 check's, shared through `tests/common/mod.rs`: a
//! plain `TcpListener` serving canned responses by "METHOD /path".

use std::process::Output;

mod common;

use common::{code, fake_daemon, health_ok, marketrig, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";

const FILLS: &str = r#"{"fills":[{"id":"f2","client_order_id":"o-2","trade_id":"t2","instrument_id":"0700.XHKG","side":"SELL","quantity":"100","price":"301.00","commission":"33.11","currency":"HKD","occurred_at_ns":400},{"id":"f1","client_order_id":"o-1","trade_id":"t1","instrument_id":"AAPL.XNAS","side":"BUY","quantity":"1","price":"191.20","commission":"0","currency":"USD","occurred_at_ns":300}]}"#;

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

#[test]
fn history_exit_codes_zero_on_success() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (
            200,
            r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY"}]}"#,
        ),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/history/fills" => (200, FILLS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/history/orders" => (
            200,
            r#"{"orders":[{"client_order_id":"o-2","instrument_id":"0700.XHKG","side":"BUY","type":"LIMIT","quantity":"100","price":"300.00","status":"ACCEPTED"},{"client_order_id":"o-1","instrument_id":"AAPL.XNAS","side":"BUY","type":"MARKET","quantity":"1","price":null,"kind":"OrderFilled"}]}"#,
        ),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/history/cycles" => (
            200,
            r#"{"cycles":[{"id":"c1","position_id":"p1","instrument_id":"AAPL.XNAS","opened_at_ns":1,"closed_at_ns":2,"realized_pnl":"-93.71","currency":"USD"}]}"#,
        ),
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    });
    write_endpoint(root.path(), port);

    // Human rows, newest first exactly as the daemon ordered them.
    let fills = marketrig(root.path(), &["history", "fills", DESK]);
    assert_eq!(code(&fills), 0, "{fills:?}");
    let lines: Vec<String> = stdout(&fills).lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        [
            "400\t0700.XHKG\tSELL\t100\t301.00\t33.11\tHKD",
            "300\tAAPL.XNAS\tBUY\t1\t191.20\t0\tUSD",
        ],
        "fill rows"
    );

    // `--json` is the route's body verbatim.
    let json = marketrig(root.path(), &["--json", "history", "fills", DESK]);
    assert_eq!(code(&json), 0, "{json:?}");
    assert_eq!(stdout(&json).trim(), FILLS);

    // A name resolves through the daemon's own listing, as `desk show` does.
    let by_name = marketrig(root.path(), &["history", "fills", "alpha"]);
    assert_eq!(code(&by_name), 0, "{by_name:?}");
    assert_eq!(by_name.stdout, fills.stdout);

    // A market order has no price and reports its latest event kind; neither
    // absence panics or shifts a column.
    let orders = marketrig(root.path(), &["history", "orders", DESK]);
    assert_eq!(code(&orders), 0, "{orders:?}");
    let lines: Vec<String> = stdout(&orders).lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        [
            "o-2\t0700.XHKG\tBUY\tLIMIT\t100\t300.00\tACCEPTED",
            "o-1\tAAPL.XNAS\tBUY\tMARKET\t1\t\tOrderFilled",
        ],
        "order rows"
    );

    let cycles = marketrig(root.path(), &["history", "cycles", DESK]);
    assert_eq!(code(&cycles), 0, "{cycles:?}");
    assert_eq!(
        stdout(&cycles).trim_end(),
        "2\tAAPL.XNAS\t-93.71\tUSD",
        "cycle rows"
    );
}

#[test]
fn history_exit_codes_one_on_unknown_desk() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, r#"{"desks":[]}"#),
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    });
    write_endpoint(root.path(), port);

    let output = marketrig(root.path(), &["history", "cycles", "nowhere"]);
    assert_eq!(code(&output), 1, "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
        "error: DESK_NOT_FOUND: No desk is named nowhere.\n"
    );
}

#[test]
fn history_exit_codes_one_on_daemon_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        _ => (
            409,
            r#"{"code":"DESK_NOT_READY","message":"Desk alpha is not READY."}"#,
        ),
    });
    write_endpoint(root.path(), port);

    let human = marketrig(root.path(), &["history", "orders", DESK]);
    assert_eq!(code(&human), 1, "{human:?}");
    assert_eq!(
        String::from_utf8(human.stderr).expect("utf-8 stderr"),
        "error: DESK_NOT_READY: Desk alpha is not READY.\n"
    );

    let json = marketrig(root.path(), &["--json", "history", "orders", DESK]);
    assert_eq!(code(&json), 1, "{json:?}");
    assert_eq!(
        stdout(&json).trim(),
        r#"{"code":"DESK_NOT_READY","message":"Desk alpha is not READY."}"#
    );
}

#[test]
fn history_exit_codes_two_on_usage_errors() {
    let root = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["history"],
        vec!["history", "bogus", DESK],
        vec!["history", "fills"],
        vec!["history", "fills", DESK, "--json"], // the global flag precedes the group
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 2, "{args:?} -> {output:?}");
    }
}

#[test]
fn history_exit_codes_three_without_a_daemon() {
    let root = tempfile::tempdir().expect("tempdir");
    let output = marketrig(root.path(), &["history", "fills", DESK]);
    assert_eq!(code(&output), 3, "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf-8 stderr")
            .starts_with("error: DAEMON_UNREACHABLE: ")
    );
}
