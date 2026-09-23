//! `pidge mcp logout <url>` — delete the stored session for a hosted pidge
//! MCP server from every backend that holds it, and drop its server-index
//! entry. Idempotent: logging out of a server with no stored session is not
//! an error.

use anyhow::Result;
use serde_json::json;

use pidge_client::mcp::McpTokenStore;
use pidge_core::TokenStorage;

use crate::commands::mcp_session::{candidate_backends, indexed_backend, try_each_backend};

pub async fn run(url: String, store: Option<TokenStorage>, json_output: bool) -> Result<()> {
    // `--store` is a preference order, not a restriction (see mcp_status):
    // the named (or indexed) backend is checked first, but the other one is
    // still tried, since a stale index or a wrong guess must not hide a
    // session that's actually there.
    let preferred = store.unwrap_or_else(|| indexed_backend(&url));
    let candidates = candidate_backends(preferred);

    let present = try_each_backend(&candidates, |backend| {
        McpTokenStore::load(&url, backend).map(|tokens| tokens.is_some())
    })?;
    let removed = present.iter().any(|(_, found)| *found);

    // Always attempt to delete from every backend, regardless of what the
    // presence check found: `delete` is already a no-op for an absent
    // entry, and this also clears a stale index entry whose backend no
    // longer actually holds anything (see task-3-review.md I4).
    try_each_backend(&candidates, |backend| McpTokenStore::delete(&url, backend))?;

    if json_output {
        println!("{}", json!({ "server": url, "removed": removed }));
    } else if removed {
        println!("Signed out of {url} and removed its stored session.");
    } else {
        println!("No stored session found for {url}; nothing to remove.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `run` itself is all I/O (real backends), so these exercise the
    // decision it's built from directly: does "found in any backend" come
    // out right, and does the preferred-vs-fallback ordering `logout`
    // shares with `status` behave the way I3/I4 call for.

    #[test]
    fn removed_is_true_when_any_backend_has_a_hit() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let present = try_each_backend(&candidates, |backend| {
            Ok::<_, anyhow::Error>(backend == TokenStorage::File)
        })
        .unwrap();
        assert!(present.iter().any(|(_, found)| *found));
    }

    #[test]
    fn removed_is_false_when_no_backend_has_a_hit() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let present = try_each_backend(&candidates, |_| Ok::<_, anyhow::Error>(false)).unwrap();
        assert!(!present.iter().any(|(_, found)| *found));
    }

    #[test]
    fn a_fallback_backend_error_does_not_hide_a_hit_on_the_preferred_one() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let present = try_each_backend(&candidates, |backend| match backend {
            TokenStorage::Keychain => Ok::<bool, anyhow::Error>(true),
            TokenStorage::File => Err(anyhow::anyhow!("no secret service running")),
        })
        .unwrap();
        assert!(present.iter().any(|(_, found)| *found));
    }
}
