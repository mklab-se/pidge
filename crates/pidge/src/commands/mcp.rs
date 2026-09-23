//! `pidge mcp ...` dispatcher — manage the connection to a hosted pidge MCP
//! server (see `crates/pidge-client/src/mcp/` for the client-side protocol
//! implementation this builds on).

use anyhow::Result;

use pidge_client::ClientError;

use crate::cli::McpCommands;
use crate::commands::{mcp_connect, mcp_logout, mcp_status};

pub async fn run(command: McpCommands, json: bool) -> Result<()> {
    match command {
        McpCommands::Connect { url, store, yes } => {
            mcp_connect::run(url, store.into(), yes, json).await
        }
        McpCommands::Status { url, store } => {
            mcp_status::run(url, store.map(Into::into), json).await
        }
        McpCommands::Logout { url, store } => {
            mcp_logout::run(url, store.map(Into::into), json).await
        }
    }
}

/// CLI-level errors about *which* hosted MCP server a command should act
/// on: no url given to `pidge mcp status` and nothing is connected, several
/// are connected and the caller must disambiguate, or an explicitly-named
/// server has no stored session at all. Kept separate from [`ClientError`]
/// (see [`ClientError::McpSessionExpired`] for the "has a session but it's
/// unrefreshable" case) since nothing in `pidge-client` needs to produce
/// these — they're purely about the CLI's own server bookkeeping.
#[derive(Debug, thiserror::Error)]
pub enum McpUsageError {
    #[error("no hosted server connected; run `pidge mcp connect <url>`")]
    NoServerConnected,

    /// `example` is a complete, ready-to-run command built by the caller
    /// (e.g. `"pidge mcp status https://a.example.com"`) — the message
    /// itself never hard-codes a subcommand name, since this enum is shared
    /// by every url-less resolution, not just `status`'s.
    #[error(
        "connected to multiple hosted servers ({servers}); pass the server url explicitly, e.g. `{example}`"
    )]
    AmbiguousServer { servers: String, example: String },

    #[error("not connected to {server}; run `pidge mcp connect {server}` to sign in")]
    NotConnected { server: String },
}

/// Remap a [`ClientError::SessionExpired`] found anywhere in `err`'s chain
/// into a [`ClientError::McpSessionExpired`] so both its message and
/// `exitcode::classify`'s hint point at `pidge mcp connect <url>` — the fix
/// for a hosted MCP session — instead of the hard-coded `pidge account add`
/// hint that's only correct for a Microsoft account's own session.
///
/// Every `pidge mcp` subcommand that can surface a hosted-session expiry
/// (a refresh failing outside the direct, typed handling already done in
/// `mcp_connect::ensure_signed_in`, `mcp_status::run`, or `mcp_logout::run`)
/// should route its top-level error through this before returning it to
/// `main`.
pub fn remap_session_expired(url: &str, err: anyhow::Error) -> anyhow::Error {
    let expired = err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ClientError>(),
            Some(ClientError::SessionExpired { .. })
        )
    });
    if expired {
        ClientError::McpSessionExpired {
            server: url.to_string(),
        }
        .into()
    } else {
        err
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_rewrites_session_expired_hint_to_mcp_connect() {
        let err = anyhow::Error::from(ClientError::SessionExpired {
            email: "https://mcp.example.com/mcp".into(),
        });
        let remapped = remap_session_expired("https://mcp.example.com", err);
        assert!(
            remapped
                .to_string()
                .contains("pidge mcp connect https://mcp.example.com"),
            "{remapped}"
        );
    }

    #[test]
    fn remap_produces_a_client_error_so_it_still_classifies_as_auth_expired() {
        let err = anyhow::Error::from(ClientError::SessionExpired {
            email: "https://mcp.example.com/mcp".into(),
        });
        let remapped = remap_session_expired("https://mcp.example.com", err);
        assert!(matches!(
            remapped.downcast_ref::<ClientError>(),
            Some(ClientError::McpSessionExpired { .. })
        ));
    }

    #[test]
    fn remap_leaves_other_errors_untouched() {
        let err = anyhow::anyhow!("some other failure");
        let remapped = remap_session_expired("https://mcp.example.com", err);
        assert_eq!(remapped.to_string(), "some other failure");
    }
}
