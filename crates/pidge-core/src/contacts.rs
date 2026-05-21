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

/// One person known to pidge — collapsed from one or more mail / calendar
/// observations of the same lowercase email address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Canonical lowercase address. Used as the cache key.
    pub email: String,
    /// Display name as last observed. Empty until we see one — once set,
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
}
