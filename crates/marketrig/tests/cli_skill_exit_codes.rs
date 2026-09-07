//! `cli::skill_exit_codes` — the `skill` group's file read, its request, and
//! its human output (feature SPEC `openviking-continuity` §5.5) against the
//! shared fake endpoint of `tests/common/mod.rs`.

use serde_json::{Value, json};

mod common;

use common::{Requests, code, fake_daemon, health_ok, marketrig, write_endpoint};

const DESK: &str = "01997f00-0000-7000-8000-00000000000a";

const DESKS: &str =
    r#"{"desks":[{"id":"01997f00-0000-7000-8000-00000000000a","name":"alpha","state":"READY"}]}"#;

const WRITTEN: &str =
    r#"{"name":"spread-watch","path":"/desks/alpha/.agents/skills/spread-watch/SKILL.md"}"#;

const REMOVED: &str = r#"{"name":"spread-watch"}"#;

fn respond(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        "GET /desks" => (200, DESKS),
        "PUT /desks/01997f00-0000-7000-8000-00000000000a/skills/spread-watch" => (200, WRITTEN),
        "DELETE /desks/01997f00-0000-7000-8000-00000000000a/skills/spread-watch" => (200, REMOVED),
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    }
}

fn sent(requests: &Requests) -> Vec<(String, Value)> {
    requests
        .lock()
        .expect("request log")
        .drain(..)
        .map(|(route, body)| (route, serde_json::from_str(&body).unwrap_or(Value::Null)))
        .collect()
}

/// The frontmatter names the skill, so the CLI puts that name in the path and
/// the content in the body; `delete` takes the name as an argument.
#[test]
fn put_takes_its_name_from_the_frontmatter() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);

    let file = root.path().join("SKILL.md");
    let content = "---\nname: spread-watch\ndescription: Watch spreads.\n---\n\nStep one.\n";
    std::fs::write(&file, content).expect("write the skill");

    let put = marketrig(
        root.path(),
        &[
            "skill",
            "put",
            "alpha",
            "--file",
            file.to_str().expect("utf-8 path"),
        ],
    );
    assert_eq!(code(&put), 0, "{put:?}");
    assert_eq!(
        sent(&requests),
        [
            ("GET /health".to_string(), Value::Null),
            ("GET /desks".to_string(), Value::Null),
            (
                format!("PUT /desks/{DESK}/skills/spread-watch"),
                json!({ "content": content }),
            ),
        ]
    );
    assert_eq!(
        String::from_utf8(put.stdout).expect("utf-8 stdout"),
        "name: spread-watch\npath: /desks/alpha/.agents/skills/spread-watch/SKILL.md\n"
    );

    let deleted = marketrig(root.path(), &["skill", "delete", DESK, "spread-watch"]);
    assert_eq!(code(&deleted), 0, "{deleted:?}");
    assert_eq!(
        sent(&requests),
        [
            ("GET /health".to_string(), Value::Null),
            (
                format!("DELETE /desks/{DESK}/skills/spread-watch"),
                Value::Null,
            ),
        ],
        "a UUID desk skips GET /desks"
    );
}

/// The file is read before the daemon is contacted: too large, unreadable, or
/// carrying no frontmatter name is a usage error, and nothing is sent.
#[test]
fn an_unusable_file_is_a_usage_error_before_contact() {
    let root = tempfile::tempdir().expect("tempdir");
    let (port, requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);
    let put = |path: &std::path::Path| {
        marketrig(
            root.path(),
            &["skill", "put", "alpha", "--file", path.to_str().unwrap()],
        )
    };

    let big = root.path().join("big.md");
    let mut source = "---\nname: spread-watch\ndescription: d\n---\n\n".to_string();
    source.push_str(&"x".repeat(64 * 1024));
    std::fs::write(&big, &source).expect("write the oversized skill");
    assert_eq!(code(&put(&big)), 2);

    let nameless = root.path().join("nameless.md");
    std::fs::write(&nameless, "# no frontmatter\n").expect("write");
    assert_eq!(code(&put(&nameless)), 2);

    assert_eq!(code(&put(&root.path().join("absent.md"))), 2);
    assert!(sent(&requests).is_empty(), "no daemon was contacted");
}
