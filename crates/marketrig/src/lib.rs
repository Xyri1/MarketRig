//! `marketrig` — the continuity-plane CLI (feature SPEC
//! `r0-workspace-desk-identity` §8). R0 ships exactly the `desk` group.
//!
//! The crate is also the shared daemon-access library: `marketrig-mcp` reuses
//! [`client::Endpoint`] for discovery, verification, and HTTP rather than
//! carrying a second copy (slice 002 §2).

pub mod client;

use clap::{Parser, Subcommand};
use client::{Endpoint, Fault};

/// Global flags precede the group (root SPEC §13.2).
#[derive(Parser)]
#[command(name = "marketrig", version, about = "MarketRig continuity-plane CLI")]
struct Cli {
    /// Emit the daemon's JSON resource verbatim.
    #[arg(long)]
    json: bool,
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
}

/// Exit codes (feature SPEC §8): 0 success, 1 daemon error, 2 usage (clap's
/// own), 3 no usable daemon.
pub fn run() -> i32 {
    let cli = Cli::parse();
    match dispatch(&cli.group) {
        Ok(body) => {
            emit(cli.json, &body);
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
    let Group::Desk { command } = group;
    let endpoint = Endpoint::discover()?;
    match command {
        DeskCommand::Create { name } => {
            endpoint.post("/desks", Some(serde_json::json!({ "name": name })))
        }
        DeskCommand::List => endpoint.get("/desks"),
        DeskCommand::Show { desk } => {
            let id = resolve(&endpoint, desk)?;
            endpoint.get(&format!("/desks/{id}"))
        }
        DeskCommand::Retry { desk } => {
            let id = resolve(&endpoint, desk)?;
            endpoint.post(&format!("/desks/{id}/retry"), None)
        }
    }
}

/// Name-or-id: a canonical lowercase UUID is used as is, any other token is a
/// name resolved through `GET /desks` and never any other way (§8).
fn resolve(endpoint: &Endpoint, desk: &str) -> Result<String, Fault> {
    if uuid::Uuid::try_parse(desk).is_ok_and(|u| u.hyphenated().to_string() == desk) {
        return Ok(desk.to_string());
    }
    let body = endpoint.get("/desks")?;
    let listing: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Fault::reported("INTERNAL", format!("Cannot read the desk listing: {e}.")))?;
    listing["desks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["name"].as_str() == Some(desk))
        .and_then(|d| d["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| Fault::reported("DESK_NOT_FOUND", format!("No desk is named {desk}.")))
}

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
    match value["desks"].as_array() {
        Some(desks) => {
            for desk in desks {
                println!(
                    "{}\t{}\t{}",
                    text(&desk["name"]),
                    text(&desk["state"]),
                    text(&desk["id"])
                );
            }
        }
        None => {
            for field in [
                "id",
                "name",
                "state",
                "workspace_path",
                "workspace_status",
                "workspace_status_reason",
                "created_at_ns",
                "ready_at_ns",
                "failure_code",
                "failure_message",
            ] {
                if !value[field].is_null() {
                    println!("{field}: {}", text(&value[field]));
                }
            }
        }
    }
}

fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
