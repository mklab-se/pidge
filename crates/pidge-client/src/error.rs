//! Error type for `pidge-client`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("pidge has not been provisioned yet. The maintainer needs to run scripts/register-pidge-app.sh and update APP_CLIENT_ID.")]
    NotProvisioned,

    #[error("OS keychain unavailable: {0}")]
    Keychain(#[from] keyring::Error),

    #[error("device code flow: authorization pending exceeded poll deadline")]
    DeviceCodeTimeout,

    #[error("device code flow: user denied consent")]
    DeviceCodeAccessDenied,

    #[error("device code flow: {kind}{}", description.as_ref().map(|d| format!(" — {d}")).unwrap_or_default())]
    DeviceCodeOther {
        kind: String,
        description: Option<String>,
    },

    #[error("session expired for {email}. Run `pidge auth login` to re-add this account.")]
    SessionExpired { email: String },

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Microsoft Graph: {status} {message}")]
    Graph { status: u16, message: String },

    #[error("core: {0}")]
    Core(#[from] pidge_core::CoreError),

    #[error("token response missing access_token")]
    MissingAccessToken,
}
