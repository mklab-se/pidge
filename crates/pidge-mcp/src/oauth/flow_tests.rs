//! End-to-end tests of the authorization server with Microsoft mocked:
//! register → authorize (consent page) → authorize/go → callback → token →
//! MCP request.

use std::collections::HashSet;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use pidge_client::{AuthClient, GraphClient};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::app::build_router;
use crate::config::{Config, SecretsBackend};
use crate::mailbox::SecretTokenBackend;
use crate::oauth::jwt::{Signer, pkce_challenge, random_bytes};
use crate::secrets::{FileSecrets, SharedSecrets, mailbox_secret_name};
use crate::state::{AppState, PendingAuthorization, PendingKind, SharedState};
use crate::users::{Identity, UserRecord, UserStore};

const PUBLIC: &str = "http://localhost:8080";
const CLIENT_REDIRECT: &str = "http://localhost:9999/cb";

struct Harness {
    app: Router,
    microsoft: MockServer,
    state: SharedState,
    secrets: SharedSecrets,
    secrets_dir: tempfile::TempDir,
    /// The key `state.signer` signs with, for hand-built tokens.
    key: Vec<u8>,
}

const TENANT: &str = "tenant-1";

/// The object id the mocked Microsoft gives the account `upn`.
fn oid_for(upn: &str) -> String {
    format!("oid-{}", upn.to_ascii_lowercase())
}

fn identity_for(upn: &str) -> Identity {
    Identity {
        tid: TENANT.into(),
        oid: oid_for(upn),
    }
}

/// An unsigned ID token carrying `claims`, shaped like Microsoft's.
fn id_token(claims: serde_json::Value) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        URL_SAFE_NO_PAD.encode("dummy-signature")
    )
}

/// Microsoft signs in as `signed_in_email` (its principal name and `mail`).
async fn harness(signed_in_email: &str) -> Harness {
    harness_with(signed_in_email, signed_in_email, &oid_for(signed_in_email)).await
}

/// Microsoft signs in as the account with principal name `upn`, profile
/// `mail` attribute `mail`, and ID-token object id `oid`.
async fn harness_with(upn: &str, mail: &str, oid: &str) -> Harness {
    let microsoft = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "MS_AT", "refresh_token": "MS_RT", "expires_in": 3600,
            "id_token": id_token(serde_json::json!({ "tid": TENANT, "oid": oid })),
        })))
        .mount(&microsoft)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "u1", "userPrincipalName": upn, "mail": mail
        })))
        .mount(&microsoft)
        .await;

    let secrets_dir = tempfile::tempdir().unwrap();
    let secrets: SharedSecrets = Arc::new(FileSecrets::new(secrets_dir.path()).unwrap());
    let config = Config {
        port: 8080,
        public_url: Url::parse(PUBLIC).unwrap(),
        allowed_emails: HashSet::from([
            "jane@example.com".to_string(),
            "anna@example.com".to_string(),
        ]),
        secrets: SecretsBackend::File {
            dir: secrets_dir.path().to_path_buf(),
        },
        markitdown: "markitdown".into(),
        alt_hosts: Vec::new(),
        legacy_issuers: Vec::new(),
    };
    let key = random_bytes(32);
    let signer = Signer::new(&key, PUBLIC, format!("{PUBLIC}/mcp"));
    let token_backend = Arc::new(SecretTokenBackend::new(secrets.clone()));
    let auth = AuthClient::for_test("cid", microsoft.uri()).with_backend(token_backend.clone());
    let graph = GraphClient::for_test(auth, format!("{}/v1.0", microsoft.uri()));
    let state = Arc::new(AppState::new(
        config,
        signer,
        graph,
        token_backend,
        secrets.clone(),
    ));
    Harness {
        app: build_router(state.clone(), CancellationToken::new()),
        microsoft,
        state,
        secrets,
        secrets_dir,
        key,
    }
}

/// Like [`harness`], but the router's `Host` allowlist also accepts
/// `alt_hosts` (see `PIDGE_MCP_ALT_HOSTS`). No sign-in flow is needed for
/// this harness's tests, so access tokens are minted directly from
/// `h.state.signer`.
async fn harness_with_alt_hosts(alt_hosts: Vec<String>) -> Harness {
    let microsoft = MockServer::start().await;
    let secrets_dir = tempfile::tempdir().unwrap();
    let secrets: SharedSecrets = Arc::new(FileSecrets::new(secrets_dir.path()).unwrap());
    let config = Config {
        port: 8080,
        public_url: Url::parse(PUBLIC).unwrap(),
        allowed_emails: HashSet::from(["jane@example.com".to_string()]),
        secrets: SecretsBackend::File {
            dir: secrets_dir.path().to_path_buf(),
        },
        markitdown: "markitdown".into(),
        alt_hosts,
        legacy_issuers: Vec::new(),
    };
    let key = random_bytes(32);
    let signer = Signer::new(&key, PUBLIC, format!("{PUBLIC}/mcp"));
    let token_backend = Arc::new(SecretTokenBackend::new(secrets.clone()));
    let auth = AuthClient::for_test("cid", microsoft.uri()).with_backend(token_backend.clone());
    let graph = GraphClient::for_test(auth, format!("{}/v1.0", microsoft.uri()));
    let state = Arc::new(AppState::new(
        config,
        signer,
        graph,
        token_backend,
        secrets.clone(),
    ));
    Harness {
        app: build_router(state.clone(), CancellationToken::new()),
        microsoft,
        state,
        secrets,
        secrets_dir,
        key,
    }
}

/// A pending entry with no OAuth client attached, as `accounts_connect` makes.
fn pending_stub() -> PendingAuthorization {
    PendingAuthorization {
        kind: PendingKind::SignIn,
        client_id: String::new(),
        client_redirect_uri: String::new(),
        client_state: None,
        code_challenge: String::new(),
        microsoft_verifier: "stub-verifier".into(),
        consent_nonce: None,
        created_at: chrono::Utc::now(),
    }
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn query(url: &str, key: &str) -> Option<String> {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

async fn register(app: &Router) -> String {
    register_named(app, "test").await
}

/// Registration is deterministic for identical metadata (same signed claims),
/// so a distinct client needs distinct metadata.
async fn register_named(app: &Router, name: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"redirect_uris":[CLIENT_REDIRECT],"client_name":name})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    body_json(resp).await["client_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn authorize_uri(client_id: &str, verifier: &str) -> String {
    format!(
        "/authorize?response_type=code&client_id={}&redirect_uri={}&state=client-state&code_challenge={}&code_challenge_method=S256",
        urlenc(client_id),
        urlenc(CLIENT_REDIRECT),
        pkce_challenge(verifier)
    )
}

/// Opens `/authorize` and returns the consent page's Continue path and the
/// cookie it set (`name=value`).
async fn consent(h: &Harness, client_id: &str, verifier: &str) -> (String, String) {
    let resp = get(h, &authorize_uri(client_id, verifier)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::LOCATION).is_none());
    let cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let page = body_text(resp).await;
    let at = page.find("/authorize/go?state=").expect("Continue link");
    let go = page[at..].split('"').next().unwrap().to_string();
    (go, cookie)
}

/// Runs authorize → consent → authorize/go → callback and returns the
/// callback's response (a redirect carrying the client's code on success).
async fn sign_in(h: &Harness, client_id: &str, verifier: &str) -> axum::response::Response {
    let (go, cookie) = consent(h, client_id, verifier).await;
    let resp = get_with_cookie(h, &go, &cookie).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&h.microsoft.uri()),
        "redirects to Microsoft"
    );
    assert_eq!(
        query(&location, "redirect_uri").unwrap(),
        format!("{PUBLIC}/callback")
    );
    let ms_state = query(&location, "state").unwrap();

    h.app
        .clone()
        .oneshot(
            Request::get(format!("/callback?code=ms-code&state={ms_state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn redeem(app: &Router, form: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/token")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn mcp_initialize(app: &Router, bearer: Option<&str>) -> StatusCode {
    mcp_initialize_with_host(app, bearer, "localhost:8080").await
}

async fn mcp_initialize_with_host(app: &Router, bearer: Option<&str>, host: &str) -> StatusCode {
    let mut req = Request::post("/mcp")
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(b) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}});
    app.clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn full_flow_for_allowed_user() {
    let h = harness("Jane@Example.com").await;
    let client_id = register(&h.app).await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

    let resp = sign_in(&h, &client_id, verifier).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with(CLIENT_REDIRECT));
    assert_eq!(query(&location, "state").as_deref(), Some("client-state"));
    let code = query(&location, "code").expect("code in redirect");

    // The sign-in stored the mailbox refresh token under the lower-cased address.
    let stored = std::fs::read_to_string(
        h.secrets_dir
            .path()
            .join(mailbox_secret_name("jane@example.com")),
    )
    .unwrap();
    assert!(stored.contains("MS_RT"));
    let users = UserStore::new(h.secrets.clone());
    let mailbox = users
        .load_mailbox("jane@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mailbox.owner, "jane@example.com");
    assert_eq!(mailbox.identity, Some(identity_for("jane@example.com")));
    let rec = users.load("jane@example.com").await.unwrap().unwrap();
    assert_eq!(
        rec,
        UserRecord {
            identity: Some(identity_for("jane@example.com")),
            ..UserRecord::new("jane@example.com")
        }
    );

    // Wrong verifier fails, right verifier succeeds, replay fails.
    let (status, body) = redeem(
        &h.app,
        &format!("grant_type=authorization_code&client_id={}&code={}&code_verifier=wrong&redirect_uri={}", urlenc(&client_id), urlenc(&code), urlenc(CLIENT_REDIRECT)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");

    let form = format!(
        "grant_type=authorization_code&client_id={}&code={}&code_verifier={verifier}&redirect_uri={}",
        urlenc(&client_id),
        urlenc(&code),
        urlenc(CLIENT_REDIRECT)
    );
    let (status, body) = redeem(&h.app, &form).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let access = body["access_token"].as_str().unwrap().to_string();
    let refresh = body["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(body["token_type"], "Bearer");

    let (status, body) = redeem(&h.app, &form).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error_description"], "code already used");

    // The access token opens the MCP endpoint; garbage and nothing do not.
    assert_eq!(mcp_initialize(&h.app, Some(&access)).await, StatusCode::OK);
    assert_eq!(
        mcp_initialize(&h.app, Some("garbage")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(mcp_initialize(&h.app, None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        mcp_initialize(&h.app, Some(&refresh)).await,
        StatusCode::UNAUTHORIZED,
        "refresh token is not a bearer"
    );

    // Refresh grant issues a new pair; another client's id is rejected.
    let (status, body) = redeem(
        &h.app,
        &format!(
            "grant_type=refresh_token&client_id={}&refresh_token={}",
            urlenc(&client_id),
            urlenc(&refresh)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["access_token"].as_str().is_some());

    let other_client = register_named(&h.app, "other").await;
    assert_ne!(other_client, client_id);
    let (status, body) = redeem(
        &h.app,
        &format!(
            "grant_type=refresh_token&client_id={}&refresh_token={}",
            urlenc(&other_client),
            urlenc(&refresh)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn alternate_hosts_are_accepted() {
    let h = harness_with_alt_hosts(vec!["alt.test".into()]).await;
    let access = h
        .state
        .signer
        .issue_access("jane@example.com", "mail", 0)
        .unwrap();

    assert_eq!(
        mcp_initialize_with_host(&h.app, Some(&access), "alt.test").await,
        StatusCode::OK,
        "an alt host is accepted"
    );
    let blocked = mcp_initialize_with_host(&h.app, Some(&access), "evil.test").await;
    assert!(
        blocked.is_client_error(),
        "a host outside the allowlist is rejected: {blocked}"
    );
}

#[tokio::test]
async fn user_not_on_allowlist_is_refused_and_nothing_is_stored() {
    let h = harness("mallory@example.com").await;
    let client_id = register(&h.app).await;
    let resp = sign_in(
        &h,
        &client_id,
        "verifier-verifier-verifier-verifier-verifier",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        !h.secrets_dir
            .path()
            .join(mailbox_secret_name("mallory@example.com"))
            .exists()
    );
}

#[tokio::test]
async fn microsoft_callback_state_is_single_use() {
    let h = harness("jane@example.com").await;
    let client_id = register(&h.app).await;
    let resp = sign_in(
        &h,
        &client_id,
        "verifier-verifier-verifier-verifier-verifier",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::get("/callback?code=x&state=unknown")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unregistered_redirect_uri_never_redirects() {
    let h = harness("jane@example.com").await;
    let client_id = register(&h.app).await;
    let uri = format!(
        "/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge=abc&code_challenge_method=S256",
        urlenc(&client_id),
        urlenc("http://localhost:9999/other")
    );
    let resp = h
        .app
        .clone()
        .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sign_in_creates_user_record_with_owner_stamp() {
    let h = harness("jane@example.com").await;
    let client_id = register(&h.app).await;
    let resp = sign_in(
        &h,
        &client_id,
        "verifier-verifier-verifier-verifier-verifier",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let users = UserStore::new(h.secrets.clone());
    let rec = users.load("jane@example.com").await.unwrap().unwrap();
    assert_eq!(rec.mailboxes, vec!["jane@example.com"]);
    assert_eq!(
        users
            .load_mailbox("jane@example.com")
            .await
            .unwrap()
            .unwrap()
            .owner,
        "jane@example.com"
    );
}

#[tokio::test]
async fn sign_in_keeps_an_existing_user_record() {
    let h = harness("jane@example.com").await;
    let users = UserStore::new(h.secrets.clone());
    let mut rec = UserRecord::new("jane@example.com");
    rec.mailboxes.push("second@example.com".into());
    rec.timezone = "Europe/London".into();
    rec.identity = Some(identity_for("jane@example.com"));
    users.save(&rec).await.unwrap();
    let client_id = register(&h.app).await;
    let resp = sign_in(
        &h,
        &client_id,
        "verifier-verifier-verifier-verifier-verifier",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(users.load("jane@example.com").await.unwrap().unwrap(), rec);
}

#[tokio::test]
async fn sign_in_refuses_a_mailbox_another_user_connected() {
    let h = harness("jane@example.com").await;
    let users = UserStore::new(h.secrets.clone());
    users
        .save_mailbox(
            &crate::users::MailboxRecord {
                owner: "mallory@example.com".into(),
                tokens: pidge_client::auth::TokenSet {
                    access_token: "a".into(),
                    refresh_token: "MALLORY_RT".into(),
                    expires_at: chrono::Utc::now(),
                },
                identity: None,
            },
            "jane@example.com",
        )
        .await
        .unwrap();
    let client_id = register(&h.app).await;
    let resp = sign_in(
        &h,
        &client_id,
        "verifier-verifier-verifier-verifier-verifier",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let mailbox = users
        .load_mailbox("jane@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mailbox.owner, "mallory@example.com", "record untouched");
    assert!(users.load("jane@example.com").await.unwrap().is_none());
}

#[tokio::test]
async fn connect_invalidates_the_owners_read_cache() {
    let h = harness("second@example.com").await; // Microsoft mock signs in as second@…
    h.state.insert_pending(
        "s1".into(),
        PendingAuthorization {
            kind: PendingKind::Connect {
                owner: "jane@example.com".into(),
            },
            ..pending_stub()
        },
    );
    UserStore::new(h.secrets.clone())
        .save(&UserRecord::new("jane@example.com"))
        .await
        .unwrap();
    h.state
        .cache
        .put("jane@example.com", "k".into(), "v".into());

    let resp = h
        .app
        .clone()
        .oneshot(
            Request::get("/callback?code=x&state=s1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(h.state.cache.get("jane@example.com", "k").is_none());
}

#[tokio::test]
async fn connect_binds_second_mailbox_to_owner_and_refuses_foreign_ownership() {
    let h = harness("second@example.com").await; // Microsoft mock signs in as second@…
    let state = h.state.clone();
    state.insert_pending(
        "s1".into(),
        PendingAuthorization {
            kind: PendingKind::Connect {
                owner: "jane@example.com".into(),
            },
            ..pending_stub()
        },
    );
    UserStore::new(h.secrets.clone())
        .save(&UserRecord::new("jane@example.com"))
        .await
        .unwrap();
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::get("/callback?code=x&state=s1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let page = body_text(resp).await;
    assert!(
        page.contains("Mailbox connected to jane@example.com"),
        "{page}"
    );
    let rec = UserStore::new(h.secrets.clone())
        .load("jane@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rec.mailboxes,
        vec!["jane@example.com", "second@example.com"]
    );
    assert_eq!(
        UserStore::new(h.secrets.clone())
            .load_mailbox("second@example.com")
            .await
            .unwrap()
            .unwrap()
            .owner,
        "jane@example.com"
    );
    // Anna, another allowed user, tries to connect the same mailbox.
    UserStore::new(h.secrets.clone())
        .save(&UserRecord::new("anna@example.com"))
        .await
        .unwrap();
    state.insert_pending(
        "s2".into(),
        PendingAuthorization {
            kind: PendingKind::Connect {
                owner: "anna@example.com".into(),
            },
            ..pending_stub()
        },
    );
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::get("/callback?code=x&state=s2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let anna = UserStore::new(h.secrets.clone())
        .load("anna@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(anna.mailboxes, vec!["anna@example.com"]);
}

fn connect_pending(owner: &str) -> PendingAuthorization {
    PendingAuthorization {
        kind: PendingKind::Connect {
            owner: owner.into(),
        },
        ..pending_stub()
    }
}

async fn get(h: &Harness, uri: &str) -> axum::response::Response {
    h.app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn get_with_cookie(h: &Harness, uri: &str, cookie: &str) -> axum::response::Response {
    h.app
        .clone()
        .oneshot(
            Request::get(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn connect_link_confirms_the_owner_then_goes_to_microsoft() {
    let h = harness("second@example.com").await;
    h.state
        .insert_pending("s1".into(), connect_pending("jane@example.com"));

    // The link first shows whose pidge account the mailbox will join.
    let resp = get(&h, "/connect?state=s1").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::LOCATION).is_none());
    let set_cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/connect"), "{set_cookie}");
    let cookie = set_cookie.split(';').next().unwrap().to_string(); // "pidge_connect=<nonce>"
    let page = body_text(resp).await;
    assert!(
        page.contains("connect a mailbox to the pidge account jane@example.com"),
        "{page}"
    );
    assert!(
        page.contains(&format!("{PUBLIC}/connect/go?state=s1")),
        "{page}"
    );

    // Continue goes to Microsoft with the link's state and verifier, but
    // only when the browser presents the confirmation page's cookie.
    let resp = get_with_cookie(&h, "/connect/go?state=s1", &cookie).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with(&h.microsoft.uri()));
    assert_eq!(query(&location, "state").as_deref(), Some("s1"));
    assert_eq!(
        query(&location, "code_challenge").unwrap(),
        pkce_challenge("stub-verifier")
    );
    assert_eq!(
        query(&location, "redirect_uri").unwrap(),
        format!("{PUBLIC}/callback")
    );
    assert!(h.state.peek_pending("s1").is_some(), "link still usable");
}

#[tokio::test]
async fn connect_go_requires_the_confirmation_cookie() {
    let h = harness("second@example.com").await;
    h.state
        .insert_pending("s1".into(), connect_pending("jane@example.com"));

    // Handed the Continue URL directly, having never seen the confirmation.
    let resp = get(&h, "/connect/go?state=s1").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(resp.headers().get(header::LOCATION).is_none());
    let resp = get_with_cookie(&h, "/connect/go?state=s1", "pidge_connect=guess").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Someone else's browser saw the confirmation; a different nonce still fails.
    let resp = get(&h, "/connect?state=s1").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = get_with_cookie(&h, "/connect/go?state=s1", "pidge_connect=wrong-nonce").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(resp.headers().get(header::LOCATION).is_none());
}

#[tokio::test]
async fn connect_link_refuses_unknown_expired_and_sign_in_states() {
    let h = harness("second@example.com").await;
    h.state.insert_pending("signin".into(), pending_stub());
    h.state.insert_pending(
        "old".into(),
        PendingAuthorization {
            created_at: chrono::Utc::now() - chrono::Duration::minutes(11),
            ..connect_pending("jane@example.com")
        },
    );
    for path in ["/connect", "/connect/go"] {
        for query in ["", "?state=nope", "?state=signin", "?state=old"] {
            let uri = format!("{path}{query}");
            let resp = get(&h, &uri).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert!(resp.headers().get(header::LOCATION).is_none(), "{uri}");
        }
    }
}

#[tokio::test]
async fn connect_failures_render_a_page_instead_of_redirecting() {
    let h = harness("second@example.com").await;
    h.state
        .insert_pending("denied".into(), connect_pending("jane@example.com"));
    let resp = get(&h, "/callback?error=access_denied&state=denied").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(resp.headers().get(header::LOCATION).is_none());

    // Microsoft refuses the code: a higher-priority mock overrides the harness's.
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "bad code"
        })))
        .with_priority(1)
        .mount(&h.microsoft)
        .await;
    h.state
        .insert_pending("badcode".into(), connect_pending("jane@example.com"));
    let resp = get(&h, "/callback?code=x&state=badcode").await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(resp.headers().get(header::LOCATION).is_none());
    assert!(
        UserStore::new(h.secrets.clone())
            .load_mailbox("second@example.com")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn connect_refuses_another_users_sign_in_identity() {
    // Anna is on the allowlist, so her Microsoft account is her own pidge
    // user and can't become one of Jane's mailboxes.
    let h = harness("anna@example.com").await;
    let users = UserStore::new(h.secrets.clone());
    users
        .save(&UserRecord::new("jane@example.com"))
        .await
        .unwrap();
    h.state
        .insert_pending("s1".into(), connect_pending("jane@example.com"));
    let resp = get(&h, "/callback?code=x&state=s1").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        users
            .load_mailbox("anna@example.com")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        users
            .load("jane@example.com")
            .await
            .unwrap()
            .unwrap()
            .mailboxes,
        vec!["jane@example.com"]
    );
}

#[tokio::test]
async fn connect_refuses_an_owner_no_longer_on_the_allowlist() {
    let h = harness("second@example.com").await;
    h.state
        .insert_pending("s1".into(), connect_pending("mallory@example.com"));
    let resp = get(&h, "/callback?code=x&state=s1").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        UserStore::new(h.secrets.clone())
            .load_mailbox("second@example.com")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn profile_read_failure_logs_only_the_status() {
    use crate::test_support::{LogCapture, assert_no_address};

    let h = harness("jane@example.com").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"code": "x", "message": "no profile for jane@example.com"}
        })))
        .with_priority(1)
        .mount(&h.microsoft)
        .await;
    let client_id = register(&h.app).await;
    let (logs, _guard) = LogCapture::start();
    let resp = sign_in(
        &h,
        &client_id,
        "verifier-verifier-verifier-verifier-verifier",
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "error goes back to the client"
    );
    let logged = logs.text();
    assert!(
        logged.contains("reading signed-in profile failed"),
        "{logged}"
    );
    assert_no_address("logs", &logged);
}

// ---------------------------------------------------------------------------
// Sign-in consent page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn authorize_shows_a_consent_page_naming_the_client_and_host() {
    let h = harness("jane@example.com").await;
    let client_id = register_named(&h.app, "Sneaky <b>App</b>").await;
    let resp = get(
        &h,
        &authorize_uri(&client_id, "verifier-verifier-verifier-verifier"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::LOCATION).is_none());
    let set_cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with("pidge_authorize="), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=600"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/authorize"), "{set_cookie}");
    let page = body_text(resp).await;
    assert!(
        page.contains("Sign in to pidge for Sneaky &lt;b&gt;App&lt;/b&gt; at localhost?"),
        "{page}"
    );
    assert!(page.contains("Continue only if you started this from that app."));
    assert!(
        page.contains(&format!("{PUBLIC}/authorize/go?state=")),
        "{page}"
    );
}

#[tokio::test]
async fn authorize_go_needs_the_consent_cookie() {
    let h = harness("jane@example.com").await;
    let client_id = register(&h.app).await;
    let (go, cookie) = consent(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    let key = query(&format!("{PUBLIC}{go}"), "state").unwrap();

    // Handed the Continue URL directly, never having seen the page.
    let resp = get(&h, &go).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(resp.headers().get(header::LOCATION).is_none());
    let resp = get_with_cookie(&h, &go, "pidge_authorize=guess").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(resp.headers().get(header::LOCATION).is_none());
    // A connect link's cookie doesn't stand in for it.
    let other = cookie.replacen("pidge_authorize", "pidge_connect", 1);
    let resp = get_with_cookie(&h, &go, &other).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = get_with_cookie(&h, &go, &cookie).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with(&h.microsoft.uri()), "{location}");
    assert_eq!(query(&location, "state").as_deref(), Some(key.as_str()));
    let pending = h.state.peek_pending(&key).expect("still pending");
    assert_eq!(
        query(&location, "code_challenge").unwrap(),
        pkce_challenge(&pending.microsoft_verifier)
    );
}

#[tokio::test]
async fn authorize_go_refuses_unknown_and_connect_states() {
    let h = harness("jane@example.com").await;
    h.state
        .insert_pending("conn".into(), connect_pending("jane@example.com"));
    for uri in [
        "/authorize/go",
        "/authorize/go?state=nope",
        "/authorize/go?state=conn",
    ] {
        let resp = get_with_cookie(&h, uri, "pidge_authorize=x").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert!(resp.headers().get(header::LOCATION).is_none(), "{uri}");
    }
}

// ---------------------------------------------------------------------------
// Identity: principal name only, pinned to tid + oid
// ---------------------------------------------------------------------------

fn assert_nothing_stored(h: &Harness, addresses: &[&str]) {
    for a in addresses {
        assert!(
            !h.secrets_dir.path().join(mailbox_secret_name(a)).exists(),
            "mailbox secret for {a}"
        );
        assert!(
            !h.secrets_dir
                .path()
                .join(crate::users::user_secret_name(a))
                .exists(),
            "user record for {a}"
        );
    }
}

#[tokio::test]
async fn sign_in_ignores_a_mail_attribute_set_to_an_allowlisted_address() {
    // A foreign tenant's admin sets `mail` on their own account to Jane's
    // address; the principal name gives them away.
    let h = harness_with("mallory@evil.example", "jane@example.com", "oid-mallory").await;
    let client_id = register(&h.app).await;
    let resp = sign_in(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(resp.headers().get(header::LOCATION).is_none());
    assert_nothing_stored(&h, &["jane@example.com", "mallory@evil.example"]);
}

#[tokio::test]
async fn sign_in_with_a_different_object_id_than_the_pinned_one_is_refused() {
    let h = harness_with("jane@example.com", "jane@example.com", "oid-impostor").await;
    let users = UserStore::new(h.secrets.clone());
    let rec = UserRecord {
        identity: Some(identity_for("jane@example.com")),
        ..UserRecord::new("jane@example.com")
    };
    users.save(&rec).await.unwrap();
    let client_id = register(&h.app).await;
    let resp = sign_in(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(resp)
            .await
            .contains("could not verify the Microsoft account")
    );
    assert!(
        users
            .load_mailbox("jane@example.com")
            .await
            .unwrap()
            .is_none(),
        "no tokens stored"
    );
    assert_eq!(users.load("jane@example.com").await.unwrap().unwrap(), rec);
}

#[tokio::test]
async fn sign_in_refuses_a_mailbox_pinned_to_a_different_account() {
    let h = harness_with("jane@example.com", "jane@example.com", "oid-impostor").await;
    let users = UserStore::new(h.secrets.clone());
    users
        .save_mailbox(
            &crate::users::MailboxRecord {
                owner: "jane@example.com".into(),
                tokens: pidge_client::auth::TokenSet {
                    access_token: "a".into(),
                    refresh_token: "JANE_RT".into(),
                    expires_at: chrono::Utc::now(),
                },
                identity: Some(identity_for("jane@example.com")),
            },
            "jane@example.com",
        )
        .await
        .unwrap();
    let client_id = register(&h.app).await;
    let resp = sign_in(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let mailbox = users
        .load_mailbox("jane@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mailbox.tokens.refresh_token, "JANE_RT", "tokens untouched");
    assert!(users.load("jane@example.com").await.unwrap().is_none());
}

#[tokio::test]
async fn a_record_from_before_pinning_is_pinned_at_the_next_sign_in() {
    let h = harness("jane@example.com").await;
    let users = UserStore::new(h.secrets.clone());
    let mut rec = UserRecord::new("jane@example.com");
    rec.timezone = "Europe/London".into();
    users.save(&rec).await.unwrap();

    let client_id = register(&h.app).await;
    let resp = sign_in(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let pinned = users.load("jane@example.com").await.unwrap().unwrap();
    assert_eq!(pinned.identity, Some(identity_for("jane@example.com")));
    assert_eq!(pinned.timezone, "Europe/London", "the rest is kept");

    // The same account signs in again: still fine.
    let resp = sign_in(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers()[header::LOCATION].to_str().unwrap();
    assert!(location.starts_with(CLIENT_REDIRECT), "{location}");
}

#[tokio::test]
async fn sign_in_without_an_id_token_is_refused() {
    let h = harness("jane@example.com").await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "MS_AT", "refresh_token": "MS_RT", "expires_in": 3600,
            "id_token": id_token(serde_json::json!({ "tid": TENANT })),
        })))
        .with_priority(1)
        .mount(&h.microsoft)
        .await;
    let client_id = register(&h.app).await;
    let resp = sign_in(&h, &client_id, "verifier-verifier-verifier-verifier").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(resp)
            .await
            .contains("could not verify the Microsoft account")
    );
    assert_nothing_stored(&h, &["jane@example.com"]);
}

#[tokio::test]
async fn connect_binds_the_principal_name_and_pins_its_identity() {
    // `mail` claims Anna's (allowlisted) address; the account is second@….
    let h = harness_with("second@example.com", "anna@example.com", "oid-second").await;
    let users = UserStore::new(h.secrets.clone());
    users
        .save(&UserRecord::new("jane@example.com"))
        .await
        .unwrap();
    h.state
        .insert_pending("s1".into(), connect_pending("jane@example.com"));
    let resp = get(&h, "/callback?code=x&state=s1").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mailbox = users
        .load_mailbox("second@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mailbox.owner, "jane@example.com");
    assert_eq!(
        mailbox.identity,
        Some(Identity {
            tid: TENANT.into(),
            oid: "oid-second".into()
        })
    );
    assert!(
        users
            .load_mailbox("anna@example.com")
            .await
            .unwrap()
            .is_none()
    );

    // Another account under the same name can't take the mailbox over.
    let h2 = harness_with("second@example.com", "second@example.com", "oid-other").await;
    let users2 = UserStore::new(h2.secrets.clone());
    users2
        .save(&UserRecord::new("jane@example.com"))
        .await
        .unwrap();
    users2
        .save_mailbox(&mailbox, "second@example.com")
        .await
        .unwrap();
    h2.state
        .insert_pending("s2".into(), connect_pending("jane@example.com"));
    let resp = get(&h2, "/callback?code=x&state=s2").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        users2
            .load_mailbox("second@example.com")
            .await
            .unwrap()
            .unwrap()
            .identity,
        mailbox.identity
    );
}

#[tokio::test]
async fn microsoft_error_text_never_reaches_the_log() {
    use crate::test_support::{LogCapture, assert_no_address};

    let h = harness("second@example.com").await;
    let (logs, _guard) = LogCapture::start();
    h.state
        .insert_pending("denied".into(), connect_pending("jane@example.com"));
    let resp = get(
        &h,
        "/callback?error=access_denied&error_description=user%20jane%40example.com%20cancelled&state=denied",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "code for jane@example.com was already redeemed"
        })))
        .with_priority(1)
        .mount(&h.microsoft)
        .await;
    h.state
        .insert_pending("badcode".into(), connect_pending("jane@example.com"));
    let resp = get(&h, "/callback?code=x&state=badcode").await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let logged = logs.text();
    assert!(logged.contains("access_denied"), "{logged}");
    assert!(
        logged.contains("redeeming Microsoft code failed"),
        "{logged}"
    );
    assert!(logged.contains("400"), "{logged}");
    assert!(!logged.contains("cancelled"), "{logged}");
    assert!(!logged.contains("already redeemed"), "{logged}");
    assert_no_address("logs", &logged);
}

/// Signs `h`'s user in with a fresh client and redeems the code; returns
/// `(client_id, access, refresh)`.
async fn sign_in_and_redeem(h: &Harness) -> (String, String, String) {
    let client_id = register(&h.app).await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let resp = sign_in(h, &client_id, verifier).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers()[header::LOCATION].to_str().unwrap();
    let code = query(location, "code").expect("code in redirect");
    let (status, body) = redeem(
        &h.app,
        &format!(
            "grant_type=authorization_code&client_id={}&code={}&code_verifier={verifier}&redirect_uri={}",
            urlenc(&client_id),
            urlenc(&code),
            urlenc(CLIENT_REDIRECT)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        client_id,
        body["access_token"].as_str().unwrap().to_string(),
        body["refresh_token"].as_str().unwrap().to_string(),
    )
}

async fn refresh_grant(
    h: &Harness,
    client_id: &str,
    refresh: &str,
) -> (StatusCode, serde_json::Value) {
    redeem(
        &h.app,
        &format!(
            "grant_type=refresh_token&client_id={}&refresh_token={}",
            urlenc(client_id),
            urlenc(refresh)
        ),
    )
    .await
}

#[tokio::test]
async fn sign_out_everywhere_revokes_access_and_refresh_tokens() {
    let h = harness("jane@example.com").await;
    let (client_id, access, refresh) = sign_in_and_redeem(&h).await;
    assert_eq!(mcp_initialize(&h.app, Some(&access)).await, StatusCode::OK);

    let mcp = crate::tools::PidgeMcp::new(h.state.clone());
    let out = mcp
        .accounts_update(
            rmcp::handler::server::wrapper::Parameters(crate::tools::accounts::UpdateArgs {
                sign_out_everywhere: Some(true),
                ..Default::default()
            }),
            crate::tools::tests::request_context("jane@example.com"),
        )
        .await
        .unwrap();
    let out = crate::tools::tests::text(&out);
    assert!(out.contains("All sessions signed out"), "{out}");

    assert_eq!(
        mcp_initialize(&h.app, Some(&access)).await,
        StatusCode::UNAUTHORIZED,
        "old access token is revoked"
    );
    let (status, body) = refresh_grant(&h, &client_id, &refresh).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
    assert_eq!(body["error_description"], "session was signed out");

    // A fresh sign-in gets tokens of the new generation, which work.
    let (client_id, access, refresh) = sign_in_and_redeem(&h).await;
    assert_eq!(mcp_initialize(&h.app, Some(&access)).await, StatusCode::OK);
    let (status, body) = refresh_grant(&h, &client_id, &refresh).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Signs arbitrary `claims` with `key`, as a server from before token
/// generations would have.
fn sign_raw(claims: serde_json::Value, key: &[u8]) -> String {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(key),
    )
    .unwrap()
}

#[tokio::test]
async fn tokens_without_a_generation_claim_are_generation_zero() {
    let h = harness("jane@example.com").await;
    UserStore::new(h.secrets.clone())
        .save(&UserRecord::new("jane@example.com"))
        .await
        .unwrap();

    let now = chrono::Utc::now().timestamp();
    let old_access = sign_raw(
        serde_json::json!({
            "typ": "access", "jti": "j1", "iss": PUBLIC, "aud": format!("{PUBLIC}/mcp"),
            "sub": "jane@example.com", "iat": now, "exp": now + 600, "scope": "mail",
        }),
        &h.key,
    );
    assert_eq!(
        mcp_initialize(&h.app, Some(&old_access)).await,
        StatusCode::OK
    );

    let client_id = register(&h.app).await;
    let old_refresh = sign_raw(
        serde_json::json!({
            "typ": "refresh", "jti": "j2", "sub": "jane@example.com",
            "client_id": client_id, "exp": now + 600,
        }),
        &h.key,
    );
    let (status, body) = refresh_grant(&h, &client_id, &old_refresh).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Once the user's generation moves on, the old-format tokens stop working.
    let mut rec = UserRecord::new("jane@example.com");
    rec.token_generation = 1;
    UserStore::new(h.secrets.clone()).save(&rec).await.unwrap();
    h.state.set_generation("jane@example.com", 1);
    assert_eq!(
        mcp_initialize(&h.app, Some(&old_access)).await,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = refresh_grant(&h, &client_id, &old_refresh).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}
