//! `zone mcp`: the library as Model Context Protocol tools over stdin and stdout.
//!
//! The server opens the SQLite file itself, so it works whether or not the daemon is running.
//! Standard output carries only protocol messages. Anything for a person goes to stderr.

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
};

use crate::db::Db;
use crate::tools::{self, Access};

pub struct ZoneServer {
    db: Mutex<Db>,
    /// Whether this server may flag items, and which items it shows (see scope.rs).
    access: Access,
}

impl ZoneServer {
    pub fn new(db: Db, access: Access) -> Self {
        Self { db: Mutex::new(db), access }
    }

    fn tool_list(&self) -> Vec<Tool> {
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

impl ServerHandler for ZoneServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("zone", env!("CARGO_PKG_VERSION"))
                    .with_title("Zone")
                    .with_description("The person's dictations, voice notes and meeting transcripts from the Zone iPhone app, kept on this computer."),
            )
            .with_instructions(tools::instructions(&self.access.scope))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tool_list()))
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
