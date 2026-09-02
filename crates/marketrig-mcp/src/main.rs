//! `marketrig-mcp` — the market plane's stdio MCP adapter (feature SPEC
//! `r1-equity-paper-trading` §8, per D4 and R1-7).
//!
//! One process serves one desk: five concrete resources proxying the daemon's
//! §7 read routes and two tools proxying its order routes. The adapter holds no
//! cache, no state, and no judgment — every call reaches the daemon at call
//! time and the daemon decides.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use marketrig::client::{Endpoint, Fault};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData, JsonObject,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt};

#[derive(Parser)]
#[command(
    name = "marketrig-mcp",
    version,
    about = "MarketRig market-plane MCP adapter"
)]
struct Cli {
    /// The desk this adapter serves, by kebab name or UUID.
    #[arg(long)]
    desk: String,

    /// Serve Claude Code's development channel instead of the market plane
    /// (R3 feature SPEC §5.3): no tools, no resources — every frame the daemon
    /// writes for this desk becomes one `notifications/claude/channel`.
    #[arg(long)]
    channel: bool,
}

/// The five resources of §8: URI leaf, and the §7 route each read proxies.
const RESOURCES: [(&str, &str); 5] = [
    ("quotes", "market/quotes"),
    ("book", "market/book"),
    ("positions", "positions"),
    ("orders", "orders"),
    ("instruments", "market/instruments"),
];

/// A verified daemon plus the one desk this process is bound to.
struct Adapter {
    endpoint: Endpoint,
    desk_id: String,
    desk_name: String,
}

impl Adapter {
    fn uri(&self, leaf: &str) -> String {
        format!("marketrig://desk/{}/{leaf}", self.desk_name)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let adapter = match connect(&cli.desk) {
        Ok(adapter) => adapter,
        Err(fault) => return fail(&fault.code, &fault.message, fault.exit),
    };
    if cli.channel {
        return bridge(adapter);
    }
    // Blocking daemon calls run inside the handlers, so the runtime is
    // multi-threaded: one stalled worker never stops the stdio reader.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => return fail("INTERNAL", &format!("Cannot start a runtime: {error}."), 1),
    };
    runtime.block_on(async move {
        match adapter.serve(rmcp::transport::stdio()).await {
            Ok(service) => {
                let _ = service.waiting().await;
                ExitCode::SUCCESS
            }
            Err(error) => fail(
                "INTERNAL",
                &format!("Cannot serve MCP over stdio: {error}."),
                1,
            ),
        }
    })
}

/// Startup failures reach the operator as the §4.3 envelope on standard error,
/// because standard output belongs to the protocol.
fn fail(code: &str, message: &str, exit: i32) -> ExitCode {
    eprintln!(
        "{}",
        serde_json::json!({ "code": code, "message": message })
    );
    ExitCode::from(exit.clamp(1, 255) as u8)
}

fn connect(desk: &str) -> Result<Adapter, Fault> {
    let endpoint = Endpoint::discover()?;
    let (desk_id, desk_name) = resolve(&endpoint, desk)?;
    Ok(Adapter {
        endpoint,
        desk_id,
        desk_name,
    })
}

/// Name-or-id resolved through the daemon's own listing (§8); the adapter needs
/// the UUID for its routes and the kebab name for its URIs, so it matches
/// either field.
///
/// ponytail: deliberately duplicated from the CLI's private `resolve` — ten
/// lines each, and the CLI's returns only an id. Upgrade path: one resolver
/// exposed on the `marketrig` library when a third caller wants it.
fn resolve(endpoint: &Endpoint, desk: &str) -> Result<(String, String), Fault> {
    let body = endpoint.get("/desks")?;
    let listing: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Fault::reported("INTERNAL", format!("Cannot read the desk listing: {e}.")))?;
    listing["desks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["id"].as_str() == Some(desk) || d["name"].as_str() == Some(desk))
        .and_then(|d| {
            Some((
                d["id"].as_str()?.to_string(),
                d["name"].as_str()?.to_string(),
            ))
        })
        .ok_or_else(|| Fault::reported("DESK_NOT_FOUND", format!("No desk is {desk}.")))
}

/// A hand-written schema is documentation, never a trust boundary (per D4).
fn schema(schema: serde_json::Value) -> Arc<JsonObject> {
    match schema {
        serde_json::Value::Object(map) => Arc::new(map),
        other => unreachable!("a tool schema is a JSON object, not {other}"),
    }
}

impl ServerHandler for Adapter {
    /// Resources and tools, and nothing else: no subscription, no completion
    /// (§8).
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            RESOURCES
                .iter()
                .map(|(leaf, _)| {
                    Resource::new(self.uri(leaf), *leaf).with_mime_type("application/json")
                })
                .collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let route = RESOURCES
            .iter()
            .find(|(leaf, _)| request.uri == self.uri(leaf))
            .ok_or_else(|| {
                ErrorData::resource_not_found(format!("No resource is {}.", request.uri), None)
            })?
            .1;
        let body = self
            .endpoint
            .get(&format!("/desks/{}/{route}", self.desk_id))
            .map_err(|fault| {
                ErrorData::internal_error(format!("{}: {}", fault.code, fault.message), None)
            })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(body, request.uri.as_str()).with_mime_type("application/json"),
        ])
        // The daemon is read at read time and nothing may cache the answer.
        .with_ttl_ms(0)
        .into())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new(
                "submit_order",
                "Submit a paper order for this desk. The daemon validates every \
                 argument and answers with the action record.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action_id": { "type": "string" },
                        "instrument_id": { "type": "string" },
                        "side": { "type": "string", "enum": ["BUY", "SELL"] },
                        "type": { "type": "string", "enum": ["MARKET", "LIMIT"] },
                        "quantity": { "type": "string" },
                        "price": { "type": ["string", "null"] }
                    },
                    "required": ["action_id", "instrument_id", "side", "type", "quantity"]
                })),
            ),
            Tool::new(
                "cancel_order",
                "Cancel one resting order of this desk by its client order id.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "client_order_id": { "type": "string" },
                        "action_id": { "type": "string" }
                    },
                    "required": ["client_order_id", "action_id"]
                })),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = serde_json::Value::Object(request.arguments.unwrap_or_default());
        let answered = match request.name.as_ref() {
            "submit_order" => self
                .endpoint
                .post(&format!("/desks/{}/orders", self.desk_id), Some(arguments)),
            "cancel_order" => match arguments["client_order_id"].as_str().map(str::to_string) {
                // The one argument the adapter must read, because it addresses
                // the route — and therefore the one it must keep to a single
                // path segment; everything about it is still the daemon's call.
                Some(id) if !id.is_empty() && id.chars().all(path_segment) => self.endpoint.post(
                    &format!("/desks/{}/orders/{id}/cancel", self.desk_id),
                    Some(arguments),
                ),
                _ => Err(Fault::reported(
                    "ORDER_INVALID",
                    "cancel_order needs a client_order_id of [A-Za-z0-9._-].",
                )),
            },
            other => {
                return Err(ErrorData::invalid_params(
                    format!("No tool is {other}."),
                    None,
                ));
            }
        };
        // A refusal, an unreachable daemon, or a failed verification is a
        // structured tool error carrying the envelope — never a retry (§8).
        Ok(match answered {
            Ok(body) => CallToolResult::success(vec![ContentBlock::text(body)]),
            Err(fault) => CallToolResult::error(vec![ContentBlock::text(format!(
                "{}: {}",
                fault.code, fault.message
            ))]),
        }
        .into())
    }
}

// ---------------------------------------------------------------------------
// The channel bridge (R3 feature SPEC §5.3)
// ---------------------------------------------------------------------------

/// The bridge's own server: no tools, no resources, one experimental
/// capability, and a sentence saying what arrives on it.
struct Bridge;

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.experimental = Some(
            [("claude/channel".to_string(), JsonObject::new())]
                .into_iter()
                .collect(),
        );
        ServerInfo::new(capabilities).with_instructions(
            "Events on this channel come from MarketRig: each one is a prompt from your desk \
             for you to act on.",
        )
    }
}

/// One daemon frame as the notification the channel publishes. A frame that is
/// not the documented object is dropped rather than guessed at.
fn notification(frame: &str) -> Option<rmcp::model::CustomNotification> {
    let frame: serde_json::Value = serde_json::from_str(frame).ok()?;
    Some(rmcp::model::CustomNotification::new(
        "notifications/claude/channel",
        Some(serde_json::json!({
            "content": frame.get("content")?,
            "meta": {
                "prompt_id": frame.get("prompt_id")?,
                "kind": frame.get("kind")?,
            },
        })),
    ))
}

/// Serves the channel over stdio while forwarding the desk's socket, and exits
/// when either end goes away (§5.3).
fn bridge(adapter: Adapter) -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => return fail("INTERNAL", &format!("Cannot start a runtime: {error}."), 1),
    };
    runtime.block_on(async move {
        let service = match Bridge.serve(rmcp::transport::stdio()).await {
            Ok(service) => service,
            Err(error) => {
                return fail(
                    "INTERNAL",
                    &format!("Cannot serve MCP over stdio: {error}."),
                    1,
                );
            }
        };
        let peer = service.peer().clone();
        let socket = tokio::spawn(forward(
            adapter.endpoint.base().replacen("http://", "ws://", 1)
                + &format!("/desks/{}/channel", adapter.desk_id),
            format!("Bearer {}", adapter.endpoint.credential()),
            peer,
        ));
        // Either end going away ends the bridge (§5.3).
        tokio::select! {
            _ = socket => {}
            _ = service.waiting() => {}
        }
        ExitCode::SUCCESS
    })
}

async fn forward(url: String, bearer: String, peer: rmcp::service::Peer<RoleServer>) {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let Ok(mut request) = url.into_client_request() else {
        return;
    };
    let Ok(bearer) = bearer.parse() else { return };
    request.headers_mut().insert("authorization", bearer);
    let Ok((mut socket, _)) = tokio_tungstenite::connect_async(request).await else {
        return;
    };
    while let Some(Ok(message)) = socket.next().await {
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message
            && let Some(notification) = notification(&text)
        {
            let sent = peer
                .send_notification(rmcp::model::ServerNotification::CustomNotification(
                    notification,
                ))
                .await;
            if sent.is_err() {
                return;
            }
        }
    }
}

fn path_segment(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_declares_the_channel_and_nothing_else() {
        let info = Bridge.get_info();
        assert_eq!(
            info.capabilities.experimental.as_ref().map(|e| e.len()),
            Some(1)
        );
        assert!(
            info.capabilities
                .experimental
                .as_ref()
                .is_some_and(|e| e.contains_key("claude/channel"))
        );
        assert!(info.capabilities.tools.is_none());
        assert!(info.capabilities.resources.is_none());
        assert!(
            info.instructions
                .as_deref()
                .is_some_and(|i| i.contains("MarketRig"))
        );
    }

    #[test]
    fn a_frame_becomes_one_channel_notification() {
        let frame = r#"{"content":"MarketRig TRIGGER_RESULT p-1:","prompt_id":"p-1",
                        "kind":"TRIGGER_RESULT"}"#;
        let published = notification(frame).expect("a documented frame");
        assert_eq!(published.method, "notifications/claude/channel");
        assert_eq!(
            published.params,
            Some(serde_json::json!({
                "content": "MarketRig TRIGGER_RESULT p-1:",
                "meta": { "prompt_id": "p-1", "kind": "TRIGGER_RESULT" },
            }))
        );
        // Anything else is dropped, never guessed at.
        assert!(notification("not json").is_none());
        assert!(notification(r#"{"content":"no meta"}"#).is_none());
    }
}
