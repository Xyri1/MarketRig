//! `marketrig` — the continuity-plane CLI (feature SPEC
//! `r0-workspace-desk-identity` §8, extended with the `history` group by
//! `r1-equity-paper-trading` §9 and the `trigger` and `prompt` groups by
//! `r2-scheduled-triggers` §9).
//!
//! The crate is also the shared daemon-access library: `marketrig-mcp` reuses
//! [`client::Endpoint`] for discovery, verification, and HTTP rather than
//! carrying a second copy (slice 002 §2).

pub mod client;

use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use client::{Endpoint, Fault};
use serde_json::{Map, Value, json};

/// Global flags precede the group (root SPEC §13.2).
#[derive(Parser)]
#[command(name = "marketrig", version, about = "MarketRig continuity-plane CLI")]
struct Cli {
    /// Emit the daemon's JSON resource verbatim.
    #[arg(long)]
    json: bool,
    /// Desk UUID, for the commands the daemon itself launches (R3 §5.2).
    #[arg(long, global = true)]
    desk: Option<String>,
    #[command(subcommand)]
    group: Group,
}

#[derive(Subcommand)]
enum Group {
    /// Desk identities.
    Desk {
        #[command(subcommand)]
        command: DeskCommand,
    },
    /// Durable trading records, newest first.
    // R1 feature SPEC §9; live positions and open orders are the MCP plane's.
    History {
        record: HistoryRecord,
        /// Desk name or UUID.
        desk: String,
    },
    /// Scheduled triggers and their firings.
    // R2 feature SPEC §9. Boxed because `create` alone carries a dozen
    // options and every other group would pay for its size.
    Trigger {
        #[command(subcommand)]
        command: Box<TriggerCommand>,
    },
    /// The desk's daemon-prompt queue, newest first.
    Prompt {
        #[command(subcommand)]
        command: PromptCommand,
    },
    /// Session ingress the runtime itself invokes (R3 feature SPEC §5.2).
    // Never a session lifecycle control: those are REST and desktop actions
    // (root §13.2, per D69).
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// The desk's own experiential memory.
    // R4 feature SPEC §4.4. The desk writes and reads it; MarketRig only
    // carries it (per D17).
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Show the memory child and provider behind this desk.
    Status {
        /// Desk name or UUID.
        desk: String,
    },
    /// Retain one memory in this desk's bank.
    #[command(group = clap::ArgGroup::new("material").required(true).args(["content", "file"]))]
    Retain {
        /// Desk name or UUID.
        desk: String,
        /// The memory itself.
        #[arg(long, value_name = "TEXT")]
        content: Option<String>,
        /// Read the memory from this file instead.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Extra material carried beside the content.
        #[arg(long, value_name = "TEXT")]
        context: Option<String>,
        /// One tag; repeat to build the whole list.
        #[arg(long = "tag", value_name = "TAG")]
        tag: Vec<String>,
    },
    /// Recall this desk's memories matching a query.
    Recall {
        /// Desk name or UUID.
        desk: String,
        #[arg(long, value_name = "TEXT")]
        query: String,
        #[arg(long)]
        budget: Option<Budget>,
        /// One tag the memory must carry; repeat to widen the filter.
        #[arg(long = "tag", value_name = "TAG")]
        tag: Vec<String>,
    },
    /// Reflect over this desk's memories.
    Reflect {
        /// Desk name or UUID.
        desk: String,
        #[arg(long, value_name = "TEXT")]
        query: String,
        #[arg(long)]
        budget: Option<Budget>,
    },
}

/// How much the child may spend on one recall or reflection (R4 §4.3).
#[derive(Clone, Copy, ValueEnum)]
enum Budget {
    Low,
    Mid,
    High,
}

impl Budget {
    fn as_str(self) -> &'static str {
        match self {
            Budget::Low => "low",
            Budget::Mid => "mid",
            Budget::High => "high",
        }
    }
}

/// `--file`'s own bound, checked before the daemon is contacted so an oversize
/// file is a usage error and never a request (R4 §4.4).
const CONTENT_LIMIT: usize = 64 * 1024;

#[derive(Subcommand)]
enum SessionCommand {
    /// Post one Claude Code hook object, read from standard input, to the
    /// daemon. Always exits 0 and prints nothing.
    Hook,
}

/// The hook body cap (R3 feature SPEC §5.2); anything larger is dropped.
const HOOK_LIMIT: usize = 64 * 1024;

/// `marketrig --desk <id> session hook`: read standard input to EOF, post it
/// unchanged, and exit 0 whatever happens — a hook must never fail the turn
/// that ran it (R3 feature SPEC §5.2).
fn session_hook(desk: Option<&str>) -> i32 {
    use std::io::Read as _;
    let Some(desk) = desk else {
        usage("session hook needs --desk <desk-id>");
    };
    let mut body = Vec::new();
    // One byte past the cap is enough to know it is over.
    if std::io::stdin()
        .take(HOOK_LIMIT as u64 + 1)
        .read_to_end(&mut body)
        .is_err()
        || body.len() > HOOK_LIMIT
    {
        return 0;
    }
    if let Ok(body) = String::from_utf8(body)
        && let Ok(endpoint) = Endpoint::discover()
    {
        let _ = endpoint.post_json_text(&format!("/desks/{desk}/session/hook"), body);
    }
    0
}

#[derive(Clone, ValueEnum)]
enum HistoryRecord {
    Orders,
    Fills,
    Cycles,
}

#[derive(Subcommand)]
enum DeskCommand {
    /// Create a desk and its workspace.
    Create { name: String },
    /// List every desk in creation order.
    List,
    /// Show one desk by name or UUID.
    Show { desk: String },
    /// Retry a failed desk creation.
    Retry { desk: String },
    /// This desk's operational events, newest first (R5 feature SPEC §4.3).
    Events {
        /// Desk name or UUID.
        desk: String,
        /// How many rows, 1 to 500; 100 by omission.
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
}

#[derive(Subcommand)]
enum TriggerCommand {
    /// Define a trigger. Exactly one schedule shape is required.
    #[command(group = clap::ArgGroup::new("schedule").required(true).args(["at", "rrule"]))]
    Create {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name, unique among the desk's live triggers.
        #[arg(long)]
        name: String,
        /// What the trigger is for, snapshotted into every firing.
        #[arg(long)]
        brief: String,
        /// Extra material carried beside the brief.
        #[arg(long)]
        context: Option<String>,
        #[command(flatten)]
        schedule: ScheduleArgs,
        #[command(flatten)]
        code: CodeArgs,
    },
    /// List the desk's live triggers in creation order.
    List {
        /// Desk name or UUID.
        desk: String,
    },
    /// Show one trigger by name or UUID, deleted ones included.
    Show {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name or UUID.
        trigger: String,
    },
    /// Change a trigger; at least one flag is required.
    Update {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name or UUID.
        trigger: String,
        /// Replace the brief.
        #[arg(long)]
        brief: Option<String>,
        /// Replace the context.
        #[arg(long)]
        context: Option<String>,
        /// Clear the context.
        #[arg(long, conflicts_with = "context")]
        no_context: bool,
        #[command(flatten)]
        schedule: ScheduleArgs,
        #[command(flatten)]
        code: CodeArgs,
        /// Detach the code snapshot.
        #[arg(long, conflicts_with = "code")]
        no_code: bool,
    },
    /// Enable a trigger and project its next occurrence.
    Enable {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name or UUID.
        trigger: String,
    },
    /// Disable a trigger; it becomes never due.
    Disable {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name or UUID.
        trigger: String,
    },
    /// Delete a trigger, keeping its firings readable.
    Delete {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name or UUID.
        trigger: String,
    },
    /// List one trigger's firings, newest first.
    Firings {
        /// Desk name or UUID.
        desk: String,
        /// Trigger name or UUID.
        trigger: String,
    },
    /// Show one firing with its captured streams.
    Firing {
        /// Desk name or UUID.
        desk: String,
        /// Firing UUID.
        firing: String,
    },
}

#[derive(Subcommand)]
enum PromptCommand {
    /// List the desk's prompts, newest first, without payloads.
    List {
        /// Desk name or UUID.
        desk: String,
    },
    /// Show one prompt with its payload.
    Show {
        /// Desk name or UUID.
        desk: String,
        /// Prompt UUID.
        prompt: String,
    },
}

/// One schedule shape or the other, never both and never half of the recurring
/// trio (R2 feature SPEC §2). Values pass through untouched; the daemon
/// validates them.
#[derive(Args)]
struct ScheduleArgs {
    /// One-off instant, RFC 3339 with an offset.
    #[arg(long, value_name = "RFC3339", conflicts_with_all = ["rrule", "dtstart", "tz"])]
    at: Option<String>,
    /// Recurrence rule, the text after `RRULE:`.
    #[arg(long, value_name = "RULE", requires_all = ["dtstart", "tz"])]
    rrule: Option<String>,
    /// Recurrence anchor as naive local wall clock, `YYYY-MM-DDTHH:MM:SS`.
    #[arg(long, value_name = "LOCAL", requires_all = ["rrule", "tz"])]
    dtstart: Option<String>,
    /// IANA time zone the wall clock is read in.
    #[arg(long, value_name = "IANA", requires_all = ["rrule", "dtstart"])]
    tz: Option<String>,
}

/// The code snapshot, read from a file because the CLI cannot carry a script
/// any other way (R2 feature SPEC §4.1, §9).
#[derive(Args)]
struct CodeArgs {
    /// Read the trigger's code from this file.
    #[arg(long, value_name = "FILE")]
    code: Option<PathBuf>,
    /// Script suffix; defaults to the file's own extension.
    #[arg(long, value_name = "SUFFIX", requires = "code")]
    suffix: Option<String>,
    /// One argv entry; repeat to build the whole vector. Defaults to `{script}`.
    #[arg(long = "arg", value_name = "ARG", requires = "code")]
    arg: Vec<String>,
    /// Seconds the child may run; the daemon's default by omission.
    #[arg(long, value_name = "SECS", requires = "code")]
    timeout: Option<u64>,
}

/// Exit codes (feature SPEC §8): 0 success, 1 daemon error, 2 usage (clap's
/// own), 3 no usable daemon.
pub fn run() -> i32 {
    let cli = Cli::parse();
    if let Group::Session {
        command: SessionCommand::Hook,
    } = cli.group
    {
        return session_hook(cli.desk.as_deref());
    }
    match dispatch(&cli.group) {
        Ok(body) => {
            match &cli.group {
                // The memory answers are nested, sentence-shaped, or empty —
                // none of which the generic renderer covers (R4 §4.4).
                Group::Memory { command } if !cli.json => emit_memory(command, &body),
                _ => emit(cli.json, &body),
            }
            0
        }
        Err(fault) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "code": fault.code, "message": fault.message })
                );
            } else {
                eprintln!("error: {}: {}", fault.code, fault.message);
            }
            fault.exit
        }
    }
}

fn dispatch(group: &Group) -> Result<String, Fault> {
    // Built before discovery: an unreadable or non-UTF-8 `--code` file and an
    // empty `update` are usage errors whether or not a daemon is up (§9).
    let body = match group {
        Group::Trigger { command } => trigger_body(command),
        Group::Memory { command } => memory_body(command),
        _ => None,
    };
    let endpoint = Endpoint::discover()?;
    match group {
        Group::Desk { command } => match command {
            DeskCommand::Create { name } => {
                endpoint.post("/desks", Some(serde_json::json!({ "name": name })))
            }
            DeskCommand::List => endpoint.get("/desks"),
            DeskCommand::Show { desk } => {
                let id = resolve(&endpoint, "/desks", "desk", desk)?;
                endpoint.get(&format!("/desks/{id}"))
            }
            DeskCommand::Retry { desk } => {
                let id = resolve(&endpoint, "/desks", "desk", desk)?;
                endpoint.post(&format!("/desks/{id}/retry"), None)
            }
            DeskCommand::Events { desk, limit } => {
                let id = resolve(&endpoint, "/desks", "desk", desk)?;
                let limit = limit.map(|n| format!("&limit={n}")).unwrap_or_default();
                endpoint.get(&format!("/events?desk_id={id}{limit}"))
            }
        },
        Group::History { record, desk } => {
            let id = resolve(&endpoint, "/desks", "desk", desk)?;
            let segment = match record {
                HistoryRecord::Orders => "orders",
                HistoryRecord::Fills => "fills",
                HistoryRecord::Cycles => "cycles",
            };
            endpoint.get(&format!("/desks/{id}/history/{segment}"))
        }
        Group::Trigger { command } => {
            let command = command.as_ref();
            let desk = match command {
                TriggerCommand::Create { desk, .. }
                | TriggerCommand::List { desk }
                | TriggerCommand::Show { desk, .. }
                | TriggerCommand::Update { desk, .. }
                | TriggerCommand::Enable { desk, .. }
                | TriggerCommand::Disable { desk, .. }
                | TriggerCommand::Delete { desk, .. }
                | TriggerCommand::Firings { desk, .. }
                | TriggerCommand::Firing { desk, .. } => desk,
            };
            let desk = resolve(&endpoint, "/desks", "desk", desk)?;
            let triggers = format!("/desks/{desk}/triggers");
            match command {
                TriggerCommand::Create { .. } => endpoint.post(&triggers, body),
                TriggerCommand::List { .. } => endpoint.get(&triggers),
                TriggerCommand::Show { trigger, .. } => {
                    let id = resolve(&endpoint, &triggers, "trigger", trigger)?;
                    endpoint.get(&format!("{triggers}/{id}"))
                }
                TriggerCommand::Update { trigger, .. }
                | TriggerCommand::Enable { trigger, .. }
                | TriggerCommand::Disable { trigger, .. } => {
                    let id = resolve(&endpoint, &triggers, "trigger", trigger)?;
                    endpoint.patch(&format!("{triggers}/{id}"), body)
                }
                TriggerCommand::Delete { trigger, .. } => {
                    let id = resolve(&endpoint, &triggers, "trigger", trigger)?;
                    endpoint.delete(&format!("{triggers}/{id}"))
                }
                TriggerCommand::Firings { trigger, .. } => {
                    let id = resolve(&endpoint, &triggers, "trigger", trigger)?;
                    endpoint.get(&format!("{triggers}/{id}/firings"))
                }
                TriggerCommand::Firing { firing, .. } => {
                    let firing = id_only("firing", firing)?;
                    endpoint.get(&format!("/desks/{desk}/firings/{firing}"))
                }
            }
        }
        Group::Session { .. } => unreachable!("handled before discovery"),
        Group::Memory { command } => {
            let (MemoryCommand::Status { desk }
            | MemoryCommand::Retain { desk, .. }
            | MemoryCommand::Recall { desk, .. }
            | MemoryCommand::Reflect { desk, .. }) = command;
            let desk = resolve(&endpoint, "/desks", "desk", desk)?;
            // Above the daemon's own 180 s and 60 s (R4 §4.3), so what the
            // caller sees is the daemon's code and never a client giving up.
            let (route, timeout) = match command {
                MemoryCommand::Status { .. } => {
                    return endpoint.get(&format!("/desks/{desk}/memory"));
                }
                MemoryCommand::Retain { .. } => ("retain", 200),
                MemoryCommand::Recall { .. } => ("recall", 80),
                MemoryCommand::Reflect { .. } => ("reflect", 200),
            };
            endpoint.post_within(
                &format!("/desks/{desk}/memory/{route}"),
                body,
                std::time::Duration::from_secs(timeout),
            )
        }
        Group::Prompt { command } => {
            let (PromptCommand::List { desk } | PromptCommand::Show { desk, .. }) = command;
            let desk = resolve(&endpoint, "/desks", "desk", desk)?;
            match command {
                PromptCommand::List { .. } => endpoint.get(&format!("/desks/{desk}/prompts")),
                PromptCommand::Show { prompt, .. } => {
                    let prompt = id_only("prompt", prompt)?;
                    endpoint.get(&format!("/desks/{desk}/prompts/{prompt}"))
                }
            }
        }
    }
}

/// The request body of the mutating `trigger` commands (R2 feature SPEC §8).
/// Every value passes through untouched; the daemon validates it.
fn trigger_body(command: &TriggerCommand) -> Option<Value> {
    let mut body = Map::new();
    match command {
        TriggerCommand::Create {
            name,
            brief,
            context,
            schedule,
            code,
            ..
        } => {
            body.insert("name".to_string(), json!(name));
            body.insert("brief".to_string(), json!(brief));
            if let Some(context) = context {
                body.insert("context".to_string(), json!(context));
            }
            if let Some(schedule) = schedule_json(schedule) {
                body.insert("schedule".to_string(), schedule);
            }
            if let Some(code) = code_json(code) {
                body.insert("code".to_string(), code);
            }
        }
        TriggerCommand::Update {
            brief,
            context,
            no_context,
            schedule,
            code,
            no_code,
            ..
        } => {
            if let Some(brief) = brief {
                body.insert("brief".to_string(), json!(brief));
            }
            if let Some(context) = context {
                body.insert("context".to_string(), json!(context));
            }
            if *no_context {
                body.insert("context".to_string(), Value::Null);
            }
            if let Some(schedule) = schedule_json(schedule) {
                body.insert("schedule".to_string(), schedule);
            }
            if let Some(code) = code_json(code) {
                body.insert("code".to_string(), code);
            }
            if *no_code {
                body.insert("code".to_string(), Value::Null);
            }
            if body.is_empty() {
                usage(
                    "trigger update needs at least one of --brief, --context, --no-context, \
                     a schedule, --code, or --no-code",
                );
            }
        }
        TriggerCommand::Enable { .. } => {
            body.insert("enabled".to_string(), json!(true));
        }
        TriggerCommand::Disable { .. } => {
            body.insert("enabled".to_string(), json!(false));
        }
        _ => return None,
    }
    Some(Value::Object(body))
}

/// The request body of the three mutating `memory` commands (R4 §4.2). Values
/// pass through untouched; the daemon validates them. `--file` is read here,
/// before discovery, so an unreadable or oversize one is a usage error whether
/// or not a daemon is up (§4.4).
fn memory_body(command: &MemoryCommand) -> Option<Value> {
    let mut body = Map::new();
    match command {
        MemoryCommand::Status { .. } => return None,
        MemoryCommand::Retain {
            content,
            file,
            context,
            tag,
            ..
        } => {
            body.insert("content".to_string(), json!(content_of(content, file)));
            if let Some(context) = context {
                body.insert("context".to_string(), json!(context));
            }
            if !tag.is_empty() {
                body.insert("tags".to_string(), json!(tag));
            }
        }
        MemoryCommand::Recall {
            query, budget, tag, ..
        } => {
            body.insert("query".to_string(), json!(query));
            if let Some(budget) = budget {
                body.insert("budget".to_string(), json!(budget.as_str()));
            }
            if !tag.is_empty() {
                body.insert("tags".to_string(), json!(tag));
            }
        }
        MemoryCommand::Reflect { query, budget, .. } => {
            body.insert("query".to_string(), json!(query));
            if let Some(budget) = budget {
                body.insert("budget".to_string(), json!(budget.as_str()));
            }
        }
    }
    Some(Value::Object(body))
}

/// `--content` verbatim, or `--file`'s text refused over 64 KiB (§4.4). Clap
/// has already required exactly one of the two.
fn content_of(content: &Option<String>, file: &Option<PathBuf>) -> String {
    if let Some(content) = content {
        return content.clone();
    }
    let path = file.as_ref().expect("clap requires --content or --file");
    let source = std::fs::read(path).unwrap_or_else(|e| {
        usage(format!(
            "cannot read the content file {}: {e}",
            path.display()
        ))
    });
    if source.len() > CONTENT_LIMIT {
        usage(format!(
            "the content file {} is {} bytes; the limit is {CONTENT_LIMIT}",
            path.display(),
            source.len()
        ));
    }
    String::from_utf8(source)
        .unwrap_or_else(|_| usage(format!("the content file {} is not UTF-8", path.display())))
}

/// `--at` or the recurring trio; clap has already rejected any other shape.
fn schedule_json(schedule: &ScheduleArgs) -> Option<Value> {
    if let Some(at) = &schedule.at {
        return Some(json!({ "at": at }));
    }
    let rrule = schedule.rrule.as_ref()?;
    Some(json!({
        "rrule": rrule,
        "dtstart": schedule.dtstart,
        "tz": schedule.tz,
    }))
}

/// The §4.1 snapshot read from `--code`'s file. The suffix defaults to the
/// file's own extension, `argv` to `{script}` alone, and an omitted timeout
/// leaves the daemon's default in place.
fn code_json(code: &CodeArgs) -> Option<Value> {
    let path = code.code.as_ref()?;
    let source = std::fs::read(path)
        .unwrap_or_else(|e| usage(format!("cannot read the code file {}: {e}", path.display())));
    let source = String::from_utf8(source)
        .unwrap_or_else(|_| usage(format!("the code file {} is not UTF-8", path.display())));
    let suffix = code.suffix.clone().unwrap_or_else(|| {
        path.extension()
            .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()))
    });
    let argv = if code.arg.is_empty() {
        vec!["{script}".to_string()]
    } else {
        code.arg.clone()
    };
    let mut snapshot = Map::new();
    snapshot.insert("source".to_string(), json!(source));
    snapshot.insert("suffix".to_string(), json!(suffix));
    snapshot.insert("argv".to_string(), json!(argv));
    if let Some(timeout) = code.timeout {
        snapshot.insert("timeout_secs".to_string(), json!(timeout));
    }
    Some(Value::Object(snapshot))
}

fn is_canonical_uuid(token: &str) -> bool {
    uuid::Uuid::try_parse(token).is_ok_and(|u| u.hyphenated().to_string() == token)
}

/// An id-only argument (firings and prompts have no name): a canonical
/// lowercase UUID reaches the route as is; anything else is the client-side
/// not-found, never a path segment.
fn id_only(noun: &str, token: &str) -> Result<String, Fault> {
    if is_canonical_uuid(token) {
        Ok(token.to_string())
    } else {
        Err(Fault::reported(
            format!("{}_NOT_FOUND", noun.to_uppercase()),
            format!("No {noun} has the id {token}."),
        ))
    }
}

/// A usage error the parser cannot state itself, surfaced the way clap
/// surfaces its own and exiting `2` (feature SPEC §8).
fn usage(message: impl std::fmt::Display) -> ! {
    Cli::command()
        .error(clap::error::ErrorKind::InvalidValue, message)
        .exit()
}

/// Name-or-id: a canonical lowercase UUID is used as is, any other token is a
/// name resolved through the daemon's own listing at `route` and never any
/// other way (R0 §8, R2 §9). `noun` names both the listing key (`{noun}s`) and
/// the `{NOUN}_NOT_FOUND` code the CLI reports client-side.
fn resolve(endpoint: &Endpoint, route: &str, noun: &str, token: &str) -> Result<String, Fault> {
    if is_canonical_uuid(token) {
        return Ok(token.to_string());
    }
    let body = endpoint.get(route)?;
    let listing: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        Fault::reported("INTERNAL", format!("Cannot read the {noun} listing: {e}."))
    })?;
    listing[format!("{noun}s")]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["name"].as_str() == Some(token))
        .and_then(|row| row["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            Fault::reported(
                format!("{}_NOT_FOUND", noun.to_uppercase()),
                format!("No {noun} is named {token}."),
            )
        })
}

/// The list bodies the CLI renders as tab-separated rows, in the daemon's own
/// order — newest first for the history, firing, and prompt routes (R1 feature
/// SPEC §7, §9; R2 feature SPEC §8, §9).
///
/// `a|b` prints the first field present: a history order carries the sandbox's
/// `status`, or its latest event `kind` before a status exists. `a.b` reads one
/// level down. The daemon owns every shape, so a missing or unknown field
/// prints blank and never panics.
const LISTS: [(&str, &[&str]); 8] = [
    ("desks", &["name", "state", "id"]),
    // The payload is an object, which `text` renders as one-line JSON (R5 §4.3).
    ("events", &["occurred_at_ns", "kind", "payload"]),
    (
        "orders",
        &[
            "client_order_id",
            "instrument_id",
            "side",
            "type",
            "quantity",
            "price",
            "status|kind",
        ],
    ),
    (
        "fills",
        &[
            "occurred_at_ns",
            "instrument_id",
            "side",
            "quantity",
            "price",
            "commission",
            "currency",
        ],
    ),
    (
        "cycles",
        &["closed_at_ns", "instrument_id", "realized_pnl", "currency"],
    ),
    (
        "triggers",
        &["name", "recurrence", "enabled", "next_occurrence_ns", "id"],
    ),
    (
        "firings",
        &["id", "occurrence_ns", "accepted_at_ns", "execution.outcome"],
    ),
    (
        "prompts",
        &["id", "kind", "state", "failure_code", "created_at_ns"],
    ),
];

/// Single resources print `field: value` in the daemon's own key order. One
/// ordered union covers every resource the CLI reads — a field the resource at
/// hand does not carry is simply skipped — and anything the daemon adds that
/// this list does not name follows, so nothing is silently dropped.
const FIELDS: [&str; 30] = [
    "id",
    "desk_id",
    "name",
    "source",
    "recurrence",
    "kind",
    "state",
    "trigger_id",
    "occurrence_ns",
    "accepted_at_ns",
    "trigger_revision",
    "brief",
    "context",
    "code_snapshot_id",
    "execution",
    "schedule",
    "enabled",
    "revision",
    "next_occurrence_ns",
    "code",
    "workspace_path",
    "workspace_status",
    "workspace_status_reason",
    "created_at_ns",
    "updated_at_ns",
    "deleted_at_ns",
    "payload",
    "ready_at_ns",
    "failure_code",
    "failure_message",
];

/// `--json` passes the daemon's body through untouched; human output is plain
/// UTF-8 text carrying the same facts.
fn emit(json: bool, body: &str) {
    if json {
        println!("{}", body.trim_end());
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        println!("{}", body.trim_end());
        return;
    };
    if let Some((rows, columns)) = LISTS
        .iter()
        .find_map(|(key, columns)| Some((value[key].as_array()?, columns)))
    {
        for row in rows {
            let cells: Vec<String> = columns.iter().map(|column| cell(row, column)).collect();
            println!("{}", cells.join("\t"));
        }
        return;
    }
    for field in FIELDS {
        if !value[field].is_null() {
            println!("{field}: {}", text(&value[field]));
        }
    }
    for (field, entry) in value.as_object().into_iter().flatten() {
        if !entry.is_null() && !FIELDS.contains(&field.as_str()) {
            println!("{field}: {}", text(entry));
        }
    }
}

/// The desk-scoped status in the route's own key order — `serde_json` sorts an
/// object's keys on the way in, so the order lives here (R4 §4.2, §4.4). The
/// child's fields, then the provider's, then the desk; a field the answer omits
/// is skipped, exactly as [`emit`] skips one.
const MEMORY_STATUS: [&str; 13] = [
    "state",
    "executable_path",
    "validated_at_ns",
    "failure_code",
    "failure_message",
    "live",
    "pid",
    "base_url",
    "llm_model",
    "embedding_model",
    "api_key_present",
    "embedding_locked_at_ns",
    "desk_id",
];

/// The `memory` group's human output (R4 §4.4).
fn emit_memory(command: &MemoryCommand, body: &str) {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        println!("{}", body.trim_end());
        return;
    };
    match command {
        MemoryCommand::Status { .. } => {
            for field in MEMORY_STATUS {
                if let Some(entry) = [&value, &value["child"], &value["provider"]]
                    .into_iter()
                    .map(|part| &part[field])
                    .find(|entry| !entry.is_null())
                {
                    println!("{field}: {}", text(entry));
                }
            }
        }
        MemoryCommand::Retain { .. } => {
            let count = value["items_count"].as_i64().unwrap_or_default();
            println!("retained {count} item{}", if count == 1 { "" } else { "s" });
        }
        MemoryCommand::Recall { .. } => {
            let results = value["results"].as_array().cloned().unwrap_or_default();
            if results.is_empty() {
                println!("no results");
            }
            for result in &results {
                println!(
                    "{}",
                    ["id", "type", "text"]
                        .map(|field| text(&result[field]))
                        .join("\t")
                );
            }
        }
        MemoryCommand::Reflect { .. } => {
            println!("{}", text(&value["text"]));
            println!(
                "based on: {} memories",
                value["based_on"].as_array().map_or(0, Vec::len)
            );
        }
    }
}

fn cell(row: &serde_json::Value, column: &str) -> String {
    column
        .split('|')
        .map(|field| field.split('.').fold(row, |value, key| &value[key]))
        .find(|value| !value.is_null())
        .map_or(String::new(), text)
}

fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
