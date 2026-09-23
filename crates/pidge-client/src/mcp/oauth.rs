//! OAuth 2.1 client for pidge's hosted remote MCP server.
//!
//! This talks to the authorization server implemented in
//! `crates/pidge-mcp/src/oauth/`: RFC 9728 protected-resource metadata, RFC
//! 8414 authorization-server metadata, RFC 7591 dynamic client registration,
//! and an authorization-code + PKCE flow with `resource` indicators (RFC
//! 8707) on both `/authorize` and `/token`. The server issues JWT client ids
//! and JWT-backed refresh tokens to public (secret-less) clients — there's
//! no client secret anywhere in this module.

use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use url::Url;

use crate::auth::browser_flow::{
    CallbackParams, make_code_challenge, make_code_verifier, make_random, wait_for_callback,
};
use crate::error::ClientError;

use super::McpTokens;

/// The MCP sign-in callback window. Longer than the 5-minute Microsoft flow
/// timeout since this is a separate, often manually-triggered, step — the
/// user may have just finished (or need to first do) the Microsoft sign-in
/// in the same browser session.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(600);

/// MCP protocol version this client speaks. Sent as the `MCP-Protocol-Version`
/// header on every JSON-RPC call (see [`super::rpc::McpRpc`]) and as
/// `protocolVersion` in the `initialize` request.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The authorization server endpoints discovered for one MCP server.
#[derive(Debug, Clone)]
pub struct Discovery {
    /// The canonical `resource` identifier the protected-resource metadata
    /// advertised (normally the MCP endpoint URL itself).
    pub resource: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// `None` if the server doesn't support RFC 7591 dynamic client
    /// registration — [`register`] fails with a clear error in that case.
    pub registration_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
}

/// Discover the OAuth endpoints that protect `mcp_url` (e.g.
/// `https://mcp.example.com/mcp`).
///
/// Tries `<origin>/.well-known/oauth-protected-resource/mcp` first (mirrors
/// the resource's own path, per common RFC 9728 server conventions), falling
/// back to the bare `<origin>/.well-known/oauth-protected-resource`. Then
/// fetches `<authorization_server>/.well-known/oauth-authorization-server`
/// for the actual endpoints.
pub async fn discover(http: &reqwest::Client, mcp_url: &str) -> Result<Discovery, ClientError> {
    let origin = super::normalize_origin(mcp_url)?;
    let suffixed = format!("{origin}/.well-known/oauth-protected-resource/mcp");
    let bare = format!("{origin}/.well-known/oauth-protected-resource");

    let metadata: ProtectedResourceMetadata = match fetch_json(http, &suffixed).await {
        Ok(m) => m,
        Err(_) => fetch_json(http, &bare).await?,
    };

    let as_origin = metadata
        .authorization_servers
        .first()
        .ok_or_else(|| ClientError::Graph {
            status: 502,
            message: "protected resource metadata has no authorization_servers".to_string(),
        })?
        .trim_end_matches('/');
    let as_metadata: AuthorizationServerMetadata = fetch_json(
        http,
        &format!("{as_origin}/.well-known/oauth-authorization-server"),
    )
    .await?;

    Ok(Discovery {
        resource: metadata.resource,
        authorization_endpoint: as_metadata.authorization_endpoint,
        token_endpoint: as_metadata.token_endpoint,
        registration_endpoint: as_metadata.registration_endpoint,
    })
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<T, ClientError> {
    let resp = http.get(url).send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Register this client with the server's dynamic-client-registration
/// endpoint (RFC 7591). Returns the issued `client_id`.
///
/// Errors with `ClientError::Graph { status: 400, .. }` if `discovery` has no
/// `registration_endpoint` — the server doesn't support DCR.
pub async fn register(
    http: &reqwest::Client,
    discovery: &Discovery,
    redirect_uri: &str,
    client_name: &str,
) -> Result<String, ClientError> {
    let Some(endpoint) = discovery.registration_endpoint.as_deref() else {
        return Err(ClientError::Graph {
            status: 400,
            message: "server does not support dynamic client registration".to_string(),
        });
    };

    let body = json!({
        "redirect_uris": [redirect_uri],
        "client_name": client_name,
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
    });

    let resp = http.post(endpoint).json(&body).send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }

    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    value
        .get("client_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ClientError::Graph {
            status: 502,
            message: "registration response missing client_id".to_string(),
        })
}

/// Run the full sign-in flow against `mcp_url`: discover, register, PKCE
/// authorize via a one-shot localhost callback server, redeem the code.
///
/// `on_open` is called once the authorize URL is built — the caller is
/// expected to print it and best-effort open a browser.
pub async fn sign_in<F: FnOnce(&str)>(
    http: &reqwest::Client,
    mcp_url: &str,
    client_name: &str,
    on_open: F,
) -> Result<McpTokens, ClientError> {
    let discovery = discover(http, mcp_url).await?;

    // Bind first so we know which port to embed in the redirect URI and
    // register.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(ClientError::Io)?;
    let port = listener.local_addr().map_err(ClientError::Io)?.port();
    let redirect_uri = format!("http://localhost:{port}");

    let client_id = register(http, &discovery, &redirect_uri, client_name).await?;

    let verifier = make_code_verifier();
    let challenge = make_code_challenge(&verifier);
    let state = make_random(32);

    let authorize_url = build_authorize_url(
        &discovery.authorization_endpoint,
        &client_id,
        &redirect_uri,
        &challenge,
        &state,
        mcp_url,
    )?;
    on_open(&authorize_url);

    let CallbackParams {
        code,
        state: returned_state,
    } = wait_for_callback(listener, CALLBACK_TIMEOUT).await?;
    if returned_state != state {
        return Err(ClientError::Graph {
            status: 400,
            message: "OAuth state mismatch — possible CSRF or stale request".to_string(),
        });
    }

    exchange_code(
        http,
        &discovery,
        &client_id,
        &code,
        &verifier,
        &redirect_uri,
        mcp_url,
    )
    .await
}

fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    resource: &str,
) -> Result<String, ClientError> {
    let mut url = Url::parse(authorization_endpoint).map_err(|e| ClientError::Graph {
        status: 502,
        message: format!("invalid authorization_endpoint: {e}"),
    })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", resource);
    Ok(url.into())
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

async fn exchange_code(
    http: &reqwest::Client,
    discovery: &Discovery,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    resource: &str,
) -> Result<McpTokens, ClientError> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
        ("resource", resource),
    ];
    let resp = http
        .post(&discovery.token_endpoint)
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }

    let tr: TokenResponse = serde_json::from_slice(&bytes)?;
    Ok(McpTokens {
        server: resource.to_string(),
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.unwrap_or_default(),
        expires_at: Utc::now() + chrono::Duration::seconds(tr.expires_in.unwrap_or(3600)),
        client_id: client_id.to_string(),
    })
}

/// Redeem `current`'s refresh token for a new access/refresh pair.
///
/// Errors with `ClientError::SessionExpired { email: current.server }` if the
/// server rejects the refresh token as `invalid_grant` (revoked or expired).
pub async fn refresh(
    http: &reqwest::Client,
    discovery: &Discovery,
    current: &McpTokens,
) -> Result<McpTokens, ClientError> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", current.refresh_token.as_str()),
        ("client_id", current.client_id.as_str()),
        ("resource", current.server.as_str()),
    ];
    let resp = http
        .post(&discovery.token_endpoint)
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;

    if !status.is_success() {
        let err: ErrorResponse =
            serde_json::from_slice(&bytes).map_err(|_| ClientError::Graph {
                status: status.as_u16(),
                message: String::from_utf8_lossy(&bytes).into_owned(),
            })?;
        if err.error == "invalid_grant" {
            return Err(ClientError::SessionExpired {
                email: current.server.clone(),
            });
        }
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: err.error_description.unwrap_or(err.error),
        });
    }

    let tr: TokenResponse = serde_json::from_slice(&bytes)?;
    Ok(McpTokens {
        server: current.server.clone(),
        access_token: tr.access_token,
        refresh_token: tr
            .refresh_token
            .unwrap_or_else(|| current.refresh_token.clone()),
        expires_at: Utc::now() + chrono::Duration::seconds(tr.expires_in.unwrap_or(3600)),
        client_id: current.client_id.clone(),
    })
}

/// Return a valid access token for `tokens`, refreshing in place if it's
/// within 60 seconds of expiring (or already expired).
///
/// Discovery is re-run on every refresh since this is a low-frequency path
/// (tokens are typically valid for the better part of an hour) and it saves
/// callers from having to thread a `Discovery` through everywhere they hold
/// an `McpTokens`.
pub async fn valid_access_token(
    http: &reqwest::Client,
    mcp_url: &str,
    tokens: &mut McpTokens,
) -> Result<String, ClientError> {
    if !tokens.needs_refresh() {
        return Ok(tokens.access_token.clone());
    }
    let discovery = discover(http, mcp_url).await?;
    let refreshed = refresh(http, &discovery, tokens).await?;
    *tokens = refreshed;
    Ok(tokens.access_token.clone())
}

#[cfg(test)]
mod tests {
    use chrono::Duration as ChronoDuration;
    use wiremock::matchers::{body_partial_json, body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn discovery_for(server: &MockServer) -> Discovery {
        Discovery {
            resource: format!("{}/mcp", server.uri()),
            authorization_endpoint: format!("{}/authorize", server.uri()),
            token_endpoint: format!("{}/token", server.uri()),
            registration_endpoint: Some(format!("{}/register", server.uri())),
        }
    }

    fn sample_tokens(server: &MockServer) -> McpTokens {
        McpTokens {
            server: format!("{}/mcp", server.uri()),
            access_token: "OLD_AT".into(),
            refresh_token: "OLD_RT".into(),
            expires_at: Utc::now() - ChronoDuration::seconds(60),
            client_id: "CID".into(),
        }
    }

    async fn mount_protected_resource(server: &MockServer, at_mcp_suffix: bool) {
        let path_str = if at_mcp_suffix {
            "/.well-known/oauth-protected-resource/mcp"
        } else {
            "/.well-known/oauth-protected-resource"
        };
        Mock::given(method("GET"))
            .and(path(path_str))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resource": format!("{}/mcp", server.uri()),
                "authorization_servers": [server.uri()],
            })))
            .mount(server)
            .await;
    }

    async fn mount_as_metadata(server: &MockServer, registration_endpoint: Option<&str>) {
        let mut body = serde_json::json!({
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        });
        if let Some(reg) = registration_endpoint {
            body["registration_endpoint"] = serde_json::json!(reg);
        }
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn discover_uses_mcp_suffixed_metadata_first() {
        let server = MockServer::start().await;
        mount_protected_resource(&server, true).await;
        mount_as_metadata(&server, Some(&format!("{}/register", server.uri()))).await;

        let http = reqwest::Client::new();
        let discovery = discover(&http, &format!("{}/mcp", server.uri()))
            .await
            .unwrap();

        assert_eq!(discovery.resource, format!("{}/mcp", server.uri()));
        assert_eq!(
            discovery.authorization_endpoint,
            format!("{}/authorize", server.uri())
        );
        assert_eq!(discovery.token_endpoint, format!("{}/token", server.uri()));
        assert_eq!(
            discovery.registration_endpoint,
            Some(format!("{}/register", server.uri()))
        );
    }

    #[tokio::test]
    async fn discover_falls_back_to_bare_metadata_path() {
        let server = MockServer::start().await;
        // Only the bare path is mounted — the /mcp-suffixed GET 404s.
        mount_protected_resource(&server, false).await;
        mount_as_metadata(&server, None).await;

        let http = reqwest::Client::new();
        let discovery = discover(&http, &format!("{}/mcp", server.uri()))
            .await
            .unwrap();

        assert_eq!(discovery.registration_endpoint, None);
    }

    #[tokio::test]
    async fn register_posts_expected_json_and_returns_client_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .and(body_partial_json(serde_json::json!({
                "redirect_uris": ["http://localhost:54321"],
                "client_name": "pidge",
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "client_id": "issued-client-jwt",
                "client_id_issued_at": 0,
                "client_secret_expires_at": 0,
                "redirect_uris": ["http://localhost:54321"],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let discovery = discovery_for(&server);
        let client_id = register(&http, &discovery, "http://localhost:54321", "pidge")
            .await
            .unwrap();

        assert_eq!(client_id, "issued-client-jwt");
    }

    #[tokio::test]
    async fn register_errors_when_registration_endpoint_missing() {
        let server = MockServer::start().await;
        let mut discovery = discovery_for(&server);
        discovery.registration_endpoint = None;

        let http = reqwest::Client::new();
        let err = register(&http, &discovery, "http://localhost:1", "pidge")
            .await
            .unwrap_err();

        match err {
            ClientError::Graph { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    "server does not support dynamic client registration"
                );
            }
            other => panic!("expected ClientError::Graph, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_on_expiry_calls_refresh_grant_once_and_returns_new_tokens() {
        let server = MockServer::start().await;
        // The refresh grant is form-encoded (not JSON), so match on
        // substrings of the form body rather than `body_partial_json`.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=OLD_RT"))
            .and(body_string_contains("client_id=CID"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "refresh_token": "NEW_RT",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let discovery = discovery_for(&server);
        let old_tokens = sample_tokens(&server);

        let new_tokens = refresh(&http, &discovery, &old_tokens).await.unwrap();

        assert_eq!(new_tokens.access_token, "NEW_AT");
        assert_eq!(new_tokens.refresh_token, "NEW_RT");
        assert_eq!(new_tokens.server, old_tokens.server);
        assert_eq!(new_tokens.client_id, old_tokens.client_id);
    }

    #[tokio::test]
    async fn valid_access_token_refreshes_expired_tokens_via_discovery() {
        let server = MockServer::start().await;
        mount_protected_resource(&server, true).await;
        mount_as_metadata(&server, Some(&format!("{}/register", server.uri()))).await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "refresh_token": "NEW_RT",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let mut tokens = sample_tokens(&server);
        let server_url = tokens.server.clone();

        let access = valid_access_token(&http, &server_url, &mut tokens)
            .await
            .unwrap();

        assert_eq!(access, "NEW_AT");
        assert_eq!(tokens.access_token, "NEW_AT");
    }

    #[tokio::test]
    async fn refresh_maps_invalid_grant_to_session_expired() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "refresh token revoked"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let discovery = discovery_for(&server);
        let tokens = sample_tokens(&server);

        let err = refresh(&http, &discovery, &tokens).await.unwrap_err();
        match err {
            ClientError::SessionExpired { email } => assert_eq!(email, tokens.server),
            other => panic!("expected SessionExpired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fresh_token_skips_the_network_entirely() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let mut tokens = sample_tokens(&server);
        tokens.expires_at = Utc::now() + ChronoDuration::seconds(3600);
        let server_url = tokens.server.clone();

        let access = valid_access_token(&http, &server_url, &mut tokens)
            .await
            .unwrap();
        assert_eq!(access, "OLD_AT");
    }
}
