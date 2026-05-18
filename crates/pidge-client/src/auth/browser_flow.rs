//! OAuth 2.0 authorization code + PKCE flow with a one-shot local HTTP server.
//!
//! Why this and not device-code:
//!
//! Device-code flow works perfectly for work/school M365 accounts, but personal
//! Microsoft accounts (live.com / outlook.com / hotmail.com) consistently hit
//! `invalid_request: response_type missing` errors deep inside Microsoft's MSA
//! pipeline. After significant debugging — and trying multiple redirect-URI
//! shapes (nativeclient, http://localhost, urn:ietf:wg:oauth:2.0:oob) — the
//! conclusion was that personal MSA's device-code support is fragile in ways
//! that can't be papered over via app-registration tweaks.
//!
//! The auth-code + PKCE + local-server flow is what modern MSAL libraries use
//! and what Microsoft itself recommends for desktop / CLI apps. Both account
//! types route through the same `/oauth2/v2.0/authorize` endpoint and the
//! same redirect-URI plumbing, so M365 and MSA behave identically.
//!
//! Flow:
//!
//! 1. Bind a `TcpListener` on `127.0.0.1:0` — OS picks a free port.
//! 2. Generate a 64-char random `code_verifier`, derive `code_challenge =
//!    base64url(SHA256(code_verifier))`, and a random `state` for CSRF.
//! 3. Open the user's browser to
//!    `https://login.microsoftonline.com/common/oauth2/v2.0/authorize?
//!     client_id=…&response_type=code&redirect_uri=http://localhost:{port}&
//!     scope=…&code_challenge=…&code_challenge_method=S256&state=…`
//! 4. The user signs in; Microsoft redirects the browser back to
//!    `http://localhost:{port}/?code=…&state=…`.
//! 5. Our local listener accepts exactly one connection, reads the first
//!    request line, extracts the query params, writes a friendly HTML
//!    response, and closes.
//! 6. We POST `/oauth2/v2.0/token` with the auth code, code_verifier, and
//!    redirect_uri to exchange for an access + refresh token.
//!
//! The local server is single-shot: one connection, one response, then close.
//! No port collisions because we let the OS pick; no listening process left
//! behind; no firewall surprises because nothing outside the loopback
//! interface can reach it.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rand::distributions::Alphanumeric;
use rand::{Rng, thread_rng};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

/// The result of a successful sign-in.
pub struct AuthSuccess {
    pub tokens: TokenSet,
    /// The raw `id_token`, if Microsoft returned one. Caller uses this to
    /// extract the tenant ID via the existing `jwt::extract_tenant_id`.
    pub id_token: Option<String>,
}

/// Run the full browser flow to completion. Returns the access+refresh
/// tokens and (optionally) the id_token for tenant extraction.
///
/// `on_open` is called once we know the authorize URL — the caller is
/// expected to print it to the user and best-effort spawn their browser.
pub async fn run<F: FnOnce(&str)>(
    http: &reqwest::Client,
    authority_base: &str,
    client_id: &str,
    scope: &str,
    on_open: F,
) -> Result<AuthSuccess, ClientError> {
    // Bind first so we know which port to embed in the redirect URI.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(ClientError::Io)?;
    let port = listener.local_addr().map_err(ClientError::Io)?.port();
    let redirect_uri = format!("http://localhost:{port}");

    let verifier = make_code_verifier();
    let challenge = make_code_challenge(&verifier);
    let state = make_random(32);

    let authorize_url = build_authorize_url(
        authority_base,
        client_id,
        &redirect_uri,
        scope,
        &challenge,
        &state,
    );
    on_open(&authorize_url);

    let CallbackParams {
        code,
        state: returned_state,
    } = wait_for_callback(listener).await?;
    if returned_state != state {
        return Err(ClientError::Graph {
            status: 400,
            message: "OAuth state mismatch — possible CSRF or stale request".to_string(),
        });
    }

    let tokens_response = exchange_code(
        http,
        authority_base,
        client_id,
        &code,
        &verifier,
        &redirect_uri,
    )
    .await?;

    Ok(AuthSuccess {
        tokens: TokenSet {
            access_token: tokens_response.access_token,
            refresh_token: tokens_response.refresh_token.unwrap_or_default(),
            expires_at: chrono::Utc::now()
                + chrono::Duration::seconds(tokens_response.expires_in.unwrap_or(3600)),
        },
        id_token: tokens_response.id_token,
    })
}

fn build_authorize_url(
    authority_base: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    challenge: &str,
    state: &str,
) -> String {
    let mut url = url::Url::parse(&format!("{authority_base}/oauth2/v2.0/authorize"))
        .expect("authority_base is a valid URL");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_mode", "query")
        .append_pair("scope", scope)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        // `prompt=select_account` forces Microsoft to show the account picker
        // even if the user is already signed in to *some* account — this is
        // what stops "browser is already signed in to my M365 account so
        // pidge auto-grabs that one when I wanted my live.com account".
        .append_pair("prompt", "select_account");
    url.into()
}

struct CallbackParams {
    code: String,
    state: String,
}

/// Accept a single connection on the listener, parse the request line for
/// query parameters, write a success/error response, close. Single-shot.
async fn wait_for_callback(listener: TcpListener) -> Result<CallbackParams, ClientError> {
    // Generous timeout: users might take a minute or two to authenticate,
    // especially on MFA. 5 minutes matches Microsoft's own OAuth code TTL.
    let accept = listener.accept();
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(300), accept)
        .await
        .map_err(|_| ClientError::Graph {
            status: 408,
            message: "timed out waiting for browser sign-in (5 min)".to_string(),
        })?
        .map_err(ClientError::Io)?;

    // We only need the first ~1KB to parse the request line "GET /?...".
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).await.map_err(ClientError::Io)?;
    let request = std::str::from_utf8(&buf[..n]).unwrap_or("");
    let first_line = request.lines().next().unwrap_or("");
    let path_and_query =
        first_line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| ClientError::Graph {
                status: 400,
                message: "malformed browser callback request".to_string(),
            })?;

    // Parse "/?code=...&state=..." (or "/?error=...&error_description=...").
    let query_start = path_and_query.find('?').unwrap_or(path_and_query.len());
    let query = &path_and_query[query_start.saturating_add(1)..];
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut err: Option<String> = None;
    let mut err_description: Option<String> = None;
    for (k, v) in pairs {
        match k.as_str() {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => err = Some(v),
            "error_description" => err_description = Some(v),
            _ => {}
        }
    }

    if let Some(e) = err {
        let detail = err_description.unwrap_or_default();
        write_html(&mut stream, &error_page_html(&e, &detail), "Sign-in failed")
            .await
            .ok();
        return Err(ClientError::Graph {
            status: 400,
            message: format!("Microsoft sign-in: {e} — {detail}"),
        });
    }

    let code = code.ok_or_else(|| ClientError::Graph {
        status: 400,
        message: "browser callback missing `code` parameter".to_string(),
    })?;
    let state = state.unwrap_or_default();

    write_html(&mut stream, SUCCESS_HTML, "Signed in")
        .await
        .ok();

    Ok(CallbackParams { code, state })
}

async fn write_html(
    stream: &mut tokio::net::TcpStream,
    body: &str,
    title: &str,
) -> std::io::Result<()> {
    let body_bytes = body.as_bytes();
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         X-Title: {}\r\n\
         \r\n",
        body_bytes.len(),
        title,
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body_bytes).await?;
    stream.shutdown().await
}

const SUCCESS_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Signed in</title>
<style>
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
       max-width: 480px; margin: 80px auto; text-align: center; color: #1d1d1f; }
.check { font-size: 48px; color: #34c759; }
h1 { font-size: 24px; margin: 16px 0 8px; }
p { color: #6e6e73; }
</style></head>
<body>
  <div class="check">✓</div>
  <h1>Signed in to pidge</h1>
  <p>You can close this window and return to the terminal.</p>
</body></html>"#;

fn error_page_html(err: &str, description: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Sign-in failed</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
       max-width: 540px; margin: 80px auto; color: #1d1d1f; }}
.x {{ font-size: 48px; color: #ff3b30; text-align: center; }}
h1 {{ font-size: 22px; margin: 16px 0 8px; text-align: center; }}
.detail {{ background: #f5f5f7; padding: 16px; border-radius: 8px; font-family: ui-monospace, monospace;
         font-size: 13px; white-space: pre-wrap; word-break: break-word; }}
</style></head>
<body>
  <div class="x">✕</div>
  <h1>Sign-in failed</h1>
  <p class="detail"><strong>{err}</strong>
{description}</p>
  <p>You can close this window. Return to the terminal for next steps.</p>
</body></html>"#
    )
}

// --- PKCE helpers ----------------------------------------------------------

/// RFC 7636 says the code_verifier is "a high-entropy cryptographic random
/// STRING, using the unreserved characters … with a minimum length of 43
/// characters and a maximum length of 128 characters." 64 alphanumerics is
/// comfortably inside the spec and gives ~380 bits of entropy.
fn make_code_verifier() -> String {
    let mut rng = thread_rng();
    (0..64).map(|_| rng.sample(Alphanumeric) as char).collect()
}

/// `base64url(SHA256(code_verifier))` per RFC 7636 §4.2. URL_SAFE_NO_PAD is
/// the exact encoding the OAuth spec requires.
fn make_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// 32 bytes of OS random → URL-safe base64. Used for the `state` CSRF nonce.
fn make_random(byte_len: usize) -> String {
    let mut buf = vec![0u8; byte_len];
    thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(&buf)
}

// --- token exchange --------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    id_token: Option<String>,
}

async fn exchange_code(
    http: &reqwest::Client,
    authority_base: &str,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, ClientError> {
    let url = format!("{authority_base}/oauth2/v2.0/token");
    let params = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("redirect_uri", redirect_uri),
    ];
    let resp = http.post(&url).form(&params).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    Ok(resp.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_verifier_is_64_alphanumerics() {
        let v = make_code_verifier();
        assert_eq!(v.len(), 64);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn challenge_matches_rfc_7636_example() {
        // RFC 7636 §4 example:
        //   verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        //   challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            make_code_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let url = build_authorize_url(
            "https://login.microsoftonline.com/common",
            "client-id-here",
            "http://localhost:47821",
            "User.Read offline_access",
            "challenge-here",
            "state-here",
        );
        assert!(url.contains("client_id=client-id-here"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=challenge-here"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-here"));
        // redirect_uri is percent-encoded inside the query string.
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A47821"));
        assert!(url.contains("prompt=select_account"));
    }

    #[test]
    fn random_state_is_unique_per_call() {
        let a = make_random(32);
        let b = make_random(32);
        assert_ne!(a, b);
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
    }
}
