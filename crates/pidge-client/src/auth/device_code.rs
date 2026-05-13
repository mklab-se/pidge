//! RFC 8628 OAuth 2.0 device authorization grant flow.

use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

/// Response from the devicecode endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    #[serde(default)]
    pub message: Option<String>,
}

/// Token response. `refresh_token` is optional because refresh-token-rotation
/// responses sometimes omit it; for device-code initial responses Microsoft always
/// includes it (the `offline_access` scope is requested).
#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Result of a successful poll: tokens + the id_token (for tenant_id extraction).
#[derive(Debug)]
pub struct PollSuccess {
    pub tokens: TokenSet,
    pub id_token: Option<String>,
}

/// POST to `{base_url}/oauth2/v2.0/devicecode`.
pub async fn start(
    client: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    scope: &str,
) -> Result<DeviceCodeResponse, ClientError> {
    let url = format!("{base_url}/oauth2/v2.0/devicecode");
    let resp = client
        .post(&url)
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }

    Ok(resp.json::<DeviceCodeResponse>().await?)
}

/// Poll `{base_url}/oauth2/v2.0/token` per RFC 8628 §3.5.
/// Returns when the user has approved consent, or with an error on denial / expiry.
///
/// The `sleep` parameter is a closure so tests can inject a fast no-op sleep
/// instead of real `tokio::time::sleep`.
pub async fn poll<F, Fut>(
    client: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    device_code: &str,
    initial_interval: u64,
    expires_in: u64,
    mut sleep: F,
) -> Result<PollSuccess, ClientError>
where
    F: FnMut(Duration) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let url = format!("{base_url}/oauth2/v2.0/token");
    let mut interval = initial_interval;
    let deadline = Utc::now() + chrono::Duration::seconds(expires_in as i64);

    loop {
        if Utc::now() >= deadline {
            return Err(ClientError::DeviceCodeTimeout);
        }
        sleep(Duration::from_secs(interval)).await;

        let resp = client
            .post(&url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device_code),
            ])
            .send()
            .await?;

        let status = resp.status();
        let body = resp.bytes().await?;

        if status.is_success() {
            let tr: TokenResponse = serde_json::from_slice(&body)?;
            let expires = tr.expires_in.unwrap_or(3600);
            let refresh_token = tr.refresh_token.ok_or(ClientError::MissingAccessToken)?;
            return Ok(PollSuccess {
                tokens: TokenSet {
                    access_token: tr.access_token,
                    refresh_token,
                    expires_at: Utc::now() + chrono::Duration::seconds(expires as i64 - 60),
                },
                id_token: tr.id_token,
            });
        }

        let err: ErrorResponse = serde_json::from_slice(&body).map_err(|_| ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&body).into_owned(),
        })?;

        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += 5;
                continue;
            }
            "access_denied" => return Err(ClientError::DeviceCodeAccessDenied),
            "expired_token" => return Err(ClientError::DeviceCodeTimeout),
            other => {
                return Err(ClientError::DeviceCodeOther {
                    kind: other.to_string(),
                    description: err.error_description,
                });
            }
        }
    }
}

/// Real-time sleep helper for production use.
pub async fn real_sleep(d: Duration) {
    tokio::time::sleep(d).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Test sleep: no-op; we don't actually want to wait in tests.
    async fn no_sleep(_: Duration) {}

    #[tokio::test]
    async fn start_returns_device_code_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/devicecode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "DC123",
                "user_code": "ABCD-1234",
                "verification_uri": "https://microsoft.com/devicelogin",
                "expires_in": 900,
                "interval": 5,
                "message": "Please sign in"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let resp = start(&client, &server.uri(), "CID", "openid")
            .await
            .unwrap();
        assert_eq!(resp.user_code, "ABCD-1234");
        assert_eq!(resp.interval, 5);
    }

    #[tokio::test]
    async fn poll_returns_tokens_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT123",
                "refresh_token": "RT123",
                "expires_in": 3600,
                "id_token": "eyJh.eyJ0aWQiOiJ0aWQifQ.sig"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 1, 60, no_sleep)
            .await
            .unwrap();
        assert_eq!(result.tokens.access_token, "AT123");
        assert_eq!(result.tokens.refresh_token, "RT123");
        assert_eq!(
            result.id_token.as_deref(),
            Some("eyJh.eyJ0aWQiOiJ0aWQifQ.sig")
        );
    }

    #[tokio::test]
    async fn poll_retries_on_authorization_pending_then_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending"
            })))
            .up_to_n_times(2)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT",
                "refresh_token": "RT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep)
            .await
            .unwrap();
        assert_eq!(result.tokens.access_token, "AT");
    }

    #[tokio::test]
    async fn poll_returns_access_denied_on_user_cancel() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep).await;
        assert!(matches!(result, Err(ClientError::DeviceCodeAccessDenied)));
    }

    #[tokio::test]
    async fn poll_returns_timeout_on_expired_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "expired_token"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep).await;
        assert!(matches!(result, Err(ClientError::DeviceCodeTimeout)));
    }

    #[tokio::test]
    async fn poll_returns_other_on_unknown_error_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "consent_required",
                "error_description": "AADSTS65001: consent needed"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep).await;
        match result {
            Err(ClientError::DeviceCodeOther { kind, description }) => {
                assert_eq!(kind, "consent_required");
                assert_eq!(description.as_deref(), Some("AADSTS65001: consent needed"));
            }
            other => panic!("expected DeviceCodeOther, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn poll_slow_down_increases_interval_then_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "slow_down"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT",
                "refresh_token": "RT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll(&client, &server.uri(), "CID", "DC", 0, 60, no_sleep)
            .await
            .unwrap();
        assert_eq!(result.tokens.access_token, "AT");
    }
}
