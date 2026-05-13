//! GET /me/mailFolders/inbox/messages — list inbox messages.

use pidge_core::{Message, MessageFrom};
use serde::Deserialize;

use crate::error::ClientError;

#[derive(Debug, Deserialize)]
struct GraphMessage {
    id: String,
    subject: Option<String>,
    from: Option<GraphFromWrapper>,
    #[serde(rename = "receivedDateTime")]
    received_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "isRead")]
    is_read: Option<bool>,
    #[serde(rename = "bodyPreview")]
    body_preview: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphFromWrapper {
    #[serde(rename = "emailAddress")]
    email_address: GraphFromAddress,
}

#[derive(Debug, Deserialize)]
struct GraphFromAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphList {
    value: Vec<GraphMessage>,
}

/// List the top N messages in the Inbox folder, sorted by `receivedDateTime desc`.
pub async fn list_inbox(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    limit: usize,
    unread_only: bool,
) -> Result<Vec<Message>, ClientError> {
    let mut url = format!(
        "{base_url}/me/mailFolders/inbox/messages\
         ?$select=id,subject,from,receivedDateTime,isRead,bodyPreview\
         &$orderby=receivedDateTime%20desc\
         &$top={limit}"
    );
    if unread_only {
        url.push_str("&$filter=isRead%20eq%20false");
    }

    let resp = http
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }

    let list: GraphList = resp.json().await?;
    Ok(list
        .value
        .into_iter()
        .map(|g| Message {
            account: account.to_string(),
            id: g.id,
            from: MessageFrom {
                name: g
                    .from
                    .as_ref()
                    .and_then(|f| f.email_address.name.clone())
                    .unwrap_or_default(),
                address: g
                    .from
                    .as_ref()
                    .and_then(|f| f.email_address.address.clone())
                    .unwrap_or_default(),
            },
            subject: g.subject.unwrap_or_default(),
            received_at: g.received_date_time,
            is_read: g.is_read.unwrap_or(true),
            preview: g.body_preview.unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_inbox_parses_graph_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .and(header("authorization", "Bearer AT"))
            .and(query_param("$top", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "AAAA",
                        "subject": "Hello",
                        "from": {
                            "emailAddress": {
                                "name": "Maria",
                                "address": "maria@mklab.se"
                            }
                        },
                        "receivedDateTime": "2026-05-13T22:00:00Z",
                        "isRead": false,
                        "bodyPreview": "Hi there"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let msgs = list_inbox(&http, &server.uri(), "AT", "u@e.com", 5, false)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].subject, "Hello");
        assert_eq!(msgs[0].from.address, "maria@mklab.se");
        assert!(!msgs[0].is_read);
        assert_eq!(msgs[0].account, "u@e.com");
    }

    #[tokio::test]
    async fn list_inbox_adds_filter_when_unread_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .and(query_param("$filter", "isRead eq false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": []
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let msgs = list_inbox(&http, &server.uri(), "AT", "u@e.com", 5, true)
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }
}
