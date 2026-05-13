# M365 Auth + `pidge inbox list` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OAuth device-code sign-in to Microsoft 365 / personal Microsoft accounts (multi-account, keychain-stored tokens) and ship `pidge inbox list` as the validation command that merges messages from every signed-in account.

**Architecture:** Workspace splits 1 → 3 crates. `pidge-core` owns plain types + config I/O (`Account`, `Config`, `Message`). `pidge-client` owns the OAuth device-code flow (hand-rolled per RFC 8628), token refresh, OS keychain storage via the `keyring` crate, and the Microsoft Graph HTTP client. `pidge` is the CLI: clap definitions, command dispatch, output rendering.

**Tech Stack:** Hand-rolled RFC 8628 device authorization grant via `reqwest`. JWT id_token decoded with `base64` (no signature verification — TLS trust to login.microsoftonline.com). Tokens persisted via `keyring = "3"`. Microsoft Graph v1.0 (`https://graph.microsoft.com/v1.0`). Tests use `wiremock` for HTTP fixtures and `keyring`'s `MockBackend` so CI never touches the real keychain.

**Reference spec:** `docs/superpowers/specs/2026-05-13-m365-auth-and-inbox-list-design.md` — every decision in this plan derives from there.

**Working directory:** `/Users/kristofer/repos/mklab-se/pidge`

---

## File inventory

### New crates
```
crates/pidge-core/
├── Cargo.toml
└── src/
    ├── lib.rs               # re-exports
    ├── error.rs             # CoreError
    ├── account.rs           # Account, AccountSet
    ├── message.rs           # Message, MessageFrom
    └── config.rs            # Config, Defaults, load/save

crates/pidge-client/
├── Cargo.toml
└── src/
    ├── lib.rs               # re-exports GraphClient
    ├── error.rs             # ClientError
    ├── auth/
    │   ├── mod.rs           # AuthClient (public façade)
    │   ├── config.rs        # APP_CLIENT_ID, SCOPES, AUTHORITY URLs
    │   ├── tokens.rs        # TokenSet
    │   ├── jwt.rs           # extract_tenant_id
    │   ├── store.rs         # KeychainStore (load/save/delete)
    │   ├── device_code.rs   # start + poll (RFC 8628 §3)
    │   └── refresh.rs       # refresh_token grant
    └── graph/
        ├── mod.rs           # GraphClient
        ├── me.rs            # GET /me
        └── mail.rs          # GET /me/mailFolders/inbox/messages
```

### Modified files in `crates/pidge/`
```
crates/pidge/Cargo.toml          # add pidge-core, pidge-client deps
src/cli.rs                       # add Auth + Inbox subcommands + OutputFormat
src/commands/mod.rs              # add module declarations
src/commands/auth.rs             # new — top-level dispatcher
src/commands/auth_login.rs       # new
src/commands/auth_list.rs        # new
src/commands/auth_status.rs      # new
src/commands/auth_logout.rs      # new
src/commands/auth_default.rs     # new
src/commands/inbox.rs            # new
```

### Other new/modified files
```
Cargo.toml                              # add workspace.members + workspace.dependencies
scripts/register-pidge-app.sh           # new
scripts/pidge-app-permissions.json      # new
DEVELOPMENT.md                          # new (portal walkthrough fallback)
.github/workflows/release.yml           # publish three crates in dep order
.claude/skills/release/SKILL.md         # mention internal crate version bumps
CHANGELOG.md                            # [Unreleased] entry
README.md                                # add "Account setup" + "Inbox" sections
CLAUDE.md                                # update architecture for 3-crate layout
```

---

## Task 1: Add new workspace dependencies and create `pidge-core` skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/pidge-core/Cargo.toml`
- Create: `crates/pidge-core/src/lib.rs`

This task creates the empty crate and registers it in the workspace. No `pidge-core` code yet — that's the next tasks. The pre-existing `crates/pidge` is unaffected.

- [ ] **Step 1: Add new workspace dependencies and member to root `Cargo.toml`**

Open `/Users/kristofer/repos/mklab-se/pidge/Cargo.toml`. Replace the `members` list with both crates and add the new dependencies in the appropriate sections of `[workspace.dependencies]`. The diff:

```toml
[workspace]
resolver = "2"
members = ["crates/pidge", "crates/pidge-core"]

[workspace.package]
# unchanged
```

Then append new entries to `[workspace.dependencies]` (preserve all existing ones; only the additions below are new):

```toml
# Keychain
keyring = "3"

# Base64 (for JWT decoding)
base64 = "0.22"

# YAML for config files
serde_yaml = "0.9"

# Test-only HTTP mock
wiremock = "0.6"

# Table output
comfy-table = "7"

# Interactive prompts
inquire = "0.7"

# Future combinators (for join_all in multi-account fetch)
futures = "0.3"

# URL handling
url = "2"

# Internal crates
pidge-core = { version = "0.1.0", path = "crates/pidge-core" }
```

**Order matters for `cargo publish`:** internal crate entries must follow the external pins so the `version = "X.Y.Z"` form is grouped with other path-based internal deps later. Place the `pidge-core` entry as the last entry in `[workspace.dependencies]`.

- [ ] **Step 2: Create `crates/pidge-core/Cargo.toml`**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/Cargo.toml`:

```toml
[package]
name = "pidge-core"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "Core types for the pidge CLI: accounts, configuration, normalized message model"
readme = "../../README.md"
keywords = ["pidge", "email", "calendar"]
categories = ["command-line-utilities", "email"]

[dependencies]
serde.workspace = true
serde_yaml.workspace = true
chrono.workspace = true
thiserror.workspace = true
dirs.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`tempfile` is not yet in workspace.dependencies. Add it to `Cargo.toml` at the root:

```toml
# Testing
tempfile = "3.10"
```

(Place it near the bottom with other dev-time deps. The foundation hadn't added it yet.)

- [ ] **Step 3: Create the empty `crates/pidge-core/src/lib.rs`**

```rust
//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.
```

(Single doc comment, no exports yet. Subsequent tasks fill in modules.)

- [ ] **Step 4: Build the workspace**

Run: `cargo build --workspace`
Expected: clean build, no warnings. `pidge-core` compiles as an empty library.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/pidge-core/Cargo.toml crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core skeleton and workspace dependencies for M365 auth"
```

---

## Task 2: Create `pidge-client` skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/pidge-client/Cargo.toml`
- Create: `crates/pidge-client/src/lib.rs`

Same shape as Task 1: empty crate, registered in the workspace. Subsequent tasks fill it.

- [ ] **Step 1: Add `pidge-client` to workspace members and dependencies**

Open `/Users/kristofer/repos/mklab-se/pidge/Cargo.toml`. Modify the `members` line to:

```toml
members = ["crates/pidge", "crates/pidge-core", "crates/pidge-client"]
```

Add to the end of `[workspace.dependencies]`:

```toml
pidge-client = { version = "0.1.0", path = "crates/pidge-client" }
```

- [ ] **Step 2: Create `crates/pidge-client/Cargo.toml`**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/Cargo.toml`:

```toml
[package]
name = "pidge-client"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "Microsoft 365 / Graph client and OAuth flows for the pidge CLI"
readme = "../../README.md"
keywords = ["pidge", "microsoft-graph", "oauth", "m365"]
categories = ["command-line-utilities", "email", "authentication"]

[dependencies]
pidge-core.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
serde_yaml.workspace = true
tokio.workspace = true
anyhow.workspace = true
thiserror.workspace = true
chrono.workspace = true
tracing.workspace = true
keyring = { workspace = true, features = ["apple-native", "windows-native", "linux-native"] }
base64.workspace = true
url.workspace = true

[dev-dependencies]
wiremock.workspace = true
tempfile.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

The `keyring` features pin the native backends per OS so the dependency footprint is small and explicit.

- [ ] **Step 3: Create the empty `crates/pidge-client/src/lib.rs`**

```rust
//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.
```

- [ ] **Step 4: Build the workspace**

Run: `cargo build --workspace`
Expected: clean build, no warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/pidge-client/Cargo.toml crates/pidge-client/src/lib.rs
git commit -m "Add pidge-client skeleton with Microsoft Graph and OAuth dependencies"
```

---

## Task 3: `pidge-core::error` and `pidge-core::account`

**Files:**
- Create: `crates/pidge-core/src/error.rs`
- Create: `crates/pidge-core/src/account.rs`
- Modify: `crates/pidge-core/src/lib.rs`

- [ ] **Step 1: Write the failing test for `Account::is_personal`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/account.rs`:

```rust
//! Account types — represents a single Microsoft account signed into pidge.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A signed-in Microsoft account.
///
/// This is metadata only — no tokens. Tokens live in the OS keychain,
/// keyed by `email`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub tenant_id: String,
    pub home_account_id: String,
    pub added_at: DateTime<Utc>,
}

impl Account {
    /// Well-known tenant ID for personal Microsoft accounts (outlook.com, live.com, hotmail.com).
    /// Microsoft documents this as the "MSA" tenant.
    pub const PERSONAL_MSA_TENANT: &'static str = "9188040d-6c67-4c5b-b112-36a304b66dad";

    /// True if this account is a personal Microsoft account.
    pub fn is_personal(&self) -> bool {
        self.tenant_id == Self::PERSONAL_MSA_TENANT
    }

    /// A short human label for the tenant — "personal MSA" for MSA, GUID prefix otherwise.
    pub fn tenant_label(&self) -> String {
        if self.is_personal() {
            "personal MSA".to_string()
        } else {
            let prefix: String = self.tenant_id.chars().take(8).collect();
            format!("{prefix}…")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(tenant_id: &str) -> Account {
        Account {
            email: "x@example.com".into(),
            tenant_id: tenant_id.into(),
            home_account_id: "home".into(),
            added_at: DateTime::parse_from_rfc3339("2026-05-13T22:00:00Z")
                .unwrap()
                .to_utc(),
        }
    }

    #[test]
    fn personal_msa_tenant_is_recognised() {
        assert!(make_account(Account::PERSONAL_MSA_TENANT).is_personal());
    }

    #[test]
    fn org_tenant_is_not_personal() {
        assert!(!make_account("11111111-2222-3333-4444-555555555555").is_personal());
    }

    #[test]
    fn tenant_label_for_msa() {
        assert_eq!(
            make_account(Account::PERSONAL_MSA_TENANT).tenant_label(),
            "personal MSA"
        );
    }

    #[test]
    fn tenant_label_for_org_truncates_to_8_chars() {
        assert_eq!(
            make_account("11111111-2222-3333-4444-555555555555").tenant_label(),
            "11111111…"
        );
    }
}
```

- [ ] **Step 2: Create `error.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/error.rs`:

```rust
//! Error type for `pidge-core`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("config I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse: {0}")]
    Parse(#[from] serde_yaml::Error),

    #[error("unknown account: {email}")]
    UnknownAccount { email: String },

    #[error("config directory unavailable on this platform")]
    NoConfigDir,
}
```

- [ ] **Step 3: Wire modules in `lib.rs`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/lib.rs` to:

```rust
//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.

mod account;
mod error;

pub use account::Account;
pub use error::CoreError;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p pidge-core`
Expected: `4 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-core/src/error.rs crates/pidge-core/src/account.rs crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core Account type and CoreError"
```

---

## Task 4: `pidge-core::message`

**Files:**
- Create: `crates/pidge-core/src/message.rs`
- Modify: `crates/pidge-core/src/lib.rs`

The normalized provider-agnostic message type. Microsoft Graph JSON is mapped *to* this in `pidge-client::graph::mail`; future Gmail support maps to the same type.

- [ ] **Step 1: Write the message module**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/message.rs`:

```rust
//! Normalized message representation, provider-agnostic.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One message in a mailbox, as presented to commands and output formatters.
///
/// `account` is the email of the signed-in account this message belongs to,
/// so multi-account merges can show provenance in a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub account: String,
    pub id: String,
    pub from: MessageFrom,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub is_read: bool,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFrom {
    pub name: String,
    pub address: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrips_through_json() {
        let m = Message {
            account: "a@b.com".into(),
            id: "id-1".into(),
            from: MessageFrom {
                name: "Maria Lindberg".into(),
                address: "maria@mklab.se".into(),
            },
            subject: "Quarterly numbers".into(),
            received_at: DateTime::parse_from_rfc3339("2026-05-13T22:00:00Z")
                .unwrap()
                .to_utc(),
            is_read: false,
            preview: "Hi, attached are…".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(m, m2);
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

Edit `crates/pidge-core/src/lib.rs` to:

```rust
//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.

mod account;
mod error;
mod message;

pub use account::Account;
pub use error::CoreError;
pub use message::{Message, MessageFrom};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-core`
Expected: `5 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-core/src/message.rs crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core Message type for normalized mail rows"
```

The `serde_json` dependency the message test uses already comes in transitively via `serde_yaml`. If `cargo test` reports it as unresolved, add `serde_json.workspace = true` to `crates/pidge-core/Cargo.toml`'s `[dev-dependencies]` block.

---

## Task 5: `pidge-core::config`

**Files:**
- Create: `crates/pidge-core/src/config.rs`
- Modify: `crates/pidge-core/src/lib.rs`

The config schema, load/save logic, and account/default mutators.

- [ ] **Step 1: Write the config module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/config.rs`:

```rust
//! Persistent configuration file for pidge.
//!
//! Path: `${XDG_CONFIG_HOME:-~/.config}/pidge/config.yaml`.
//! Contains only non-sensitive metadata — tokens live in the OS keychain.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::error::CoreError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub defaults: Defaults,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub send: Option<String>,
    pub calendar: Option<String>,
}

impl Config {
    /// Default path: `${XDG_CONFIG_HOME:-~/.config}/pidge/config.yaml`.
    pub fn default_path() -> Result<PathBuf, CoreError> {
        let dir = dirs::config_dir().ok_or(CoreError::NoConfigDir)?.join("pidge");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("config.yaml"))
    }

    /// Load the config from the default path. If the file doesn't exist, returns `Config::default()`.
    pub fn load() -> Result<Self, CoreError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path. Useful for tests.
    pub fn load_from(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Save the config to the default path.
    pub fn save(&self) -> Result<(), CoreError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    /// Save to a specific path. Useful for tests.
    pub fn save_to(&self, path: &Path) -> Result<(), CoreError> {
        let text = serde_yaml::to_string(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Add or replace an account by email. If this is the first account,
    /// also sets it as the default send AND default calendar account.
    pub fn add_account(&mut self, account: Account) {
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.email == account.email) {
            *existing = account;
            return;
        }
        if self.accounts.is_empty() {
            self.defaults.send = Some(account.email.clone());
            self.defaults.calendar = Some(account.email.clone());
        }
        self.accounts.push(account);
    }

    /// Remove an account by email. Returns the removed account.
    /// If the removed account was a default, that default is cleared.
    pub fn remove_account(&mut self, email: &str) -> Option<Account> {
        let idx = self.accounts.iter().position(|a| a.email == email)?;
        let removed = self.accounts.remove(idx);
        if self.defaults.send.as_deref() == Some(email) {
            self.defaults.send = None;
        }
        if self.defaults.calendar.as_deref() == Some(email) {
            self.defaults.calendar = None;
        }
        Some(removed)
    }

    /// Set the default send account. Errors if the email isn't a known signed-in account.
    pub fn set_default_send(&mut self, email: &str) -> Result<(), CoreError> {
        if !self.accounts.iter().any(|a| a.email == email) {
            return Err(CoreError::UnknownAccount {
                email: email.to_string(),
            });
        }
        self.defaults.send = Some(email.to_string());
        Ok(())
    }

    /// Set the default calendar account. Errors if the email isn't a known signed-in account.
    pub fn set_default_calendar(&mut self, email: &str) -> Result<(), CoreError> {
        if !self.accounts.iter().any(|a| a.email == email) {
            return Err(CoreError::UnknownAccount {
                email: email.to_string(),
            });
        }
        self.defaults.calendar = Some(email.to_string());
        Ok(())
    }

    /// Find an account by email.
    pub fn find(&self, email: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.email == email)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_account(email: &str) -> Account {
        Account {
            email: email.into(),
            tenant_id: "tid".into(),
            home_account_id: "home".into(),
            added_at: chrono::Utc.with_ymd_and_hms(2026, 5, 13, 22, 0, 0).unwrap(),
        }
    }

    #[test]
    fn empty_config_serializes_and_deserializes() {
        let c = Config::default();
        let yaml = serde_yaml::to_string(&c).unwrap();
        let c2: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(c, c2);
    }

    #[test]
    fn first_added_account_becomes_both_defaults() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        assert_eq!(c.defaults.send.as_deref(), Some("a@b.com"));
        assert_eq!(c.defaults.calendar.as_deref(), Some("a@b.com"));
    }

    #[test]
    fn second_added_account_does_not_change_defaults() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        c.add_account(make_account("c@d.com"));
        assert_eq!(c.defaults.send.as_deref(), Some("a@b.com"));
        assert_eq!(c.defaults.calendar.as_deref(), Some("a@b.com"));
        assert_eq!(c.accounts.len(), 2);
    }

    #[test]
    fn removing_default_account_clears_default() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        c.add_account(make_account("c@d.com"));
        c.remove_account("a@b.com");
        assert_eq!(c.defaults.send, None);
        assert_eq!(c.defaults.calendar, None);
    }

    #[test]
    fn set_default_send_for_unknown_account_errors() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        assert!(matches!(
            c.set_default_send("ghost@nowhere.com"),
            Err(CoreError::UnknownAccount { .. })
        ));
    }

    #[test]
    fn config_roundtrips_through_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");

        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        c.add_account(make_account("c@d.com"));
        c.set_default_calendar("c@d.com").unwrap();
        c.save_to(&path).unwrap();

        let c2 = Config::load_from(&path).unwrap();
        assert_eq!(c, c2);
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

Edit `crates/pidge-core/src/lib.rs` to:

```rust
//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.

mod account;
mod config;
mod error;
mod message;

pub use account::Account;
pub use config::{Config, Defaults};
pub use error::CoreError;
pub use message::{Message, MessageFrom};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-core`
Expected: `11 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-core/src/config.rs crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core Config with accounts, defaults, and YAML persistence"
```

---

## Task 6: `pidge-client::error` and `pidge-client::auth::config`

**Files:**
- Create: `crates/pidge-client/src/error.rs`
- Create: `crates/pidge-client/src/auth/mod.rs` (initial — minimal, expanded in Task 12)
- Create: `crates/pidge-client/src/auth/config.rs`
- Modify: `crates/pidge-client/src/lib.rs`

- [ ] **Step 1: Create `error.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/error.rs`:

```rust
//! Error type for `pidge-client`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("pidge has not been provisioned yet. The maintainer needs to run scripts/register-pidge-app.sh and update APP_CLIENT_ID.")]
    NotProvisioned,

    #[error("OS keychain unavailable: {0}")]
    Keychain(#[from] keyring::Error),

    #[error("device code flow: authorization pending exceeded poll deadline")]
    DeviceCodeTimeout,

    #[error("device code flow: user denied consent")]
    DeviceCodeAccessDenied,

    #[error("device code flow: {kind}{}", description.as_ref().map(|d| format!(" — {d}")).unwrap_or_default())]
    DeviceCodeOther {
        kind: String,
        description: Option<String>,
    },

    #[error("session expired for {email}. Run `pidge auth login` to re-add this account.")]
    SessionExpired { email: String },

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Microsoft Graph: {status} {message}")]
    Graph { status: u16, message: String },

    #[error("core: {0}")]
    Core(#[from] pidge_core::CoreError),

    #[error("token response missing access_token")]
    MissingAccessToken,
}
```

- [ ] **Step 2: Create `auth/config.rs`**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/config.rs`:

```rust
//! Compile-time constants and runtime overrides for the pidge OAuth app.

/// The pidge app's `client_id` in Microsoft Entra.
///
/// Empty string means "not yet provisioned". Set by `scripts/register-pidge-app.sh`
/// after registering the app in Entra. Until then, set the `PIDGE_CLIENT_ID` env var
/// for development.
pub const APP_CLIENT_ID: &str = "";

/// Microsoft Graph delegated scopes pidge requests at sign-in.
/// Locked in at app registration time — changing them later requires updating
/// the Entra app permissions AND triggering incremental consent on existing accounts.
pub const SCOPES: &[&str] = &[
    "offline_access",
    "User.Read",
    "Mail.ReadWrite",
    "Mail.Send",
    "Calendars.ReadWrite",
];

/// Microsoft identity platform endpoints (common = multi-tenant + personal MSA).
pub const AUTHORITY: &str = "https://login.microsoftonline.com/common";
pub const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/devicecode";
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// Microsoft Graph base URL.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Resolved client_id: env var wins, otherwise the compile-time constant (if non-empty).
pub fn client_id() -> Option<String> {
    if let Ok(v) = std::env::var("PIDGE_CLIENT_ID") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if APP_CLIENT_ID.is_empty() {
        None
    } else {
        Some(APP_CLIENT_ID.to_string())
    }
}

/// The space-separated scope string sent to Microsoft.
pub fn scope_string() -> String {
    SCOPES.join(" ")
}
```

- [ ] **Step 3: Create `auth/mod.rs` (minimal)**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
```

(Will be expanded in Task 12 once submodules exist.)

- [ ] **Step 4: Wire `error` and `auth` into `lib.rs`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/lib.rs`:

```rust
//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.

pub mod auth;
mod error;

pub use error::ClientError;
```

- [ ] **Step 5: Build**

Run: `cargo build -p pidge-client`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/pidge-client/src/error.rs crates/pidge-client/src/auth crates/pidge-client/src/lib.rs
git commit -m "Add pidge-client ClientError and auth::config constants"
```

---

## Task 7: `pidge-client::auth::tokens`

**Files:**
- Create: `crates/pidge-client/src/auth/tokens.rs`
- Modify: `crates/pidge-client/src/auth/mod.rs`

The `TokenSet` is what gets serialized into the keychain entry.

- [ ] **Step 1: Write the module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/tokens.rs`:

```rust
//! Token storage shape — what gets serialized into the keychain.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A user's OAuth tokens for one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

impl TokenSet {
    /// True if the access token is within 60 seconds of expiring (or already expired).
    /// We refresh before this threshold to absorb clock skew.
    pub fn needs_refresh(&self) -> bool {
        Utc::now() + Duration::seconds(60) >= self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_does_not_need_refresh() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        };
        assert!(!t.needs_refresh());
    }

    #[test]
    fn token_expiring_within_60s_needs_refresh() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now() + Duration::seconds(30),
        };
        assert!(t.needs_refresh());
    }

    #[test]
    fn already_expired_token_needs_refresh() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now() - Duration::seconds(10),
        };
        assert!(t.needs_refresh());
    }

    #[test]
    fn tokens_roundtrip_through_json() {
        let t = TokenSet {
            access_token: "ey…".into(),
            refresh_token: "M.C5…".into(),
            expires_at: DateTime::parse_from_rfc3339("2026-05-13T23:00:00Z")
                .unwrap()
                .to_utc(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let t2: TokenSet = serde_json::from_str(&json).unwrap();
        assert_eq!(t, t2);
    }
}
```

- [ ] **Step 2: Wire into `auth/mod.rs`**

Edit `crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
mod tokens;

pub use tokens::TokenSet;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-client tokens`
Expected: `4 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-client/src/auth/tokens.rs crates/pidge-client/src/auth/mod.rs
git commit -m "Add pidge-client TokenSet with refresh threshold"
```

---

## Task 8: `pidge-client::auth::jwt`

**Files:**
- Create: `crates/pidge-client/src/auth/jwt.rs`
- Modify: `crates/pidge-client/src/auth/mod.rs`

Minimal JWT decoder — extracts the `tid` claim from the middle segment without signature verification (we trust the TLS path).

- [ ] **Step 1: Write the module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/jwt.rs`:

```rust
//! Minimal JWT decoder — extracts the `tid` (tenant_id) claim from id_tokens.
//!
//! We don't verify the signature: we trust the token because we just received it
//! over TLS from `login.microsoftonline.com`. The decoder is base64url-without-padding,
//! which is what Microsoft uses.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    tid: Option<String>,
}

/// Extract the `tid` claim from a JWT. Returns `None` if the JWT is malformed
/// or if `tid` is missing.
pub fn extract_tenant_id(jwt: &str) -> Option<String> {
    let mid = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(mid).ok()?;
    let claims: Claims = serde_json::from_slice(&bytes).ok()?;
    claims.tid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload_json: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json);
        let signature = URL_SAFE_NO_PAD.encode("dummy-signature");
        format!("{header}.{payload}.{signature}")
    }

    #[test]
    fn extracts_tid_from_valid_jwt() {
        let jwt = make_jwt(
            r#"{"tid":"11111111-2222-3333-4444-555555555555","iss":"https://login.microsoftonline.com/.."}"#,
        );
        assert_eq!(
            extract_tenant_id(&jwt),
            Some("11111111-2222-3333-4444-555555555555".to_string())
        );
    }

    #[test]
    fn returns_none_when_tid_is_missing() {
        let jwt = make_jwt(r#"{"iss":"https://login.microsoftonline.com/.."}"#);
        assert_eq!(extract_tenant_id(&jwt), None);
    }

    #[test]
    fn returns_none_for_malformed_jwt() {
        assert_eq!(extract_tenant_id("not-a-jwt"), None);
    }

    #[test]
    fn returns_none_for_non_base64_middle_segment() {
        assert_eq!(extract_tenant_id("a.this-isn't-base64-because-of-?-char.b"), None);
    }
}
```

- [ ] **Step 2: Wire into `auth/mod.rs`**

Edit `crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
mod jwt;
mod tokens;

pub use jwt::extract_tenant_id;
pub use tokens::TokenSet;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-client jwt`
Expected: `4 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-client/src/auth/jwt.rs crates/pidge-client/src/auth/mod.rs
git commit -m "Add pidge-client JWT tid extractor for id_token decoding"
```

---

## Task 9: `pidge-client::auth::store`

**Files:**
- Create: `crates/pidge-client/src/auth/store.rs`
- Modify: `crates/pidge-client/src/auth/mod.rs`

OS keychain access for `TokenSet`. Service `pidge`, account is the email.

- [ ] **Step 1: Write the module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/store.rs`:

```rust
//! OS keychain access for OAuth tokens.
//!
//! Service name: "pidge". Account name: the user's email.
//! The value is a JSON-serialized `TokenSet`.

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

const SERVICE_NAME: &str = "pidge";

pub struct KeychainStore;

impl KeychainStore {
    fn entry(email: &str) -> Result<keyring::Entry, ClientError> {
        keyring::Entry::new(SERVICE_NAME, email).map_err(ClientError::Keychain)
    }

    /// Load tokens for an email. Returns `None` if there's no entry for that account.
    pub fn load(email: &str) -> Result<Option<TokenSet>, ClientError> {
        let entry = Self::entry(email)?;
        match entry.get_password() {
            Ok(blob) => {
                let tokens: TokenSet = serde_json::from_str(&blob)?;
                Ok(Some(tokens))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ClientError::Keychain(e)),
        }
    }

    /// Save tokens for an email, overwriting any existing entry.
    pub fn save(email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        let blob = serde_json::to_string(tokens)?;
        let entry = Self::entry(email)?;
        entry.set_password(&blob).map_err(ClientError::Keychain)
    }

    /// Remove tokens for an email. No-op if no entry exists.
    pub fn delete(email: &str) -> Result<(), ClientError> {
        let entry = Self::entry(email)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ClientError::Keychain(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    // Keychain tests are platform-dependent and require credential-store backends.
    // We trust the `keyring` crate's own integration tests for backend correctness
    // and limit ourselves to a serialization-only test that doesn't touch the OS.

    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn token_blob_serializes_and_deserializes() {
        let t = TokenSet {
            access_token: "abc".into(),
            refresh_token: "xyz".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        };
        let blob = serde_json::to_string(&t).unwrap();
        let t2: TokenSet = serde_json::from_str(&blob).unwrap();
        assert_eq!(t, t2);
    }
}
```

Note: live keychain integration is verified manually during end-to-end testing (Task 27, smoke test). The `keyring` crate's `mock` feature exists but requires explicit setup via `keyring::set_default_credential_builder(keyring::mock::default_credential_builder())`, which conflicts with the live keychain backends loaded by feature flags. We skip the mock-backend test here.

- [ ] **Step 2: Wire into `auth/mod.rs`**

Edit `crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
mod jwt;
mod store;
mod tokens;

pub use jwt::extract_tenant_id;
pub use store::KeychainStore;
pub use tokens::TokenSet;
```

- [ ] **Step 3: Build and run tests**

Run: `cargo test -p pidge-client store`
Expected: `1 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-client/src/auth/store.rs crates/pidge-client/src/auth/mod.rs
git commit -m "Add pidge-client KeychainStore for OAuth token persistence"
```

---

## Task 10: `pidge-client::auth::device_code`

**Files:**
- Create: `crates/pidge-client/src/auth/device_code.rs`
- Modify: `crates/pidge-client/src/auth/mod.rs`

Hand-rolled implementation of RFC 8628 device authorization grant. Two functions: `start` (POST to `/devicecode`) and `poll` (POST to `/token` in a loop until success or terminal error).

- [ ] **Step 1: Write the module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/device_code.rs`:

```rust
//! RFC 8628 OAuth 2.0 device authorization grant flow.

use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

/// Response from the devicecode endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    #[serde(default)]
    pub message: Option<String>,
}

/// Token response. `refresh_token` is optional because refresh-token-rotation
/// responses sometimes omit it; for device-code initial responses Microsoft always
/// includes it (the `offline_access` scope is requested).
#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Result of a successful poll: tokens + the id_token (for tenant_id extraction).
pub struct PollSuccess {
    pub tokens: TokenSet,
    pub id_token: Option<String>,
}

/// POST to `{base_url}/oauth2/v2.0/devicecode`.
pub async fn start(
    client: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    scope: &str,
) -> Result<DeviceCodeResponse, ClientError> {
    let url = format!("{base_url}/oauth2/v2.0/devicecode");
    let resp = client
        .post(&url)
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }

    Ok(resp.json::<DeviceCodeResponse>().await?)
}

/// Poll `{base_url}/oauth2/v2.0/token` per RFC 8628 §3.5.
/// Returns when the user has approved consent, or with an error on denial / expiry.
///
/// The `sleep` parameter is a closure so tests can inject a fast no-op sleep
/// instead of real `tokio::time::sleep`.
pub async fn poll<F, Fut>(
    client: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    device_code: &str,
    initial_interval: u64,
    expires_in: u64,
    mut sleep: F,
) -> Result<PollSuccess, ClientError>
where
    F: FnMut(Duration) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let url = format!("{base_url}/oauth2/v2.0/token");
    let mut interval = initial_interval;
    let deadline = Utc::now() + chrono::Duration::seconds(expires_in as i64);

    loop {
        if Utc::now() >= deadline {
            return Err(ClientError::DeviceCodeTimeout);
        }
        sleep(Duration::from_secs(interval)).await;

        let resp = client
            .post(&url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device_code),
            ])
            .send()
            .await?;

        let status = resp.status();
        let body = resp.bytes().await?;

        if status.is_success() {
            let tr: TokenResponse = serde_json::from_slice(&body)?;
            let expires = tr.expires_in.unwrap_or(3600);
            let refresh_token = tr.refresh_token.ok_or(ClientError::MissingAccessToken)?;
            return Ok(PollSuccess {
                tokens: TokenSet {
                    access_token: tr.access_token,
                    refresh_token,
                    expires_at: Utc::now() + chrono::Duration::seconds(expires as i64 - 60),
                },
                id_token: tr.id_token,
            });
        }

        let err: ErrorResponse = serde_json::from_slice(&body).map_err(|_| ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&body).into_owned(),
        })?;

        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += 5;
                continue;
            }
            "access_denied" => return Err(ClientError::DeviceCodeAccessDenied),
            "expired_token" => return Err(ClientError::DeviceCodeTimeout),
            other => {
                return Err(ClientError::DeviceCodeOther {
                    kind: other.to_string(),
                    description: err.error_description,
                });
            }
        }
    }
}

/// Real-time sleep helper for production use.
pub async fn real_sleep(d: Duration) {
    tokio::time::sleep(d).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Test sleep: no-op; we don't actually want to wait in tests.
    async fn no_sleep(_: Duration) {}

    #[tokio::test]
    async fn start_returns_device_code_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/devicecode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "DC123",
                "user_code": "ABCD-1234",
                "verification_uri": "https://microsoft.com/devicelogin",
                "expires_in": 900,
                "interval": 5,
                "message": "Please sign in"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let resp = start(&client, &server.uri(), "CID", "openid").await.unwrap();
        assert_eq!(resp.user_code, "ABCD-1234");
        assert_eq!(resp.interval, 5);
    }

    #[tokio::test]
    async fn poll_returns_tokens_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT123",
                "refresh_token": "RT123",
                "expires_in": 3600,
                "id_token": "eyJh.eyJ0aWQiOiJ0aWQifQ.sig"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 1, 60, no_sleep).await.unwrap();
        assert_eq!(result.tokens.access_token, "AT123");
        assert_eq!(result.tokens.refresh_token, "RT123");
        assert_eq!(result.id_token.as_deref(), Some("eyJh.eyJ0aWQiOiJ0aWQifQ.sig"));
    }

    #[tokio::test]
    async fn poll_retries_on_authorization_pending_then_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending"
            })))
            .up_to_n_times(2)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT",
                "refresh_token": "RT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep).await.unwrap();
        assert_eq!(result.tokens.access_token, "AT");
    }

    #[tokio::test]
    async fn poll_returns_access_denied_on_user_cancel() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep).await;
        assert!(matches!(result, Err(ClientError::DeviceCodeAccessDenied)));
    }

    #[tokio::test]
    async fn poll_returns_timeout_on_expired_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "expired_token"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep).await;
        assert!(matches!(result, Err(ClientError::DeviceCodeTimeout)));
    }
}
```

- [ ] **Step 2: Wire into `auth/mod.rs`**

Edit `crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
pub mod device_code;
mod jwt;
mod store;
mod tokens;

pub use jwt::extract_tenant_id;
pub use store::KeychainStore;
pub use tokens::TokenSet;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-client device_code`
Expected: `5 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-client/src/auth/device_code.rs crates/pidge-client/src/auth/mod.rs
git commit -m "Add pidge-client device code flow (RFC 8628) with polling and error handling"
```

---

## Task 11: `pidge-client::auth::refresh`

**Files:**
- Create: `crates/pidge-client/src/auth/refresh.rs`
- Modify: `crates/pidge-client/src/auth/mod.rs`

Refresh-token grant. Handles `invalid_grant` (expired refresh token) as a typed error.

- [ ] **Step 1: Write the module with tests**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/refresh.rs`:

```rust
//! OAuth refresh-token grant.

use chrono::Utc;

use crate::auth::device_code::{ErrorResponse, TokenResponse};
use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

/// Refresh the access token using the stored refresh token.
///
/// Returns the new `TokenSet`. If Microsoft rotates the refresh token, it's
/// included in the response and used. If not, the current refresh token is preserved.
///
/// Errors:
/// - `ClientError::SessionExpired` if Microsoft returns `invalid_grant`
/// - `ClientError::Graph` for other HTTP errors
pub async fn refresh(
    client: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    current: &TokenSet,
    scope: &str,
    email: &str,
) -> Result<TokenSet, ClientError> {
    let url = format!("{base_url}/oauth2/v2.0/token");
    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", &current.refresh_token),
            ("scope", scope),
        ])
        .send()
        .await?;

    let status = resp.status();
    let body = resp.bytes().await?;

    if !status.is_success() {
        let err: ErrorResponse = serde_json::from_slice(&body).map_err(|_| ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&body).into_owned(),
        })?;
        if err.error == "invalid_grant" {
            return Err(ClientError::SessionExpired {
                email: email.to_string(),
            });
        }
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: err.error_description.unwrap_or(err.error),
        });
    }

    let tr: TokenResponse = serde_json::from_slice(&body)?;
    let expires = tr.expires_in.unwrap_or(3600);
    let new_refresh = tr.refresh_token.unwrap_or_else(|| current.refresh_token.clone());

    Ok(TokenSet {
        access_token: tr.access_token,
        refresh_token: new_refresh,
        expires_at: Utc::now() + chrono::Duration::seconds(expires as i64 - 60),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn old_tokens() -> TokenSet {
        TokenSet {
            access_token: "OLD_AT".into(),
            refresh_token: "OLD_RT".into(),
            expires_at: Utc::now() - Duration::seconds(60),
        }
    }

    #[tokio::test]
    async fn refresh_returns_new_tokens_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "refresh_token": "NEW_RT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let new = refresh(&client, &server.uri(), "CID", &old_tokens(), "scope", "u@e.com")
            .await
            .unwrap();
        assert_eq!(new.access_token, "NEW_AT");
        assert_eq!(new.refresh_token, "NEW_RT");
    }

    #[tokio::test]
    async fn refresh_preserves_refresh_token_when_response_omits_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let new = refresh(&client, &server.uri(), "CID", &old_tokens(), "scope", "u@e.com")
            .await
            .unwrap();
        assert_eq!(new.access_token, "NEW_AT");
        assert_eq!(new.refresh_token, "OLD_RT");
    }

    #[tokio::test]
    async fn refresh_returns_session_expired_on_invalid_grant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "AADSTS50173: refresh token expired"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = refresh(&client, &server.uri(), "CID", &old_tokens(), "scope", "u@e.com")
            .await
            .unwrap_err();
        match err {
            ClientError::SessionExpired { email } => assert_eq!(email, "u@e.com"),
            other => panic!("expected SessionExpired, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Make `ErrorResponse` and `TokenResponse` visible to `refresh.rs`**

Visibility tweak in `crates/pidge-client/src/auth/device_code.rs`: `ErrorResponse` and `TokenResponse` are `pub(crate)` in the spec above — verify the `pub(crate)` is present so `refresh.rs` can use them.

- [ ] **Step 3: Wire into `auth/mod.rs`**

Edit `crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
pub mod device_code;
mod jwt;
pub mod refresh;
mod store;
mod tokens;

pub use jwt::extract_tenant_id;
pub use store::KeychainStore;
pub use tokens::TokenSet;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pidge-client refresh`
Expected: `3 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-client/src/auth/refresh.rs crates/pidge-client/src/auth/mod.rs
git commit -m "Add pidge-client refresh-token grant with session-expired detection"
```

---

## Task 12: `pidge-client::auth` — `AuthClient` façade

**Files:**
- Modify: `crates/pidge-client/src/auth/mod.rs`

`AuthClient` is what the CLI sees. It glues `device_code`, `refresh`, `store`, `jwt`, and `config` together behind a clean API.

- [ ] **Step 1: Replace `auth/mod.rs` with the full façade**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/auth/mod.rs`:

```rust
//! OAuth device-code flow, token refresh, and keychain storage.

pub mod config;
pub mod device_code;
mod jwt;
pub mod refresh;
mod store;
mod tokens;

pub use jwt::extract_tenant_id;
pub use store::KeychainStore;
pub use tokens::TokenSet;

use crate::error::ClientError;

/// High-level auth client. Holds a shared `reqwest::Client` and the resolved
/// `client_id`; provides device-code sign-in and access-token retrieval (with
/// transparent refresh).
pub struct AuthClient {
    http: reqwest::Client,
    client_id: String,
    authority_base: String,
    scope: String,
}

impl AuthClient {
    /// Construct an AuthClient from compile-time/env configuration.
    ///
    /// Errors with `ClientError::NotProvisioned` if no client_id is available.
    pub fn from_env() -> Result<Self, ClientError> {
        let client_id = config::client_id().ok_or(ClientError::NotProvisioned)?;
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
                .build()?,
            client_id,
            authority_base: config::AUTHORITY.to_string(),
            scope: config::scope_string(),
        })
    }

    /// Construct an AuthClient against a specific authority — for tests with wiremock.
    pub fn for_test(client_id: impl Into<String>, authority_base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            client_id: client_id.into(),
            authority_base: authority_base.into(),
            scope: config::scope_string(),
        }
    }

    /// Run the device code flow. Returns the device code response immediately;
    /// caller is expected to display the user code and call `poll_for_tokens`.
    pub async fn start_device_code(&self) -> Result<device_code::DeviceCodeResponse, ClientError> {
        device_code::start(&self.http, &self.authority_base, &self.client_id, &self.scope).await
    }

    /// Poll for tokens after `start_device_code`. Blocks until success or terminal error.
    pub async fn poll_for_tokens(
        &self,
        device_code_resp: &device_code::DeviceCodeResponse,
    ) -> Result<device_code::PollSuccess, ClientError> {
        device_code::poll(
            &self.http,
            &self.authority_base,
            &self.client_id,
            &device_code_resp.device_code,
            device_code_resp.interval,
            device_code_resp.expires_in,
            device_code::real_sleep,
        )
        .await
    }

    /// Get a valid (un-expired) access token for an email, refreshing if necessary.
    /// Returns `ClientError::SessionExpired` if the refresh fails — caller should
    /// prompt the user to `pidge auth login` again for that account.
    pub async fn get_valid_token(&self, email: &str) -> Result<String, ClientError> {
        let tokens = KeychainStore::load(email)?.ok_or_else(|| ClientError::SessionExpired {
            email: email.to_string(),
        })?;

        if !tokens.needs_refresh() {
            return Ok(tokens.access_token);
        }

        let new_tokens = refresh::refresh(
            &self.http,
            &self.authority_base,
            &self.client_id,
            &tokens,
            &self.scope,
            email,
        )
        .await?;
        KeychainStore::save(email, &new_tokens)?;
        Ok(new_tokens.access_token)
    }
}
```

- [ ] **Step 2: Re-export `AuthClient` from `lib.rs`**

Edit `crates/pidge-client/src/lib.rs`:

```rust
//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.

pub mod auth;
mod error;

pub use auth::AuthClient;
pub use error::ClientError;
```

- [ ] **Step 3: Build**

Run: `cargo build -p pidge-client`
Expected: clean.

- [ ] **Step 4: Run full crate tests**

Run: `cargo test -p pidge-client`
Expected: previous tests still pass; total: `13 passed` (4 tokens + 4 jwt + 1 store + 5 device_code + 3 refresh = 17 total — adjust expected count if you reconcile).

Adjust the expected count to match the actual number reported by `cargo test`; the totals above are approximate.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-client/src/auth/mod.rs crates/pidge-client/src/lib.rs
git commit -m "Add pidge-client AuthClient façade for sign-in and token retrieval"
```

---

## Task 13: `pidge-client::graph` — Graph API client

**Files:**
- Create: `crates/pidge-client/src/graph/mod.rs`
- Create: `crates/pidge-client/src/graph/me.rs`
- Create: `crates/pidge-client/src/graph/mail.rs`
- Modify: `crates/pidge-client/src/lib.rs`

Microsoft Graph API client. Two endpoints needed: `GET /me` (post-login identity) and `GET /me/mailFolders/inbox/messages` (inbox list).

- [ ] **Step 1: Create `graph/me.rs`**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/graph`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/graph/me.rs`:

```rust
//! GET /me — fetch the signed-in user's identity.

use serde::Deserialize;

use crate::error::ClientError;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub id: String,
    pub user_principal_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub mail: Option<String>,
}

pub async fn get_me(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
) -> Result<Me, ClientError> {
    let url = format!("{base_url}/me");
    let resp = http
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    Ok(resp.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_me_returns_identity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "user-id",
                "userPrincipalName": "kristofer@mklab.se",
                "displayName": "Kristofer Liljeblad",
                "mail": "kristofer@mklab.se"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let me = get_me(&http, &server.uri(), "AT").await.unwrap();
        assert_eq!(me.id, "user-id");
        assert_eq!(me.user_principal_name, "kristofer@mklab.se");
    }
}
```

- [ ] **Step 2: Create `graph/mail.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/graph/mail.rs`:

```rust
//! GET /me/mailFolders/inbox/messages — list inbox messages.

use pidge_core::{Message, MessageFrom};
use serde::Deserialize;

use crate::error::ClientError;

#[derive(Debug, Deserialize)]
struct GraphMessage {
    id: String,
    subject: Option<String>,
    from: Option<GraphFromWrapper>,
    #[serde(rename = "receivedDateTime")]
    received_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "isRead")]
    is_read: Option<bool>,
    #[serde(rename = "bodyPreview")]
    body_preview: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphFromWrapper {
    #[serde(rename = "emailAddress")]
    email_address: GraphFromAddress,
}

#[derive(Debug, Deserialize)]
struct GraphFromAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphList {
    value: Vec<GraphMessage>,
}

/// List the top N messages in the Inbox folder, sorted by `receivedDateTime desc`.
pub async fn list_inbox(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    limit: usize,
    unread_only: bool,
) -> Result<Vec<Message>, ClientError> {
    let mut url = format!(
        "{base_url}/me/mailFolders/inbox/messages\
         ?$select=id,subject,from,receivedDateTime,isRead,bodyPreview\
         &$orderby=receivedDateTime%20desc\
         &$top={limit}"
    );
    if unread_only {
        url.push_str("&$filter=isRead%20eq%20false");
    }

    let resp = http
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }

    let list: GraphList = resp.json().await?;
    Ok(list
        .value
        .into_iter()
        .map(|g| Message {
            account: account.to_string(),
            id: g.id,
            from: MessageFrom {
                name: g
                    .from
                    .as_ref()
                    .and_then(|f| f.email_address.name.clone())
                    .unwrap_or_default(),
                address: g
                    .from
                    .as_ref()
                    .and_then(|f| f.email_address.address.clone())
                    .unwrap_or_default(),
            },
            subject: g.subject.unwrap_or_default(),
            received_at: g.received_date_time,
            is_read: g.is_read.unwrap_or(true),
            preview: g.body_preview.unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_inbox_parses_graph_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .and(header("authorization", "Bearer AT"))
            .and(query_param("$top", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "AAAA",
                        "subject": "Hello",
                        "from": {
                            "emailAddress": {
                                "name": "Maria",
                                "address": "maria@mklab.se"
                            }
                        },
                        "receivedDateTime": "2026-05-13T22:00:00Z",
                        "isRead": false,
                        "bodyPreview": "Hi there"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let msgs = list_inbox(&http, &server.uri(), "AT", "u@e.com", 5, false)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].subject, "Hello");
        assert_eq!(msgs[0].from.address, "maria@mklab.se");
        assert!(!msgs[0].is_read);
        assert_eq!(msgs[0].account, "u@e.com");
    }

    #[tokio::test]
    async fn list_inbox_adds_filter_when_unread_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .and(query_param("$filter", "isRead eq false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": []
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let msgs = list_inbox(&http, &server.uri(), "AT", "u@e.com", 5, true)
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }
}
```

- [ ] **Step 3: Create `graph/mod.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/graph/mod.rs`:

```rust
//! Microsoft Graph API client.

mod mail;
mod me;

pub use mail::list_inbox;
pub use me::{get_me, Me};

use crate::auth::AuthClient;
use crate::auth::config;
use crate::error::ClientError;
use pidge_core::Message;

/// Stateful Microsoft Graph client. Holds an AuthClient and a shared HTTP client.
pub struct GraphClient {
    auth: AuthClient,
    http: reqwest::Client,
    base_url: String,
}

impl GraphClient {
    pub fn new(auth: AuthClient) -> Result<Self, ClientError> {
        Ok(Self {
            auth,
            http: reqwest::Client::builder()
                .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
                .build()?,
            base_url: config::GRAPH_BASE.to_string(),
        })
    }

    pub fn for_test(auth: AuthClient, base_url: impl Into<String>) -> Self {
        Self {
            auth,
            http: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }

    pub fn auth(&self) -> &AuthClient {
        &self.auth
    }

    /// GET /me. Used right after sign-in to learn the user's email.
    pub async fn me(&self, access_token: &str) -> Result<Me, ClientError> {
        get_me(&self.http, &self.base_url, access_token).await
    }

    /// GET /me/mailFolders/inbox/messages for a given account email.
    /// Acquires/refreshes a token transparently via `AuthClient::get_valid_token`.
    pub async fn list_inbox(
        &self,
        account: &str,
        limit: usize,
        unread_only: bool,
    ) -> Result<Vec<Message>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        list_inbox(&self.http, &self.base_url, &token, account, limit, unread_only).await
    }
}
```

- [ ] **Step 4: Re-export `GraphClient` from `lib.rs`**

Edit `crates/pidge-client/src/lib.rs`:

```rust
//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.

pub mod auth;
mod error;
pub mod graph;

pub use auth::AuthClient;
pub use error::ClientError;
pub use graph::GraphClient;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p pidge-client`
Expected: all previous + 3 new tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/pidge-client/src/graph crates/pidge-client/src/lib.rs
git commit -m "Add pidge-client GraphClient with GET /me and inbox list"
```

---

## Task 14: `pidge` CLI — add Auth and Inbox subcommand definitions

**Files:**
- Modify: `crates/pidge/Cargo.toml` (add new deps)
- Modify: `crates/pidge/src/cli.rs`
- Modify: `crates/pidge/src/commands/mod.rs`

Just the clap-level definitions and `commands/mod.rs` declarations. Command implementations come in Tasks 15–21. The `Cli::run` dispatch for the new commands lands in Task 20 (auth) and Task 21 (inbox).

- [ ] **Step 1: Extend `crates/pidge/Cargo.toml` with new dependencies**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/Cargo.toml` and add these entries (alphabetical inside `[dependencies]`):

```toml
pidge-core.workspace = true
pidge-client.workspace = true
serde_yaml.workspace = true
comfy-table.workspace = true
inquire.workspace = true
futures.workspace = true
```

- [ ] **Step 2: Add Auth + Inbox subcommands to `cli.rs`**

In `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/cli.rs`, find the `Commands` enum and add two new variants between `Ai` and `Completion`:

```rust
    /// Manage Microsoft 365 account authentication
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// View messages in your inbox
    Inbox {
        #[command(subcommand)]
        command: InboxCommands,
    },
```

Then add the new subcommand enums at the bottom of the file (before the `Shell` enum):

```rust
#[derive(clap::Subcommand)]
pub enum AuthCommands {
    /// Sign in to a Microsoft account (interactive device code flow)
    Login,
    /// List signed-in accounts
    List,
    /// Show authentication status and defaults
    Status,
    /// Sign out of one or all accounts
    Logout {
        /// Email of the account to sign out
        #[arg(long)]
        account: Option<String>,
        /// Sign out of every signed-in account
        #[arg(long, conflicts_with = "account")]
        all: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show or set default accounts
    Default {
        /// Set the default sender account
        #[arg(long)]
        send: Option<String>,
        /// Set the default calendar account
        #[arg(long)]
        calendar: Option<String>,
    },
}

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

        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}
```

- [ ] **Step 3: Add placeholder dispatch arms in `Cli::run`**

In the `impl Cli { pub async fn run(self) -> Result<()> { match self.command { ... } } }` block in `cli.rs`, add two arms — just stubs that return `Ok(())` for now. The real handlers land in Tasks 20 and 21.

```rust
            Some(Commands::Auth { command }) => crate::commands::auth::run(command).await,
            Some(Commands::Inbox { command }) => crate::commands::inbox::run(command).await,
```

- [ ] **Step 4: Declare new modules in `commands/mod.rs`**

Edit `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/mod.rs` so it now reads:

```rust
//! CLI command implementations

pub mod ai;
pub mod auth;
pub mod auth_default;
pub mod auth_list;
pub mod auth_login;
pub mod auth_logout;
pub mod auth_status;
pub mod completion;
pub mod inbox;
pub mod skill;
```

- [ ] **Step 5: Create placeholder module files**

The previous step references modules that don't exist yet — placeholders prevent the build from failing while we work through subsequent tasks. Create each with a minimal `pub async fn run(...) -> Result<()> { unimplemented!() }`:

`crates/pidge/src/commands/auth.rs`:

```rust
//! `pidge auth ...` dispatcher.

use anyhow::Result;

use crate::cli::AuthCommands;

pub async fn run(_command: AuthCommands) -> Result<()> {
    unimplemented!("auth dispatcher is implemented in a later task")
}
```

`crates/pidge/src/commands/auth_login.rs`:

```rust
//! `pidge auth login`

use anyhow::Result;

pub async fn run() -> Result<()> {
    unimplemented!("auth_login is implemented in a later task")
}
```

`crates/pidge/src/commands/auth_list.rs`:

```rust
//! `pidge auth list`

use anyhow::Result;

pub fn run() -> Result<()> {
    unimplemented!("auth_list is implemented in a later task")
}
```

`crates/pidge/src/commands/auth_status.rs`:

```rust
//! `pidge auth status`

use anyhow::Result;

pub fn run() -> Result<()> {
    unimplemented!("auth_status is implemented in a later task")
}
```

`crates/pidge/src/commands/auth_logout.rs`:

```rust
//! `pidge auth logout`

use anyhow::Result;

pub fn run(_account: Option<String>, _all: bool, _yes: bool) -> Result<()> {
    unimplemented!("auth_logout is implemented in a later task")
}
```

`crates/pidge/src/commands/auth_default.rs`:

```rust
//! `pidge auth default`

use anyhow::Result;

pub fn run(_send: Option<String>, _calendar: Option<String>) -> Result<()> {
    unimplemented!("auth_default is implemented in a later task")
}
```

`crates/pidge/src/commands/inbox.rs`:

```rust
//! `pidge inbox list`

use anyhow::Result;

use crate::cli::InboxCommands;

pub async fn run(_command: InboxCommands) -> Result<()> {
    unimplemented!("inbox list is implemented in a later task")
}
```

- [ ] **Step 6: Build**

Run: `cargo build --workspace`
Expected: clean. Note: clippy will warn about `unimplemented!` macros in unused arms. That's expected — they're transient placeholders.

- [ ] **Step 7: Smoke-test the CLI definitions parse**

Run: `cargo run -q -- auth --help`
Expected: clap prints the auth subcommand help, listing `login`, `list`, `status`, `logout`, `default`.

Run: `cargo run -q -- inbox --help`
Expected: lists `list`.

Run: `cargo run -q -- inbox list --help`
Expected: shows `--account`, `-n/--limit`, `--unread`, `--output` flags.

- [ ] **Step 8: Commit**

```bash
git add crates/pidge/Cargo.toml crates/pidge/src/cli.rs crates/pidge/src/commands
git commit -m "Add pidge auth/inbox subcommand definitions and placeholder modules"
```

---

## Task 15: `pidge auth login` command

**Files:**
- Modify: `crates/pidge/src/commands/auth_login.rs`

Real implementation of the device code login flow.

- [ ] **Step 1: Write `auth_login.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_login.rs` with:

```rust
//! `pidge auth login` — sign in to a Microsoft account via OAuth device code flow.

use anyhow::{Context, Result};
use chrono::Utc;
use colored::Colorize;

use pidge_client::auth::{extract_tenant_id, AuthClient, KeychainStore};
use pidge_core::{Account, Config};

pub async fn run() -> Result<()> {
    let auth = AuthClient::from_env().context("AuthClient initialisation failed")?;

    println!();
    println!("Adding a new account to pidge.");
    println!();

    let dc = auth
        .start_device_code()
        .await
        .context("failed to start device code flow")?;

    println!("  {}       {}", "Go to:".bold(), dc.verification_uri.cyan());
    println!("  {}  {}", "Enter code:".bold(), dc.user_code.bold().cyan());
    println!();
    println!(
        "{}",
        "Waiting for sign-in… (press Ctrl-C to cancel)".dimmed()
    );

    // Best-effort browser open
    let _ = open_browser(&dc.verification_uri);

    let success = auth
        .poll_for_tokens(&dc)
        .await
        .context("device code flow failed")?;

    // Tenant from id_token
    let tenant_id = success
        .id_token
        .as_deref()
        .and_then(extract_tenant_id)
        .unwrap_or_default();

    // Identity from Graph /me
    let graph = pidge_client::GraphClient::new(AuthClient::from_env()?)?;
    let me = graph
        .me(&success.tokens.access_token)
        .await
        .context("failed to fetch /me")?;
    let email = me
        .mail
        .clone()
        .unwrap_or_else(|| me.user_principal_name.clone());

    // Persist tokens
    KeychainStore::save(&email, &success.tokens)?;

    // Persist account in config
    let mut config = Config::load()?;
    let was_first = config.accounts.is_empty();
    config.add_account(Account {
        email: email.clone(),
        tenant_id,
        home_account_id: me.id,
        added_at: Utc::now(),
    });
    config.save()?;

    println!();
    println!(
        "{} {} <{}>",
        "✔".green(),
        "Signed in as".bold(),
        email
    );
    if was_first {
        println!();
        println!("This is your first account, so pidge has set it as:");
        println!("  • Default send-from account");
        println!("  • Default calendar account");
        println!();
        println!(
            "Change with {} or {}.",
            "`pidge auth default --send <email>`".cyan(),
            "`--calendar <email>`".cyan()
        );
    } else {
        println!("Currently signed in: {} accounts.", config.accounts.len());
    }

    Ok(())
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "start";

    std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p pidge`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/auth_login.rs
git commit -m "Implement pidge auth login (device code flow + first-account defaults)"
```

---

## Task 16: `pidge auth list` command

**Files:**
- Modify: `crates/pidge/src/commands/auth_list.rs`

List signed-in accounts in a table.

- [ ] **Step 1: Write `auth_list.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_list.rs` with:

```rust
//! `pidge auth list` — display signed-in accounts.

use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};

use pidge_core::Config;

pub fn run() -> Result<()> {
    let config = Config::load()?;

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

- [ ] **Step 2: Build**

Run: `cargo build -p pidge`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/auth_list.rs
git commit -m "Implement pidge auth list with defaults markers"
```

---

## Task 17: `pidge auth status` command

**Files:**
- Modify: `crates/pidge/src/commands/auth_status.rs`

- [ ] **Step 1: Write `auth_status.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_status.rs` with:

```rust
//! `pidge auth status` — summary of accounts and defaults.

use anyhow::Result;
use colored::Colorize;

use pidge_core::Config;

pub fn run() -> Result<()> {
    let config = Config::load()?;

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
```

- [ ] **Step 2: Build**

Run: `cargo build -p pidge`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/auth_status.rs
git commit -m "Implement pidge auth status"
```

---

## Task 18: `pidge auth logout` command

**Files:**
- Modify: `crates/pidge/src/commands/auth_logout.rs`

- [ ] **Step 1: Write `auth_logout.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_logout.rs` with:

```rust
//! `pidge auth logout` — remove tokens and account entry from pidge.

use anyhow::{anyhow, Result};
use colored::Colorize;
use inquire::{Confirm, Select};

use pidge_client::auth::KeychainStore;
use pidge_core::Config;

pub fn run(account: Option<String>, all: bool, yes: bool) -> Result<()> {
    let mut config = Config::load()?;

    if config.accounts.is_empty() {
        println!("No accounts signed in.");
        return Ok(());
    }

    if all {
        if !yes {
            let confirmed = Confirm::new(&format!(
                "Sign out of all {} accounts?",
                config.accounts.len()
            ))
            .with_default(false)
            .prompt()?;
            if !confirmed {
                println!("Aborted.");
                return Ok(());
            }
        }
        let emails: Vec<String> = config.accounts.iter().map(|a| a.email.clone()).collect();
        for email in &emails {
            KeychainStore::delete(email)?;
            config.remove_account(email);
        }
        config.save()?;
        println!("{} Signed out of {} accounts.", "✔".green(), emails.len());
        return Ok(());
    }

    // Resolve which email to log out
    let email = match account {
        Some(e) => e,
        None => {
            if config.accounts.len() == 1 {
                config.accounts[0].email.clone()
            } else {
                let options: Vec<String> =
                    config.accounts.iter().map(|a| a.email.clone()).collect();
                Select::new("Which account to sign out?", options).prompt()?
            }
        }
    };

    if config.find(&email).is_none() {
        return Err(anyhow!("not signed in to {email}"));
    }

    if !yes {
        let confirmed = Confirm::new(&format!("Sign out of {email}?"))
            .with_default(false)
            .prompt()?;
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    KeychainStore::delete(&email)?;
    config.remove_account(&email);
    config.save()?;
    println!("{} Signed out of {email}.", "✔".green());
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p pidge`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/auth_logout.rs
git commit -m "Implement pidge auth logout with --account, --all, and confirmation"
```

---

## Task 19: `pidge auth default` command

**Files:**
- Modify: `crates/pidge/src/commands/auth_default.rs`

- [ ] **Step 1: Write `auth_default.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth_default.rs` with:

```rust
//! `pidge auth default` — show or set default accounts.

use anyhow::Result;
use colored::Colorize;

use pidge_core::Config;

pub fn run(send: Option<String>, calendar: Option<String>) -> Result<()> {
    let mut config = Config::load()?;
    let mut changed = false;

    if let Some(email) = send {
        config.set_default_send(&email)?;
        println!("{} Default send account → {email}", "✔".green());
        changed = true;
    }
    if let Some(email) = calendar {
        config.set_default_calendar(&email)?;
        println!("{} Default calendar account → {email}", "✔".green());
        changed = true;
    }

    if changed {
        config.save()?;
        return Ok(());
    }

    // No flags → show current
    println!(
        "send:     {}",
        config.defaults.send.as_deref().unwrap_or("(none)")
    );
    println!(
        "calendar: {}",
        config.defaults.calendar.as_deref().unwrap_or("(none)")
    );
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p pidge`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/auth_default.rs
git commit -m "Implement pidge auth default --send/--calendar"
```

---

## Task 20: `pidge auth` top-level dispatcher

**Files:**
- Modify: `crates/pidge/src/commands/auth.rs`

- [ ] **Step 1: Write `auth.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/auth.rs` with:

```rust
//! `pidge auth ...` dispatcher.

use anyhow::Result;

use crate::cli::AuthCommands;
use crate::commands::{auth_default, auth_list, auth_login, auth_logout, auth_status};

pub async fn run(command: AuthCommands) -> Result<()> {
    match command {
        AuthCommands::Login => auth_login::run().await,
        AuthCommands::List => auth_list::run(),
        AuthCommands::Status => auth_status::run(),
        AuthCommands::Logout { account, all, yes } => auth_logout::run(account, all, yes),
        AuthCommands::Default { send, calendar } => auth_default::run(send, calendar),
    }
}
```

- [ ] **Step 2: Build and smoke-test**

Run: `cargo build -p pidge`
Run: `cargo run -q -- auth status`
Expected: prints `0 accounts signed in.` followed by the suggestion.

- [ ] **Step 3: Commit**

```bash
git add crates/pidge/src/commands/auth.rs
git commit -m "Wire pidge auth dispatcher across all auth subcommands"
```

---

## Task 21: `pidge inbox list` command

**Files:**
- Modify: `crates/pidge/src/commands/inbox.rs`

Multi-account merge with the output renderer.

- [ ] **Step 1: Write `inbox.rs`**

Replace `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/inbox.rs` with:

```rust
//! `pidge inbox list` — list messages merged across signed-in accounts.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use futures::future::join_all;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message};

use crate::cli::{InboxCommands, OutputFormat};

pub async fn run(command: InboxCommands) -> Result<()> {
    match command {
        InboxCommands::List {
            account,
            limit,
            unread,
            output,
        } => list(account, limit, unread, output).await,
    }
}

async fn list(
    account_filter: Vec<String>,
    limit: usize,
    unread_only: bool,
    output: OutputFormat,
) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge auth login` to add one."
        ));
    }

    // Resolve which accounts to query
    let target_emails: Vec<String> = if account_filter.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        // Validate filter — every requested email must be signed in
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

    // Sort by received_at desc, slice to limit
    all_messages.sort_by(|a, b| b.received_at.cmp(&a.received_at));
    all_messages.truncate(limit);

    let single_account = target_emails.len() == 1;

    match output {
        OutputFormat::Text => render_text(&all_messages, single_account),
        OutputFormat::Json => render_json(&all_messages),
    }
}

fn compute_per_account_fetch(limit: usize, num_accounts: usize) -> usize {
    if num_accounts == 0 {
        return limit;
    }
    let calc = (limit as f64 * 1.2 / num_accounts as f64).ceil() as usize;
    calc.max(10)
}

fn render_text(messages: &[Message], hide_account_column: bool) -> Result<()> {
    let mut table = Table::new();
    let mut header = vec!["ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account_column {
        header.remove(0);
    }
    table
        .set_header(header)
        .set_content_arrangement(ContentArrangement::Dynamic);

    for m in messages {
        let unread_marker = if !m.is_read {
            format!("{} ", "●".magenta().dimmed())
        } else {
            "  ".to_string()
        };
        let from = format!(
            "{unread_marker}{}",
            if m.from.name.is_empty() {
                &m.from.address
            } else {
                &m.from.name
            }
        );

        let mut row = vec![
            m.account.clone(),
            from,
            m.subject.clone(),
            relative_received(m.received_at),
        ];
        if hide_account_column {
            row.remove(0);
        }
        table.add_row(row);
    }

    println!("{table}");
    Ok(())
}

fn render_json(messages: &[Message]) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(messages)?);
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

- [ ] **Step 2: Build and run tests**

Run: `cargo build -p pidge`
Run: `cargo test -p pidge inbox`
Expected: 2 unit tests pass.

- [ ] **Step 3: Lint and format**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pidge/src/commands/inbox.rs
git commit -m "Implement pidge inbox list with multi-account merge and partial-failure tolerance"
```

---

## Task 22: App registration script and permissions JSON

**Files:**
- Create: `scripts/pidge-app-permissions.json`
- Create: `scripts/register-pidge-app.sh`

- [ ] **Step 1: Create permissions JSON**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/scripts`

Create `/Users/kristofer/repos/mklab-se/pidge/scripts/pidge-app-permissions.json`:

```json
[
  {
    "resourceAppId": "00000003-0000-0000-c000-000000000000",
    "resourceAccess": [
      {"id": "7427e0e9-2fba-42fe-b0c0-848c9e6a8182", "type": "Scope"},
      {"id": "e1fe6dd8-ba31-4d61-89e7-88639da4683d", "type": "Scope"},
      {"id": "024d486e-b451-40bb-833d-3e66d98c5c73", "type": "Scope"},
      {"id": "e383f46e-2787-4529-855e-0e479a3ffac0", "type": "Scope"},
      {"id": "1ec239c2-d7c9-4623-a91a-a9775856bb36", "type": "Scope"}
    ]
  }
]
```

GUID legend (from Microsoft Graph documented delegated scope IDs):
- `7427e0e9-2fba-42fe-b0c0-848c9e6a8182` — `offline_access`
- `e1fe6dd8-ba31-4d61-89e7-88639da4683d` — `User.Read`
- `024d486e-b451-40bb-833d-3e66d98c5c73` — `Mail.ReadWrite`
- `e383f46e-2787-4529-855e-0e479a3ffac0` — `Mail.Send`
- `1ec239c2-d7c9-4623-a91a-a9775856bb36` — `Calendars.ReadWrite`

- [ ] **Step 2: Create the script**

Create `/Users/kristofer/repos/mklab-se/pidge/scripts/register-pidge-app.sh`:

```bash
#!/usr/bin/env bash
# One-time developer setup: register pidge as a multi-tenant public-client app in Entra.
# Run once. Paste the printed client_id into crates/pidge-client/src/auth/config.rs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v az >/dev/null 2>&1; then
  cat <<EOF >&2
Azure CLI not found. Install it from https://aka.ms/install-azure-cli, then re-run.

Or follow the manual portal walkthrough in DEVELOPMENT.md.
EOF
  exit 1
fi

if ! az account show >/dev/null 2>&1; then
  echo "Run 'az login' first." >&2
  exit 1
fi

echo "Registering pidge app in your Entra tenant…"
CLIENT_ID=$(az ad app create \
  --display-name "pidge" \
  --sign-in-audience AzureADandPersonalMicrosoftAccount \
  --is-fallback-public-client true \
  --required-resource-accesses "@${SCRIPT_DIR}/pidge-app-permissions.json" \
  --query appId -o tsv)

cat <<EOF

✔ pidge app registered.
  client_id: ${CLIENT_ID}

Paste this into crates/pidge-client/src/auth/config.rs:

    pub const APP_CLIENT_ID: &str = "${CLIENT_ID}";

Then commit and continue.

EOF
```

Make it executable:

```bash
chmod +x /Users/kristofer/repos/mklab-se/pidge/scripts/register-pidge-app.sh
```

- [ ] **Step 3: Validate the JSON file**

Run: `python3 -c "import json; json.load(open('scripts/pidge-app-permissions.json'))" && echo "JSON OK"`
Expected: `JSON OK`. If `python3` isn't available, skip; the bash script will catch malformed JSON at runtime.

- [ ] **Step 4: Commit**

```bash
git add scripts/pidge-app-permissions.json scripts/register-pidge-app.sh
git commit -m "Add register-pidge-app.sh for one-time Entra app registration"
```

---

## Task 23: `DEVELOPMENT.md` portal walkthrough

**Files:**
- Create: `DEVELOPMENT.md`

- [ ] **Step 1: Write `DEVELOPMENT.md`**

Create `/Users/kristofer/repos/mklab-se/pidge/DEVELOPMENT.md`:

```markdown
# pidge — Development setup

Most workflows are documented in [CONTRIBUTING.md](CONTRIBUTING.md). This file covers two developer-only concerns:

1. One-time Entra app registration (the `client_id` the binary needs)
2. Local development without the registered app (env-var override)

## 1. Register the pidge app in Entra

pidge talks to Microsoft Graph as a public client (no client secret) using OAuth 2.0 device authorization grant. Microsoft requires every such app to be registered in an Entra tenant. **This is a one-time setup done by the maintainer**, never by end-users.

### Automated (recommended)

```bash
bash scripts/register-pidge-app.sh
```

Requires the Azure CLI (`az`). The script will:

1. Confirm you're logged in with `az login`.
2. Run `az ad app create` with the right settings (multi-tenant + personal MS accounts, fallback public client enabled, the five Microsoft Graph delegated permissions in `scripts/pidge-app-permissions.json`).
3. Print the resulting `client_id` GUID.

Paste the GUID into `crates/pidge-client/src/auth/config.rs`:

```rust
pub const APP_CLIENT_ID: &str = "<GUID-from-script>";
```

Commit and you're done. End-users of pidge installed via brew/cargo will never need to register anything.

### Manual portal fallback

If you can't or don't want to use the Azure CLI:

1. Open <https://portal.azure.com> → **Microsoft Entra ID** → **App registrations** → **New registration**.
2. Name: `pidge`.
3. Supported account types: **Accounts in any organizational directory (Any Microsoft Entra ID tenant — Multitenant) and personal Microsoft accounts (e.g. Skype, Xbox)**.
4. Redirect URI: leave empty (we use device code flow, no redirect needed).
5. Click **Register**.
6. From the app overview, copy the **Application (client) ID** — this is your `APP_CLIENT_ID`.
7. Go to **Authentication** → enable **Allow public client flows** (Yes) → Save.
8. Go to **API permissions** → **Add a permission** → **Microsoft Graph** → **Delegated permissions** → check:
   - `offline_access`
   - `User.Read`
   - `Mail.ReadWrite`
   - `Mail.Send`
   - `Calendars.ReadWrite`
   → **Add permissions**.
9. (Optional, for org tenants: click **Grant admin consent**. Personal MSA users will consent on first sign-in either way.)
10. Paste the client_id into `crates/pidge-client/src/auth/config.rs`, commit.

## 2. Developing without (or before) the public registration

Until `APP_CLIENT_ID` is populated, `pidge auth login` errors with a clear message. To develop against your own test app:

```bash
export PIDGE_CLIENT_ID="<your-test-app-client-id>"
cargo run -- auth login
```

The env var overrides the compile-time constant. Unset it to use the baked-in value.
```

- [ ] **Step 2: Commit**

```bash
git add DEVELOPMENT.md
git commit -m "Add DEVELOPMENT.md with Entra app registration walkthrough"
```

---

## Task 24: Update `release.yml` for three-crate publish

**Files:**
- Modify: `.github/workflows/release.yml`

The `crates-io` job currently runs `cargo publish -p pidge`. Now it publishes three crates in dependency order with 30-second pauses (matches cosq's pattern exactly).

- [ ] **Step 1: Replace the `crates-io` job**

Open `/Users/kristofer/repos/mklab-se/pidge/.github/workflows/release.yml`. Find the `crates-io` job near the bottom and replace its `steps:` block with:

```yaml
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Publish pidge-core
        run: cargo publish -p pidge-core
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}

      - name: Wait for crates.io index
        run: sleep 30

      - name: Publish pidge-client
        run: cargo publish -p pidge-client
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}

      - name: Wait for crates.io index
        run: sleep 30

      - name: Publish pidge
        run: cargo publish -p pidge
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))" && echo "YAML OK"`
Expected: `YAML OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "Update release workflow to publish pidge-core, pidge-client, and pidge"
```

---

## Task 25: Update `/release` skill

**Files:**
- Modify: `.claude/skills/release/SKILL.md`

Step 4 of the release skill currently says "single workspace member, no other Cargo files need editing." Now there are internal crate dependencies in `[workspace.dependencies]` that need their `version = "X.Y.Z"` pin bumped in lock-step.

- [ ] **Step 1: Replace Step 4 in the skill**

Open `/Users/kristofer/repos/mklab-se/pidge/.claude/skills/release/SKILL.md`. Find the `### 4. Bump version numbers` section and replace its bullet with:

```markdown
- Update `version` in the root `Cargo.toml` `[workspace.package]` section.
- Update internal-crate `version = "X.Y.Z"` pins in the root `Cargo.toml` `[workspace.dependencies]` section — both `pidge-core` and `pidge-client`. They use the bumped version (no `=` prefix).
```

- [ ] **Step 2: Commit**

```bash
git add .claude/skills/release/SKILL.md
git commit -m "Update /release skill for multi-crate workspace version bumps"
```

---

## Task 26: CHANGELOG and docs

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Append `[Unreleased]` entries to `CHANGELOG.md`**

Open `/Users/kristofer/repos/mklab-se/pidge/CHANGELOG.md`. Under `## [Unreleased]` `### Added`, append:

```markdown
- Workspace split into `pidge`, `pidge-core`, `pidge-client`
- OAuth 2.0 device code sign-in for Microsoft 365 and personal Microsoft accounts (`pidge auth login`)
- Multi-account support: `pidge auth list`, `pidge auth status`, `pidge auth logout`, `pidge auth default --send/--calendar`
- Tokens stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux libsecret)
- `pidge inbox list` — list messages across all signed-in accounts, filterable by `--account`, `--unread`, `-n <limit>`, `--output text|json`
- One-time setup script `scripts/register-pidge-app.sh` for registering the pidge app in Entra
```

- [ ] **Step 2: Update `README.md`**

Open `/Users/kristofer/repos/mklab-se/pidge/README.md`. Find the `## Status` section and replace it with a new pair of sections — Status (updated) and a new "Account setup" block. Specifically replace the `## Status` paragraph with:

```markdown
## Status

**Early days.** pidge can sign in to one or more Microsoft 365 / personal Microsoft accounts and list inbox messages from each. Read/write mail, draft, send, and calendar commands are on the roadmap.

## Account setup

```bash
# Sign in (interactive device code flow)
pidge auth login

# Check what you're signed in to
pidge auth list

# Sign out
pidge auth logout
```

The first account becomes the default for sending mail and for calendar; change with `pidge auth default --send <email>` or `--calendar <email>`. Sign in to multiple accounts and pidge merges reads across all of them.

## Reading the inbox

```bash
# All accounts, top 25 by received time
pidge inbox list

# One account, only unread
pidge inbox list --account kristofer@mklab.se --unread

# Pipe to scripts
pidge inbox list --output json | jq '.[].subject'
```
```

Leave the existing **Quick Start**, **AI Integration**, **Development**, and **License** sections alone.

- [ ] **Step 3: Update `CLAUDE.md`**

Open `/Users/kristofer/repos/mklab-se/pidge/CLAUDE.md`. Replace the `## Architecture` section's file tree with the three-crate version:

```markdown
## Architecture

Rust workspace with three crates:

\`\`\`
crates/
  pidge/                # CLI binary (package and binary name: pidge)
    src/
      main.rs           # Entry point
      cli.rs            # Clap CLI definitions
      banner.rs         # ASCII logo
      update.rs         # Crates.io update checker
      commands/         # `pidge ai`, `pidge auth`, `pidge inbox`, `pidge completion`, etc.
  pidge-core/           # Provider-agnostic types: Account, Config, Message
  pidge-client/         # Microsoft Graph client, OAuth flows, keychain token storage
    src/
      auth/             # Device code flow, refresh, JWT, keychain
      graph/            # Graph API endpoints (currently /me, inbox)
\`\`\`

- Workspace root `Cargo.toml` defines shared dependencies and version
- `pidge-core` has no HTTP or auth code — it's safe to depend on from any consumer
- `pidge-client` knows nothing about clap or terminal output
```

(Use real backticks in the actual file — they're escaped here so the file's prose displays correctly in this plan.)

In the same file, find the `## Releasing` section and update the secrets reminder if it changed (no edit needed — both required secrets are already documented).

- [ ] **Step 4: Run full CI checks**

Run:
```
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```
Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md README.md CLAUDE.md
git commit -m "Update CHANGELOG, README, and CLAUDE.md for auth + inbox feature"
```

---

## Task 27: Final verification

**Files:** none modified.

- [ ] **Step 1: Run the full CI suite**

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo check --workspace
```

Expected: every command exits 0, no warnings. Total test count: 11 (pidge-core) + ~13 (pidge-client) + 2 (pidge banner) + 2 (pidge inbox) ≈ 28 tests passing. Adjust if the count differs slightly — verify nothing has regressed.

- [ ] **Step 2: Smoke-test the CLI surface**

```bash
cargo run -q -- --help
cargo run -q -- auth --help
cargo run -q -- auth status
cargo run -q -- inbox list --help
cargo run -q -- ai status
cargo run -q -- version
```

Expected outputs:
- `--help` lists `ai`, `auth`, `completion`, `inbox`, `version` subcommands.
- `auth --help` lists `login`, `list`, `status`, `logout`, `default`.
- `auth status` prints `0 accounts signed in.` (since `APP_CLIENT_ID` is empty and no keychain entries exist).
- `inbox list --help` lists `--account`, `-n/--limit`, `--unread`, `--output`.
- `ai status` continues to work (existing functionality untouched).
- `version` prints the banner.

- [ ] **Step 3: Confirm working tree state**

Run: `git status`
Expected: clean.

Run: `git log --oneline 2cbbaa3..HEAD | wc -l`
Expected: approximately 26 new commits (each of Tasks 1–26 produces one commit).

- [ ] **Step 4: Provide the hand-off summary**

In the controller's reply to the user, summarise:
1. The 26 commits (auth + inbox feature, on top of the foundation).
2. Pre-release todos for the user:
   - Run `bash scripts/register-pidge-app.sh` once to provision the app
   - Paste the printed `client_id` into `crates/pidge-client/src/auth/config.rs`'s `APP_CLIENT_ID`
   - Commit that change
   - Test end-to-end manually: `cargo run -- auth login`, sign in with a real account, `cargo run -- inbox list`
3. Explicitly deferred: mail send/draft/delete, all calendar commands, inbox search, folder navigation, local cache, Gmail provider.

---

## Plan self-review

**Spec coverage check** — every section of `docs/superpowers/specs/2026-05-13-m365-auth-and-inbox-list-design.md` is covered:

| Spec section | Task(s) |
|---|---|
| Workspace split (3 crates) | Tasks 1, 2 |
| `pidge-core::account/message/config/error` | Tasks 3, 4, 5 |
| `pidge-client::auth::{config, tokens, jwt, store, device_code, refresh, mod}` | Tasks 6, 7, 8, 9, 10, 11, 12 |
| `pidge-client::graph::{me, mail, mod}` | Task 13 |
| One-time app registration script | Task 22 |
| Manual portal fallback | Task 23 |
| Where state lives (keychain + config) | Tasks 5 (config), 9 (keychain), 6 (constants) |
| Token refresh (60s buffer) | Tasks 7, 11, 12 |
| Hand-rolled device code flow per RFC 8628 | Task 10 (start + poll with all 5 documented error codes) |
| JWT id_token tid extraction | Task 8 |
| `pidge auth login` | Task 15 |
| `pidge auth list` | Task 16 |
| `pidge auth status` | Task 17 |
| `pidge auth logout` | Task 18 |
| `pidge auth default` | Task 19 |
| `pidge auth` top-level dispatcher | Task 20 |
| `pidge inbox list` | Task 14 (clap), 21 (implementation) |
| Multi-account merge with partial-failure tolerance | Task 21 |
| Output rendering (text + json, unread glyph, relative time) | Task 21 |
| Error mapping (NotProvisioned, SessionExpired, etc.) | Tasks 6 (types), 10/11 (raised), 15/21 (surfaced) |
| Release pipeline updated for 3-crate publish | Task 24 |
| `/release` skill updated for internal-crate version bumps | Task 25 |
| CHANGELOG, README, CLAUDE.md updates | Task 26 |
| Final verification | Task 27 |

**Placeholder scan:** Every code block is the actual content to land. There are temporary `unimplemented!()` placeholders in Task 14 only — by design, replaced in Tasks 15–21.

**Type consistency:**
- `Account` (pidge-core): `email`, `tenant_id`, `home_account_id`, `added_at` — same names referenced in `auth_login.rs`, `auth_list.rs`, etc.
- `Config::{add_account, remove_account, set_default_send, set_default_calendar, find}` — names match across producer (Task 5) and all consumers (Tasks 15–19, 21).
- `TokenSet::{access_token, refresh_token, expires_at, needs_refresh}` — consistent across `tokens.rs` (Task 7), `store.rs` (Task 9), `device_code.rs` (Task 10), `refresh.rs` (Task 11), `auth/mod.rs` (Task 12).
- `Message::{account, id, from, subject, received_at, is_read, preview}` — consistent across `message.rs` (Task 4), `graph/mail.rs` (Task 13), `inbox.rs` (Task 21).
- `ClientError::{NotProvisioned, Keychain, DeviceCodeTimeout, DeviceCodeAccessDenied, DeviceCodeOther, SessionExpired, Http, Json, Graph, Core, MissingAccessToken}` — defined in Task 6, raised throughout Tasks 9, 10, 11, 12, 13.
- `AuthClient::{from_env, for_test, start_device_code, poll_for_tokens, get_valid_token}` — defined in Task 12, called in Tasks 15, 21.
- `GraphClient::{new, for_test, auth, me, list_inbox}` — defined in Task 13, called in Tasks 15, 21.

**Notable risks called out in the spec that are inherited by the plan:**
- Keychain headless Linux fallback: not addressed in this plan; the error message in Task 6 is the surface.
- JWT base64 padding: handled explicitly in Task 8 via `URL_SAFE_NO_PAD`.
- Refresh response without `refresh_token`: handled in Task 11 via the `unwrap_or_else` fallback.
- `expires_in` missing on refresh: handled in Task 11 via `unwrap_or(3600)`.
