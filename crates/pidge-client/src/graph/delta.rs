//! Graph delta queries: "what changed since last time".
//!
//! Bootstrap walks `@odata.nextLink` pages to the terminal `@odata.deltaLink`
//! (establishing the sync state); subsequent calls at the deltaLink return
//! only created/updated messages plus `@removed` tombstones and a fresh
//! deltaLink. An expired token (HTTP 410) surfaces as
//! [`ClientError::DeltaExpired`] so callers can re-bootstrap.

use chrono::{DateTime, Utc};
use pidge_core::{Event, Message};
use serde_json::Value;

use crate::error::ClientError;

#[derive(Debug, Clone)]
pub enum MailDeltaEvent {
    Created(Message),
    Updated(Message),
    Deleted { graph_id: String },
}

#[derive(Debug, Clone)]
pub enum CalendarDeltaEvent {
    CreatedOrUpdated(Box<Event>),
    Deleted { graph_id: String },
}

/// Follow a delta stream from `url` to its terminal deltaLink, collecting
/// raw item values along the way. Returns (items, delta_link).
async fn drain(
    http: &reqwest::Client,
    access_token: &str,
    url: &str,
    prefer: Option<&str>,
) -> Result<(Vec<Value>, String), ClientError> {
    let mut items: Vec<Value> = Vec::new();
    let mut next = url.to_string();
    loop {
        let mut req = http.get(&next).bearer_auth(access_token);
        if let Some(prefer) = prefer {
            req = req.header("Prefer", prefer);
        }
        let resp = super::send_with_retry(req).await?;
        let status = resp.status();
        if status.as_u16() == 410 {
            return Err(ClientError::DeltaExpired);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Graph {
                status: status.as_u16(),
                message: text,
            });
        }
        let body: Value = resp.json().await?;
        if let Some(page_items) = body.get("value").and_then(Value::as_array) {
            items.extend(page_items.iter().cloned());
        }
        if let Some(delta_link) = body.get("@odata.deltaLink").and_then(Value::as_str) {
            return Ok((items, delta_link.to_string()));
        }
        match body.get("@odata.nextLink").and_then(Value::as_str) {
            Some(link) => next = link.to_string(),
            None => {
                return Err(ClientError::Graph {
                    status: 500,
                    message: "delta response carried neither nextLink nor deltaLink".into(),
                });
            }
        }
    }
}

/// Establish mail delta state for a folder. Returns the deltaLink (and the
/// current messages, which `--full` bootstraps replay as `created` events).
pub async fn mail_delta_bootstrap(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    folder: &str,
) -> Result<(Vec<Message>, String), ClientError> {
    let url = format!(
        "{base_url}/me/mailFolders/{folder}/messages/delta?$select=id,subject,from,receivedDateTime,isRead,bodyPreview,hasAttachments,flag"
    );
    let (items, delta_link) = drain(http, access_token, &url, None).await?;
    let messages = items
        .into_iter()
        .filter(|v| v.get("@removed").is_none())
        .filter_map(|v| super::mail::message_from_delta_value(v, account))
        .collect();
    Ok((messages, delta_link))
}

/// Poll a mail deltaLink: changes since the link was minted + a fresh link.
pub async fn mail_delta(
    http: &reqwest::Client,
    access_token: &str,
    account: &str,
    delta_link: &str,
) -> Result<(Vec<MailDeltaEvent>, String), ClientError> {
    let (items, next_link) = drain(http, access_token, delta_link, None).await?;
    let events = items
        .into_iter()
        .filter_map(|v| {
            if v.get("@removed").is_some() {
                return v
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| MailDeltaEvent::Deleted {
                        graph_id: id.to_string(),
                    });
            }
            // Graph delta doesn't distinguish created vs updated reliably;
            // a freshly received message is reported the same way as a flag
            // change. Callers treat both as "changed"; we classify by
            // isRead=false && recent as a heuristic-free "created" only when
            // the message is new to the caller's own state. Here: everything
            // non-removed is Updated; the CLI layer decides presentation.
            super::mail::message_from_delta_value(v, account).map(MailDeltaEvent::Updated)
        })
        .collect();
    Ok((events, next_link))
}

/// Establish calendar delta state over a rolling window.
pub async fn calendar_delta_bootstrap(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(Vec<Event>, String), ClientError> {
    let url = format!(
        "{base_url}/me/calendarView/delta?startDateTime={}&endDateTime={}",
        start.to_rfc3339(),
        end.to_rfc3339()
    );
    let (items, delta_link) =
        drain(http, access_token, &url, Some("outlook.timezone=\"UTC\"")).await?;
    let events = items
        .into_iter()
        .filter(|v| v.get("@removed").is_none())
        .filter_map(|v| super::events::event_from_delta_value(v, account))
        .collect();
    Ok((events, delta_link))
}

/// Poll a calendar deltaLink.
pub async fn calendar_delta(
    http: &reqwest::Client,
    access_token: &str,
    account: &str,
    delta_link: &str,
) -> Result<(Vec<CalendarDeltaEvent>, String), ClientError> {
    let (items, next_link) = drain(
        http,
        access_token,
        delta_link,
        Some("outlook.timezone=\"UTC\""),
    )
    .await?;
    let events = items
        .into_iter()
        .filter_map(|v| {
            if v.get("@removed").is_some() {
                return v
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| CalendarDeltaEvent::Deleted {
                        graph_id: id.to_string(),
                    });
            }
            super::events::event_from_delta_value(v, account)
                .map(|e| CalendarDeltaEvent::CreatedOrUpdated(Box::new(e)))
        })
        .collect();
    Ok((events, next_link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn graph_msg(id: &str) -> Value {
        serde_json::json!({
            "id": id,
            "subject": format!("s-{id}"),
            "from": {"emailAddress": {"name": "N", "address": "n@x.se"}},
            "receivedDateTime": "2026-07-05T10:00:00Z",
            "isRead": false,
            "bodyPreview": "p",
            "hasAttachments": false
        })
    }

    #[tokio::test]
    async fn bootstrap_follows_next_links_to_delta_link() {
        let server = MockServer::start().await;
        let page2 = format!("{}/delta-page-2", server.uri());
        let final_delta = format!("{}/delta-final?token=abc", server.uri());
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [graph_msg("m1")],
                "@odata.nextLink": page2
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/delta-page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [graph_msg("m2")],
                "@odata.deltaLink": final_delta
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let (messages, delta_link) =
            mail_delta_bootstrap(&http, &server.uri(), "tok", "a@b.se", "inbox")
                .await
                .unwrap();
        assert_eq!(messages.len(), 2);
        assert!(delta_link.contains("token=abc"));
    }

    #[tokio::test]
    async fn poll_parses_changes_and_removals() {
        let server = MockServer::start().await;
        let next_delta = format!("{}/delta-final?token=next", server.uri());
        Mock::given(method("GET"))
            .and(path("/delta-poll"))
            .and(query_param("token", "prev"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    graph_msg("m9"),
                    {"id": "gone-1", "@removed": {"reason": "deleted"}}
                ],
                "@odata.deltaLink": next_delta
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let url = format!("{}/delta-poll?token=prev", server.uri());
        let (events, new_link) = mail_delta(&http, "tok", "a@b.se", &url).await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], MailDeltaEvent::Updated(m) if m.id == "m9"));
        assert!(matches!(&events[1], MailDeltaEvent::Deleted { graph_id } if graph_id == "gone-1"));
        assert!(new_link.contains("token=next"));
    }

    #[tokio::test]
    async fn expired_delta_is_typed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/delta-poll"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let url = format!("{}/delta-poll", server.uri());
        let err = mail_delta(&http, "tok", "a@b.se", &url).await.unwrap_err();
        assert!(matches!(err, ClientError::DeltaExpired));
    }
}
