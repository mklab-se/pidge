//! `/ai`: the page where a user enters their own AI provider key, opened
//! from a link minted by `accounts_connect_ai`. The key goes from the
//! browser straight to this server and into the user's own secret; it
//! never passes through the AI harness or its model.
//!
//! Like `/connect`, the page names the pidge account first and binds each
//! form to the browser that saw it: the GET sets a cookie nonce that the
//! POST must present, both as the cookie and as the form's token. Opening
//! the page changes nothing else: chat apps and browsers fetch links to
//! preview them before the user clicks, so a GET can't claim the link.
//! The first successful save uses it up.

use std::collections::HashMap;
use std::sync::Mutex;

use ailloy::{ErrorKind, Question, error_kind};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use axum::{Form, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::oauth::jwt::random_id;
use crate::oauth::pages;
use crate::review::ai::{self, AiProvider, AiSecret, AiStatus};
use crate::state::SharedState;
use crate::users::user_hash;

/// How long a link works.
pub const LINK_TTL: Duration = Duration::minutes(10);
const MAX_LINKS: usize = 1_000;
const COOKIE: &str = "pidge_ai";
/// Forms (page views) a link keeps valid at once; the oldest goes first.
const MAX_VIEWS: usize = 5;

#[derive(Debug, Clone)]
struct AiLink {
    owner: String,
    /// The nonces of the forms shown so far, newest last.
    nonces: Vec<String>,
    created_at: DateTime<Utc>,
}

/// Live `/ai` links, keyed by their state.
#[derive(Default)]
pub struct AiLinks {
    links: Mutex<HashMap<String, AiLink>>,
}

impl AiLinks {
    /// A new link for `owner`; returns its state.
    pub fn issue(&self, owner: &str) -> String {
        let key = random_id();
        let mut map = self.links.lock().expect("ai links lock");
        let cutoff = Utc::now() - LINK_TTL;
        map.retain(|_, l| l.created_at > cutoff);
        while map.len() >= MAX_LINKS {
            let Some(oldest) = map
                .iter()
                .min_by_key(|(_, l)| l.created_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
        map.insert(
            key.clone(),
            AiLink {
                owner: owner.to_string(),
                nonces: Vec::new(),
                created_at: Utc::now(),
            },
        );
        key
    }

    fn peek(&self, key: &str) -> Option<AiLink> {
        let map = self.links.lock().expect("ai links lock");
        map.get(key)
            .filter(|l| l.created_at > Utc::now() - LINK_TTL)
            .cloned()
    }

    fn add_nonce(&self, key: &str, nonce: String) {
        if let Some(l) = self.links.lock().expect("ai links lock").get_mut(key) {
            l.nonces.push(nonce);
            if l.nonces.len() > MAX_VIEWS {
                l.nonces.remove(0);
            }
        }
    }

    fn take(&self, key: &str) {
        self.links.lock().expect("ai links lock").remove(key);
    }
}

pub fn router() -> Router<SharedState> {
    Router::new().route("/ai", get(show).post(submit))
}

#[derive(Debug, Deserialize)]
struct ShowParams {
    state: Option<String>,
}

async fn show(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(p): Query<ShowParams>,
) -> Response {
    let Some((key, link)) = p
        .state
        .and_then(|k| state.review.ai_links.peek(&k).map(|l| (k, l)))
    else {
        return expired();
    };
    // A browser that already has a form for this link keeps its nonce.
    let nonce = match cookie_nonce(&headers, &state) {
        Some(c) if link.nonces.contains(&c) => c,
        _ => {
            let nonce = random_id();
            state.review.ai_links.add_nonce(&key, nonce.clone());
            nonce
        }
    };
    with_cookie(
        pages::ai_form(&link.owner, &key, &nonce, None),
        &nonce,
        &state,
    )
}

#[derive(Deserialize)]
struct SubmitForm {
    state: String,
    token: String,
    provider: String,
    #[serde(default)]
    model: String,
    key: String,
}

async fn submit(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Form(form): Form<SubmitForm>,
) -> Response {
    let Some(link) = state.review.ai_links.peek(&form.state) else {
        return expired();
    };
    let presented = cookie_nonce(&headers, &state);
    if presented.as_deref() != Some(form.token.as_str()) || !link.nonces.contains(&form.token) {
        return pages::ai_error(
            StatusCode::BAD_REQUEST,
            "Open the link from your assistant in this browser and fill in the form there.",
        );
    }
    let retry =
        |message: &str| pages::ai_form(&link.owner, &form.state, &form.token, Some(message));

    let Some(provider) = AiProvider::parse(&form.provider) else {
        return retry("Pick one of the listed providers.");
    };
    let model = match ai::check_model(provider, &form.model) {
        Ok(m) => m,
        Err(e) => return retry(&e),
    };
    let api_key = match ai::check_key(&form.key) {
        Ok(k) => k,
        Err(e) => return retry(&e),
    };
    let secret = AiSecret {
        provider,
        model,
        api_key,
        updated_at: Utc::now(),
    };

    if let Err(kind) = try_key(&state, &secret).await {
        tracing::info!(user = %user_hash(&link.owner), kind = ?kind, "ai key test failed");
        return retry(match kind {
            ErrorKind::Auth => "The provider rejected this key. Check that you copied all of it.",
            ErrorKind::QuotaExhausted => {
                "The key works, but the account has no credits left. Top up at the provider and try again."
            }
            ErrorKind::RateLimited | ErrorKind::Unavailable | ErrorKind::Network => {
                "The provider could not be reached right now. Try again in a minute."
            }
            _ => {
                "The provider refused the test request. Check the model name, or leave it empty for the default."
            }
        });
    }

    if let Err(e) = ai::save(&state.secrets, &link.owner, &secret).await {
        crate::users::log_store_error("saving ai key", &link.owner, &e);
        return pages::ai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "pidge could not save the key right now. Try again in a minute.",
        );
    }
    state.review.ai_clients.forget(&link.owner);
    let label = ai::label(&secret);
    let saved = async {
        let mut rec = state
            .users
            .load(&link.owner)
            .await?
            .unwrap_or_else(|| crate::users::UserRecord::new(&link.owner));
        // A fresh `status_since` marks the new key, so a review still
        // running with the old one won't write its result over it.
        rec.ai.set_status(AiStatus::Ok, Utc::now());
        rec.ai.status_since = Some(Utc::now());
        rec.ai.provider = Some(label.clone());
        rec.ai.last_attempt_at = None;
        state.users.save(&rec).await
    }
    .await;
    if let Err(e) = saved {
        crate::users::log_store_error("saving ai status", &link.owner, &e);
    }
    state.cache.invalidate_user(&link.owner);
    state.review.ai_links.take(&form.state);
    tracing::info!(user = %user_hash(&link.owner), provider = ?secret.provider, "ai key saved");
    pages::done(&format!(
        "AI sorting is set up with {label} for the pidge account {}. You can close this page and tell your assistant.",
        link.owner
    ))
}

/// One tiny request (a single yes/no on a one-line text) to check the key.
async fn try_key(state: &SharedState, secret: &AiSecret) -> Result<(), ErrorKind> {
    let client = ai::build_client(secret, state.review.ai_endpoint.as_deref())
        .map_err(|_| ErrorKind::InvalidRequest)?;
    let q = Question::yes_no("Is this a greeting?");
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client.eval_one("Hello", q),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(error_kind(&e)),
        Err(_) => Err(ErrorKind::Network),
    }
}

fn expired() -> Response {
    pages::ai_error(
        StatusCode::BAD_REQUEST,
        "This link has expired or is not valid. Ask your assistant for a new one.",
    )
}

/// Over https the cookie is `__Host-` prefixed, `Secure` and `Path=/` (like
/// the sign-in flows' cookies); over plain http (loopback development) it
/// keeps the bare name and `Path=/ai`.
fn secure(state: &SharedState) -> bool {
    state.config.public_url.scheme() == "https"
}

fn cookie_name(state: &SharedState) -> String {
    if secure(state) {
        format!("__Host-{COOKIE}")
    } else {
        COOKIE.to_string()
    }
}

fn with_cookie(mut resp: Response, nonce: &str, state: &SharedState) -> Response {
    let (flag, path) = if secure(state) {
        (" Secure;", "/")
    } else {
        ("", "/ai")
    };
    let cookie = format!(
        "{}={nonce}; HttpOnly;{flag} SameSite=Strict; Max-Age={}; Path={path}",
        cookie_name(state),
        LINK_TTL.num_seconds()
    );
    resp.headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().expect("cookie is ASCII"));
    resp
}

fn cookie_nonce(headers: &HeaderMap, state: &SharedState) -> Option<String> {
    let name = cookie_name(state);
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, value)| value.to_string())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http::Request;
    use tower::ServiceExt;
    use wiremock::matchers::{header as wm_header, method, path};
    use wiremock::{Mock, ResponseTemplate};

    use super::*;
    use crate::tools::tests::ToolHarness;

    fn app(h: &ToolHarness) -> Router {
        router().with_state(h.state.clone())
    }

    async fn body(resp: Response) -> String {
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Opens the link; returns the cookie nonce (also the form token).
    async fn open(h: &ToolHarness, key: &str) -> String {
        let resp = app(h)
            .oneshot(
                Request::get(format!("/ai?state={key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()[header::CONTENT_SECURITY_POLICY],
            pages::CONTENT_SECURITY_POLICY
        );
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        let cookie = resp.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
            "{cookie}"
        );
        let page = body(resp).await;
        assert!(page.contains(&h.signin), "names the account");
        assert!(page.contains(r#"type="password""#));
        cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_string()
    }

    async fn submit(h: &ToolHarness, key: &str, cookie: Option<&str>, token: &str) -> Response {
        let form = format!("state={key}&token={token}&provider=typesafe&model=&key=ts-test-key");
        let mut req =
            Request::post("/ai").header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, format!("{COOKIE}={c}"));
        }
        app(h)
            .oneshot(req.body(Body::from(form)).unwrap())
            .await
            .unwrap()
    }

    async fn mount_typesafe(h: &ToolHarness, status: u16) {
        let body = if status == 200 {
            serde_json::json!({"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.9}},"usage":{"input_tokens":10,"output_tokens":1}})
        } else {
            serde_json::json!({"detail":{"message":"nope"}})
        };
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .and(wm_header("authorization", "Bearer ts-test-key"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&h.graph)
            .await;
    }

    #[tokio::test]
    async fn a_tested_key_is_saved_and_the_status_turns_ok() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        mount_typesafe(&h, 200).await;
        let key = h.state.review.ai_links.issue(&h.signin);
        let nonce = open(&h, &key).await;
        let resp = submit(&h, &key, Some(&nonce), &nonce).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let page = body(resp).await;
        assert!(page.contains("TypeSafe jev-latest"), "{page}");
        assert!(!page.contains("ts-test-key"));

        let secret = ai::load(&h.secrets, &h.signin).await.unwrap().unwrap();
        assert_eq!(secret.api_key, "ts-test-key");
        let rec = h.record().await;
        assert_eq!(rec.ai.status, AiStatus::Ok);
        assert!(!serde_json::to_string(&rec).unwrap().contains("ts-test-key"));
        // One use only.
        let again = submit(&h, &key, Some(&nonce), &nonce).await;
        assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_rejected_key_is_not_saved_and_the_form_says_why() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        mount_typesafe(&h, 401).await;
        let key = h.state.review.ai_links.issue(&h.signin);
        let nonce = open(&h, &key).await;
        let page = body(submit(&h, &key, Some(&nonce), &nonce).await).await;
        assert!(page.contains("rejected this key"), "{page}");
        assert!(ai::load(&h.secrets, &h.signin).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_link_preview_fetched_first_does_not_lock_the_user_out() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        mount_typesafe(&h, 200).await;
        let key = h.state.review.ai_links.issue(&h.signin);
        // The chat app's link preview, then the user's own browser.
        open(&h, &key).await;
        let nonce = open(&h, &key).await;
        let resp = submit(&h, &key, Some(&nonce), &nonce).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(ai::load(&h.secrets, &h.signin).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn the_form_only_works_in_the_browser_that_opened_the_link() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        mount_typesafe(&h, 200).await;
        let key = h.state.review.ai_links.issue(&h.signin);
        let nonce = open(&h, &key).await;
        for (cookie, token) in [
            (None, nonce.as_str()),
            (Some("other"), nonce.as_str()),
            (Some(nonce.as_str()), "other"),
        ] {
            let resp = submit(&h, &key, cookie, token).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }
        assert!(ai::load(&h.secrets, &h.signin).await.unwrap().is_none());
        let unknown = submit(&h, "nope", Some(&nonce), &nonce).await;
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    }
}
