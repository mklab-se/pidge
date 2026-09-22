//! End-to-end tests of the authorization server with Microsoft mocked:
//! register → authorize → callback → token → MCP request.

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
use crate::users::{UserRecord, UserStore};

const PUBLIC: &str = "http://localhost:8080";
const CLIENT_REDIRECT: &str = "http://localhost:9999/cb";

struct Harness {
    app: Router,
    microsoft: MockServer,
    state: SharedState,
    secrets: SharedSecrets,
    secrets_dir: tempfile::TempDir,
}

async fn harness(signed_in_email: &str) -> Harness {
    let microsoft = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "MS_AT", "refresh_token": "MS_RT", "expires_in": 3600
        })))
        .mount(&microsoft)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "u1", "userPrincipalName": signed_in_email, "mail": signed_in_email
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
    };
    let signer = Signer::new(&random_bytes(32), PUBLIC, format!("{PUBLIC}/mcp"));
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

/// Runs register → authorize → callback and returns the client's code.
async fn sign_in(h: &Harness, client_id: &str, verifier: &str) -> axum::response::Response {
    let uri = format!(
        "/authorize?response_type=code&client_id={}&redirect_uri={}&state=client-state&code_challenge={}&code_challenge_method=S256",
        urlenc(client_id),
        urlenc(CLIENT_REDIRECT),
        pkce_challenge(verifier)
    );
    let resp = h
        .app
        .clone()
        .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
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
    let mut req = Request::post("/mcp")
        .header(header::HOST, "localhost:8080")
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
    let rec = users.load("jane@example.com").await.unwrap().unwrap();
    assert_eq!(rec, UserRecord::new("jane@example.com"));

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
    let page = body_text(resp).await;
    assert!(
        page.contains("connect a mailbox to the pidge account jane@example.com"),
        "{page}"
    );
    assert!(
        page.contains(&format!("{PUBLIC}/connect/go?state=s1")),
        "{page}"
    );

    // Continue goes to Microsoft with the link's state and verifier.
    let resp = get(&h, "/connect/go?state=s1").await;
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
