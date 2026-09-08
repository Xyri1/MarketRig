# Vendored: hithink-finance

`skills/hithink-finance/` from <https://github.com/HiThink-Tech/Financial-API>, plus that
repository's `LICENSE` (MIT). Everything beside this file is upstream's, byte for byte.

| | |
| --- | --- |
| Commit | `44b7aa34dd504675f3ddaa15b3d478ea16f97884` (`main`) |
| Vendored | 2026-09-08 |

MarketRig does not read this tree at runtime. `scripts/hithink-skill.mjs` rewrites it into
`crates/marketrigd/seed/skills/hithink-finance/`, the skill every new desk is seeded with, and
into `crates/marketrigd/src/research_paths.rs`, the research allowlist (feature SPEC
[`hithink-a-share`](../../sdd/features/hithink-a-share/SPEC.md) §5, per HT-5).

## Re-vendoring

```bash
git clone https://github.com/HiThink-Tech/Financial-API /tmp/hithink-src   # or fetch + checkout
git -C /tmp/hithink-src checkout <new commit>
rm -rf vendor/hithink-finance
mkdir -p vendor/hithink-finance
cp -R /tmp/hithink-src/skills/hithink-finance/. vendor/hithink-finance/
cp /tmp/hithink-src/LICENSE vendor/hithink-finance/LICENSE
# restore this file, update the commit and date above, then:
node scripts/hithink-skill.mjs
```

Commit the regenerated seed and `research_paths.rs` with the bump, and update the commit named
in HT-5 and feature SPEC §5.1. `cargo test -p marketrigd --lib skill::` reports how many curl
examples the rewrite covered; `research::allowlist_matches_capability_map` fails if the endpoint
count moved.
