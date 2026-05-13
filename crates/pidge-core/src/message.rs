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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFrom {
    pub name: String,
    pub address: String,
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
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(m, m2);
    }
}
