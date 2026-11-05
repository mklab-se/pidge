//! Google sign-in: authorization code + PKCE on a loopback redirect, token
//! exchange and refresh, and an access-token source for [`crate::GmailProvider`].
//!
//! pidge cannot ship a Google OAuth client for everyone (`gmail.modify` is a
//! restricted scope), so whoever runs pidge brings their own: a "Desktop
//! app" client locally (`PIDGE_GOOGLE_CLIENT_ID` / `PIDGE_GOOGLE_CLIENT_SECRET`),
//! a "Web application" client when hosted. Google treats a desktop client's
//! secret as non-confidential, but its token endpoint still wants it.
//!
//! Token persistence reuses `pidge-client`'s [`TokenBackend`] seam, so the
//! CLI's keychain/file store ([`pidge_client::auth::LocalBackend`]) and the
//! hosted server's secret store both work unchanged.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD_INDIFFERENT;
use chrono::Utc;
use pidge_client::auth::browser_flow::{
    make_code_challenge, make_code_verifier, make_random, wait_for_callback,
};
use pidge_client::auth::{TokenBackend, TokenSet};
use pidge_core::provider::{ProviderError, ProviderResult};
use serde::Deserialize;

/// Google's authorization endpoint.
pub const GOOGLE_AUTH_BASE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint.
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// The scopes pidge asks Google for, space separated.
///
/// - `openid email`: the id token's `email` claim names the account.
/// - `gmail.modify`: read, label, archive, trash, draft and send. Never
///   permanent deletion (that would need the full `mail.google.com` scope).
/// - `calendar.events`: read and write events on the user's calendars.
/// - `calendar.freebusy`: `freeBusy.query` for other people. It is the
///   narrowest scope that endpoint accepts; `calendar.readonly` would also
///   work but grants reading every calendar's details, which pidge never
///   needs (it only uses the `primary` calendar, so no `calendarList` read).
pub const SCOPES: &str = "openid email https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.freebusy";

/// How long the loopback flow waits for the browser.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// A Google OAuth client.
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Authorization endpoint (default [`GOOGLE_AUTH_BASE`]).
    pub auth_base: String,
    /// Token endpoint (default [`GOOGLE_TOKEN_URL`]).
    pub token_url: String,
}

impl std::fmt::Debug for GoogleOAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleOAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("auth_base", &self.auth_base)
            .field("token_url", &self.token_url)
            .finish()
    }
}

impl GoogleOAuthConfig {
    /// A client with Google's real endpoints.
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_base: GOOGLE_AUTH_BASE.to_string(),
            token_url: GOOGLE_TOKEN_URL.to_string(),
        }
    }

    /// From `PIDGE_GOOGLE_CLIENT_ID` and `PIDGE_GOOGLE_CLIENT_SECRET`; `None`
    /// unless both are set and non-empty.
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Some(Self::new(
            get("PIDGE_GOOGLE_CLIENT_ID")?,
            get("PIDGE_GOOGLE_CLIENT_SECRET")?,
        ))
    }
}

/// The URL to send the user's browser to. `challenge` is the PKCE S256
/// challenge, `state` the CSRF nonce. `access_type=offline` plus
/// `prompt=consent` makes Google return a refresh token every time.
pub fn authorize_url(
    cfg: &GoogleOAuthConfig,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    login_hint: Option<&str>,
) -> String {
    let mut url = url::Url::parse(&cfg.auth_base)
        .unwrap_or_else(|_| url::Url::parse(GOOGLE_AUTH_BASE).expect("constant URL parses"));
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &cfg.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPES)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("include_granted_scopes", "true")
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);
        if let Some(hint) = login_hint.map(str::trim).filter(|h| !h.is_empty()) {
            q.append_pair("login_hint", hint);
        }
    }
    url.into()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Deserialize, Default)]
struct TokenError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// POST a form to the token endpoint. `invalid_grant` (a revoked or
/// expired refresh token, or a stale code) is `Auth` with an empty account;
/// callers that know the account fill it in.
async fn token_request(
    http: &reqwest::Client,
    cfg: &GoogleOAuthConfig,
    form: &[(&str, &str)],
) -> ProviderResult<TokenResponse> {
    let resp = crate::http::send_with_retry(http.post(&cfg.token_url).form(form)).await?;
    let status = resp.status();
    let text = resp.text().await.map_err(crate::http::transport)?;
    if status.is_success() {
        return serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("unreadable Google token response: {e}")));
    }
    let err: TokenError = serde_json::from_str(&text).unwrap_or_default();
    let message = if err.error_description.is_empty() {
        format!("{} (HTTP {})", err.error, status.as_u16())
    } else {
        format!("{}: {}", err.error, err.error_description)
    };
    Err(match err.error.as_str() {
        "invalid_grant" | "unauthorized_client" | "invalid_client" => ProviderError::Auth {
            account: String::new(),
            message,
        },
        _ => ProviderError::Other(format!("Google token endpoint: {message}")),
    })
}

fn expires_at(expires_in: Option<i64>) -> chrono::DateTime<Utc> {
    Utc::now() + chrono::Duration::seconds(expires_in.unwrap_or(3600).max(0))
}

/// The `email` claim of an id token. Not verified: the token came straight
/// from Google's token endpoint over TLS, in answer to our own code.
fn email_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD_INDIFFERENT.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase())
}

/// Exchange an authorization code for tokens; also returns the account's
/// address from the id token.
pub async fn exchange_code(
    http: &reqwest::Client,
    cfg: &GoogleOAuthConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> ProviderResult<(TokenSet, String)> {
    let r = token_request(
        http,
        cfg,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("client_id", &cfg.client_id),
            ("client_secret", &cfg.client_secret),
        ],
    )
    .await?;
    let email = r
        .id_token
        .as_deref()
        .and_then(email_from_id_token)
        .ok_or_else(|| ProviderError::Other("Google did not say which account signed in".into()))?;
    let refresh_token = r.refresh_token.ok_or_else(|| {
        ProviderError::Other("Google returned no refresh token; sign in again".into())
    })?;
    Ok((
        TokenSet {
            access_token: r.access_token,
            refresh_token,
            expires_at: expires_at(r.expires_in),
            scope: r.scope,
        },
        email,
    ))
}

/// Refresh `tokens`. Google usually leaves `refresh_token` out of a refresh
/// response: the old one stays valid and is kept.
pub async fn refresh(
    http: &reqwest::Client,
    cfg: &GoogleOAuthConfig,
    tokens: &TokenSet,
) -> ProviderResult<TokenSet> {
    let r = token_request(
        http,
        cfg,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &tokens.refresh_token),
            ("client_id", &cfg.client_id),
            ("client_secret", &cfg.client_secret),
        ],
    )
    .await?;
    Ok(TokenSet {
        access_token: r.access_token,
        refresh_token: r
            .refresh_token
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| tokens.refresh_token.clone()),
        expires_at: expires_at(r.expires_in),
        scope: r.scope.or_else(|| tokens.scope.clone()),
    })
}

/// The whole local sign-in: listen on `127.0.0.1:<random port>`, hand the
/// authorization URL to `on_open` (open a browser, print it), wait up to
/// five minutes for Google's redirect, check `state`, exchange the code.
pub async fn run_loopback_flow(
    cfg: &GoogleOAuthConfig,
    on_open: impl FnOnce(&str),
) -> ProviderResult<(TokenSet, String)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| ProviderError::Other(format!("could not listen for the sign-in: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| ProviderError::Other(format!("could not listen for the sign-in: {e}")))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let verifier = make_code_verifier();
    let challenge = make_code_challenge(&verifier);
    let state = make_random(32);
    on_open(&authorize_url(cfg, &redirect_uri, &challenge, &state, None));
    let params = wait_for_callback(listener, CALLBACK_TIMEOUT, "Google sign-in")
        .await
        .map_err(|e| ProviderError::Other(e.to_string()))?;
    if params.state != state {
        return Err(ProviderError::Invalid(
            "the sign-in answer did not match this sign-in (state mismatch)".into(),
        ));
    }
    let http = http_client()?;
    exchange_code(&http, cfg, &params.code, &verifier, &redirect_uri).await
}

/// A `reqwest::Client` with pidge's user agent.
pub(crate) fn http_client() -> ProviderResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(crate::http::transport)
}

/// Access tokens for one Google account, refreshed on demand and saved
/// back through `backend`.
pub struct GoogleAuth {
    http: reqwest::Client,
    cfg: GoogleOAuthConfig,
    backend: Arc<dyn TokenBackend>,
    email: String,
    /// Serializes refreshes so two concurrent calls don't both spend the
    /// refresh token.
    refresh_lock: tokio::sync::Mutex<()>,
}

impl GoogleAuth {
    pub fn new(
        http: reqwest::Client,
        cfg: GoogleOAuthConfig,
        backend: Arc<dyn TokenBackend>,
        email: impl Into<String>,
    ) -> Self {
        Self {
            http,
            cfg,
            backend,
            email: email.into(),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The account's address.
    pub fn email(&self) -> &str {
        &self.email
    }

    /// The HTTP client this account's calls share.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn auth_error(&self, message: impl Into<String>) -> ProviderError {
        ProviderError::Auth {
            account: self.email.clone(),
            message: message.into(),
        }
    }

    async fn load(&self) -> ProviderResult<TokenSet> {
        self.backend
            .load(&self.email)
            .await
            .map_err(|e| ProviderError::Other(format!("could not read stored tokens: {e}")))?
            .ok_or_else(|| self.auth_error("no stored session"))
    }

    /// A valid access token, refreshing (and saving) first when the stored
    /// one is within a minute of expiry.
    pub async fn access_token(&self) -> ProviderResult<String> {
        let tokens = self.load().await?;
        if !tokens.needs_refresh() {
            return Ok(tokens.access_token);
        }
        let _guard = self.refresh_lock.lock().await;
        // Another task may have refreshed while we waited.
        let tokens = self.load().await?;
        if !tokens.needs_refresh() {
            return Ok(tokens.access_token);
        }
        let fresh = refresh(&self.http, &self.cfg, &tokens)
            .await
            .map_err(|e| match e {
                ProviderError::Auth { message, .. } => self.auth_error(message),
                other => other,
            })?;
        self.backend
            .save(&self.email, &fresh)
            .await
            .map_err(|e| ProviderError::Other(format!("could not save refreshed tokens: {e}")))?;
        Ok(fresh.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MemoryBackend;
    use chrono::Duration as ChronoDuration;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg(server: &MockServer) -> GoogleOAuthConfig {
        GoogleOAuthConfig {
            client_id: "cid".into(),
            client_secret: "secret".into(),
            auth_base: format!("{}/auth", server.uri()),
            token_url: format!("{}/token", server.uri()),
        }
    }

    #[test]
    fn authorize_url_asks_for_offline_access_with_pkce() {
        let c = GoogleOAuthConfig::new("cid", "s");
        let u = url::Url::parse(&authorize_url(
            &c,
            "http://127.0.0.1:5555",
            "CH",
            "ST",
            Some("jane@example.com"),
        ))
        .unwrap();
        assert_eq!(u.host_str(), Some("accounts.google.com"));
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "CH");
        assert_eq!(q["state"], "ST");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5555");
        assert_eq!(q["login_hint"], "jane@example.com");
        assert!(q["scope"].contains("gmail.modify"));
    }

    #[tokio::test]
    async fn exchange_reads_the_email_from_the_id_token() {
        let server = MockServer::start().await;
        let claims = URL_SAFE_NO_PAD_INDIFFERENT.encode(br#"{"email":"Jane@Example.com"}"#);
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code_verifier=VER"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT",
                "refresh_token": "RT",
                "expires_in": 3599,
                "scope": SCOPES,
                "id_token": format!("h.{claims}.s"),
            })))
            .mount(&server)
            .await;
        let (t, email) = exchange_code(
            &reqwest::Client::new(),
            &cfg(&server),
            "CODE",
            "VER",
            "http://127.0.0.1:1",
        )
        .await
        .unwrap();
        assert_eq!(email, "jane@example.com");
        assert_eq!(t.access_token, "AT");
        assert_eq!(t.refresh_token, "RT");
    }

    #[tokio::test]
    async fn refresh_keeps_the_old_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=OLD_RT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "NEW_AT",
                "expires_in": 3599
            })))
            .expect(1)
            .mount(&server)
            .await;
        let backend = Arc::new(MemoryBackend::default());
        backend.put(
            "jane@example.com",
            TokenSet {
                access_token: "OLD_AT".into(),
                refresh_token: "OLD_RT".into(),
                expires_at: Utc::now() - ChronoDuration::seconds(10),
                scope: Some(SCOPES.into()),
            },
        );
        let auth = GoogleAuth::new(
            reqwest::Client::new(),
            cfg(&server),
            backend.clone(),
            "jane@example.com",
        );
        assert_eq!(auth.access_token().await.unwrap(), "NEW_AT");
        let stored = backend.get("jane@example.com").unwrap();
        assert_eq!(stored.access_token, "NEW_AT");
        assert_eq!(stored.refresh_token, "OLD_RT");
        assert_eq!(stored.scope.as_deref(), Some(SCOPES));
        // Fresh now: no second refresh.
        assert_eq!(auth.access_token().await.unwrap(), "NEW_AT");
    }

    #[tokio::test]
    async fn invalid_grant_is_an_auth_error_for_the_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Token has been expired or revoked."
            })))
            .mount(&server)
            .await;
        let backend = Arc::new(MemoryBackend::default());
        backend.put(
            "jane@example.com",
            TokenSet {
                access_token: "OLD_AT".into(),
                refresh_token: "OLD_RT".into(),
                expires_at: Utc::now() - ChronoDuration::seconds(10),
                scope: None,
            },
        );
        let auth = GoogleAuth::new(
            reqwest::Client::new(),
            cfg(&server),
            backend,
            "jane@example.com",
        );
        match auth.access_token().await.unwrap_err() {
            ProviderError::Auth { account, message } => {
                assert_eq!(account, "jane@example.com");
                assert!(message.contains("invalid_grant"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }
}
