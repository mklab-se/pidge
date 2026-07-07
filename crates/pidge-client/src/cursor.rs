//! Opaque pagination/delta cursors for agents.
//!
//! A cursor wraps each account's Graph continuation URL (`@odata.nextLink`
//! or `@odata.deltaLink`) in a versioned, base64-encoded JSON envelope.
//! Agents treat it as opaque; pidge validates version and kind on decode.

use std::collections::BTreeMap;

use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("invalid cursor (not a pidge cursor)")]
    Malformed,

    #[error("cursor version {0} is not supported by this pidge build")]
    Version(u8),

    #[error("cursor is for '{actual}' but this command expects '{expected}'")]
    Kind { expected: String, actual: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub v: u8,
    pub kind: String,
    /// account email -> continuation URL; `None` = that account is exhausted.
    pub per_account: BTreeMap<String, Option<String>>,
}

impl Cursor {
    pub fn new(kind: &str) -> Self {
        Self {
            v: 1,
            kind: kind.to_string(),
            per_account: BTreeMap::new(),
        }
    }

    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(self).expect("cursor serializes"))
    }

    pub fn decode(token: &str, expected_kind: &str) -> Result<Self, CursorError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token.trim())
            .map_err(|_| CursorError::Malformed)?;
        let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
        if cursor.v != 1 {
            return Err(CursorError::Version(cursor.v));
        }
        if cursor.kind != expected_kind {
            return Err(CursorError::Kind {
                expected: expected_kind.to_string(),
                actual: cursor.kind,
            });
        }
        Ok(cursor)
    }

    /// True when every account is exhausted (no further pages anywhere).
    pub fn exhausted(&self) -> bool {
        self.per_account.values().all(|v| v.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut cursor = Cursor::new("mail-list");
        cursor
            .per_account
            .insert("a@b.se".into(), Some("https://graph/next?x=1".into()));
        cursor.per_account.insert("c@d.se".into(), None);
        let token = cursor.encode();
        let back = Cursor::decode(&token, "mail-list").unwrap();
        assert_eq!(back, cursor);
        assert!(!back.exhausted());
    }

    #[test]
    fn rejects_garbage_wrong_version_wrong_kind() {
        assert!(matches!(
            Cursor::decode("not base64 at all!!", "mail-list"),
            Err(CursorError::Malformed)
        ));
        let mut wrong_version = Cursor::new("mail-list");
        wrong_version.v = 9;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&wrong_version).unwrap());
        assert!(matches!(
            Cursor::decode(&token, "mail-list"),
            Err(CursorError::Version(9))
        ));
        let cal = Cursor::new("cal-list").encode();
        assert!(matches!(
            Cursor::decode(&cal, "mail-list"),
            Err(CursorError::Kind { .. })
        ));
    }

    #[test]
    fn exhausted_when_all_none() {
        let mut cursor = Cursor::new("mail-delta");
        cursor.per_account.insert("a@b.se".into(), None);
        assert!(cursor.exhausted());
    }
}
