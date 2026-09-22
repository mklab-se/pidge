//! Pluggable token persistence for [`super::AuthClient`].
//!
//! The CLI resolves each account's backend (OS keychain or plaintext file)
//! from `config.yaml`; that behaviour lives in [`LocalBackend`] and stays the
//! default. Hosted consumers (e.g. the remote MCP server) implement
//! [`TokenBackend`] themselves so tokens can live in a per-user secret store
//! instead — `AuthClient` and `GraphClient` don't care which.

use async_trait::async_trait;
use pidge_core::TokenStorage;

use crate::auth::jwt;
use crate::auth::token_store::TokenStore;
use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

/// Where an account's [`TokenSet`] is loaded from and saved to.
///
/// Implementations must be safe to share across tasks; `AuthClient` holds one
/// behind an `Arc`.
#[async_trait]
pub trait TokenBackend: Send + Sync {
    /// Load the stored tokens for `email`. `Ok(None)` means "no session".
    async fn load(&self, email: &str) -> Result<Option<TokenSet>, ClientError>;

    /// Persist `tokens` for `email`, replacing whatever was stored before.
    async fn save(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError>;

    /// Hook invoked with every access token handed out by
    /// [`super::AuthClient::get_valid_token`]. The default is a no-op; the
    /// CLI backend uses it to backfill account metadata.
    fn on_access_token(&self, _email: &str, _access_token: &str) {}
}

/// The CLI's backend: consults `config.yaml` for the account's
/// [`TokenStorage`] and dispatches to the keychain or file store.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalBackend;

impl LocalBackend {
    /// Resolve the token storage backend for an email by consulting `config.yaml`.
    /// Falls back to [`TokenStorage::Keychain`] (the default) if the config can't
    /// be read or the account isn't listed yet.
    fn storage_for(email: &str) -> TokenStorage {
        pidge_core::Config::load()
            .ok()
            .and_then(|c| c.find(email).map(|a| a.storage))
            .unwrap_or_default()
    }
}

#[async_trait]
impl TokenBackend for LocalBackend {
    async fn load(&self, email: &str) -> Result<Option<TokenSet>, ClientError> {
        TokenStore::load(email, Self::storage_for(email))
    }

    async fn save(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        TokenStore::save(email, tokens, Self::storage_for(email))
    }

    /// Opportunistic backfill: accounts added before pidge requested the
    /// `openid` scope have an empty tenant_id in config. Microsoft Graph
    /// access tokens are JWTs that carry the `tid` claim, so we can fix
    /// this once per such account on the next Graph call without any
    /// user action. Silent on any failure — cosmetic, not a correctness
    /// requirement.
    fn on_access_token(&self, email: &str, access_token: &str) {
        let Ok(mut config) = pidge_core::Config::load() else {
            return;
        };
        let Some(existing) = config.find(email).cloned() else {
            return;
        };
        if !existing.tenant_id.is_empty() {
            return;
        }
        let Some(tid) = jwt::extract_tenant_id(access_token) else {
            return;
        };
        if tid.is_empty() {
            return;
        }
        let mut updated = existing;
        updated.tenant_id = tid;
        config.add_account(updated);
        let _ = config.save();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use chrono::{Duration, Utc};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::auth::AuthClient;

    /// The kind of backend a hosted consumer would write: tokens live in a
    /// map instead of the keychain, and the refresh path must round-trip
    /// through it.
    #[derive(Default)]
    struct MemoryBackend {
        tokens: Mutex<HashMap<String, TokenSet>>,
        seen_access_tokens: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TokenBackend for MemoryBackend {
        async fn load(&self, email: &str) -> Result<Option<TokenSet>, ClientError> {
            Ok(self.tokens.lock().unwrap().get(email).cloned())
        }

        async fn save(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
            self.tokens
                .lock()
                .unwrap()
                .insert(email.to_string(), tokens.clone());
            Ok(())
        }

        fn on_access_token(&self, _email: &str, access_token: &str) {
            self.seen_access_tokens
                .lock()
                .unwrap()
                .push(access_token.to_string());
        }
    }

    #[tokio::test]
    async fn custom_backend_receives_refreshed_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "refresh_token": "NEW_RT",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = Arc::new(MemoryBackend::default());
        backend
            .save(
                "jane@example.com",
                &TokenSet {
                    access_token: "OLD_AT".into(),
                    refresh_token: "OLD_RT".into(),
                    expires_at: Utc::now() - Duration::seconds(60),
                },
            )
            .await
            .unwrap();

        let auth = AuthClient::for_test("cid", server.uri()).with_backend(backend.clone());
        let token = auth.get_valid_token("jane@example.com").await.unwrap();

        assert_eq!(token, "NEW_AT");
        let stored = backend.load("jane@example.com").await.unwrap().unwrap();
        assert_eq!(stored.refresh_token, "NEW_RT");
        assert_eq!(*backend.seen_access_tokens.lock().unwrap(), vec!["NEW_AT"]);
    }

    #[tokio::test]
    async fn fresh_token_is_returned_without_touching_microsoft() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let backend = Arc::new(MemoryBackend::default());
        backend
            .save(
                "jane@example.com",
                &TokenSet {
                    access_token: "FRESH".into(),
                    refresh_token: "RT".into(),
                    expires_at: Utc::now() + Duration::seconds(3600),
                },
            )
            .await
            .unwrap();

        let auth = AuthClient::for_test("cid", server.uri()).with_backend(backend);
        assert_eq!(
            auth.get_valid_token("jane@example.com").await.unwrap(),
            "FRESH"
        );
    }

    #[tokio::test]
    async fn missing_session_maps_to_session_expired() {
        let server = MockServer::start().await;
        let auth = AuthClient::for_test("cid", server.uri())
            .with_backend(Arc::new(MemoryBackend::default()));
        let err = auth
            .get_valid_token("nobody@example.com")
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::SessionExpired { .. }));
    }
}
