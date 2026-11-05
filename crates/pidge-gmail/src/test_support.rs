//! Test helpers: an in-memory token backend and a provider pointed at a
//! wiremock server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use pidge_client::ClientError;
use pidge_client::auth::{TokenBackend, TokenSet};
use wiremock::MockServer;

use crate::GmailProvider;
use crate::auth::{GoogleAuth, GoogleOAuthConfig};

pub(crate) const ME: &str = "jane@example.com";

#[derive(Default)]
pub(crate) struct MemoryBackend {
    tokens: Mutex<HashMap<String, TokenSet>>,
}

impl MemoryBackend {
    pub(crate) fn put(&self, email: &str, tokens: TokenSet) {
        self.tokens
            .lock()
            .unwrap()
            .insert(email.to_string(), tokens);
    }

    pub(crate) fn get(&self, email: &str) -> Option<TokenSet> {
        self.tokens.lock().unwrap().get(email).cloned()
    }
}

#[async_trait]
impl TokenBackend for MemoryBackend {
    async fn load(&self, email: &str) -> Result<Option<TokenSet>, ClientError> {
        Ok(self.get(email))
    }

    async fn save(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        self.put(email, tokens.clone());
        Ok(())
    }
}

/// A provider for [`ME`] with a fresh access token `AT`, whose Gmail base is
/// `<server>/gmail` and Calendar base `<server>/calendar`.
pub(crate) fn provider(server: &MockServer) -> GmailProvider {
    let backend = Arc::new(MemoryBackend::default());
    backend.put(
        ME,
        TokenSet {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
            scope: None,
        },
    );
    let cfg = GoogleOAuthConfig {
        client_id: "cid".into(),
        client_secret: "secret".into(),
        auth_base: format!("{}/auth", server.uri()),
        token_url: format!("{}/token", server.uri()),
    };
    let auth = GoogleAuth::new(reqwest::Client::new(), cfg, backend, ME);
    GmailProvider::for_test(
        auth,
        format!("{}/gmail", server.uri()),
        format!("{}/calendar", server.uri()),
    )
}
