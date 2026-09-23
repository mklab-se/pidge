//! `pidge mcp logout <url>` — delete the stored session for a hosted pidge
//! MCP server from every backend that holds it, and drop its server-index
//! entry. Idempotent: logging out of a server with no stored session is not
//! an error. `--dry-run` reports what would be removed without touching
//! anything.

use anyhow::Result;
use serde_json::json;

use pidge_client::mcp::McpTokenStore;
use pidge_core::TokenStorage;

use crate::commands::mcp_session::{
    backend_name, candidate_backends, preferred_backend_for, try_each_backend,
};

pub async fn run(url: String, store: Option<TokenStorage>, json_output: bool) -> Result<()> {
    // `--store` is a preference order, not a restriction (see mcp_status):
    // the named (or indexed) backend is checked first, but the other one is
    // still tried, since a stale index or a wrong guess must not hide a
    // session that's actually there. `McpTokenStore::list` is only read
    // when `--store` didn't already settle the question.
    let preferred = match store {
        Some(s) => s,
        None => preferred_backend_for(&url, &McpTokenStore::list()?),
    };
    let candidates = candidate_backends(preferred);

    if crate::guardrail::dry_run_active() {
        return run_dry(&url, &candidates, json_output);
    }

    let removed = logout_from(
        &candidates,
        |backend| McpTokenStore::load(&url, backend).map(|tokens| tokens.is_some()),
        |backend| McpTokenStore::delete(&url, backend),
    )?;

    if json_output {
        println!("{}", json!({ "server": url, "removed": removed }));
    } else if removed {
        println!("Signed out of {url} and removed its stored session.");
    } else {
        println!("No stored session found for {url}; nothing to remove.");
    }

    Ok(())
}

/// `--dry-run`: report which backends (if any) hold a session for `url`,
/// touching nothing — no delete, and no index write. Mirrors `logout_from`'s
/// backend ordering/tolerance (via [`try_each_backend`]) but only ever
/// calls `load`.
fn run_dry(url: &str, candidates: &[TokenStorage], json_output: bool) -> Result<()> {
    let present = try_each_backend(candidates, |backend| {
        McpTokenStore::load(url, backend).map(|tokens| tokens.is_some())
    })?;
    let found: Vec<TokenStorage> = present
        .into_iter()
        .filter_map(|(backend, found)| found.then_some(backend))
        .collect();

    if json_output {
        println!(
            "{}",
            json!({
                "dry_run": true,
                "server": url,
                "would_remove": !found.is_empty(),
                "backends": found.iter().map(|b| backend_name(*b)).collect::<Vec<_>>(),
            })
        );
    } else if found.is_empty() {
        println!("Dry run: no stored session found for {url}; nothing would be removed.");
    } else {
        let names = found
            .iter()
            .map(|b| backend_name(*b))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "Dry run: would sign out of {url}, removing its session from: {names} (and its server-index entry)."
        );
    }
    Ok(())
}

/// The full logout decision for one server. For every one of `candidates`,
/// in one pass per backend (so an unreachable backend is only reported —
/// and warned about, via [`try_each_backend`] — once, not once per phase):
/// check whether a session is present, then delete it regardless of what
/// was found there. Deleting unconditionally, even on a miss, is what
/// clears a stale index entry whose backend no longer actually holds
/// anything (task-3-review.md I4). Returns whether a session was found in
/// *any* backend. Generic over injectable `load`/`delete` so this is
/// directly unit-testable with a fake, with no real keychain/file I/O.
fn logout_from<E: std::fmt::Display>(
    candidates: &[TokenStorage],
    mut load: impl FnMut(TokenStorage) -> Result<bool, E>,
    mut delete: impl FnMut(TokenStorage) -> Result<(), E>,
) -> Result<bool, E> {
    let results = try_each_backend(candidates, |backend| {
        let found = load(backend)?;
        delete(backend)?;
        Ok::<bool, E>(found)
    })?;
    Ok(results.into_iter().any(|(_, found)| found))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn logout_from_deletes_every_backend_even_when_nothing_was_found() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let deleted = RefCell::new(Vec::new());

        let removed = logout_from(
            &candidates,
            |_| Ok::<bool, anyhow::Error>(false),
            |backend| {
                deleted.borrow_mut().push(backend);
                Ok::<(), anyhow::Error>(())
            },
        )
        .unwrap();

        assert!(!removed);
        assert_eq!(
            deleted.into_inner(),
            vec![TokenStorage::Keychain, TokenStorage::File],
            "delete must run on every candidate, not stop after a miss"
        );
    }

    #[test]
    fn logout_from_reports_removed_when_any_backend_had_a_session() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let removed = logout_from(
            &candidates,
            |backend| Ok::<bool, anyhow::Error>(backend == TokenStorage::File),
            |_| Ok::<(), anyhow::Error>(()),
        )
        .unwrap();
        assert!(removed);
    }

    #[test]
    fn logout_from_still_deletes_the_preferred_backend_when_it_was_the_hit() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let deleted = RefCell::new(Vec::new());

        let removed = logout_from(
            &candidates,
            |backend| Ok::<bool, anyhow::Error>(backend == TokenStorage::Keychain),
            |backend| {
                deleted.borrow_mut().push(backend);
                Ok::<(), anyhow::Error>(())
            },
        )
        .unwrap();

        assert!(removed);
        assert_eq!(
            deleted.into_inner(),
            vec![TokenStorage::Keychain, TokenStorage::File]
        );
    }

    #[test]
    fn logout_from_propagates_an_error_from_the_preferred_backend() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let err = logout_from(
            &candidates,
            |_| Err::<bool, _>(anyhow::anyhow!("keychain unavailable")),
            |_| Ok::<(), anyhow::Error>(()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("keychain unavailable"));
    }

    #[test]
    fn logout_from_tolerates_a_fallback_backend_error() {
        let candidates = candidate_backends(TokenStorage::Keychain);
        let removed = logout_from(
            &candidates,
            |backend| match backend {
                TokenStorage::Keychain => Ok::<bool, anyhow::Error>(true),
                TokenStorage::File => Err(anyhow::anyhow!("no secret service running")),
            },
            |_| Ok::<(), anyhow::Error>(()),
        )
        .unwrap();
        assert!(
            removed,
            "a fallback-backend error must not hide a hit on the preferred one"
        );
    }
}
