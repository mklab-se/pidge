//! Bridges pidge-client's token persistence to the secret store, with an
//! in-memory cache so a Graph call doesn't cost a Key Vault round trip.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Context;
use async_trait::async_trait;
use pidge_client::ClientError;
use pidge_client::auth::{TokenBackend, TokenSet};

use crate::secrets::{SharedSecrets, mailbox_secret_name};

pub struct SecretTokenBackend {
    secrets: SharedSecrets,
    cache: Mutex<HashMap<String, TokenSet>>,
}

impl SecretTokenBackend {
    pub fn new(secrets: SharedSecrets) -> Self {
        Self {
            secrets,
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
        let Some(raw) = self
            .secrets
            .get(&mailbox_secret_name(email))
            .await
            .map_err(store_error)?
        else {
            return Ok(None);
        };
        let tokens: TokenSet = serde_json::from_str(&raw)?;
        self.cache
            .lock()
            .expect("cache lock")
            .insert(email.to_string(), tokens.clone());
        Ok(Some(tokens))
    }

    async fn save(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        let raw = serde_json::to_string(tokens)?;
        self.secrets
            .set(&mailbox_secret_name(email), &raw)
            .await
            .context("saving mailbox tokens")
            .map_err(store_error)?;
        self.cache
            .lock()
            .expect("cache lock")
            .insert(email.to_string(), tokens.clone());
        Ok(())
    }
}
