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
}
