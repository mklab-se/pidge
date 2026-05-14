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
    /// Graph returns this when there are more pages. We expose it so the CLI
    /// can decide whether to keep paging.
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

/// One page of inbox messages plus a flag indicating whether more pages exist.
pub struct InboxPage {
    pub messages: Vec<Message>,
    pub has_more: bool,
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

/// List a page of messages from the Inbox folder, sorted by `receivedDateTime desc`.
///
/// `skip` is the offset into the result set (page * page_size for 0-based paging).
/// Returns an `InboxPage` whose `has_more` is true when Graph included an
/// `@odata.nextLink` — i.e., there are more messages beyond this page.
pub async fn list_inbox(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    limit: usize,
    skip: usize,
    unread_only: bool,
) -> Result<InboxPage, ClientError> {
    let url = format!("{base_url}/me/mailFolders/inbox/messages");
    let mut req = http.get(&url).bearer_auth(access_token).query(&[
        (
            "$select",
            "id,subject,from,receivedDateTime,isRead,bodyPreview",
        ),
        ("$orderby", "receivedDateTime desc"),
        ("$top", &limit.to_string()),
    ]);
    if skip > 0 {
        req = req.query(&[("$skip", &skip.to_string())]);
    }
    if unread_only {
        req = req.query(&[("$filter", "isRead eq false")]);
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }

    let list: GraphList = resp.json().await?;
    Ok(InboxPage {
        has_more: list.next_link.is_some(),
        messages: list
            .value
            .into_iter()
            .map(|g| to_message(g, account))
            .collect(),
    })
}

/// Search messages across all folders using Graph's `$search` KQL query.
///
/// `$search` doesn't combine with `$filter` or `$orderby` — results come back
/// in Graph's relevance ranking, not date order. Common query forms users can
/// pass:
///
/// - `alice budget`          → matches anywhere in subject/body/sender
/// - `from:alice@example.com`
/// - `subject:"q4 review"`
/// - `from:alice AND subject:budget`
pub async fn search_messages(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<Message>, ClientError> {
    // $search expects a quoted KQL string; the user passes the raw query.
    let quoted = format!("\"{}\"", query.replace('"', "\\\""));
    let url = format!("{base_url}/me/messages");
    let resp = http
        .get(&url)
        .bearer_auth(access_token)
        .query(&[
            (
                "$select",
                "id,subject,from,receivedDateTime,isRead,bodyPreview",
            ),
            ("$top", &limit.to_string()),
            ("$search", &quoted),
        ])
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
        .map(|g| to_message(g, account))
        .collect())
}

fn to_message(g: GraphMessage, account: &str) -> Message {
    Message {
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
    }
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
    patch_message(
        http,
        base_url,
        access_token,
        message_id,
        &serde_json::json!({ "isRead": true }),
    )
    .await
}

/// PATCH /me/messages/{id} — mark the message as unread.
pub async fn mark_unread(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), ClientError> {
    patch_message(
        http,
        base_url,
        access_token,
        message_id,
        &serde_json::json!({ "isRead": false }),
    )
    .await
}

/// PATCH /me/messages/{id} — set or clear the follow-up flag.
pub async fn set_flag(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    flagged: bool,
) -> Result<(), ClientError> {
    let status = if flagged { "flagged" } else { "notFlagged" };
    patch_message(
        http,
        base_url,
        access_token,
        message_id,
        &serde_json::json!({ "flag": { "flagStatus": status } }),
    )
    .await
}

async fn patch_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    body: &serde_json::Value,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}");
    let resp = http
        .patch(&url)
        .bearer_auth(access_token)
        .json(body)
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

/// POST /me/sendMail — compose-and-send a new message in one call.
///
/// The Graph endpoint takes ownership of the body — we wrap the `Outgoing`
/// in `{ "message": ..., "saveToSentItems": true }` so a copy lands in the
/// sender's Sent Items folder. Returns 202 Accepted on success.
pub async fn send_mail(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message: &Outgoing,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/sendMail");
    let body = serde_json::json!({
        "message": message.to_graph_json(),
        "saveToSentItems": true,
    });
    post_no_body(http, &url, access_token, &body).await
}

/// POST /me/messages/{id}/reply — reply to a message with an optional comment
/// prepended to Graph's auto-generated quoted text.
pub async fn reply_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    comment: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/reply");
    let body = serde_json::json!({ "comment": comment });
    post_no_body(http, &url, access_token, &body).await
}

/// POST /me/messages/{id}/replyAll — reply to every recipient on the thread.
pub async fn reply_all_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    comment: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/replyAll");
    let body = serde_json::json!({ "comment": comment });
    post_no_body(http, &url, access_token, &body).await
}

/// POST /me/messages/{id}/forward — forward to new recipients with optional comment.
pub async fn forward_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    to: &[String],
    comment: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/forward");
    let body = serde_json::json!({
        "comment": comment,
        "toRecipients": to.iter().map(|addr| serde_json::json!({
            "emailAddress": { "address": addr }
        })).collect::<Vec<_>>(),
    });
    post_no_body(http, &url, access_token, &body).await
}

async fn post_no_body(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    body: &serde_json::Value,
) -> Result<(), ClientError> {
    let resp = http
        .post(url)
        .bearer_auth(access_token)
        .json(body)
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

/// What the user is sending — pre-Graph-serialization shape.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub subject: String,
    pub body_text: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
}

impl Outgoing {
    fn to_graph_json(&self) -> serde_json::Value {
        fn addresses(list: &[String]) -> Vec<serde_json::Value> {
            list.iter()
                .map(|addr| serde_json::json!({ "emailAddress": { "address": addr } }))
                .collect()
        }
        serde_json::json!({
            "subject": self.subject,
            "body": {
                "contentType": "Text",
                "content": self.body_text,
            },
            "toRecipients": addresses(&self.to),
            "ccRecipients": addresses(&self.cc),
            "bccRecipients": addresses(&self.bcc),
        })
    }
}

/// POST /me/messages/{id}/move — move the message to another folder.
///
/// `destination` is either a Graph folder ID or a well-known folder name
/// (`"archive"`, `"deleteditems"`, `"junkemail"`, …). Graph returns the new
/// message (it gets a new ID in the target folder); we discard that since
/// the caller's cache will be refreshed on the next list/search.
pub async fn move_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    destination: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/move");
    let resp = http
        .post(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "destinationId": destination }))
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
        let page = list_inbox(&http, &server.uri(), "AT", "u@e.com", 5, 0, false)
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].subject, "Hello");
        assert_eq!(page.messages[0].from.address, "maria@mklab.se");
        assert!(!page.messages[0].is_read);
        assert_eq!(page.messages[0].account, "u@e.com");
        assert!(!page.has_more);
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
        let page = list_inbox(&http, &server.uri(), "AT", "u@e.com", 5, 0, true)
            .await
            .unwrap();
        assert!(page.messages.is_empty());
    }

    #[tokio::test]
    async fn list_inbox_passes_skip_when_nonzero() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .and(query_param("$skip", "25"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [],
                "@odata.nextLink": "https://graph.example/next"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let page = list_inbox(&http, &server.uri(), "AT", "u@e.com", 25, 25, false)
            .await
            .unwrap();
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn search_messages_passes_search_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/messages"))
            .and(query_param("$search", "\"alice budget\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "S1",
                        "subject": "Q4 budget review",
                        "from": { "emailAddress": { "name": "Alice", "address": "alice@example.com" } },
                        "receivedDateTime": "2026-05-13T22:00:00Z",
                        "isRead": true,
                        "bodyPreview": "Numbers attached"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let msgs = search_messages(&http, &server.uri(), "AT", "u@e.com", "alice budget", 25)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].subject, "Q4 budget review");
    }

    #[tokio::test]
    async fn mark_unread_patches_isread_false() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        mark_unread(&http, &server.uri(), "AT", "MSG")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_flag_patches_flag_status() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        set_flag(&http, &server.uri(), "AT", "MSG", true)
            .await
            .unwrap();
        set_flag(&http, &server.uri(), "AT", "MSG", false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_mail_wraps_outgoing_in_message_envelope() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/me/sendMail"))
            .and(body_partial_json(serde_json::json!({
                "saveToSentItems": true,
                "message": {
                    "subject": "Hello",
                    "body": { "contentType": "Text", "content": "Hi there" },
                    "toRecipients": [{ "emailAddress": { "address": "alice@example.com" } }]
                }
            })))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let msg = Outgoing {
            subject: "Hello".into(),
            body_text: "Hi there".into(),
            to: vec!["alice@example.com".into()],
            cc: vec![],
            bcc: vec![],
        };
        send_mail(&http, &server.uri(), "AT", &msg).await.unwrap();
    }

    #[tokio::test]
    async fn reply_message_posts_comment() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/reply"))
            .and(body_partial_json(
                serde_json::json!({ "comment": "Thanks!" }),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        reply_message(&http, &server.uri(), "AT", "MSG", "Thanks!")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn forward_message_posts_recipients_and_comment() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/forward"))
            .and(body_partial_json(serde_json::json!({
                "comment": "FYI",
                "toRecipients": [{ "emailAddress": { "address": "bob@example.com" } }]
            })))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        forward_message(
            &http,
            &server.uri(),
            "AT",
            "MSG",
            &["bob@example.com".into()],
            "FYI",
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn move_message_posts_destination() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/move"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": "NEW"})),
            )
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        move_message(&http, &server.uri(), "AT", "MSG", "archive")
            .await
            .unwrap();
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
        let m = get_message(&http, &server.uri(), "AT", "u@e.com", "AAA")
            .await
            .unwrap();
        assert_eq!(m.id, "AAA");
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.from.name, "Maria");
        assert_eq!(m.to.len(), 1);
        assert_eq!(m.to[0].address, "kristofer@mklab.se");
        assert!(matches!(
            m.body_content_type,
            pidge_core::BodyContentType::Html
        ));
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
        let atts = list_attachments(&http, &server.uri(), "AT", "MSG")
            .await
            .unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].name, "report.pdf");
        assert_eq!(atts[0].size_bytes, 12345);
    }

    #[tokio::test]
    async fn get_attachment_bytes_decodes_base64() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(
                "/me/messages/[A-Za-z0-9]+/attachments/[A-Za-z0-9-]+",
            ))
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
        let bytes = get_attachment_bytes(&http, &server.uri(), "AT", "MSG", "att-1")
            .await
            .unwrap();
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
