//! Per-user records and mailbox ownership.
//!
//! A signed-in user is identified by their sign-in address (lower-cased) and
//! owns one or more mailboxes. Each mailbox's secret holds a [`MailboxRecord`]
//! pairing the owning user's sign-in with its [`TokenSet`], so a stolen or
//! misdirected mailbox name can never be used to read another user's tokens.

use anyhow::Result;
use pidge_client::auth::TokenSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::secrets::{SharedSecrets, mailbox_secret_name};

/// A signed-in user's profile: which mailboxes they own, their default
/// sending address, timezone, and trust settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRecord {
    pub signin: String,
    pub mailboxes: Vec<String>,
    pub default_sender: String,
    pub timezone: String,
    #[serde(default)]
    pub trusted_senders: Vec<String>,
    #[serde(default)]
    pub token_generation: u32,
}

impl UserRecord {
    /// A fresh record for `signin`: one mailbox (itself), Stockholm timezone.
    pub fn new(signin: &str) -> Self {
        let signin = signin.to_ascii_lowercase();
        Self {
            mailboxes: vec![signin.clone()],
            default_sender: signin.clone(),
            timezone: "Europe/Stockholm".to_string(),
            signin,
            trusted_senders: Vec::new(),
            token_generation: 0,
        }
    }

    /// Case-insensitive check of whether this user owns `mailbox`.
    pub fn owns(&self, mailbox: &str) -> bool {
        self.mailboxes
            .iter()
            .any(|m| m.eq_ignore_ascii_case(mailbox))
    }

    /// The user's timezone, falling back to Stockholm if `timezone` doesn't
    /// parse as an IANA zone.
    pub fn tz(&self) -> chrono_tz::Tz {
        self.timezone
            .parse()
            .unwrap_or(chrono_tz::Europe::Stockholm)
    }
}

/// A mailbox's stored tokens plus the sign-in address that owns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxRecord {
    pub owner: String,
    pub tokens: TokenSet,
}

/// Backward-compatible shape for reading a mailbox secret: either the
/// current `MailboxRecord`, or a bare `TokenSet` left over from the spike
/// (before ownership was tracked), which is treated as owned by the mailbox
/// address itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredMailbox {
    Record(MailboxRecord),
    Legacy(TokenSet),
}

/// A mailbox is claimed by someone other than the caller.
#[derive(Debug, thiserror::Error)]
pub enum OwnershipError {
    #[error("mailbox is owned by another user")]
    OwnedByOther,
    #[error("secret store: {0}")]
    Store(#[from] anyhow::Error),
}

/// Reads and writes [`UserRecord`]s and [`MailboxRecord`]s through the
/// shared secret store.
pub struct UserStore {
    secrets: SharedSecrets,
}

impl UserStore {
    pub fn new(secrets: SharedSecrets) -> Self {
        Self { secrets }
    }

    /// Load the user record for `signin`, `None` if never signed in.
    pub async fn load(&self, signin: &str) -> Result<Option<UserRecord>> {
        let Some(raw) = self.secrets.get(&user_secret_name(signin)).await? else {
            return Ok(None);
        };
        if raw.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// Persist a user record, keyed by its (already lower-cased) `signin`.
    pub async fn save(&self, rec: &UserRecord) -> Result<()> {
        let raw = serde_json::to_string(rec)?;
        self.secrets.set(&user_secret_name(&rec.signin), &raw).await
    }

    /// Load a mailbox record, adopting the legacy bare-`TokenSet` shape if
    /// that's what's stored. `None` if unset or deleted (empty secret).
    pub async fn load_mailbox(&self, mailbox: &str) -> Result<Option<MailboxRecord>> {
        let Some(raw) = self.secrets.get(&mailbox_secret_name(mailbox)).await? else {
            return Ok(None);
        };
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let stored: StoredMailbox = serde_json::from_str(&raw)?;
        Ok(Some(match stored {
            StoredMailbox::Record(rec) => rec,
            StoredMailbox::Legacy(tokens) => MailboxRecord {
                owner: mailbox.to_ascii_lowercase(),
                tokens,
            },
        }))
    }

    /// Persist a mailbox record under `mailbox`'s secret name.
    pub async fn save_mailbox(&self, rec: &MailboxRecord, mailbox: &str) -> Result<()> {
        let raw = serde_json::to_string(rec)?;
        self.secrets.set(&mailbox_secret_name(mailbox), &raw).await
    }

    /// `Ok(())` if `mailbox` is unowned or already owned by `owner`;
    /// `Err(OwnershipError::OwnedByOther)` if it's claimed by someone else.
    pub async fn check_ownership(&self, mailbox: &str, owner: &str) -> Result<(), OwnershipError> {
        match self.load_mailbox(mailbox).await? {
            Some(rec) if !rec.owner.eq_ignore_ascii_case(owner) => {
                Err(OwnershipError::OwnedByOther)
            }
            _ => Ok(()),
        }
    }

    /// Delete a mailbox's stored tokens. Modelled as writing an empty
    /// string rather than a real delete: Key Vault soft-delete makes true
    /// deletion slow, and the name space is per-mailbox anyway.
    pub async fn delete_mailbox(&self, mailbox: &str) -> Result<()> {
        self.secrets.set(&mailbox_secret_name(mailbox), "").await
    }
}

/// Logs a secret-store failure as a fixed message plus the hashed account it
/// concerned. The error's own text is deliberately dropped: store errors carry
/// secret names, and mailbox secret names are derived from the address.
pub fn log_store_error(context: &str, account: &str, _err: &anyhow::Error) {
    tracing::error!(account = %user_hash(account), "{context}: secret store failure");
}

/// A short, stable, non-reversible tag for a sign-in address, for logs:
/// the first 8 hex characters of the SHA-256 of the lower-cased address.
/// Addresses themselves never go into a log line.
pub fn user_hash(signin: &str) -> String {
    sha256_hex(signin)[..8].to_string()
}

fn sha256_hex(signin: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(signin.to_ascii_lowercase().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Secret name for a user record: `"user-"` + the first 16 hex characters of
/// the SHA-256 hash of the lower-cased sign-in address. Hashed (rather than
/// sanitized like [`mailbox_secret_name`]) so the secret name itself doesn't
/// leak the address into logs or Key Vault's secret listing.
pub fn user_secret_name(signin: &str) -> String {
    format!("user-{}", &sha256_hex(signin)[..16])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::secrets::FileSecrets;

    #[test]
    fn user_hash_is_short_stable_and_case_insensitive() {
        let h = user_hash("Jane@Example.com");
        assert_eq!(h.len(), 8);
        assert_eq!(h, user_hash("jane@example.com"));
        assert!(!h.contains('@'));
        assert_ne!(h, user_hash("mallory@example.com"));
        assert!(
            user_secret_name("jane@example.com").ends_with(&sha256_hex("jane@example.com")[..16])
        );
    }

    #[tokio::test]
    async fn new_user_record_defaults() {
        let r = UserRecord::new("Jane@Example.com");
        assert_eq!(r.signin, "jane@example.com");
        assert_eq!(r.mailboxes, vec!["jane@example.com"]);
        assert_eq!(r.default_sender, "jane@example.com");
        assert_eq!(r.timezone, "Europe/Stockholm");
        assert!(r.owns("JANE@example.com"));
    }

    #[tokio::test]
    async fn ownership_is_enforced_and_legacy_secrets_are_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: SharedSecrets = Arc::new(FileSecrets::new(dir.path()).unwrap());
        let store = UserStore::new(secrets.clone());
        // legacy: bare TokenSet
        let legacy = serde_json::to_string(&TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now(),
        })
        .unwrap();
        secrets
            .set(&mailbox_secret_name("old@example.com"), &legacy)
            .await
            .unwrap();
        let rec = store
            .load_mailbox("old@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.owner, "old@example.com");
        assert!(
            store
                .check_ownership("old@example.com", "old@example.com")
                .await
                .is_ok()
        );
        assert!(matches!(
            store
                .check_ownership("old@example.com", "mallory@example.com")
                .await,
            Err(OwnershipError::OwnedByOther)
        ));
        store.delete_mailbox("old@example.com").await.unwrap();
        assert!(
            store
                .load_mailbox("old@example.com")
                .await
                .unwrap()
                .is_none()
        );
    }
}
