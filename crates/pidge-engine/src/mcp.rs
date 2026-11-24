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

    /// The program calling, when the transport knows better than the MCP
    /// session does (a stateless HTTP request has no session).
    fn client(&self, _ctx: &RequestContext<RoleServer>) -> Option<crate::activity::Client> {
        None
    }

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
    // pidge learns from what is asked: agents are to say it on every call.
    // A call without it still runs.
    let required = schema
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(list) = required {
        list.push(Value::String("intent".into()));
    }
    let mut tool = Tool::new(op.name, op.description, Arc::new(schema));
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(op.read_only)
            .destructive(false),
    );
    tool
}

/// A tool pidge does not have. Before 3.0 some had other names, and a client
/// may still hold the old list.
fn unknown_tool(name: &str) -> String {
    let now = match name {
        "mail_brief" | "mail_triage" => "mail_inbox",
        "mail_setup" | "accounts" => "settings",
        "calendar_event" | "calendar_respond" => "calendar_act",
        "intent" => "the intent field every tool takes",
        _ => return format!("unknown tool {name}"),
    };
    format!(
        "{name} is now {now}. The user should refresh the pidge connection in this app so that it sees the current tools."
    )
}

/// Who is calling: what the request itself says, then the MCP session, then
/// the transport.
fn caller<S: UserSource>(
    source: &S,
    request: &CallToolRequestParams,
    context: &RequestContext<RoleServer>,
) -> Option<crate::activity::Client> {
    const KEY: &str = "io.modelcontextprotocol/clientInfo";
    let named = |v: &Value| -> Option<crate::activity::Client> {
        Some(crate::activity::Client {
            name: v.get("name")?.as_str()?.chars().take(100).collect(),
            version: v
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(50)
                .collect(),
        })
    };
    let in_request = request
        .meta
        .as_ref()
        .and_then(|m| serde_json::to_value(m).ok())
        .or_else(|| serde_json::to_value(&context.meta).ok())
        .and_then(|m| m.get(KEY).and_then(named));
    // Without a session the library fills in its own name.
    let in_session = context
        .peer
        .peer_info()
        .filter(|p| p.client_info.name != "rmcp")
        .map(|p| crate::activity::Client {
            name: p.client_info.name.clone(),
            version: p.client_info.version.clone(),
        });
    in_request.or(in_session).or_else(|| source.client(context))
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
    pub async fn call_for(
        &self,
        user: &Arc<User>,
        name: &str,
        args: Value,
        client: Option<&crate::activity::Client>,
    ) -> CallToolResult {
        let Some(op) = self.ops.iter().find(|o| o.name == name) else {
            return CallToolResult::error(vec![ContentBlock::text(unknown_tool(name))]);
        };
        match registry::run_from(user, op, args, client).await {
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
        let client = caller(self.source.as_ref(), &request, &context);
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let result = match self.source.user(&context).await {
            Ok(user) => self.call_for(&user, &name, args, client.as_ref()).await,
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
