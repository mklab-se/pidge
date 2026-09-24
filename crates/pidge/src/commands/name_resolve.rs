//! Resolve user-typed attendee/recipient tokens against the local contacts
//! cache.
//!
//! The resolution rules and predicate live in `pidge_core::contacts` (shared
//! with the MCP server); this module re-exports them for CLI callers and
//! keeps the CLI-specific batch helper that aggregates failures.

use anyhow::{Result, anyhow};
use pidge_core::ContactsCache;
pub use pidge_core::contacts::{
    ResolveOutcome, contact_matches as contact_matches_public, resolve_one,
};

/// Resolve a list of tokens against the loaded cache. Aggregates all
/// failures into a single error so the user sees every problem token in
/// one pass instead of "fix one → rerun → discover the next".
pub fn resolve_addresses(tokens: &[String], cache: &ContactsCache) -> Result<Vec<String>> {
    let mut resolved: Vec<String> = Vec::with_capacity(tokens.len());
    let mut problems: Vec<String> = Vec::new();
    for token in tokens {
        match resolve_one(token, cache) {
            ResolveOutcome::Literal(s) | ResolveOutcome::One(s) => resolved.push(s),
            ResolveOutcome::Unknown(t) => problems.push(format!(
                "  {t}: unknown (run `pidge contacts refresh` to update the index)"
            )),
            ResolveOutcome::Ambiguous { token, candidates } => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|c| {
                        if c.display_name.is_empty() {
                            c.email.clone()
                        } else {
                            format!("{} <{}>", c.display_name, c.email)
                        }
                    })
                    .collect();
                problems.push(format!("  {token}: ambiguous: {}", names.join(", ")));
            }
        }
    }
    if !problems.is_empty() {
        return Err(anyhow!(
            "Could not resolve {} attendee/recipient token{}:\n{}",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" },
            problems.join("\n")
        ));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use pidge_core::ContactSource;

    fn cache_with(entries: &[(&str, &str, i32, u32, u32)]) -> ContactsCache {
        let mut c = ContactsCache::default();
        for (email, name, day, _mail_count, _cal_count) in entries {
            let ts = Utc
                .with_ymd_and_hms(2026, 5, *day as u32, 12, 0, 0)
                .unwrap();
            c.upsert(email, name, ts, ContactSource::Calendar);
        }
        c
    }

    #[test]
    fn resolve_addresses_aggregates_literals_and_lookups() {
        let cache = cache_with(&[("dino@needefy.se", "Dino Semovic", 20, 0, 1)]);
        let out = resolve_addresses(&["@dino".into(), "literal@x.com".into()], &cache).unwrap();
        assert_eq!(out, vec!["dino@needefy.se", "literal@x.com"]);
    }

    #[test]
    fn resolve_addresses_reports_every_failure_at_once() {
        let cache = cache_with(&[
            ("john.smith@a.com", "John Smith", 18, 0, 1),
            ("john.doe@b.com", "John Doe", 20, 0, 1),
        ]);
        let err = resolve_addresses(&["@john".into(), "@nope".into()], &cache)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Could not resolve 2"));
        assert!(err.contains("@john: ambiguous"));
        assert!(err.contains("@nope: unknown"));
    }
}
