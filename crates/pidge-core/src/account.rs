//! Account types — represents a single Microsoft account signed into pidge.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A signed-in Microsoft account.
///
/// This is metadata only — no tokens. Tokens live in the OS keychain,
/// keyed by `email`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub tenant_id: String,
    pub home_account_id: String,
    pub added_at: DateTime<Utc>,
}

impl Account {
    /// Well-known tenant ID for personal Microsoft accounts (outlook.com, live.com, hotmail.com).
    /// Microsoft documents this as the "MSA" tenant.
    pub const PERSONAL_MSA_TENANT: &'static str = "9188040d-6c67-4c5b-b112-36a304b66dad";

    /// True if this account is a personal Microsoft account.
    pub fn is_personal(&self) -> bool {
        self.tenant_id == Self::PERSONAL_MSA_TENANT
    }

    /// A short human label for the tenant — "personal MSA" for MSA, GUID prefix otherwise.
    pub fn tenant_label(&self) -> String {
        if self.is_personal() {
            "personal MSA".to_string()
        } else {
            let prefix: String = self.tenant_id.chars().take(8).collect();
            format!("{prefix}…")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(tenant_id: &str) -> Account {
        Account {
            email: "x@example.com".into(),
            tenant_id: tenant_id.into(),
            home_account_id: "home".into(),
            added_at: DateTime::parse_from_rfc3339("2026-05-13T22:00:00Z")
                .unwrap()
                .to_utc(),
        }
    }

    #[test]
    fn personal_msa_tenant_is_recognised() {
        assert!(make_account(Account::PERSONAL_MSA_TENANT).is_personal());
    }

    #[test]
    fn org_tenant_is_not_personal() {
        assert!(!make_account("11111111-2222-3333-4444-555555555555").is_personal());
    }

    #[test]
    fn tenant_label_for_msa() {
        assert_eq!(
            make_account(Account::PERSONAL_MSA_TENANT).tenant_label(),
            "personal MSA"
        );
    }

    #[test]
    fn tenant_label_for_org_truncates_to_8_chars() {
        assert_eq!(
            make_account("11111111-2222-3333-4444-555555555555").tenant_label(),
            "11111111…"
        );
    }
}
