//! Resolve user-typed attendee/recipient tokens against the local contacts
//! cache.
//!
//! Tokens follow this contract:
//! - Without a leading `@`, the token is treated as a literal email address
//!   and passed through unchanged. Existing behaviour for `--invite
//!   alice@x.com` is preserved exactly.
//! - With a leading `@`, the rest of the token is looked up in
//!   `ContactsCache`. Exact email matches win; otherwise a case-insensitive
//!   substring match runs over the email, its local-part, and the display
//!   name.
//!
//! Multi-match resolution **errors** rather than prompting — the agent-first
//! CLI design prefers deterministic failure with the candidate list over
//! interactive picking that breaks scripting.

use anyhow::{Result, anyhow};
use pidge_core::{Contact, ContactsCache};

/// Resolution outcome for a single token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// Token had no `@` prefix; passed through verbatim.
    Literal(String),
    /// Token matched exactly one contact.
    One(String),
    /// Token matched zero contacts.
    Unknown(String),
    /// Token matched more than one contact (most recent first, capped at 8).
    Ambiguous {
        token: String,
        candidates: Vec<Contact>,
    },
}

/// Pure resolution function. No I/O.
pub fn resolve_one(token: &str, cache: &ContactsCache) -> ResolveOutcome {
    let trimmed = token.trim();
    let Some(query) = trimmed.strip_prefix('@') else {
        return ResolveOutcome::Literal(trimmed.to_string());
    };
    let query_lc = query.to_lowercase();
    if query_lc.is_empty() {
        return ResolveOutcome::Unknown(token.to_string());
    }
    if let Some(c) = cache.by_email.get(&query_lc) {
        return ResolveOutcome::One(c.email.clone());
    }
    let mut matches: Vec<Contact> = cache
        .by_email
        .values()
        .filter(|c| contact_matches(c, &query_lc))
        .cloned()
        .collect();
    matches.sort_by_key(|c| std::cmp::Reverse(c.last_seen));
    match matches.len() {
        0 => ResolveOutcome::Unknown(token.to_string()),
        1 => ResolveOutcome::One(matches.remove(0).email),
        _ => ResolveOutcome::Ambiguous {
            token: token.to_string(),
            candidates: matches.into_iter().take(8).collect(),
        },
    }
}

/// Whether a contact matches a query under the same rules used by
/// `resolve_one` (case-insensitive substring on email, local-part, or name).
/// Exposed for `contacts find` to keep the predicate consistent.
pub fn contact_matches_public(c: &Contact, q: &str) -> bool {
    contact_matches(c, q)
}

fn contact_matches(c: &Contact, q: &str) -> bool {
    if c.email.to_lowercase().contains(q) {
        return true;
    }
    let local_part = c.email.split('@').next().unwrap_or("").to_lowercase();
    if local_part.contains(q) {
        return true;
    }
    c.display_name.to_lowercase().contains(q)
}

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
                "  {t} — unknown (run `pidge contacts refresh` to update the index)"
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
                problems.push(format!("  {token} — ambiguous: {}", names.join(", ")));
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
    fn token_without_at_prefix_passes_through_literally() {
        let cache = ContactsCache::default();
        assert_eq!(
            resolve_one("alice@x.com", &cache),
            ResolveOutcome::Literal("alice@x.com".into())
        );
    }

    #[test]
    fn exact_email_match_after_at_prefix_wins() {
        let cache = cache_with(&[
            ("dino@needefy.se", "Dino Semovic", 20, 0, 1),
            ("dino@elsewhere.com", "Dino Other", 19, 0, 1),
        ]);
        assert_eq!(
            resolve_one("@dino@needefy.se", &cache),
            ResolveOutcome::One("dino@needefy.se".into())
        );
    }

    #[test]
    fn substring_matches_display_name() {
        let cache = cache_with(&[("dino@needefy.se", "Dino Semovic", 20, 0, 1)]);
        assert_eq!(
            resolve_one("@dino", &cache),
            ResolveOutcome::One("dino@needefy.se".into())
        );
    }

    #[test]
    fn substring_matches_email_local_part() {
        let cache = cache_with(&[("bob.smith@x.com", "", 20, 0, 1)]);
        assert_eq!(
            resolve_one("@smith", &cache),
            ResolveOutcome::One("bob.smith@x.com".into())
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        let cache = cache_with(&[("dino@needefy.se", "Dino Semovic", 20, 0, 1)]);
        assert_eq!(
            resolve_one("@DINO", &cache),
            ResolveOutcome::One("dino@needefy.se".into())
        );
    }

    #[test]
    fn multiple_matches_return_ambiguous_with_recent_first() {
        let cache = cache_with(&[
            ("john.smith@a.com", "John Smith", 18, 0, 1),
            ("john.doe@b.com", "John Doe", 20, 0, 1),
        ]);
        let r = resolve_one("@john", &cache);
        match r {
            ResolveOutcome::Ambiguous { token, candidates } => {
                assert_eq!(token, "@john");
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].email, "john.doe@b.com");
                assert_eq!(candidates[1].email, "john.smith@a.com");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn no_match_returns_unknown_with_original_token() {
        let cache = ContactsCache::default();
        assert_eq!(
            resolve_one("@nope", &cache),
            ResolveOutcome::Unknown("@nope".into())
        );
    }

    #[test]
    fn bare_at_token_is_unknown() {
        let cache = cache_with(&[("a@b.com", "A", 20, 0, 1)]);
        assert_eq!(
            resolve_one("@", &cache),
            ResolveOutcome::Unknown("@".into())
        );
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
        assert!(err.contains("@john — ambiguous"));
        assert!(err.contains("@nope — unknown"));
    }
}
