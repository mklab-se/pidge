//! OAuth browser-based sign-in (auth-code + PKCE), token refresh, and
//! credential storage.

mod backend;
pub mod browser_flow;
pub mod config;
pub mod device_code;
pub(crate) mod file_store;
mod jwt;
pub mod refresh;
mod store;
mod token_store;
mod tokens;

pub use backend::{LocalBackend, TokenBackend};
pub use browser_flow::AuthSuccess;
pub use file_store::FileStore;
pub use jwt::{IdTokenClaims, extract_id_claims, extract_tenant_id};
pub use store::KeychainStore;
pub use token_store::TokenStore;
pub use tokens::TokenSet;

use std::sync::Arc;

use crate::error::ClientError;

/// High-level auth client. Holds a shared `reqwest::Client` and the resolved
/// `client_id`; provides device-code sign-in and access-token retrieval (with
/// transparent refresh).
pub struct AuthClient {
    http: reqwest::Client,
    client_id: String,
    authority_base: String,
    scope: String,
    backend: Arc<dyn TokenBackend>,
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
            backend: Arc::new(LocalBackend),
        })
    }

    /// Like [`Self::from_env`], but tokens are loaded from and saved to
    /// `backend` instead of the CLI's config-resolved keychain/file store.
    /// This is the constructor for hosted consumers such as the MCP server.
    pub fn from_env_with_backend(backend: Arc<dyn TokenBackend>) -> Result<Self, ClientError> {
        let mut client = Self::from_env()?;
        client.backend = backend;
        Ok(client)
    }

    /// Replace the token backend (builder style). Handy for tests that pair
    /// [`Self::for_test`] with an in-memory store.
    pub fn with_backend(mut self, backend: Arc<dyn TokenBackend>) -> Self {
        self.backend = backend;
        self
    }

    /// The space-separated Microsoft Graph scope string this client requests.
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// The Entra `client_id` this client authenticates as.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Persist a freshly obtained [`TokenSet`] for `email` through the
    /// configured backend. Hosted sign-in flows call this after
    /// [`Self::exchange_code`].
    pub async fn store_tokens(&self, email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        self.backend.save(email, tokens).await
    }

    /// Build the Microsoft `/authorize` URL for an auth-code + PKCE sign-in
    /// whose callback lands on `redirect_uri` (which must be registered on
    /// the Entra app). The caller owns `state` and the PKCE verifier behind
    /// `code_challenge`; pair with [`Self::exchange_code`].
    pub fn authorize_url(&self, redirect_uri: &str, code_challenge: &str, state: &str) -> String {
        self.authorize_url_with_hint(redirect_uri, code_challenge, state, None)
    }

    /// [`Self::authorize_url`] with a `login_hint`: the address Microsoft
    /// preselects in its account picker. The picker is still shown
    /// (`prompt=select_account`), so the user can pick another account; the
    /// hint only makes the expected one the obvious choice.
    pub fn authorize_url_with_hint(
        &self,
        redirect_uri: &str,
        code_challenge: &str,
        state: &str,
        login_hint: Option<&str>,
    ) -> String {
        browser_flow::build_authorize_url_with_hint(
            &self.authority_base,
            &self.client_id,
            redirect_uri,
            &self.scope,
            code_challenge,
            state,
            login_hint,
        )
    }

    /// Redeem an authorization code delivered to `redirect_uri` for tokens.
    /// Nothing is stored; call [`Self::store_tokens`] once the caller has
    /// decided which account the tokens belong to.
    pub async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<AuthSuccess, ClientError> {
        browser_flow::exchange_code_to_success(
            &self.http,
            &self.authority_base,
            &self.client_id,
            code,
            code_verifier,
            redirect_uri,
        )
        .await
    }

    /// Construct an AuthClient against a specific authority, for tests with wiremock.
    pub fn for_test(client_id: impl Into<String>, authority_base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            client_id: client_id.into(),
            authority_base: authority_base.into(),
            scope: config::scope_string(),
            backend: Arc::new(LocalBackend),
        }
    }

    /// Run the OAuth 2.0 authorization-code + PKCE sign-in flow with a
    /// one-shot localhost HTTP server for the redirect callback.
    ///
    /// `on_authorize_url_ready` receives the constructed `/authorize` URL
    /// once the local listener is bound and the URL is built. The caller
    /// is responsible for printing it to the user and (best-effort) opening
    /// the browser.
    ///
    /// Works for both work/school (M365) and personal (live.com /
    /// outlook.com / hotmail.com) Microsoft accounts. Device-code is kept
    /// in tree for potential future headless use but no longer the
    /// default sign-in path.
    pub async fn run_browser_flow<F>(
        &self,
        on_authorize_url_ready: F,
    ) -> Result<AuthSuccess, ClientError>
    where
        F: FnOnce(&str),
    {
        browser_flow::run(
            &self.http,
            &self.authority_base,
            &self.client_id,
            &self.scope,
            on_authorize_url_ready,
        )
        .await
    }

    /// Get a valid (un-expired) access token for an email, refreshing if necessary.
    /// Returns `ClientError::SessionExpired` if the refresh fails; the caller should
    /// prompt the user to `pidge auth login` again for that account.
    ///
    /// Tokens come from the configured [`TokenBackend`]. The default,
    /// [`LocalBackend`], resolves the storage backend from the account's
    /// config entry and falls back to the OS keychain if the email has no
    /// entry in `config.yaml` yet (e.g. mid-login).
    pub async fn get_valid_token(&self, email: &str) -> Result<String, ClientError> {
        let tokens =
            self.backend
                .load(email)
                .await?
                .ok_or_else(|| ClientError::SessionExpired {
                    email: email.to_string(),
                })?;

        let access_token = if tokens.needs_refresh() {
            let new_tokens = refresh::refresh(
                &self.http,
                &self.authority_base,
                &self.client_id,
                &tokens,
                &self.scope,
                email,
            )
            .await?;
            self.backend.save(email, &new_tokens).await?;
            new_tokens.access_token
        } else {
            tokens.access_token
        };

        self.backend.on_access_token(email, &access_token);

        Ok(access_token)
    }
}
