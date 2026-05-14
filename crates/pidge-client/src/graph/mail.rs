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

#[derive(Debug, Deserialize)]
struct GraphFullMessage {
    id: String,
    subject: Option<String>,
    from: Option<GraphFromWrapper>,
    #[serde(rename = "toRecipients", default)]
    to_recipients: Vec<GraphFromWrapper>,
    #[serde(rename = "ccRecipients", default)]
    cc_recipients: Vec<GraphFromWrapper>,
    #[serde(rename = "bccRecipients", default)]
    bcc_recipients: Vec<GraphFromWrapper>,
    #[serde(rename = "receivedDateTime")]
    received_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "sentDateTime")]
    sent_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "isRead")]
    is_read: Option<bool>,
    body: GraphBody,
    #[serde(rename = "hasAttachments")]
    has_attachments: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GraphBody {
    #[serde(rename = "contentType")]
    content_type: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct GraphAttachmentList {
    value: Vec<GraphAttachment>,
}

#[derive(Debug, Deserialize)]
struct GraphAttachment {
    id: String,
    name: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    size: Option<u64>,
    #[serde(rename = "isInline")]
    is_inline: Option<bool>,
    #[serde(rename = "contentId")]
    content_id: Option<String>,
    #[serde(rename = "@odata.type", default)]
    odata_type: Option<String>,
    /// Only populated when fetching a single attachment (not in list endpoint).
    #[serde(rename = "contentBytes", default)]
    content_bytes: Option<String>,
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

    let resp = http.get(&url).bearer_auth(access_token).send().await?;
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

/// GET /me/messages/{id} — fetch a single message with full body.
pub async fn get_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    message_id: &str,
) -> Result<pidge_core::FullMessage, ClientError> {
    let url = format!(
        "{base_url}/me/messages/{message_id}\
         ?$select=id,subject,from,toRecipients,ccRecipients,bccRecipients,\
receivedDateTime,sentDateTime,isRead,body,hasAttachments"
    );
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let g: GraphFullMessage = resp.json().await?;

    fn from(addr: GraphFromAddress) -> pidge_core::MessageFrom {
        pidge_core::MessageFrom {
            name: addr.name.unwrap_or_default(),
            address: addr.address.unwrap_or_default(),
        }
    }
    fn unwrap_recipients(rs: Vec<GraphFromWrapper>) -> Vec<pidge_core::MessageFrom> {
        rs.into_iter().map(|w| from(w.email_address)).collect()
    }

    let content_type = match g.body.content_type.to_lowercase().as_str() {
        "html" => pidge_core::BodyContentType::Html,
        _ => pidge_core::BodyContentType::Text,
    };

    Ok(pidge_core::FullMessage {
        account: account.to_string(),
        id: g.id,
        from: g
            .from
            .map(|w| from(w.email_address))
            .unwrap_or_else(|| pidge_core::MessageFrom {
                name: String::new(),
                address: String::new(),
            }),
        to: unwrap_recipients(g.to_recipients),
        cc: unwrap_recipients(g.cc_recipients),
        bcc: unwrap_recipients(g.bcc_recipients),
        subject: g.subject.unwrap_or_default(),
        received_at: g.received_date_time,
        sent_at: g.sent_date_time,
        is_read: g.is_read.unwrap_or(true),
        body_content_type: content_type,
        body_content: g.body.content,
        has_attachments: g.has_attachments.unwrap_or(false),
    })
}

/// GET /me/messages/{id}/attachments — list attachments without fetching bytes.
/// Filters to file attachments only.
pub async fn list_attachments(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<pidge_core::Attachment>, ClientError> {
    let url = format!(
        "{base_url}/me/messages/{message_id}/attachments\
         ?$select=id,name,contentType,size,isInline,contentId"
    );
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let list: GraphAttachmentList = resp.json().await?;
    Ok(list
        .value
        .into_iter()
        .filter(|a| {
            a.odata_type
                .as_deref()
                .map(|t| t == "#microsoft.graph.fileAttachment")
                .unwrap_or(true)
        })
        .map(|a| pidge_core::Attachment {
            id: a.id,
            name: a.name.unwrap_or_default(),
            content_type: a.content_type.unwrap_or_default(),
            size_bytes: a.size.unwrap_or(0),
            is_inline: a.is_inline.unwrap_or(false),
            content_id: a.content_id,
        })
        .collect())
}

/// GET /me/messages/{id}/attachments/{attachment_id} — fetch a single attachment
/// with its base64 contentBytes. Returns the decoded bytes.
pub async fn get_attachment_bytes(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>, ClientError> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let url = format!("{base_url}/me/messages/{message_id}/attachments/{attachment_id}");
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let g: GraphAttachment = resp.json().await?;
    let b64 = g.content_bytes.ok_or_else(|| ClientError::Graph {
        status: 200,
        message: "attachment response missing contentBytes".to_string(),
    })?;
    STANDARD.decode(&b64).map_err(|e| ClientError::Graph {
        status: 200,
        message: format!("attachment base64 decode: {e}"),
    })
}

/// PATCH /me/messages/{id} — mark the message as read.
pub async fn mark_read(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}");
    let resp = http
        .patch(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "isRead": true }))
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, path_regex, query_param};
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

    #[tokio::test]
    async fn get_message_parses_graph_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "AAA",
                "subject": "Hello",
                "from": { "emailAddress": { "name": "Maria", "address": "maria@mklab.se" } },
                "toRecipients": [
                    { "emailAddress": { "name": "Kristofer", "address": "kristofer@mklab.se" } }
                ],
                "ccRecipients": [],
                "bccRecipients": [],
                "receivedDateTime": "2026-05-14T22:00:00Z",
                "sentDateTime": "2026-05-14T21:59:30Z",
                "isRead": false,
                "body": { "contentType": "html", "content": "<p>Hi</p>" },
                "hasAttachments": true
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let m = get_message(&http, &server.uri(), "AT", "u@e.com", "AAA").await.unwrap();
        assert_eq!(m.id, "AAA");
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.from.name, "Maria");
        assert_eq!(m.to.len(), 1);
        assert_eq!(m.to[0].address, "kristofer@mklab.se");
        assert!(matches!(m.body_content_type, pidge_core::BodyContentType::Html));
        assert_eq!(m.body_content, "<p>Hi</p>");
        assert!(m.has_attachments);
    }

    #[tokio::test]
    async fn list_attachments_filters_file_attachments() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/attachments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "@odata.type": "#microsoft.graph.fileAttachment",
                        "id": "att-1",
                        "name": "report.pdf",
                        "contentType": "application/pdf",
                        "size": 12345,
                        "isInline": false
                    },
                    {
                        "@odata.type": "#microsoft.graph.itemAttachment",
                        "id": "att-2",
                        "name": "an-email.eml",
                        "contentType": "message/rfc822",
                        "size": 7777,
                        "isInline": false
                    }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let atts = list_attachments(&http, &server.uri(), "AT", "MSG").await.unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].name, "report.pdf");
        assert_eq!(atts[0].size_bytes, 12345);
    }

    #[tokio::test]
    async fn get_attachment_bytes_decodes_base64() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/attachments/[A-Za-z0-9-]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "att-1",
                "name": "report.pdf",
                "contentType": "application/pdf",
                "size": 5,
                "isInline": false,
                "contentBytes": "aGVsbG8="
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let bytes = get_attachment_bytes(&http, &server.uri(), "AT", "MSG", "att-1").await.unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[tokio::test]
    async fn mark_read_patches_isread_true() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        mark_read(&http, &server.uri(), "AT", "MSG").await.unwrap();
    }
}
