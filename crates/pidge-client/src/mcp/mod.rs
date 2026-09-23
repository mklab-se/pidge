//! Client for pidge's hosted remote MCP server.
//!
//! This is the counterpart to `pidge-mcp`'s own OAuth 2.1 authorization
//! server (see `crates/pidge-mcp/src/oauth/`): [`oauth::discover`] finds that
//! server's endpoints via RFC 9728 protected-resource metadata and RFC 8414
//! authorization-server metadata, [`oauth::register`] performs RFC 7591
//! dynamic client registration, and [`oauth::sign_in`] runs the browser
//! auth-code + PKCE flow (reusing the local-callback-server plumbing in
//! [`crate::auth::browser_flow`]). [`store::McpTokenStore`] persists the
//! resulting [`McpTokens`] the same way `pidge-client`'s Microsoft tokens are
//! stored — OS keychain by default, an opt-in plaintext file otherwise.
//! [`rpc::McpRpc`] is a minimal JSON-RPC client for the MCP `/mcp` endpoint
//! itself (Streamable HTTP transport: JSON or SSE bodies, session-id
//! stickiness).
//!
//! Nothing here knows about clap or terminal output: sign-in's `on_open`
//! callback is how a caller prints the authorize URL and (optionally) opens
//! a browser.

pub mod oauth;
pub mod rpc;
pub mod store;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub use oauth::{Discovery, discover, refresh, register, sign_in, valid_access_token};
pub use rpc::{McpRpc, ToolResult};
pub use store::McpTokenStore;

use crate::error::ClientError;

/// A signed-in session against one pidge MCP server.
///
/// `server` is the MCP endpoint URL (e.g. `https://mcp.example.com/mcp`) —
/// it doubles as the `resource` parameter on every authorize/token call and
/// as the key [`McpTokenStore`] persists under. `client_id` is the JWT the
/// server's dynamic-client-registration endpoint issued; it's re-sent on
/// every refresh grant since the server is a public (secret-less) client
/// registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTokens {
    pub server: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub client_id: String,
}

impl McpTokens {
    /// True if the access token is within 60 seconds of expiring (or already
    /// expired). Mirrors `auth::TokenSet::needs_refresh`.
    pub fn needs_refresh(&self) -> bool {
        Utc::now() + Duration::seconds(60) >= self.expires_at
    }
}

/// Normalize a server URL down to its origin (`scheme://host[:port]`),
/// dropping any path. Used both as the keychain account name and as the
/// basis for the on-disk file name — anything that identifies "which MCP
/// server" without caring which specific endpoint path was passed in.
pub(crate) fn normalize_origin(server_url: &str) -> Result<String, ClientError> {
    let url = url::Url::parse(server_url).map_err(|e| ClientError::Graph {
        status: 400,
        message: format!("invalid MCP server URL: {e}"),
    })?;
    if url.host_str().is_none() {
        return Err(ClientError::Graph {
            status: 400,
            message: "MCP server URL has no host".to_string(),
        });
    }
    Ok(url.origin().ascii_serialization())
}
