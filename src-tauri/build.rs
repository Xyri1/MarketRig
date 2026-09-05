//! `tauri_build::build()` resolves `bundle.externalBin` on every build, not
//! only on `tauri build`: without the three sidecars present it fails with
//! "resource path … doesn't exist", which would break `cargo build`, Clippy,
//! and `cargo test` for everyone who has not run a release build first. So the
//! placeholders are created here when they are missing. `pnpm build` copies the
//! real `cargo build --release` binaries over them before `tauri build` runs,
//! and the placeholders are never bundled.
fn main() {
    let triple = std::env::var("TARGET").expect("TARGET");
    let suffix = if triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let dir = std::path::Path::new("binaries");
    std::fs::create_dir_all(dir).expect("binaries/");
    for name in ["marketrigd", "marketrig", "marketrig-mcp"] {
        let path = dir.join(format!("{name}-{triple}{suffix}"));
        if !path.exists() {
            std::fs::write(&path, b"").expect("sidecar placeholder");
        }
    }
    tauri_build::build();
}
