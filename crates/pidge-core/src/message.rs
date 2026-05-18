//! Normalized message representation, provider-agnostic.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One message in a mailbox, as presented to commands and output formatters.
///
/// `account` is the email of the signed-in account this message belongs to,
/// so multi-account merges can show provenance in a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub account: String,
    pub id: String,
    pub from: MessageFrom,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub is_read: bool,
    pub preview: String,
    /// Outlook follow-up flag. Defaults to `NotFlagged` so old cache entries
    /// and providers that don't expose flags deserialize cleanly.
    #[serde(default)]
    pub flag_status: FlagStatus,
}

/// Outlook's follow-up flag state on a message. Three values to match what
/// Graph returns; the CLI currently only sets `Flagged` / `NotFlagged`, but
/// it reads and displays all three so a flag a user completed in Outlook
/// shows up correctly here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlagStatus {
    #[default]
    NotFlagged,
    Flagged,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFrom {
    pub name: String,
    pub address: String,
}

/// Full message content as returned by `GraphClient::get_message`.
/// Compared to `Message` (the list-row shape), this carries full body,
/// all recipient lists, sent/received timestamps, and a `has_attachments`
/// flag for triggering the attachment fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullMessage {
    pub account: String,
    pub id: String,
    pub from: MessageFrom,
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    pub bcc: Vec<MessageFrom>,
    pub subject: String,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub sent_at: chrono::DateTime<chrono::Utc>,
    pub is_read: bool,
    pub body_content_type: BodyContentType,
    pub body_content: String,
    pub has_attachments: bool,
    #[serde(default)]
    pub flag_status: FlagStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyContentType {
    Text,
    Html,
}

/// An attachment listed on a message. Bytes are NOT included — fetch separately
/// via `GraphClient::get_attachment_bytes(account, message_id, attachment.id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Provider-specific identifier (Microsoft Graph attachment id).
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub is_inline: bool,
    pub content_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrips_through_json() {
        let m = Message {
            account: "a@b.com".into(),
            id: "id-1".into(),
            from: MessageFrom {
                name: "Maria Lindberg".into(),
                address: "maria@mklab.se".into(),
            },
            subject: "Quarterly numbers".into(),
            received_at: DateTime::parse_from_rfc3339("2026-05-13T22:00:00Z")
                .unwrap()
                .to_utc(),
            is_read: false,
            preview: "Hi, attached are…".into(),
            flag_status: FlagStatus::NotFlagged,
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn full_message_roundtrips_through_json() {
        let m = FullMessage {
            account: "u@e.com".into(),
            id: "graph-id".into(),
            from: MessageFrom {
                name: "Maria".into(),
                address: "maria@mklab.se".into(),
            },
            to: vec![MessageFrom {
                name: "Kristofer".into(),
                address: "kristofer@mklab.se".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            received_at: chrono::DateTime::parse_from_rfc3339("2026-05-14T22:00:00Z")
                .unwrap()
                .to_utc(),
            sent_at: chrono::DateTime::parse_from_rfc3339("2026-05-14T21:59:30Z")
                .unwrap()
                .to_utc(),
            is_read: false,
            body_content_type: BodyContentType::Html,
            body_content: "<p>Hello</p>".into(),
            has_attachments: true,
            flag_status: FlagStatus::Flagged,
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: FullMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn attachment_roundtrips_through_json() {
        let a = Attachment {
            id: "att-1".into(),
            name: "report.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 12345,
            is_inline: false,
            content_id: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        let a2: Attachment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, a2);
    }

    #[test]
    fn body_content_type_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&BodyContentType::Html).unwrap(),
            "\"html\""
        );
        assert_eq!(
            serde_json::to_string(&BodyContentType::Text).unwrap(),
            "\"text\""
        );
    }
}
