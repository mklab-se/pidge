//! `pidge mcp logout <url>` — delete the stored session for a hosted pidge
//! MCP server, from whichever backend holds it. Idempotent: logging out of
//! a server with no stored session is not an error.

use anyhow::Result;
use serde_json::json;

use pidge_client::mcp::{McpTokenStore, normalize_origin};
use pidge_core::TokenStorage;

use crate::commands::mcp_connect::{backend_name, candidate_backends};

pub async fn run(url: String, store: Option<TokenStorage>, json_output: bool) -> Result<()> {
    let backend = find_backend(&url, store)?;

    if let Some(backend) = backend {
        McpTokenStore::delete(&url, backend)?;
    }

    if json_output {
        println!("{}", json!({ "server": url, "removed": backend.is_some() }));
    } else if let Some(backend) = backend {
        println!(
            "Signed out of {url} (removed the {} session).",
            backend_name(backend)
        );
    } else {
        println!("No stored session found for {url}; nothing to remove.");
    }

    Ok(())
}

/// Which backend (if any) currently holds a session for `url`. With
/// `override_store` set, only that backend is consulted. Otherwise, prefers
/// the server index (`McpTokenStore::list`) — a hit there means no need to
/// touch the OS keychain at all when the session turns out to be a plain
/// file — falling back to trying both backends directly if the index has no
/// matching entry (it may predate this feature, or have gone stale).
fn find_backend(url: &str, override_store: Option<TokenStorage>) -> Result<Option<TokenStorage>> {
    if let Some(s) = override_store {
        return Ok(McpTokenStore::load(url, s)?.map(|_| s));
    }

    let origin = normalize_origin(url)?;
    if let Some(indexed) = McpTokenStore::list()?
        .into_iter()
        .find(|e| e.server == origin)
        && McpTokenStore::load(url, indexed.storage)?.is_some()
    {
        return Ok(Some(indexed.storage));
    }

    for backend in candidate_backends(TokenStorage::Keychain) {
        if McpTokenStore::load(url, backend)?.is_some() {
            return Ok(Some(backend));
        }
    }
    Ok(None)
}
