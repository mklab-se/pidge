//! Persistent cache mapping short message hashes to Microsoft Graph IDs.
//!
//! Pidge identifies messages by an 8-char hex short hash derived from `sha256` of
//! the Graph ID. The full Graph ID is opaque (~100+ characters) and not human-typable;
//! the short hash is intended for `pidge mail show <fragment>` substring lookup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMessageRef {
    pub graph_id: String,
    pub account: String,
    pub cached_at: DateTime<Utc>,
}

/// Typed fragment-resolution failure, used for exit-code classification
/// (agents get exit 4 + a machine-readable code instead of prose-only).
#[derive(Debug, thiserror::Error)]
pub enum FragmentError {
    #[error("no match found for fragment '{fragment}'")]
    NotFound { fragment: String },

    #[error("fragment '{fragment}' matches {count} entries — provide more characters")]
    Ambiguous { fragment: String, count: usize },
}

/// Result of looking up a fragment against the cache.
///
/// Generic over the cached entry type so the same enum serves both
/// `MessageCache` (default: `CachedMessageRef`) and `EventCache`
/// (`CacheLookup<CachedEventRef>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup<T = CachedMessageRef> {
    NotFound,
    One(String, T),
    /// Two or more entries match the fragment.
    ///
    /// The inner vec is capped at 10 entries by `find_by_fragment`
    /// to keep error messages readable. If more than 10 entries actually match,
    /// only the first 10 are surfaced — callers should ask the user for a
    /// longer fragment rather than try to enumerate all candidates.
    Ambiguous(Vec<(String, T)>),
}

/// Compute the 8-char hex short hash for a Graph ID.
/// Deterministic: same input -> same output. Stable across pidge runs and machines.
pub fn short_hash(graph_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(graph_id.as_bytes());
    let result = h.finalize();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        result[0], result[1], result[2], result[3]
    )
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::CoreError;

const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MessageCache {
    #[serde(default)]
    pub entries: HashMap<String, CachedMessageRef>,
}

impl MessageCache {
    /// Default path: `${XDG_CACHE_HOME:-~/.cache}/pidge/messages.json`.
    pub fn default_path() -> Result<PathBuf, CoreError> {
        let dir = dirs::cache_dir()
            .ok_or(CoreError::NoConfigDir)?
            .join("pidge");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("messages.json"))
    }

    /// Load the cache from the default path. Missing file -> empty cache.
    pub fn load() -> Result<Self, CoreError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let cache: MessageCache = serde_json::from_str(&text)
            .map_err(|e| CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        Ok(cache)
    }

    pub fn save(&self) -> Result<(), CoreError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), CoreError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Insert messages and evict oldest entries if over MAX_ENTRIES.
    /// Each tuple: (graph_id, account_email). Hashes are computed here.
    pub fn insert_many(&mut self, msgs: &[(String, String)]) {
        let now = Utc::now();
        for (graph_id, account) in msgs {
            let hash = short_hash(graph_id);
            self.entries.insert(
                hash,
                CachedMessageRef {
                    graph_id: graph_id.clone(),
                    account: account.clone(),
                    cached_at: now,
                },
            );
        }
        self.evict_oldest_if_needed();
    }

    fn evict_oldest_if_needed(&mut self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let excess = self.entries.len() - MAX_ENTRIES;
        let mut sorted: Vec<(String, DateTime<Utc>)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.cached_at))
            .collect();
        sorted.sort_by_key(|(_, t)| *t);
        for (k, _) in sorted.into_iter().take(excess) {
            self.entries.remove(&k);
        }
    }

    /// Find a message by a fragment of its short hash. The fragment may be a
    /// prefix, suffix, or any contiguous substring of the 8-char hash.
    /// Empty fragment is treated as NotFound.
    pub fn find_by_fragment(&self, fragment: &str) -> CacheLookup {
        if fragment.is_empty() {
            return CacheLookup::NotFound;
        }
        let mut matches: Vec<(String, CachedMessageRef)> = self
            .entries
            .iter()
            .filter(|(k, _)| k.contains(fragment))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        match matches.len() {
            0 => CacheLookup::NotFound,
            1 => {
                let (k, v) = matches.remove(0);
                CacheLookup::One(k, v)
            }
            _ => CacheLookup::Ambiguous(matches.into_iter().take(10).collect()),
        }
    }
}

const MAX_EVENT_ENTRIES: usize = 1000;

/// Cached pointer to a Microsoft Graph event. Keyed in the cache by
/// `short_hash("{account}|{event_id}")` so the same event ID under
/// different signed-in accounts gets distinct hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedEventRef {
    pub event_id: String,
    pub calendar_id: String,
    pub account: String,
    pub cached_at: DateTime<Utc>,
}

impl CachedEventRef {
    pub fn new(event_id: String, calendar_id: String, account: String) -> Self {
        Self {
            event_id,
            calendar_id,
            account,
            cached_at: Utc::now(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EventCache {
    #[serde(default)]
    pub entries: HashMap<String, CachedEventRef>,
}

impl EventCache {
    /// Default path: `${XDG_CACHE_HOME:-~/.cache}/pidge/events.json`.
    pub fn default_path() -> Result<PathBuf, CoreError> {
        let dir = dirs::cache_dir()
            .ok_or(CoreError::NoConfigDir)?
            .join("pidge");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("events.json"))
    }

    pub fn load() -> Result<Self, CoreError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let cache: EventCache = serde_json::from_str(&text)
            .map_err(|e| CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        Ok(cache)
    }

    pub fn save(&self) -> Result<(), CoreError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), CoreError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Insert event refs and evict oldest entries if over MAX_EVENT_ENTRIES.
    pub fn insert_many(&mut self, events: &[CachedEventRef]) {
        let now = Utc::now();
        for e in events {
            let key = format!("{}|{}", e.account, e.event_id);
            let hash = short_hash(&key);
            let mut entry = e.clone();
            entry.cached_at = now;
            self.entries.insert(hash, entry);
        }
        self.evict_oldest_if_needed();
    }

    fn evict_oldest_if_needed(&mut self) {
        if self.entries.len() <= MAX_EVENT_ENTRIES {
            return;
        }
        let excess = self.entries.len() - MAX_EVENT_ENTRIES;
        let mut sorted: Vec<(String, DateTime<Utc>)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.cached_at))
            .collect();
        sorted.sort_by_key(|(_, t)| *t);
        for (k, _) in sorted.into_iter().take(excess) {
            self.entries.remove(&k);
        }
    }

    pub fn find_by_fragment(&self, fragment: &str) -> CacheLookup<CachedEventRef> {
        if fragment.is_empty() {
            return CacheLookup::NotFound;
        }
        let mut matches: Vec<(String, CachedEventRef)> = self
            .entries
            .iter()
            .filter(|(k, _)| k.contains(fragment))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        match matches.len() {
            0 => CacheLookup::NotFound,
            1 => {
                let (k, v) = matches.remove(0);
                CacheLookup::One(k, v)
            }
            _ => CacheLookup::Ambiguous(matches.into_iter().take(10).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_is_deterministic() {
        let a = short_hash("AAMkAGI2TG93AAA=");
        let b = short_hash("AAMkAGI2TG93AAA=");
        assert_eq!(a, b);
    }

    #[test]
    fn short_hash_is_8_hex_chars() {
        let h = short_hash("anything");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn short_hash_differs_for_different_inputs() {
        assert_ne!(short_hash("x"), short_hash("y"));
    }

    #[test]
    fn empty_cache_roundtrips_through_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("messages.json");

        let cache = MessageCache::default();
        cache.save_to(&path).unwrap();
        let loaded = MessageCache::load_from(&path).unwrap();
        assert_eq!(loaded.entries.len(), 0);
    }

    #[test]
    fn populated_cache_roundtrips_through_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("messages.json");

        let mut cache = MessageCache::default();
        cache.insert_many(&[
            ("AAA".to_string(), "user@example.com".to_string()),
            ("BBB".to_string(), "user@example.com".to_string()),
        ]);
        cache.save_to(&path).unwrap();

        let loaded = MessageCache::load_from(&path).unwrap();
        assert_eq!(loaded.entries.len(), 2);
        let hash_aaa = short_hash("AAA");
        let entry = loaded.entries.get(&hash_aaa).unwrap();
        assert_eq!(entry.graph_id, "AAA");
        assert_eq!(entry.account, "user@example.com");
    }

    #[test]
    fn load_from_missing_file_returns_empty_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let cache = MessageCache::load_from(&path).unwrap();
        assert_eq!(cache.entries.len(), 0);
    }

    #[test]
    fn insert_many_evicts_oldest_when_over_max() {
        let mut cache = MessageCache::default();

        // Pre-populate with MAX_ENTRIES old entries
        let now = Utc::now();
        let old_time = now - chrono::Duration::seconds(3600);
        for i in 0..MAX_ENTRIES {
            let hash = format!("{:08x}", i);
            cache.entries.insert(
                hash,
                CachedMessageRef {
                    graph_id: format!("old-{}", i),
                    account: "user@example.com".into(),
                    cached_at: old_time,
                },
            );
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);

        // Insert one new entry — should evict one old entry
        cache.insert_many(&[("new-graph-id".into(), "user@example.com".into())]);
        assert_eq!(cache.entries.len(), MAX_ENTRIES);

        // The new entry must be present
        let new_hash = short_hash("new-graph-id");
        assert!(cache.entries.contains_key(&new_hash));
    }

    fn cache_with(entries: &[(&str, &str)]) -> MessageCache {
        let mut cache = MessageCache::default();
        let pairs: Vec<(String, String)> = entries
            .iter()
            .map(|(g, a)| (g.to_string(), a.to_string()))
            .collect();
        cache.insert_many(&pairs);
        cache
    }

    #[test]
    fn find_by_fragment_returns_not_found_when_no_match() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        assert!(matches!(
            cache.find_by_fragment("zzzzzz"),
            CacheLookup::NotFound
        ));
    }

    #[test]
    fn find_by_fragment_returns_not_found_for_empty_fragment() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        assert!(matches!(cache.find_by_fragment(""), CacheLookup::NotFound));
    }

    #[test]
    fn find_by_fragment_returns_one_for_exact_match() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        let hash = short_hash("hello");
        match cache.find_by_fragment(&hash) {
            CacheLookup::One(h, _) => assert_eq!(h, hash),
            other => panic!("expected One, got {other:?}"),
        }
    }

    #[test]
    fn find_by_fragment_matches_prefix() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        let hash = short_hash("hello");
        let prefix = &hash[..3];
        assert!(matches!(
            cache.find_by_fragment(prefix),
            CacheLookup::One(_, _)
        ));
    }

    #[test]
    fn find_by_fragment_matches_suffix() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        let hash = short_hash("hello");
        let suffix = &hash[hash.len() - 3..];
        assert!(matches!(
            cache.find_by_fragment(suffix),
            CacheLookup::One(_, _)
        ));
    }

    #[test]
    fn find_by_fragment_matches_middle_substring() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        let hash = short_hash("hello");
        let middle = &hash[2..5];
        assert!(matches!(
            cache.find_by_fragment(middle),
            CacheLookup::One(_, _)
        ));
    }

    #[test]
    fn event_cache_roundtrips_through_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("events.json");

        let mut cache = EventCache::default();
        cache.insert_many(&[CachedEventRef::new(
            "evt-1".into(),
            "cal-1".into(),
            "u@e.com".into(),
        )]);
        cache.save_to(&path).unwrap();

        let loaded = EventCache::load_from(&path).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        let h = short_hash("u@e.com|evt-1");
        let e = loaded.entries.get(&h).unwrap();
        assert_eq!(e.event_id, "evt-1");
        assert_eq!(e.calendar_id, "cal-1");
        assert_eq!(e.account, "u@e.com");
    }

    #[test]
    fn event_hash_differs_for_different_accounts() {
        let a = short_hash("user-a@x|same-event");
        let b = short_hash("user-b@x|same-event");
        assert_ne!(a, b);
    }

    #[test]
    fn event_cache_find_by_fragment_returns_one_on_exact_match() {
        let mut cache = EventCache::default();
        cache.insert_many(&[CachedEventRef::new(
            "evt-1".into(),
            "cal-1".into(),
            "u@e.com".into(),
        )]);
        let h = short_hash("u@e.com|evt-1");
        match cache.find_by_fragment(&h) {
            CacheLookup::One(found, _) => assert_eq!(found, h),
            other => panic!("expected One, got {other:?}"),
        }
    }

    #[test]
    fn event_cache_load_from_missing_file_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("none.json");
        let cache = EventCache::load_from(&path).unwrap();
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn event_cache_evicts_oldest_when_over_max() {
        let mut cache = EventCache::default();
        let now = Utc::now();
        let old = now - chrono::Duration::seconds(3600);
        for i in 0..MAX_EVENT_ENTRIES {
            let hash = format!("{:08x}", i);
            cache.entries.insert(
                hash,
                CachedEventRef {
                    event_id: format!("old-{i}"),
                    calendar_id: "cal".into(),
                    account: "u@e.com".into(),
                    cached_at: old,
                },
            );
        }
        cache.insert_many(&[CachedEventRef::new(
            "new".into(),
            "cal".into(),
            "u@e.com".into(),
        )]);
        assert_eq!(cache.entries.len(), MAX_EVENT_ENTRIES);
        let new_hash = short_hash("u@e.com|new");
        assert!(cache.entries.contains_key(&new_hash));
    }

    #[test]
    fn find_by_fragment_returns_ambiguous_when_two_hashes_share_fragment() {
        let mut cache = MessageCache::default();
        let mut entries = Vec::new();
        for i in 0..200 {
            entries.push((format!("graph-{i}"), "u@e.com".to_string()));
        }
        cache.insert_many(&entries);

        let hashes: Vec<String> = cache.entries.keys().cloned().collect();
        let mut found_ambiguous = false;
        'outer: for h in &hashes {
            for start in 0..h.len() - 1 {
                let frag = &h[start..start + 2];
                let count = hashes.iter().filter(|other| other.contains(frag)).count();
                if count >= 2 {
                    if let CacheLookup::Ambiguous(matches) = cache.find_by_fragment(frag) {
                        assert!(matches.len() >= 2);
                        found_ambiguous = true;
                        break 'outer;
                    }
                }
            }
        }
        assert!(
            found_ambiguous,
            "expected at least one ambiguous fragment in 200-entry cache"
        );
    }
}
