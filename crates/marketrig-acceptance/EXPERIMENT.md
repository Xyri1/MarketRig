# Running the attended experiment — Windows cells

The experiment is the operator-attended half of the acceptance chain (root `sdd/SPEC.md` §17; R1 feature SPEC §10.3). Slice 002's exit needs E1 (Codex CLI) and E2 (Claude Code) once per platform-and-runtime cell. The macOS cells ran on 2026-09-02 (`target/acceptance/experiment-codex-1788316954/` and `experiment-claude-1788317581/`); this guide is for the two Windows cells and doubles as the procedure anywhere. Commands are PowerShell.

## 1. Before you start

- A checkout of the commit under test. `rust-toolchain.toml` pins the toolchain, so the first `cargo` command installs it; the MSVC linker (Visual Studio Build Tools, "Desktop development with C++") must already be present, as on CI's `windows-latest`.
- Codex CLI and Claude Code installed and signed in: `codex --version`, `claude --version`. The version each cell ran on belongs in the evidence line.
- Nothing else. Do **not** set `MARKETRIG_TEST_DATA_ROOT`, `MARKETRIG_TEST_NO_TRADING`, or `MARKETRIG_TEST_QUOTE_URL` yourself: the harness relocates the root into the bundle and clears both feed seams so the daemon polls real Yahoo.
- Pick the time. Before asking you to do anything, the harness waits up to 60 seconds for one `LIVE` observation from Yahoo, so at least one catalog market must be in session (feature SPEC §2.2, Monday–Friday, no holiday calendar):

  | Market | Zone | Sessions |
  | --- | --- | --- |
  | US | America/New_York | 09:30–16:00 |
  | HK | Asia/Hong_Kong | 09:30–12:00, 13:00–16:00 |
  | CN | Asia/Shanghai | 09:30–11:30, 13:00–15:00 |

  On an exchange holiday the phase still reads `OPEN` but the feed never goes `LIVE`; the cell then fails mechanically at the precheck. Choose another window and rerun.

## 2. Start the cell

One cell per invocation; the other test skips. The harness builds `marketrigd`, `marketrig`, and `marketrig-mcp` itself, spawns the daemon, creates a run-stamped desk, and prints its instructions. It then waits up to 15 minutes per step on the daemon's durable rows.

```powershell
$env:MARKETRIG_EXPERIMENT = "codex"     # or "claude"
cargo test -p marketrig-acceptance --test experiment -- --nocapture
```

The bundle is `target\acceptance\experiment-<cell>-<stamp>\` (`MARKETRIG_ACCEPTANCE_OUT` overrides). Copy the `Desk:`, `Data root:`, and `Adapter:` lines from the printout; the same text is in `<bundle>\instructions.txt`. Leave this window running and open a second one for the session.

## 3. Register the adapter, project-scoped

R1 keeps registration operator-performed. The printout offers the global `codex mcp add` / `claude mcp add-json` form; the project-scoped form below keeps your user config untouched and lands the registration in the bundle as evidence. Substitute `<bundle>` and `<desk>` from the printout; the adapter path already ends in `.exe`.

**Codex CLI** — `<bundle>\.codex\config.toml`. Codex layers `.codex\config.toml` from the session's directory up through its parents, so this applies to every session started under the bundle. TOML literal strings (single quotes) need no backslash escaping:

```toml
[mcp_servers.marketrig]
command = 'C:\path\to\MarketRig\target\debug\marketrig-mcp.exe'
args = ["--desk", "<desk>"]

[mcp_servers.marketrig.env]
MARKETRIG_TEST_DATA_ROOT = 'C:\path\to\MarketRig\target\acceptance\experiment-<cell>-<stamp>'
```

The project layer is enabled only for a trusted project: if the checkout is not yet trusted in Codex, accept the trust prompt on the session's first launch. Verify from the desk workspace before starting the session:

```powershell
cd <bundle>\desks\<desk>
codex mcp list        # marketrig … enabled
```

**Claude Code** — `<bundle>\desks\<desk>\.mcp.json`, in the workspace itself, which is the session's cwd. JSON needs doubled backslashes:

```json
{
  "mcpServers": {
    "marketrig": {
      "command": "C:\\path\\to\\MarketRig\\target\\debug\\marketrig-mcp.exe",
      "args": ["--desk", "<desk>"],
      "env": { "MARKETRIG_TEST_DATA_ROOT": "C:\\path\\to\\MarketRig\\target\\acceptance\\experiment-<cell>-<stamp>" }
    }
  }
}
```

`claude mcp list` from the workspace shows `marketrig … ⏸ Pending approval` until the session approves it; that is the expected state.

Adding either file to the desk workspace is fine: after `READY` MarketRig never rewrites agent-owned files and reconciles only `CLAUDE.md` and the `.claude\skills` link.

## 4. Drive the session

Start the runtime in the desk workspace and answer its own dialogs by hand (folder trust; Claude Code additionally asks to approve the `marketrig` server):

```powershell
cd <bundle>\desks\<desk>
codex          # or: claude
```

Then, with 15 minutes per step from the moment the instructions printed:

1. **Two quote reads.** Ask the session to read `marketrig://desk/<desk>/quotes`, wait a little, and read it again. Only you can judge the second read is fresher; observations are never persisted, so the harness records this aspect `INCONCLUSIVE` by construction. Read an instrument whose market is open, otherwise the observation cannot advance between reads.
2. **Submit.** Ask for one paper order through `submit_order`: a `LIMIT` `BUY` well below the last price so it rests. The market phase never gates an order, so a closed market still rests it (E2 on macOS rested AAPL after the US close). The harness records the durable action and the sandbox's lifecycle: `OrderInitialized`, `OrderSubmitted`, `OrderAccepted`.
3. **Cancel.** Ask the session to cancel it through `cancel_order`, naming the client order id the submit answered with. The harness records `OrderCanceled`, stops the daemon cleanly, and exits.

Prompts that worked on macOS: "Read the marketrig quotes resource for this desk, wait 30 seconds, read it again and tell me whether the observation advanced." / "Submit a resting paper order: LIMIT BUY 100 × 0700.XHKG at 400.00 through submit_order." / "Cancel that order through cancel_order using the client order id you were given."

## 5. Read the result

`test result: ok` says only that the harness ran; the verdict is in `<bundle>\observations.jsonl`, one JSON line per step:

- a complete cell ends with `"note": "attended cell complete"` and lists `SUBMIT <id>` and `CANCEL <id>` under `actions`;
- step 5, the quote reads, is `INCONCLUSIVE` in every cell;
- a step that timed out ends the cell with `INCONCLUSIVE` and `waited_secs: 900` instead. That is not a defect. Rerun the cell; every run creates a new desk and bundle, so rewrite the project-scoped file for the new `<desk>` and `<bundle>`, and delete the timed-out bundle;
- a mechanical failure (no daemon, no `LIVE` quote, a wrong row) panics the test with the reason; `<bundle>\marketrigd-1.stderr` and `<bundle>\logs\` hold the daemon's side.

The macOS bundles named at the top are the reference shape for a complete cell.

## 6. Evidence and cleanup

- Keep one complete bundle per cell: `experiment-codex-<stamp>` and `experiment-claude-<stamp>`. The bundle is the evidence; the `.codex\config.toml` and `.mcp.json` inside it carry no secret, only the data-root path.
- Record each cell's stamp, runtime version, and the commit in the slice's freeze note and the roadmap's evidence line.
- Nothing was written to `~\.codex\config.toml` or Claude Code's user config. If you used the global form from the printout instead, remove it: `codex mcp remove marketrig` / `claude mcp remove marketrig`.
- Aborting the harness with Ctrl-C skips its teardown; check for a leftover daemon with `Get-Process marketrigd` and stop it before the next cell.
