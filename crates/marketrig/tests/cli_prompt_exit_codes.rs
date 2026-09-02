//! `cli::prompt_exit_codes` — the `prompt` group's 0/1/3 mapping, its routes,
//! and its human output (feature SPEC `r2-scheduled-triggers` §9, root §11.1)
//! against the shared fake endpoint of `tests/common/mod.rs`.

use std::process::Output;

use serde_json::json;

mod common;

use common::{Requests, code, fake_daemon, health_ok, marketrig, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";
const PROMPT: &str = "01997f00-0000-7000-8000-00000000000f";

const DESKS: &str =
    r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY"}]}"#;

const PROMPTS: &str = r#"{"prompts":[{"id":"01997f00-0000-7000-8000-00000000000f","desk_id":"01997f00-0000-7000-8000-00000000000a","kind":"TRIGGER_RESULT","state":"QUEUED","created_at_ns":20},{"id":"01997f00-0000-7000-8000-000000000010","desk_id":"01997f00-0000-7000-8000-00000000000a","kind":"EVALUATION","state":"DELIVERED","created_at_ns":19}]}"#;

const PROMPT_RESOURCE: &str = r#"{"id":"01997f00-0000-7000-8000-00000000000f","desk_id":"01997f00-0000-7000-8000-00000000000a","kind":"TRIGGER_RESULT","state":"QUEUED","created_at_ns":20,"payload":{"kind":"TRIGGER_RESULT","firing_id":"01997f00-0000-7000-8000-00000000000c","execution":null}}"#;

fn respond(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, DESKS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/prompts" => (200, PROMPTS),
        "GET /desks/01997f00-0000-7000-8000-00000000000a/prompts/01997f00-0000-7000-8000-00000000000f" => {
            (200, PROMPT_RESOURCE)
        }
        _ => (
            404,
            r#"{"code":"PROMPT_NOT_FOUND","message":"No such prompt."}"#,
        ),
    }
}

fn routes(requests: &Requests) -> Vec<String> {
    requests
        .lock()
        .expect("request log")
        .drain(..)
        .map(|(route, _)| route)
        .collect()
}

fn lines(output: &Output) -> Vec<String> {
    String::from_utf8(output.stdout.clone())
        .expect("utf-8 stdout")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn list_and_show_hit_the_documented_routes() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    // Listing: id, kind, state, created — the delivery facts without payloads.
    let list = marketrig(root.path(), &["prompt", "list", "alpha"]);
    assert_eq!(code(&list), 0, "{list:?}");
    assert_eq!(
        routes(&requests),
        [
            "GET /health".to_string(),
            "GET /desks".to_string(),
            format!("GET /desks/{DESK}/prompts"),
        ]
    );
    assert_eq!(
        lines(&list),
        [
            format!("{PROMPT}\tTRIGGER_RESULT\tQUEUED\t20"),
            "01997f00-0000-7000-8000-000000000010\tEVALUATION\tDELIVERED\t19".to_string(),
        ]
    );

    // A single read adds the payload, printed as compact JSON.
    let show = marketrig(root.path(), &["prompt", "show", DESK, PROMPT]);
    assert_eq!(code(&show), 0, "{show:?}");
    assert_eq!(
        routes(&requests),
        [
            "GET /health".to_string(),
            format!("GET /desks/{DESK}/prompts/{PROMPT}"),
        ],
        "a prompt id is never resolved through a listing"
    );
    assert_eq!(
        lines(&show),
        [
            format!("id: {PROMPT}"),
            format!("desk_id: {DESK}"),
            "kind: TRIGGER_RESULT".to_string(),
            "state: QUEUED".to_string(),
            "created_at_ns: 20".to_string(),
            format!(
                "payload: {}",
                json!({
                    "kind": "TRIGGER_RESULT",
                    "firing_id": "01997f00-0000-7000-8000-00000000000c",
                    "execution": null,
                })
            ),
        ]
    );

    // `--json` is the route's body verbatim.
    let json = marketrig(root.path(), &["--json", "prompt", "show", DESK, PROMPT]);
    assert_eq!(code(&json), 0, "{json:?}");
    assert_eq!(
        String::from_utf8(json.stdout).expect("utf-8 stdout").trim(),
        PROMPT_RESOURCE
    );
}

#[test]
fn an_unknown_prompt_exits_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, _) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    let human = marketrig(
        root.path(),
        &[
            "prompt",
            "show",
            DESK,
            "01997f00-0000-7000-8000-0000000000aa",
        ],
    );
    assert_eq!(code(&human), 1, "{human:?}");
    assert_eq!(
        String::from_utf8(human.stderr).expect("utf-8 stderr"),
        "error: PROMPT_NOT_FOUND: No such prompt.\n"
    );

    let json = marketrig(
        root.path(),
        &[
            "--json",
            "prompt",
            "show",
            DESK,
            "01997f00-0000-7000-8000-0000000000aa",
        ],
    );
    assert_eq!(code(&json), 1, "{json:?}");
    assert_eq!(
        String::from_utf8(json.stdout).expect("utf-8 stdout").trim(),
        r#"{"code":"PROMPT_NOT_FOUND","message":"No such prompt."}"#
    );
}

#[test]
fn usage_errors_exit_two() {
    let root = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["prompt"],
        vec!["prompt", "list"],
        vec!["prompt", "show", DESK],
        vec!["prompt", "list", "alpha", "--json"],
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 2, "{args:?} -> {output:?}");
    }
}

#[test]
fn without_a_daemon_the_group_exits_three() {
    let root = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["prompt", "list", "alpha"],
        vec!["prompt", "show", DESK, PROMPT],
    ] {
        let output = marketrig(root.path(), &args);
        assert_eq!(code(&output), 3, "{args:?} -> {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).starts_with("error: DAEMON_UNREACHABLE: "),
            "{args:?} -> {output:?}"
        );
    }
}
