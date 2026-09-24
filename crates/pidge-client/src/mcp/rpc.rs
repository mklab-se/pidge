//! Minimal JSON-RPC client for the MCP `/mcp` endpoint (Streamable HTTP
//! transport, as served by `rmcp`'s `StreamableHttpService` in
//! `pidge-mcp`'s `crates/pidge-mcp/src/app.rs`).
//!
//! Each call is a single `POST` carrying one JSON-RPC request, with
//! `Accept: application/json, text/event-stream`; the server may answer
//! with either a plain JSON body or a `text/event-stream` body whose last
//! `data:` line carries the JSON-RPC response. The server hands back an
//! `Mcp-Session-Id` header on `initialize`; every later call on the same
//! session echoes it back.

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde_json::{Value, json};

use crate::error::ClientError;

use super::oauth::PROTOCOL_VERSION;

const SESSION_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_HEADER: &str = "MCP-Protocol-Version";

/// The text content of a `tools/call` response, with content blocks
/// concatenated.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// The concatenated text of every `type: "text"` content block, in
    /// order, with no separator between blocks.
    pub text: String,
    /// The JSON-RPC result's `isError` flag: a tool-level failure reported
    /// as a normal result rather than a JSON-RPC `error` response, per the
    /// MCP spec.
    pub is_error: bool,
}

/// A JSON-RPC session against one MCP server. Not thread-safe by itself;
/// wrap in a mutex/actor if shared across tasks.
pub struct McpRpc {
    http: reqwest::Client,
    mcp_url: String,
    access_token: String,
    session_id: Option<String>,
    next_id: u64,
}

impl McpRpc {
    /// Start a new (uninitialized) session. Call [`Self::initialize`] before
    /// any other method.
    pub fn new(
        http: reqwest::Client,
        mcp_url: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            http,
            mcp_url: mcp_url.into(),
            access_token: access_token.into(),
            session_id: None,
            next_id: 1,
        }
    }

    /// The `Mcp-Session-Id` the server assigned, once `initialize` has run.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Replace the bearer token sent on every subsequent call. For a caller
    /// holding a session across a long wait (e.g. a multi-account migration
    /// that can span several minutes), this is how a refreshed access token
    /// gets swapped in without discarding the session id or rebuilding the
    /// client. The session id and request counter are left untouched.
    pub fn set_access_token(&mut self, access_token: impl Into<String>) {
        self.access_token = access_token.into();
    }

    /// Run the MCP `initialize` handshake and send the required
    /// `notifications/initialized` follow-up. Stores the session id the
    /// server returns, if any, for subsequent calls.
    pub async fn initialize(&mut self) -> Result<(), ClientError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "pidge", "version": env!("CARGO_PKG_VERSION")},
        });
        self.request_once("initialize", params).await?;
        self.notify_once("notifications/initialized", json!({}))
            .await
    }

    /// Drop the current session and run the handshake again. Called when the
    /// server answers 404 to a request carrying our `Mcp-Session-Id`: MCP
    /// 2025-06-18 (Transport, Session Management) says the client MUST then
    /// start a new session. `pidge-mcp` drops sessions idle for 5 minutes and
    /// on every redeploy, so a long interactive wait hits this routinely.
    async fn reinitialize(&mut self) -> Result<(), ClientError> {
        tracing::debug!("MCP session expired on the server (404); starting a new one");
        self.session_id = None;
        self.initialize().await
    }

    /// List the names of the tools this server exposes.
    pub async fn list_tools(&mut self) -> Result<Vec<String>, ClientError> {
        let result = self.request("tools/list", json!({})).await?;
        let names = result
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        Ok(names)
    }

    /// Call tool `name` with `arguments`, concatenating its text content
    /// blocks into [`ToolResult::text`].
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolResult, ClientError> {
        let params = json!({"name": name, "arguments": arguments});
        let result = self.request("tools/call", params).await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        Ok(ToolResult { text, is_error })
    }

    /// Send a JSON-RPC request and return its `result` value. Errors on a
    /// JSON-RPC `error` response or a non-2xx HTTP status. If the request
    /// carried a session id and the server answers 404 (session expired),
    /// re-runs the handshake and retries once; a second 404 is returned.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let had_session = self.session_id.is_some();
        match self.request_once(method, params.clone()).await {
            Err(err) if had_session && is_session_gone(&err) => {
                self.reinitialize().await?;
                self.request_once(method, params).await
            }
            other => other,
        }
    }

    /// One attempt at a JSON-RPC request, with no session recovery.
    async fn request_once(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});

        let (session_id, value) = self.send(&body, Some(id)).await?;
        if session_id.is_some() {
            self.session_id = session_id;
        }
        let value = value.ok_or_else(|| ClientError::Graph {
            status: 502,
            message: format!("MCP server returned no body for {method}"),
        })?;
        if let Some(err) = value.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP error")
                .to_string();
            return Err(ClientError::Graph {
                status: 502,
                message,
            });
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send a JSON-RPC notification (no `id`, no response body expected).
    /// The only notification pidge sends is `notifications/initialized`,
    /// which is part of the handshake itself, so it gets no session
    /// recovery of its own: a 404 there right after `initialize` is not an
    /// idle expiry and is returned as is.
    async fn notify_once(&mut self, method: &str, params: Value) -> Result<(), ClientError> {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let (session_id, _) = self.send(&body, None).await?;
        if session_id.is_some() {
            self.session_id = session_id;
        }
        Ok(())
    }

    /// POST one JSON-RPC frame and parse whatever comes back, JSON or SSE.
    /// `expected_id` is the id of the request we sent (`None` for a
    /// notification); for an SSE body it picks out the one event that is
    /// our actual response among any the server also chose to interleave
    /// (progress notifications, server-initiated requests, …) in the same
    /// stream. Returns the server's `Mcp-Session-Id` (if present) and the
    /// decoded JSON-RPC message (if the body wasn't empty, as for
    /// notifications).
    async fn send(
        &self,
        body: &Value,
        expected_id: Option<u64>,
    ) -> Result<(Option<String>, Option<Value>), ClientError> {
        let mut req = self
            .http
            .post(&self.mcp_url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION)
            .header(AUTHORIZATION, format!("Bearer {}", self.access_token))
            .json(body);
        if let Some(session_id) = &self.session_id {
            req = req.header(SESSION_HEADER, session_id.as_str());
        }

        let resp = req.send().await?;
        let status = resp.status();
        let session_id = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(header_to_str)
            .map(str::to_string);
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(header_to_str)
            .unwrap_or_default()
            .to_string();
        let bytes = resp.bytes().await?;

        if !status.is_success() {
            return Err(ClientError::Graph {
                status: status.as_u16(),
                message: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        if bytes.is_empty() {
            return Ok((session_id, None));
        }

        let text = String::from_utf8_lossy(&bytes);
        let value = if content_type.contains("text/event-stream") {
            select_sse_response(&text, expected_id)
        } else {
            Some(serde_json::from_str(&text)?)
        };
        Ok((session_id, value))
    }
}

/// Whether `err` is the server's "unknown session" answer: HTTP 404 on a
/// request that carried an `Mcp-Session-Id`.
fn is_session_gone(err: &ClientError) -> bool {
    matches!(err, ClientError::Graph { status: 404, .. })
}

fn header_to_str(v: &HeaderValue) -> Option<&str> {
    v.to_str().ok()
}

/// Split an SSE body into its individual events. Lines are grouped on blank
/// lines (the SSE event boundary); a `data:` field that spans several lines
/// is reassembled by joining them with `"\n"`, per the SSE spec. Only the
/// `data:` field is used; `event:`, `id:`, `retry:` and comment lines are
/// ignored, since the Streamable HTTP transport only needs the JSON-RPC
/// payload. An event whose joined `data:` doesn't parse as JSON is silently
/// dropped rather than failing the whole body.
fn parse_sse_events(text: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let mut data_lines: Vec<&str> = Vec::new();

    let flush = |data_lines: &mut Vec<&str>, events: &mut Vec<Value>| {
        if data_lines.is_empty() {
            return;
        }
        let payload = data_lines.join("\n");
        data_lines.clear();
        if let Ok(value) = serde_json::from_str::<Value>(&payload) {
            events.push(value);
        }
    };

    for line in text.lines() {
        if line.is_empty() {
            flush(&mut data_lines, &mut events);
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    flush(&mut data_lines, &mut events);

    events
}

/// Pick the event that is our actual JSON-RPC response out of an SSE body.
///
/// With `expected_id`, selects the event whose top-level `id` matches; the
/// Streamable HTTP transport can interleave server notifications (no `id`)
/// or server-initiated requests (a different `id`) into the same stream
/// before or after our response. Without an expected id (we sent a
/// notification), there's nothing to match against, so this falls back to
/// the last event, matching the common case of a single-event stream.
fn select_sse_response(text: &str, expected_id: Option<u64>) -> Option<Value> {
    let events = parse_sse_events(text);
    match expected_id {
        Some(id) => events
            .into_iter()
            .find(|event| event.get("id").and_then(Value::as_u64) == Some(id)),
        None => events.into_iter().next_back(),
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_partial_json, headers, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn call_tool_parses_json_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE.as_str(), "application/json")
                    .set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "content": [{"type": "text", "text": "hello"}],
                            "isError": false
                        }
                    })),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let mut rpc = McpRpc::new(http, format!("{}/mcp", server.uri()), "AT");
        let result = rpc
            .call_tool("inbox_latest", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.text, "hello");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn call_tool_parses_sse_response() {
        let server = MockServer::start().await;
        let sse_body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"from sse\"}],\"isError\":false}}\n\n";
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE.as_str(), "text/event-stream")
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let mut rpc = McpRpc::new(http, format!("{}/mcp", server.uri()), "AT");
        let result = rpc
            .call_tool("inbox_latest", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.text, "from sse");
    }

    #[tokio::test]
    async fn initialize_stores_and_echoes_session_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(headers(
                ACCEPT.as_str(),
                vec!["application/json", "text/event-stream"],
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE.as_str(), "application/json")
                    .insert_header(SESSION_HEADER, "sess-123")
                    .set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}
                    })),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // The notifications/initialized follow-up gets an empty 202 body.
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(session_header_matcher("sess-123"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let mut rpc = McpRpc::new(http, format!("{}/mcp", server.uri()), "AT");
        rpc.initialize().await.unwrap();

        assert_eq!(rpc.session_id(), Some("sess-123"));
    }

    fn session_header_matcher(expected: &'static str) -> impl wiremock::Match {
        move |req: &Request| {
            req.headers
                .get(SESSION_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some(expected)
        }
    }

    #[tokio::test]
    async fn set_access_token_replaces_the_bearer_sent_on_the_next_call() {
        let server = MockServer::start().await;
        // Only a request bearing the NEW token succeeds; if `set_access_token`
        // didn't take effect, the client would still send the old one and get
        // no matching mock (wiremock's default 404 for an unmatched request).
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(headers(AUTHORIZATION.as_str(), vec!["Bearer NEW_AT"]))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE.as_str(), "application/json")
                    .set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "content": [{"type": "text", "text": "hello"}],
                            "isError": false
                        }
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let mut rpc = McpRpc::new(http, format!("{}/mcp", server.uri()), "OLD_AT");
        rpc.set_access_token("NEW_AT");
        let result = rpc
            .call_tool("inbox_latest", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.text, "hello");
    }

    #[test]
    fn parse_sse_events_joins_multiline_data_and_skips_unparsable() {
        // A multi-line `data:` field is reassembled by joining with "\n":
        // `{"a":\n1}` is still valid JSON, so this also proves the join
        // happened rather than each line being parsed on its own.
        let text = "data: {\"a\":\ndata: 1}\n\ndata: not json\n\ndata: {\"b\":2}\n\n";

        let events = parse_sse_events(text);

        assert_eq!(events, vec![json!({"a": 1}), json!({"b": 2})]);
    }

    #[tokio::test]
    async fn call_tool_selects_the_response_matching_the_request_id_amid_other_sse_events() {
        // The stream carries a server notification (no `id`) before our
        // response and a server-initiated request (a different `id`) after
        // it; the client must pick out only the event matching the id it
        // sent, not just take the last event in the stream.
        let sse_body = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n",
            "\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":",
            "[{\"type\":\"text\",\"text\":\"picked\"}],\"isError\":false}}\n",
            "\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"sampling/createMessage\",",
            "\"params\":{}}\n",
            "\n",
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE.as_str(), "text/event-stream")
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        // A fresh McpRpc's first call sends id 1, matching the response
        // event above (and not the notification or the id-99 event).
        let mut rpc = McpRpc::new(http, format!("{}/mcp", server.uri()), "AT");
        let result = rpc
            .call_tool("inbox_latest", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.text, "picked");
    }

    fn no_session_header() -> impl wiremock::Match {
        |req: &Request| req.headers.get(SESSION_HEADER).is_none()
    }

    fn rpc_method(name: &'static str) -> impl wiremock::Match {
        body_partial_json(json!({"method": name}))
    }

    /// Mount an `initialize` answer that hands out `session` (only for a
    /// request carrying no session id, as a fresh handshake must), plus a
    /// 202 for the `notifications/initialized` that follows on `session`.
    async fn mount_handshake(server: &MockServer, session: &'static str, times: u64) {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("initialize"))
            .and(no_session_header())
            .respond_with(move |req: &Request| {
                let id = serde_json::from_slice::<Value>(&req.body).unwrap()["id"].clone();
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE.as_str(), "application/json")
                    .insert_header(SESSION_HEADER, session)
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}
                    }))
            })
            .up_to_n_times(times)
            .expect(times)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("notifications/initialized"))
            .and(session_header_matcher(session))
            .respond_with(ResponseTemplate::new(202))
            .expect(times)
            .mount(server)
            .await;
    }

    fn echo_result(result: Value) -> impl Fn(&Request) -> ResponseTemplate {
        move |req: &Request| {
            let id = serde_json::from_slice::<Value>(&req.body).unwrap()["id"].clone();
            ResponseTemplate::new(200)
                .insert_header(CONTENT_TYPE.as_str(), "application/json")
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result.clone()}))
        }
    }

    fn session_not_found() -> ResponseTemplate {
        ResponseTemplate::new(404).set_body_string("Not Found: Session not found")
    }

    #[tokio::test]
    async fn call_tool_starts_a_new_session_and_retries_once_after_a_404() {
        let server = MockServer::start().await;
        mount_handshake(&server, "sess-old", 1).await;
        mount_handshake(&server, "sess-new", 1).await;
        // The idle-expired session answers 404, as rmcp's LocalSessionManager
        // does for an unknown `Mcp-Session-Id`.
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("tools/call"))
            .and(session_header_matcher("sess-old"))
            .respond_with(session_not_found())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("tools/call"))
            .and(session_header_matcher("sess-new"))
            .respond_with(echo_result(json!({
                "content": [{"type": "text", "text": "after retry"}],
                "isError": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut rpc = McpRpc::new(
            reqwest::Client::new(),
            format!("{}/mcp", server.uri()),
            "AT",
        );
        rpc.initialize().await.unwrap();
        let result = rpc.call_tool("accounts_list", json!({})).await.unwrap();

        assert_eq!(result.text, "after retry");
        assert_eq!(rpc.session_id(), Some("sess-new"));
    }

    #[tokio::test]
    async fn list_tools_starts_a_new_session_and_retries_once_after_a_404() {
        let server = MockServer::start().await;
        mount_handshake(&server, "sess-old", 1).await;
        mount_handshake(&server, "sess-new", 1).await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("tools/list"))
            .and(session_header_matcher("sess-old"))
            .respond_with(session_not_found())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("tools/list"))
            .and(session_header_matcher("sess-new"))
            .respond_with(echo_result(json!({"tools": [{"name": "accounts_list"}]})))
            .expect(1)
            .mount(&server)
            .await;

        let mut rpc = McpRpc::new(
            reqwest::Client::new(),
            format!("{}/mcp", server.uri()),
            "AT",
        );
        rpc.initialize().await.unwrap();
        let tools = rpc.list_tools().await.unwrap();

        assert_eq!(tools, vec!["accounts_list".to_string()]);
    }

    #[tokio::test]
    async fn a_second_404_after_the_new_session_is_returned() {
        let server = MockServer::start().await;
        mount_handshake(&server, "sess-old", 1).await;
        mount_handshake(&server, "sess-new", 1).await;
        // Both sessions 404: the client re-initializes exactly once (the
        // handshake mocks' `expect(1)`) and then surfaces the error.
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(rpc_method("tools/call"))
            .respond_with(session_not_found())
            .expect(2)
            .mount(&server)
            .await;

        let mut rpc = McpRpc::new(
            reqwest::Client::new(),
            format!("{}/mcp", server.uri()),
            "AT",
        );
        rpc.initialize().await.unwrap();
        let err = rpc.call_tool("accounts_list", json!({})).await.unwrap_err();

        assert!(
            matches!(err, ClientError::Graph { status: 404, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_404_without_a_session_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(session_not_found())
            .expect(1)
            .mount(&server)
            .await;

        let mut rpc = McpRpc::new(
            reqwest::Client::new(),
            format!("{}/mcp", server.uri()),
            "AT",
        );
        let err = rpc.call_tool("accounts_list", json!({})).await.unwrap_err();

        assert!(
            matches!(err, ClientError::Graph { status: 404, .. }),
            "{err:?}"
        );
    }
}
