//! GET /me/mailFolders/inbox/messages — list inbox messages.

use pidge_core::{FlagStatus, Message, MessageFrom};
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
    /// Full body, requested with `Prefer: outlook.body-content-type="text"`
    /// so Graph converts HTML to plain text server-side. Used to build a
    /// richer preview than the 255-char `bodyPreview` cap allows.
    body: Option<GraphBody>,
    #[serde(rename = "hasAttachments")]
    has_attachments: Option<bool>,
    #[serde(rename = "conversationId", default)]
    conversation_id: Option<String>,
    #[serde(default)]
    flag: Option<GraphFlag>,
}

#[derive(Debug, Deserialize)]
struct GraphFlag {
    #[serde(rename = "flagStatus", default)]
    flag_status: Option<String>,
}

fn flag_status_from(g: Option<GraphFlag>) -> FlagStatus {
    match g.and_then(|f| f.flag_status).as_deref() {
        Some("flagged") => FlagStatus::Flagged,
        Some("complete") => FlagStatus::Complete,
        _ => FlagStatus::NotFlagged,
    }
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
    /// Graph continuation URL for the next page (`@odata.nextLink`).
    pub next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphFullMessage {
    id: String,
    #[serde(rename = "conversationId", default)]
    conversation_id: Option<String>,
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
    #[serde(default)]
    flag: Option<GraphFlag>,
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
    list_folder(
        http,
        base_url,
        access_token,
        account,
        "inbox",
        limit,
        skip,
        unread_only,
    )
    .await
}

/// List a page of messages from an arbitrary folder, identified by its Graph
/// folder ID (or a well-known name). Same shape as `list_inbox`; used by
/// `mail list --folder` to page through custom folders.
#[allow(clippy::too_many_arguments)]
pub async fn list_folder_messages(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    folder_id: &str,
    limit: usize,
    skip: usize,
    unread_only: bool,
) -> Result<InboxPage, ClientError> {
    list_folder(
        http,
        base_url,
        access_token,
        account,
        folder_id,
        limit,
        skip,
        unread_only,
    )
    .await
}

/// List a page of drafts from the Drafts folder. Drafts don't have a real
/// "received" time, so Graph sorts by `lastModifiedDateTime desc` here.
pub async fn list_drafts(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    limit: usize,
    skip: usize,
) -> Result<InboxPage, ClientError> {
    list_folder(
        http,
        base_url,
        access_token,
        account,
        "drafts",
        limit,
        skip,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn list_folder(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    folder: &str,
    limit: usize,
    skip: usize,
    unread_only: bool,
) -> Result<InboxPage, ClientError> {
    let url = format!("{base_url}/me/mailFolders/{folder}/messages");
    // Body is fetched in its native content type (HTML or text) so the
    // renderer can use html2text to extract anchor text + click targets —
    // making LINK TEXT (not URLs) the clickable surface in previews.
    let mut req = http.get(&url).bearer_auth(access_token).query(&[
        (
            "$select",
            "id,subject,from,receivedDateTime,isRead,bodyPreview,body,hasAttachments,flag,conversationId",
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

    let resp = super::send_with_retry(req).await?;
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
        next_link: list.next_link,
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
///
/// Parse one raw delta item into a [`Message`] (None if the shape is not a
/// message — e.g. a tombstone or a partial patch without required fields).
pub(crate) fn message_from_delta_value(
    value: serde_json::Value,
    account: &str,
) -> Option<pidge_core::Message> {
    let g: GraphMessage = serde_json::from_value(value).ok()?;
    Some(to_message(g, account))
}

/// Fetch every message in a conversation (thread), oldest first.
pub async fn list_conversation(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    conversation_id: &str,
) -> Result<Vec<pidge_core::Message>, ClientError> {
    let url = format!("{base_url}/me/messages");
    let filter = format!(
        "conversationId eq '{}'",
        conversation_id.replace('\'', "''")
    );
    let req = http
        .get(&url)
        .bearer_auth(access_token)
        .header("Prefer", "outlook.body-content-type=\"text\"")
        .query(&[
            (
                "$select",
                "id,subject,from,receivedDateTime,isRead,bodyPreview,body,hasAttachments,flag,conversationId",
            ),
            ("$filter", filter.as_str()),
            ("$orderby", "receivedDateTime asc"),
            ("$top", "100"),
        ]);
    let resp = super::send_with_retry(req).await?;
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

/// Fetch a page of messages at an absolute Graph URL (an `@odata.nextLink`
/// carried in a pidge cursor). Continues any listing or search stream.
pub async fn list_messages_at(
    http: &reqwest::Client,
    access_token: &str,
    account: &str,
    url: &str,
) -> Result<InboxPage, ClientError> {
    let req = http.get(url).bearer_auth(access_token);
    let resp = super::send_with_retry(req).await?;
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
        next_link: list.next_link,
        messages: list
            .value
            .into_iter()
            .map(|g| to_message(g, account))
            .collect(),
    })
}

pub async fn search_messages(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    query: &str,
    limit: usize,
) -> Result<InboxPage, ClientError> {
    // $search expects a quoted KQL string; the user passes the raw query.
    let quoted = format!("\"{}\"", query.replace('"', "\\\""));
    let url = format!("{base_url}/me/messages");
    let resp = super::send_with_retry(
        http.get(&url)
            .bearer_auth(access_token)
            .header("Prefer", "outlook.body-content-type=\"text\"")
            .query(&[
                (
                    "$select",
                    "id,subject,from,receivedDateTime,isRead,bodyPreview,body,hasAttachments,flag,conversationId",
                ),
                ("$top", &limit.to_string()),
                ("$search", &quoted),
            ]),
    )
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
    Ok(InboxPage {
        has_more: list.next_link.is_some(),
        next_link: list.next_link,
        messages: list
            .value
            .into_iter()
            .map(|g| to_message(g, account))
            .collect(),
    })
}

fn to_message(g: GraphMessage, account: &str) -> Message {
    let (body, body_content_type) = match g.body {
        Some(b) => {
            let kind = if b.content_type.eq_ignore_ascii_case("html") {
                pidge_core::BodyContentType::Html
            } else {
                pidge_core::BodyContentType::Text
            };
            (b.content, kind)
        }
        None => (String::new(), pidge_core::BodyContentType::Text),
    };
    Message {
        account: account.to_string(),
        id: g.id,
        conversation_id: g.conversation_id.unwrap_or_default(),
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
        // `preview` keeps the 255-char plain-text snippet Graph computes
        // (bodyPreview). It's used as a cheap fallback when body is absent
        // and as the plain-text source for `--json` output (AI agents and
        // scripts that don't want to deal with HTML).
        preview: g.body_preview.unwrap_or_default(),
        flag_status: flag_status_from(g.flag),
        has_attachments: g.has_attachments.unwrap_or(false),
        body,
        body_content_type,
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
receivedDateTime,sentDateTime,isRead,body,hasAttachments,flag,conversationId"
    );
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
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
        conversation_id: g.conversation_id.unwrap_or_default(),
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
        flag_status: flag_status_from(g.flag),
    })
}

/// GET /me/messages/{id}?$select=internetMessageHeaders — fetch just the
/// raw RFC 5322 headers for a message. Used by `pidge mail unsubscribe`
/// to locate `List-Unsubscribe` / `List-Unsubscribe-Post`.
pub async fn fetch_message_headers(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<(String, String)>, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}?$select=internetMessageHeaders");
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let body: GraphHeadersResponse = resp.json().await?;
    Ok(body
        .internet_message_headers
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.name, h.value))
        .collect())
}

#[derive(serde::Deserialize)]
struct GraphHeadersResponse {
    #[serde(rename = "internetMessageHeaders", default)]
    internet_message_headers: Option<Vec<GraphHeader>>,
}

#[derive(serde::Deserialize)]
struct GraphHeader {
    name: String,
    value: String,
}

/// GET /me/messages/{id}/attachments — list attachments without fetching bytes.
/// Filters to file attachments only.
pub async fn list_attachments(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<pidge_core::Attachment>, ClientError> {
    // contentId is intentionally NOT in $select: it lives on the
    // `microsoft.graph.fileAttachment` subtype, not the base attachment
    // type, so Graph rejects the query with a 400 when it's in a flat
    // select clause. We don't currently use content_id in the CLI; if we
    // ever need it (e.g. to match inline `cid:` references), per-attachment
    // GETs return it without a cast.
    let url = format!(
        "{base_url}/me/messages/{message_id}/attachments\
         ?$select=id,name,contentType,size,isInline"
    );
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
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
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
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

#[derive(serde::Deserialize)]
struct GraphCategories {
    #[serde(default)]
    categories: Vec<String>,
}

/// GET /me/messages/{id}?$select=categories — read a message's categories.
pub async fn get_categories(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<String>, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}?$select=categories");
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let body: GraphCategories = resp.json().await?;
    Ok(body.categories)
}

/// PATCH /me/messages/{id} with `{ "categories": [...] }` — replace categories.
pub async fn set_categories(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    categories: &[String],
) -> Result<(), ClientError> {
    patch_message(
        http,
        base_url,
        access_token,
        message_id,
        &serde_json::json!({ "categories": categories }),
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
    let resp =
        super::send_with_retry(http.patch(&url).bearer_auth(access_token).json(body)).await?;
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

/// Convert plain-text body to HTML, escaping special chars and preserving
/// the line- and paragraph-breaks the user typed.
///
/// Graph's `/reply` and `/forward` endpoints accept a `comment` string and
/// insert it into the reply body — but when the source message body is HTML
/// (which Outlook always serves), newlines in `comment` collapse to spaces.
/// To keep the user's formatting, we send the body as HTML via a
/// `createReply` + PATCH dance (see `prepend_html_to_draft`), and this helper
/// is the text→HTML conversion that feeds it.
fn text_to_html(text: &str) -> String {
    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|paragraph| {
            let lines: Vec<String> = paragraph
                .trim_matches('\n')
                .split('\n')
                .map(escape)
                .collect();
            format!("<p>{}</p>", lines.join("<br>"))
        })
        .collect::<Vec<_>>()
        .join("")
}

#[derive(Debug, Deserialize)]
struct GraphBodyOnly {
    body: GraphBody,
}

/// GET a draft's body, splice `html_to_prepend` in above Graph's auto-quoted
/// text, and PATCH the draft. Used by reply/reply-all/forward to deliver
/// HTML-formatted comments that survive Outlook's HTML rendering.
async fn prepend_html_to_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    html_to_prepend: &str,
) -> Result<(), ClientError> {
    let get_url = format!("{base_url}/me/messages/{message_id}?$select=body");
    let resp = super::send_with_retry(http.get(&get_url).bearer_auth(access_token)).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let existing: GraphBodyOnly = resp.json().await?;
    let existing_content = existing.body.content;

    // Insert right after the opening `<body...>` tag if present, otherwise
    // prepend to the whole string. Case-insensitive match because Outlook
    // sometimes serves uppercase `<BODY>`.
    let new_content = match find_body_tag_end(&existing_content) {
        Some(pos) => {
            let mut s = String::with_capacity(existing_content.len() + html_to_prepend.len());
            s.push_str(&existing_content[..pos]);
            s.push_str(html_to_prepend);
            s.push_str(&existing_content[pos..]);
            s
        }
        None => format!("{html_to_prepend}{existing_content}"),
    };

    let patch_url = format!("{base_url}/me/messages/{message_id}");
    let patch_body = serde_json::json!({
        "body": {
            "contentType": "HTML",
            "content": new_content,
        }
    });
    let resp = super::send_with_retry(
        http.patch(&patch_url)
            .bearer_auth(access_token)
            .json(&patch_body),
    )
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

fn find_body_tag_end(html: &str) -> Option<usize> {
    let lc = html.to_ascii_lowercase();
    let start = lc.find("<body")?;
    let after = &html[start..];
    let close_rel = after.find('>')?;
    Some(start + close_rel + 1)
}

/// Reply to a message — sends immediately. Uses createReply + body PATCH +
/// send so the comment is delivered as HTML and the user's paragraph and
/// line breaks survive Outlook's HTML rendering.
pub async fn reply_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    comment: &str,
) -> Result<(), ClientError> {
    let draft_id = create_reply_draft(http, base_url, access_token, message_id, comment).await?;
    send_draft(http, base_url, access_token, &draft_id).await
}

/// Reply-all variant of `reply_message`.
pub async fn reply_all_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    comment: &str,
) -> Result<(), ClientError> {
    let draft_id =
        create_reply_all_draft(http, base_url, access_token, message_id, comment).await?;
    send_draft(http, base_url, access_token, &draft_id).await
}

/// Forward — sends immediately, with HTML-formatted comment.
pub async fn forward_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    to: &[String],
    comment: &str,
) -> Result<(), ClientError> {
    let draft_id =
        create_forward_draft(http, base_url, access_token, message_id, to, comment).await?;
    send_draft(http, base_url, access_token, &draft_id).await
}

async fn post_no_body(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    body: &serde_json::Value,
) -> Result<(), ClientError> {
    let resp = super::send_with_retry(http.post(url).bearer_auth(access_token).json(body)).await?;
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

/// POST /me/messages — create a draft message in the Drafts folder.
/// Returns the new draft's Graph message ID.
pub async fn create_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message: &Outgoing,
) -> Result<String, ClientError> {
    let url = format!("{base_url}/me/messages");
    let body = message.to_graph_json();
    let resp =
        super::send_with_retry(http.post(&url).bearer_auth(access_token).json(&body)).await?;
    parse_id_from_response(resp).await
}

/// POST /me/messages/{id}/createReply — create a reply draft. Returns the
/// new draft's Graph message ID.
///
/// The comment is converted from plain text to HTML and spliced into the
/// draft body via PATCH, so paragraph- and line-breaks survive Outlook's
/// HTML rendering. (Graph's own `comment` parameter would collapse them.)
pub async fn create_reply_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    comment: &str,
) -> Result<String, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/createReply");
    let resp = super::send_with_retry(
        http.post(&url)
            .bearer_auth(access_token)
            .json(&serde_json::json!({})),
    )
    .await?;
    let draft_id = parse_id_from_response(resp).await?;
    if !comment.is_empty() {
        prepend_html_to_draft(
            http,
            base_url,
            access_token,
            &draft_id,
            &text_to_html(comment),
        )
        .await?;
    }
    Ok(draft_id)
}

/// POST /me/messages/{id}/createReplyAll — create a reply-all draft.
pub async fn create_reply_all_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    comment: &str,
) -> Result<String, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/createReplyAll");
    let resp = super::send_with_retry(
        http.post(&url)
            .bearer_auth(access_token)
            .json(&serde_json::json!({})),
    )
    .await?;
    let draft_id = parse_id_from_response(resp).await?;
    if !comment.is_empty() {
        prepend_html_to_draft(
            http,
            base_url,
            access_token,
            &draft_id,
            &text_to_html(comment),
        )
        .await?;
    }
    Ok(draft_id)
}

/// POST /me/messages/{id}/createForward — create a forward draft with the
/// given recipients already populated.
pub async fn create_forward_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    to: &[String],
    comment: &str,
) -> Result<String, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/createForward");
    let resp = super::send_with_retry(http.post(&url).bearer_auth(access_token).json(
        &serde_json::json!({
            "toRecipients": to.iter().map(|addr| serde_json::json!({
                "emailAddress": { "address": addr }
            })).collect::<Vec<_>>(),
        }),
    ))
    .await?;
    let draft_id = parse_id_from_response(resp).await?;
    if !comment.is_empty() {
        prepend_html_to_draft(
            http,
            base_url,
            access_token,
            &draft_id,
            &text_to_html(comment),
        )
        .await?;
    }
    Ok(draft_id)
}

/// POST /me/messages/{id}/send — send an existing draft.
pub async fn send_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/send");
    // Graph's /send takes no body but requires `Content-Length: 0`; reqwest
    // omits that header for body-less requests, so the Graph edge layer
    // responds with HTTP 411. Sending an explicit empty body forces the
    // header to land.
    let resp = super::send_with_retry(
        http.post(&url)
            .bearer_auth(access_token)
            .header(reqwest::header::CONTENT_LENGTH, 0)
            .body(reqwest::Body::from("")),
    )
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

/// PATCH /me/messages/{id} — overwrite a draft's editable fields. Only the
/// fields in `Outgoing` are patched, since that's what our wizard owns.
pub async fn update_draft(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    message: &Outgoing,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}");
    let body = message.to_graph_json();
    let resp =
        super::send_with_retry(http.patch(&url).bearer_auth(access_token).json(&body)).await?;
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

/// DELETE /me/messages/{id} — moves the message to Deleted Items. Same call
/// works for drafts and for inbox messages; the destination folder differs
/// only by what the user is currently in.
pub async fn delete_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}");
    let resp = super::send_with_retry(http.delete(&url).bearer_auth(access_token)).await?;
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

/// POST /me/messages/{id}/attachments — attach a file to a draft.
///
/// Uses Graph's simple (non-resumable) upload, which is limited to ~3 MB per
/// attachment. Larger files require `createUploadSession`, which isn't wired
/// yet — the CLI rejects oversized attachments before calling this.
pub async fn add_attachment(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<String, ClientError> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let url = format!("{base_url}/me/messages/{message_id}/attachments");
    let body = serde_json::json!({
        "@odata.type": "#microsoft.graph.fileAttachment",
        "name": name,
        "contentType": content_type,
        "contentBytes": STANDARD.encode(bytes),
    });
    let resp =
        super::send_with_retry(http.post(&url).bearer_auth(access_token).json(&body)).await?;
    parse_id_from_response(resp).await
}

/// DELETE /me/messages/{id}/attachments/{att_id}.
pub async fn delete_attachment(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    attachment_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}/attachments/{attachment_id}");
    let resp = super::send_with_retry(http.delete(&url).bearer_auth(access_token)).await?;
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

async fn parse_id_from_response(resp: reqwest::Response) -> Result<String, ClientError> {
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let v: serde_json::Value = resp.json().await?;
    v["id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| ClientError::Graph {
            status: 200,
            message: "draft response missing 'id'".to_string(),
        })
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
    let resp = super::send_with_retry(
        http.post(&url)
            .bearer_auth(access_token)
            .json(&serde_json::json!({ "destinationId": destination })),
    )
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

/// A mail folder as returned by Graph's `/me/mailFolders` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct MailFolder {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// Total message count, present when selected. `None` if Graph omitted it.
    #[serde(rename = "totalItemCount", default)]
    pub total_item_count: Option<u64>,
    /// Unread message count, present when selected.
    #[serde(rename = "unreadItemCount", default)]
    pub unread_item_count: Option<u64>,
    /// Number of immediate child folders, present when selected. Lets callers
    /// skip a `childFolders` round-trip for folders that have none.
    #[serde(rename = "childFolderCount", default)]
    pub child_folder_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GraphFolderList {
    value: Vec<MailFolder>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

/// Fields every folder listing selects — shared so top-level and child
/// listings return identically-shaped `MailFolder`s.
const FOLDER_SELECT: &str = "id,displayName,totalItemCount,unreadItemCount,childFolderCount";

/// Page through a `mailFolders`/`childFolders` collection starting at `url`,
/// following `@odata.nextLink` until exhausted.
async fn fetch_folder_pages(
    http: &reqwest::Client,
    access_token: &str,
    mut url: String,
) -> Result<Vec<MailFolder>, ClientError> {
    let mut folders: Vec<MailFolder> = Vec::new();
    loop {
        let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Graph {
                status: status.as_u16(),
                message: text,
            });
        }
        let list: GraphFolderList = resp.json().await?;
        folders.extend(list.value);
        // `@odata.nextLink` is an absolute URL that already carries the
        // `$select`/`$top` query — follow it verbatim.
        match list.next_link {
            Some(next) => url = next,
            None => break,
        }
    }
    Ok(folders)
}

/// GET /me/mailFolders — list the top-level mail folders (id, displayName,
/// counts). Pages through `@odata.nextLink` so mailboxes with many folders
/// are fully enumerated rather than silently truncated at one page.
pub async fn list_mail_folders(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
) -> Result<Vec<MailFolder>, ClientError> {
    let url = format!("{base_url}/me/mailFolders?$select={FOLDER_SELECT}&$top=100");
    fetch_folder_pages(http, access_token, url).await
}

/// GET /me/mailFolders/{parent_id}/childFolders — list a folder's immediate
/// children. Same shape and paging as `list_mail_folders`.
pub async fn list_child_folders(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    parent_id: &str,
) -> Result<Vec<MailFolder>, ClientError> {
    let url = format!(
        "{base_url}/me/mailFolders/{parent_id}/childFolders?$select={FOLDER_SELECT}&$top=100"
    );
    fetch_folder_pages(http, access_token, url).await
}

async fn post_folder(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    display_name: &str,
) -> Result<MailFolder, ClientError> {
    let resp = super::send_with_retry(
        http.post(url)
            .bearer_auth(access_token)
            .json(&serde_json::json!({ "displayName": display_name })),
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let folder: MailFolder = resp.json().await?;
    Ok(folder)
}

/// POST /me/mailFolders — create a new top-level folder, returning it.
///
/// Graph rejects a duplicate `displayName` with 409; callers that want
/// "create if missing" semantics should list first and only call this when
/// no case-insensitive match exists.
pub async fn create_mail_folder(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    display_name: &str,
) -> Result<MailFolder, ClientError> {
    let url = format!("{base_url}/me/mailFolders");
    post_folder(http, &url, access_token, display_name).await
}

/// POST /me/mailFolders/{parent_id}/childFolders — create a child folder
/// under `parent_id`, returning it.
pub async fn create_child_folder(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    parent_id: &str,
    display_name: &str,
) -> Result<MailFolder, ClientError> {
    let url = format!("{base_url}/me/mailFolders/{parent_id}/childFolders");
    post_folder(http, &url, access_token, display_name).await
}

/// DELETE /me/mailFolders/{id} — delete a folder. Outlook moves the folder
/// (and any contents) to Deleted Items, so this is recoverable. Works for
/// top-level and child folders alike.
pub async fn delete_mail_folder(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    folder_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/mailFolders/{folder_id}");
    let resp = super::send_with_retry(http.delete(&url).bearer_auth(access_token)).await?;
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
            .unwrap()
            .messages;
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
    async fn reply_message_creates_draft_patches_body_and_sends() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;

        // 1. createReply → returns draft ID
        Mock::given(method("POST"))
            .and(path_regex("/me/messages/MSG/createReply"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "DRAFT" })),
            )
            .mount(&server)
            .await;
        // 2. GET draft body
        Mock::given(method("GET"))
            .and(path("/me/messages/DRAFT"))
            .and(query_param("$select", "body"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "body": { "contentType": "HTML", "content": "<html><body><div></div></body></html>" }
            })))
            .mount(&server)
            .await;
        // 3. PATCH draft body — verify our HTML lands in `body.content`
        Mock::given(method("PATCH"))
            .and(path("/me/messages/DRAFT"))
            .and(body_partial_json(serde_json::json!({
                "body": { "contentType": "HTML" }
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // 4. send
        Mock::given(method("POST"))
            .and(path("/me/messages/DRAFT/send"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        reply_message(&http, &server.uri(), "AT", "MSG", "Thanks!")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn forward_message_creates_draft_with_recipients_and_sends() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex("/me/messages/MSG/createForward"))
            .and(body_partial_json(serde_json::json!({
                "toRecipients": [{ "emailAddress": { "address": "bob@example.com" } }]
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "DRAFT" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me/messages/DRAFT"))
            .and(query_param("$select", "body"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "body": { "contentType": "HTML", "content": "<html><body></body></html>" }
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/me/messages/DRAFT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/me/messages/DRAFT/send"))
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

    #[test]
    fn text_to_html_escapes_and_breaks_paragraphs() {
        let out = text_to_html("Hej Edward,\n\nLine 1\nLine 2\n\n<script>x</script>");
        assert!(out.contains("<p>Hej Edward,</p>"));
        assert!(out.contains("<p>Line 1<br>Line 2</p>"));
        assert!(out.contains("&lt;script&gt;x&lt;/script&gt;"));
        assert!(!out.contains("<script>"));
    }

    #[test]
    fn text_to_html_normalizes_crlf() {
        let out = text_to_html("A\r\nB\r\n\r\nC");
        assert!(out.contains("<p>A<br>B</p>"));
        assert!(out.contains("<p>C</p>"));
    }

    #[test]
    fn find_body_tag_end_handles_attributes_and_case() {
        let html = "<html><BODY class=\"x\">content</BODY></html>";
        let pos = find_body_tag_end(html).unwrap();
        assert_eq!(&html[pos..pos + 7], "content");
    }

    #[tokio::test]
    async fn list_mail_folders_parses_and_pages() {
        let server = MockServer::start().await;
        // First page advertises a next link; second page closes it out.
        Mock::given(method("GET"))
            .and(path("/me/mailFolders"))
            .and(query_param("$top", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "F1", "displayName": "Biljetter", "totalItemCount": 3, "unreadItemCount": 0 }
                ],
                "@odata.nextLink": format!("{}/me/mailFolders?page=2", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "F2", "displayName": "Kvitton", "totalItemCount": 7, "unreadItemCount": 2 }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let folders = list_mail_folders(&http, &server.uri(), "AT").await.unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].display_name, "Biljetter");
        assert_eq!(folders[0].total_item_count, Some(3));
        assert_eq!(folders[1].display_name, "Kvitton");
        assert_eq!(folders[1].unread_item_count, Some(2));
    }

    #[tokio::test]
    async fn create_mail_folder_posts_display_name() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/me/mailFolders"))
            .and(body_partial_json(
                serde_json::json!({ "displayName": "Biljetter" }),
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({ "id": "NEWF", "displayName": "Biljetter" })),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let folder = create_mail_folder(&http, &server.uri(), "AT", "Biljetter")
            .await
            .unwrap();
        assert_eq!(folder.id, "NEWF");
        assert_eq!(folder.display_name, "Biljetter");
    }

    #[tokio::test]
    async fn list_child_folders_hits_child_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/PARENT/childFolders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "C1", "displayName": "MKLab", "totalItemCount": 5, "unreadItemCount": 0, "childFolderCount": 0 }
                ]
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let children = list_child_folders(&http, &server.uri(), "AT", "PARENT")
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].display_name, "MKLab");
    }

    #[tokio::test]
    async fn create_child_folder_posts_to_parent() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/me/mailFolders/PARENT/childFolders"))
            .and(body_partial_json(
                serde_json::json!({ "displayName": "MKLab" }),
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({ "id": "C9", "displayName": "MKLab" })),
            )
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let folder = create_child_folder(&http, &server.uri(), "AT", "PARENT", "MKLab")
            .await
            .unwrap();
        assert_eq!(folder.id, "C9");
    }

    #[tokio::test]
    async fn delete_mail_folder_hits_delete_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/me/mailFolders/F1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        delete_mail_folder(&http, &server.uri(), "AT", "F1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn get_categories_parses_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/me/messages/.+$"))
            .and(query_param("$select", "categories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "categories": ["Receipts", "Urgent"]
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let cats = get_categories(&http, &server.uri(), "AT", "MSG")
            .await
            .unwrap();
        assert_eq!(cats, vec!["Receipts", "Urgent"]);
    }

    #[tokio::test]
    async fn set_categories_patches_array() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex(r"^/me/messages/.+$"))
            .and(body_partial_json(
                serde_json::json!({ "categories": ["receipt", "ticket"] }),
            ))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        set_categories(
            &http,
            &server.uri(),
            "AT",
            "MSG",
            &["receipt".to_string(), "ticket".to_string()],
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

    #[tokio::test]
    async fn fetch_message_headers_parses_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/me/messages/.+$"))
            .and(query_param("$select", "internetMessageHeaders"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "internetMessageHeaders": [
                    { "name": "List-Unsubscribe", "value": "<mailto:u@x>, <https://x/u>" },
                    { "name": "List-Unsubscribe-Post", "value": "List-Unsubscribe=One-Click" }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let headers = fetch_message_headers(&http, &server.uri(), "AT", "MSGID")
            .await
            .unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "List-Unsubscribe");
        assert_eq!(headers[0].1, "<mailto:u@x>, <https://x/u>");
        assert_eq!(headers[1].0, "List-Unsubscribe-Post");
    }
}

#[cfg(test)]
mod cursor_paging_tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn msg(id: &str, received: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "subject": format!("s-{id}"),
            "from": {"emailAddress": {"name": "N", "address": "n@x.se"}},
            "receivedDateTime": received,
            "isRead": true,
            "bodyPreview": "p",
            "hasAttachments": false
        })
    }

    #[tokio::test]
    async fn next_link_is_surfaced_and_followable() {
        let server = MockServer::start().await;
        let page2_url = format!("{}/me/mailFolders/inbox/messages?page=2", server.uri());
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [msg("m3", "2026-07-01T10:00:00Z")]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [msg("m1", "2026-07-03T10:00:00Z"), msg("m2", "2026-07-02T10:00:00Z")],
                "@odata.nextLink": page2_url
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let page1 = list_inbox(&http, &server.uri(), "tok", "a@b.se", 2, 0, false)
            .await
            .unwrap();
        assert_eq!(page1.messages.len(), 2);
        let next = page1.next_link.expect("first page links onward");

        let page2 = list_messages_at(&http, "tok", "a@b.se", &next)
            .await
            .unwrap();
        assert_eq!(page2.messages.len(), 1);
        assert_eq!(page2.messages[0].id, "m3");
        assert!(page2.next_link.is_none(), "final page has no continuation");
    }
}
