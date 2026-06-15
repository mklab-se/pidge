//! Best-effort cache of classifications keyed by message graph-id + prompt
//! hash. A corrupt or missing cache is ignored, never fatal.

// Items are pub and will be used by later tasks; suppress dead_code for now.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

/// Stable 16-hex-char key from a message id and the prompt text.
pub fn cache_key(graph_id: &str, prompt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(prompt.as_bytes());
    let digest = h.finalize();
    let hex = format!("{:x}", digest);
    format!("{graph_id}:{}", &hex[..16])
}

#[derive(Default)]
pub struct ClassifyCache {
    map: HashMap<String, Vec<String>>,
    path: Option<PathBuf>,
}

impl ClassifyCache {
    pub fn load() -> Self {
        Self::load_from(default_path())
    }

    pub fn load_from(path: Option<PathBuf>) -> Self {
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { map, path }
    }

    pub fn get(&self, key: &str) -> Option<Vec<String>> {
        self.map.get(key).cloned()
    }

    pub fn put(&mut self, key: String, labels: Vec<String>) {
        self.map.insert(key, labels);
    }

    pub fn save(&self) {
        if let Some(p) = &self.path {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string(&self.map) {
                let _ = std::fs::write(p, json);
            }
        }
    }
}

fn default_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("pidge").join("classify-cache.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_distinct() {
        let a = cache_key("MSG", "prompt one");
        let b = cache_key("MSG", "prompt one");
        let c = cache_key("MSG", "prompt two");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("MSG:"));
    }

    #[test]
    fn roundtrips_through_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("c.json");
        let mut c = ClassifyCache::load_from(Some(p.clone()));
        c.put("k1".into(), vec!["receipt".into()]);
        c.save();
        let c2 = ClassifyCache::load_from(Some(p));
        assert_eq!(c2.get("k1"), Some(vec!["receipt".to_string()]));
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let c = ClassifyCache::load_from(Some(PathBuf::from("/nonexistent/x.json")));
        assert!(c.get("anything").is_none());
    }
}
