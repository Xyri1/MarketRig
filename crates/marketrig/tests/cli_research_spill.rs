//! `cli::research_spill` — the 256 KiB boundary, `--out`, and the `--param`
//! usage error of `marketrig research hithink` (feature SPEC `hithink-a-share`
//! §4.2) against the shared fake endpoint of `tests/common/mod.rs`.

use std::sync::LazyLock;

mod common;

use common::{code, fake_daemon, health_ok, marketrig_in, write_endpoint};

/// The CLI's own ceiling: at or under it the body prints, above it it spills.
const INLINE: usize = 256 * 1024;

/// An envelope of exactly `bytes` bytes, leaked because the fake endpoint
/// answers `&'static str`.
fn envelope(bytes: usize) -> &'static str {
    let (head, tail) = (r#"{"code":0,"message":"success","data":""#, r#""}"#);
    let mut body = String::with_capacity(bytes);
    body.push_str(head);
    body.push_str(&"x".repeat(bytes - head.len() - tail.len()));
    body.push_str(tail);
    assert_eq!(body.len(), bytes);
    Box::leak(body.into_boxed_str())
}

static AT_CEILING: LazyLock<&'static str> = LazyLock::new(|| envelope(INLINE));
static PAST_CEILING: LazyLock<&'static str> = LazyLock::new(|| envelope(INLINE + 1));

/// The two routes carry the encoded query the CLI is expected to have built:
/// `.` rides unreserved, a space becomes `%20`.
fn respond(route: &str, _: &str) -> (u16, &'static str) {
    match route {
        "GET /health" => (200, health_ok()),
        "GET /research/hithink/meta/tickers/search?q=600519" => (200, *AT_CEILING),
        "GET /research/hithink/a-share/financials/income-statements?thscode=600519.SH&name=Kweichow%20Moutai" => {
            (200, *PAST_CEILING)
        }
        _ => (500, r#"{"code":"INTERNAL","message":"Unexpected route."}"#),
    }
}

/// A scratch endpoint plus the working directory the spill lands in.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    let (port, _requests) = fake_daemon(respond);
    write_endpoint(root.path(), port);
    (root, cwd)
}

fn files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read the working directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// Exactly at the ceiling the body goes to standard output verbatim, the same
/// bytes under `--json`, and nothing is written to the working directory.
#[test]
fn at_the_boundary_the_body_prints() {
    let (root, cwd) = fixture();
    let args = [
        "research",
        "hithink",
        "meta/tickers/search",
        "--param",
        "q=600519",
    ];

    let plain = marketrig_in(cwd.path(), root.path(), &args);
    assert_eq!(
        code(&plain),
        0,
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert_eq!(plain.stdout, format!("{}\n", *AT_CEILING).into_bytes());
    assert!(
        files(cwd.path()).is_empty(),
        "an inline body writes no file"
    );

    let json = marketrig_in(cwd.path(), root.path(), &[&["--json"], &args[..]].concat());
    assert_eq!(
        json.stdout, plain.stdout,
        "the body is the machine form already"
    );
}

/// One byte past it the body becomes a file named for the path and the second,
/// and the command prints where it went.
#[test]
fn past_the_boundary_it_spills() {
    let (root, cwd) = fixture();
    let args = [
        "research",
        "hithink",
        "a-share/financials/income-statements",
        "--param",
        "thscode=600519.SH",
        "--param",
        "name=Kweichow Moutai",
    ];

    let spilled = marketrig_in(cwd.path(), root.path(), &args);
    assert_eq!(
        code(&spilled),
        0,
        "{}",
        String::from_utf8_lossy(&spilled.stderr)
    );
    let printed = String::from_utf8(spilled.stdout).expect("utf-8 stdout");
    let written = files(cwd.path());
    assert_eq!(written.len(), 1, "{written:?}");
    let name = &written[0];
    assert!(
        name.starts_with("hithink-a-share-financials-income-statements-")
            && name.ends_with(".json")
            && name["hithink-a-share-financials-income-statements-".len()..name.len() - 5]
                .chars()
                .all(|c| c.is_ascii_digit()),
        "{name}"
    );
    assert_eq!(printed, format!("wrote {name} ({} bytes)\n", INLINE + 1));
    assert_eq!(
        std::fs::read_to_string(cwd.path().join(name)).expect("the spilled body"),
        *PAST_CEILING
    );

    // `--json` names the same two facts as one object.
    let json = marketrig_in(cwd.path(), root.path(), &[&["--json"], &args[..]].concat());
    let printed: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("a JSON spill note");
    assert_eq!(printed["bytes"], INLINE + 1);
    let named = printed["path"].as_str().expect("a path");
    assert!(cwd.path().join(named).is_file(), "{named}");
}

/// `--out` spills whatever the size, to the named file.
#[test]
fn out_writes_the_named_file() {
    let (root, cwd) = fixture();
    let target = cwd.path().join("search.json");
    let out = marketrig_in(
        cwd.path(),
        root.path(),
        &[
            "research",
            "hithink",
            "meta/tickers/search",
            "--param",
            "q=600519",
            "--out",
            target.to_str().expect("utf-8 path"),
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        format!("wrote {} ({INLINE} bytes)\n", target.display())
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("the named file"),
        *AT_CEILING
    );
}

/// A `--param` without `=` is a usage error, exit 2, before any request.
#[test]
fn a_param_without_an_equals_is_usage() {
    let (root, cwd) = fixture();
    let bad = marketrig_in(
        cwd.path(),
        root.path(),
        &["research", "hithink", "meta/tickers/search", "--param", "q"],
    );
    assert_eq!(code(&bad), 2);
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("--param q is not key=value"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    assert!(files(cwd.path()).is_empty());
}
