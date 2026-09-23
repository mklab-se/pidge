//! Minimal JSON-RPC client for the MCP `/mcp` endpoint (Streamable HTTP
//! transport, as served by `rmcp`'s `StreamableHttpService` in
//! `pidge-mcp`'s `crates/pidge-mcp/src/app.rs`).
//!
//! Each call is a single `POST` carrying one JSON-RPC request, with
//! `Accept: application/json, text/event-stream` — the server may answer
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
/// concatenated. `is_error` mirrors the JSON-RPC result's `isError` flag —
/// tool errors come back as a normal (non-JSON-RPC-error) result with this
/// set, per the MCP spec.
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

/// A JSON-RPC session against one MCP server. Not thread-safe by itself —
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

    /// Run the MCP `initialize` handshake and send the required
    /// `notifications/initialized` follow-up. Stores the session id the
    /// server returns, if any, for subsequent calls.
    pub async fn initialize(&mut self) -> Result<(), ClientError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "pidge", "version": env!("CARGO_PKG_VERSION")},
        });
        self.request("initialize", params).await?;
        self.notify("notifications/initialized", json!({})).await
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
    /// JSON-RPC `error` response or a non-2xx HTTP status.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});

        let (session_id, value) = self.send(&body).await?;
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
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), ClientError> {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let (session_id, _) = self.send(&body).await?;
        if session_id.is_some() {
            self.session_id = session_id;
        }
        Ok(())
    }

    /// POST one JSON-RPC frame and parse whatever comes back — JSON or SSE.
    /// Returns the server's `Mcp-Session-Id` (if present) and the decoded
    /// JSON-RPC message (if the body wasn't empty, as for notifications).
    async fn send(&self, body: &Value) -> Result<(Option<String>, Option<Value>), ClientError> {
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
            parse_sse_last_data(&text)?
        } else {
            Some(serde_json::from_str(&text)?)
        };
        Ok((session_id, value))
    }
}

fn header_to_str(v: &HeaderValue) -> Option<&str> {
    v.to_str().ok()
}

/// Parse an SSE body and return the JSON-decoded payload of its last `data:`
/// line — the Streamable HTTP transport sends one event per JSON-RPC
/// message, and a single request yields exactly one response event.
fn parse_sse_last_data(text: &str) -> Result<Option<Value>, ClientError> {
    let mut last = None;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        last = Some(serde_json::from_str(data)?);
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{headers, method, path};
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
}
