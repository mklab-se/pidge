//! Bridges pidge-client's token persistence to the per-mailbox record store,
//! with an in-memory cache so a Graph call doesn't cost a Key Vault round trip.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use pidge_client::ClientError;
use pidge_client::auth::{TokenBackend, TokenSet};

use crate::secrets::SharedSecrets;
use crate::users::UserStore;

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
}

fn store_error(e: anyhow::Error) -> ClientError {
    ClientError::Io(std::io::Error::other(format!("secret store: {e:#}")))
}

#[async_trait]
impl TokenBackend for SecretTokenBackend {
    async fn load(&self, email: &str) -> Result<Option<TokenSet>, ClientError> {
        if let Some(hit) = self.cache.lock().expect("cache lock").get(email) {
            return Ok(Some(hit.clone()));
        }
        let Some(rec) = self.users.load_mailbox(email).await.map_err(store_error)? else {
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
        let Some(mut rec) = self.users.load_mailbox(email).await.map_err(store_error)? else {
            return Err(ClientError::SessionExpired {
                email: email.to_string(),
            });
        };
        rec.tokens = tokens.clone();
        self.users
            .save_mailbox(&rec, email)
            .await
            .map_err(store_error)?;
        self.cache
            .lock()
            .expect("cache lock")
            .insert(email.to_string(), rec.tokens);
        Ok(())
    }
}
