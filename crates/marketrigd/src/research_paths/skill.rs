//! `skill::rewritten_examples_route_through_marketrig`, `skill::seed_is_current`
//! and `skill::seed_is_whole` (feature SPEC `hithink-a-share` §7): the committed
//! seed says only what a desk can act on, it is still what the script produces
//! from the vendor tree, and it is what §5.3 uploads, whole.

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

/// The archive §5.3 uploads is the committed directory and nothing less: every
/// file on disk is in `HITHINK_SKILL_FILES` at its own relative path, with its
/// own bytes.
#[test]
fn seed_is_whole() {
    let root = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/seed/skills/hithink-finance"
    ));
    let mut on_disk: Vec<(String, String)> = seed_files()
        .iter()
        .map(|file| {
            (
                file.strip_prefix(root)
                    .expect("a seed file under the seed root")
                    .to_string_lossy()
                    .replace('\\', "/"),
                std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file:?}: {e}")),
            )
        })
        .collect();
    on_disk.sort();
    let mut uploaded: Vec<(String, String)> = crate::desk::HITHINK_SKILL_FILES
        .iter()
        .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
        .collect();
    uploaded.sort();
    assert_eq!(
        uploaded.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        on_disk.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        "the uploaded archive and the committed seed name different files"
    );
    assert_eq!(uploaded, on_disk, "a seeded file went up with other bytes");
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
