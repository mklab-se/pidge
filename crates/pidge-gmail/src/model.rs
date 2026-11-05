//! Gmail's JSON shapes and their mapping onto pidge's provider-neutral types.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use pidge_core::MessageFrom;
use pidge_core::provider::{BulkHint, Folder, MailItem};
use serde::Deserialize;

/// The metadata headers every list/sync fetch asks for.
pub(crate) const METADATA_HEADERS: &[&str] =
    &["From", "To", "Cc", "Subject", "Date", "List-Unsubscribe"];

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmailMessage {
    pub id: String,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: String,
    /// Milliseconds since the epoch, as a string.
    #[serde(default)]
    pub internal_date: Option<String>,
    #[serde(default)]
    pub payload: Option<Part>,
    /// `format=raw`: the whole RFC 5322 message, base64url.
    #[serde(default)]
    pub raw: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: Option<PartBody>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartBody {
    #[serde(default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Header {
    pub name: String,
    pub value: String,
}

impl Part {
    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }
}

impl GmailMessage {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.payload.as_ref().and_then(|p| p.header(name))
    }

    pub fn received_at(&self) -> DateTime<Utc> {
        self.internal_date
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"))
    }

    pub fn has_label(&self, label: &str) -> bool {
        self.label_ids.iter().any(|l| l == label)
    }
}

/// A label as `labels.list` / `labels.get` return it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Label {
    pub id: String,
    pub name: String,
    /// `system` or `user`.
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub messages_total: Option<u32>,
    #[serde(default)]
    pub messages_unread: Option<u32>,
}

impl Label {
    pub fn is_user(&self) -> bool {
        self.kind == "user"
    }
}

/// Which pidge folder a message is in, from its labels. Trash and spam
/// win (a trashed message can keep other labels), then inbox, sent and
/// drafts; a message with none of those is archived. User labels do not
/// make a folder: they are categories.
pub(crate) fn folder_from_labels(labels: &[String]) -> Folder {
    let has = |l: &str| labels.iter().any(|x| x == l);
    if has("TRASH") {
        Folder::Deleted
    } else if has("SPAM") {
        Folder::Junk
    } else if has("INBOX") {
        Folder::Inbox
    } else if has("SENT") {
        Folder::Sent
    } else if has("DRAFT") {
        Folder::Drafts
    } else {
        Folder::Archive
    }
}

/// Gmail's inbox category tabs as pidge's bulk hint.
pub(crate) fn bulk_hint_from_labels(labels: &[String]) -> Option<BulkHint> {
    labels.iter().find_map(|l| match l.as_str() {
        "CATEGORY_PROMOTIONS" => Some(BulkHint::Promotions),
        "CATEGORY_SOCIAL" => Some(BulkHint::Social),
        "CATEGORY_UPDATES" => Some(BulkHint::Updates),
        "CATEGORY_FORUMS" => Some(BulkHint::Forums),
        "CATEGORY_PERSONAL" => Some(BulkHint::Primary),
        _ => None,
    })
}

/// Decode the HTML entities Gmail leaves in snippets.
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let semicolon = rest
            .char_indices()
            .take(12)
            .find(|&(_, c)| c == ';')
            .map(|(i, _)| i);
        let Some(end) = semicolon else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse an address-list header value (`"Jane" <jane@x>, bob@y`), RFC 2047
/// words included, by letting mail-parser read it as a header.
pub(crate) fn parse_addresses(value: &str) -> Vec<MessageFrom> {
    let clean: String = value
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect();
    let raw = format!("To: {clean}\r\n\r\n");
    let Some(msg) = mail_parser::MessageParser::default().parse(raw.as_bytes()) else {
        return Vec::new();
    };
    msg.to().map(addresses_of).unwrap_or_default()
}

/// mail-parser's address (list or groups) as pidge's.
pub(crate) fn addresses_of(a: &mail_parser::Address<'_>) -> Vec<MessageFrom> {
    a.iter()
        .filter_map(|addr| {
            let address = addr.address()?.trim().to_string();
            if address.is_empty() {
                return None;
            }
            Some(MessageFrom {
                name: addr.name().unwrap_or("").trim().to_string(),
                address,
            })
        })
        .collect()
}

/// A message fetched with `format=metadata` as a [`MailItem`]. `labels` maps
/// label ids to labels (for category names).
///
/// `has_attachments` is a heuristic: metadata carries no part list, but a
/// message with attachments is almost always `multipart/mixed` at the top.
/// Mail with only inline images (`multipart/related`) reads as no
/// attachments, and a rare `multipart/mixed` with no real attachment reads
/// as one. `is_invite` is always false here (the part list is needed).
pub(crate) fn mail_item(
    account: &str,
    m: &GmailMessage,
    labels: &HashMap<String, Label>,
) -> MailItem {
    let header_addrs = |name: &str| m.header(name).map(parse_addresses).unwrap_or_default();
    let from = header_addrs("From")
        .into_iter()
        .next()
        .unwrap_or(MessageFrom {
            name: String::new(),
            address: String::new(),
        });
    let mut categories: Vec<String> = m
        .label_ids
        .iter()
        .filter_map(|id| labels.get(id))
        .filter(|l| l.is_user())
        .map(|l| l.name.clone())
        .collect();
    categories.sort();
    MailItem {
        account: account.to_string(),
        id: m.id.clone(),
        thread_id: m.thread_id.clone(),
        folder: folder_from_labels(&m.label_ids),
        from,
        to: header_addrs("To"),
        cc: header_addrs("Cc"),
        subject: decode_subject(m.header("Subject").unwrap_or("")),
        received_at: m.received_at(),
        preview: decode_entities(&m.snippet),
        unread: m.has_label("UNREAD"),
        flagged: m.has_label("STARRED"),
        has_attachments: m
            .payload
            .as_ref()
            .is_some_and(|p| p.mime_type.eq_ignore_ascii_case("multipart/mixed")),
        categories,
        bulk_hint: bulk_hint_from_labels(&m.label_ids),
        list_unsubscribe: m
            .header("List-Unsubscribe")
            .is_some_and(|v| !v.trim().is_empty()),
        is_invite: false,
    }
}

/// Gmail's metadata headers are raw: decode RFC 2047 words in a subject.
fn decode_subject(value: &str) -> String {
    if !value.contains("=?") {
        return value.trim().to_string();
    }
    let raw = format!("Subject: {}\r\n\r\n", value.replace(['\r', '\n'], " "));
    mail_parser::MessageParser::default()
        .parse(raw.as_bytes())
        .and_then(|m| m.subject().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| value.trim().to_string())
}

/// Gmail's `labelIds` and payload for a set of id-only list entries.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageList {
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageRef {
    pub id: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryList {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub history_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryRecord {
    #[serde(default)]
    pub messages_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub messages_deleted: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_removed: Vec<HistoryMessage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryMessage {
    pub message: MessageRef,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Profile {
    #[serde(default)]
    pub history_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(ls: &[&str]) -> Vec<String> {
        ls.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn folders_follow_system_labels() {
        assert_eq!(
            folder_from_labels(&labels(&["INBOX", "UNREAD"])),
            Folder::Inbox
        );
        assert_eq!(
            folder_from_labels(&labels(&["SENT", "Label_1"])),
            Folder::Sent
        );
        assert_eq!(folder_from_labels(&labels(&["DRAFT"])), Folder::Drafts);
        assert_eq!(
            folder_from_labels(&labels(&["TRASH", "INBOX"])),
            Folder::Deleted
        );
        assert_eq!(folder_from_labels(&labels(&["SPAM"])), Folder::Junk);
        assert_eq!(
            folder_from_labels(&labels(&["Label_1", "IMPORTANT"])),
            Folder::Archive
        );
        assert_eq!(folder_from_labels(&[]), Folder::Archive);
    }

    #[test]
    fn category_tabs_become_bulk_hints() {
        assert_eq!(
            bulk_hint_from_labels(&labels(&["INBOX", "CATEGORY_PROMOTIONS"])),
            Some(BulkHint::Promotions)
        );
        assert_eq!(
            bulk_hint_from_labels(&labels(&["CATEGORY_SOCIAL"])),
            Some(BulkHint::Social)
        );
        assert_eq!(
            bulk_hint_from_labels(&labels(&["CATEGORY_UPDATES"])),
            Some(BulkHint::Updates)
        );
        assert_eq!(
            bulk_hint_from_labels(&labels(&["CATEGORY_FORUMS"])),
            Some(BulkHint::Forums)
        );
        assert_eq!(
            bulk_hint_from_labels(&labels(&["CATEGORY_PERSONAL"])),
            Some(BulkHint::Primary)
        );
        assert_eq!(bulk_hint_from_labels(&labels(&["INBOX"])), None);
    }

    #[test]
    fn snippet_entities_are_decoded() {
        assert_eq!(
            decode_entities("Tom &amp; Jerry &lt;3 &#39;hi&#39; &quot;x&quot; &#x41; & more"),
            "Tom & Jerry <3 'hi' \"x\" A & more"
        );
        assert_eq!(decode_entities("&bogus; &"), "&bogus; &");
    }

    #[test]
    fn address_headers_parse_with_names_and_encoded_words() {
        let a = parse_addresses(r#""Doe, Jane" <jane@example.com>, bob@example.com"#);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "Doe, Jane");
        assert_eq!(a[0].address, "jane@example.com");
        assert_eq!(a[1].address, "bob@example.com");
        let b = parse_addresses("=?UTF-8?B?SsO2cmFu?= <jo@example.com>");
        assert_eq!(b[0].name, "J\u{f6}ran");
    }

    #[test]
    fn subjects_decode_encoded_words() {
        assert_eq!(decode_subject("=?UTF-8?Q?Hej_d=C3=A5?="), "Hej d\u{e5}");
        assert_eq!(decode_subject(" plain "), "plain");
    }
}
