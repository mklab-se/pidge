//! Token storage shape — what gets serialized into the keychain.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A user's OAuth tokens for one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}
