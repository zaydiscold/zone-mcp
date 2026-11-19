//! `zone mcp`: the library as Model Context Protocol tools over stdin and stdout.
//!
//! The server opens the SQLite file itself, so it works whether or not the daemon is running.
//! Standard output carries only protocol messages. Anything for a person goes to stderr.
//!
//! Every request reads the agents switch (`zone agents`, see agents.rs) from the library. While
//! it is off the server offers one tool that says so, and answers everything else with the
//! same sentence. A watcher tells the client within a few seconds when the switch changes, so
//! a client that keeps its lists sees the new ones without a restart.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse,
        GetPromptResult, Implementation, JsonObject, ListPromptsResult, ListToolsResult, PaginatedRequestParams, Prompt,
        PromptArgument, PromptMessage, Role, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::{NotificationContext, RequestContext, RoleServer},
    transport::stdio,
};
use serde_json::json;

use crate::agents;
use crate::db::Db;
use crate::prompts;
use crate::tools::{self, Access};

/// How often a running server looks at the agents switch, to tell its client when it changed.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// The one tool a server offers while agent access is off. It has the name of the tool agents
/// call first anyway, so one that tries finds out why at once.
const OFF_TOOL: &str = "sync_status";

pub struct ZoneServer {
    db: Arc<Mutex<Db>>,
    /// Whether this server may flag items, and which items it shows (see scope.rs).
    access: Access,
    /// The switch as the client was last told it.
    told_on: Arc<AtomicBool>,
}

impl ZoneServer {
    pub fn new(db: Db, access: Access) -> Self {
        let on = agents::enabled(&db);
        Self { db: Arc::new(Mutex::new(db)), access, told_on: Arc::new(AtomicBool::new(on)) }
    }

    /// Whether agents may use the library right now. Read on every request.
    fn agents_on(&self) -> bool {
        self.db.lock().map(|db| agents::enabled(&db)).unwrap_or(true)
    }

    fn tool_list(&self) -> Vec<Tool> {
        if !self.agents_on() {
            return vec![off_tool()];
        }
        tools::definitions(&self.access)
            .into_iter()
            .map(|def| {
                let schema: JsonObject = def.input_schema.as_object().cloned().unwrap_or_default();
                // Every tool works on the local library only, and repeating a call is harmless.
                let annotations = ToolAnnotations::with_title(def.title)
                    .read_only(def.read_only)
                    .destructive(false)
                    .idempotent(true)
                    .open_world(false);
                Tool::new(def.name, def.description, Arc::new(schema)).with_title(def.title).with_annotations(annotations)
            })
            .collect()
    }
}

/// The prompts this server offers, as MCP has them. None while agent access is off.
fn prompt_list(access: &Access) -> Vec<Prompt> {
    prompts::offered(&access.scope)
        .into_iter()
        .map(|def| {
            let arguments: Vec<PromptArgument> = def
                .args
                .iter()
                .map(|arg| {
                    let description = match arg.default {
                        Some(default) => format!("{} Default: {default}", arg.description),
                        None => arg.description.to_string(),
                    };
                    PromptArgument::new(arg.name).with_description(description).with_required(arg.required)
                })
                .collect();
            Prompt::new(def.id, Some(def.description), (!arguments.is_empty()).then_some(arguments)).with_title(def.title)
        })
        .collect()
}

/// The tool on offer while agent access is off.
fn off_tool() -> Tool {
    let schema: JsonObject = json!({ "type": "object", "properties": {}, "additionalProperties": false }).as_object().cloned().unwrap_or_default();
    let annotations = ToolAnnotations::with_title("Zone: agent access is off").read_only(true).destructive(false).idempotent(true).open_world(false);
    Tool::new(
        OFF_TOOL,
        "Agent access to Zone is turned off on this computer. Call this to get the exact words to tell the person, including how they turn it back on. No other Zone tool works until they do.",
        Arc::new(schema),
    )
    .with_title("Zone: agent access is off")
    .with_annotations(annotations)
}

impl ServerHandler for ZoneServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities =
            ServerCapabilities::builder().enable_tools().enable_tool_list_changed().enable_prompts().enable_prompts_list_changed().build();
        let instructions = if self.agents_on() { tools::instructions(&self.access.scope) } else { agents::off_message() };
        ServerConfig::new(capabilities)
            .with_server_info(
                Implementation::new("zone", env!("CARGO_PKG_VERSION"))
                    .with_title("Zone")
                    .with_description("The person's dictations, voice notes and meeting transcripts from the Zone iPhone app, kept on this computer."),
            )
            .with_instructions(instructions)
    }

    /// Starts the watcher that tells the client when the agents switch changes.
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let (db, told_on, peer) = (self.db.clone(), self.told_on.clone(), context.peer);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(WATCH_EVERY).await;
                let Ok(on) = db.lock().map(|db| agents::enabled(&db)) else { return };
                if told_on.swap(on, Ordering::SeqCst) != on
                    && (peer.notify_tool_list_changed().await.is_err() || peer.notify_prompt_list_changed().await.is_err())
                {
                    // The client has gone.
                    return;
                }
            }
        });
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tool_list()))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let list = if self.agents_on() { prompt_list(&self.access) } else { Vec::new() };
        Ok(ListPromptsResult::with_all_items(list))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        if !self.agents_on() {
            return Err(ErrorData::invalid_request(agents::off_message(), None));
        }
        let Some(def) = prompts::find(&request.name).filter(|def| def.offered(&self.access.scope)) else {
            let names: Vec<&str> = prompts::offered(&self.access.scope).iter().map(|def| def.id).collect();
            return Err(ErrorData::invalid_params(format!("no prompt is called \"{}\". This server offers: {}", request.name, names.join(", ")), None));
        };
        let mut given = std::collections::BTreeMap::new();
        for (name, value) in request.arguments.unwrap_or_default() {
            let text = match value {
                serde_json::Value::String(text) => text,
                serde_json::Value::Null => continue,
                other => other.to_string(),
            };
            given.insert(name, text);
        }
        let text = prompts::render(def, &given, &self.access, false).map_err(|error| ErrorData::invalid_params(format!("{error:#}"), None))?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).with_description(def.description).into())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_list().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.as_ref();
        if !self.agents_on() {
            // Whatever was asked for, including a tool the client remembered from before.
            let result = if name == OFF_TOOL {
                let answer = json!({ "agent_access": "off", "message": agents::off_message() });
                CallToolResult::success(vec![ContentBlock::text(answer.to_string())])
            } else {
                CallToolResult::error(vec![ContentBlock::text(agents::off_message())])
            };
            return Ok(result.into());
        }
        if !tools::definitions(&self.access).iter().any(|def| def.name == name) {
            return Err(ErrorData::invalid_params(format!("unknown tool: {name}"), None));
        }
        let arguments = request.arguments.unwrap_or_default();
        let outcome = {
            let mut db = self.db.lock().map_err(|_| ErrorData::internal_error("the library is unavailable", None))?;
            tools::call(&mut db, name, &arguments, &self.access)
        };
        let result = match outcome {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
            // A tool that could not do what was asked says why, so the agent can try again.
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
        };
        Ok(result.into())
    }
}

/// Serves until the client closes stdin.
pub async fn serve(access: Access) -> Result<()> {
    let db = Db::open_default()?;
    let server = ZoneServer::new(db, access);
    let running = server.serve(stdio()).await.map_err(|e| anyhow!("the MCP client did not complete the handshake: {e}"))?;
    running.waiting().await.map_err(|e| anyhow!("the MCP server stopped unexpectedly: {e}"))?;
    Ok(())
}
