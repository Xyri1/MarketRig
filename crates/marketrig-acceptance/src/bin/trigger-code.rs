//! `trigger-code` — the acceptance modes' trigger script runner.
//!
//! Contract: `sdd/features/r2-scheduled-triggers/SPEC.md` §10.1, per R2-8. Every
//! code-bearing trigger the gate or the experiment defines carries the same
//! snapshot shape — `argv = [<this binary>, "{script}"]` and a `source` of one
//! line — so the script the daemon writes is a plain instruction this binary
//! reads back, never a shell script and never a second language on the machine.
//!
//! The daemon hands the child its own environment plus the four `MARKETRIG_*`
//! identifiers (§4.2), which is how the `order` line finds the same daemon: the
//! `MARKETRIG_TEST_*` seam variables and the two attribution variables come
//! through untouched.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

fn main() {
    let script = match std::env::args().nth(1) {
        Some(path) => path,
        None => fail("usage: trigger-code <script-file>"),
    };
    let source = std::fs::read_to_string(&script)
        .unwrap_or_else(|e| fail(format!("cannot read the script {script}: {e}")));
    let line = source.lines().next().unwrap_or_default().trim().to_owned();
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["env"] => environment(),
        ["order", instrument_id, side, quantity] => order(instrument_id, side, quantity),
        ["exit", code] => {
            let code: i32 = number(code);
            eprintln!("trigger-code exiting {code}");
            std::process::exit(code);
        }
        ["sleep", secs] => std::thread::sleep(Duration::from_secs(number(secs))),
        ["flood", bytes] => flood(number(bytes)),
        _ => fail(format!("unrecognized script line {line:?}")),
    }
}

/// `env` — the four identifiers, the working directory, and the firing document
/// the daemon wrote to standard input, as one JSON object.
fn environment() {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .unwrap_or_else(|e| fail(format!("cannot read the firing document: {e}")));
    let document: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| fail(format!("the firing document is not JSON ({e}): {raw}")));
    let cwd = std::env::current_dir()
        .unwrap_or_else(|e| fail(format!("cannot read the working directory: {e}")));
    println!(
        "{}",
        json!({
            "MARKETRIG_DESK_ID": var("MARKETRIG_DESK_ID"),
            "MARKETRIG_DESK_NAME": var("MARKETRIG_DESK_NAME"),
            "MARKETRIG_TRIGGER_ID": var("MARKETRIG_TRIGGER_ID"),
            "MARKETRIG_FIRING_ID": var("MARKETRIG_FIRING_ID"),
            "cwd": cwd.display().to_string(),
            "document": document,
        })
    );
}

/// `order <instrument_id> <side> <quantity>` — the same call twice through the
/// real adapter, with the firing id as `action_id`, so the second is R1's
/// idempotent replay of the first (R1 feature SPEC §6). Each answer is one JSON
/// line on standard output.
fn order(instrument_id: &str, side: &str, quantity: &str) {
    let adapter = beside_me("marketrig-mcp");
    let desk = var("MARKETRIG_DESK_NAME");
    let action_id = var("MARKETRIG_FIRING_ID");
    let body = json!({
        "action_id": action_id, "instrument_id": instrument_id,
        "side": side, "type": "MARKET", "quantity": quantity, "price": null,
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(format!("cannot build a runtime: {e}")));
    let answers = runtime.block_on(async {
        // The environment is inherited whole: the daemon's data-root seam and
        // the two attribution variables are what make this order a TRIGGER one.
        let command = tokio::process::Command::new(&adapter).configure(|command| {
            command.arg("--desk").arg(&desk);
        });
        let transport = TokioChildProcess::new(command)
            .unwrap_or_else(|e| fail(format!("cannot spawn {}: {e}", adapter.display())));
        let service = ()
            .serve(transport)
            .await
            .unwrap_or_else(|e| fail(format!("cannot initialize the MCP session: {e}")));

        let mut answers = Vec::new();
        for attempt in 1..=2 {
            let arguments = body
                .as_object()
                .expect("the order body is an object")
                .clone();
            let result = service
                .call_tool(CallToolRequestParams::new("submit_order").with_arguments(arguments))
                .await
                .unwrap_or_else(|e| fail(format!("call {attempt} was not routed: {e}")));
            let text = result
                .content
                .first()
                .and_then(|block| block.as_text())
                .map(|text| text.text.clone())
                .unwrap_or_else(|| fail(format!("call {attempt} answered no text content")));
            if result.is_error == Some(true) {
                fail(format!("call {attempt} answered a tool error: {text}"));
            }
            answers.push(text);
        }
        let _ = service.cancel().await;
        answers
    });
    for answer in answers {
        println!("{answer}");
    }
}

/// `flood <bytes>` — the daemon's stream cap is what this exercises, so a write
/// that fails because the daemon already terminated the group is the expected
/// end, not an error (§4.3).
fn flood(bytes: usize) {
    let chunk = [b'x'; 64 * 1024];
    let mut out = std::io::stdout().lock();
    let mut left = bytes;
    while left > 0 {
        let take = left.min(chunk.len());
        if out.write_all(&chunk[..take]).is_err() {
            return;
        }
        left -= take;
    }
    let _ = out.flush();
}

/// A sibling of this executable, with the platform's suffix — the acceptance
/// binaries are built together and land in one directory (§10.1).
fn beside_me(name: &str) -> PathBuf {
    let me = std::env::current_exe()
        .unwrap_or_else(|e| fail(format!("cannot locate this executable: {e}")));
    me.parent()
        .unwrap_or_else(|| fail("this executable has no directory"))
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fail(format!("{name} is not set")))
}

fn number<T: std::str::FromStr>(text: &str) -> T {
    text.parse()
        .unwrap_or_else(|_| fail(format!("{text:?} is not a number")))
}

/// Every failure is one line on standard error and exit 1, which is what the
/// scenario asserting it reads back out of the `executions` row.
fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("trigger-code: {message}");
    std::process::exit(1);
}
