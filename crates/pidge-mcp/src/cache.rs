//! Per-user, in-memory read cache. Short-lived (60 s TTL) and bounded (an
//! LRU per user), so it never needs to be persisted or explicitly sized for
//! more than the busiest single user. A write clears its owner's whole
//! cache rather than tracking which reads it might invalidate.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lru::LruCache;
use serde::Serialize;
use serde_json::Value;

/// A user's cached values, most-recently-used first.
type UserCache = LruCache<String, (Instant, String)>;

pub struct ReadCache {
    inner: Mutex<HashMap<String, UserCache>>,
    ttl: Duration,
    per_user: usize,
}

impl ReadCache {
    pub fn new(ttl: Duration, per_user: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
            per_user,
        }
    }

    /// A cache key for `tool` called with `args`, independent of the order
    /// fields were set in (JSON objects are canonicalised before stringifying).
    #[allow(dead_code)] // used by the mail and calendar read tools (Tasks 10+)
    pub fn key(tool: &str, args: &impl Serialize) -> String {
        let value = serde_json::to_value(args).unwrap_or(Value::Null);
        format!("{tool}\n{}", canonicalize(value))
    }

    /// The cached value for `user`'s `key`, or `None` if absent or expired.
    #[allow(dead_code)] // used via context::cached by the read tools (Tasks 10+)
    pub fn get(&self, user: &str, key: &str) -> Option<String> {
        let mut inner = self.inner.lock().expect("read cache lock");
        let cache = inner.get_mut(user)?;
        let (inserted, value) = cache.get(key)?.clone();
        if inserted.elapsed() > self.ttl {
            cache.pop(key);
            return None;
        }
        Some(value)
    }

    #[allow(dead_code)] // used via context::cached by the read tools (Tasks 10+)
    pub fn put(&self, user: &str, key: String, value: String) {
        let mut inner = self.inner.lock().expect("read cache lock");
        let cache = inner.entry(user.to_string()).or_insert_with(|| {
            LruCache::new(NonZeroUsize::new(self.per_user).expect("per_user must be > 0"))
        });
        cache.put(key, (Instant::now(), value));
    }

    /// Drops everything cached for `user`, e.g. after one of their writes.
    pub fn invalidate_user(&self, user: &str) {
        let mut inner = self.inner.lock().expect("read cache lock");
        inner.remove(user);
    }
}

/// Recursively sorts object keys (via a `BTreeMap`) so two JSON values that
/// differ only in field order stringify identically. `preserve_order` keeps
/// `serde_json::Map` in insertion order otherwise, so this can't be skipped.
#[allow(dead_code)] // used by ReadCache::key, itself unused until Tasks 10+
fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, canonicalize(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_until_ttl_then_miss() {
        let c = ReadCache::new(Duration::from_millis(50), 8);
        c.put("u", "k".into(), "v".into());
        assert_eq!(c.get("u", "k").as_deref(), Some("v"));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(c.get("u", "k"), None);
    }

    #[test]
    fn users_are_isolated_and_invalidation_is_per_user() {
        let c = ReadCache::new(Duration::from_secs(60), 8);
        c.put("a", "k".into(), "va".into());
        c.put("b", "k".into(), "vb".into());
        assert_eq!(c.get("b", "k").as_deref(), Some("vb"));
        c.invalidate_user("a");
        assert_eq!(c.get("a", "k"), None);
        assert_eq!(c.get("b", "k").as_deref(), Some("vb"));
    }

    #[test]
    fn key_is_order_independent() {
        assert_eq!(
            ReadCache::key("t", &serde_json::json!({"b":1,"a":2})),
            ReadCache::key("t", &serde_json::json!({"a":2,"b":1}))
        );
    }
}
