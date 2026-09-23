//! `pidge mcp ...` dispatcher — manage the connection to a hosted pidge MCP
//! server (see `crates/pidge-client/src/mcp/` for the client-side protocol
//! implementation this builds on).

use anyhow::{Result, anyhow};

use pidge_client::ClientError;

use crate::cli::McpCommands;
use crate::commands::mcp_connect;

pub async fn run(command: McpCommands, json: bool) -> Result<()> {
    match command {
        McpCommands::Connect { url, store, yes } => {
            mcp_connect::run(url, store.into(), yes, json).await
        }
        McpCommands::Status => Err(anyhow!("`pidge mcp status` is not implemented yet")),
        McpCommands::Logout => Err(anyhow!("`pidge mcp logout` is not implemented yet")),
    }
}

/// Remap a [`ClientError::SessionExpired`] found anywhere in `err`'s chain
/// so its hint points at `pidge mcp connect <url>` — the fix for a hosted
/// MCP session — instead of `exitcode::classify`'s hard-coded `pidge account
/// add` hint, which is only correct for a Microsoft account's own session.
///
/// Every `pidge mcp` subcommand that can surface a hosted-session expiry
/// (a refresh failing outside the direct, typed handling already done in
/// `mcp_connect::ensure_signed_in`) should route its top-level error through
/// this before returning it to `main`.
pub fn remap_session_expired(url: &str, err: anyhow::Error) -> anyhow::Error {
    let expired = err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ClientError>(),
            Some(ClientError::SessionExpired { .. })
        )
    });
    if expired {
        anyhow!(
            "session expired for the hosted pidge server. Run `pidge mcp connect {url}` to sign in again."
        )
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
    fn remap_leaves_other_errors_untouched() {
        let err = anyhow::anyhow!("some other failure");
        let remapped = remap_session_expired("https://mcp.example.com", err);
        assert_eq!(remapped.to_string(), "some other failure");
    }
}
