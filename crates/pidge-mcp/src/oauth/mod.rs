//! The server's own OAuth 2.1 authorization server.
//!
//! MCP clients (Claude, ChatGPT, Claude Code, MCP Inspector, …) discover it
//! through RFC 9728 protected-resource metadata, register themselves with
//! RFC 7591 dynamic client registration, and run an authorization-code +
//! PKCE flow. The *user* authenticates at Microsoft: `/authorize` bounces to
//! Microsoft login, `/callback` verifies the returned account against the
//! allowlist and, because the very same sign-in grants Graph access to that
//! account's mailbox, stores its refresh token — sign-in and mailbox
//! connection are one step.
//!
//! No database: clients, codes and tokens are all signed JWTs (see [`jwt`]).
//! The only in-memory state is the minutes-long window between `/authorize`
//! and `/callback`, and the set of already-redeemed codes.

pub mod bearer;
#[cfg(test)]
mod flow_tests;
pub mod jwt;
mod pages;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use pidge_client::auth::TokenSet;

use crate::state::{PendingAuthorization, PendingKind, SharedState};
use crate::users::{MailboxRecord, OwnershipError, UserRecord, user_hash};

pub const SCOPE: &str = "mail";

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route(
            "/.well-known/openid-configuration",
            get(authorization_server),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/callback", get(callback))
        .route("/connect", get(connect))
        .route("/token", post(token))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

async fn protected_resource(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(json!({
        "resource": state.config.resource_url(),
        "authorization_servers": [state.config.base_url()],
        "bearer_methods_supported": ["header"],
        "scopes_supported": [SCOPE],
        "resource_name": "pidge",
    }))
}

async fn authorization_server(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let base = state
        .config
        .public_url
        .as_str()
        .trim_end_matches('/')
        .to_string();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE],
        "service_documentation": "https://github.com/mklab-se/pidge",
    }))
}

// ---------------------------------------------------------------------------
// Dynamic client registration (RFC 7591)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterResponse {
    client_id: String,
    client_id_issued_at: i64,
    client_secret_expires_at: i64,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: &'static str,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

fn oauth_error(status: StatusCode, error: &str, description: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_description": description.into() })),
    )
        .into_response()
}

/// Loopback redirects are allowed over plain http (Claude Code, MCP
/// Inspector); anything else must be https.
fn redirect_uri_is_acceptable(uri: &str) -> bool {
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    if url.fragment().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

async fn register(State(state): State<SharedState>, Json(req): Json<RegisterRequest>) -> Response {
    if req.redirect_uris.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "redirect_uris is required",
        );
    }
    if let Some(bad) = req
        .redirect_uris
        .iter()
        .find(|u| !redirect_uri_is_acceptable(u))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            format!("redirect URI not acceptable: {bad}"),
        );
    }

    let client_id = match state
        .signer
        .issue_client(req.redirect_uris.clone(), req.client_name.clone())
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "issuing client id");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "could not register",
            );
        }
    };

    tracing::info!(client_name = ?req.client_name, redirect_uris = ?req.redirect_uris, "registered client");

    (
        StatusCode::CREATED,
        Json(RegisterResponse {
            client_id,
            client_id_issued_at: chrono::Utc::now().timestamp(),
            client_secret_expires_at: 0,
            redirect_uris: req.redirect_uris,
            token_endpoint_auth_method: "none",
            grant_types: req
                .grant_types
                .unwrap_or_else(|| vec!["authorization_code".into(), "refresh_token".into()]),
            response_types: req.response_types.unwrap_or_else(|| vec!["code".into()]),
            client_name: req.client_name,
            scope: req.scope,
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Authorization endpoint → Microsoft
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
    resource: Option<String>,
}

fn redirect_with_error(
    redirect_uri: &str,
    client_state: Option<&str>,
    error: &str,
    description: &str,
) -> Response {
    let mut url = Url::parse(redirect_uri).expect("validated at registration");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("error", error);
        q.append_pair("error_description", description);
        if let Some(s) = client_state {
            q.append_pair("state", s);
        }
    }
    Redirect::to(url.as_str()).into_response()
}

async fn authorize(State(state): State<SharedState>, Query(p): Query<AuthorizeParams>) -> Response {
    // Errors in client identity or redirect URI must never redirect (RFC 6749 §4.1.2.1).
    let Some(client_id) = p.client_id.as_deref() else {
        return pages::error(StatusCode::BAD_REQUEST, "Missing client_id.");
    };
    let client = match state.signer.verify_client(client_id) {
        Ok(c) => c,
        Err(_) => return pages::error(StatusCode::BAD_REQUEST, "Unknown client_id."),
    };
    let Some(redirect_uri) = p.redirect_uri.as_deref() else {
        return pages::error(StatusCode::BAD_REQUEST, "Missing redirect_uri.");
    };
    if !client.redirect_uris.iter().any(|u| u == redirect_uri) {
        return pages::error(
            StatusCode::BAD_REQUEST,
            "redirect_uri is not registered for this client.",
        );
    }

    let client_state = p.state.as_deref();
    if p.response_type.as_deref() != Some("code") {
        return redirect_with_error(
            redirect_uri,
            client_state,
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    let Some(code_challenge) = p.code_challenge.as_deref().filter(|c| !c.is_empty()) else {
        return redirect_with_error(
            redirect_uri,
            client_state,
            "invalid_request",
            "PKCE code_challenge is required",
        );
    };
    if p.code_challenge_method.as_deref().unwrap_or("plain") != "S256" {
        return redirect_with_error(
            redirect_uri,
            client_state,
            "invalid_request",
            "only code_challenge_method=S256 is supported",
        );
    }
    if let Some(resource) = p.resource.as_deref()
        && resource.trim_end_matches('/') != state.config.resource_url()
    {
        return redirect_with_error(
            redirect_uri,
            client_state,
            "invalid_target",
            "unknown resource",
        );
    }

    let microsoft_state = jwt::random_id();
    let microsoft_verifier = jwt::random_id() + &jwt::random_id();
    let microsoft_url = state.graph.auth().authorize_url(
        &state.config.microsoft_callback_url(),
        &jwt::pkce_challenge(&microsoft_verifier),
        &microsoft_state,
    );

    state.insert_pending(
        microsoft_state,
        PendingAuthorization {
            kind: PendingKind::SignIn,
            client_id: client_id.to_string(),
            client_redirect_uri: redirect_uri.to_string(),
            client_state: client_state.map(str::to_string),
            code_challenge: code_challenge.to_string(),
            microsoft_verifier,
            created_at: chrono::Utc::now(),
        },
    );

    tracing::info!(client_name = ?client.client_name, "authorization started, redirecting to Microsoft");
    Redirect::to(&microsoft_url).into_response()
}

// ---------------------------------------------------------------------------
// Microsoft callback → allowlist → authorization code for the client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// A failure after Microsoft returned: a sign-in redirects the error to the
/// waiting OAuth client; a connect has no client, so it renders a page.
fn callback_failure(pending: &PendingAuthorization, error: &str, description: &str) -> Response {
    match pending.kind {
        PendingKind::SignIn => redirect_with_error(
            &pending.client_redirect_uri,
            pending.client_state.as_deref(),
            error,
            description,
        ),
        PendingKind::Connect { .. } => {
            let status = if error == "access_denied" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            pages::error(
                status,
                &format!("{description}. Ask your AI client for a new connect link."),
            )
        }
    }
}

fn owned_by_other_page(mailbox: &str) -> Response {
    pages::error(
        StatusCode::FORBIDDEN,
        &format!("{mailbox} is already connected to another pidge user."),
    )
}

async fn callback(State(state): State<SharedState>, Query(p): Query<CallbackParams>) -> Response {
    let Some(pending) = p.state.as_deref().and_then(|s| state.take_pending(s)) else {
        return pages::error(
            StatusCode::BAD_REQUEST,
            "This sign-in link has expired or was already used. Start again from your AI client.",
        );
    };

    if let Some(err) = p.error.as_deref() {
        tracing::warn!(error = err, description = ?p.error_description, "Microsoft sign-in failed");
        return callback_failure(
            &pending,
            "access_denied",
            "Microsoft sign-in was not completed",
        );
    }
    let Some(code) = p.code.as_deref() else {
        return callback_failure(&pending, "server_error", "Microsoft returned no code");
    };

    let success = match state
        .graph
        .auth()
        .exchange_code(
            code,
            &pending.microsoft_verifier,
            &state.config.microsoft_callback_url(),
        )
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "redeeming Microsoft code");
            return callback_failure(
                &pending,
                "server_error",
                "could not complete Microsoft sign-in",
            );
        }
    };

    let me = match state.graph.me(&success.tokens.access_token).await {
        Ok(me) => me,
        Err(e) => {
            tracing::error!(error = %e, "reading signed-in profile");
            return callback_failure(
                &pending,
                "server_error",
                "could not read the signed-in account",
            );
        }
    };
    let email = me
        .mail
        .clone()
        .unwrap_or(me.user_principal_name.clone())
        .to_ascii_lowercase();

    match &pending.kind {
        PendingKind::SignIn => sign_in_complete(&state, &pending, &email, success.tokens).await,
        PendingKind::Connect { owner } => {
            connect_complete(&state, &pending, owner, &email, success.tokens).await
        }
    }
}

/// Binds `mailbox`'s fresh tokens to `owner`, refusing (403 page) if another
/// user owns it. Evicts any cached tokens so the new ones take effect.
/// Returns the response to send on failure, `None` on success.
async fn bind_mailbox(
    state: &SharedState,
    pending: &PendingAuthorization,
    mailbox: &str,
    owner: &str,
    tokens: TokenSet,
) -> Option<Response> {
    match state.users.check_ownership(mailbox, owner).await {
        Ok(()) => {}
        Err(OwnershipError::OwnedByOther) => {
            tracing::warn!(
                user = %user_hash(owner),
                mailbox = %user_hash(mailbox),
                "refused: mailbox owned by another user"
            );
            return Some(owned_by_other_page(mailbox));
        }
        Err(OwnershipError::Store(e)) => {
            tracing::error!(error = %e, "checking mailbox ownership");
            return Some(callback_failure(
                pending,
                "server_error",
                "could not store the mailbox session",
            ));
        }
    }
    let record = MailboxRecord {
        owner: owner.to_string(),
        tokens,
    };
    if let Err(e) = state.users.save_mailbox(&record, mailbox).await {
        tracing::error!(error = %e, "storing mailbox tokens");
        return Some(callback_failure(
            pending,
            "server_error",
            "could not store the mailbox session",
        ));
    }
    state.token_backend.forget(mailbox);
    None
}

async fn sign_in_complete(
    state: &SharedState,
    pending: &PendingAuthorization,
    email: &str,
    tokens: TokenSet,
) -> Response {
    let client_state = pending.client_state.as_deref();

    if !state.config.is_allowed(email) {
        tracing::warn!(user = %user_hash(email), "sign-in refused: not on allowlist");
        return pages::error(
            StatusCode::FORBIDDEN,
            &format!("{email} is not allowed to use this server."),
        );
    }

    if let Some(resp) = bind_mailbox(state, pending, email, email, tokens).await {
        return resp;
    }

    let has_record = match state.users.load(email).await {
        Ok(rec) => rec.is_some(),
        Err(e) => {
            tracing::error!(error = %e, "loading user record");
            return callback_failure(pending, "server_error", "could not load the user profile");
        }
    };
    if !has_record && let Err(e) = state.users.save(&UserRecord::new(email)).await {
        tracing::error!(error = %e, "creating user record");
        return callback_failure(pending, "server_error", "could not create the user profile");
    }

    let code = match state.signer.issue_code(
        email,
        &pending.client_id,
        &pending.client_redirect_uri,
        &pending.code_challenge,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "issuing authorization code");
            return callback_failure(pending, "server_error", "could not issue code");
        }
    };

    tracing::info!(user = %user_hash(email), "sign-in complete, mailbox connected");
    let mut url = Url::parse(&pending.client_redirect_uri).expect("validated at registration");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("code", &code);
        if let Some(s) = client_state {
            q.append_pair("state", s);
        }
    }
    Redirect::to(url.as_str()).into_response()
}

/// A connect link finished: `mailbox` becomes one of `owner`'s mailboxes.
/// The allowlist governs sign-in identities only, so it isn't consulted.
async fn connect_complete(
    state: &SharedState,
    pending: &PendingAuthorization,
    owner: &str,
    mailbox: &str,
    tokens: TokenSet,
) -> Response {
    if let Some(resp) = bind_mailbox(state, pending, mailbox, owner, tokens).await {
        return resp;
    }

    let mut record = match state.users.load(owner).await {
        Ok(Some(rec)) => rec,
        Ok(None) => UserRecord::new(owner),
        Err(e) => {
            tracing::error!(error = %e, "loading user record");
            return callback_failure(pending, "server_error", "could not load the user profile");
        }
    };
    if !record.owns(mailbox) {
        record.mailboxes.push(mailbox.to_string());
    }
    if let Err(e) = state.users.save(&record).await {
        tracing::error!(error = %e, "saving user record");
        return callback_failure(pending, "server_error", "could not save the user profile");
    }

    tracing::info!(
        user = %user_hash(owner),
        mailbox = %user_hash(mailbox),
        "mailbox connected"
    );
    pages::done("Mailbox connected. You can close this tab.")
}

// ---------------------------------------------------------------------------
// Connect link → Microsoft
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ConnectParams {
    state: Option<String>,
}

/// Opens a link minted by `accounts_connect`: looks up its pending entry
/// (without consuming it, so the link can be reopened until it expires) and
/// sends the browser to Microsoft with the same state.
async fn connect(State(state): State<SharedState>, Query(p): Query<ConnectParams>) -> Response {
    let found = p
        .state
        .as_deref()
        .and_then(|s| state.peek_pending(s).map(|pending| (s, pending)))
        .filter(|(_, pending)| matches!(pending.kind, PendingKind::Connect { .. }));
    let Some((microsoft_state, pending)) = found else {
        return pages::error(
            StatusCode::BAD_REQUEST,
            "This connect link has expired or is not valid. Ask your AI client for a new one.",
        );
    };
    let microsoft_url = state.graph.auth().authorize_url(
        &state.config.microsoft_callback_url(),
        &jwt::pkce_challenge(&pending.microsoft_verifier),
        microsoft_state,
    );
    Redirect::to(&microsoft_url).into_response()
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenParams {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    client_id: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

/// Public clients send `client_id` in the body; some libraries put it in a
/// Basic header with an empty secret instead. Accept both.
fn client_id_from(headers: &HeaderMap, form: &TokenParams) -> Option<String> {
    if let Some(id) = form.client_id.as_deref().filter(|s| !s.is_empty()) {
        return Some(id.to_string());
    }
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (id, _secret) = decoded.split_once(':')?;
    let id = percent_encoding_decode(id);
    (!id.is_empty()).then_some(id)
}

fn percent_encoding_decode(s: &str) -> String {
    url::form_urlencoded::parse(s.as_bytes())
        .map(|(k, _)| k.into_owned())
        .next()
        .unwrap_or_default()
}

fn token_response(state: &SharedState, sub: &str, client_id: &str) -> Response {
    let access = state.signer.issue_access(sub, SCOPE);
    let refresh = state.signer.issue_refresh(sub, client_id);
    match (access, refresh) {
        (Ok(access_token), Ok(refresh_token)) => (
            StatusCode::OK,
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::PRAGMA, "no-cache"),
            ],
            Json(json!({
                "access_token": access_token,
                "token_type": "Bearer",
                "expires_in": jwt::ACCESS_TOKEN_TTL.num_seconds(),
                "refresh_token": refresh_token,
                "scope": SCOPE,
            })),
        )
            .into_response(),
        (Err(e), _) | (_, Err(e)) => {
            tracing::error!(error = %e, "issuing tokens");
            oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "could not issue tokens",
            )
        }
    }
}

async fn token(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Form(p): Form<TokenParams>,
) -> Response {
    let Some(client_id) = client_id_from(&headers, &p) else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client_id is required",
        );
    };
    if state.signer.verify_client(&client_id).is_err() {
        return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "unknown client");
    }
    if let Some(resource) = p.resource.as_deref()
        && resource.trim_end_matches('/') != state.config.resource_url()
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "unknown resource",
        );
    }

    match p.grant_type.as_deref() {
        Some("authorization_code") => {
            let (Some(code), Some(verifier)) = (p.code.as_deref(), p.code_verifier.as_deref())
            else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code and code_verifier are required",
                );
            };
            let claims = match state.signer.verify_code(code) {
                Ok(c) => c,
                Err(_) => {
                    return oauth_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_grant",
                        "code is invalid or expired",
                    );
                }
            };
            if claims.client_id != client_id {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "code was issued to another client",
                );
            }
            if let Some(uri) = p.redirect_uri.as_deref()
                && uri != claims.redirect_uri
            {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "redirect_uri mismatch",
                );
            }
            if jwt::pkce_challenge(verifier) != claims.code_challenge {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "PKCE verification failed",
                );
            }
            if !state.mark_code_used(&claims.jti, claims.exp) {
                tracing::warn!(user = %user_hash(&claims.sub), "authorization code replayed");
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "code already used",
                );
            }
            tracing::info!(user = %user_hash(&claims.sub), "issued tokens (authorization_code)");
            token_response(&state, &claims.sub, &client_id)
        }
        Some("refresh_token") => {
            let Some(refresh) = p.refresh_token.as_deref() else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "refresh_token is required",
                );
            };
            let claims = match state.signer.verify_refresh(refresh) {
                Ok(c) => c,
                Err(_) => {
                    return oauth_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_grant",
                        "refresh token is invalid or expired",
                    );
                }
            };
            if claims.client_id != client_id {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "refresh token was issued to another client",
                );
            }
            // The user may have been removed from the allowlist since.
            if !state.config.is_allowed(&claims.sub) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "user is no longer allowed",
                );
            }
            tracing::info!(user = %user_hash(&claims.sub), "issued tokens (refresh_token)");
            token_response(&state, &claims.sub, &client_id)
        }
        _ => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "use authorization_code or refresh_token",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_policy() {
        assert!(redirect_uri_is_acceptable(
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(redirect_uri_is_acceptable(
            "http://localhost:6274/oauth/callback"
        ));
        assert!(redirect_uri_is_acceptable("http://127.0.0.1:8123/cb"));
        assert!(!redirect_uri_is_acceptable("http://evil.example/cb"));
        assert!(!redirect_uri_is_acceptable("https://claude.ai/cb#frag"));
        assert!(!redirect_uri_is_acceptable("myapp://callback"));
        assert!(!redirect_uri_is_acceptable("not a url"));
    }

    #[test]
    fn client_id_from_basic_header() {
        let mut headers = HeaderMap::new();
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "abc%2Bdef:");
        headers.insert(
            header::AUTHORIZATION,
            format!("Basic {encoded}").parse().unwrap(),
        );
        let form = TokenParams {
            grant_type: None,
            code: None,
            redirect_uri: None,
            code_verifier: None,
            client_id: None,
            refresh_token: None,
            resource: None,
        };
        assert_eq!(client_id_from(&headers, &form).as_deref(), Some("abc+def"));
    }
}
