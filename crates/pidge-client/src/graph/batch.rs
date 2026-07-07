//! Graph `$batch`: up to 20 sub-requests per round-trip.
//!
//! Used for bulk mutations (mark-read, move, delete, categorize) where pidge
//! previously issued one HTTP call per message. Sub-request throttling
//! (per-item 429 with a `retryAfter` hint) is honored by re-batching the
//! throttled subset.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ClientError;

pub const MAX_BATCH: usize = 20;
const MAX_ROUNDS: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub struct BatchRequest {
    pub id: String,
    pub method: &'static str,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<serde_json::Map<String, Value>>,
}

impl BatchRequest {
    /// A JSON-bodied request (adds the Content-Type header Graph requires).
    pub fn json(
        id: impl Into<String>,
        method: &'static str,
        url: impl Into<String>,
        body: Value,
    ) -> Self {
        let mut headers = serde_json::Map::new();
        headers.insert(
            "Content-Type".into(),
            Value::String("application/json".into()),
        );
        Self {
            id: id.into(),
            method,
            url: url.into(),
            body: Some(body),
            headers: Some(headers),
        }
    }

    /// A body-less request (POST /move-style actions take json; DELETE takes none).
    pub fn bare(id: impl Into<String>, method: &'static str, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            method,
            url: url.into(),
            body: None,
            headers: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BatchResponse {
    pub id: String,
    pub status: u16,
    #[serde(default)]
    pub body: Value,
}

impl BatchResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn retry_after(&self) -> Option<u64> {
        self.body
            .pointer("/error/retryAfterSeconds")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                self.body
                    .pointer("/error/retryAfterSeconds")
                    .and_then(Value::as_u64)
            })
    }
}

#[derive(Debug, Deserialize)]
struct BatchEnvelope {
    responses: Vec<BatchResponse>,
}

/// Execute all `requests` (chunked ≤20), re-batching per-item 429s up to
/// [`MAX_ROUNDS`] times. Returns one response per request id (order not
/// guaranteed — correlate by id). Items still throttled after the final
/// round are returned with their last 429 response.
pub async fn batch_all(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    requests: Vec<BatchRequest>,
) -> Result<Vec<BatchResponse>, ClientError> {
    let mut pending = requests;
    let mut done: Vec<BatchResponse> = Vec::new();

    for round in 0..MAX_ROUNDS {
        if pending.is_empty() {
            break;
        }
        let mut next_pending: Vec<BatchRequest> = Vec::new();
        let mut max_retry_after = 0u64;

        for chunk in pending.chunks(MAX_BATCH) {
            let payload = serde_json::json!({ "requests": chunk });
            let req = http
                .post(format!("{base_url}/$batch"))
                .bearer_auth(access_token)
                .json(&payload);
            let resp = super::send_with_retry(req).await?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ClientError::Graph {
                    status: status.as_u16(),
                    message: text,
                });
            }
            let envelope: BatchEnvelope = resp.json().await?;
            for item in envelope.responses {
                if item.status == 429 && round + 1 < MAX_ROUNDS {
                    max_retry_after = max_retry_after.max(item.retry_after().unwrap_or(1));
                    if let Some(original) = chunk.iter().find(|r| r.id == item.id) {
                        next_pending.push(original.clone());
                        continue;
                    }
                }
                done.push(item);
            }
        }

        pending = next_pending;
        if !pending.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(max_retry_after.min(30))).await;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    #[tokio::test]
    async fn chunks_over_20_into_multiple_posts() {
        let server = MockServer::start().await;
        struct Echo;
        impl Respond for Echo {
            fn respond(&self, req: &Request) -> ResponseTemplate {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let responses: Vec<Value> = body["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| serde_json::json!({"id": r["id"], "status": 204, "body": null}))
                    .collect();
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"responses": responses}))
            }
        }
        Mock::given(method("POST"))
            .and(path("/$batch"))
            .respond_with(Echo)
            .expect(2) // 25 requests → 20 + 5
            .mount(&server)
            .await;

        let requests: Vec<BatchRequest> = (0..25)
            .map(|i| BatchRequest::bare(i.to_string(), "DELETE", format!("/me/messages/{i}")))
            .collect();
        let http = reqwest::Client::new();
        let out = batch_all(&http, &server.uri(), "tok", requests)
            .await
            .unwrap();
        assert_eq!(out.len(), 25);
        assert!(out.iter().all(BatchResponse::is_success));
    }

    #[tokio::test]
    async fn per_item_429_is_rebatched() {
        let server = MockServer::start().await;
        struct ThrottleOnce {
            hits: std::sync::atomic::AtomicU32,
        }
        impl Respond for ThrottleOnce {
            fn respond(&self, req: &Request) -> ResponseTemplate {
                let first = self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let responses: Vec<Value> = body["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        if first && r["id"] == "1" {
                            serde_json::json!({"id": r["id"], "status": 429,
                                "body": {"error": {"code": "TooManyRequests", "retryAfterSeconds": 0}}})
                        } else {
                            serde_json::json!({"id": r["id"], "status": 200, "body": {}})
                        }
                    })
                    .collect();
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"responses": responses}))
            }
        }
        Mock::given(method("POST"))
            .and(path("/$batch"))
            .respond_with(ThrottleOnce {
                hits: std::sync::atomic::AtomicU32::new(0),
            })
            .expect(2)
            .mount(&server)
            .await;

        let requests = vec![
            BatchRequest::json(
                "0",
                "PATCH",
                "/me/messages/a",
                serde_json::json!({"isRead": true}),
            ),
            BatchRequest::json(
                "1",
                "PATCH",
                "/me/messages/b",
                serde_json::json!({"isRead": true}),
            ),
        ];
        let http = reqwest::Client::new();
        let out = batch_all(&http, &server.uri(), "tok", requests)
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert!(
            out.iter().all(BatchResponse::is_success),
            "throttled item retried"
        );
    }

    #[tokio::test]
    async fn per_item_404_surfaces_without_failing_batch() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/$batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "responses": [
                    {"id": "0", "status": 204, "body": null},
                    {"id": "1", "status": 404, "body": {"error": {"code": "ErrorItemNotFound"}}}
                ]
            })))
            .mount(&server)
            .await;

        let requests = vec![
            BatchRequest::bare("0", "DELETE", "/me/messages/a"),
            BatchRequest::bare("1", "DELETE", "/me/messages/b"),
        ];
        let http = reqwest::Client::new();
        let out = batch_all(&http, &server.uri(), "tok", requests)
            .await
            .unwrap();
        assert_eq!(out.iter().filter(|r| r.is_success()).count(), 1);
        assert_eq!(out.iter().find(|r| r.id == "1").unwrap().status, 404);
    }
}
