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
pub use store::{McpTokenStore, StoredServer};

use crate::error::ClientError;

/// A signed-in session against one pidge MCP server.
///
/// `server` is the canonical MCP resource URL (`Discovery::resource` at the
/// time of sign-in/refresh, e.g. `https://mcp.example.com/mcp`) — it doubles
/// as the `resource` parameter on every authorize/token/refresh call and as
/// the key [`McpTokenStore`] persists under. It's the value to pass back in
/// as `mcp_url` to [`oauth::valid_access_token`] and as the URL
/// [`rpc::McpRpc`] posts to. `client_id` is the JWT the server's
/// dynamic-client-registration endpoint issued; it's re-sent on every
/// refresh grant since the server is a public (secret-less) client
/// registry.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTokens {
    pub server: String,
    /// Bearer token sent as `Authorization: Bearer <access_token>` on `/mcp`
    /// calls (see [`rpc::McpRpc`]).
    pub access_token: String,
    /// Redeemed for a new access/refresh pair via [`oauth::refresh`] once
    /// `access_token` is within 60 seconds of `expires_at`.
    pub refresh_token: String,
    /// When `access_token` expires; see [`Self::needs_refresh`].
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

/// Hand-written so `access_token`/`refresh_token` are never printed by an
/// incidental `{:?}` — this struct is `pub` and handled by the CLI, so a
/// stray debug print (a log line, a test failure message, …) shouldn't leak
/// bearer credentials.
impl std::fmt::Debug for McpTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTokens")
            .field("server", &self.server)
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("client_id", &self.client_id)
            .finish()
    }
}

/// Normalize a server URL down to its origin (`scheme://host[:port]`),
/// dropping any path. Used both as the keychain account name and as the
/// basis for the on-disk file name — anything that identifies "which MCP
/// server" without caring which specific endpoint path was passed in. `pub`
/// so callers (e.g. `pidge mcp logout`) can compare a user-typed url against
/// [`store::StoredServer::server`], which is always stored in this form.
pub fn normalize_origin(server_url: &str) -> Result<String, ClientError> {
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
    // The session's bearer and refresh tokens travel to this origin on
    // every call; plain http is only for a server on this machine.
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(ClientError::Graph {
            status: 400,
            message: "MCP server URL must use https (http is allowed for localhost only)"
                .to_string(),
        });
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls_must_be_https_except_on_loopback() {
        assert_eq!(
            normalize_origin("https://pidge.example.com/mcp").unwrap(),
            "https://pidge.example.com"
        );
        assert_eq!(
            normalize_origin("http://localhost:8080/mcp").unwrap(),
            "http://localhost:8080"
        );
        assert!(normalize_origin("http://127.0.0.1:8080/mcp").is_ok());
        let err = normalize_origin("http://pidge.example.com/mcp").unwrap_err();
        assert!(err.to_string().contains("https"), "{err}");
    }

    #[test]
    fn debug_redacts_both_tokens() {
        let tokens = McpTokens {
            server: "https://mcp.example.com/mcp".into(),
            access_token: "super-secret-access".into(),
            refresh_token: "super-secret-refresh".into(),
            expires_at: Utc::now(),
            client_id: "client-jwt".into(),
        };

        let debug = format!("{tokens:?}");

        assert!(!debug.contains("super-secret-access"));
        assert!(!debug.contains("super-secret-refresh"));
        assert!(debug.contains("<redacted>"));
        // Non-secret fields still show up, so a debug print stays useful.
        assert!(debug.contains("mcp.example.com"));
        assert!(debug.contains("client-jwt"));
    }
}
