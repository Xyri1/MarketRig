//! `skill::rewritten_examples_route_through_marketrig` and `skill::seed_is_current`
//! (feature SPEC `hithink-a-share` §7): the committed seed says only what a desk
//! can act on, and it is still what the script produces from the vendor tree.

use std::path::{Path, PathBuf};

/// Every line of the seeded skill, file by file, in walk order.
fn seed_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|e| e.expect("a directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push(path);
            }
        }
    }

    let mut found = Vec::new();
    walk(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/seed/skills/hithink-finance"
        )),
        &mut found,
    );
    found
}

#[test]
fn rewritten_examples_route_through_marketrig() {
    let files = seed_files();
    assert!(files.len() > 10, "the seed lost its reference pages");

    let (mut rewritten, mut left) = (0usize, 0usize);
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
        for line in text.lines() {
            for forbidden in [
                "X-api-key",
                "fuyao.aicubes.cn/mcp",
                "hithink-finance auth",
                "pip install",
                "npx",
            ] {
                assert!(
                    !line.contains(forbidden),
                    "{}: {forbidden} survives in {line:?}",
                    file.display()
                );
            }
            if line.starts_with("marketrig research hithink") {
                rewritten += 1;
            }
            if line.trim_start().starts_with("curl") {
                left += 1;
            }
        }
    }

    println!("{rewritten} examples rewritten, {left} curl lines left");
    assert!(rewritten > 0, "no request example routes through marketrig");
    // The one line left is not a request to the service: it downloads the
    // presigned S3 URL a rewritten call returned, and carries no key
    // (endpoints-market-dumps.md, 完整下载流程).
    assert_eq!(left, 1, "an unrewritten curl example reached the seed");
}

#[test]
fn seed_is_current() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let checked = std::process::Command::new("node")
        .args(["scripts/hithink-skill.mjs", "--check"])
        .current_dir(root)
        .output();
    let Ok(output) = checked else {
        println!("skipped: node is not on PATH");
        return;
    };
    assert!(
        output.status.success(),
        "node scripts/hithink-skill.mjs --check failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
