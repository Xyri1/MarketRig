//! `marketrigd --openapi` as the frontend's generator runs it: the document on
//! standard output, exit `0`, and nothing touched on the way.
//!
//! Contract: `sdd/features/r5-desktop-approval-controls/SPEC.md` §6.1, §8
//! check 6. In-process coverage of the document itself is `api`'s own check;
//! this one exists for the two facts only a process can show — the exit code
//! and the untouched data root.

use std::process::Command;

/// The flag returns before `Roots::from_env()`, so a run with no test data root
/// must still create nothing. The check proves it by pointing the platform's
/// own root variables at an empty directory and asserting it stays empty.
#[test]
fn openapi_prints_a_document_and_touches_no_data_root() {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_marketrigd"));
    command
        .arg("--openapi")
        .env_remove("MARKETRIG_TEST_DATA_ROOT")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LOCALAPPDATA", home.path());
    let output = command.output().expect("marketrigd runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "exit {:?}: {stderr}",
        output.status
    );

    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the document is JSON on standard output");
    assert_eq!(document["info"]["title"], "MarketRig");
    let paths = document["paths"].as_object().expect("paths");
    assert_eq!(
        paths.len(),
        marketrigd::api::HTTP_PATHS.len(),
        "every HTTP route is described and nothing else"
    );
    for path in marketrigd::api::HTTP_PATHS {
        assert!(paths.contains_key(*path), "{path} is not described");
    }

    assert_eq!(
        std::fs::read_dir(home.path()).unwrap().count(),
        0,
        "the flag answers before startup resolves a data root"
    );
}
