//! Local name → email index built from the user's own mail and calendar.
//!
//! The cache lives at `${XDG_CACHE_HOME:-~/.cache}/pidge/contacts.json` and
//! mirrors the I/O patterns of `MessageCache` / `EventCache` (atomic write,
//! lazy load, schema-tolerant via `#[serde(default)]`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// One person known to pidge, collapsed from one or more mail / calendar
/// observations of the same lowercase email address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Canonical lowercase address. Used as the cache key.
    pub email: String,
    /// Display name as last observed. Empty until we see one; once set,
    /// it is only replaced by another non-empty observation.
    #[serde(default)]
    pub display_name: String,
    /// Most recent `received_at` (mail) or `start.at` (calendar) we saw.
    pub last_seen: DateTime<Utc>,
    /// How many inbox messages mentioned this address as the sender.
    #[serde(default)]
    pub seen_in_mail: u32,
    /// How many calendar events mentioned this address as organizer or
    /// attendee.
    #[serde(default)]
    pub seen_in_calendar: u32,
}

/// JSON-backed contact cache. Keyed by lowercase email.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ContactsCache {
    #[serde(default)]
    pub by_email: HashMap<String, Contact>,
    #[serde(default)]
    pub last_refreshed: Option<DateTime<Utc>>,
}

/// Where a contact observation came from. Determines which `seen_in_*`
/// counter gets incremented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactSource {
    Mail,
    Calendar,
}

impl ContactsCache {
    /// `${XDG_CACHE_HOME:-~/.cache}/pidge/contacts.json`.
    pub fn default_path() -> Result<PathBuf, CoreError> {
        let dir = dirs::cache_dir()
            .ok_or(CoreError::NoConfigDir)?
            .join("pidge");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("contacts.json"))
    }

    pub fn load() -> Result<Self, CoreError> {
        Self::load_from(&Self::default_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let cache: ContactsCache = serde_json::from_str(&text)
            .map_err(|e| CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        Ok(cache)
    }

    pub fn save(&self) -> Result<(), CoreError> {
        self.save_to(&Self::default_path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), CoreError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Insert one observation. Email is lowercased; display name is only
    /// applied when non-empty (we never overwrite a known name with `""`).
    /// `last_seen` advances to the later of the existing and new values so
    /// out-of-order refreshes converge to the right state.
    pub fn upsert(
        &mut self,
        email: &str,
        display_name: &str,
        seen_at: DateTime<Utc>,
        source: ContactSource,
    ) {
        let email = email.trim().to_lowercase();
        if email.is_empty() {
            return;
        }
        let entry = self
            .by_email
            .entry(email.clone())
            .or_insert_with(|| Contact {
                email: email.clone(),
                display_name: String::new(),
                last_seen: seen_at,
                seen_in_mail: 0,
                seen_in_calendar: 0,
            });
        let name = display_name.trim();
        if !name.is_empty() {
            entry.display_name = name.to_string();
        }
        if seen_at > entry.last_seen {
            entry.last_seen = seen_at;
        }
        match source {
            ContactSource::Mail => entry.seen_in_mail = entry.seen_in_mail.saturating_add(1),
            ContactSource::Calendar => {
                entry.seen_in_calendar = entry.seen_in_calendar.saturating_add(1)
            }
        }
    }

    pub fn mark_refreshed(&mut self, at: DateTime<Utc>) {
        self.last_refreshed = Some(at);
    }

    /// Resolve a token the way the MCP surface does: names don't need the
    /// CLI's `@` prefix convention. A token containing `@` followed by a
    /// `.` in the domain part is treated as a literal address; anything
    /// else (with or without a leading `@`) is looked up as a name.
    pub fn resolve_any(&self, token: &str) -> ResolveOutcome {
        let t = token.trim();
        let looks_like_address = t.split_once('@').is_some_and(|(_, d)| d.contains('.'));
        if looks_like_address {
            return ResolveOutcome::Literal(t.to_string());
        }
        let lookup = format!("@{}", t.trim_start_matches('@'));
        resolve_one(&lookup, self)
    }
}

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
///
/// Tokens follow this contract:
/// - Without a leading `@`, the token is treated as a literal email address
///   and passed through unchanged. Existing behaviour for `--invite
///   alice@x.com` is preserved exactly.
/// - With a leading `@`, the rest of the token is looked up in
///   `ContactsCache`. Exact email matches win; otherwise a case-insensitive
///   substring match runs over the email, its local-part, and the display
///   name.
///
/// Multi-match resolution **errors** rather than prompting; the agent-first
/// CLI design prefers deterministic failure with the candidate list over
/// interactive picking that breaks scripting.
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
pub fn contact_matches(c: &Contact, q: &str) -> bool {
    if c.email.to_lowercase().contains(q) {
        return true;
    }
    let local_part = c.email.split('@').next().unwrap_or("").to_lowercase();
    if local_part.contains(q) {
        return true;
    }
    c.display_name.to_lowercase().contains(q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn default_cache_is_empty() {
        let c = ContactsCache::default();
        assert!(c.by_email.is_empty());
        assert!(c.last_refreshed.is_none());
    }

    #[test]
    fn upsert_inserts_new_contact() {
        let mut c = ContactsCache::default();
        c.upsert(
            "Dino@Needefy.SE",
            "Dino Semovic",
            dt(2026, 5, 21),
            ContactSource::Calendar,
        );
        let entry = c.by_email.get("dino@needefy.se").expect("inserted");
        assert_eq!(entry.email, "dino@needefy.se");
        assert_eq!(entry.display_name, "Dino Semovic");
        assert_eq!(entry.seen_in_calendar, 1);
        assert_eq!(entry.seen_in_mail, 0);
    }

    #[test]
    fn upsert_merges_by_lowercase_email() {
        let mut c = ContactsCache::default();
        c.upsert("Bob@x.com", "Bob B.", dt(2026, 5, 20), ContactSource::Mail);
        c.upsert("bob@X.com", "Bob B.", dt(2026, 5, 21), ContactSource::Mail);
        assert_eq!(c.by_email.len(), 1);
        let entry = c.by_email.get("bob@x.com").unwrap();
        assert_eq!(entry.seen_in_mail, 2);
    }

    #[test]
    fn upsert_keeps_latest_last_seen_regardless_of_order() {
        let mut c = ContactsCache::default();
        c.upsert("a@b.com", "A", dt(2026, 5, 21), ContactSource::Mail);
        c.upsert("a@b.com", "A", dt(2026, 5, 10), ContactSource::Mail);
        assert_eq!(
            c.by_email.get("a@b.com").unwrap().last_seen,
            dt(2026, 5, 21)
        );
    }

    #[test]
    fn upsert_preserves_name_when_new_is_empty() {
        let mut c = ContactsCache::default();
        c.upsert("a@b.com", "Alice", dt(2026, 5, 20), ContactSource::Mail);
        c.upsert("a@b.com", "", dt(2026, 5, 21), ContactSource::Mail);
        assert_eq!(c.by_email.get("a@b.com").unwrap().display_name, "Alice");
    }

    #[test]
    fn upsert_updates_name_when_new_provided() {
        let mut c = ContactsCache::default();
        c.upsert("a@b.com", "Alice", dt(2026, 5, 20), ContactSource::Mail);
        c.upsert(
            "a@b.com",
            "Alice Andersson",
            dt(2026, 5, 21),
            ContactSource::Mail,
        );
        assert_eq!(
            c.by_email.get("a@b.com").unwrap().display_name,
            "Alice Andersson"
        );
    }

    #[test]
    fn upsert_skips_empty_email() {
        let mut c = ContactsCache::default();
        c.upsert("", "Ghost", dt(2026, 5, 21), ContactSource::Mail);
        c.upsert("   ", "Whitespace", dt(2026, 5, 21), ContactSource::Mail);
        assert!(c.by_email.is_empty());
    }

    #[test]
    fn cache_roundtrips_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.json");
        let mut c = ContactsCache::default();
        c.upsert("x@y.com", "X Y", dt(2026, 5, 21), ContactSource::Calendar);
        c.mark_refreshed(dt(2026, 5, 21));
        c.save_to(&path).unwrap();
        let loaded = ContactsCache::load_from(&path).unwrap();
        assert_eq!(loaded.by_email.len(), 1);
        assert_eq!(loaded.last_refreshed, Some(dt(2026, 5, 21)));
        assert_eq!(loaded.by_email.get("x@y.com").unwrap().display_name, "X Y");
    }

    fn cache_with(entries: &[(&str, &str, u32, u32, u32)]) -> ContactsCache {
        let mut c = ContactsCache::default();
        for (email, name, day, _mail_count, _cal_count) in entries {
            c.upsert(email, name, dt(2026, 5, *day), ContactSource::Calendar);
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
    fn resolve_any_treats_bare_names_as_lookups() {
        let mut c = ContactsCache::default();
        c.upsert(
            "anna@example.com",
            "Anna Holmberg",
            Utc::now(),
            ContactSource::Mail,
        );
        assert_eq!(
            c.resolve_any("anna"),
            ResolveOutcome::One("anna@example.com".into())
        );
        assert_eq!(
            c.resolve_any("bob@example.org"),
            ResolveOutcome::Literal("bob@example.org".into())
        );
        assert!(matches!(
            c.resolve_any("nobody"),
            ResolveOutcome::Unknown(_)
        ));
    }
}
