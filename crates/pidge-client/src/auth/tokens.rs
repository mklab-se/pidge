//! Token storage shape — what gets serialized into the keychain.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A user's OAuth tokens for one account.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

impl TokenSet {
    /// True if the access token is within 60 seconds of expiring (or already expired).
    /// We refresh before this threshold to absorb clock skew.
    pub fn needs_refresh(&self) -> bool {
        Utc::now() + Duration::seconds(60) >= self.expires_at
    }
}

/// Hand-written so `access_token`/`refresh_token` are never printed by an
/// incidental `{:?}` (a log line, a test failure message, …) — see
/// `crate::mcp::McpTokens`'s matching `Debug` impl.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_does_not_need_refresh() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        };
        assert!(!t.needs_refresh());
    }

    #[test]
    fn token_expiring_within_60s_needs_refresh() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now() + Duration::seconds(30),
        };
        assert!(t.needs_refresh());
    }

    #[test]
    fn already_expired_token_needs_refresh() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: Utc::now() - Duration::seconds(10),
        };
        assert!(t.needs_refresh());
    }

    #[test]
    fn tokens_roundtrip_through_json() {
        let t = TokenSet {
            access_token: "ey…".into(),
            refresh_token: "M.C5…".into(),
            expires_at: DateTime::parse_from_rfc3339("2026-05-13T23:00:00Z")
                .unwrap()
                .to_utc(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let t2: TokenSet = serde_json::from_str(&json).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn debug_redacts_both_tokens() {
        let t = TokenSet {
            access_token: "super-secret-access".into(),
            refresh_token: "super-secret-refresh".into(),
            expires_at: Utc::now(),
        };

        let debug = format!("{t:?}");

        assert!(!debug.contains("super-secret-access"));
        assert!(!debug.contains("super-secret-refresh"));
        assert!(debug.contains("<redacted>"));
    }
}
