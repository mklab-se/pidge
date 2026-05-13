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
        device_code::start(
            &self.http,
            &self.authority_base,
            &self.client_id,
            &self.scope,
        )
        .await
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
