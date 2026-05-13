//! OS keychain access for OAuth tokens.
//!
//! Service name: "pidge". Account name: the user's email.
//! The value is a JSON-serialized `TokenSet`.

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

const SERVICE_NAME: &str = "pidge";

pub struct KeychainStore;

impl KeychainStore {
    fn entry(email: &str) -> Result<keyring::Entry, ClientError> {
        keyring::Entry::new(SERVICE_NAME, email).map_err(ClientError::Keychain)
    }

    /// Load tokens for an email. Returns `None` if there's no entry for that account.
    pub fn load(email: &str) -> Result<Option<TokenSet>, ClientError> {
        let entry = Self::entry(email)?;
        match entry.get_password() {
            Ok(blob) => {
                let tokens: TokenSet = serde_json::from_str(&blob)?;
                Ok(Some(tokens))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ClientError::Keychain(e)),
        }
    }

    /// Save tokens for an email, overwriting any existing entry.
    pub fn save(email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        let blob = serde_json::to_string(tokens)?;
        let entry = Self::entry(email)?;
        entry.set_password(&blob).map_err(ClientError::Keychain)
    }

    /// Remove tokens for an email. No-op if no entry exists.
    pub fn delete(email: &str) -> Result<(), ClientError> {
        let entry = Self::entry(email)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ClientError::Keychain(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    // Keychain tests are platform-dependent and require credential-store backends.
    // We trust the `keyring` crate's own integration tests for backend correctness
    // and limit ourselves to a serialization-only test that doesn't touch the OS.

    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn token_blob_serializes_and_deserializes() {
        let t = TokenSet {
            access_token: "abc".into(),
            refresh_token: "xyz".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        };
        let blob = serde_json::to_string(&t).unwrap();
        let t2: TokenSet = serde_json::from_str(&blob).unwrap();
        assert_eq!(t, t2);
    }
}
