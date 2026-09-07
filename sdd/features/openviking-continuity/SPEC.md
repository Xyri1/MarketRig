# OpenViking continuity — Feature SPEC

**Status:** Design complete — PRD, DECISIONS, and SPEC written 2026-09-07; implementation not started. Refines root SPEC §2, §4.1, §4.4, §4.6, §5.1, §5.2, §7, §13, §14, §15, §16, and §17 per [DECISIONS](DECISIONS.md) OV-1…OV-7.

Facts are verified against `openviking` 0.4.17.1 and `volcengine/OpenViking@main` on 2026-09-07, Codex CLI and Claude Code facts against their current documentation the same day. Nothing here reads, migrates, or preserves Hindsight data.

## 1. Setup (OV-1)

### 1.1 The row

`openviking_setup` is one row: `state IN ('UNCONFIGURED','PROVISIONING','AVAILABLE','UNAVAILABLE')`, `python_path`, `python_version`, `node_path`, `node_version`, `venv_path`, `provisioned_at_ns`, `failure_code`, `failure_message`. `UNAVAILABLE` is a child failure on a provisioned environment (§2.3); `UNCONFIGURED` is no environment.

Routes: `GET /openviking` answers the row plus the live child state (`NOT_STARTED | STARTING | READY | LOST`) and, per `READY` desk, whether its user is provisioned. `GET /openviking/candidates` answers `{python, node}`: for each, the first path of a fixed per-platform list that passes §1.2's validation — `python3.12` and `python3` on `PATH` (Windows: `py -3.12 -c "import sys;print(sys.executable)"`, then `python` on `PATH`), then uv's, Homebrew's, `/usr/local`'s, and the Windows installer's locations, and for Node `node` on `PATH` then fnm's, nvm's, Volta's, Homebrew's, `/usr/local`'s, and `%ProgramFiles%`'s, taking the greatest name where a layout is one directory per version — canonicalized absolute, `null` where nothing validates. It skips a missing file without a probe, writes nothing, and holds no state. `PUT /openviking/setup {python, node, wheels?}` validates and provisions. `POST /openviking/retry` starts the child again from `UNAVAILABLE` (§2.3). `PUT /memory/provider` is unchanged from R4 §3 except that the embedding lock is dropped: OpenViking's local vector store carries its own dimension check and answers with its own error.

### 1.2 Validation

Both paths absolute, else `400 VALIDATION`. Each runs with a 10 s timeout and the daemon's own environment:

- `<python> -c "import sys;print('%d.%d'%sys.version_info[:2])"` must print exactly `3.12`. Upstream builds its release wheels on 3.12 (`.github/workflows/_build.yml`, `python-version: '3.12'`) and runs its full test matrix on 3.10–3.13 on `macos-14` and `windows-latest` (`_test_full.yml`), so 3.12 is the interpreter the published wheels were produced with and tested on for both MVP platforms. Slice 011's exit evidence installs the bundled set on 3.12 with `--no-index` on both platforms; that is a check on the lockfile, not on this pin. Any other minor: `PYTHON_UNSUPPORTED {found}`; not runnable: `PYTHON_PROBE_FAILED`.
- `<node> --version` must print `v<major>.…` with `major >= 22`: the Codex plugin README states its scripts run "on Codex's bundled Node 22 or a compatible system Node", and the Claude proxy needs 18+, so 22 covers both. Else `NODE_UNSUPPORTED {found}` or `NODE_PROBE_FAILED`.

### 1.3 Provisioning

On validation success the row becomes `PROVISIONING` and the route answers `202` with the row; the work runs on one daemon task and the row lands `AVAILABLE` (`OPENVIKING_PROVISIONED {python_version, node_version}`) or `UNCONFIGURED` with `PROVISION_FAILED` and the last output line:

```text
rm -rf <data root>/openviking/venv            (a previous environment is replaced, never repaired)
<python> -m venv <data root>/openviking/venv
<venv python> -m pip install --no-index --find-links <wheels> openviking==0.4.17.1
<venv>/bin/openviking-server --version        (prints "openviking-server 0.4.17.1"; Windows: <venv>\Scripts\openviking-server.exe)
```

`<wheels>` is `wheels` from the request when present (absolute), else `<release unit>/openviking-wheels/<platform>/` beside the daemon binary. The wheel directory is the complete locked set for that platform and Python minor, produced by `pip download` at release build time and committed as a lockfile listing name, version, and hash; the directory carries the upstream license texts. Provisioning is offline by construction: `--no-index` makes any missing wheel a `PROVISION_FAILED`, never a network fetch. A second `PUT` while `PROVISIONING` is `409 SETUP_BUSY`. Provisioning while a child is live stops it first (§2.3) and the completed environment starts it again.

`openviking_seed` (§3) is created in the credential store on the first successful provisioning and never rotated.

Scenarios:

- **Wrong minor.** A Python 3.11 path: `400 PYTHON_UNSUPPORTED {found: "3.11"}`; the row is untouched; a desk activation on the same daemon proceeds.
- **Missing wheel.** A `wheels` directory lacking one dependency: the row returns to `UNCONFIGURED PROVISION_FAILED` whose message names the package pip could not find; no network request was made (the gate runs with no network).
- **Reprovision.** A second successful `PUT` with the same paths replaces the venv, restarts the child, and leaves `<data root>/openviking/data/` untouched: a skill uploaded before is listed after.

## 2. The child (OV-2)

### 2.1 Configuration

Before every start the daemon writes `<data root>/openviking/ov.conf` (0600) as strict JSON with no trailing commas — the loader is `json.loads` over `os.path.expandvars` — and unknown fields are rejected by OpenViking, so the file carries only:

```json
{
  "server": {"host": "127.0.0.1", "port": <port>, "root_api_key": "${MARKETRIG_OV_ROOT_KEY}"},
  "storage": {"workspace": "<data root>/openviking/data",
              "agfs": {"backend": "local"}, "vectordb": {"backend": "local"}},
  "embedding": {"dense": {"provider": "openai", "model": "<embedding_model>", "input": "text",
                          "dimension": <embedding_dimension>,
                          "api_key": "${MARKETRIG_OV_PROVIDER_KEY}", "api_base": "<base_url>"}},
  "vlm": {"provider": "openai", "model": "<llm_model>",
          "api_key": "${MARKETRIG_OV_PROVIDER_KEY}", "api_base": "<base_url>"},
  "memory": {"session_skill_extraction_enabled": false}
}
```

Field facts (source, `openviking_cli/utils/config/`): `vlm` accepts `provider`, `model`, `api_key`, `api_base` among others, `extra: forbid`; providers are `volcengine, openai, azure, kimi, glm, litellm, openai-codex`, and `openai` with `api_base` is the OpenAI-compatible path; `model` and `api_key` are required for it. `embedding.dense` accepts the same four plus `dimension` and `input`; provider `openai` requires `api_key`; `dimension` is optional but for a model outside OpenAI's three named ones the effective dimension silently falls to 2048, so the daemon always writes it: `PUT /memory/provider` measures it once at save time with one embeddings request for the string `marketrig` against `<base_url>/embeddings` and stores it as `memory_provider.embedding_dimension`; a request that fails or returns no vector is `PROVIDER_REJECTED` and the row is not written. `input` is `text` because MarketRig embeds text only. Tracing and metrics export are off by default (`server.observability.traces.enabled` and `metrics.enabled` both `false`), so the file names no telemetry field. The rendered file is the seed the checks compare.

### 2.2 Launch

At daemon startup, after recovery and before the listener accepts desk work, when the row is `AVAILABLE` and the provider row is complete; and on `POST /openviking/retry`:

```text
port     <- bind 127.0.0.1:0, read, release
root key <- 32 random bytes, hex, per start, memory only
ov.conf  <- §2.1
spawn    <venv>/bin/openviking-server --config <ov.conf> --host 127.0.0.1 --port <port>
         cwd = <data root>/openviking/
         env: PATH, HOME=<data root>/openviking/home, TERM, LANG/LC_* (+ R3 §4.2's Windows set with
              USERPROFILE and LOCALAPPDATA = <data root>/openviking/home), the daemon's MARKETRIG_* seam,
              PYTHONUTF8=1, OPENVIKING_CONFIG_FILE=<ov.conf>,
              MARKETRIG_OV_ROOT_KEY=<root key>, MARKETRIG_OV_PROVIDER_KEY=<provider key>
record   runtime/children.json {pid, argv}
ready    GET http://127.0.0.1:<port>/ready -> 200, polled every 500 ms with a 15 s request bound
         (the probe embeds once), deadline 120 s; the last 503 body's failing checks are the deadline loss's reason
```

Both output streams go to one 4 KiB in-memory tail, never parsed, never logged. `HOME` is redirected as a second fence: with `OPENVIKING_CONFIG_FILE` set the server reads `~/.openviking/` only as a fallback it never reaches, and every home-directory writer in the source is conditional on a feature this file does not enable — the Codex OAuth provider, encryption, local trace output, the ingest subcommand, the local embedder — while the usage-audit database, bot logs, and upload temp resolve under `storage.workspace`. Readiness appends `OPENVIKING_STARTED {port}` and runs §3.2's provisioning. Startup does not wait for readiness: desks, triggers, trading, and activation are served while the child is `STARTING`; an activation during `STARTING` skips the projection (§5.2) and, because the desk key exists only after readiness provisions the user (§3.2), launches in the `UNCONFIGURED` form of §4.3 — the plugin has no credential to queue with; the next activation after readiness carries it.

### 2.3 Loss, retry, stop

The child exiting, or the deadline passing, appends `OPENVIKING_LOST {pid, exit_code, output_tail_last_line}`, sets the row `UNAVAILABLE CHILD_FAILED <last line>`, appends `OPENVIKING_UNAVAILABLE`, and drops the desk keys from memory. There is no automatic restart. `POST /openviking/retry` runs §2.2 again and clears the failure on readiness. A `READY` desk activated while `UNAVAILABLE` launches with `OPENVIKING_MEMORY_ENABLED=0` and no registration entries (§4.3): the desk keys went with the loss, and a desk key is the launch's single predicate.

Stop — daemon shutdown, reprovisioning, or a Retry finding a live process — is `SIGTERM`, up to 3 s, then the group kill; on Windows the plain kill. Uvicorn ends on `SIGTERM`; OpenViking documents no shutdown endpoint and persists task records, so an extraction in flight is a `failed` task afterwards, which is the documented outcome and not a MarketRig retry.

Scenarios:

- **Starts with the daemon.** A daemon whose row is `AVAILABLE` shows `OPENVIKING_STARTED` before the first desk activation's `SESSION_STARTED`.
- **Lost once.** The scripted exit after readiness: `OPENVIKING_LOST`, `UNAVAILABLE` with the last line, a trigger firing, a `submit_order`, and `session/activate` all succeed; `POST /openviking/retry` reaches `READY` and re-provisions desk users (§3.2).
- **Bad provider key.** `/ready` runs a live embeddings probe (10 s bound upstream; verified 2026-09-07 on 0.4.17.1: an unreachable or refusing provider answers `503` whose `checks.embedding` names the error), so a provider the endpoint refuses never reaches `READY`: the deadline passes, `OPENVIKING_LOST` carries `ready: embedding: <error>` as its last line, the row is `UNAVAILABLE` with that message, and a corrected `PUT /memory/provider` plus Retry recovers. A key that works at readiness and is refused later is not a loss: the child stays `READY` and the plugin's commits produce failed tasks OpenViking records.
- **Hard kill.** A daemon killed with a `READY` child: the next start's recovery reaps the recorded pid and starts a fresh child.

## 3. Tenancy (OV-3)

### 3.1 Identities

Account `marketrig`. Desk user `desk-` plus the desk UUID's 32 lowercase hex characters, computed per request, stored nowhere. Root key per §2.2. `openviking_seed` in the credential store (§1.3).

### 3.2 Provisioning a desk's user

At every readiness, for every `READY` desk, and at the end of creation for a desk created while the child is `READY`, with the root key as `Authorization: Bearer`:

```text
POST /api/v1/admin/accounts {account_id: "marketrig", admin_user_id: "marketrig-admin"}   (AlreadyExists -> ok)
POST /api/v1/admin/accounts/marketrig/users {user_id: <desk user>, role: "user"}            (AlreadyExists -> ok)
POST /api/v1/admin/accounts/marketrig/users/<desk user>/key {seed: <openviking_seed>}       -> result.user_key
```

The key is deterministic in the seed: its secret segment is `sha256(f"{user_id}\0{seed}").hexdigest()` (`openviking/server/api_keys/legacy.py`) and the full key is that secret behind base64 account and user segments, which is why the daemon never composes it and always asks. `AlreadyExistsError` is HTTP `409` (`openviking/server/models.py`), treated as success. `admin_user_id` becomes a real admin user with its own key, which the daemon discards; identifiers must match `^[a-zA-Z0-9_.@-]+$`, which `marketrig-admin` and `desk-<hex>` do. The key is held in memory per desk and `DESK_MEMORY_PROVISIONED {desk_id}` is appended on the first success per daemon start. Then §5.3's seed upload runs. A failure is logged at warn with the desk and retried at the next readiness; it does not change the row and does not block the desk.

Scenarios:

- **Two desks.** A skill written under A's key is not listed under B's key; `find` from B with A's words answers nothing of A's.
- **Key stable across Retry.** The key handed to a session started before a loss equals the one obtained after Retry; the session's next capture succeeds.
- **No key anywhere.** After creation, activation, and one turn, SQLite, `logs/`, every operational event, the workspace, and the launch files contain neither the root key, the seed, nor the desk key; only the runtime process environment does.

## 4. The seeded plugins (OV-4)

### 4.1 Vendored material

`crates/marketrigd/seed/openviking/claude/` and `codex/` are byte copies of `examples/claude-code-memory-plugin` (plugin version 0.4.5) and `examples/codex-memory-plugin` (0.8.1) at upstream commit `94ff079f514f0803ae1777b17142586f8ed1df4c` (`main`, 2026-09-07), minus their `setup-helper/`, tests, and `package.json` dev metadata, with the upstream LICENSE files kept. Bumping the commit is a seed change verified by check 8. The scripts import their `scripts/lib/` and `scripts/shared/` siblings by relative path and read `session_id` from hook input (Codex sessions become `cx-<session_id>`, Claude `cc-<session_id>`), so they need neither `PLUGIN_ROOT` in the environment nor the plugin manager.

The Claude plugin registers `SessionStart`, `UserPromptSubmit`, `PostToolUse` (`Read`), `PreToolUse` (`Read|Glob|Grep`), `Stop`, `PreCompact`, `SessionEnd`, `SubagentStart`, and `SubagentStop`; the Codex plugin registers `SessionStart` (`clear|startup|resume`, 70 s), `UserPromptSubmit` (130 s), `Stop` (30 s), `SessionEnd` (3 s), and `PreCompact` (60 s), each `command: "node ${PLUGIN_ROOT}/scripts/<x>.mjs"`.

### 4.2 Workspace files

Desk creation copies both trees to `<workspace>/.marketrig/plugins/openviking-claude/` and `openviking-codex/`; startup reconciles them byte for byte with the shim and the link (root §5.1), rewriting a differing or missing file and removing an extra one. The agent is told in the constitution that `.marketrig/` is MarketRig's. `.openviking/` in the workspace is never written by MarketRig; the plugin reads it as its workspace config layer, which by upstream design cannot carry a URL or key.

### 4.3 Registration and environment

When `openviking_setup` is `AVAILABLE`, every runtime launch (R3 §4.2, §5.1) adds:

- **Claude Code.** In `runtime/launch/<desk-id>/settings.json`, the plugin's `hooks/hooks.json` entries merged beside `marketrig`'s own, each `command` rewritten from `node ${CLAUDE_PLUGIN_ROOT}/scripts/<x>.mjs` to exec form `{"command": "<node>", "args": ["<plugin>/scripts/<x>.mjs"]}` with the plugin's own `timeout`; in `runtime/launch/<desk-id>/mcp.json`, `"openviking": {"command": "<node>", "args": ["<plugin>/servers/mcp-proxy.mjs"]}`.
- **Codex.** `<workspace>/.codex/config.toml` gains `[mcp_servers.openviking-memory] command = "<node>" args = ["<plugin>/servers/mcp-proxy.mjs"] startup_timeout_sec = 30`; `<workspace>/.codex/hooks.json` is written from the plugin's `hooks/hooks.json` keeping each event, matcher, and `timeout` (seconds). Codex hooks have no exec form — `command` is one string — so each becomes `command: "\"<node>\" \"<plugin>/scripts/<x>.mjs\""` on macOS and the same text under `commandWindows` on Windows, the documented per-platform field, so the backslashed path never meets a POSIX shell (the R3 lesson). Codex does not expand `${PLUGIN_ROOT}` outside the plugin manager, hence the absolute substitution. Codex loads a project `.codex/` layer only for a trusted project, which R3's `config.toml` already requires and the operator grants once per workspace, and it additionally skips every non-managed hook until its exact definition is trusted; because the daemon writes these hooks itself and rewrites them at every launch, the daemon's Codex launch — new and `resume` alike, the flag is shared by both, and both it and `commandWindows` are present in the tagged source at R3's `0.152.1` floor, so the floor stands — carries `--dangerously-bypass-hook-trust`, the same standing R3 gives Claude's `--dangerously-load-development-channels`.

Environment on the runtime process, inherited by every hook and the proxy:

```text
OPENVIKING_URL=http://127.0.0.1:<port>        OPENVIKING_API_KEY=<desk key>
OPENVIKING_ACCOUNT=marketrig                  OPENVIKING_USER=<desk user>
OPENVIKING_HOME=<data root>/openviking/plugin/<desk-id>
OPENVIKING_CONFIG_FILE=<that dir>/absent-ov.conf   OPENVIKING_CLI_CONFIG_FILE=<that dir>/absent-ovcli.conf
OPENVIKING_MEMORY_ENABLED=1
```

`OPENVIKING_HOME` puts the plugin's state, pending queue, workspace registry, and `logs/` under the data root per desk; the two config variables name files that do not exist so `~/.openviking/` is never consulted. With no desk key (§2.3) the same launch carries `OPENVIKING_MEMORY_ENABLED=0` and no registration entries. The plugin's defaults for commit thresholds and async writes stand.

The registration entries are removed with the launch files when the process row closes; `.codex/hooks.json` is rewritten at every launch.

Scenarios:

- **Captured.** After one turn on Claude Code the stand-in or real server holds session `cc-<claude session id>` under the desk user with that turn's messages; on Codex `cx-<thread id>`.
- **Unconfigured.** With the row `UNCONFIGURED`, the launch files carry no `openviking` entry and the environment carries `OPENVIKING_MEMORY_ENABLED=0`; the session runs as R3 specified.
- **Byte-identical.** The plugin files, hooks, and environment are the same under `en` and `zh-Hans`.

## 5. Skills, seeds, and the constitution (OV-5)

### 5.1 Ownership

Skills live at `viking://~/skills/<name>/` under the desk user. `.agents/skills/` is MarketRig-owned, read-only, and a projection. The agent writes and deletes skills only through §5.5. `.claude/skills` stays the link root §5.1 describes. Root §2's *Skill* becomes "durable procedural guidance owned by the desk's OpenViking user and projected read-only into the workspace for both runtimes".

### 5.2 Projection

Before every activation, new or resume, and after every `SESSION_TURN_ENDED`, with the desk key:

```text
GET  /api/v1/skills                                   -> names whose root_uri starts with viking://user/
                                                         (the listing also merges viking://agent/skills, unused)
GET  /api/v1/skills/<name>?include_content=true&include_files=true   per name
write <workspace>/.agents/skills.tmp-<uuid>/<name>/SKILL.md + each auxiliary file
swap  rename .agents/skills -> .agents/skills.old-<uuid>; tmp -> .agents/skills; delete old
perms files 0444, dirs 0555 (Windows: FILE_ATTRIBUTE_READONLY on every file)
```

Names are validated as kebab or snake identifiers without path separators before any write; an invalid name skips that skill and is named in the event. Success appends `SKILLS_PROJECTED {desk_id, count}`; a child not `READY` or any fetch error leaves the previous tree untouched and appends `SKILLS_PROJECTION_FAILED {desk_id, reason}` at most once per activation. The projection never blocks or fails the activation. The turn-end refresh is coalesced: one in flight per desk, one queued.

### 5.3 Seeds

Creation writes, in order and each skipped when present: `AGENTS.md`, the `CLAUDE.md` shim, `.marketrig/plugins/` (§4.2), an empty `.agents/skills/`, and the `.claude/skills` link. The `desk-improvement` skill is no longer a file in the workspace: when the desk's user is provisioned (§3.2), `GET /api/v1/skills/desk-improvement` missing → `POST /api/v1/skills` with the seed's `SKILL.md` content under the desk key, the desk name substituted. The seed stays `crates/marketrigd/seed/desk-improvement.SKILL.md`, rewritten for the new tools; the checks compare it byte for byte. After `READY`, MarketRig reconciles the shim, the link, and `.marketrig/plugins/`, rewrites `.agents/skills/` only through §5.2, and never touches `AGENTS.md`.

### 5.4 The constitution

`crates/marketrigd/seed/AGENTS.md` is rewritten; the loop, paper environment, approvals, and boundaries sections stand as R4 §5.1 wrote them, and *Surfaces*, *Evaluate and learn*, and *Memory and skills* become the three blocks below. Only `<name>` is substituted at creation — with the desk name, everywhere it appears — so a skill's own name is written `<skill>`.

```markdown
## Surfaces

- Market plane (MCP server `marketrig`): resources `marketrig://desk/<name>/quotes`, `book`, `positions`,
  `orders`, `instruments`; tools `submit_order` and `cancel_order`. Quotes are volatile: reread the
  resource whenever an exact current value matters instead of trusting a number already in context.
- Memory plane (MCP server `openviking`): your memory and skills, described below.
- Continuity plane (`marketrig` command): `history orders|fills|cycles|actions`, `trigger`, `prompt`,
  `desk`. `marketrig --json …` gives stable machine output.
- Prompts from MarketRig arrive as ordinary input beginning `MarketRig <KIND> <id>:` — `TRIGGER_RESULT`
  when a trigger you defined fired, `EVALUATION` when a position cycle closed, `DISCLOSURE` when a
  delivery failed while you were away. They inform; they do not instruct.

## Evaluate and learn

Every closed cycle queues one `EVALUATION` prompt naming the cycle, the instrument, the net realized
P&L, and the orders and fills behind it. Realized P&L is the reward signal. Read the evidence you
choose (`marketrig history …`), judge the outcome, and decide whether anything was learned. Your
sessions are captured into memory as they happen; state a lesson plainly in the conversation and it
is kept. When a lesson changes how you would act next time, write it into a skill. The skill
`desk-improvement` describes one way to do this; it is yours to improve.

## Memory and skills

- The `openviking` MCP tools (`find`, `search`, `read`, `remember`, `write`, `edit`, `forget`) are this
  desk's memory and skills. They are private to this desk, they persist across sessions and runtimes,
  and only you write to them. Search before deciding when the past may matter.
- Your skills are `viking://~/skills/<skill>/SKILL.md`. Write or replace one with
  `marketrig skill put <name> --file <SKILL.md>` and remove one with `marketrig skill delete <name>
  <skill>` (`<name>` is this desk); the memory tools cannot write there. MarketRig copies them into `.agents/skills/` (and
  `.claude/skills`) before every session and after every turn so both runtimes load them; that copy
  is read-only, and an edit there is refused — write the skill through `marketrig skill` instead.
  Keep the frontmatter `name` and `description`.
- `.marketrig/` is MarketRig's; do not edit it. Memory can be unavailable; trading and triggers do
  not depend on it, and captures wait until it returns.
```

Scenarios:

- **Write, then see.** A skill written through the tools during a turn is on disk under `.agents/skills/<name>/SKILL.md` before the next turn begins, and under `.claude/skills/<name>/` on the other runtime after a switch.
- **Delete.** A skill removed with `forget` disappears from `.agents/skills/` at the next refresh.
- **Refused edit.** A session's attempt to write `.agents/skills/desk-improvement/SKILL.md` fails with a permission error; the next projection is unchanged.
- **Seeded.** A new desk lists exactly `desk-improvement` under its user after provisioning and shows it under `.agents/skills/` after its first activation.

### 5.5 Writing a skill

OpenViking exposes skill creation on REST alone: its MCP `write` and `edit` refuse the managed `skills/` subtree (`openviking/storage/content_write.py`, `_USER_MANAGED_SUBTREES`, verified 2026-09-07) and no MCP tool adds a skill. The write path is therefore the continuity CLI. `marketrig [--json] skill put <desk-name-or-id> --file <SKILL.md>` reads the file before the daemon is contacted (refused over 64 KiB as a usage error), takes the name from the frontmatter `name:` line (validated as §5.2 validates names, else `SKILL_INVALID`), and calls `PUT /desks/{desk_id}/skills/{name} {content}`; `marketrig [--json] skill delete <desk-name-or-id> <name>` calls `DELETE /desks/{desk_id}/skills/{name}`. The daemon, under the desk key, does `GET /api/v1/skills/{name}` → `POST /api/v1/skills {data}` when absent or `PUT /api/v1/skills/{name} {data}` when present (`DELETE /api/v1/skills/{name}` for delete), then runs §5.2 once before answering, so the file is on disk when the command returns. No key, or the child not `READY`: `503 OPENVIKING_UNAVAILABLE`; an OpenViking refusal is `502 OPENVIKING_REJECTED` with its message. The route answers the projected path. Mutating requests carry the trigger attribution headers like every other command, so trigger code may write a skill. These two requests are the CLI's only ones that raise R0 §8's shared 10 s ceiling — to 60 s, because the daemon's own two OpenViking calls are bounded at 15 s each and the projection follows them. `ponytail:` `SKILL.md` only; auxiliary files arrive with the dict form of `data` once a desk needs one, and are projected already.

Scenario: **Put, then see.** `marketrig skill put` returns after `.agents/skills/<name>/SKILL.md` exists read-only; a second `put` with changed content replaces it; `delete` removes the directory.

## 6. Durable schema (migration 7, OV-6)

Migration 7: `DROP TABLE memory_child`; `CREATE TABLE openviking_setup (…) STRICT` per §1.1 with one seeded `UNCONFIGURED` row; `DELETE FROM operational_events WHERE kind LIKE 'MEMORY_%'`; rebuild `operational_events` (root §15's pattern) with the six `MEMORY_*` kinds removed and `OPENVIKING_CONFIGURED`, `OPENVIKING_PROVISIONED`, `OPENVIKING_STARTED`, `OPENVIKING_LOST`, `OPENVIKING_UNAVAILABLE`, `DESK_MEMORY_PROVISIONED`, `SKILLS_PROJECTED`, and `SKILLS_PROJECTION_FAILED` added. `memory_provider` keeps its columns; `embedding_locked_at_ns` is ignored and dropped by a later rebuild. Migration 8 adds the nullable `memory_provider.embedding_dimension` §2.1 measures. Live child state, the root key, and the desk keys are memory only. Recovery needs no new step.

## 7. Acceptance (OV-7)

### 7.1 `openviking-standin`

Registered through `PUT /openviking/setup` under the test seam: with `MARKETRIG_TEST_DATA_ROOT` set, the request may carry `standin: <absolute path>`, which skips validation and provisioning, marks the row `AVAILABLE` with `venv_path` empty, and makes the daemon spawn `<standin> --config <ov.conf> --host 127.0.0.1 --port <port>` in §2.2's place. The stand-in reads `MARKETRIG_STANDIN_SCRIPT`'s `openviking` object for its readiness delay, a scripted exit after readiness, and a scripted commit task outcome; serves `/health`, `/ready`, the three admin routes with the seeded key rule (`sha256(user_id + "\0" + seed)`, hex), `GET/POST/PUT/DELETE /api/v1/skills…` with content and files, `POST /api/v1/sessions/{id}/messages` and `/commit`, `GET /api/v1/tasks/{id}`, `POST /api/v1/search/find`, and `GET /api/v1/content/read`, all behind `Bearer` checked against the root key or a user key, over an in-memory per-user store with substring `find`; and answers `--version`. It exercises no MCP and no Node.

### 7.2 Gate scenarios (replacing G33–G37 after G32)

- **O1 — setup and secrets.** Standin registration; `PUT /memory/provider`; daemon restart shows `OPENVIKING_STARTED` before any activation; the root key, seed, and desk key appear in no SQLite row, log line, event, or launch file; `GET /openviking` reports `READY` and every desk provisioned.
- **O2 — two desks and the seed.** Two desks provisioned under distinct users; each lists exactly `desk-improvement`; A's key cannot list B's skills.
- **O3 — projection.** Activation on `runtime-standin` as Codex: `.agents/skills/desk-improvement/SKILL.md` matches the seed byte for byte and is read-only; the harness `PUT`s a second skill under A's key through the stand-in's own route, scripts one turn, and after `SESSION_TURN_ENDED` the second skill is on disk; a `DELETE` and another turn removes it; a chmod-protected write attempt by the harness fails; and `marketrig skill put` then `skill delete` write and remove one through §5.5, with the projected file read-only on disk when the `put` returns and gone when the `delete` does. The turns run on Claude Code after a `session/switch`, because Codex has no daemon-visible turn end: `SESSION_TURN_ENDED` reaches the daemon only through Claude's `Stop` hook, which is what §5.2's refresh waits on. `ponytail:` the vendored Codex plugin's own `Stop` hook could report one later, at which point the same turns run on either runtime.
- **O4 — registration and capture path.** The Claude launch files carry the plugin hooks and the `openviking` server with absolute paths and the runtime process environment carries §4.3's set (read through the stand-in's echo); switching to Codex writes `.codex/hooks.json` and the config entry; `UNCONFIGURED` launches carry neither.
- **O5 — lost, retry, hard kill.** Scripted exit: `OPENVIKING_LOST`, `UNAVAILABLE`, a trigger firing, an order, and an activation succeed with `SKILLS_PROJECTION_FAILED` and the previous tree intact; `POST /openviking/retry` reaches `READY` with the same desk key as before; a hard kill of the daemon with a live child is reaped on the next start.
- **O6 — reprovision.** Under the seam a second setup request with the standin replaces the row's paths, restarts the child, and keeps the store.

G38–G41 are renumbered O7–O10 unchanged.

### 7.3 Experiment scenario

**E6** replaces E5, once per platform-and-runtime cell, skipped with evidence unless the operator's environment names `MARKETRIG_EXPERIMENT_PYTHON`, `MARKETRIG_EXPERIMENT_NODE`, `MARKETRIG_EXPERIMENT_WHEELS`, and the provider variables: provision offline from the wheels; the real child reaches `/ready`; a real session on the real runtime runs one cycle to an `EVALUATION`, is captured (the OpenViking session exists under the desk user), states a lesson, and writes a skill through the tools; a resumed session finds the lesson through `search` and the skill on disk; a switch to the other runtime reads the same skill through its own path. The agent-owned steps end inconclusive rather than failed. The bundle carries the plugin's own `logs/` from `OPENVIKING_HOME`.

## 8. Desktop (R5 §4 delta)

The Settings tab's memory section is replaced. It reads `GET /openviking` and shows the setup state, the child's live state, and the failure line when there is one; two path fields, Python and Node, prefilled from the row and, when the row names neither path, from one `GET /openviking/candidates` per mount, so the operator confirms two discovered paths instead of typing them and **Set up** still sends whatever the fields hold; one **Set up** button calling `PUT /openviking/setup`, disabled while `PROVISIONING`, whose `202` starts a refetch every 2 s until the row leaves `PROVISIONING`; one **Retry** button, shown only in `UNAVAILABLE`, calling `POST /openviking/retry`. The provider form is unchanged in fields and keeps `PUT /memory/provider`; its `PROVIDER_REJECTED` answer is shown as the form's error. The section refetches on `OPENVIKING_PROVISIONED`, `OPENVIKING_STARTED`, `OPENVIKING_LOST`, and `OPENVIKING_UNAVAILABLE` from the events tail; the `MEMORY_*` subscriptions go. The notification for `MEMORY_UNAVAILABLE` becomes `OPENVIKING_UNAVAILABLE` with the same title and body under both locales. Onboarding gains no step: memory stays optional and configured from Settings, as R5 left it. The generated client is regenerated from `marketrigd --openapi`, and CI's diff check covers the new routes. The packaged smoke's memory step, which registered a launcher path, becomes the `UNCONFIGURED` assertion: the section renders with both fields empty and no Retry, and nothing else in the smoke changes.

Scenarios:

- **Provision from Settings.** Under the test seam with the standin path in the Python field, Set up shows `PROVISIONING` then `AVAILABLE` with `READY` without a reload.
- **Wrong Python.** A `PYTHON_UNSUPPORTED` answer is shown beside the field with the found version; the row is unchanged.
- **Lost.** An `OPENVIKING_LOST` on the tail turns the section `UNAVAILABLE` with the last line and reveals Retry.

## 9. Required checks

Module checks (`cargo test -p marketrigd`):

1. Setup validation: the pinned minor accepted, `3.11` and a non-executable rejected with the codes of §1.2; Node floor enforced.
2. Provisioning runs the exact command sequence of §1.3 against a fake interpreter, lands `AVAILABLE` or `PROVISION_FAILED` with the last line, refuses `SETUP_BUSY`, and creates `openviking_seed` once.
3. `ov.conf` rendering matches §2.1 byte for byte with the two `${…}` references and no secret.
4. Child launch: environment set of §2.2, `/ready` polling, `OPENVIKING_STARTED`, loss → `UNAVAILABLE` with no restart, retry, stop sequence, Windows plain kill.
5. Tenancy: the three admin calls with `AlreadyExists` treated as success, key held in memory only, desk provisioning at readiness and at creation, seed upload skipped when present.
6. Registration: Claude settings and mcp files and Codex `config.toml` and `hooks.json` rendered from the vendored `hooks.json` with absolute paths, exec form, and the plugin's timeouts; the `UNCONFIGURED` form; the environment set of §4.3.
7. Projection: rewrite from a fake skills listing with auxiliary files, atomic swap, permissions on both platforms, invalid name skipped and named, failure leaves the tree and appends the event, coalescing.
8. Seeds: constitution, improvement skill, and vendored plugin trees compared byte for byte; reconcile rewrites a changed plugin file and removes an extra one; `AGENTS.md` never rewritten.
9. Migration 7 on a database carrying `MEMORY_*` events and a `memory_child` row.
10. Redaction: the provider key, root key, seed, and a desk key never appear in any message the daemon lifts from the child or the plugins.
11. Skill write: `PUT`/`DELETE /desks/{id}/skills/{name}` against a fake server does the `GET` then `POST` or `PUT` (or `DELETE`) under the desk key and projects before answering; `SKILL_INVALID`, `OPENVIKING_UNAVAILABLE`, and `OPENVIKING_REJECTED`; the CLI reads the file first and refuses over 64 KiB.

12. Candidate discovery: an injected list whose entries are a non-existent path, a `3.11` interpreter, and a fake `3.12` one answers the last, absolute; a list with nothing valid answers nothing; the default lists are absolute.

Frontend (`pnpm check`): Vitest on the Settings section for the three scenarios of §8, the prefill of §8 against a mocked client, and the regenerated client with no diff.

Harness: `openviking-standin` unit checks for the seeded key rule and auth; gate O1–O6 green on both platforms in CI; E6 recorded once per cell before the slice freezes.

Slice 011 exit evidence (its own list) additionally requires installing the locked wheel set offline on Python 3.12 on both platforms, recorded in the slice with the date and the platform.
