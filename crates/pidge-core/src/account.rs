//! Account types — represents a single Microsoft account signed into pidge.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where pidge stores OAuth tokens for an account.
///
/// `Keychain` is the OS-native credential store (macOS Keychain, Windows
/// Credential Manager, Linux libsecret) — encrypted, OS-managed access control.
/// `File` is a JSON file at `~/.config/pidge/tokens/<email>.json` with mode 0600
/// (user-only read/write). The file backend is useful for headless or dev
/// scenarios where the keychain prompts are friction, but it stores refresh
/// tokens in plaintext on disk and is less safe than the keychain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenStorage {
    #[default]
    Keychain,
    File,
}

/// A signed-in Microsoft account.
///
/// This is metadata only — no tokens. Tokens live in the OS keychain
/// or in a per-account file at `~/.config/pidge/tokens/<email>.json`,
/// keyed by `email`. The `storage` field tells pidge which backend to look in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub tenant_id: String,
    pub home_account_id: String,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub storage: TokenStorage,
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

    /// Short human-readable label for the account's provider / kind. Today
    /// we only support Microsoft, so this distinguishes personal MSA (Outlook
    /// / Live / Hotmail) from organizational M365 tenants. As we add Gmail,
    /// IMAP, etc., this becomes a stored field rather than a derived one.
    pub fn provider_label(&self) -> &'static str {
        if self.is_personal() {
            "Outlook"
        } else {
            "M365"
        }
    }

    /// Machine-readable provider identifier — used in `--json` output and any
    /// future config-file lookups. Stays stable as labels change.
    pub fn provider_id(&self) -> &'static str {
        if self.is_personal() {
            "outlook"
        } else {
            "m365"
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
            storage: TokenStorage::default(),
        }
    }

    #[test]
    fn token_storage_default_is_keychain() {
        assert!(matches!(TokenStorage::default(), TokenStorage::Keychain));
    }

    #[test]
    fn token_storage_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&TokenStorage::Keychain).unwrap(),
            "\"keychain\""
        );
        assert_eq!(
            serde_json::to_string(&TokenStorage::File).unwrap(),
            "\"file\""
        );
    }

    #[test]
    fn account_without_storage_field_deserializes_as_keychain() {
        let yaml = r#"
email: a@b.com
tenant_id: tid
home_account_id: hid
added_at: "2026-05-13T22:00:00Z"
"#;
        let a: Account = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(a.storage, TokenStorage::Keychain));
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

    #[test]
    fn provider_label_for_personal_msa_is_outlook() {
        let a = make_account(Account::PERSONAL_MSA_TENANT);
        assert_eq!(a.provider_label(), "Outlook");
        assert_eq!(a.provider_id(), "outlook");
    }

    #[test]
    fn provider_label_for_org_tenant_is_m365() {
        let a = make_account("11111111-2222-3333-4444-555555555555");
        assert_eq!(a.provider_label(), "M365");
        assert_eq!(a.provider_id(), "m365");
    }
}
