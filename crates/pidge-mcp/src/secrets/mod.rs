//! Secret storage: the token-signing key and one entry per connected mailbox.
//!
//! Secret *names* are the only thing the rest of the server knows about;
//! [`mailbox_secret_name`] derives a Key Vault-safe name from an e-mail
//! address so the same code runs against the file backend in development.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

mod file;
mod keyvault;

pub use file::FileSecrets;
pub use keyvault::KeyVaultSecrets;

pub const SIGNING_KEY_SECRET: &str = "jwt-signing-key";

/// A minimal secret store. Values are opaque strings; the caller decides
/// on the encoding.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Fetch the current value of a secret, `None` if it does not exist.
    async fn get(&self, name: &str) -> Result<Option<String>>;
    /// Create or overwrite a secret.
    async fn set(&self, name: &str, value: &str) -> Result<()>;
}

pub type SharedSecrets = Arc<dyn SecretStore>;

/// Key Vault secret names may only contain `[0-9a-zA-Z-]`, so every other
/// character of the e-mail is mapped to `-`. Collisions between two real
/// addresses that differ only in punctuation are theoretical for a
/// two-person allowlist, but the allowlist is the authority anyway: a
/// mailbox is only ever written after its owner signed in.
pub fn mailbox_secret_name(email: &str) -> String {
    format!(
        "{}-{}",
        legacy_mailbox_secret_name(email),
        &crate::users::user_hash_long(email)[..8]
    )
}

/// The name mailbox secrets had before the hash suffix: the address with
/// every non-alphanumeric character folded to `-`, so `jane.doe@` and
/// `jane-doe@` shared a name. Read as a fallback so existing deployments
/// keep their sessions; never written.
pub fn legacy_mailbox_secret_name(email: &str) -> String {
    let sanitized: String = email
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("mailbox-{sanitized}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_secret_name_is_key_vault_safe() {
        let name = mailbox_secret_name("Jane.Doe+tag@Example.com");
        assert!(
            name.starts_with("mailbox-jane-doe-tag-example-com-"),
            "{name}"
        );
        assert_eq!(name.len(), "mailbox-jane-doe-tag-example-com-".len() + 8);
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(name, mailbox_secret_name("jane.doe+tag@example.com"));
        assert_eq!(
            legacy_mailbox_secret_name("Jane.Doe+tag@Example.com"),
            "mailbox-jane-doe-tag-example-com"
        );
    }

    #[test]
    fn addresses_that_differ_only_in_punctuation_get_distinct_names() {
        let a = mailbox_secret_name("jane.doe@example.com");
        let b = mailbox_secret_name("jane-doe@example.com");
        let c = mailbox_secret_name("jane+doe@example.com");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
        // Which the legacy scheme did not.
        assert_eq!(
            legacy_mailbox_secret_name("jane.doe@example.com"),
            legacy_mailbox_secret_name("jane-doe@example.com")
        );
    }
}
