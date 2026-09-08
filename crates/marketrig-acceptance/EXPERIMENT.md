# Running the attended experiment — Windows cells

The experiment is the operator-attended half of the acceptance chain (root `sdd/SPEC.md` §17; R1 feature SPEC §10.3, R2 feature SPEC §10.3). Slice 002's exit needs E1 (Codex CLI) and E2 (Claude Code) once per platform-and-runtime cell; slice 003 adds E3, the scheduled-trigger scenario, and slice 004 adds E4, the runtime-delivery scenario, to the same cells. E5 was Hindsight's and went with it; slice 011 adds E6, the OpenViking memory-and-skills scenario (`sdd/features/openviking-continuity/SPEC.md` §7.3), which runs last in the cell and skips with evidence unless its own prerequisites are named (§1, §7). The R1 macOS cells ran on 2026-09-02 (`target/acceptance/experiment-codex-1788316954/` and `experiment-claude-1788317581/`); this guide is for the two Windows cells and doubles as the procedure anywhere. Commands are PowerShell.

One invocation per cell runs **four** scenarios back to back — E1 or E2, E3, E4, and E6 — each on its own daemon, desk, and bundle. They share your terminal and your hands, so the harness serializes them: each scenario's instructions print only once the previous one has finished. E4 and E6 are unlike the first two: MarketRig launches the runtime itself, with the adapter already registered, and **your console becomes the desk's terminal** for the whole scenario.

## 1. Before you start

- A checkout of the commit under test. `rust-toolchain.toml` pins the toolchain, so the first `cargo` command installs it; the MSVC linker (Visual Studio Build Tools, "Desktop development with C++") must already be present, as on CI's `windows-latest`.
- Codex CLI and Claude Code installed and signed in: `codex --version`, `claude --version`. The version each cell ran on belongs in the evidence line.
- E6's own prerequisites (`sdd/features/openviking-continuity/SPEC.md` §1.2, §1.3). Without them E6 skips with evidence and the rest of the cell still runs:
  - `MARKETRIG_EXPERIMENT_PYTHON` — an **absolute** path to a Python that reports exactly `3.12`. The minor is exact by decision (OV-1); anything else is `PYTHON_UNSUPPORTED` and the cell fails at the setup call.
  - `MARKETRIG_EXPERIMENT_NODE` — an **absolute** path to Node **22 or newer**, the runtime the memory plugin's hooks and its MCP proxy run on.
  - `MARKETRIG_EXPERIMENT_WHEELS` — the locked wheel directory. It is not committed; produce it once per platform, on that same interpreter:

    ```powershell
    node scripts\openviking-wheels.mjs --python <python3.12> --platform windows-x64   # macOS: --platform macos-arm64
    node scripts\openviking-wheels.mjs --platform windows-x64 --check
    ```

    It writes `openviking-wheels\<platform>\` at the checkout root and a `<platform>.lock` beside it; `--check` verifies an existing directory against that lockfile. Generate a set on the platform it is for — the script refuses any other, because pip evaluates the dependencies' environment markers against the host — and a download that drifts from the committed lockfile prints the drift and fails until `--write-lock` accepts it. MarketRig installs from it with `pip --no-index`, so a wheel the set is missing is a `PROVISION_FAILED`, never a download.
  - `MARKETRIG_EXPERIMENT_MEMORY_BASE_URL`, `_API_KEY`, `_LLM_MODEL`, `_EMBEDDING_MODEL` — one real OpenAI-compatible endpoint, which both the child's VLM and its embeddings use. Saving the provider makes one real embeddings request to measure the model's dimension, so a wrong base URL, key, or model name is a `PROVIDER_REJECTED` at that call. The key goes straight into the OS credential store; the harness never prints it and never writes it to the bundle.
- One dot-sourced script selects the cell and exports all seven of those — `. crates\marketrig-acceptance\experiment-env.ps1 codex|claude` in PowerShell, `source crates/marketrig-acceptance/experiment-env.sh codex|claude` on macOS. It prompts for the provider key without echoing it, and leaves E6's variables unset (so E6 skips) when the interpreters or the wheel directory are not there.
- Nothing else of MarketRig's running on the box. The packaged desktop smoke (`pnpm smoke`, R5 feature SPEC §7.3) opens by killing every `marketrigd`, `marketrig-desktop`, and `runtime-standin` process and wiping the real per-user root, so it and a cell must never overlap; the same holds for the gate.
- Nothing else. Do **not** set `MARKETRIG_TEST_DATA_ROOT`, `MARKETRIG_TEST_NO_TRADING`, or `MARKETRIG_TEST_QUOTE_URL` yourself: the harness relocates the root into the bundle and clears both feed seams so the daemon polls real Yahoo.
- Pick the time. Before asking you to do anything, the harness waits up to 60 seconds for one `LIVE` observation from Yahoo, so at least one catalog market must be in session (feature SPEC §2.2, Monday–Friday, no holiday calendar):

  | Market | Zone | Sessions |
  | --- | --- | --- |
  | US | America/New_York | 09:30–16:00 |
  | HK | Asia/Hong_Kong | 09:30–12:00, 13:00–16:00 |
  | CN | Asia/Shanghai | 09:30–11:30, 13:00–15:00 |

  On an exchange holiday the phase still reads `OPEN` but the feed never goes `LIVE`; the cell then fails mechanically at the precheck. Choose another window and rerun. The converse is not checked: `LIVE` means the last poll succeeded (R1 feature SPEC §2), so outside a session the precheck still passes on a `CLOSED` synthesized quote and the agent rightly refuses to trade — the Windows Codex E6 of 2026-09-08 timed out at the cycle step that way. Start inside a session.

## 2. Start the cell

One cell per invocation; the other cell's tests skip. The harness builds `marketrigd`, `marketrig`, `marketrig-mcp`, and the `trigger-code` helper itself, spawns a daemon, creates a run-stamped desk, and prints its instructions. It then waits up to 15 minutes per step on the daemon's durable rows. Budget 90–120 minutes for a cell: E1/E2, then E3, E4, and E6 — E6 alone spends several minutes provisioning its own Python environment from the wheels and starting the real OpenViking server before it asks you for anything.

```powershell
$env:MARKETRIG_EXPERIMENT = "codex"     # or "claude"
cargo test -p marketrig-acceptance --test experiment -- --nocapture
```

The bundles are `target\acceptance\experiment-<cell>-<stamp>\` for E1/E2, `target\acceptance\experiment-e3-<cell>-<stamp>\` for E3, `target\acceptance\experiment-e4-<cell>-<stamp>\` for E4, and `target\acceptance\experiment-e6-<cell>-<stamp>\` for E6. Do **not** set `MARKETRIG_ACCEPTANCE_OUT` for a cell: it would point every scenario at one directory and each would overwrite the last one's `observations.jsonl`. Copy the `Desk:`, `Data root:`, and `Adapter:` lines from each printout; the same text is in `<bundle>\instructions.txt` (E1/E2), `<bundle>\instructions-e3.txt` (E3), `<bundle>\instructions-e4.txt` (E4), and `<bundle>\instructions-e6.txt` (E6). Leave this window running and open a second one for the session. E6's bundle is the largest by far: the data root *is* the bundle, so it carries the whole provisioned virtual environment as well as the plugin's own state and logs.

## 3. Register the adapter, project-scoped

Each scenario has its own desk and bundle, so this is done twice per cell — once for E1/E2 and again with E3's `<desk>` and `<bundle>` when its instructions print. R1 keeps registration operator-performed. The printout offers the global `codex mcp add` / `claude mcp add-json` form; the project-scoped form below keeps your user config untouched and lands the registration in the bundle as evidence. Substitute `<bundle>` and `<desk>` from the printout; the adapter path already ends in `.exe`.

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

## 4. Drive the session — E1 / E2

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

## 5. Drive the session — E3

E3 is the R2 scenario: a real session **defines a trigger whose code trades**, and the daemon runs that code with no session alive. Its printout adds three lines E1/E2 do not have — `CLI:` (the `marketrig` binary this run built), `trigger-code:` (the acceptance helper every code-bearing trigger names as `argv[0]`), and `Instrument:` (one the real feed is observing right now) — plus the whole `trigger create` command line, already filled in with an instant about two minutes out.

E3's harness sets trigger code to *Always allow* on its own data root before the session starts, because R5's installed default is *Require approval* and E3 is about the session defining a trigger that runs, not about you approving one. Nothing else in the cell changes.

E3 runs after E1/E2 in the same invocation and gets its **own desk and data root**, so the registration from E1/E2 does not carry over: remove it (`codex mcp remove marketrig` / `claude mcp remove marketrig`, or rewrite the project-scoped file for the new `<desk>` and `<bundle>` as in §3), register the adapter for E3's desk, and start the runtime in E3's workspace. Time the cell so a catalog market is **open when the trigger fires** (§1's table), not only when the harness prechecks: the trigger's code places a market order, which the paper book refuses without a live price, and that refusal ends the cell `INCONCLUSIVE` rather than as evidence. Then:

1. **The script.** Ask the session to write a one-line file in the workspace, `job.txt`, containing exactly `order <Instrument> BUY 1`. That line is not a shell script: the `trigger-code` helper reads it and places the order through `marketrig-mcp` with the firing id as `action_id`.
2. **The trigger.** Give the session the whole command line from the printout. It carries `MARKETRIG_TEST_DATA_ROOT` because the CLI needs the same data root the daemon is using — the adapter's registration does not cover a command the session runs itself. If the session takes more than two minutes to get there, have it pick a fresh `--at` a couple of minutes ahead, in the same RFC 3339 UTC form.
3. **Nothing else.** You may close the session. The harness watches for the `triggers` row, the `firings` row, the completed `executions` row, the `trading_actions` row attributed to the firing, and the queued `TRIGGER_RESULT` prompt. Reading that prompt back (`marketrig prompt list <desk>`) is worth doing with the session if it is still open, but the harness does not wait on it.

## 6. Drive the session — E4

E4 is the R3 scenario, and it inverts the other two: **you register nothing and start nothing.** MarketRig discovers the runtime on your `PATH`, creates a desk on it, launches it in that desk's workspace with the MarketRig adapter already registered, and delivers a scheduled trigger's result to it as the session's own input. Your console becomes that session's terminal — raw mode, window size relayed — from the moment the instructions print until the scenario ends, so everything you type goes to the session and everything it prints appears in this window. The harness's own progress lines are interleaved in the same window; that is expected.

1. **Answer the first launch.** A real CLI asks its own questions, and only you can answer them; readiness — and therefore every delivery — waits on your answer, inside the adapter's 120-second deadline. Observed on macOS:
   - **Codex CLI**, first launch per workspace only (remembered in `~/.codex`): *"Do you trust the contents of this directory? …"* › `1. Yes, continue` / `2. No, quit`. `1` is preselected, so Enter accepts. `SESSION_READY` arrives only after this.
   - **Claude Code**, first launch per workspace: *"Quick safety check: Is this a project you created or one you trust?"* — **"No, exit" is preselected**, so press Down, then Enter.
   - **Claude Code**, *every* launch including resumes: *"WARNING: Loading development channels … Channels: server:marketrig-channel"* › `1. I am using this for local development` / `2. Exit`; Enter accepts. Both Claude prompts block the bridge's connection, which is what readiness is (§5.3).

   Answer them and nothing more.
2. **Watch the first delivery.** A code-less one-off is due about two minutes after the instructions print. A new session is oriented first, so you should see MarketRig's orientation paragraph arrive as the session's first input, then `MarketRig TRIGGER_RESULT <id>:` with the firing's JSON. Read it; whether the session acts on it is not the point and is recorded `INCONCLUSIVE`.
3. **Keep it busy.** Once the first result has arrived, ask the session something that takes a while. The harness prints `give the session something to chew on now` at that point, but the runtime's full-screen redraw usually paints over it, so do not wait to see it. A second one-off is due two minutes later. Codex holds it until the turn ends. Claude Code queues it itself and has been observed attaching it to the running turn as a queued command; either way it is never typed into the terminal, and `DELIVERED` is the write.
4. **Two things that look like faults and are not.** A Claude `--resume` only succeeds once the earlier session completed a turn, so let a `SESSION_TURN_ENDED` land before ending one. And a Codex thread lives inside one app-server lifetime: restart the daemon and the resume fails, the dispatcher repoints the desk `unresumable` and starts a new session — correct behaviour, recorded as evidence.
5. **The switch.** The harness then discovers the other runtime, switches the desk to it, and asserts the desk's pointers and history did not move. If the other runtime is not installed, that leg is recorded `INCONCLUSIVE` and the cell is still complete.

If a Windows console is left echoing every keypress as `;1;0;1_[`-style key reports after a cell (win32-input-mode left on by a cell that ended abnormally), run `[Console]::Write("`e[?9001l")` in it or open a new tab. Nothing else to clean up afterwards: the launch files live under the bundle's `data\runtime\launch\` and the daemon deletes them when the process row closes. Your own Codex or Claude configuration was never touched.

## 7. Drive the session — E6

E6 is slice 011's scenario (`sdd/features/openviking-continuity/SPEC.md` §7.3): **real OpenViking**, provisioned by MarketRig itself, capturing a real session and owning its skills. It runs last in the cell, on its own daemon and desk, and skips with evidence when §1's seven variables are not all set. Before it asks you for anything, the harness has already:

1. discovered the cell's runtime and saved the provider — that save makes one real embeddings request to measure the model's dimension, which the child's configuration needs;
2. sent `PUT /openviking/setup` with your Python, Node, and wheel directory and waited for the row to leave `PROVISIONING` — `python -m venv`, then `pip install --no-index openviking==0.4.17.1`, several minutes on a cold directory on macOS and 13–17 minutes on Windows, where every one of the ~100 k unpacked files meets Defender unless `targetcceptance` is excluded; the harness allows 30 — and then for the child to answer `/ready`;
3. created the desk on the cell's runtime, which gives it its own OpenViking user, a key derived from the installation seed, and the seeded `desk-improvement` skill.

Each of those is mechanical and fails the cell with the reason in `<bundle>\observations.jsonl`: `PYTHON_UNSUPPORTED` or `NODE_UNSUPPORTED` for a prerequisite the daemon ran and rejected, `PROVISION_FAILED` naming the package pip could not find for an incomplete wheel set, `UNAVAILABLE` with the child's last output line when the server itself will not start.

Then, as in E4, MarketRig launches the runtime and **your console becomes the desk's terminal** — this time with the memory plugin's hooks and the `openviking` MCP server in the launch files and the desk's OpenViking identity on the runtime's process environment. Answer the first-launch questions of §6.1 and nothing more. Then:

1. **Close one cycle.** Ask the session to buy one unit of the instrument the printout names as `LIVE` through `submit_order`, and then to sell that same unit. That closes a position cycle, and MarketRig queues its `EVALUATION` prompt and delivers it as the session's own input.
2. **Let it learn.** Let the session do what its constitution says: state the lesson plainly in the conversation — there is no retain command any more, the plugin captures the turn itself — and write it into a skill with the `openviking` tools (`write` on `viking://~/skills/<name>/SKILL.md`). Do not write either yourself. `.agents/skills/` is MarketRig's read-only projection; an attempt to edit a file there must be refused, and it is worth provoking that refusal once.
3. **Confirm the capture yourself.** Ask the session to run

   ```text
   node .marketrig\plugins\openviking-<cell>\scripts\ov-memory-doctor.mjs
   ```

   from the desk workspace. It runs inside the session, which is the only place this desk's OpenViking key exists — the harness holds none and records capture `INCONCLUSIVE` either way. That report, and the plugin's own state and logs under `<bundle>\data\openviking\plugin\<desk-id>\`, are the evidence. (Claude Code's plugin also carries `scripts\ov-status.mjs`, the script behind its `/ov` command.)
4. **The resumed session.** The harness then ends the session and resumes the same thread. Let the first session finish a turn before that: a Claude `--resume` only succeeds once the earlier session completed one (§6.4). Ask the new session what this desk learned — it must reach for the `openviking` `search` tool, not for a file — and ask it to read its own skill back.
5. **The switch.** The harness discovers the other runtime, switches the desk to it, and reads every projected skill through that runtime's own path — `.claude\skills\` on Claude Code, `.agents\skills\` on Codex. If the other runtime is not installed, that leg is `INCONCLUSIVE` and the cell is still complete.

What fails and what does not: the offline install, the child's readiness, the desk's user, the projection of `desk-improvement` before the launch, the `SKILL.md` of every skill that appeared, the projection at the resumed activation, and the other runtime's path are MarketRig's own and fail the cell. Whether a cycle was closed, an evaluation delivered, a lesson stated, a skill written, the turn captured, and the lesson found again are the agent's and end `INCONCLUSIVE` with their evidence.

## 8. Read the result

`test result: ok` says only that the harness ran; the verdict is in `<bundle>\observations.jsonl`, one JSON line per step:

- a complete E1/E2 cell ends with `"note": "attended cell complete"` and lists `SUBMIT <id>` and `CANCEL <id>` under `actions`; a complete E3 ends the same way after a note naming the attributed action, its `source: TRIGGER`, and the firing id;
- E1/E2's quote-reads step is `INCONCLUSIVE` in every cell;
- E3's agent-dependent steps — no trigger, no firing, a trigger with no `--code`, or code that placed no order — end `INCONCLUSIVE` with what the harness did see, including the script's captured standard output. Everything after a code-bearing firing exists is the daemon's own and fails the cell instead: the completed execution, the queued prompt, and the attribution on any action row;
- a step that timed out ends the cell with `INCONCLUSIVE` and `waited_secs: 900` instead. That is not a defect. Rerun the cell; every run creates a new desk and bundle, so rewrite the project-scoped file for the new `<desk>` and `<bundle>`, and delete the timed-out bundle;
- E4's agent-dependent steps — no session started, no readiness (its first-launch questions unanswered), a result never delivered — end `INCONCLUSIVE`; a delivery the daemon's own rows contradict (a `DELIVERED` prompt naming no runtime or no native session) fails the cell. Whether the delivered text appeared as the session's input is yours to confirm on the console and is recorded `INCONCLUSIVE` by construction;
- E6 skipped entirely reads as one `INCONCLUSIVE` line naming the variables that were unset. A cell that ran ends with a note listing the plugin's own files under `OPENVIKING_HOME`; its capture and `search` legs are `INCONCLUSIVE` by construction, and a cycle never closed, an evaluation never delivered, or a skill never written are `INCONCLUSIVE` with the skill tree and the `SESSION_TURN_ENDED` count the harness did see;
- a mechanical failure (no daemon, no `LIVE` quote, a wrong row) panics the test with the reason; `<bundle>\marketrigd-1.stderr` and `<bundle>\logs\` hold the daemon's side.

The macOS bundles named at the top are the reference shape for a complete cell.

## 9. Evidence and cleanup

- Keep all four bundles per cell: `experiment-<cell>-<stamp>`, `experiment-e3-<cell>-<stamp>`, `experiment-e4-<cell>-<stamp>`, and `experiment-e6-<cell>-<stamp>`. The bundle is the evidence; the `.codex\config.toml` and `.mcp.json` inside it carry no secret, only the data-root path — `<bundle>\data\runtime\credentials.json` is the relocated credential store and holds a provider key and E6's OpenViking seed, and `<bundle>\data\openviking\plugin\<desk-id>\ovcli.conf` holds that desk's derived OpenViking key, both by design. E6's bundle also carries the provisioned virtual environment under `data\openviking\venv\`; delete that subdirectory if the bundle has to be kept small, and nothing else.
- Record each cell's stamps, runtime version, and the commit in the slice's freeze note and the roadmap's evidence line.
- Nothing was written to `~\.codex\config.toml` or Claude Code's user config. If you used the global form from the printout instead, remove it: `codex mcp remove marketrig` / `claude mcp remove marketrig`.
- While E4 or E6 holds the console in raw mode, Ctrl-C is a byte for the session, not a signal for the harness: abort from a second terminal with `pkill -f deps/experiment-` (Windows: `Get-Process experiment-* | Stop-Process`). Aborting the harness that way, or with Ctrl-C during E1–E3, skips its teardown; check for a leftover daemon with `Get-Process marketrigd` and stop it before the next cell. On macOS a hard-stopped run can also leave a `trigger-code` child in its own session; `pkill trigger-code` clears it. Aborting E4 or E6 also leaves your console in raw mode and the launched runtime running: `Get-Process codex, claude` (macOS: `pkill -f 'codex --remote'`), and `stty sane` on macOS restores the terminal. An aborted E6 can additionally leave its memory child alive, which a clean stop would have ended: `Get-Process openviking-server | Stop-Process` (macOS: `pkill -f openviking-server`).
