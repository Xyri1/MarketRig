//! `research::allowlist_matches_capability_map` (feature SPEC `hithink-a-share` §7).

/// The allowlist is the vendored capability map's own endpoint tables. Parse them
/// here, from the vendor tree rather than from the generator's output, so a
/// re-vendoring that moves an endpoint fails until the script has been re-run.
#[test]
fn allowlist_matches_capability_map() {
    let map = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/hithink-finance/references/api/capability-map.md"
    ))
    .expect("the vendored capability map");

    let mut documented: Vec<&str> = map
        .split("`GET /api/")
        .skip(1)
        .map(|rest| rest.split('`').next().expect("a closing backtick"))
        .collect();
    documented.sort_unstable();
    documented.dedup();

    assert_eq!(
        &documented[..],
        super::RESEARCH_PATHS,
        "run `node scripts/hithink-skill.mjs` and commit research_paths.rs"
    );
    assert_eq!(
        super::RESEARCH_PATHS.len(),
        59,
        "the pinned capability map documents 59 endpoints (per HT-4)"
    );
}
