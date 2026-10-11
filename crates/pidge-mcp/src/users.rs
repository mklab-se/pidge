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

use crate::secrets::{SharedSecrets, legacy_mailbox_secret_name, mailbox_secret_name};

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
    /// Inbox-review categories and the last review time.
    #[serde(
        default,
        skip_serializing_if = "crate::review::ReviewSettings::is_default"
    )]
    pub review: crate::review::ReviewSettings,
    /// The user's AI sorting status (the key itself is a separate secret).
    #[serde(
        default,
        skip_serializing_if = "crate::review::ai::AiSettings::is_default"
    )]
    pub ai: crate::review::ai::AiSettings,
    /// The current refresh token of each OAuth client the user signed in
    /// with, at most [`MAX_REFRESH_CHAINS`]; see [`Self::refresh_check`].
    /// Records from before rotation have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refresh_chains: Vec<RefreshChain>,
}

/// Clients whose refresh chain a user record keeps; the least recently
/// rotated is dropped beyond it, and its next refresh starts a new chain.
pub const MAX_REFRESH_CHAINS: usize = 20;

/// How long the refresh token just retired by a rotation is still answered,
/// so a client that sent the same refresh twice at once isn't taken for a
/// thief. See [`RefreshCheck::Grace`].
pub const REFRESH_GRACE: chrono::Duration = chrono::Duration::seconds(30);

/// One OAuth client's refresh-token chain: the id (`jti`) of the refresh
/// token it holds now, and of the one it held before the last rotation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshChain {
    /// [`client_key`] of the client id (client ids are long signed tokens).
    pub client: String,
    pub current: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    pub rotated_at: chrono::DateTime<chrono::Utc>,
}

/// What a refresh token's `jti` is, against the client's stored chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshCheck {
    /// The client's current token: rotate.
    Current,
    /// No chain for this client: a token from before rotation (or from a
    /// client whose chain was dropped). Accepted once; it starts the chain.
    Unchained,
    /// The token the last rotation retired, presented within
    /// [`REFRESH_GRACE`]: the same refresh sent twice at once. Answered
    /// with a token carrying the chain's current id (held here), so both
    /// responses continue the one chain and neither copy becomes reuse.
    Grace(String),
    /// Anything else: a retired token presented again, so someone else
    /// holds a copy of it.
    Reused,
}

/// A short, stable key for an OAuth client id: the first 32 hex characters
/// (128 bits) of its SHA-256. Case-sensitive, unlike [`user_hash`].
pub fn client_key(client_id: &str) -> String {
    Sha256::digest(client_id.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
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
            review: Default::default(),
            ai: Default::default(),
            refresh_chains: Vec::new(),
        }
    }

    /// Classifies a refresh token `jti` issued to `client_id`, as of `now`.
    pub fn refresh_check(
        &self,
        client_id: &str,
        jti: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> RefreshCheck {
        let key = client_key(client_id);
        let Some(chain) = self.refresh_chains.iter().find(|c| c.client == key) else {
            return RefreshCheck::Unchained;
        };
        if chain.current == jti {
            RefreshCheck::Current
        } else if chain.previous.as_deref() == Some(jti) && now - chain.rotated_at < REFRESH_GRACE {
            RefreshCheck::Grace(chain.current.clone())
        } else {
            RefreshCheck::Reused
        }
    }

    /// Makes `jti` `client_id`'s current refresh token as of `now`;
    /// `previous` is the one it replaces (`None` for a fresh sign-in).
    /// Keeps at most [`MAX_REFRESH_CHAINS`], dropping the least recently
    /// rotated.
    pub fn set_refresh(
        &mut self,
        client_id: &str,
        jti: &str,
        previous: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let key = client_key(client_id);
        self.refresh_chains.retain(|c| c.client != key);
        self.refresh_chains.push(RefreshChain {
            client: key,
            current: jti.to_string(),
            previous: previous.map(str::to_string),
            rotated_at: now,
        });
        cap_chains(&mut self.refresh_chains);
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
    /// can't resurrect the revoked tokens. Refresh chains are merged the
    /// same way, the more recently rotated entry per client winning, so a
    /// tool saving a record it loaded before a refresh can't roll a
    /// client's chain back (which would make its next refresh look like
    /// reuse and sign the user out).
    pub async fn save(&self, rec: &UserRecord) -> Result<u32> {
        let stored = self.load(&rec.signin).await?;
        let stored_generation = stored.as_ref().map_or(0, |r| r.token_generation);
        let rec = UserRecord {
            token_generation: rec.token_generation.max(stored_generation),
            refresh_chains: merge_chains(
                &rec.refresh_chains,
                stored.map(|r| r.refresh_chains).unwrap_or_default(),
            ),
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
    /// A mailbox stored under its pre-hash-suffix name (see
    /// [`legacy_mailbox_secret_name`]) is found there until its next save
    /// moves it; a legacy name that another address also folds to is only
    /// consulted when the current name is unset.
    pub async fn load_mailbox(&self, mailbox: &str) -> Result<Option<MailboxRecord>> {
        let raw = match self.secrets.get(&mailbox_secret_name(mailbox)).await? {
            Some(raw) => raw,
            None => match self
                .secrets
                .get(&legacy_mailbox_secret_name(mailbox))
                .await?
            {
                Some(raw) => raw,
                None => return Ok(None),
            },
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
        self.secrets.set(&mailbox_secret_name(mailbox), "").await?;
        // Only if something is there: a blank legacy secret would otherwise
        // shadow nothing but still be created for every disconnect.
        let legacy = legacy_mailbox_secret_name(mailbox);
        if self.secrets.get(&legacy).await?.is_some() {
            self.secrets.set(&legacy, "").await?;
        }
        Ok(())
    }
}

/// `ours` plus every chain of `stored` that is newer than ours for its
/// client (or absent from ours), capped like [`UserRecord::set_refresh`].
fn merge_chains(ours: &[RefreshChain], stored: Vec<RefreshChain>) -> Vec<RefreshChain> {
    let mut merged = ours.to_vec();
    for theirs in stored {
        match merged.iter_mut().find(|c| c.client == theirs.client) {
            Some(mine) if theirs.rotated_at > mine.rotated_at => *mine = theirs,
            Some(_) => {}
            None => merged.push(theirs),
        }
    }
    cap_chains(&mut merged);
    merged
}

/// Drops the least recently rotated chains beyond [`MAX_REFRESH_CHAINS`].
fn cap_chains(chains: &mut Vec<RefreshChain>) {
    if chains.len() > MAX_REFRESH_CHAINS {
        chains.sort_by_key(|c| std::cmp::Reverse(c.rotated_at));
        chains.truncate(MAX_REFRESH_CHAINS);
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

    #[test]
    fn refresh_tokens_are_current_unchained_in_grace_or_reused() {
        let t0 = chrono::Utc::now();
        let mut rec = UserRecord::new("jane@example.com");
        assert_eq!(rec.refresh_check("c1", "j1", t0), RefreshCheck::Unchained);
        rec.set_refresh("c1", "j1", None, t0);
        assert_eq!(rec.refresh_check("c1", "j1", t0), RefreshCheck::Current);
        assert_eq!(rec.refresh_check("c1", "jx", t0), RefreshCheck::Reused);
        assert_eq!(rec.refresh_check("c2", "j1", t0), RefreshCheck::Unchained);

        rec.set_refresh("c1", "j2", Some("j1"), t0);
        assert_eq!(rec.refresh_check("c1", "j2", t0), RefreshCheck::Current);
        let soon = t0 + REFRESH_GRACE - chrono::Duration::seconds(1);
        assert_eq!(
            rec.refresh_check("c1", "j1", soon),
            RefreshCheck::Grace("j2".into())
        );
        let late = t0 + REFRESH_GRACE;
        assert_eq!(rec.refresh_check("c1", "j1", late), RefreshCheck::Reused);
        // One rotation further, the first token is reuse even at once.
        rec.set_refresh("c1", "j3", Some("j2"), t0);
        assert_eq!(rec.refresh_check("c1", "j1", t0), RefreshCheck::Reused);
        assert_eq!(rec.refresh_chains.len(), 1);
    }

    #[test]
    fn refresh_chains_are_capped_by_dropping_the_least_recently_rotated() {
        let t0 = chrono::Utc::now();
        let mut rec = UserRecord::new("jane@example.com");
        for i in 0..=MAX_REFRESH_CHAINS {
            rec.set_refresh(
                &format!("c{i}"),
                "j",
                None,
                t0 + chrono::Duration::seconds(i as i64),
            );
        }
        assert_eq!(rec.refresh_chains.len(), MAX_REFRESH_CHAINS);
        assert_eq!(rec.refresh_check("c0", "j", t0), RefreshCheck::Unchained);
        assert_eq!(rec.refresh_check("c1", "j", t0), RefreshCheck::Current);
        // Client ids are stored only as a hash.
        assert!(!serde_json::to_string(&rec).unwrap().contains("\"c1\""));
    }

    #[tokio::test]
    async fn saving_a_stale_record_never_rolls_a_refresh_chain_back() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: SharedSecrets = Arc::new(FileSecrets::new(dir.path()).unwrap());
        let store = UserStore::new(secrets);
        let t0 = chrono::Utc::now();
        let mut rec = UserRecord::new("jane@example.com");
        rec.set_refresh("c1", "j1", None, t0);
        store.save(&rec).await.unwrap();
        // A tool loads the record, a refresh rotates, the tool saves.
        let stale = store.load("jane@example.com").await.unwrap().unwrap();
        let mut fresh = stale.clone();
        let t1 = t0 + chrono::Duration::seconds(5);
        fresh.set_refresh("c1", "j2", Some("j1"), t1);
        fresh.set_refresh("c2", "k1", None, t1);
        store.save(&fresh).await.unwrap();
        store
            .save(&UserRecord {
                timezone: "Europe/London".into(),
                ..stale
            })
            .await
            .unwrap();
        let rec = store.load("jane@example.com").await.unwrap().unwrap();
        assert_eq!(rec.timezone, "Europe/London");
        assert_eq!(rec.refresh_check("c1", "j2", t1), RefreshCheck::Current);
        assert_eq!(rec.refresh_check("c2", "k1", t1), RefreshCheck::Current);
    }

    #[test]
    fn records_from_before_refresh_rotation_still_load() {
        let user: UserRecord = serde_json::from_str(
            r#"{"signin":"jane@example.com","mailboxes":["jane@example.com"],"default_sender":"jane@example.com","timezone":"Europe/Stockholm","token_generation":3}"#,
        )
        .unwrap();
        assert!(user.refresh_chains.is_empty());
        assert_eq!(user.token_generation, 3);
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
            .set(&legacy_mailbox_secret_name("old@example.com"), &legacy)
            .await
            .unwrap();
        let rec = store
            .load_mailbox("old@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.owner, "old@example.com");
        // A save moves it to the current name; the legacy one is not read
        // while the current one is set, and a delete blanks both.
        store.save_mailbox(&rec, "old@example.com").await.unwrap();
        assert!(
            secrets
                .get(&mailbox_secret_name("old@example.com"))
                .await
                .unwrap()
                .is_some()
        );
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
        assert_eq!(
            secrets
                .get(&legacy_mailbox_secret_name("old@example.com"))
                .await
                .unwrap()
                .as_deref(),
            Some("")
        );
    }

    #[tokio::test]
    async fn a_mailbox_whose_address_folds_to_anothers_legacy_name_is_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: SharedSecrets = Arc::new(FileSecrets::new(dir.path()).unwrap());
        let store = UserStore::new(secrets.clone());
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now(),
        };
        store
            .save_mailbox(
                &MailboxRecord {
                    owner: "jane.doe@example.com".into(),
                    tokens: tokens.clone(),
                    identity: None,
                },
                "jane.doe@example.com",
            )
            .await
            .unwrap();
        // jane-doe@ is a different Microsoft account; it must not see or
        // shadow jane.doe@'s record.
        assert!(
            store
                .load_mailbox("jane-doe@example.com")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_binding("jane-doe@example.com", "mallory@example.com", None)
                .await
                .is_ok()
        );
        store
            .save_mailbox(
                &MailboxRecord {
                    owner: "mallory@example.com".into(),
                    tokens,
                    identity: None,
                },
                "jane-doe@example.com",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .load_mailbox("jane.doe@example.com")
                .await
                .unwrap()
                .unwrap()
                .owner,
            "jane.doe@example.com"
        );
    }
}
