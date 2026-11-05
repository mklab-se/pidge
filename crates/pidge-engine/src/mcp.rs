//! The MCP surface, shared by `pidge mcp serve` (stdio) and the hosted
//! server (HTTP): tools are the registry's operations, the instructions are
//! the playbooks. A [`UserSource`] says who is calling (the local user, or
//! the bearer identity the hosted server verified).

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ResultType,
    ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;

use crate::ctx::User;
use crate::registry::{self, OpDef};

/// Who a call is for.
#[async_trait]
pub trait UserSource: Send + Sync + 'static {
    /// The caller's engine user, or a message for the harness.
    async fn user(&self, ctx: &RequestContext<RoleServer>) -> Result<Arc<User>, String>;

    /// One line per call for the operator's logs (no arguments, no content).
    fn log_call(&self, _tool: &str, _ctx: &RequestContext<RoleServer>, _ms: u64, _outcome: &str) {}
}

pub struct PidgeServer<S: UserSource> {
    source: Arc<S>,
    ops: Arc<Vec<OpDef>>,
    tools: Arc<Vec<Tool>>,
    instructions: String,
}

impl<S: UserSource> Clone for PidgeServer<S> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            ops: self.ops.clone(),
            tools: self.tools.clone(),
            instructions: self.instructions.clone(),
        }
    }
}

fn tool_of(op: &OpDef) -> Tool {
    let mut schema: JsonObject = op.schema.as_object().cloned().unwrap_or_default();
    schema.remove("$schema");
    schema.remove("title");
    let mut tool = Tool::new(op.name, op.description, Arc::new(schema));
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(op.read_only)
            .destructive(false),
    );
    tool
}

impl<S: UserSource> PidgeServer<S> {
    pub fn new(source: S) -> Self {
        let ops = registry::all();
        let tools = ops.iter().map(tool_of).collect();
        Self {
            source: Arc::new(source),
            ops: Arc::new(ops),
            tools: Arc::new(tools),
            instructions: crate::playbooks::instructions(),
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    /// Run a tool for an already-known user (tests, and the HTTP layer).
    pub async fn call_for(&self, user: &Arc<User>, name: &str, args: Value) -> CallToolResult {
        let Some(op) = self.ops.iter().find(|o| o.name == name) else {
            return CallToolResult::error(vec![ContentBlock::text(format!("unknown tool {name}"))]);
        };
        match registry::run(user, op, args).await {
            Ok(out) => CallToolResult::success(vec![ContentBlock::text(out.text)]),
            Err(e) => {
                CallToolResult::error(vec![ContentBlock::text(format!("Error: {}", e.full()))])
            }
        }
    }
}

impl<S: UserSource> ServerHandler for PidgeServer<S> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info({
                let mut info = Implementation::new("pidge", env!("CARGO_PKG_VERSION"));
                info.title = Some("pidge".into());
                info.description = Some("The user's e-mail and calendar, for AI agents".into());
                info.website_url = Some("https://github.com/mklab-se/pidge".into());
                info
            })
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(self.instructions.clone())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let start = Instant::now();
        let name = request.name.to_string();
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let result = match self.source.user(&context).await {
            Ok(user) => self.call_for(&user, &name, args).await,
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        };
        let outcome = if result.is_error == Some(true) {
            "tool_error"
        } else {
            "ok"
        };
        self.source
            .log_call(&name, &context, start.elapsed().as_millis() as u64, outcome);
        Ok(CallToolResponse::Complete(result))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.tools.as_ref().clone(),
            meta: None,
            next_cursor: None,
            ttl_ms: None,
            cache_scope: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }
}
