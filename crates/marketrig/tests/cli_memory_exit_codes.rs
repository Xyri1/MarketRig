//! `cli::memory` — the `memory` group's grammar, routes, human output, and
//! 0/1/2 mapping (feature SPEC `r4-memory-skills-loop` §4.4, root §11.1)
//! against the shared fake endpoint of `tests/common/mod.rs`.

use std::process::Output;

mod common;

use common::{Requests, code, fake_daemon, health_ok, marketrig, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";

const DESKS: &str =
    r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY"}]}"#;

const STATUS: &str = r#"{"child":{"state":"UNAVAILABLE","executable_path":"/opt/hindsight-api","failure_code":"CHILD_FAILED","failure_message":"it exited","live":"NOT_STARTED"},"provider":{"base_url":"http://127.0.0.1:9/v1","llm_model":"llm-1","embedding_model":"emb-1","api_key_present":true},"desk_id":"01997f00-0000-7000-8000-00000000000a"}"#;

const RECALLED: &str = r#"{"results":[{"id":"m-1","text":"buy the dip only with a thesis","type":"experience","context":null,"tags":["lesson"],"metadata":{"source":"INTERACTIVE"},"occurred_start":"2026-09-04T00:00:00Z","mentioned_at":null}]}"#;

const REFLECTED: &str = r#"{"text":"two lessons, one theme","based_on":[{"id":"m-1","text":"a lesson","type":"experience"},{"id":"m-2","text":"another","type":"experience"}]}"#;

fn respond(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, DESKS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/memory" => (200, STATUS),
        "POST /desks/01997f00-0000-7000-8000-00000000000a/memory/retain" => {
            (200, r#"{"items_count":1}"#)
        }
        "POST /desks/01997f00-0000-7000-8000-00000000000a/memory/recall" => (200, RECALLED),
        "POST /desks/01997f00-0000-7000-8000-00000000000a/memory/reflect" => (200, REFLECTED),
        _ => (
            404,
            r#"{"code":"DESK_NOT_FOUND","message":"No such desk."}"#,
        ),
    }
}

fn calls(requests: &Requests) -> Vec<(String, String)> {
    requests.lock().expect("request log").drain(..).collect()
}

fn lines(output: &Output) -> Vec<String> {
    String::from_utf8(output.stdout.clone())
        .expect("utf-8 stdout")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The four commands, their routes and bodies, and the §4.4 output shapes.
#[test]
fn the_four_commands_hit_the_documented_routes() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    // status: `field: value` in the route's key order, child then provider.
    let status = marketrig(root.path(), &["memory", "status", "alpha"]);
    assert_eq!(code(&status), 0, "{status:?}");
    assert_eq!(
        calls(&requests)
            .into_iter()
            .map(|(route, _)| route)
            .collect::<Vec<_>>(),
        [
            "GET /health".to_string(),
            "GET /desks".to_string(),
            format!("GET /desks/{DESK}/memory"),
        ]
    );
    assert_eq!(
        lines(&status),
        [
            "state: UNAVAILABLE",
            "executable_path: /opt/hindsight-api",
            "failure_code: CHILD_FAILED",
            "failure_message: it exited",
            "live: NOT_STARTED",
            "base_url: http://127.0.0.1:9/v1",
            "llm_model: llm-1",
            "embedding_model: emb-1",
            "api_key_present: true",
            format!("desk_id: {DESK}").as_str(),
        ]
    );

    // retain: the daemon gets exactly what the flags carried, and no more.
    let retain = marketrig(
        root.path(),
        &[
            "memory",
            "retain",
            DESK,
            "--content",
            "buy the dip only with a thesis",
            "--context",
            "cycle 7",
            "--tag",
            "lesson",
            "--tag",
            "AAPL.XNAS",
        ],
    );
    assert_eq!(code(&retain), 0, "{retain:?}");
    let sent = calls(&requests);
    assert_eq!(
        sent.last().expect("a request").0,
        format!("POST /desks/{DESK}/memory/retain")
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sent.last().expect("a request").1)
            .expect("a JSON body"),
        serde_json::json!({
            "content": "buy the dip only with a thesis",
            "context": "cycle 7",
            "tags": ["lesson", "AAPL.XNAS"],
        })
    );
    assert_eq!(lines(&retain), ["retained 1 item"]);
    assert_eq!(
        sent.len(),
        2,
        "a canonical UUID is never resolved through the listing"
    );

    // recall: one tab-separated row per result, like every other listing.
    let recall = marketrig(
        root.path(),
        &[
            "memory", "recall", DESK, "--query", "the dip", "--budget", "high", "--tag", "lesson",
        ],
    );
    assert_eq!(code(&recall), 0, "{recall:?}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls(&requests).last().expect("a request").1)
            .expect("a JSON body"),
        serde_json::json!({ "query": "the dip", "budget": "high", "tags": ["lesson"] })
    );
    assert_eq!(
        lines(&recall),
        ["m-1\texperience\tbuy the dip only with a thesis"]
    );

    // reflect: the text, then how many memories it rests on.
    let reflect = marketrig(
        root.path(),
        &["memory", "reflect", DESK, "--query", "what have I learned"],
    );
    assert_eq!(code(&reflect), 0, "{reflect:?}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls(&requests).last().expect("a request").1)
            .expect("a JSON body"),
        serde_json::json!({ "query": "what have I learned" }),
        "an omitted budget is the daemon's default, never the CLI's"
    );
    assert_eq!(
        lines(&reflect),
        ["two lessons, one theme", "based on: 2 memories"]
    );

    // `--json` is the route's body verbatim, for all four.
    for (args, body) in [
        (vec!["memory", "status", DESK], STATUS),
        (
            vec!["memory", "recall", DESK, "--query", "the dip"],
            RECALLED,
        ),
        (
            vec!["memory", "reflect", DESK, "--query", "the dip"],
            REFLECTED,
        ),
    ] {
        let mut argv = vec!["--json"];
        argv.extend(args);
        let output = marketrig(root.path(), &argv);
        assert_eq!(code(&output), 0, "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout)
                .expect("utf-8 stdout")
                .trim(),
            body
        );
    }
}

/// An empty recall reads as such rather than as silence (§4.4).
#[test]
fn an_empty_recall_says_no_results() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        _ => (200, r#"{"results":[]}"#),
    });
    write_endpoint(root.path(), port);
    let recall = marketrig(root.path(), &["memory", "recall", DESK, "--query", "gone"]);
    assert_eq!(code(&recall), 0, "{recall:?}");
    assert_eq!(lines(&recall), ["no results"]);
}

/// Every grammar failure §4.4 names is clap's own exit `2`, and `--file` is
/// read and bounded before a daemon is ever contacted.
#[test]
fn the_grammar_and_the_file_bound_exit_two() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    let big = root.path().join("big.txt");
    std::fs::write(&big, vec![b'x'; 64 * 1024 + 1]).expect("write the oversize file");
    let ok = root.path().join("lesson.txt");
    std::fs::write(&ok, "a lesson from a file").expect("write the content file");
    let missing = root.path().join("nowhere.txt");

    for (label, args) in [
        ("no material at all", vec!["memory", "retain", DESK]),
        (
            "both material flags",
            vec![
                "memory",
                "retain",
                DESK,
                "--content",
                "c",
                "--file",
                ok.to_str().expect("path"),
            ],
        ),
        ("no query", vec!["memory", "recall", DESK]),
        (
            "an unknown budget",
            vec![
                "memory", "recall", DESK, "--query", "q", "--budget", "enormous",
            ],
        ),
        ("no desk", vec!["memory", "status"]),
        (
            "a file over 64 KiB",
            vec![
                "memory",
                "retain",
                DESK,
                "--file",
                big.to_str().expect("path"),
            ],
        ),
        (
            "an unreadable file",
            vec![
                "memory",
                "retain",
                DESK,
                "--file",
                missing.to_str().expect("path"),
            ],
        ),
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 2, "{label}: {output:?}");
    }
    assert!(
        calls(&requests).is_empty(),
        "none of those reached the daemon"
    );

    // The same file just inside the bound is read and sent as the content.
    let sent = marketrig(
        root.path(),
        &[
            "memory",
            "retain",
            DESK,
            "--file",
            ok.to_str().expect("path"),
        ],
    );
    assert_eq!(code(&sent), 0, "{sent:?}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&calls(&requests).last().expect("a request").1)
            .expect("a JSON body"),
        serde_json::json!({ "content": "a lesson from a file" })
    );
}

/// A daemon-reported memory failure is exit `1` with the envelope's own words
/// (§4.4): trading and triggers go on, and only this command fails.
#[test]
fn a_refused_operation_exits_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        _ => (
            503,
            r#"{"code":"MEMORY_UNAVAILABLE","message":"The memory child is unavailable: it exited."}"#,
        ),
    });
    write_endpoint(root.path(), port);

    let retain = marketrig(
        root.path(),
        &["memory", "retain", DESK, "--content", "a lesson"],
    );
    assert_eq!(code(&retain), 1, "{retain:?}");
    assert_eq!(
        String::from_utf8(retain.stderr)
            .expect("utf-8 stderr")
            .trim(),
        "error: MEMORY_UNAVAILABLE: The memory child is unavailable: it exited."
    );
}

/// The CLI's own budget is above the daemon's, so a slow operation surfaces the
/// daemon's answer and never a client that gave up (§4.4). The agent's default
/// is ten seconds; this daemon takes longer than that on purpose.
#[test]
fn retain_outlasts_the_agents_default_budget() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(|route, _| match route {
        "GET /health" => (200, health_ok()),
        _ => {
            std::thread::sleep(std::time::Duration::from_secs(12));
            (200, r#"{"items_count":1}"#)
        }
    });
    write_endpoint(root.path(), port);

    let retain = marketrig(
        root.path(),
        &["memory", "retain", DESK, "--content", "a slow lesson"],
    );
    assert_eq!(code(&retain), 0, "{retain:?}");
    assert_eq!(lines(&retain), ["retained 1 item"]);
}
