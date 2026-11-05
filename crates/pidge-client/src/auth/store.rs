//! OS keychain access for OAuth tokens.
//!
//! Service name: "pidge". Account name: the user's email.
//! The value is a JSON-serialized `TokenSet`.

use std::time::Duration;

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

const SERVICE_NAME: &str = "pidge";

/// How long pidge waits for the keychain, which on macOS may be waiting for
/// the user to answer a dialog nobody is looking at.
const KEYCHAIN_PATIENCE: Duration = Duration::from_secs(120);

/// Run `work` on its own thread and stop waiting after `patience`. `None`
/// means it did not finish; the thread is left to end by itself.
fn within<T: Send + 'static>(
    patience: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(patience).ok()
}

pub struct KeychainStore;

impl KeychainStore {
    fn entry(email: &str) -> Result<keyring::Entry, ClientError> {
        keyring::Entry::new(SERVICE_NAME, email).map_err(ClientError::Keychain)
    }

    /// Load tokens for an email. Returns `None` if there's no entry for that account.
    pub fn load(email: &str) -> Result<Option<TokenSet>, ClientError> {
        let entry = Self::entry(email)?;
        let answer = within(KEYCHAIN_PATIENCE, move || entry.get_password()).ok_or_else(|| {
            ClientError::KeychainTimeout {
                email: email.to_string(),
            }
        })?;
        match answer {
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
        within(KEYCHAIN_PATIENCE, move || entry.set_password(&blob))
            .ok_or_else(|| ClientError::KeychainTimeout {
                email: email.to_string(),
            })?
            .map_err(ClientError::Keychain)
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
    fn slow_work_is_given_up_on() {
        let patience = std::time::Duration::from_millis(20);
        let slow = || std::thread::sleep(std::time::Duration::from_secs(5));
        let started = std::time::Instant::now();

        assert_eq!(within(patience, slow), None);

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn work_in_time_returns_its_result() {
        assert_eq!(within(std::time::Duration::from_secs(5), || 7), Some(7));
    }

    #[test]
    fn token_blob_serializes_and_deserializes() {
        let t = TokenSet {
            access_token: "abc".into(),
            refresh_token: "xyz".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
            scope: None,
        };
        let blob = serde_json::to_string(&t).unwrap();
        let t2: TokenSet = serde_json::from_str(&blob).unwrap();
        assert_eq!(t, t2);
    }
}
