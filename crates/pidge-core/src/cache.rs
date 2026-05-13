//! Persistent cache mapping short message hashes to Microsoft Graph IDs.
//!
//! Pidge identifies messages by an 8-char hex short hash derived from `sha256` of
//! the Graph ID. The full Graph ID is opaque (~100+ characters) and not human-typable;
//! the short hash is intended for `pidge inbox show <fragment>` substring lookup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMessageRef {
    pub graph_id: String,
    pub account: String,
    pub cached_at: DateTime<Utc>,
}

/// Result of looking up a fragment against the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    NotFound,
    One(String, CachedMessageRef),
    Ambiguous(Vec<(String, CachedMessageRef)>),
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
        let dir = dirs::cache_dir().ok_or(CoreError::NoConfigDir)?.join("pidge");
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
        let cache: MessageCache = serde_json::from_str(&text).map_err(|e| {
            CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        Ok(cache)
    }

    pub fn save(&self) -> Result<(), CoreError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), CoreError> {
        let text = serde_json::to_string_pretty(self).map_err(|e| {
            CoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
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
}
