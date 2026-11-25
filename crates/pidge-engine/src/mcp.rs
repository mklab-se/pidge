//! The MCP surface, shared by `pidge mcp serve` (stdio) and the hosted
//! server (HTTP): tools are the registry's operations, the instructions are
//! the playbooks. A [`UserSource`] says who is calling (the local user, or
//! the bearer identity the hosted server verified).

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ContentBlock,
    ElicitRequest, ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema,
    GetSkillRequestParams, GetSkillResult, Implementation, InputRequest, InputRequests,
    InputRequiredResult, JsonObject, ListResourcesResult, ListSkillsResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ResourceContents, ResultType, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;

use crate::ctx::User;
use serde::{Deserialize, Serialize};

use crate::activity::Client;
use crate::registry::{self, Ask, Asking, OpDef};
use crate::skills;

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

/// Who is calling, with what: what the request itself says, then the MCP
/// session, then the transport.
fn caller<S: UserSource>(source: &S, context: &RequestContext<RoleServer>) -> Option<Client> {
    let short = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let mut client = context
        .client_info()
        // Without a session the library fills in its own name.
        .filter(|i| i.name != "rmcp")
        .map(|i| Client {
            name: short(&i.name, 100),
            version: short(&i.version, 50),
            ..Client::default()
        })
        .or_else(|| source.client(context))
        .unwrap_or_default();
    client.protocol = context
        .meta
        .protocol_version()
        .or_else(|| context.protocol_version())
        .map(|v| short(v.as_str(), 20));
    if let Some(caps) = context.client_capabilities() {
        client.can = caps
            .extensions
            .iter()
            .flatten()
            .map(|(name, _)| short(name, 60))
            .take(20)
            .collect();
        if takes_forms(&caps) {
            client.can.push("form".into());
        }
        if caps.elicitation.as_ref().is_some_and(|e| e.url.is_some()) {
            client.can.push("url".into());
        }
    }
    Some(client).filter(|c| *c != Client::default())
}

/// The client can put a form to the user. (An empty declaration means forms.)
fn takes_forms(caps: &ClientCapabilities) -> bool {
    caps.elicitation
        .as_ref()
        .is_some_and(|e| e.form.is_some() || e.url.is_none())
}

/// Whether a question can be put to the user in this request: the protocol
/// carries one without a session, and the client can show it.
fn asking_of(context: &RequestContext<RoleServer>) -> Asking {
    let new_enough = context
        .meta
        .protocol_version()
        .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28);
    let shows = context
        .meta
        .client_capabilities()
        .is_some_and(|c| takes_forms(&c));
    if new_enough && shows {
        Asking::CanAsk
    } else {
        Asking::InChat
    }
}

/// A question under way, kept with the user it is put to.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Question {
    /// The tool whose call it belongs to.
    asked_in: String,
    ask: Ask,
}

const QUESTION: &str = "confirm";
const QUESTION_MINUTES: i64 = 15;

fn form(ask: &Ask) -> Result<ElicitationSchema, &'static str> {
    let yes = ask.yes.clone();
    ElicitationSchema::builder()
        .required_bool_with(QUESTION, |b| b.title(yes).with_default(true))
        .build()
}

fn said(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text.into())])
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

    /// Run a tool for an already-known user.
    async fn run(
        &self,
        user: &Arc<User>,
        name: &str,
        args: Value,
        client: Option<&Client>,
        asking: Asking,
    ) -> CallToolResponse {
        let Some(op) = self.ops.iter().find(|o| o.name == name) else {
            return CallToolResult::error(vec![ContentBlock::text(unknown_tool(name))]).into();
        };
        match registry::run_from(user, op, args, client, asking).await {
            Ok(out) => {
                if asking == Asking::CanAsk
                    && let Some(ask) = out.ask
                    && let Some(question) = self.put(user, name, ask)
                {
                    return question.into();
                }
                said(out.text).into()
            }
            Err(e) => {
                CallToolResult::error(vec![ContentBlock::text(format!("Error: {}", e.full()))])
                    .into()
            }
        }
    }

    /// Put a question to the user. The state the client echoes is only a
    /// random name for it; the question stays here, with this user.
    fn put(&self, user: &Arc<User>, tool: &str, ask: Ask) -> Option<InputRequiredResult> {
        let schema = form(&ask).ok()?;
        let id = format!("{:032x}", rand::random::<u128>());
        let message = ask.message.clone();
        user.store
            .put_doc(
                "question",
                &id,
                &Question {
                    asked_in: tool.to_string(),
                    ask,
                },
                Some(chrono::Utc::now() + chrono::Duration::minutes(QUESTION_MINUTES)),
            )
            .ok()?;
        let mut requests = InputRequests::new();
        requests.insert(
            QUESTION.into(),
            InputRequest::Elicitation(ElicitRequest::new(
                ElicitRequestParams::FormElicitationParams {
                    meta: None,
                    message,
                    requested_schema: schema,
                },
            )),
        );
        Some(InputRequiredResult::new(Some(requests), Some(id)))
    }

    /// The user's answer to a question.
    async fn answered(
        &self,
        user: &Arc<User>,
        name: &str,
        state: &str,
        answer: Option<&Value>,
        client: Option<&Client>,
    ) -> CallToolResult {
        let question = user
            .store
            .get_doc::<Question>("question", state)
            .ok()
            .flatten()
            .filter(|q| q.asked_in == name);
        let Some(Question { ask, .. }) = question else {
            return CallToolResult::error(vec![ContentBlock::text(
                "Error: that question is no longer open; make the call again",
            )]);
        };
        // One answer each.
        let _ = user.store.delete_doc("question", state);
        let answer = answer.and_then(|a| serde_json::from_value::<ElicitResult>(a.clone()).ok());
        let yes = answer.as_ref().is_some_and(|a| {
            a.action == ElicitationAction::Accept
                && a.content
                    .as_ref()
                    .and_then(|c| c.get(QUESTION))
                    .and_then(Value::as_bool)
                    == Some(true)
        });
        let no = answer
            .as_ref()
            .is_some_and(|a| a.action != ElicitationAction::Cancel);
        let word = match (yes, no) {
            (true, _) => "yes",
            (_, true) => "no",
            _ => "none",
        };
        user.record(crate::activity::ask_line(client, &ask.tool, word))
            .await;
        let run = |input: Value, asking| self.run(user, &ask.tool, input, client, asking);
        let done = match (yes, no, ask.on_no.clone()) {
            (true, _, _) => run(ask.input.clone(), Asking::Said).await,
            (_, true, Some(input)) => run(input, Asking::InChat).await,
            (_, true, None) => said("The user said no: nothing was done.").into(),
            _ => said(
                "The user did not answer the question: nothing was done. Do not try again unless they ask.",
            )
            .into(),
        };
        match done {
            CallToolResponse::Complete(mut r) if !yes && no && ask.on_no.is_some() => {
                r.content.insert(0, ContentBlock::text("The user said no."));
                r
            }
            CallToolResponse::Complete(r) => r,
            _ => said("Nothing was done."),
        }
    }
}

impl<S: UserSource> ServerHandler for PidgeServer<S> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_skills()
                .build(),
        )
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
        let client = caller(self.source.as_ref(), &context);
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let response = match self.source.user(&context).await {
            Ok(user) => match &request.request_state {
                Some(state) => {
                    let answer = request
                        .input_responses
                        .as_ref()
                        .and_then(|r| r.get(QUESTION));
                    self.answered(&user, &name, state, answer, client.as_ref())
                        .await
                        .into()
                }
                None => {
                    let asking = asking_of(&context);
                    self.run(&user, &name, args, client.as_ref(), asking).await
                }
            },
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]).into(),
        };
        let outcome = match &response {
            CallToolResponse::Complete(r) if r.is_error == Some(true) => "tool_error",
            CallToolResponse::Complete(_) => "ok",
            _ => "asked",
        };
        self.source
            .log_call(&name, &context, start.elapsed().as_millis() as u64, outcome);
        Ok(response)
    }

    async fn list_skills(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        Ok(ListSkillsResult::with_all_items(
            skills::all()
                .iter()
                .map(skills::PidgeSkill::entry)
                .collect(),
        ))
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        skills::find(&request.uri)
            .map(|s| GetSkillResult::new(s.entry()))
            .ok_or_else(|| McpError::invalid_params(format!("no skill {}", request.uri), None))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(
            skills::all()
                .iter()
                .map(skills::PidgeSkill::resource)
                .collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        skills::find(&request.uri)
            .map(|s| {
                ReadResourceResult::new(vec![
                    ResourceContents::text(s.markdown(), s.uri()).with_mime_type("text/markdown"),
                ])
                .into()
            })
            .ok_or_else(|| McpError::resource_not_found(request.uri, None))
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
