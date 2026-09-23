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
        assert_eq!(name, "mailbox-jane-doe-tag-example-com");
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
