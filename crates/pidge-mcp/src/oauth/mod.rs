//! The server's own OAuth 2.1 authorization server.
//!
//! MCP clients (Claude, ChatGPT, Claude Code, MCP Inspector, …) discover it
//! through RFC 9728 protected-resource metadata, register themselves with
//! RFC 7591 dynamic client registration, and run an authorization-code +
//! PKCE flow. The *user* authenticates at Microsoft: `/authorize` shows a
//! consent page naming the client and where it will be sent, its Continue
//! (`/authorize/go`) bounces to Microsoft login, and `/callback` verifies the
//! returned account against the allowlist and, because the very same sign-in
//! grants Graph access to that account's mailbox, stores its refresh token —
//! sign-in and mailbox connection are one step.
//!
//! An account is identified by its `userPrincipalName` (never the editable
//! `mail` attribute) and pinned to the immutable tenant and object id from
//! the ID token the first time it is seen.
//!
//! No database: clients, codes and tokens are all signed JWTs (see [`jwt`]).
//! The only in-memory state is the minutes-long window between `/authorize`
//! and `/callback`, and the set of already-redeemed codes.

pub mod bearer;
#[cfg(test)]
mod flow_tests;
pub mod jwt;
pub(crate) mod pages;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use pidge_client::auth::TokenSet;

use crate::state::{PENDING_TTL, PendingAuthorization, PendingKind, SharedState};
use crate::users::{
    Identity, MailboxRecord, OwnershipError, UserRecord, log_store_error, user_hash,
};

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
        .route("/authorize/go", get(authorize_go))
        .route("/callback", get(callback))
        .route("/connect", get(connect))
        .route("/connect/go", get(connect_go))
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

    // Anyone can register a client with any https redirect and hand a user
    // this link, so before going to Microsoft (which would show only an
    // account picker) the user is told which client and host will receive
    // the sign-in. The Continue step is bound to this browser by a cookie.
    let microsoft_state = jwt::random_id();
    let nonce = jwt::random_id();
    state.insert_pending(
        microsoft_state.clone(),
        PendingAuthorization {
            kind: PendingKind::SignIn,
            client_id: client_id.to_string(),
            client_redirect_uri: redirect_uri.to_string(),
            client_state: client_state.map(str::to_string),
            code_challenge: code_challenge.to_string(),
            microsoft_verifier: jwt::random_id() + &jwt::random_id(),
            consent_nonce: Some(nonce.clone()),
            created_at: chrono::Utc::now(),
        },
    );

    let redirect_host = Url::parse(redirect_uri)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let go = format!(
        "{}/authorize/go?state={}",
        state.config.base_url(),
        urlencode(&microsoft_state)
    );
    tracing::info!(client_name = ?client.client_name, "authorization started, asking for consent");
    let resp = pages::confirm_sign_in(client.client_name.as_deref(), &redirect_host, &go);
    with_consent_cookie(resp, &SIGN_IN_CONSENT, &nonce, &state)
}

/// The sign-in consent page's Continue: sends the browser to Microsoft with
/// the pending sign-in's state, but only from the browser shown the page.
async fn authorize_go(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(p): Query<ConnectParams>,
) -> Response {
    let Some((key, pending)) = p
        .state
        .as_deref()
        .and_then(|key| Some((key, state.peek_pending(key)?)))
        .filter(|(_, pending)| pending.kind == PendingKind::SignIn)
    else {
        return pages::error(
            StatusCode::BAD_REQUEST,
            "This sign-in link has expired or is not valid. Start again from your AI client.",
        );
    };
    let presented = consent_nonce_from(&headers, &SIGN_IN_CONSENT);
    if pending.consent_nonce.is_none() || presented != pending.consent_nonce {
        return pages::error(
            StatusCode::BAD_REQUEST,
            "Start the sign-in from your AI client and confirm it on the pidge page that opens.",
        );
    }
    let microsoft_url = state.graph.auth().authorize_url(
        &state.config.microsoft_callback_url(),
        &jwt::pkce_challenge(&pending.microsoft_verifier),
        key,
    );
    Redirect::to(&microsoft_url).into_response()
}

// ---------------------------------------------------------------------------
// Microsoft callback → allowlist → authorization code for the client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    /// Microsoft's error code. Its `error_description` is deliberately not
    /// read: free text that would otherwise reach the log.
    error: Option<String>,
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

/// The one answer for a Microsoft account that can't be tied to a stable
/// identity, or whose identity differs from the one pinned for its address.
fn unverified_account() -> Response {
    pages::error(
        StatusCode::FORBIDDEN,
        "could not verify the Microsoft account",
    )
}

/// Logs a failed call to Microsoft with only the HTTP status (when there is
/// one): the error's text can echo Microsoft's response body.
fn log_microsoft_failure(what: &str, e: &pidge_client::ClientError) {
    match e {
        pidge_client::ClientError::Graph { status, .. } => {
            tracing::error!(status, "{what} failed");
        }
        _ => tracing::error!("{what} failed"),
    }
}

/// Microsoft's `error` code as logged: its word characters only, capped, so
/// a crafted callback can't inject free text into the log.
fn error_code(err: &str) -> String {
    err.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
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
        // Only the code: `error_description` is free text from the URL.
        tracing::warn!(error = %error_code(err), "Microsoft sign-in failed");
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
            log_microsoft_failure("redeeming Microsoft code", &e);
            return callback_failure(
                &pending,
                "server_error",
                "could not complete Microsoft sign-in",
            );
        }
    };

    // The account's immutable identity, from the ID token Microsoft just
    // returned over TLS. Without it the account can't be pinned: refuse.
    let Some(claims) = success
        .id_token
        .as_deref()
        .and_then(pidge_client::auth::extract_id_claims)
    else {
        tracing::warn!("sign-in refused: no tenant and object id in the ID token");
        return unverified_account();
    };
    let identity = Identity {
        tid: claims.tid,
        oid: claims.oid,
    };

    let me = match state.graph.me(&success.tokens.access_token).await {
        Ok(me) => me,
        Err(e) => {
            log_microsoft_failure("reading signed-in profile", &e);
            return callback_failure(
                &pending,
                "server_error",
                "could not read the signed-in account",
            );
        }
    };
    // Never `mail`: a tenant admin can set it to any address, including an
    // allowlisted one. The principal name is the sign-in name itself.
    let email = me.user_principal_name.to_ascii_lowercase();

    match &pending.kind {
        PendingKind::SignIn => {
            sign_in_complete(&state, &pending, &email, &identity, success.tokens).await
        }
        PendingKind::Connect { owner } => {
            connect_complete(&state, &pending, owner, &email, &identity, success.tokens).await
        }
    }
}

/// Binds `mailbox`'s fresh tokens to `owner`, refusing (403 page) if another
/// user owns it or its record is pinned to a different Microsoft account;
/// pins `identity` otherwise. Evicts any cached tokens so the new ones take
/// effect. Returns the response to send on failure, `None` on success.
async fn bind_mailbox(
    state: &SharedState,
    pending: &PendingAuthorization,
    mailbox: &str,
    owner: &str,
    identity: &Identity,
    tokens: TokenSet,
) -> Option<Response> {
    match state
        .users
        .check_binding(mailbox, owner, Some(identity))
        .await
    {
        Ok(()) => {}
        Err(OwnershipError::IdentityMismatch) => {
            tracing::warn!(
                user = %user_hash(owner),
                mailbox = %user_hash(mailbox),
                "refused: mailbox is pinned to a different Microsoft account"
            );
            return Some(unverified_account());
        }
        Err(OwnershipError::OwnedByOther) => {
            tracing::warn!(
                user = %user_hash(owner),
                mailbox = %user_hash(mailbox),
                "refused: mailbox owned by another user"
            );
            return Some(owned_by_other_page(mailbox));
        }
        Err(OwnershipError::Store(e)) => {
            log_store_error("checking mailbox ownership", mailbox, &e);
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
        identity: Some(identity.clone()),
    };
    if let Err(e) = state.users.save_mailbox(&record, mailbox).await {
        log_store_error("storing mailbox tokens", mailbox, &e);
        return Some(callback_failure(
            pending,
            "server_error",
            "could not store the mailbox session",
        ));
    }
    state.token_backend.forget(mailbox);
    state.cache.invalidate_user(owner);
    None
}

/// A sign-in finished at Microsoft as `email` (its principal name) with the
/// immutable `identity`. The address must be allowlisted; a user record
/// pinned to another identity refuses (403, nothing stored); an unpinned
/// one (from before pinning) is pinned now.
async fn sign_in_complete(
    state: &SharedState,
    pending: &PendingAuthorization,
    email: &str,
    identity: &Identity,
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

    let existing = match state.users.load(email).await {
        Ok(rec) => rec,
        Err(e) => {
            log_store_error("loading user record", email, &e);
            return callback_failure(pending, "server_error", "could not load the user profile");
        }
    };
    if let Some(pinned) = existing.as_ref().and_then(|r| r.identity.as_ref())
        && pinned != identity
    {
        tracing::warn!(
            user = %user_hash(email),
            "sign-in refused: a different Microsoft account than the one pinned"
        );
        return unverified_account();
    }

    if let Some(resp) = bind_mailbox(state, pending, email, email, identity, tokens).await {
        return resp;
    }

    if existing.as_ref().is_none_or(|r| r.identity.is_none()) {
        let mut record = existing.unwrap_or_else(|| UserRecord::new(email));
        record.identity = Some(identity.clone());
        if let Err(e) = state.users.save(&record).await {
            log_store_error("saving user record", email, &e);
            return callback_failure(pending, "server_error", "could not create the user profile");
        }
        state.cache.invalidate_user(email);
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
/// The allowlist governs sign-in identities, so a connected mailbox needn't
/// be on it; but an identity that *is* on it belongs to its own pidge user
/// and is never bound to someone else. Stores nothing when refusing.
async fn connect_complete(
    state: &SharedState,
    pending: &PendingAuthorization,
    owner: &str,
    mailbox: &str,
    identity: &Identity,
    tokens: TokenSet,
) -> Response {
    if !state.config.is_allowed(owner) {
        tracing::warn!(user = %user_hash(owner), "connect refused: owner no longer allowed");
        return pages::error(
            StatusCode::FORBIDDEN,
            "The pidge account this link belongs to is no longer allowed to use this server.",
        );
    }
    if state.config.is_allowed(mailbox) && !mailbox.eq_ignore_ascii_case(owner) {
        tracing::warn!(
            user = %user_hash(owner),
            mailbox = %user_hash(mailbox),
            "connect refused: mailbox is another user's sign-in identity"
        );
        return pages::error(
            StatusCode::FORBIDDEN,
            &format!(
                "{mailbox} signs in to pidge as its own user and cannot be connected to another account."
            ),
        );
    }

    if let Some(resp) = bind_mailbox(state, pending, mailbox, owner, identity, tokens).await {
        return resp;
    }

    let mut record = match state.users.load(owner).await {
        Ok(Some(rec)) => rec,
        Ok(None) => UserRecord::new(owner),
        Err(e) => {
            log_store_error("loading user record", owner, &e);
            return callback_failure(pending, "server_error", "could not load the user profile");
        }
    };
    if !record.owns(mailbox) {
        record.mailboxes.push(mailbox.to_string());
    }
    if let Err(e) = state.users.save(&record).await {
        log_store_error("saving user record", owner, &e);
        return callback_failure(pending, "server_error", "could not save the user profile");
    }
    state.cache.invalidate_user(owner);

    tracing::info!(
        user = %user_hash(owner),
        mailbox = %user_hash(mailbox),
        "mailbox connected"
    );
    pages::done(&format!(
        "Mailbox connected to {owner}. You can close this tab."
    ))
}

// ---------------------------------------------------------------------------
// Connect link → confirmation → Microsoft
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ConnectParams {
    state: Option<String>,
}

/// The live `Connect` entry for a link's state, with its owner; `None` if
/// the state is missing, unknown, expired or belongs to a sign-in.
/// Non-consuming, so the link works until the callback uses it.
fn connect_pending(
    state: &SharedState,
    p: &ConnectParams,
) -> Option<(String, String, PendingAuthorization)> {
    let key = p.state.as_deref()?;
    let pending = state.peek_pending(key)?;
    match &pending.kind {
        PendingKind::Connect { owner } => Some((key.to_string(), owner.clone(), pending)),
        PendingKind::SignIn => None,
    }
}

fn expired_connect_link() -> Response {
    pages::error(
        StatusCode::BAD_REQUEST,
        "This connect link has expired or is not valid. Ask your AI client for a new one.",
    )
}

/// A cookie carrying a confirmation page's nonce to its Continue step,
/// scoped to that flow's path.
struct ConsentCookie {
    name: &'static str,
    path: &'static str,
}

/// For a connect link: `/connect` → `/connect/go`.
const CONNECT_CONSENT: ConsentCookie = ConsentCookie {
    name: "pidge_connect",
    path: "/connect",
};

/// For a client's sign-in: `/authorize` → `/authorize/go`.
const SIGN_IN_CONSENT: ConsentCookie = ConsentCookie {
    name: "pidge_authorize",
    path: "/authorize",
};

/// `Set-Cookie` value for a consent nonce: scoped to the flow's path, gone
/// with the pending entry's lifetime, `Secure` whenever we're served over https.
fn consent_cookie(cookie: &ConsentCookie, nonce: &str, secure: bool) -> String {
    format!(
        "{}={nonce}; HttpOnly;{} SameSite=Lax; Max-Age={}; Path={}",
        cookie.name,
        if secure { " Secure;" } else { "" },
        PENDING_TTL.num_seconds(),
        cookie.path
    )
}

/// `resp` with the consent cookie set.
fn with_consent_cookie(
    mut resp: Response,
    cookie: &ConsentCookie,
    nonce: &str,
    state: &SharedState,
) -> Response {
    let secure = state.config.public_url.scheme() == "https";
    resp.headers_mut().insert(
        header::SET_COOKIE,
        consent_cookie(cookie, nonce, secure)
            .parse()
            .expect("cookie is ASCII"),
    );
    resp
}

/// The consent nonce from the request's `Cookie` header(s), if any.
fn consent_nonce_from(headers: &HeaderMap, cookie: &ConsentCookie) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == cookie.name)
        .map(|(_, value)| value.to_string())
}

/// Opens a link minted by `accounts_connect`. Shows which pidge account the
/// mailbox will be bound to before anything happens, so someone handed a
/// stranger's link doesn't connect their mailbox to it unawares, and binds
/// the Continue step to this browser with a cookie nonce.
async fn connect(State(state): State<SharedState>, Query(p): Query<ConnectParams>) -> Response {
    let Some((key, owner, _)) = connect_pending(&state, &p) else {
        return expired_connect_link();
    };
    let nonce = jwt::random_id();
    state.set_consent_nonce(&key, nonce.clone());
    let go = format!(
        "{}/connect/go?state={}",
        state.config.base_url(),
        urlencode(&key)
    );
    with_consent_cookie(
        pages::confirm_connect(&owner, &go),
        &CONNECT_CONSENT,
        &nonce,
        &state,
    )
}

/// The confirmation's Continue: sends the browser to Microsoft with the
/// link's state, but only from the browser that was shown the confirmation.
async fn connect_go(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(p): Query<ConnectParams>,
) -> Response {
    let Some((key, _, pending)) = connect_pending(&state, &p) else {
        return expired_connect_link();
    };
    let presented = consent_nonce_from(&headers, &CONNECT_CONSENT);
    if pending.consent_nonce.is_none() || presented != pending.consent_nonce {
        return pages::error(
            StatusCode::BAD_REQUEST,
            "Open the connect link itself (not this page's address) and confirm the account there.",
        );
    }
    let microsoft_url = state.graph.auth().authorize_url(
        &state.config.microsoft_callback_url(),
        &jwt::pkce_challenge(&pending.microsoft_verifier),
        &key,
    );
    Redirect::to(&microsoft_url).into_response()
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
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

/// Issues an access/refresh pair stamped with the user's current token `generation`.
fn token_response(state: &SharedState, sub: &str, client_id: &str, generation: u32) -> Response {
    let access = state.signer.issue_access(sub, SCOPE, generation);
    let refresh = state.signer.issue_refresh(sub, client_id, generation);
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
            let generation = state.generation_for(&claims.sub).await;
            token_response(&state, &claims.sub, &client_id, generation)
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
            // `accounts_update sign_out_everywhere` bumped the generation.
            let generation = state.generation_for(&claims.sub).await;
            if claims.r#gen != generation {
                tracing::info!(user = %user_hash(&claims.sub), "refused signed-out refresh token");
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "session was signed out",
                );
            }
            tracing::info!(user = %user_hash(&claims.sub), "issued tokens (refresh_token)");
            token_response(&state, &claims.sub, &client_id, generation)
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
    fn consent_cookie_attributes_and_parsing() {
        let c = consent_cookie(&CONNECT_CONSENT, "abc_-1", false);
        assert_eq!(
            c,
            "pidge_connect=abc_-1; HttpOnly; SameSite=Lax; Max-Age=600; Path=/connect"
        );
        assert!(consent_cookie(&CONNECT_CONSENT, "abc", true).contains("; Secure;"));
        assert_eq!(
            consent_cookie(&SIGN_IN_CONSENT, "n", true),
            "pidge_authorize=n; HttpOnly; Secure; SameSite=Lax; Max-Age=600; Path=/authorize"
        );

        let mut headers = HeaderMap::new();
        assert_eq!(consent_nonce_from(&headers, &CONNECT_CONSENT), None);
        headers.append(
            header::COOKIE,
            "other=1; pidge_connect=n1; pidge_authorize=s1"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            consent_nonce_from(&headers, &CONNECT_CONSENT).as_deref(),
            Some("n1")
        );
        assert_eq!(
            consent_nonce_from(&headers, &SIGN_IN_CONSENT).as_deref(),
            Some("s1")
        );
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, "a=b".parse().unwrap());
        headers.append(header::COOKIE, "pidge_connect=n2".parse().unwrap());
        assert_eq!(
            consent_nonce_from(&headers, &CONNECT_CONSENT).as_deref(),
            Some("n2")
        );
    }

    #[test]
    fn logged_error_codes_are_word_characters_only() {
        assert_eq!(error_code("access_denied"), "access_denied");
        assert_eq!(
            error_code("x\nuser jane@example.com logged in"),
            "xuserjaneexamplecomloggedin"
        );
        assert_eq!(error_code(&"a".repeat(100)).len(), 64);
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
