//! Transport helpers shared by the Gmail and Calendar calls: retry on
//! throttling, status to [`ProviderError`] mapping, and id confinement.

use pidge_core::provider::{ProviderError, ProviderResult};
use reqwest::{Response, StatusCode};

/// Attempts (first try included) before a 429/503/504 becomes `Throttled`.
const MAX_ATTEMPTS: u32 = 4;

fn is_transient(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 503 | 504)
}

/// A transport failure (DNS, TLS, connection reset, ...).
pub(crate) fn transport(err: reqwest::Error) -> ProviderError {
    ProviderError::Other(format!("request to Google failed: {err}"))
}

/// Send `req`, retrying 429/503/504 with `Retry-After` (seconds) or a short
/// exponential backoff. A request that is still throttled after
/// [`MAX_ATTEMPTS`] tries becomes [`ProviderError::Throttled`].
pub(crate) async fn send_with_retry(req: reqwest::RequestBuilder) -> ProviderResult<Response> {
    let mut attempt: u32 = 0;
    loop {
        let this_try = match req.try_clone() {
            Some(clone) => clone,
            None => return req.send().await.map_err(transport),
        };
        let resp = this_try.send().await.map_err(transport)?;
        let status = resp.status();
        if !is_transient(status) {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        attempt += 1;
        if attempt >= MAX_ATTEMPTS {
            return Err(ProviderError::Throttled { retry_after });
        }
        let backoff = retry_after
            .map(std::time::Duration::from_secs)
            .unwrap_or_else(|| {
                let jitter = std::time::Duration::from_millis(u64::from(attempt) * 83 % 250);
                std::time::Duration::from_secs(1u64 << attempt.min(4)) / 2 + jitter
            });
        tracing::debug!(
            status = status.as_u16(),
            attempt,
            ?backoff,
            "retrying Google request"
        );
        tokio::time::sleep(backoff).await;
    }
}

/// Google's error envelope: `{"error": {"code", "message", "errors": [{"reason"}]}}`.
#[derive(serde::Deserialize, Default)]
struct ErrorEnvelope {
    #[serde(default)]
    error: ErrorBody,
}

#[derive(serde::Deserialize, Default)]
struct ErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<ErrorItem>,
}

#[derive(serde::Deserialize, Default)]
struct ErrorItem {
    #[serde(default)]
    reason: String,
}

/// Turn a non-success response into a [`ProviderError`]; pass a success
/// through. `account` names the mailbox in `Auth` errors; `what` names the
/// thing in `NotFound`.
pub(crate) async fn check(resp: Response, account: &str, what: &str) -> ProviderResult<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    let env: ErrorEnvelope = serde_json::from_str(&text).unwrap_or_default();
    let message = if env.error.message.is_empty() {
        format!("Google returned HTTP {}", status.as_u16())
    } else {
        env.error.message.clone()
    };
    let rate_limited = env
        .error
        .errors
        .iter()
        .any(|e| e.reason.ends_with("RateLimitExceeded") || e.reason == "quotaExceeded");
    Err(match status.as_u16() {
        401 => ProviderError::Auth {
            account: account.to_string(),
            message,
        },
        403 if rate_limited => ProviderError::Throttled { retry_after: None },
        404 | 410 => ProviderError::NotFound(what.to_string()),
        400 | 403 | 409 | 412 | 422 => ProviderError::Invalid(message),
        _ => ProviderError::Other(message),
    })
}

/// Confine a provider id (message, thread, label, draft, attachment or event
/// id) to `[A-Za-z0-9_-]` before it goes into a request path. Gmail ids are
/// hex, label ids `Label_123` or upper-case system names, attachment ids
/// base64url, Calendar ids base32hex with an optional `_<instant>` suffix:
/// all inside that alphabet, and nothing in it can reshape a URL path.
pub(crate) fn check_id<'a>(id: &'a str, what: &str) -> ProviderResult<&'a str> {
    let ok = !id.is_empty()
        && id.len() <= 2048
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(id)
    } else {
        Err(ProviderError::Invalid(format!("not a valid {what} id")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_confined_to_the_token_alphabet() {
        for good in [
            "18c2f0a9b1d2e3f4",
            "Label_12",
            "INBOX",
            "abc_20261105T100000Z",
            "r-123",
        ] {
            assert!(check_id(good, "message").is_ok(), "{good}");
        }
        for bad in ["", "..", "a/b", "a\\b", "a?b=1", "a%2F", "a b", "a.b", "é"] {
            assert!(
                matches!(check_id(bad, "message"), Err(ProviderError::Invalid(_))),
                "{bad}"
            );
        }
    }
}
