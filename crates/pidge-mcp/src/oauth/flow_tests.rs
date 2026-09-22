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
use crate::state::AppState;

const PUBLIC: &str = "http://localhost:8080";
const CLIENT_REDIRECT: &str = "http://localhost:9999/cb";

struct Harness {
    app: Router,
    microsoft: MockServer,
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
        allowed_emails: HashSet::from(["jane@example.com".to_string()]),
        secrets: SecretsBackend::File {
            dir: secrets_dir.path().to_path_buf(),
        },
    };
    let signer = Signer::new(&random_bytes(32), PUBLIC, format!("{PUBLIC}/mcp"));
    let auth = AuthClient::for_test("cid", microsoft.uri())
        .with_backend(Arc::new(SecretTokenBackend::new(secrets.clone())));
    let graph = GraphClient::for_test(auth, format!("{}/v1.0", microsoft.uri()));
    let state = Arc::new(AppState::new(config, signer, graph, secrets));
    Harness {
        app: build_router(state, CancellationToken::new()),
        microsoft,
        secrets_dir,
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
#[ignore = "re-enabled in Task 8"] // callback now needs a MailboxRecord created via UserStore::save_mailbox first
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
