# AI E-mail Classification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `pidge ai classify` (AI-computed, multi-label e-mail classification via ailloy), `pidge categorize` (native Outlook categories), and `pidge config` (git-style settings with classify defaults).

**Architecture:** A pure label-parser turns model text into a deduped `Vec<String>`; a `LabelModel` trait seams the ailloy chat call so logic is testable without a live model; Graph gains `get/set categories`; `pidge-core::Config` gains a `classify` block with key-path get/set/unset driving `pidge config`. Batch classification reuses the existing `buffer_unordered` concurrency pattern with an optional on-disk cache.

**Tech Stack:** Rust 2024, clap derive, tokio, ailloy 0.7 (`Client::for_capability("chat")`), Microsoft Graph, serde_yaml, wiremock (tests).

**Prerequisite:** Commit/ship the pending v0.4.7 folder work first so this builds on a clean, released base. Reference spec: `docs/superpowers/specs/2026-06-15-ai-classification-design.md`.

---

## File Structure

- `crates/pidge-core/src/config.rs` (modify) — add `ClassifyConfig` + `ConfigKey` get/set/unset.
- `crates/pidge-client/src/graph/mail.rs` (modify) — `get_categories` / `set_categories`.
- `crates/pidge-client/src/graph/mod.rs` (modify) — `GraphClient` wrappers + re-exports.
- `crates/pidge/src/commands/classify_parse.rs` (create) — pure parser + allowed-set validation.
- `crates/pidge/src/commands/classify_model.rs` (create) — `LabelModel` trait + ailloy impl + prompt assembly.
- `crates/pidge/src/commands/classify_cache.rs` (create) — best-effort on-disk cache.
- `crates/pidge/src/commands/ai_classify.rs` (create) — `pidge ai classify` (single/text/batch).
- `crates/pidge/src/commands/mail_categorize.rs` (create) — `pidge categorize`.
- `crates/pidge/src/commands/config_cmd.rs` (create) — `pidge config`.
- `crates/pidge/src/commands/mod.rs` (modify) — register new modules.
- `crates/pidge/src/cli.rs` (modify) — `AiCommands::Classify`, top-level `Categorize`, `Config` subcommands.
- `crates/pidge/src/commands/ai.rs` (modify) — dispatch `Classify`.
- `crates/pidge/src/main.rs` (modify) — dispatch `Categorize` and `Config`.
- `CHANGELOG.md` (modify).

---

## Task 1: `ClassifyConfig` in pidge-core

**Files:**
- Modify: `crates/pidge-core/src/config.rs`

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` in `config.rs`:

```rust
#[test]
fn classify_config_defaults_are_empty() {
    let c = Config::default();
    assert!(c.classify.prompt.is_none());
    assert!(c.classify.parallel.is_none());
    assert!(c.classify.cache.is_none());
    assert!(c.classify.labels.is_empty());
}

#[test]
fn classify_config_roundtrips_through_yaml() {
    let mut c = Config::default();
    c.classify.prompt = Some("Classify it".into());
    c.classify.parallel = Some(8);
    c.classify.cache = Some(true);
    c.classify.labels = vec!["invoice".into(), "receipt".into()];
    let yaml = serde_yaml::to_string(&c).unwrap();
    let back: Config = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(back.classify.prompt.as_deref(), Some("Classify it"));
    assert_eq!(back.classify.parallel, Some(8));
    assert_eq!(back.classify.labels, vec!["invoice", "receipt"]);
}

#[test]
fn config_set_get_unset_roundtrip() {
    let mut c = Config::default();
    c.set_key("classify.parallel", "8").unwrap();
    assert_eq!(c.get_key("classify.parallel"), Some("8".to_string()));
    c.set_key("classify.labels", "invoice,receipt,ticket").unwrap();
    assert_eq!(c.get_key("classify.labels"), Some("invoice,receipt,ticket".to_string()));
    c.set_key("classify.cache", "true").unwrap();
    assert_eq!(c.get_key("classify.cache"), Some("true".to_string()));
    c.unset_key("classify.parallel").unwrap();
    assert_eq!(c.get_key("classify.parallel"), None);
}

#[test]
fn config_set_rejects_unknown_key_and_bad_value() {
    let mut c = Config::default();
    assert!(c.set_key("classify.nope", "x").is_err());
    assert!(c.set_key("classify.parallel", "notanumber").is_err());
    assert!(c.set_key("classify.cache", "maybe").is_err());
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p pidge-core config::tests::classify -- --nocapture`
Expected: FAIL (no field `classify`, no method `set_key`).

- [ ] **Step 3: Add the struct and methods**

In `config.rs`, add the field to `Config`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub defaults: Defaults,
    pub trusted_senders: Vec<String>,
    pub classify: ClassifyConfig,
}
```

Add the new struct near `Defaults`:

```rust
/// User-configurable defaults for `pidge ai classify`. Every field is
/// optional; an unset field falls back to a built-in default at call time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClassifyConfig {
    /// Default classification prompt (instructions + valid outputs).
    pub prompt: Option<String>,
    /// Default batch concurrency.
    pub parallel: Option<usize>,
    /// Whether to cache classifications by message-id + prompt hash.
    pub cache: Option<bool>,
    /// Optional allowed-label set for validation.
    pub labels: Vec<String>,
}
```

Add the key-path accessors in `impl Config`:

```rust
/// Read a dotted config key as a display string, or `None` if unset.
pub fn get_key(&self, key: &str) -> Option<String> {
    match key {
        "classify.prompt" => self.classify.prompt.clone(),
        "classify.parallel" => self.classify.parallel.map(|n| n.to_string()),
        "classify.cache" => self.classify.cache.map(|b| b.to_string()),
        "classify.labels" => {
            if self.classify.labels.is_empty() {
                None
            } else {
                Some(self.classify.labels.join(","))
            }
        }
        _ => None,
    }
}

/// Set a dotted config key from a string value. Errors on unknown key or
/// unparseable value.
pub fn set_key(&mut self, key: &str, value: &str) -> Result<(), CoreError> {
    match key {
        "classify.prompt" => self.classify.prompt = Some(value.to_string()),
        "classify.parallel" => {
            let n: usize = value
                .trim()
                .parse()
                .map_err(|_| CoreError::InvalidConfigValue {
                    key: key.to_string(),
                    value: value.to_string(),
                })?;
            self.classify.parallel = Some(n);
        }
        "classify.cache" => {
            let b = match value.trim() {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(CoreError::InvalidConfigValue {
                        key: key.to_string(),
                        value: value.to_string(),
                    });
                }
            };
            self.classify.cache = Some(b);
        }
        "classify.labels" => {
            self.classify.labels = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        _ => {
            return Err(CoreError::UnknownConfigKey {
                key: key.to_string(),
            });
        }
    }
    Ok(())
}

/// Revert a dotted config key to its unset/default state.
pub fn unset_key(&mut self, key: &str) -> Result<(), CoreError> {
    match key {
        "classify.prompt" => self.classify.prompt = None,
        "classify.parallel" => self.classify.parallel = None,
        "classify.cache" => self.classify.cache = None,
        "classify.labels" => self.classify.labels.clear(),
        _ => {
            return Err(CoreError::UnknownConfigKey {
                key: key.to_string(),
            });
        }
    }
    Ok(())
}

/// Every settable config key, for `pidge config show`/help.
pub const KNOWN_KEYS: &'static [&'static str] = &[
    "classify.prompt",
    "classify.parallel",
    "classify.cache",
    "classify.labels",
];
```

Add the error variants in `crates/pidge-core/src/error.rs` (`CoreError` enum):

```rust
#[error("unknown config key '{key}'")]
UnknownConfigKey { key: String },

#[error("invalid value '{value}' for config key '{key}'")]
InvalidConfigValue { key: String, value: String },
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p pidge-core config::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-core/src/config.rs crates/pidge-core/src/error.rs
git commit -m "feat(core): add ClassifyConfig and dotted-key config accessors"
```

---

## Task 2: Graph categories (get/set)

**Files:**
- Modify: `crates/pidge-client/src/graph/mail.rs`
- Modify: `crates/pidge-client/src/graph/mod.rs`

- [ ] **Step 1: Write failing tests**

Add to `mail.rs` `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn get_categories_parses_field() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/me/messages/.+$"))
        .and(query_param("$select", "categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "categories": ["Receipts", "Urgent"]
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let cats = get_categories(&http, &server.uri(), "AT", "MSG").await.unwrap();
    assert_eq!(cats, vec!["Receipts", "Urgent"]);
}

#[tokio::test]
async fn set_categories_patches_array() {
    use wiremock::matchers::body_partial_json;
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path_regex(r"^/me/messages/.+$"))
        .and(body_partial_json(serde_json::json!({ "categories": ["receipt", "ticket"] })))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    set_categories(&http, &server.uri(), "AT", "MSG",
        &["receipt".to_string(), "ticket".to_string()]).await.unwrap();
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p pidge-client graph::mail::tests::get_categories graph::mail::tests::set_categories`
Expected: FAIL (functions undefined).

- [ ] **Step 3: Implement**

In `mail.rs`, add near `patch_message`:

```rust
#[derive(serde::Deserialize)]
struct GraphCategories {
    #[serde(default)]
    categories: Vec<String>,
}

/// GET /me/messages/{id}?$select=categories — read a message's categories.
pub async fn get_categories(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<String>, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}?$select=categories");
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph { status: status.as_u16(), message: text });
    }
    let body: GraphCategories = resp.json().await?;
    Ok(body.categories)
}

/// PATCH /me/messages/{id} with `{ "categories": [...] }` — replace categories.
pub async fn set_categories(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    categories: &[String],
) -> Result<(), ClientError> {
    patch_message(
        http,
        base_url,
        access_token,
        message_id,
        &serde_json::json!({ "categories": categories }),
    )
    .await
}
```

In `mod.rs`, add to the `pub use mail::{…}` list: `get_categories, set_categories`. Add `GraphClient` methods:

```rust
/// GET /me/messages/{id}?$select=categories.
pub async fn get_categories(&self, account: &str, message_id: &str) -> Result<Vec<String>, ClientError> {
    let token = self.auth.get_valid_token(account).await?;
    mail::get_categories(&self.http, &self.base_url, &token, message_id).await
}

/// PATCH /me/messages/{id} categories.
pub async fn set_categories(&self, account: &str, message_id: &str, categories: &[String]) -> Result<(), ClientError> {
    let token = self.auth.get_valid_token(account).await?;
    mail::set_categories(&self.http, &self.base_url, &token, message_id, categories).await
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p pidge-client graph::mail::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-client/src/graph/mail.rs crates/pidge-client/src/graph/mod.rs
git commit -m "feat(client): get/set message categories via Graph"
```

---

## Task 3: Pure label parser + allowed-set validation

**Files:**
- Create: `crates/pidge/src/commands/classify_parse.rs`
- Modify: `crates/pidge/src/commands/mod.rs` (add `pub mod classify_parse;`)

- [ ] **Step 1: Write failing tests** (create the file with tests + stubs)

```rust
//! Pure parsing of a model's text response into a deduped label set, plus
//! optional validation against an allowed set. No I/O — unit-testable.

/// Parse a model's raw `content` into an ordered, deduped, lowercased label
/// set. Tolerates a JSON array, or a comma/newline separated list. Empty
/// input yields `["unknown"]`.
pub fn parse_labels(content: &str) -> Vec<String> {
    todo!()
}

/// Keep only labels present in `allowed` (case-insensitive). If none remain,
/// return `["unknown"]`. If `allowed` is empty, return `labels` unchanged.
pub fn validate_labels(labels: Vec<String>, allowed: &[String]) -> Vec<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> { s.iter().map(|x| x.to_string()).collect() }

    #[test]
    fn parses_comma_list() {
        assert_eq!(parse_labels("receipt, ticket"), v(&["receipt", "ticket"]));
    }
    #[test]
    fn parses_newlines_and_trims_and_lowercases() {
        assert_eq!(parse_labels("Receipt\n  TICKET \n"), v(&["receipt", "ticket"]));
    }
    #[test]
    fn parses_json_array() {
        assert_eq!(parse_labels("[\"receipt\", \"ticket\"]"), v(&["receipt", "ticket"]));
    }
    #[test]
    fn dedups_preserving_order() {
        assert_eq!(parse_labels("receipt, ticket, receipt"), v(&["receipt", "ticket"]));
    }
    #[test]
    fn empty_becomes_unknown() {
        assert_eq!(parse_labels("   "), v(&["unknown"]));
    }
    #[test]
    fn validate_keeps_in_set_only() {
        let allowed = v(&["invoice", "receipt", "ticket"]);
        assert_eq!(validate_labels(v(&["receipt", "spam"]), &allowed), v(&["receipt"]));
    }
    #[test]
    fn validate_none_in_set_is_unknown() {
        let allowed = v(&["invoice"]);
        assert_eq!(validate_labels(v(&["spam"]), &allowed), v(&["unknown"]));
    }
    #[test]
    fn validate_empty_allowed_is_passthrough() {
        assert_eq!(validate_labels(v(&["x", "y"]), &[]), v(&["x", "y"]));
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p pidge classify_parse`
Expected: FAIL (`todo!()` panics).

- [ ] **Step 3: Implement the two functions**

```rust
pub fn parse_labels(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    // Try JSON array of strings first.
    let raw: Vec<String> = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<String>>(trimmed).unwrap_or_default()
    } else {
        trimmed
            .split(['\n', ','])
            .map(|s| s.trim().to_string())
            .collect()
    };
    let mut out: Vec<String> = Vec::new();
    for label in raw {
        let norm = label.trim().to_lowercase();
        if norm.is_empty() || out.contains(&norm) {
            continue;
        }
        out.push(norm);
    }
    if out.is_empty() {
        out.push("unknown".to_string());
    }
    out
}

pub fn validate_labels(labels: Vec<String>, allowed: &[String]) -> Vec<String> {
    if allowed.is_empty() {
        return labels;
    }
    let allow_lower: Vec<String> = allowed.iter().map(|a| a.to_lowercase()).collect();
    let kept: Vec<String> = labels
        .into_iter()
        .filter(|l| allow_lower.contains(&l.to_lowercase()))
        .collect();
    if kept.is_empty() {
        vec!["unknown".to_string()]
    } else {
        kept
    }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p pidge classify_parse`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge/src/commands/classify_parse.rs crates/pidge/src/commands/mod.rs
git commit -m "feat: pure label parser and allowed-set validation"
```

---

## Task 4: Classifier model seam + prompt assembly

**Files:**
- Create: `crates/pidge/src/commands/classify_model.rs`
- Modify: `crates/pidge/src/commands/mod.rs` (add `pub mod classify_model;`)

- [ ] **Step 1: Write failing tests**

```rust
//! The AI seam for classification. `LabelModel` abstracts the chat call so
//! batch/dispatch logic is testable with a fake. `AilloyModel` is the
//! production implementation. `build_input` assembles the text shown to the
//! model from a message (pure, unit-tested).

use anyhow::Result;
use pidge_core::FullMessage;

/// Maximum characters of body text fed to the model (token budget guard).
const MAX_BODY_CHARS: usize = 4000;

#[allow(async_fn_in_trait)]
pub trait LabelModel {
    /// Return the model's raw text response for `prompt` applied to `input`.
    async fn classify(&self, prompt: &str, input: &str) -> Result<String>;
}

/// Build the text block describing a message for the model: subject, sender,
/// and a length-capped plain-text body.
pub fn build_input(subject: &str, from: &str, body_text: &str) -> String {
    let body: String = body_text.chars().take(MAX_BODY_CHARS).collect();
    format!("Subject: {subject}\nFrom: {from}\n\n{body}")
}

/// Assemble the final user message sent to the model: the user's prompt,
/// then the message block.
pub fn assemble_user_message(prompt: &str, input: &str) -> String {
    format!("{prompt}\n\n---\n{input}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_input_caps_body_length() {
        let body = "x".repeat(MAX_BODY_CHARS + 500);
        let out = build_input("Sub", "a@b.c", &body);
        assert!(out.contains("Subject: Sub"));
        assert!(out.contains("From: a@b.c"));
        let body_part = out.split("\n\n").nth(1).unwrap();
        assert_eq!(body_part.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn assemble_user_message_includes_prompt_and_input() {
        let m = assemble_user_message("Classify it", "Subject: Hi");
        assert!(m.starts_with("Classify it"));
        assert!(m.contains("Subject: Hi"));
    }

    struct FakeModel(&'static str);
    impl LabelModel for FakeModel {
        async fn classify(&self, _p: &str, _i: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[tokio::test]
    async fn fake_model_returns_canned_content() {
        let m = FakeModel("receipt, ticket");
        assert_eq!(m.classify("p", "i").await.unwrap(), "receipt, ticket");
    }

    // Keep FullMessage import exercised so the trait signature stays in sync
    // with the production caller.
    #[allow(dead_code)]
    fn _assert_fullmessage_type(_m: &FullMessage) {}
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p pidge classify_model`
Expected: FAIL (file/symbols missing).

- [ ] **Step 3: Add the production `AilloyModel`** (append after the tests-referenced items, outside the test module)

```rust
/// Production `LabelModel` backed by the user's configured ailloy chat node.
pub struct AilloyModel {
    client: ailloy::Client,
}

impl AilloyModel {
    /// Build from the configured `chat` capability. Errors if AI isn't set up.
    pub fn new() -> Result<Self> {
        let client = ailloy::Client::for_capability("chat")
            .map_err(|e| anyhow::anyhow!("AI not configured ({e}). Run `pidge ai config`."))?;
        Ok(Self { client })
    }
}

impl LabelModel for AilloyModel {
    async fn classify(&self, prompt: &str, input: &str) -> Result<String> {
        let msg = assemble_user_message(prompt, input);
        let resp = self
            .client
            .chat(&[ailloy::types::Message::user(&msg)])
            .await
            .map_err(|e| anyhow::anyhow!("AI classify failed: {e}"))?;
        Ok(resp.content)
    }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p pidge classify_model`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge/src/commands/classify_model.rs crates/pidge/src/commands/mod.rs
git commit -m "feat: LabelModel seam, ailloy impl, prompt assembly"
```

---

## Task 5: Classification cache

**Files:**
- Create: `crates/pidge/src/commands/classify_cache.rs`
- Modify: `crates/pidge/src/commands/mod.rs` (add `pub mod classify_cache;`)

- [ ] **Step 1: Write failing tests**

```rust
//! Best-effort cache of classifications keyed by message graph-id + prompt
//! hash. A corrupt or missing cache is ignored, never fatal.

use std::collections::HashMap;
use std::path::PathBuf;

/// Stable 16-hex-char key from a message id and the prompt text.
pub fn cache_key(graph_id: &str, prompt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(prompt.as_bytes());
    let digest = h.finalize();
    format!("{graph_id}:{:x}", digest)[..graph_id.len() + 1 + 16].to_string()
}

#[derive(Default)]
pub struct ClassifyCache {
    map: HashMap<String, Vec<String>>,
    path: Option<PathBuf>,
}

impl ClassifyCache {
    pub fn load() -> Self { Self::load_from(default_path()) }

    pub fn load_from(path: Option<PathBuf>) -> Self {
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { map, path }
    }

    pub fn get(&self, key: &str) -> Option<Vec<String>> { self.map.get(key).cloned() }

    pub fn put(&mut self, key: String, labels: Vec<String>) {
        self.map.insert(key, labels);
    }

    pub fn save(&self) {
        if let Some(p) = &self.path {
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
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p pidge classify_cache`
Expected: FAIL (missing `sha2` / `tempfile` dev-dep or module).

- [ ] **Step 3: Add dependencies**

In `crates/pidge/Cargo.toml`: under `[dependencies]` add `sha2 = "0.10"` (check workspace for an existing pin first and reuse it). Under `[dev-dependencies]` ensure `tempfile` is present (pidge-core already uses it; add to pidge if missing).

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p pidge classify_cache`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge/src/commands/classify_cache.rs crates/pidge/src/commands/mod.rs crates/pidge/Cargo.toml Cargo.lock
git commit -m "feat: best-effort classification cache"
```

---

## Task 6: `pidge config` command

**Files:**
- Create: `crates/pidge/src/commands/config_cmd.rs`
- Modify: `crates/pidge/src/cli.rs` (top-level `Config` + `ConfigCommands`)
- Modify: `crates/pidge/src/main.rs` (dispatch)
- Modify: `crates/pidge/src/commands/mod.rs` (`pub mod config_cmd;`)

- [ ] **Step 1: Add CLI definitions**

In `cli.rs`, add to `enum Commands`:

```rust
/// Read or write pidge's own settings (distinct from `pidge ai config`,
/// which configures the AI provider).
Config {
    #[command(subcommand)]
    command: ConfigCommands,
},
```

Add the subcommand enum:

```rust
#[derive(clap::Subcommand, Debug)]
pub enum ConfigCommands {
    /// Print all settable keys and their effective values
    Show,
    /// Print one config value
    Get { key: String },
    /// Set a config value. For multi-line values (e.g. classify.prompt) use
    /// `--file <path>` or `-` (stdin) instead of an inline value.
    Set {
        key: String,
        /// Inline value (omit when using --file or stdin)
        value: Option<String>,
        /// Read the value from a file (`-` = stdin)
        #[arg(long)]
        file: Option<String>,
    },
    /// Revert a key to its built-in default
    Unset { key: String },
}
```

- [ ] **Step 2: Write the command with tests**

Create `config_cmd.rs`:

```rust
//! `pidge config` — read/write pidge's own settings.

use anyhow::{Result, anyhow};
use colored::Colorize;
use pidge_core::Config;

use crate::cli::ConfigCommands;

pub fn run(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Show => show(),
        ConfigCommands::Get { key } => get(&key),
        ConfigCommands::Set { key, value, file } => set(&key, value, file),
        ConfigCommands::Unset { key } => unset(&key),
    }
}

fn show() -> Result<()> {
    let config = Config::load()?;
    for key in Config::KNOWN_KEYS {
        let val = config.get_key(key).unwrap_or_else(|| "(unset)".to_string());
        println!("{} = {}", key.cyan(), val.dimmed());
    }
    Ok(())
}

fn get(key: &str) -> Result<()> {
    let config = Config::load()?;
    match config.get_key(key) {
        Some(v) => println!("{v}"),
        None => return Err(anyhow!("'{key}' is unset (or unknown). See `pidge config show`.")),
    }
    Ok(())
}

fn read_value(value: Option<String>, file: Option<String>) -> Result<String> {
    match (value, file) {
        (Some(_), Some(_)) => Err(anyhow!("pass either an inline value or --file, not both")),
        (Some(v), None) => Ok(v),
        (None, Some(f)) => {
            if f == "-" {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                Ok(s.trim_end_matches('\n').to_string())
            } else {
                Ok(std::fs::read_to_string(&f)?.trim_end_matches('\n').to_string())
            }
        }
        (None, None) => Err(anyhow!("provide a value, --file <path>, or `--file -` for stdin")),
    }
}

fn set(key: &str, value: Option<String>, file: Option<String>) -> Result<()> {
    let v = read_value(value, file)?;
    let mut config = Config::load()?;
    config.set_key(key, &v)?;
    config.save()?;
    println!("{} set {}", "✔".green(), key.cyan());
    Ok(())
}

fn unset(key: &str) -> Result<()> {
    let mut config = Config::load()?;
    config.unset_key(key)?;
    config.save()?;
    println!("{} unset {}", "✔".green(), key.cyan());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_value_rejects_both_sources() {
        assert!(read_value(Some("x".into()), Some("f".into())).is_err());
    }
    #[test]
    fn read_value_inline() {
        assert_eq!(read_value(Some("x".into()), None).unwrap(), "x");
    }
    #[test]
    fn read_value_requires_a_source() {
        assert!(read_value(None, None).is_err());
    }
}
```

In `main.rs`, dispatch the new command (next to the other `Commands::` arms):

```rust
Commands::Config { command } => commands::config_cmd::run(command),
```

(`pidge config` is synchronous; call it without `.await`. If the dispatch is inside an async match returning a future, wrap as `async { ... }` consistent with neighbouring sync arms — follow the pattern already used by `Completion`.)

- [ ] **Step 3: Run tests + manual smoke**

Run: `cargo test -p pidge config_cmd`
Expected: PASS.
Run: `cargo run -- config set classify.parallel 8 && cargo run -- config get classify.parallel`
Expected: prints `8`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge/src/commands/config_cmd.rs crates/pidge/src/cli.rs crates/pidge/src/main.rs crates/pidge/src/commands/mod.rs
git commit -m "feat: pidge config get/set/unset/show"
```

---

## Task 7: `pidge categorize` command

**Files:**
- Create: `crates/pidge/src/commands/mail_categorize.rs`
- Modify: `crates/pidge/src/cli.rs` (top-level `Categorize` + `CategorizeCommands`)
- Modify: `crates/pidge/src/main.rs` (dispatch)
- Modify: `crates/pidge/src/commands/mod.rs`

- [ ] **Step 1: CLI definitions**

In `cli.rs` `enum Commands`:

```rust
/// Manage a message's native Outlook categories (labels).
Categorize {
    #[command(subcommand)]
    command: CategorizeCommands,
},
```

```rust
#[derive(clap::Subcommand, Debug)]
pub enum CategorizeCommands {
    /// Show a message's current categories
    Show { fragment: String },
    /// Replace a message's categories with the given labels
    Set { fragment: String, labels: Vec<String> },
    /// Add labels, keeping existing categories
    Add { fragment: String, labels: Vec<String> },
    /// Remove all categories from a message
    Clear { fragment: String },
}
```

Note: make `Show` the default by accepting a bare `pidge categorize <fragment>` in the `main.rs` argv preprocessor only if that pattern already exists for other commands; otherwise require the explicit `show` subcommand (simpler, no preprocessor change).

- [ ] **Step 2: Implement the command**

Create `mail_categorize.rs`:

```rust
//! `pidge categorize` — manage native Outlook categories on a message.

use anyhow::Result;
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};

use crate::cli::CategorizeCommands;
use crate::commands::mail_fragment::{purge_from_cache, resolve};

pub async fn run(command: CategorizeCommands) -> Result<()> {
    match command {
        CategorizeCommands::Show { fragment } => show(fragment).await,
        CategorizeCommands::Set { fragment, labels } => set(fragment, labels).await,
        CategorizeCommands::Add { fragment, labels } => add(fragment, labels).await,
        CategorizeCommands::Clear { fragment } => set(fragment, vec![]).await,
    }
}

async fn graph() -> Result<GraphClient> {
    Ok(GraphClient::new(AuthClient::from_env()?)?)
}

async fn show(fragment: String) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let g = graph().await?;
    let cats = g.get_categories(&msg.account, &msg.graph_id).await?;
    if cats.is_empty() {
        println!("{} no categories", short.dimmed());
    } else {
        println!("{} {}", short.dimmed(), cats.join(", ").cyan());
    }
    Ok(())
}

async fn set(fragment: String, labels: Vec<String>) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let g = graph().await?;
    g.set_categories(&msg.account, &msg.graph_id, &labels).await?;
    let _ = purge_from_cache(&short); // category change isn't a move, but keep cache honest
    if labels.is_empty() {
        println!("{} cleared categories on {}", "✔".green(), short.dimmed());
    } else {
        println!("{} set categories on {} → {}", "✔".green(), short.dimmed(), labels.join(", ").cyan());
    }
    Ok(())
}

async fn add(fragment: String, labels: Vec<String>) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let g = graph().await?;
    let mut cats = g.get_categories(&msg.account, &msg.graph_id).await?;
    for l in labels {
        if !cats.iter().any(|c| c.eq_ignore_ascii_case(&l)) {
            cats.push(l);
        }
    }
    g.set_categories(&msg.account, &msg.graph_id, &cats).await?;
    println!("{} categories on {} → {}", "✔".green(), short.dimmed(), cats.join(", ").cyan());
    Ok(())
}
```

In `main.rs`:

```rust
Commands::Categorize { command } => commands::mail_categorize::run(command).await,
```

- [ ] **Step 3: Build + manual smoke**

Run: `cargo build` then `cargo run -- categorize set <hash> Receipts` and `cargo run -- categorize show <hash>`
Expected: shows `Receipts`. Verify the colored label appears in Outlook.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge/src/commands/mail_categorize.rs crates/pidge/src/cli.rs crates/pidge/src/main.rs crates/pidge/src/commands/mod.rs
git commit -m "feat: pidge categorize (native Outlook categories)"
```

---

## Task 8: `pidge ai classify` — single & text modes

**Files:**
- Create: `crates/pidge/src/commands/ai_classify.rs`
- Modify: `crates/pidge/src/cli.rs` (`AiCommands::Classify`)
- Modify: `crates/pidge/src/commands/ai.rs` (dispatch)
- Modify: `crates/pidge/src/commands/mod.rs`

- [ ] **Step 1: CLI definition**

In `cli.rs` `enum AiCommands`, add:

```rust
/// Classify e-mail(s) into label(s) using the configured AI provider.
Classify {
    /// Fragment of one message's 8-char short hash (single mode).
    fragment: Option<String>,
    /// Classify a literal string instead of a message (prompt test).
    #[arg(long, conflicts_with = "fragment")]
    text: Option<String>,
    /// Override the configured prompt for this run.
    #[arg(long)]
    prompt: Option<String>,
    /// Read the prompt from a file (`-` = stdin).
    #[arg(long, conflicts_with = "prompt")]
    prompt_file: Option<String>,
    /// Allowed label set; answers are validated against it.
    #[arg(long, value_delimiter = ',')]
    labels: Vec<String>,
    /// BULK: only classify messages from this sender (repeatable).
    #[arg(long, conflicts_with_all = ["fragment", "text"])]
    from: Vec<String>,
    /// BULK: only classify messages older than this date/duration.
    #[arg(long, conflicts_with_all = ["fragment", "text"])]
    older_than: Option<String>,
    /// BULK: classify within this folder (nested path allowed).
    #[arg(long, conflicts_with_all = ["fragment", "text"])]
    folder: Option<String>,
    /// BULK: max messages to classify.
    #[arg(short = 'n', long)]
    limit: Option<usize>,
    /// Account(s) to act on (default: all signed-in).
    #[arg(long)]
    account: Vec<String>,
    /// Max concurrent AI calls in batch mode (overrides config).
    #[arg(long)]
    parallel: Option<usize>,
    /// Bypass the classification cache.
    #[arg(long)]
    no_cache: bool,
    /// Also write the result to the message's native Outlook categories.
    #[arg(long)]
    set_category: bool,
},
```

- [ ] **Step 2: Implement single + text modes (with a fake-model unit test)**

Create `ai_classify.rs`. Resolve the effective prompt (flag → `--prompt-file` → `config.classify.prompt`), then dispatch by mode. Include a pure helper `resolve_prompt` with tests; the model call itself is integration-tested manually.

```rust
//! `pidge ai classify` — compute label(s) for an e-mail (or literal text)
//! using the configured AI provider.

use anyhow::{Result, anyhow};
use serde::Serialize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::cli::AiCommands;
use crate::commands::classify_model::{AilloyModel, LabelModel, build_input};
use crate::commands::classify_parse::{parse_labels, validate_labels};
use crate::commands::mail_fragment::resolve;

#[derive(Serialize)]
struct ClassifyOut {
    hash: Option<String>,
    from: Option<String>,
    classification: Vec<String>,
}

/// Resolve the effective prompt from flags then config. Pure + tested.
pub fn resolve_prompt(
    prompt: Option<String>,
    prompt_file: Option<String>,
    config_prompt: Option<String>,
) -> Result<String> {
    if let Some(p) = prompt {
        return Ok(p);
    }
    if let Some(f) = prompt_file {
        let s = if f == "-" {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        } else {
            std::fs::read_to_string(&f)?
        };
        return Ok(s);
    }
    config_prompt.ok_or_else(|| {
        anyhow!("no prompt: pass --prompt/--prompt-file or set one with `pidge config set classify.prompt`")
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run(/* destructured AiCommands::Classify fields */) -> Result<()> {
    // See Step 3 for batch; this step wires single + text only.
    unimplemented!("filled across steps 2-3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prompt_prefers_flag() {
        let p = resolve_prompt(Some("flag".into()), None, Some("cfg".into())).unwrap();
        assert_eq!(p, "flag");
    }
    #[test]
    fn resolve_prompt_falls_back_to_config() {
        let p = resolve_prompt(None, None, Some("cfg".into())).unwrap();
        assert_eq!(p, "cfg");
    }
    #[test]
    fn resolve_prompt_errors_when_unset() {
        assert!(resolve_prompt(None, None, None).is_err());
    }
}
```

Now replace `run` with the real signature and single/text logic (batch added in Task 9). Match the field names from the CLI enum:

```rust
pub async fn run(args: crate::cli::ClassifyArgs, json: bool) -> Result<()> {
    let config = Config::load()?;
    let prompt = resolve_prompt(args.prompt, args.prompt_file, config.classify.prompt.clone())?;
    let allowed = if args.labels.is_empty() { config.classify.labels.clone() } else { args.labels.clone() };

    // Text mode: no mailbox.
    if let Some(text) = args.text {
        let model = AilloyModel::new()?;
        let raw = model.classify(&prompt, &text).await?;
        let labels = validate_labels(parse_labels(&raw), &allowed);
        emit_single(None, None, labels, json)?;
        return Ok(());
    }

    // Single mode.
    if let Some(fragment) = args.fragment {
        let (short, msg) = resolve(&fragment)?;
        let g = GraphClient::new(AuthClient::from_env()?)?;
        let full = g.get_message(&msg.account, &msg.graph_id).await?;
        let body = body_text(&full);
        let input = build_input(&full.subject, &full.from.address, &body);
        let model = AilloyModel::new()?;
        let labels = validate_labels(parse_labels(&model.classify(&prompt, &input).await?), &allowed);
        if args.set_category {
            g.set_categories(&msg.account, &msg.graph_id, &labels).await?;
        }
        emit_single(Some(short), Some(full.from.address), labels, json)?;
        return Ok(());
    }

    // Batch mode (Task 9).
    run_batch(args, &prompt, &allowed, json).await
}

fn body_text(m: &pidge_core::FullMessage) -> String {
    use pidge_core::BodyContentType;
    match m.body_content_type {
        BodyContentType::Text => m.body_content.clone(),
        BodyContentType::Html => html2text::from_read(m.body_content.as_bytes(), 100),
    }
}

fn emit_single(hash: Option<String>, from: Option<String>, labels: Vec<String>, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(&ClassifyOut { hash, from, classification: labels })?);
    } else {
        for l in labels { println!("{l}"); }
    }
    Ok(())
}
```

Add a `ClassifyArgs` clap `#[derive(Args)]` struct in `cli.rs` holding all the `Classify` fields, and change `AiCommands::Classify(ClassifyArgs)` to a tuple variant so it can be passed whole (mirrors `MailCommands::New(ComposeArgs)`).

In `ai.rs` dispatch:

```rust
Some(AiCommands::Classify(args)) => crate::commands::ai_classify::run(args, json).await,
```

(Thread `json` into `ai::run` — change its signature to `run(cmd, json)` and update the call site in `main.rs`.)

- [ ] **Step 3: Run tests + manual smoke**

Run: `cargo test -p pidge ai_classify`
Expected: PASS (resolve_prompt tests).
Run: `cargo run -- ai classify --text "Your invoice #123 is attached" --prompt "Reply with one of: invoice, receipt, other"`
Expected: prints `invoice` (requires AI configured).

- [ ] **Step 4: Commit**

```bash
git add crates/pidge/src/commands/ai_classify.rs crates/pidge/src/cli.rs crates/pidge/src/commands/ai.rs crates/pidge/src/main.rs crates/pidge/src/commands/mod.rs
git commit -m "feat: pidge ai classify (single + text modes)"
```

---

## Task 9: `pidge ai classify` — batch mode

**Files:**
- Modify: `crates/pidge/src/commands/ai_classify.rs`

- [ ] **Step 1: Implement `run_batch` with cache + concurrency**

Gather messages (reuse the inbox/folder/sender selection patterns from `mail_actions.rs`: sender mode → `graph.search_messages("from:<s>")`; folder mode → `graph.list_folder(id, …)`; otherwise inbox), apply `--older-than` cutoff (`mail_delete::parse_older_than`) and `--limit`, then classify concurrently:

```rust
async fn run_batch(args: crate::cli::ClassifyArgs, prompt: &str, allowed: &[String], json: bool) -> Result<()> {
    use futures::stream::{self, StreamExt};

    let config = Config::load()?;
    let parallel = args.parallel
        .or(config.classify.parallel)
        .unwrap_or(4);
    let use_cache = !args.no_cache && config.classify.cache.unwrap_or(true);

    let g = GraphClient::new(AuthClient::from_env()?)?;
    let messages = select_messages(&g, &args).await?; // Vec<(account, graph_id, short, from)>

    let model = AilloyModel::new()?;
    let mut cache = if use_cache { crate::commands::classify_cache::ClassifyCache::load() }
                    else { crate::commands::classify_cache::ClassifyCache::default() };

    // Pre-compute cache hits serially (cheap), classify misses concurrently.
    let tasks = messages.into_iter().map(|(account, id, short, from)| {
        let key = crate::commands::classify_cache::cache_key(&id, prompt);
        let cached = if use_cache { cache.get(&key) } else { None };
        let (g, model, prompt, allowed) = (&g, &model, prompt, allowed);
        async move {
            let labels = if let Some(c) = cached {
                c
            } else {
                match classify_one(g, model, &account, &id, prompt, allowed).await {
                    Ok(l) => l,
                    Err(e) => { eprintln!("  ! {short}: {e}"); return (short, from, key, vec!["unknown".to_string()], false); }
                }
            };
            let mut wrote = false;
            if args.set_category {
                if let Err(e) = g.set_categories(&account, &id, &labels).await {
                    eprintln!("  ! {short}: set-category failed: {e}");
                } else { wrote = true; }
            }
            let _ = wrote;
            (short, from, key, labels, true)
        }
    });

    let results: Vec<_> = stream::iter(tasks).buffer_unordered(parallel).collect().await;

    let mut out = Vec::new();
    for (short, from, key, labels, fresh) in &results {
        if use_cache && *fresh { cache.put(key.clone(), labels.clone()); }
        out.push(ClassifyOut { hash: Some(short.clone()), from: Some(from.clone()), classification: labels.clone() });
    }
    if use_cache { cache.save(); }

    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for o in &out {
            println!("{}  {}  {}", o.hash.as_deref().unwrap_or("").to_string(),
                     o.from.as_deref().unwrap_or(""), o.classification.join(", "));
        }
    }
    Ok(())
}

async fn classify_one(g: &GraphClient, model: &AilloyModel, account: &str, id: &str, prompt: &str, allowed: &[String]) -> Result<Vec<String>> {
    let full = g.get_message(account, id).await?;
    let input = build_input(&full.subject, &full.from.address, &body_text(&full));
    let raw = model.classify(prompt, &input).await?;
    Ok(validate_labels(parse_labels(&raw), allowed))
}
```

Implement `select_messages` mirroring `mail_actions`'s account-resolution and sender/folder/inbox selection; cap by `--limit`. Keep it under ~60 lines; if it grows, extract a `classify_select.rs`.

- [ ] **Step 2: Run tests + manual smoke**

Run: `cargo test -p pidge`
Expected: PASS.
Run: `cargo run -- ai classify --from billing@example.com -n 5 --parallel 3 --prompt "..." --json`
Expected: JSON array of `{hash, from, classification}`.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/ai_classify.rs
git commit -m "feat: pidge ai classify batch mode (filters, parallelism, cache, set-category)"
```

---

## Task 10: Docs, lint, changelog

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `crates/pidge/src/commands/skill.rs` (only if it enumerates commands — the generic skill discovers at runtime, so likely no change)

- [ ] **Step 1: Full gate**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: clean, all pass. Fix any `too_many_arguments` with `#[allow(...)]` consistent with existing code.

- [ ] **Step 2: CHANGELOG entry under `[Unreleased]`**

```markdown
### Added

- **AI e-mail classification.** `pidge ai classify` labels e-mail(s) using the configured AI provider (via ailloy) against a user-defined prompt — single message, arbitrary `--text`, or batch (`--from`/`--older-than`/`--folder`/`-n`) with `--parallel N`. Multi-label aware; `--labels a,b,c` validates the answer; `--set-category` writes the result to the message's native Outlook categories. Results are cached by message-id + prompt.
- **`pidge categorize`** — manage native Outlook categories (`show`/`set`/`add`/`clear`).
- **`pidge config`** — git-style get/set/unset/show for pidge's own settings, including `classify.prompt`, `classify.parallel`, `classify.cache`, `classify.labels`.
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: changelog for AI classification, categorize, config"
```

---

## Self-Review Notes

- **Spec coverage:** prompt config (T1/T6), test-via-`ai` (T8 `--text`), single/batch + filters (T8/T9), parallelism (T9 + config T1), provider via ailloy (T4), store-as-label (T7 `categorize` + T8/T9 `--set-category`), cache (T5/T9), multi-label set throughout (T3 parser, T7/T8/T9). Sort-by-label explicitly deferred.
- **Type consistency:** `LabelModel::classify(&self, prompt, input) -> Result<String>` used identically in T4/T8/T9; `parse_labels`/`validate_labels` signatures stable; `ClassifyArgs` tuple variant mirrors `ComposeArgs`.
- **Known follow-up:** `select_messages` reuses `mail_actions` patterns — if it drifts large, split into `classify_select.rs` (noted in T9).
