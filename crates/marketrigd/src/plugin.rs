//! The vendored OpenViking plugins: the embedded seed trees, their workspace
//! copies, and the two runtimes' registration renderings.
//!
//! Contract: `sdd/features/openviking-continuity/SPEC.md` §4.1 (the vendored
//! material, per OV-4), §4.2 (the workspace copies), §4.3 (registration).
//!
//! Nothing here reads the setup row or a secret: the caller decides whether a
//! launch registers at all and hands in the validated Node path.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

include!(concat!(env!("OUT_DIR"), "/openviking_seed.rs"));

/// Where the copies live inside a desk workspace (§4.2).
const PLUGINS: &str = "plugins";

/// The plugin directory for one runtime, absolute (§4.2, §4.3).
pub fn dir(workspace: &Path, runtime: &str) -> PathBuf {
    workspace
        .join(".marketrig")
        .join(PLUGINS)
        .join(format!("openviking-{runtime}"))
}

/// The seed's own `claude/…` or `codex/…` path as a workspace-relative one:
/// the two trees become `openviking-claude/` and `openviking-codex/`, and the
/// upstream `LICENSE` beside them stays where it is.
fn workspace_relative(seed_path: &str) -> String {
    match seed_path.split_once('/') {
        Some((runtime @ ("claude" | "codex"), rest)) => format!("openviking-{runtime}/{rest}"),
        _ => seed_path.to_string(),
    }
}

/// One vendored file's bytes, by its seed-relative path.
fn seed(path: &str) -> &'static [u8] {
    SEED.iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
        .unwrap_or_else(|| panic!("the vendored seed carries {path}"))
}

// ---------------------------------------------------------------------------
// The workspace copies (§4.2)
// ---------------------------------------------------------------------------

/// Reconciles `<workspace>/.marketrig/plugins/` with the embedded seed byte for
/// byte (§4.2): a missing or differing file is rewritten and an extra one is
/// removed. Creation and every startup run the same function, so a desk created
/// on an older seed lands on the current one at the next start.
pub fn reconcile(workspace: &Path) -> io::Result<()> {
    let root = workspace.join(".marketrig").join(PLUGINS);
    let mut expected = BTreeSet::new();
    for (path, bytes) in SEED {
        let relative = workspace_relative(path);
        let target = root.join(&relative);
        if std::fs::read(&target).ok().as_deref() != Some(*bytes) {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, bytes)?;
        }
        expected.insert(relative);
    }
    remove_extras(&root, &root, &expected);
    Ok(())
}

/// Removes every file under `root` the seed does not name, then the directories
/// left empty. A directory that will not read is skipped, never fatal: the
/// launch that follows matters more than a perfect tree.
fn remove_extras(root: &Path, dir: &Path, expected: &BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_extras(root, &path, expected);
            // Empty only when everything inside it was an extra.
            let _ = std::fs::remove_dir(&path);
        } else {
            let relative = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !expected.contains(&relative) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Registration (§4.3)
// ---------------------------------------------------------------------------

/// The script one vendored hook command names: `node ${…}/scripts/<x>.mjs`
/// becomes the absolute `<plugin>/scripts/<x>.mjs`.
fn script(plugin: &Path, command: &str) -> Option<String> {
    let (_, tail) = command.split_once("/scripts/")?;
    Some(
        plugin
            .join("scripts")
            .join(tail.trim())
            .to_string_lossy()
            .to_string(),
    )
}

/// The vendored `hooks.json` as `{event: [group, …]}`, each group's `hooks`
/// rewritten by `render`. The event, the matcher, and the plugin's own timeout
/// are kept exactly as vendored.
fn rewrite(
    runtime: &str,
    plugin: &Path,
    render: impl Fn(&str, Option<&Value>) -> Value,
) -> Map<String, Value> {
    let vendored: Value = serde_json::from_slice(seed(&format!("{runtime}/hooks/hooks.json")))
        .expect("the vendored hooks.json is JSON");
    let mut out = Map::new();
    let Some(events) = vendored.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for (event, groups) in events {
        let groups: Vec<Value> = groups
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|group| {
                let hooks: Vec<Value> = group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|hook| {
                        let command = hook.get("command").and_then(Value::as_str)?;
                        let script = script(plugin, command)?;
                        Some(render(&script, hook.get("timeout")))
                    })
                    .collect();
                match group.get("matcher") {
                    Some(matcher) => json!({ "matcher": matcher, "hooks": hooks }),
                    None => json!({ "hooks": hooks }),
                }
            })
            .collect();
        out.insert(event.clone(), Value::Array(groups));
    }
    out
}

/// The plugin's Claude Code hooks for `runtime/launch/<desk-id>/settings.json`
/// (§4.3), in exec form: with `args` present Claude spawns `command` itself, so
/// no shell sees the path on either platform (the R3 lesson).
pub fn claude_hooks(node: &Path, plugin: &Path) -> Map<String, Value> {
    let node = node.to_string_lossy().to_string();
    rewrite("claude", plugin, |script, timeout| {
        let mut hook = json!({ "type": "command", "command": node, "args": [script] });
        if let Some(timeout) = timeout {
            hook["timeout"] = timeout.clone();
        }
        hook
    })
}

/// `<workspace>/.codex/hooks.json` (§4.3). Codex hooks have no exec form, so
/// the command is one quoted string — under `commandWindows` on Windows, the
/// documented per-platform field, so a backslashed path never meets a POSIX
/// shell.
///
/// ponytail: the per-platform field is chosen at compile time, which is what
/// the daemon writing its own launch files allows; a cross-rendered file would
/// need both keys and a Codex that accepts them.
pub fn codex_hooks(node: &Path, plugin: &Path) -> String {
    let node = node.to_string_lossy().to_string();
    let field = if cfg!(windows) {
        "commandWindows"
    } else {
        "command"
    };
    let hooks = rewrite("codex", plugin, |script, timeout| {
        let mut hook = json!({ "type": "command" });
        hook[field] = json!(format!("\"{node}\" \"{script}\""));
        if let Some(timeout) = timeout {
            hook["timeout"] = timeout.clone();
        }
        hook
    });
    json!({ "hooks": Value::Object(hooks) }).to_string()
}

/// The MCP proxy the runtime registers as the `openviking` server (§4.3).
pub fn proxy(plugin: &Path) -> String {
    plugin
        .join("servers")
        .join("mcp-proxy.mjs")
        .to_string_lossy()
        .to_string()
}

// ---------------------------------------------------------------------------
// plugin (feature SPEC §9, checks 6 and 8's plugin part)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// §4.1: both trees are vendored whole, and §4.2's reconcile is the only
    /// writer of the copies — a changed file is rewritten and an extra goes.
    #[test]
    fn reconcile_is_byte_for_byte() {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("alpha");
        reconcile(&workspace).unwrap();

        let claude = dir(&workspace, "claude");
        let codex = dir(&workspace, "codex");
        assert_eq!(
            std::fs::read(claude.join("hooks/hooks.json")).unwrap(),
            seed("claude/hooks/hooks.json")
        );
        assert!(claude.join("servers/mcp-proxy.mjs").is_file());
        assert!(codex.join("scripts/shared/credentials.mjs").is_file());
        assert!(
            workspace
                .join(".marketrig/plugins/LICENSE")
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        // §4.2: `.openviking/` in the workspace is never written by MarketRig.
        assert!(!workspace.join(".openviking").exists());

        let changed = claude.join("scripts/auto-capture.mjs");
        let vendored = std::fs::read(&changed).unwrap();
        std::fs::write(&changed, "tampered\n").unwrap();
        std::fs::write(claude.join("scripts/extra.mjs"), "x").unwrap();
        std::fs::create_dir_all(codex.join("stray/deep")).unwrap();
        std::fs::write(codex.join("stray/deep/x.mjs"), "x").unwrap();

        reconcile(&workspace).unwrap();
        assert_eq!(std::fs::read(&changed).unwrap(), vendored);
        assert!(!claude.join("scripts/extra.mjs").exists());
        assert!(!codex.join("stray").exists());
    }

    /// §4.3: both runtimes' hooks come from the vendored file with absolute
    /// paths, the exec form for Claude, and the plugin's own timeouts.
    #[test]
    fn hooks_render_from_the_vendored_file() {
        let node = Path::new("/opt/node/bin/node");
        let plugin = Path::new("/desks/alpha/.marketrig/plugins/openviking-claude");

        let claude = claude_hooks(node, plugin);
        // Every event the vendored file registers, and nothing invented.
        assert_eq!(claude.len(), 9);
        let start = &claude["SessionStart"][0]["hooks"][0];
        assert_eq!(start["command"], json!("/opt/node/bin/node"));
        assert_eq!(
            start["args"],
            json!([plugin
                .join("scripts")
                .join("session-start.mjs")
                .to_string_lossy()])
        );
        assert_eq!(start["timeout"], json!(120));
        assert_eq!(claude["PreToolUse"][0]["matcher"], json!("Read|Glob|Grep"));
        assert_eq!(claude["Stop"][0]["hooks"][0]["timeout"], json!(45));

        let codex_plugin = Path::new("/desks/alpha/.marketrig/plugins/openviking-codex");
        let codex: Value = serde_json::from_str(&codex_hooks(node, codex_plugin)).unwrap();
        let field = if cfg!(windows) {
            "commandWindows"
        } else {
            "command"
        };
        assert_eq!(codex["hooks"].as_object().unwrap().len(), 5);
        let hook = &codex["hooks"]["SessionStart"][0];
        assert_eq!(hook["matcher"], json!("clear|startup|resume"));
        assert_eq!(
            hook["hooks"][0][field],
            json!(format!(
                "\"/opt/node/bin/node\" \"{}\"",
                codex_plugin
                    .join("scripts")
                    .join("session-start-commit.mjs")
                    .to_string_lossy()
            ))
        );
        assert_eq!(hook["hooks"][0]["timeout"], json!(70));
        assert_eq!(codex["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
    }
}
