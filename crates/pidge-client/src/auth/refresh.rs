//! OAuth refresh-token grant.

use chrono::Utc;

use crate::auth::device_code::{ErrorResponse, TokenResponse};
use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

/// Refresh the access token using the stored refresh token.
///
/// Returns the new `TokenSet`. If Microsoft rotates the refresh token, it's
/// included in the response and used. If not, the current refresh token is preserved.
///
/// Errors:
/// - `ClientError::SessionExpired` if Microsoft returns `invalid_grant`
/// - `ClientError::Graph` for other HTTP errors
pub async fn refresh(
    client: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    current: &TokenSet,
    scope: &str,
    email: &str,
) -> Result<TokenSet, ClientError> {
    let url = format!("{base_url}/oauth2/v2.0/token");
    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", &current.refresh_token),
            ("scope", scope),
        ])
        .send()
        .await?;

    let status = resp.status();
    let body = resp.bytes().await?;

    if !status.is_success() {
        let err: ErrorResponse = serde_json::from_slice(&body).map_err(|_| ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&body).into_owned(),
        })?;
        if err.error == "invalid_grant" {
            return Err(ClientError::SessionExpired {
                email: email.to_string(),
            });
        }
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: err.error_description.unwrap_or(err.error),
        });
    }

    let tr: TokenResponse = serde_json::from_slice(&body)?;
    let expires = tr.expires_in.unwrap_or(3600);
    let new_refresh = tr.refresh_token.unwrap_or_else(|| current.refresh_token.clone());

    Ok(TokenSet {
        access_token: tr.access_token,
        refresh_token: new_refresh,
        expires_at: Utc::now() + chrono::Duration::seconds(expires as i64 - 60),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn old_tokens() -> TokenSet {
        TokenSet {
            access_token: "OLD_AT".into(),
            refresh_token: "OLD_RT".into(),
            expires_at: Utc::now() - Duration::seconds(60),
        }
    }

    #[tokio::test]
    async fn refresh_returns_new_tokens_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "refresh_token": "NEW_RT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let new = refresh(&client, &server.uri(), "CID", &old_tokens(), "scope", "u@e.com")
            .await
            .unwrap();
        assert_eq!(new.access_token, "NEW_AT");
        assert_eq!(new.refresh_token, "NEW_RT");
    }

    #[tokio::test]
    async fn refresh_preserves_refresh_token_when_response_omits_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let new = refresh(&client, &server.uri(), "CID", &old_tokens(), "scope", "u@e.com")
            .await
            .unwrap();
        assert_eq!(new.access_token, "NEW_AT");
        assert_eq!(new.refresh_token, "OLD_RT");
    }

    #[tokio::test]
    async fn refresh_returns_session_expired_on_invalid_grant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "AADSTS50173: refresh token expired"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = refresh(&client, &server.uri(), "CID", &old_tokens(), "scope", "u@e.com")
            .await
            .unwrap_err();
        match err {
            ClientError::SessionExpired { email } => assert_eq!(email, "u@e.com"),
            other => panic!("expected SessionExpired, got {other:?}"),
        }
    }
}
