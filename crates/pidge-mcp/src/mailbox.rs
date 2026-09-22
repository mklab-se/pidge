//! Bridges pidge-client's token persistence to the per-mailbox record store,
//! with an in-memory cache so a Graph call doesn't cost a Key Vault round trip.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use pidge_client::ClientError;
use pidge_client::auth::{TokenBackend, TokenSet};

use crate::secrets::SharedSecrets;
use crate::users::{UserStore, log_store_error};

pub struct SecretTokenBackend {
    users: UserStore,
    cache: Mutex<HashMap<String, TokenSet>>,
}

impl SecretTokenBackend {
    pub fn new(secrets: SharedSecrets) -> Self {
        Self {
            users: UserStore::new(secrets),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Drop any cached tokens for `email`, so the next load reads the store.
    /// Called when a mailbox is disconnected or freshly (re)connected.
    pub fn forget(&self, email: &str) {
        self.cache.lock().expect("cache lock").remove(email);
    }
}

/// Logs the failure (redacted) and returns a `ClientError` whose text names
/// no secret, so it can't leak an address through Graph error messages.
fn store_error(email: &str, e: anyhow::Error) -> ClientError {
    log_store_error("token store", email, &e);
    ClientError::Io(std::io::Error::other("secret store failure"))
}

#[async_trait]
impl TokenBackend for SecretTokenBackend {
    async fn load(&self, email: &str) -> Result<Option<TokenSet>, ClientError> {
        if let Some(hit) = self.cache.lock().expect("cache lock").get(email) {
            return Ok(Some(hit.clone()));
        }
        let Some(rec) = self
            .users
            .load_mailbox(email)
            .await
            .map_err(|e| store_error(email, e))?
        else {
            return Ok(None);
        };
        self.cache
            .lock()
            .expect("cache lock")
            .insert(email.to_string(), rec.tokens.clone());
        Ok(Some(rec.tokens))
    }

    /// Rotates the tokens on an existing mailbox record, preserving its
    /// `owner`. This only handles refresh-rotation: the first store for a
    /// mailbox happens during sign-in, which creates the record directly via
    /// `UserStore::save_mailbox` with the owner attached. If no record
    /// exists yet, that sign-in hasn't happened (or the record was deleted),
    /// so this errors rather than silently creating an unowned mailbox.
    async fn save(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        let Some(mut rec) = self
            .users
            .load_mailbox(email)
            .await
            .map_err(|e| store_error(email, e))?
        else {
            return Err(ClientError::SessionExpired {
                email: email.to_string(),
            });
        };
        rec.tokens = tokens.clone();
        self.users
            .save_mailbox(&rec, email)
            .await
            .map_err(|e| store_error(email, e))?;
        self.cache
            .lock()
            .expect("cache lock")
            .insert(email.to_string(), rec.tokens);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_support::{FailingSecrets, LogCapture, assert_no_address};

    #[tokio::test]
    async fn store_failures_never_carry_the_address() {
        let (logs, _guard) = LogCapture::start();
        let backend = SecretTokenBackend::new(Arc::new(FailingSecrets));
        let err = backend.load("jane@example.com").await.unwrap_err();
        assert_no_address("load error", &err.to_string());
        let err = backend
            .save(
                "jane@example.com",
                &TokenSet {
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: chrono::Utc::now(),
                },
            )
            .await
            .unwrap_err();
        assert_no_address("save error", &err.to_string());
        let logged = logs.text();
        assert!(
            logged.contains("token store: secret store failure"),
            "{logged}"
        );
        assert_no_address("logs", &logged);
    }
}
