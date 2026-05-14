# Output refresh + message-ID UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refresh `pidge inbox list` with a clean Scandinavian layout (subject + preview, no bullet column, OSC 8 URL hyperlinks), introduce a global `--json` flag across data commands, and give every message a stable 8-char hex short hash plus a substring-lookup cache so future `pidge inbox show <fragment>` is friction-free.

**Architecture:** Three new external workspace deps (`linkify`, `sha2`, plus extending `comfy-table` with the `custom_styling` feature). One new `pidge-core` module (`cache.rs` for `MessageCache` + LRU + substring lookup). One new `pidge` module (`output/` for OSC 8 hyperlink + linkify-text helpers). `Cli` gains a global `--json` flag, dispatch threads it through to leaf commands. `inbox list` gains `--compact`/`-c`, computes a `short_hash` per row, persists the cache, and renders rich (default) or compact (with flag).

**Tech Stack:** Rust 2024 edition, `comfy-table` with `custom_styling` for ANSI-aware width math, `linkify` for URL detection, `sha2` for stable short hashes, OSC 8 escape sequences for clickable URLs.

**Reference spec:** `docs/superpowers/specs/2026-05-14-output-refresh-and-message-id-design.md`

**Working directory:** `/Users/kristofer/repos/mklab-se/pidge`

---

## File inventory

### New
```
crates/pidge-core/src/cache.rs              # MessageCache, CachedMessageRef, CacheLookup
crates/pidge/src/output/mod.rs              # re-exports
crates/pidge/src/output/hyperlink.rs        # OSC 8 helper
crates/pidge/src/output/linkify.rs          # URL detection + wrap
```

### Modified
```
Cargo.toml                                   # add linkify + sha2 deps; extend comfy-table features
crates/pidge-core/Cargo.toml                 # add sha2.workspace = true
crates/pidge-core/src/lib.rs                 # re-export cache types
crates/pidge/Cargo.toml                      # linkify.workspace = true
crates/pidge/src/main.rs                     # mod output;
crates/pidge/src/cli.rs                      # add global --json; remove --output + OutputFormat; add --compact
crates/pidge/src/commands/auth.rs            # plumb json
crates/pidge/src/commands/auth_list.rs       # honor --json
crates/pidge/src/commands/auth_status.rs    # honor --json
crates/pidge/src/commands/inbox.rs           # render rich/compact/json; cache integration
CHANGELOG.md                                 # [Unreleased] entries
```

---

## Task 1: Add workspace dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Read current root `Cargo.toml`**

Run: `cat /Users/kristofer/repos/mklab-se/pidge/Cargo.toml | head -90`

Note where the existing `comfy-table = "7"` and other `[workspace.dependencies]` entries live.

- [ ] **Step 2: Edit root `Cargo.toml`**

In `/Users/kristofer/repos/mklab-se/pidge/Cargo.toml`, find the existing line:

```toml
comfy-table = "7"
```

Replace with:

```toml
comfy-table = { version = "7", features = ["custom_styling"] }
```

Then add these two new entries, alphabetically placed (or simply appended after the comfy-table line if alphabetical sort isn't enforced):

```toml
linkify = "0.10"
sha2 = "0.10"
```

- [ ] **Step 3: Verify the workspace still builds**

Run: `cargo build --workspace 2>&1 | tail -3`
Expected: clean Finished message, no warnings. Cargo will fetch `linkify` and `sha2` and the additional comfy-table feature.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "Add linkify, sha2, and comfy-table custom_styling for output refresh"
```

---

## Task 2: `pidge-core::cache::MessageCache` skeleton + short_hash

**Files:**
- Create: `crates/pidge-core/src/cache.rs`
- Modify: `crates/pidge-core/Cargo.toml`
- Modify: `crates/pidge-core/src/lib.rs`

The cache module — first with just the types and the deterministic short-hash function. LRU and find-by-fragment land in later tasks.

- [ ] **Step 1: Add `sha2` to `pidge-core`'s dependencies**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/Cargo.toml`. In `[dependencies]`, add:

```toml
sha2.workspace = true
```

- [ ] **Step 2: Create `cache.rs` with types and `short_hash`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/cache.rs`:

```rust
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
```

- [ ] **Step 3: Re-export from `pidge-core::lib`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/lib.rs`. After the existing `mod` lines, add:

```rust
mod cache;
```

And in the `pub use` block, add:

```rust
pub use cache::{short_hash, CacheLookup, CachedMessageRef};
```

The full `lib.rs` should now look like:

```rust
//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.

mod account;
mod cache;
mod config;
mod error;
mod message;

pub use account::Account;
pub use cache::{short_hash, CacheLookup, CachedMessageRef};
pub use config::{Config, Defaults};
pub use error::CoreError;
pub use message::{Message, MessageFrom};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pidge-core cache`
Expected: 3 passing.

Then run the full crate to make sure nothing regressed:
Run: `cargo test -p pidge-core 2>&1 | tail -3`
Expected: `14 passed` (11 pre-existing + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-core/Cargo.toml crates/pidge-core/src/cache.rs crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core short_hash + cache type skeleton"
```

---

## Task 3: `MessageCache` struct with load/save and `insert_many`

**Files:**
- Modify: `crates/pidge-core/src/cache.rs`

Persistent cache file at `~/.cache/pidge/messages.json`. Load/save tolerates missing file. `insert_many` adds entries with current timestamp and evicts oldest entries when over `MAX_ENTRIES`.

- [ ] **Step 1: Append `MessageCache` struct + impl to `cache.rs`**

Add to `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/cache.rs`. Insert this block AFTER the existing `pub fn short_hash` but BEFORE `#[cfg(test)]`:

```rust
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
```

Also remove the existing `use chrono::Utc;` (if present) and replace with a single `use chrono::{DateTime, Utc};` near the top of the file (since both are now used). The existing `use chrono::{DateTime, Utc};` line should already exist from Task 2.

- [ ] **Step 2: Add tests for load/save and insert_many**

Append to the `#[cfg(test)] mod tests` block in `cache.rs`:

```rust
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
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-core cache`
Expected: 7 passing (3 from Task 2 + 4 new).

- [ ] **Step 4: Re-export `MessageCache` from `lib.rs`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/lib.rs` to also re-export `MessageCache`. The pub use line becomes:

```rust
pub use cache::{short_hash, CacheLookup, CachedMessageRef, MessageCache};
```

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-core/src/cache.rs crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core MessageCache with load/save and LRU eviction"
```

---

## Task 4: `MessageCache::find_by_fragment` substring lookup

**Files:**
- Modify: `crates/pidge-core/src/cache.rs`

- [ ] **Step 1: Add `find_by_fragment` method**

Inside the `impl MessageCache { … }` block in `crates/pidge-core/src/cache.rs`, after `evict_oldest_if_needed`, add:

```rust
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
```

- [ ] **Step 2: Add tests for find_by_fragment**

Append to the `#[cfg(test)] mod tests` block:

```rust
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
        assert!(matches!(cache.find_by_fragment("zzzzzz"), CacheLookup::NotFound));
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
        assert!(matches!(cache.find_by_fragment(prefix), CacheLookup::One(_, _)));
    }

    #[test]
    fn find_by_fragment_matches_suffix() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        let hash = short_hash("hello");
        let suffix = &hash[hash.len() - 3..];
        assert!(matches!(cache.find_by_fragment(suffix), CacheLookup::One(_, _)));
    }

    #[test]
    fn find_by_fragment_matches_middle_substring() {
        let cache = cache_with(&[("hello", "u@e.com")]);
        let hash = short_hash("hello");
        let middle = &hash[2..5];
        assert!(matches!(cache.find_by_fragment(middle), CacheLookup::One(_, _)));
    }

    #[test]
    fn find_by_fragment_returns_ambiguous_when_two_hashes_share_fragment() {
        // Search 100 hashes and find a pair that share a fragment.
        // We do this with construction: pick two graph_ids whose short hashes
        // share at least 2 chars somewhere; this almost always succeeds quickly.
        let mut cache = MessageCache::default();
        let mut entries = Vec::new();
        for i in 0..200 {
            entries.push((format!("graph-{i}"), "u@e.com".to_string()));
        }
        cache.insert_many(&entries);

        // Find any 2-char fragment that appears in 2+ hashes.
        // Iterate fragments and assert we find at least one ambiguous result.
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
        assert!(found_ambiguous, "expected at least one ambiguous fragment in 200-entry cache");
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-core cache`
Expected: 14 passing (7 + 7 new).

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-core/src/cache.rs
git commit -m "Add MessageCache::find_by_fragment with substring matching"
```

---

## Task 5: `pidge/src/output/hyperlink.rs` OSC 8 helper

**Files:**
- Modify: `crates/pidge/Cargo.toml`
- Create: `crates/pidge/src/output/mod.rs`
- Create: `crates/pidge/src/output/hyperlink.rs`
- Modify: `crates/pidge/src/main.rs`

OSC 8 hyperlink helper and a place to put the linkify wrapper (next task).

- [ ] **Step 1: Add `linkify` to `pidge` dependencies**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/Cargo.toml`. In `[dependencies]`, add:

```toml
linkify.workspace = true
```

- [ ] **Step 2: Create `output/hyperlink.rs`**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/output`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/output/hyperlink.rs`:

```rust
//! OSC 8 hyperlink helper.
//!
//! Modern terminals interpret the OSC 8 escape sequence as a clickable hyperlink:
//!     ESC ] 8 ; ; URL ST TEXT ESC ] 8 ; ; ST
//! where ST is the String Terminator (ESC \).
//!
//! Terminals that don't understand OSC 8 strip the escape sequences and render
//! only the visible `text`, so there's no visual breakage on legacy terminals.
//!
//! Reference: https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda

const OSC8_START: &str = "\x1b]8;;";
const ST: &str = "\x1b\\";

/// Wrap `text` so clicking it in a supporting terminal opens `url`.
pub fn hyperlink(url: &str, text: &str) -> String {
    format!("{OSC8_START}{url}{ST}{text}{OSC8_START}{ST}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyperlink_wraps_text_with_url() {
        let out = hyperlink("https://example.com", "click me");
        assert!(out.contains("https://example.com"));
        assert!(out.contains("click me"));
        assert!(out.starts_with("\x1b]8;;"));
        assert!(out.ends_with("\x1b\\"));
    }

    #[test]
    fn hyperlink_url_appears_before_text() {
        let out = hyperlink("https://example.com", "click me");
        let url_pos = out.find("https://example.com").unwrap();
        let text_pos = out.find("click me").unwrap();
        assert!(url_pos < text_pos);
    }
}
```

- [ ] **Step 3: Create `output/mod.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/output/mod.rs`:

```rust
//! Output formatting utilities.

pub mod hyperlink;

pub use hyperlink::hyperlink;
```

(The `linkify` submodule and its re-export are added in Task 6.)

- [ ] **Step 4: Declare `output` module in `main.rs`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/main.rs`. Find the existing `mod` declarations near the top (under the existing `mod banner;`, `mod cli;`, `mod commands;`, `mod update;`). Add:

```rust
mod output;
```

Alphabetical order is preserved: banner, cli, commands, output, update.

- [ ] **Step 5: Run tests and build**

Run: `cargo build -p pidge 2>&1 | tail -3`
Expected: clean.

Run: `cargo test -p pidge hyperlink`
Expected: 2 passing.

- [ ] **Step 6: Commit**

```bash
git add crates/pidge/Cargo.toml crates/pidge/src/main.rs crates/pidge/src/output
git commit -m "Add pidge output module with OSC 8 hyperlink helper"
```

---

## Task 6: `pidge/src/output/linkify.rs` URL detection + wrap

**Files:**
- Create: `crates/pidge/src/output/linkify.rs`
- Modify: `crates/pidge/src/output/mod.rs`

- [ ] **Step 1: Write the linkify module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/output/linkify.rs`:

```rust
//! Scan text for URLs and wrap each with an OSC 8 hyperlink.

use linkify::{LinkFinder, LinkKind};

use crate::output::hyperlink::hyperlink;

/// Find URLs in `text` and wrap each with an OSC 8 link pointing at the URL itself.
/// Text without URLs is returned unchanged. Only HTTP/HTTPS-style URLs are wrapped;
/// email addresses (mailto:) are NOT wrapped.
pub fn linkify_text(text: &str) -> String {
    let finder = LinkFinder::new();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for link in finder.links(text) {
        if !matches!(link.kind(), LinkKind::Url) {
            continue;
        }
        let (start, end) = (link.start(), link.end());
        out.push_str(&text[cursor..start]);
        out.push_str(&hyperlink(link.as_str(), link.as_str()));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_returns_unchanged() {
        let input = "no urls here, just text";
        assert_eq!(linkify_text(input), input);
    }

    #[test]
    fn single_url_gets_wrapped() {
        let out = linkify_text("see https://example.com today");
        assert!(out.contains("\x1b]8;;https://example.com\x1b\\"));
        assert!(out.contains("see "));
        assert!(out.contains(" today"));
    }

    #[test]
    fn multiple_urls_each_get_wrapped() {
        let out = linkify_text("first https://a.com and second https://b.com end");
        // Both URLs should appear inside OSC 8 sequences.
        let osc8_count = out.matches("\x1b]8;;").count();
        // Each URL produces 2 osc8 prefixes (start + close).
        assert_eq!(osc8_count, 4, "got: {out:?}");
    }

    #[test]
    fn email_addresses_are_not_wrapped() {
        let input = "contact me at hello@example.com";
        let out = linkify_text(input);
        // No OSC 8 escapes since email isn't URL.
        assert!(!out.contains("\x1b]8;;"));
        assert!(out.contains("hello@example.com"));
    }
}
```

- [ ] **Step 2: Re-export from `output/mod.rs`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/output/mod.rs`:

```rust
//! Output formatting utilities.

pub mod hyperlink;
pub mod linkify;

pub use hyperlink::hyperlink;
pub use linkify::linkify_text;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge linkify`
Expected: 4 passing.

Run: `cargo test -p pidge 2>&1 | tail -3`
Expected: 8 passing total (2 banner + 2 inbox + 2 hyperlink + 4 linkify; rounded count if previous batches added more).

- [ ] **Step 4: Commit**

```bash
git add crates/pidge/src/output/linkify.rs crates/pidge/src/output/mod.rs
git commit -m "Add linkify_text helper that OSC 8-wraps URLs in arbitrary text"
```

---

## Task 7: CLI changes — global `--json`, `--compact`, remove `--output`

**Files:**
- Modify: `crates/pidge/src/cli.rs`

- [ ] **Step 1: Read the current `cli.rs`**

Run: `head -110 /Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/cli.rs`

Identify the current `Cli` struct, the `InboxCommands::List` variant, and the `OutputFormat` enum.

- [ ] **Step 2: Modify `Cli` struct to add `json` global flag**

In `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/cli.rs`, find the `Cli` struct. Add a new field after the existing `no_color` field:

```rust
    /// Output as machine-readable JSON instead of formatted text
    #[arg(long, global = true)]
    pub json: bool,
```

The struct becomes:

```rust
pub struct Cli {
    /// Increase output verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Output as machine-readable JSON instead of formatted text
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}
```

- [ ] **Step 3: Replace `InboxCommands::List`'s `output` field with `compact`**

Find the `InboxCommands` enum in `cli.rs`. The current variant is:

```rust
#[derive(clap::Subcommand)]
pub enum InboxCommands {
    /// List messages in the inbox, merged across all signed-in accounts
    List {
        #[arg(long)]
        account: Vec<String>,

        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        #[arg(long)]
        unread: bool,

        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
}
```

Replace it with:

```rust
#[derive(clap::Subcommand)]
pub enum InboxCommands {
    /// List messages in the inbox, merged across all signed-in accounts
    List {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// Maximum number of messages to show
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// Show only unread messages
        #[arg(long)]
        unread: bool,

        /// One row per message (no preview lines)
        #[arg(short = 'c', long)]
        compact: bool,
    },
}
```

- [ ] **Step 4: Remove the `OutputFormat` enum**

Find and DELETE this block in `cli.rs`:

```rust
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}
```

- [ ] **Step 5: Update `Cli::run` to thread `json` into command handlers**

Find the `impl Cli { pub async fn run(self) -> Result<()> }` block. Change the match arms for `Auth` and `Inbox` to pass `self.json`:

```rust
            Some(Commands::Auth { command }) => crate::commands::auth::run(command, self.json).await,
            Some(Commands::Inbox { command }) => crate::commands::inbox::run(command, self.json).await,
```

Leave the other arms (`Ai`, `Completion`, `Version`, `None`) unchanged.

- [ ] **Step 6: Build will fail — that's expected**

`cargo build -p pidge` will now fail because `commands::auth::run` and `commands::inbox::run` don't yet accept a `json` parameter. We'll fix those in Tasks 8–10. No action this step.

- [ ] **Step 7: No commit yet**

This batch commits together with Tasks 8 and 9 when the CLI is wired end-to-end.

---

## Task 8: Auth commands honor `--json`

**Files:**
- Modify: `crates/pidge/src/commands/auth.rs`
- Modify: `crates/pidge/src/commands/auth_list.rs`
- Modify: `crates/pidge/src/commands/auth_status.rs`

- [ ] **Step 1: Update `auth.rs` dispatcher signature**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth.rs` entirely with:

```rust
//! `pidge auth ...` dispatcher.

use anyhow::Result;

use crate::cli::AuthCommands;
use crate::commands::{auth_default, auth_list, auth_login, auth_logout, auth_status};

pub async fn run(command: AuthCommands, json: bool) -> Result<()> {
    match command {
        AuthCommands::Login => auth_login::run().await,
        AuthCommands::List => auth_list::run(json),
        AuthCommands::Status => auth_status::run(json),
        AuthCommands::Logout { account, all, yes } => auth_logout::run(account, all, yes),
        AuthCommands::Default { send, calendar } => auth_default::run(send, calendar),
    }
}
```

`auth_login`, `auth_logout`, `auth_default` are interactive — they don't get `json`. `auth_list` and `auth_status` are data-output commands and do.

- [ ] **Step 2: Update `auth_list.rs` to emit JSON when requested**

Replace the entire contents of `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_list.rs` with:

```rust
//! `pidge auth list` — display signed-in accounts.

use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use serde::Serialize;

use pidge_core::Config;

pub fn run(json: bool) -> Result<()> {
    let config = Config::load()?;

    if json {
        return emit_json(&config);
    }

    if config.accounts.is_empty() {
        println!(
            "No accounts signed in. Run {} to add one.",
            "`pidge auth login`".cyan()
        );
        return Ok(());
    }

    let mut table = Table::new();
    table
        .set_header(vec!["ACCOUNT", "TENANT", "ADDED", ""])
        .set_content_arrangement(ContentArrangement::Dynamic);

    let now = Utc::now();
    for account in &config.accounts {
        let mut markers = Vec::new();
        if config.defaults.send.as_deref() == Some(account.email.as_str()) {
            markers.push("[send]".yellow().to_string());
        }
        if config.defaults.calendar.as_deref() == Some(account.email.as_str()) {
            markers.push("[calendar]".yellow().to_string());
        }
        table.add_row(vec![
            account.email.clone(),
            account.tenant_label(),
            relative_time(now, account.added_at),
            markers.join(" "),
        ]);
    }

    println!("{table}");
    println!();
    println!(
        "{} account{} signed in.",
        config.accounts.len(),
        if config.accounts.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

#[derive(Serialize)]
struct AccountOut {
    email: String,
    tenant_id: String,
    home_account_id: String,
    added_at: chrono::DateTime<Utc>,
    is_default_send: bool,
    is_default_calendar: bool,
}

fn emit_json(config: &Config) -> Result<()> {
    let out: Vec<AccountOut> = config
        .accounts
        .iter()
        .map(|a| AccountOut {
            email: a.email.clone(),
            tenant_id: a.tenant_id.clone(),
            home_account_id: a.home_account_id.clone(),
            added_at: a.added_at,
            is_default_send: config.defaults.send.as_deref() == Some(a.email.as_str()),
            is_default_calendar: config.defaults.calendar.as_deref() == Some(a.email.as_str()),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn relative_time(now: chrono::DateTime<Utc>, then: chrono::DateTime<Utc>) -> String {
    let delta = now - then;
    if delta.num_days() >= 1 {
        format!("{}d ago", delta.num_days())
    } else if delta.num_hours() >= 1 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_minutes() >= 1 {
        format!("{}m ago", delta.num_minutes())
    } else {
        "just now".to_string()
    }
}
```

- [ ] **Step 3: Update `auth_status.rs` to emit JSON when requested**

Replace the entire contents of `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_status.rs` with:

```rust
//! `pidge auth status` — summary of accounts and defaults.

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use pidge_core::Config;

pub fn run(json: bool) -> Result<()> {
    let config = Config::load()?;

    if json {
        return emit_json(&config);
    }

    let n = config.accounts.len();
    println!(
        "{} account{} signed in.",
        n,
        if n == 1 { "" } else { "s" }
    );

    if n == 0 {
        println!();
        println!("Run {} to add one.", "`pidge auth login`".cyan());
        return Ok(());
    }

    println!();
    println!("{}", "Defaults:".bold());
    println!(
        "  send:     {}",
        config.defaults.send.as_deref().unwrap_or("(none)")
    );
    println!(
        "  calendar: {}",
        config.defaults.calendar.as_deref().unwrap_or("(none)")
    );

    Ok(())
}

#[derive(Serialize)]
struct StatusOut {
    accounts: usize,
    defaults: DefaultsOut,
}

#[derive(Serialize)]
struct DefaultsOut {
    send: Option<String>,
    calendar: Option<String>,
}

fn emit_json(config: &Config) -> Result<()> {
    let out = StatusOut {
        accounts: config.accounts.len(),
        defaults: DefaultsOut {
            send: config.defaults.send.clone(),
            calendar: config.defaults.calendar.clone(),
        },
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
```

- [ ] **Step 4: Build (will still fail because inbox.rs hasn't been updated yet)**

Run: `cargo build -p pidge 2>&1 | tail -5`
Expected: still failing on `commands::inbox::run`'s signature. We fix it in Task 9.

- [ ] **Step 5: No commit yet**

Wait until Task 9's inbox.rs lands so the whole batch compiles.

---

## Task 9: `inbox.rs` rich/compact/json render + cache integration

**Files:**
- Modify: `crates/pidge/src/commands/inbox.rs`

This is the big one. Replace the current single render function with three (rich/compact/json), wire up the MessageCache, and apply subject styling + linkify.

- [ ] **Step 1: Replace `inbox.rs` entirely**

Overwrite `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/inbox.rs` with:

```rust
//! `pidge inbox list` — list messages merged across signed-in accounts.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use futures::future::join_all;
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message, MessageCache, short_hash};

use crate::cli::InboxCommands;
use crate::output::linkify_text;

/// Pair of message and its computed short hash, for rendering.
struct MessageRow {
    message: Message,
    short_hash: String,
}

pub async fn run(command: InboxCommands, json: bool) -> Result<()> {
    match command {
        InboxCommands::List {
            account,
            limit,
            unread,
            compact,
        } => list(account, limit, unread, compact, json).await,
    }
}

async fn list(
    account_filter: Vec<String>,
    limit: usize,
    unread_only: bool,
    compact: bool,
    json: bool,
) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge auth login` to add one."
        ));
    }

    let target_emails: Vec<String> = if account_filter.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        for f in &account_filter {
            if config.find(f).is_none() {
                return Err(anyhow!("not signed in to {f}"));
            }
        }
        account_filter
    };

    let per_account = compute_per_account_fetch(limit, target_emails.len());
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        async move {
            let result = graph.list_inbox(&e, per_account, unread_only).await;
            (e, result)
        }
    });

    let results = join_all(futures).await;

    let mut all_messages: Vec<Message> = Vec::new();
    let mut had_success = false;
    for (email, result) in results {
        match result {
            Ok(mut msgs) => {
                had_success = true;
                all_messages.append(&mut msgs);
            }
            Err(ClientError::SessionExpired { email: e }) => {
                eprintln!(
                    "{} {e}: session expired, run `pidge auth login`",
                    "WARNING:".yellow().bold()
                );
            }
            Err(e) => {
                eprintln!("{} {email}: {e}", "WARNING:".yellow().bold());
            }
        }
    }

    if !had_success {
        return Err(anyhow!("All accounts failed."));
    }

    all_messages.sort_by_key(|b| std::cmp::Reverse(b.received_at));
    all_messages.truncate(limit);

    // Compute short hashes and update the cache (additive).
    let rows: Vec<MessageRow> = all_messages
        .into_iter()
        .map(|m| {
            let h = short_hash(&m.id);
            MessageRow { message: m, short_hash: h }
        })
        .collect();

    update_cache(&rows)?;

    let single_account = target_emails.len() == 1;

    if json {
        return render_json(&rows);
    }
    if compact {
        render_text_compact(&rows, single_account)
    } else {
        render_text_rich(&rows, single_account)
    }
}

fn update_cache(rows: &[MessageRow]) -> Result<()> {
    let mut cache = MessageCache::load()?;
    let pairs: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r.message.id.clone(), r.message.account.clone()))
        .collect();
    cache.insert_many(&pairs);
    cache.save()?;
    Ok(())
}

fn compute_per_account_fetch(limit: usize, num_accounts: usize) -> usize {
    if num_accounts == 0 {
        return limit;
    }
    let calc = (limit as f64 * 1.2 / num_accounts as f64).ceil() as usize;
    calc.max(10)
}

fn from_display(from: &pidge_core::MessageFrom) -> &str {
    if from.name.is_empty() {
        &from.address
    } else {
        &from.name
    }
}

fn style_subject(subject: &str, is_read: bool) -> String {
    let linked = linkify_text(subject);
    if is_read {
        linked.cyan().to_string()
    } else {
        linked.bold().magenta().to_string()
    }
}

fn render_text_rich(rows: &[MessageRow], hide_account: bool) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for row in rows {
        let subject_cell = {
            let styled_subject = style_subject(&row.message.subject, row.message.is_read);
            let preview_linkified = linkify_text(&row.message.preview);
            let preview_styled = preview_linkified.dimmed().to_string();
            if row.message.preview.is_empty() {
                styled_subject
            } else {
                format!("{styled_subject}\n{preview_styled}")
            }
        };

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            row.message.account.clone(),
            from_display(&row.message.from).to_string(),
            subject_cell,
            relative_received(row.message.received_at),
        ];
        if hide_account {
            cells.remove(1);
        }
        table.add_row(cells);
    }

    println!("{table}");
    Ok(())
}

fn render_text_compact(rows: &[MessageRow], hide_account: bool) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for row in rows {
        let subject = style_subject(&row.message.subject, row.message.is_read);

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            row.message.account.clone(),
            from_display(&row.message.from).to_string(),
            subject,
            relative_received(row.message.received_at),
        ];
        if hide_account {
            cells.remove(1);
        }
        table.add_row(cells);
    }

    println!("{table}");
    Ok(())
}

#[derive(Serialize)]
struct MessageOut<'a> {
    id: &'a str,
    graph_id: &'a str,
    account: &'a str,
    from: &'a pidge_core::MessageFrom,
    subject: &'a str,
    received_at: chrono::DateTime<chrono::Utc>,
    is_read: bool,
    preview: &'a str,
}

fn render_json(rows: &[MessageRow]) -> Result<()> {
    let out: Vec<MessageOut<'_>> = rows
        .iter()
        .map(|r| MessageOut {
            id: &r.short_hash,
            graph_id: &r.message.id,
            account: &r.message.account,
            from: &r.message.from,
            subject: &r.message.subject,
            received_at: r.message.received_at,
            is_read: r.message.is_read,
            preview: &r.message.preview,
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn relative_received(then: DateTime<Utc>) -> String {
    let now = Local::now();
    let then_local: DateTime<Local> = then.with_timezone(&Local);
    let delta = now - then_local;

    if delta.num_seconds() < 60 {
        return "just now".to_string();
    }
    if delta.num_minutes() < 60 {
        return format!("{}m ago", delta.num_minutes());
    }
    if delta.num_hours() < 24 {
        return format!("{}h ago", delta.num_hours());
    }
    if now.date_naive().pred_opt() == Some(then_local.date_naive()) {
        return "yesterday".to_string();
    }
    if delta.num_days() < 7 {
        return then_local.format("%a").to_string();
    }
    if now.year() == then_local.year() {
        return then_local.format("%b %-d").to_string();
    }
    then_local.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_account_fetch_at_least_10() {
        assert_eq!(compute_per_account_fetch(5, 1), 10);
    }

    #[test]
    fn per_account_fetch_scales_with_limit_and_accounts() {
        // ceil(25 * 1.2 / 3) = ceil(10) = 10 → 10 (because max(10) wins)
        assert_eq!(compute_per_account_fetch(25, 3), 10);
        // ceil(100 * 1.2 / 3) = ceil(40) = 40
        assert_eq!(compute_per_account_fetch(100, 3), 40);
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p pidge 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge 2>&1 | tail -3`
Expected: all passing — at least 8 (2 banner + 2 inbox + 2 hyperlink + 4 linkify, plus auth tests added earlier in feature work if any).

Run: `cargo test --workspace 2>&1 | tail -3`
Expected: all passing across all crates.

- [ ] **Step 4: Format and lint**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 5: Smoke-test the CLI**

Run each and verify behavior:

```bash
cargo run -q -- --help | head -20
```
Expected: shows `--json` global flag in OPTIONS.

```bash
cargo run -q -- inbox list --help
```
Expected: shows `-c, --compact` flag; no `--output` flag.

```bash
cargo run -q -- inbox list 2>&1; echo "exit=$?"
```
Expected: `Error: No accounts signed in.` exit 1 (no panic — keychain empty, config empty).

```bash
cargo run -q -- auth status
```
Expected: `0 accounts signed in.` followed by login suggestion.

```bash
cargo run -q -- auth status --json
```
Expected: `{ "accounts": 0, "defaults": { "send": null, "calendar": null } }` JSON output.

```bash
cargo run -q -- auth list --json
```
Expected: `[]` JSON output (empty array, no accounts).

- [ ] **Step 6: Commit the whole CLI + commands batch (Tasks 7, 8, 9)**

```bash
git add crates/pidge/Cargo.toml crates/pidge/src/cli.rs crates/pidge/src/commands/auth.rs crates/pidge/src/commands/auth_list.rs crates/pidge/src/commands/auth_status.rs crates/pidge/src/commands/inbox.rs Cargo.lock
git commit -m "Add global --json flag, --compact for inbox list, and rich/compact rendering with short-hash IDs"
```

---

## Task 10: CHANGELOG entries

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Append to `[Unreleased] ### Added`**

Open `/Users/kristofer/repos/mklab-se/pidge/CHANGELOG.md`. Under the existing `## [Unreleased]` `### Added` section, append (preserving existing entries):

```markdown
- Global `--json` flag (replaces per-command `--output`) on `pidge inbox list`, `pidge auth list`, `pidge auth status`
- `pidge inbox list` shows a stable 8-char short hash ID per message; cached at `~/.cache/pidge/messages.json` for substring lookup by future `pidge inbox show`
- `pidge inbox list` rich layout: subject + 2-line preview, bold+magenta for unread, cyan for read; `--compact`/`-c` for the one-row-per-message style
- URLs in subject and preview text are OSC 8 hyperlinks (clickable in modern terminals)
- Cleaner table style — horizontal line under header only, no vertical borders
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "Update CHANGELOG for output refresh + short-hash IDs"
```

---

## Task 11: Final verification

**Files:** none modified.

- [ ] **Step 1: Run full CI suite**

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

Expected: every command exits 0. Test count increases from prior 37 baseline by:
- +3 short_hash tests
- +4 MessageCache load/save/insert tests
- +7 find_by_fragment tests
- +2 hyperlink tests
- +4 linkify_text tests

Total expected: ~57 passing tests.

- [ ] **Step 2: Smoke-test the full surface**

```bash
cargo run -q -- --help
cargo run -q -- inbox list --help
cargo run -q -- auth list --help
cargo run -q -- auth status
cargo run -q -- auth status --json
cargo run -q -- auth list --json
```

For each, verify:
- `--help` shows the global `--json` flag
- `inbox list --help` shows `-c, --compact` (no `--output`)
- `auth status` text output works
- `auth status --json` returns valid JSON `{ accounts: 0, defaults: { send: null, calendar: null } }`
- `auth list --json` returns `[]`

- [ ] **Step 3: Confirm clean working tree**

```bash
git status
git log --oneline | head -15
```

Expected: clean working tree; commits show the 7–9 task commits on top of the prior column-alignment fix.

- [ ] **Step 4: Hand-off summary**

In the controller's reply, summarize:
1. The commits landed
2. What's new for the user end-to-end (run `pidge inbox list` and they see the new rich layout)
3. The cache location and what gets stored there
4. The deferred items (`pidge inbox show`, markdown rendering)

---

## Plan self-review

**Spec coverage check:**

| Spec section | Task(s) |
|---|---|
| Workspace deps (comfy-table features, linkify, sha2) | Task 1 |
| pidge-core::cache (short_hash) | Task 2 |
| pidge-core::cache (MessageCache, load/save, insert_many, LRU) | Task 3 |
| pidge-core::cache (find_by_fragment with substring matching) | Task 4 |
| pidge::output::hyperlink (OSC 8) | Task 5 |
| pidge::output::linkify (URL detection) | Task 6 |
| Global `--json` flag | Task 7 |
| `--compact` flag on inbox list, remove `--output` | Task 7 |
| Auth commands honor --json | Task 8 |
| Inbox rich render (subject + preview, styled) | Task 9 |
| Inbox compact render (same columns, no preview) | Task 9 |
| Inbox JSON render (id + graph_id + full message data) | Task 9 |
| Cache integration in inbox list | Task 9 (`update_cache`) |
| CHANGELOG entries | Task 10 |
| Final verification | Task 11 |

**Placeholder scan:** No "TBD", "TODO", "implement later", "add appropriate validation" markers in the plan. Every code block is the actual content to land. The transient build failures between Tasks 7 and 9 are explicitly called out as expected and the batch commit lands at the end of Task 9.

**Type consistency:**
- `MessageCache`, `CachedMessageRef`, `CacheLookup`, `short_hash` — defined in Task 2/3/4, used consistently in Task 9.
- `MessageRow { message, short_hash }` — local struct in `inbox.rs`, defined Task 9.
- `MessageOut<'a>` — local serde struct in `inbox.rs`, defined Task 9. Fields match the spec's JSON shape.
- `linkify_text` — defined Task 6, called in Task 9 from `style_subject` and the preview rendering branch.
- `hyperlink` — defined Task 5, called from `linkify_text` in Task 6.
- `Cli.json` global flag — added Task 7, threaded through to auth (Task 8) and inbox (Task 9).
- `InboxCommands::List { account, limit, unread, compact }` — defined Task 7, destructured in Task 9.
- `OutputFormat` enum — DELETED in Task 7; no references remain.

**Notable risks called out:**
- The polling `comfy-table.set_header` accepts `Vec<&str>` — verified by reading existing code in this codebase; this is the same shape as the foundation already uses.
- `console` crate (transitive via `comfy-table`'s `custom_styling` feature) is pure Rust; no system dependencies. Build time grows slightly.
- The transient broken-build window between Tasks 7 (CLI signature change) and 9 (commands updated) is acceptable inside the subagent's work but means we don't commit until Task 9.
