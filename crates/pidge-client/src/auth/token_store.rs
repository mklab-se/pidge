//! Backend-agnostic facade over [`KeychainStore`] and [`FileStore`].
//!
//! Callers pass a [`TokenStorage`] enum value (looked up from the account's
//! config entry) and this module dispatches to the right backend. Use this
//! type everywhere except in narrow keychain/file-specific code paths (e.g.
//! migrations that read from one backend and write to the other).

use pidge_core::TokenStorage;

use crate::auth::file_store::FileStore;
use crate::auth::store::KeychainStore;
use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

pub struct TokenStore;

impl TokenStore {
    /// Load tokens for an email from the specified backend.
    pub fn load(email: &str, storage: TokenStorage) -> Result<Option<TokenSet>, ClientError> {
        match storage {
            TokenStorage::Keychain => KeychainStore::load(email),
            TokenStorage::File => FileStore::load(email),
        }
    }

    /// Save tokens for an email to the specified backend, overwriting any existing entry.
    pub fn save(email: &str, tokens: &TokenSet, storage: TokenStorage) -> Result<(), ClientError> {
        match storage {
            TokenStorage::Keychain => KeychainStore::save(email, tokens),
            TokenStorage::File => FileStore::save(email, tokens),
        }
    }

    /// Remove tokens for an email from the specified backend. No-op if no entry exists.
    pub fn delete(email: &str, storage: TokenStorage) -> Result<(), ClientError> {
        match storage {
            TokenStorage::Keychain => KeychainStore::delete(email),
            TokenStorage::File => FileStore::delete(email),
        }
    }
}
