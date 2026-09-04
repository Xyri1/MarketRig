# R4 — Memory, skills, and the closed loop: Feature SPEC

*Decision basis: per D16, D17, D18, D19, D20, D21, D22, D47, D49, D65, D67, D71, D73, D75 and this feature's R4-1 … R4-6.* This document refines root `sdd/SPEC.md` §4.4, §4.6, §5.1, §5.2, §13.2, §15, §16, and §17. Where it names a Hindsight environment variable, route, or field, the fact was verified on 2026-09-04 against `hindsight-api-slim[embedded-db]==0.9.2`.

## 1. Workspace additions

- `crates/marketrigd/src/memory.rs` (the child, the provider rows, the bank routes; R4-1 … R4-3), `desk.rs` grows the seeds and the link reconciliation (R4-4); `store/005_r4.sql` (R4-5).
- `crates/marketrig` gains the `memory` group (R4-3).
- `crates/marketrig-acceptance/src/bin/memory-standin.rs` (R4-6).
- New pins at plan time: the credential store as `keyring-core =1.0.0` plus one store crate per platform — `apple-native-keyring-store =1.0.2` (macOS) and `windows-native-keyring-store =1.1.0` (Windows) — set as the default store explicitly at daemon start, skipping the `keyring` facade and its CLI dependencies; those are the new crates; `hindsight-api-slim[embedded-db]==0.9.2`, the wheel every launcher must carry, recorded here and installed by R6's bundle. The daemon's Hindsight client is `reqwest`, already pinned. No other crate.

## 2. The memory child (R4-1)

### 2.1 Discovery and the row

`memory_child` starts as one `UNCONFIGURED` row. `POST /memory/discover {"executable": path}` (an absolute path; relative is `400 VALIDATION`) runs `<executable> --help` with a 10 s timeout, requires exit `0` and `HINDSIGHT_API_PORT` in standard output — standard output only: the probe runs with none of §2.2's variables set, so the launcher prints an unrelated missing-`sentence-transformers` warning to standard error that must not fail it — and writes `AVAILABLE` with `validated_at_ns` or `UNAVAILABLE` with `failure_code IN ('NOT_FOUND','CAPABILITY_MISSING','PROBE_FAILED','CHILD_FAILED')`; `POST /memory/retry` clears `CHILD_FAILED` and re-validates. Startup step 6b (after 6a, before binding) re-validates an `AVAILABLE` row the same way and is skipped under `MARKETRIG_TEST_DATA_ROOT`. Every answer is the row, secrets-free; `MEMORY_CONFIGURED {what: "child", executable_path, state}` is appended on each change.

### 2.2 Launch

Started by the first memory operation that finds no live child (never by startup, never by status):

```text
port    <- bind 127.0.0.1:0, read, release
bearer  <- 32 random bytes, hex, minted per start, held in memory only
spawn   <executable>            cwd = data root (which carries no .env for the launcher's dotenv loader)
        env: PATH, HOME=<data root>/hindsight, TERM, LANG/LC_* (+ the Windows set of R3 §4.2, with
             USERPROFILE and LOCALAPPDATA also = <data root>/hindsight), the daemon's own
             MARKETRIG_* variables (root §17's seam, as R3 §4.2 forwards them to a runtime;
             a user's daemon carries none), plus:
        PYTHONUTF8=1      (the banner is block glyphs; off a terminal Python otherwise writes the
                           system code page, and a GBK Windows box exited on the first one, 2026-09-04)
        HINDSIGHT_API_HOST=127.0.0.1            HINDSIGHT_API_PORT=<port>
        HINDSIGHT_API_WORKERS=1                 HINDSIGHT_API_LOG_LEVEL=warning
        HINDSIGHT_API_DATABASE_URL=pg0://marketrig
        HINDSIGHT_API_TENANT_EXTENSION=hindsight_api.extensions.builtin.tenant:ApiKeyTenantExtension
        HINDSIGHT_API_TENANT_API_KEY=<bearer>
        HINDSIGHT_API_MCP_ENABLED=false         HINDSIGHT_API_OTEL_TRACES_ENABLED=false
        HINDSIGHT_API_LLM_PROVIDER=openai       HINDSIGHT_API_LLM_BASE_URL=<base_url>
        HINDSIGHT_API_LLM_API_KEY=<key>         HINDSIGHT_API_LLM_MODEL=<llm_model>
        HINDSIGHT_API_EMBEDDINGS_PROVIDER=openai
        HINDSIGHT_API_EMBEDDINGS_OPENAI_BASE_URL=<base_url>
        HINDSIGHT_API_EMBEDDINGS_OPENAI_API_KEY=<key>
        HINDSIGHT_API_EMBEDDINGS_OPENAI_MODEL=<embedding_model>
        HINDSIGHT_API_RERANKER_PROVIDER=rrf
record  runtime/children.json {pid, argv}
ready   GET http://127.0.0.1:<port>/health -> 200, polled every 500 ms, deadline 120 s
```

The child runs under the R2 containment primitive (`ProcessSession` on macOS, a Job Object on Windows), its standard output and standard error both captured into one 4 KiB tail the daemon keeps in memory (the launcher logs to standard output; a startup failure's traceback goes to standard error, verified 2026-09-04 against OpenRouter) — the launcher logs its banner, its startup summary, and every `logging` record to standard output, and the banner carries ANSI escapes even off a terminal, so the tail is raw bytes and never parsed; nothing it prints is logged at `info`. Readiness appends `MEMORY_STARTED {pid}`; live state `NOT_STARTED → STARTING → READY`. A memory operation arriving during `STARTING` waits for readiness inside its own timeout (§4.3).

The 120 s deadline is measured headroom, not a guess: on 2026-09-04 on macOS arm64 this exact environment reached `/health 200` in 12.6 s cold (a fresh `HOME`, so pg0 installed PostgreSQL 18.1.0 and ran `initdb` first) and 6.1 s warm (an existing instance), and until the port is listening the poll sees connection-refused rather than a non-`200` answer, so readiness is the first successful request and nothing else.

On Windows the redirection does not reach `pg0`: it resolves the profile through the known-folder API and ignores `HOME`, `USERPROFILE`, and `LOCALAPPDATA` alike — verified 2026-09-04 on Windows 11 against the pinned wheel, `pg0 start --name marketrig` under a scratch value for all three wrote `C:\Users\<user>\.pg0\instances\marketrig\` and nothing under the scratch directory. A Windows data root therefore carries no database, every daemon on the machine shares that one cluster (a second start finds it running), and one desk's bank is another's only by name (§4). Deleting a Windows root leaves the cluster in place (root §18).

### 2.3 Loss, restart, and stop

The child exiting, or the health deadline passing, ends the attempt: the process tree is terminated, the record dropped, `MEMORY_LOST {pid, exit_code, output_tail_last_line}` appended, live state `LOST`. A provider that refuses the key is not a loss — the child is healthy and the operation carries the failure (§4.3). The next operation starts the child again once; if that attempt is lost too before a readiness in between, the row becomes `UNAVAILABLE CHILD_FAILED` with the last output line as `failure_message`, `MEMORY_UNAVAILABLE` is appended, and every operation answers `MEMORY_UNAVAILABLE` until `POST /memory/retry`. A provider change (§3) stops a live child so the next operation starts it with the new environment. Quit stops the child after every terminal and the Codex app-server, within the existing 5 s shutdown bound.

A stop — Quit, a provider change, a loss whose child is still alive — is `SIGTERM` to the child first, up to 3 s for it to end on its own, then the group kill: pg0 starts the embedded PostgreSQL in its own session, outside the containment group, and Hindsight stops it only from its `SIGTERM` handler. After every stop and every loss the daemon also sends `SIGTERM` to the postmaster named by `<data root>/hindsight/.pg0/instances/marketrig/data/postmaster.pid` when that file exists, because a child that exits on its own (a startup failure) leaves the cluster running. On Windows neither signal exists; the plain kill stands and a cluster left by a crash waits for a later `pg0 stop` (deferred, root §18). Found by the E5 cells on 2026-09-04, which each left a PostgreSQL behind.

Scenarios:

- **Never on startup.** A daemon with an `AVAILABLE` child row and a configured provider starts, serves desks, and shows `live: NOT_STARTED` until the first retain.
- **Wrong key, provider-dependent.** Against OpenRouter the startup embeddings check is fatal: the child exits `3` with `ERROR:    Application startup failed. Exiting.` as its last line, the second start does the same, and the row is `UNAVAILABLE CHILD_FAILED` with that line (verified 2026-09-04). Against OpenAI (below) the child stays up.
- **Wrong key (OpenAI).** Verified on 2026-09-04 on macOS arm64: Hindsight's startup model verification fails `401`, logs `LLM connection verification failed … Server will start but LLM-dependent operations may fail`, and **starts anyway** — `/health` answers `200` with `{"status":"healthy","database":"connected",…}` in the usual cold time. So a bad key is never a loss and never makes the row `UNAVAILABLE`: the child stays `READY`, the retain that follows answers Hindsight `500` (`detail: "Fact extraction failed: … AuthenticationError: Error code: 401 …"`), and the operation answers `MEMORY_ERROR` per §4.3 with `marketrig memory status` still showing the child live. Only a child that exits, or one that never answers `/health` inside the deadline, is a loss.
- **Bearer is per start.** Two daemon starts write two different `HINDSIGHT_API_TENANT_API_KEY` values; neither appears in SQLite, the log root, or any event.

## 3. Provider settings (R4-2)

| Route | Body | Answers |
| --- | --- | --- |
| `GET /memory` | — | `{child: {state, executable_path, validated_at_ns, failure_code, failure_message, live, pid}, provider: {base_url, llm_model, embedding_model, api_key_present, embedding_locked_at_ns}}` |
| `PUT /memory/provider` | `{base_url, api_key?, llm_model, embedding_model}` | `200 {provider}`; `400 VALIDATION`; `409 EMBEDDING_MODEL_LOCKED`; `503 CREDENTIAL_STORE_UNAVAILABLE` |
| `GET /memory/provider/models` | — | `200 {models: [id…]}`; `409 MEMORY_UNCONFIGURED` (no base URL or key); `502 PROVIDER_UNREACHABLE {message}` |
| `POST /memory/discover` | `{"executable": path}` | `200 {child}` whichever state results |
| `POST /memory/retry` | — | `200 {child}` |

`PUT` validates `base_url` as an absolute `http`/`https` URL with no userinfo, query, or fragment, trailing slash stripped; models are non-empty strings ≤ 128 characters. The key goes to the credential store first (service `marketrig`, account `hindsight-provider`); only then does the row change and `MEMORY_CONFIGURED {what: "provider", base_url, llm_model, embedding_model}` append, in one unit. Under `MARKETRIG_TEST_DATA_ROOT` the store is `runtime/credentials.json` (0600, one JSON object keyed by account) in the relocated root. The models fetch is `GET <base_url>/models` with `Authorization: Bearer <key>`, 15 s timeout, no proxy inheritance, redirects disabled, answering the `data[].id` strings in the provider's order; any non-2xx, transport error, or unparseable body is `PROVIDER_UNREACHABLE` with the status or error text, and nothing is stored. `embedding_locked_at_ns` is stamped inside the unit that records the first successful retain (§4.2).

Scenarios:

- **Key never returns.** `PUT` then `GET /memory` and `marketrig --json memory status`: `api_key_present: true`, no key field anywhere.
- **Locked model.** After one retain, `PUT` with a different `embedding_model` → `409`; the same `PUT` with the same embedding model and a new LLM model → `200` and a `MEMORY_CONFIGURED` row.
- **Stale list never.** The provider stand-in answers `500` on `/models`: `502 PROVIDER_UNREACHABLE`, and a second call after it recovers answers the fresh list.

## 4. Banks and the memory operations (R4-3)

### 4.1 Bank derivation

`bank = "desk-" + desk UUID with hyphens removed` (`desk-` plus 32 lowercase hex characters). It is computed per request, stored nowhere, and appears in no response, log line, or event. Hindsight creates it on the first retain, recall, or reflect that names it (0.9.2's `_ensure_bank_exists`), so there is no provisioning step and no bank profile write.

### 4.2 Routes

| Route | Body | Hindsight call | Answers |
| --- | --- | --- | --- |
| `GET /desks/{d}/memory` | — | — | `GET /memory`'s object plus `desk_id` |
| `POST /desks/{d}/memory/retain` | `{content, context?, tags?}` | `POST /v1/default/banks/<bank>/memories {items: [{content, context, tags, metadata}], async: false}` | `200 {items_count}` |
| `POST /desks/{d}/memory/recall` | `{query, budget?, tags?}` | `POST …/memories/recall {query, budget, tags, tags_match: "any"}` | `200 {results: [{id, text, type, context, tags, metadata, occurred_start, mentioned_at}]}` |
| `POST /desks/{d}/memory/reflect` | `{query, budget?}` | `POST …/reflect {query, budget}` | `200 {text, based_on: [{id, text, type}]}` |

`metadata` on retain is `{"source": "INTERACTIVE" | "TRIGGER", "desk_id": …}` plus `trigger_id` and `firing_id` when the request carries R2's two attribution headers, validated against the firing row as the order routes do (`ATTRIBUTION_INVALID` otherwise). Values the agent supplies pass through verbatim; MarketRig adds no tag, no timestamp, no `document_id`; an omitted `context` or `tags` is left out of the child's body rather than sent null, and `tags_match` rides only beside a tag list. Reflect's citations arrive nested as the child's `based_on.memories` and the route answers that list itself. Each call carries `Authorization: Bearer <per-start bearer>`. The retain answer's unit appends `MEMORY_RETAINED {source, trigger_id, firing_id, items_count, tags}` and, on the first retain ever, stamps `embedding_locked_at_ns`; recall and reflect append `MEMORY_RECALLED {op, results}` (`results` = the count). Content is never stored by MarketRig and never appears in an event.

### 4.3 Limits, timeouts, and errors

| Field | Limit |
| --- | --- |
| `content` | 1 byte … 64 KiB |
| `context` | ≤ 4 KiB |
| `query` | 1 byte … 8 KiB |
| `tags` | ≤ 16 entries, each 1 … 64 characters |
| `budget` | `low \| mid \| high`, default `mid` |

Timeouts (daemon → child): retain 180 s, reflect 180 s, recall 60 s; a `STARTING` child consumes the operation's own budget while it waits for readiness. Error codes, all through root §4.3's envelope: `MEMORY_UNCONFIGURED` (`409`, no `AVAILABLE` child row or no provider base URL, key, or models), `MEMORY_UNAVAILABLE` (`503`, row `UNAVAILABLE`, or `STARTING` past the operation's timeout), `MEMORY_REJECTED` (`422`, a Hindsight 4xx, its `detail` string as the message), `MEMORY_TIMEOUT` (`504`), `MEMORY_ERROR` (`502`, a Hindsight 5xx or transport failure, status and first line as the message); each message the daemon lifts is carried inside the envelope's one English sentence. Hindsight's `detail` can quote the provider's own text, which quotes the key back (verified 2026-09-04: a wrong key returns `Incorrect API key provided: <the key>`), so every message the daemon lifts from the child — into a response, an event, the `failure_message`, or a log line — is redacted first: each occurrence of the stored key, and each delimited token beginning with the key's first eight characters (a provider also echoes it masked, `sk-smoke**********-123`), becomes `<redacted>`; a key shorter than twelve characters is matched whole only. `VALIDATION` covers the limits and `DESK_NOT_FOUND` the segment; a `CREATING` or `FAILED` desk answers `DESK_NOT_READY`.

### 4.4 CLI

`marketrig [--json] memory <status|retain|recall|reflect> <desk-name-or-id> …`, name-or-id resolved through the daemon's listing (root §13.2):

```text
memory status  <desk>
memory retain  <desk> (--content <text> | --file <path>) [--context <text>] [--tag <t>]…
memory recall  <desk> --query <text> [--budget low|mid|high] [--tag <t>]…
memory reflect <desk> --query <text> [--budget low|mid|high]
```

`--file` is read before the daemon is contacted and refused over 64 KiB with exit `2`. Human output: `status` prints `field: value` lines in the route's key order; `retain` prints `retained 1 item`; `recall` prints one tab-separated line per result — `<id>`, `<type>`, `<text>`, as every other listing is separated — and `no results` when empty; `reflect` prints the text, then `based on: <n> memories`. `--json` is the route's body verbatim. The CLI's total timeout for `retain` and `reflect` is 200 s and for `recall` 80 s, above the daemon's, so the daemon's code is what the caller sees. Attribution rides the same two headers every mutating command sends (R2), so a trigger's code retaining through `marketrig` is recorded `TRIGGER` with no extra flag.

Scenarios:

- **Two desks, one lesson.** Retain on `alpha` with `--tag lesson`; `recall alpha --query <its words>` lists it with `metadata.source = "INTERACTIVE"`; `recall beta` with the same query answers `no results`.
- **From a trigger.** A code-bearing firing whose script runs `marketrig memory retain` lands `MEMORY_RETAINED {source: "TRIGGER", trigger_id, firing_id}` and the memory's metadata carries both ids.
- **Stopped.** With the child `UNAVAILABLE`: `memory status` says so with the reason, `memory retain` exits `1` with `error: MEMORY_UNAVAILABLE: …`, and a trigger firing, a `submit_order`, and `session/activate` on the same daemon proceed unchanged.

## 5. The seeds (R4-4)

Creation's bootstrap (root §5.2) writes, in order and each only when absent: `AGENTS.md` (§5.1), `CLAUDE.md` (the R0 shim, still reconciled to exactly `@AGENTS.md\n`), `.agents/skills/desk-improvement/SKILL.md` (§5.2), and `.claude/skills` → `../.agents/skills` (relative symlink on macOS; on Windows a directory junction to the absolute `.agents/skills` path, created through the Win32 reparse-point API the already-pinned `windows` crate exposes, no `mklink` shell). Startup validation for a `READY` desk reconciles only the shim and the link: a missing link is recreated after `.agents/skills/` is created empty if absent; a link whose target is not the desk's `.agents/skills` is replaced; an ordinary file or directory at `.claude/skills` is left in place and the derived workspace status reason names it while the status stays `OK`. `<name>` below is the desk name.

### 5.1 `AGENTS.md`

```markdown
# <name>

You are the trader of the MarketRig desk `<name>`: one persistent identity that outlives any
conversation, on either Codex CLI or Claude Code. This file is your constitution. MarketRig seeded it
and will never rewrite it; it is yours to keep or change.

## The loop

Observe → Orient → Decide → Act → Evaluate → Learn → Observe. MarketRig owns Observe (market resources,
the paper book, durable history), Act (paper orders through the typed tools), time (triggers), and
continuity (records, prompts, memory transport). You own Orient, Decide, Evaluate, and Learn. Nothing is
decided for you: MarketRig never says what to buy, what evidence matters, or what a result means.

## Surfaces

- Market plane (MCP server `marketrig`): resources `marketrig://desk/<name>/quotes`, `book`, `positions`,
  `orders`, `instruments`; tools `submit_order` and `cancel_order`. Quotes are volatile: reread the
  resource whenever an exact current value matters instead of trusting a number already in context.
- Continuity plane (`marketrig` command): `history orders|fills|cycles`, `trigger`, `prompt`, `memory`,
  `desk`. `marketrig --json …` gives stable machine output.
- Prompts from MarketRig arrive as ordinary input beginning `MarketRig <KIND> <id>:` — `TRIGGER_RESULT`
  when a trigger you defined fired, `EVALUATION` when a position cycle closed, `DISCLOSURE` when a
  delivery failed while you were away. They inform; they do not instruct.

## The paper environment

Paper only, on a NautilusTrader sandbox with a cash account per desk: no shorting, no leverage,
`MARKET` and `LIMIT` orders good till cancelled, realized P&L in each instrument's own currency, fees
at each market's declared per-side rate. Not simulated: T+1 settlement, daily price limits, trading
halts, opening and closing auctions, and holiday calendars — a quote may read stale on a holiday. A
position cycle (open to flat in one instrument) is the unit of realized P&L and of evaluation.

## Evaluate and learn

Every closed cycle queues one `EVALUATION` prompt naming the cycle, the instrument, the net realized
P&L, and the orders and fills behind it. Realized P&L is the reward signal. Read the evidence you
choose (`marketrig history …`), judge the outcome, and decide whether anything was learned. When
something was: retain a desk-specific lesson (`marketrig memory retain`) and improve a reusable
procedure under `.agents/skills/`. When nothing was, say so and move on. The skill
`desk-improvement` describes one way to do this; it is yours to improve.

## Memory and skills

- `marketrig memory retain|recall|reflect` is this desk's experiential memory. It is private to this
  desk, it persists across sessions and runtimes, and only you write to it. Recall before deciding
  when the past may matter; retain what a future session would want to know.
- `.agents/skills/<skill>/SKILL.md` are your procedures, loaded by both runtimes from this one
  directory (`.claude/skills` is the same place). Create, refine, and delete them freely.
- Memory can be unavailable (`marketrig memory status`). Trading and triggers do not depend on it;
  keep working and retain later.

## Boundaries

Do not exit yourself to end work; the user does that. Trigger code runs with no session alive: keep
it self-contained. Secrets never belong in this workspace, in trigger code, or in memory.
```

### 5.2 `.agents/skills/desk-improvement/SKILL.md`

```markdown
---
name: desk-improvement
description: Evaluate a closed position cycle from its MarketRig EVALUATION prompt, decide whether a lesson exists, retain it in desk memory, and improve a desk skill. Use when an EVALUATION prompt arrives or when reviewing recent cycles.
---

# Desk improvement

MarketRig seeded this skill once. It is yours: rewrite it as your own practice improves.

1. Read the prompt: cycle id, instrument, net realized P&L with its currency, the client order ids
   and fill ids. Then fetch what you need — `marketrig --json history cycles <desk>`,
   `history orders`, `history fills` — and recall what memory already says:
   `marketrig memory recall <desk> --query "<instrument> <what you did>"`.
2. Judge the outcome. Realized P&L is the reward; compare the intent you had when you acted with what
   the fills and the price path show. Separate luck from process.
3. Decide whether anything was learned. Most cycles teach nothing new; say so and stop.
4. If a lesson exists, retain it once, in one or two sentences a future session can act on, tagged so
   it can be found: `marketrig memory retain <desk> --content "<lesson>" --tag lesson --tag <instrument>`.
5. If the lesson changes how you would act next time, improve the procedure: edit an existing skill
   under `.agents/skills/` or create one, keeping the frontmatter `name` and `description`.
6. Tell the user what you concluded in one paragraph, naming the cycle id.
```

Scenarios:

- **Created once.** A new desk has all three artifacts; deleting `AGENTS.md` then restarting the daemon leaves it missing (workspace status `UNAVAILABLE`, root §5.2) and recreates nothing; deleting `.claude/skills` then restarting recreates the link and nothing else.
- **Both paths, one file.** A file written at `.agents/skills/x/SKILL.md` is byte-identical when read through `.claude/skills/x/SKILL.md` on both platforms, and a file written through the link appears in `.agents/skills/`.
- **Pre-R4 desk.** A desk created on migration 4 keeps its placeholder `AGENTS.md`, gains an empty `.agents/skills/` and the link, and is never seeded with the improvement skill.

## 6. Durable schema (migration 5, R4-5)

```sql
CREATE TABLE memory_child (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  state TEXT NOT NULL CHECK (state IN ('UNCONFIGURED','AVAILABLE','UNAVAILABLE')),
  executable_path TEXT, validated_at_ns INTEGER, failure_code TEXT, failure_message TEXT,
  CHECK ((state = 'UNAVAILABLE') = (failure_code IS NOT NULL))
) STRICT;
INSERT INTO memory_child (id, state) VALUES (1, 'UNCONFIGURED');

CREATE TABLE memory_provider (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  base_url TEXT, llm_model TEXT, embedding_model TEXT,
  key_ref TEXT,                       -- opaque; 'marketrig/hindsight-provider' once a key is stored
  embedding_locked_at_ns INTEGER, updated_at_ns INTEGER NOT NULL
) STRICT;
INSERT INTO memory_provider (id, updated_at_ns) VALUES (1, 0);

-- operational_events rebuilt (migration-2 pattern) with
--   MEMORY_CONFIGURED, MEMORY_STARTED, MEMORY_LOST, MEMORY_UNAVAILABLE, MEMORY_RETAINED, MEMORY_RECALLED
```

No new recovery step: the child is a `children.json` record, reaped by the first step (per D73). The child's live state and pid are in memory only and read `NOT_STARTED` after every start.

## 7. Acceptance (R4-6)

### 7.1 The stand-in memory child

`memory-standin` answers `--help` with usage naming `HINDSIGHT_API_PORT`, then serves HTTP on `HINDSIGHT_API_HOST:HINDSIGHT_API_PORT`: `GET /health` (`200 {"status":"ok"}` once `health_after_ms` has elapsed since start); `POST /v1/default/banks/{bank}/memories` (stores each item's `content`, `context`, `tags`, `metadata` under the bank with a UUID and the receipt instant; answers `{success, bank_id, items_count, async: false}`); `POST …/memories/recall` (case-insensitive substring match of the query's words against `content`, filtered by `tags` when given, answering `results[]` with `id`, `text`, `type: "experience"`, `context`, `tags`, `metadata`, `occurred_start`, `mentioned_at`); `POST …/reflect` (`text` = the matching contents joined by newlines, `based_on.memories` = their ids and texts). Every route but `/health` requires `Authorization: Bearer <HINDSIGHT_API_TENANT_API_KEY>` and answers `401 {"detail":"Invalid API key"}` otherwise; an unknown route is `404`. Each start appends the bearer it was launched with to `<HOME>/bearers.txt` — `<data root>/hindsight/bearers.txt`, outside the database and the log root — because the daemon holds those strings in memory only and G33's secrets check has to name them. It prints one banner line to standard output, which is what the loss event's `output_tail_last_line` carries. Knobs from the `memory` object of `MARKETRIG_STANDIN_SCRIPT` (read at start; the file is the same one `runtime-standin` reads):

| Key | Default | Effect |
| --- | --- | --- |
| `health_after_ms` | `0` | delay before `/health` answers `200` (readiness deadline coverage) |
| `exit_after_ms` | — | exit `1` that long after start (loss and restart, G37) |
| `reject_retain` | `false` | retain answers `422 {"detail":"rejected by script"}` |
| `models` | `["stand-in-llm","stand-in-embedding"]` | what `--models` serves |

`memory-standin --models <port>` serves only `GET /v1/models` → `{"data":[{"id":…}…]}` on that port with no auth, and `500` while a `models_error` knob is `true`; it is the gate's provider stand-in and is started by the gate itself, not by the daemon.

### 7.2 Gate scenarios (continuing R3's chain)

- **G33 — configuration and secrets.** `GET /memory` is `UNCONFIGURED`; `discover` to the stand-in → `AVAILABLE`; `discover` to a nonexistent path → `NOT_FOUND`; `PUT /memory/provider` with the provider stand-in's URL and a key → `MEMORY_CONFIGURED`, `api_key_present`; `/models` answers the scripted list, then `PROVIDER_UNREACHABLE` under `models_error`, then the list again; the key and, after G34, the child's bearer appear nowhere in the database file, the log root, or the events listing.
- **G34 — two desks, one lesson.** `memory status` on `alpha` shows `live: NOT_STARTED`; `memory retain alpha` starts the child (`MEMORY_STARTED`), answers `retained 1 item`, and the row is now embedding-locked (`PUT` with another embedding model → `409`); `recall alpha` finds it with `source: "INTERACTIVE"`; `recall beta` answers `no results`; a code-bearing one-off on `gamma` whose script runs `marketrig memory retain` lands `MEMORY_RETAINED {source: "TRIGGER"}` with both ids.
- **G35 — the loop closes.** On the desk G28 activated (the stand-in runtime, the stand-in feed): a market buy then sell closes one cycle; the `EVALUATION` prompt is `DELIVERED` and the terminal shows `INPUT n: MarketRig EVALUATION <id>:` naming the cycle; the harness then performs the agent's two steps through public surfaces from the desk workspace — `marketrig memory retain <desk> --content "lesson for cycle <cycle_id>" --tag lesson` and a file `.agents/skills/desk-improvement/SKILL.md` rewritten to name the cycle id — and asserts `MEMORY_RETAINED` on that desk only.
- **G36 — the later session.** `exit`, then a code-less one-off activates the desk again (`SESSION_STARTED {mode: RESUME}`); `memory recall <desk> --query "cycle <cycle_id>"` returns the lesson; the skill file read through `.claude/skills/desk-improvement/SKILL.md` names the cycle; `recall` on the other desk of G34 returns nothing about it; after `switch` to the other runtime, the same file is readable through the other path.
- **G37 — Hindsight stopped.** With the script armed `exit_after_ms`: the stand-in exits mid-run → `MEMORY_LOST`; the next `retain` restarts it (`MEMORY_STARTED`) and succeeds; armed again and lost again with no readiness between → `MEMORY_UNAVAILABLE`, `memory status` reads `UNAVAILABLE CHILD_FAILED`, `retain` exits `1` with `MEMORY_UNAVAILABLE`, while a firing, a `submit_order`, and an activation on the same daemon succeed; `POST /memory/retry` then a `retain` → `READY` again. Finally a hard kill of the daemon with the child live: the next start's recovery event lists the reaped child, no `memory-standin` process survives on macOS, and `live` reads `NOT_STARTED`.

### 7.3 Experiment scenario

**E5** per cell, after E4 in the same invocation, skipped with evidence when `MARKETRIG_EXPERIMENT_HINDSIGHT` or any `MARKETRIG_EXPERIMENT_MEMORY_*` variable is unset: the harness discovers the real runtime, configures the memory child and the provider from those variables through REST, creates two desks, appends a user-owned acceptance addendum to the first desk's `AGENTS.md` naming the steps below, and attaches the terminal to the operator's console as E4 does. The operator asks the session to buy and then sell one unit of an instrument whose market is open. Recorded: the cycle, the `EVALUATION` prompt `DELIVERED`, then — up to 15 minutes each — `MEMORY_RETAINED` on that desk, a change under `.agents/skills/`, and after `exit` and a `CONTINUE` activation, `MEMORY_RECALLED` from the resumed session; mechanically, the second desk's `recall` through the CLI returns nothing about the first. Agent-owned aspects end `INCONCLUSIVE`; a retain the daemon's rows contradict, or a bank crossing desks, fails the cell.

## 8. Required checks

Module checks (`cargo test -p marketrigd`, fakes allowed):

1. `memory::discover` — explicit path, the `--help` marker, the three probe failure codes; step 6b's skip under the seam is gate evidence (G33 starts every daemon under `MARKETRIG_TEST_DATA_ROOT`), not a module assertion, because the guard is a process-global environment read.
2. `memory::child` — against an in-process fake `hindsight-api`: the composed environment (every variable of §2.2, `HOME` redirected, the bearer fresh per start), readiness by `/health`, the 120 s deadline, loss → `MEMORY_LOST` → one restart → `UNAVAILABLE CHILD_FAILED` → retry, provider change stops a live child, Quit stops it, `children.json` carries the record.
3. `memory::provider` — URL validation, key to the seam store and `api_key_present`, `PUT` without `api_key` keeps the key, `CREDENTIAL_STORE_UNAVAILABLE` writes nothing, `/models` live and never cached, `PROVIDER_UNREACHABLE`, the embedding lock stamped by the first retain and enforced after.
4. `memory::ops` — bank derivation, the three request mappings field for field, attribution metadata from the headers (`ATTRIBUTION_INVALID` on a bad firing), the limits, every error code in §4.3 including a child `500` whose `detail` quotes the key answering `MEMORY_ERROR` with the key redacted, `MEMORY_RETAINED` and `MEMORY_RECALLED` payloads, no content in any event.
5. `desk` — the three seeds created once and never rewritten, link reconciliation on both platforms (missing, repointed, ordinary directory left alone with the status reason), a pre-R4 desk gains only the tree and the link, `CLAUDE.md` still reconciled.
6. `store` — migration 5 on a migration-4 database keeps every row; the widened event vocabulary accepts the six kinds.
7. Secrets — the module checks' fake key and bearer are grepped out of the database file, the log root, and every event payload after the suite runs (the R0 pattern).
8. `marketrig memory` — grammar, `--file` limit at exit `2`, human output shapes, `--json` passthrough, timeouts above the daemon's.

Gate G33–G37 on macOS and Windows CI; E5 attended once per cell; static checks green. Marked as R4 exit in the implementing slice.
