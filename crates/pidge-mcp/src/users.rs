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

/// The immutable identity of a Microsoft account: its tenant and object id,
/// from the ID token of the sign-in. Pinned on first use, so a later sign-in
/// whose address matches but whose account differs is refused.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub tid: String,
    pub oid: String,
}

/// A signed-in user's profile: which mailboxes they own, their default
/// sending address, timezone, and trust settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRecord {
    pub signin: String,
    /// The Microsoft account this user signs in with; `None` on records
    /// from before identities were pinned (pinned at the next sign-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
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
            identity: None,
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
    /// The Microsoft account whose tokens these are; `None` on records from
    /// before identities were pinned (pinned at the next bind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
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
    #[error("mailbox is pinned to a different Microsoft account")]
    IdentityMismatch,
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
    /// Returns the `token_generation` actually stored: never lower than the
    /// stored one, so saving a record loaded before a sign-out everywhere
    /// can't resurrect the revoked tokens.
    pub async fn save(&self, rec: &UserRecord) -> Result<u32> {
        let stored = self
            .load(&rec.signin)
            .await?
            .map_or(0, |r| r.token_generation);
        let rec = UserRecord {
            token_generation: rec.token_generation.max(stored),
            ..rec.clone()
        };
        let raw = serde_json::to_string(&rec)?;
        self.secrets
            .set(&user_secret_name(&rec.signin), &raw)
            .await?;
        Ok(rec.token_generation)
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
                identity: None,
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
    /// And when `identity` is given and the stored record has one pinned,
    /// they must match
    /// (`Err(OwnershipError::IdentityMismatch)` otherwise). A record without
    /// a pinned identity passes; the caller pins it when saving.
    pub async fn check_binding(
        &self,
        mailbox: &str,
        owner: &str,
        identity: Option<&Identity>,
    ) -> Result<(), OwnershipError> {
        let Some(rec) = self.load_mailbox(mailbox).await? else {
            return Ok(());
        };
        if !rec.owner.eq_ignore_ascii_case(owner) {
            return Err(OwnershipError::OwnedByOther);
        }
        match (&rec.identity, identity) {
            (Some(pinned), Some(new)) if pinned != new => Err(OwnershipError::IdentityMismatch),
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

/// A 64-bit form of [`user_hash`] (16 hex characters), for download links,
/// where two allowlisted addresses must never share a hash.
pub fn user_hash_long(signin: &str) -> String {
    sha256_hex(signin)[..16].to_string()
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

    #[test]
    fn records_without_an_identity_still_load() {
        let user: UserRecord = serde_json::from_str(
            r#"{"signin":"jane@example.com","mailboxes":["jane@example.com"],"default_sender":"jane@example.com","timezone":"Europe/Stockholm"}"#,
        )
        .unwrap();
        assert_eq!(user.identity, None);
        let mailbox: MailboxRecord = serde_json::from_str(
            r#"{"owner":"jane@example.com","tokens":{"access_token":"a","refresh_token":"r","expires_at":"2026-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(mailbox.identity, None);
    }

    #[tokio::test]
    async fn saving_a_stale_record_never_lowers_the_token_generation() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: SharedSecrets = Arc::new(FileSecrets::new(dir.path()).unwrap());
        let store = UserStore::new(secrets);
        let stale = UserRecord::new("jane@example.com");
        store
            .save(&UserRecord {
                token_generation: 2,
                ..stale.clone()
            })
            .await
            .unwrap();
        store
            .save(&UserRecord {
                timezone: "Europe/London".into(),
                ..stale
            })
            .await
            .unwrap();
        let rec = store.load("jane@example.com").await.unwrap().unwrap();
        assert_eq!(rec.token_generation, 2, "kept");
        assert_eq!(rec.timezone, "Europe/London", "the rest is saved");
    }

    #[tokio::test]
    async fn a_pinned_mailbox_identity_must_match() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: SharedSecrets = Arc::new(FileSecrets::new(dir.path()).unwrap());
        let store = UserStore::new(secrets);
        let pinned = Identity {
            tid: "t".into(),
            oid: "o1".into(),
        };
        let other = Identity {
            tid: "t".into(),
            oid: "o2".into(),
        };
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now(),
        };
        let mut rec = MailboxRecord {
            owner: "jane@example.com".into(),
            tokens,
            identity: None,
        };
        store.save_mailbox(&rec, "jane@example.com").await.unwrap();
        // Unpinned: any identity passes.
        assert!(
            store
                .check_binding("jane@example.com", "jane@example.com", Some(&other))
                .await
                .is_ok()
        );
        rec.identity = Some(pinned.clone());
        store.save_mailbox(&rec, "jane@example.com").await.unwrap();
        assert!(
            store
                .check_binding("jane@example.com", "jane@example.com", Some(&pinned))
                .await
                .is_ok()
        );
        assert!(matches!(
            store
                .check_binding("jane@example.com", "jane@example.com", Some(&other))
                .await,
            Err(OwnershipError::IdentityMismatch)
        ));
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
                .check_binding("old@example.com", "old@example.com", None)
                .await
                .is_ok()
        );
        assert!(matches!(
            store
                .check_binding("old@example.com", "mallory@example.com", None)
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
