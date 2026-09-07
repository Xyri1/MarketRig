//! Embeds the vendored OpenViking plugin trees (feature SPEC
//! `openviking-continuity` §4.1) as one `include_bytes!` per file, so the seed
//! ships inside the daemon binary and a changed file rebuilds the crate.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let seed = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("seed")
        .join("openviking");
    // The directory itself, so a file added or removed is noticed too.
    println!("cargo:rerun-if-changed={}", seed.display());

    let mut files = Vec::new();
    walk(&seed, String::new(), &mut files);
    files.sort();

    let mut out = String::from("/// The vendored plugin trees: relative path, bytes.\n");
    out.push_str("pub static SEED: &[(&str, &[u8])] = &[\n");
    for relative in &files {
        let absolute = seed.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        let absolute = absolute.to_str().expect("the seed path is UTF-8");
        println!("cargo:rerun-if-changed={absolute}");
        writeln!(out, "    ({relative:?}, include_bytes!({absolute:?})),").expect("string");
    }
    out.push_str("];\n");

    let path = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("openviking_seed.rs");
    std::fs::write(&path, out).expect("writing the seed list");
}

/// Every file under `dir`, as `/`-separated paths relative to the seed root.
fn walk(dir: &Path, prefix: String, files: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("the seed directory")
        .map(|e| e.expect("a seed entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a UTF-8 seed name");
        let relative = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            walk(&path, relative, files);
        } else {
            files.push(relative);
        }
    }
}
