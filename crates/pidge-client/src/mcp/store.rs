//! Credential storage for [`super::McpTokens`].
//!
//! Mirrors `crate::auth::token_store` (which dispatches between
//! `crate::auth::store::KeychainStore` and `crate::auth::file_store::FileStore`
//! for Microsoft tokens), but keyed by MCP server origin instead of e-mail,
//! and under its own keychain service name / file subdirectory so the two
//! credential kinds never collide.
//!
//! - Keychain: service `"pidge-mcp"`, account = the server's normalized
//!   origin (`scheme://host[:port]`).
//! - File: `${XDG_CONFIG_HOME:-~/.config}/pidge/mcp/<host>.json`, where
//!   `<host>` is `host[:port]` with `:` replaced by `_` (mode 0600 on Unix).

use std::path::{Path, PathBuf};

use pidge_core::TokenStorage;

use super::{McpTokens, normalize_origin};
use crate::error::ClientError;

const SERVICE_NAME: &str = "pidge-mcp";

/// Persists [`McpTokens`] to the OS keychain or a plaintext file, matching
/// whichever [`TokenStorage`] backend the caller resolved for this server.
pub struct McpTokenStore;

impl McpTokenStore {
    /// Load the stored session for `server_url`, if any.
    pub fn load(server_url: &str, storage: TokenStorage) -> Result<Option<McpTokens>, ClientError> {
        match storage {
            TokenStorage::Keychain => Self::load_keychain(server_url),
            TokenStorage::File => Self::load_file(server_url),
        }
    }

    /// Save `tokens`, overwriting any existing entry for its server.
    pub fn save(tokens: &McpTokens, storage: TokenStorage) -> Result<(), ClientError> {
        match storage {
            TokenStorage::Keychain => Self::save_keychain(tokens),
            TokenStorage::File => Self::save_file(tokens),
        }
    }

    /// Remove the stored session for `server_url`. No-op if none exists.
    pub fn delete(server_url: &str, storage: TokenStorage) -> Result<(), ClientError> {
        match storage {
            TokenStorage::Keychain => Self::delete_keychain(server_url),
            TokenStorage::File => Self::delete_file(server_url),
        }
    }

    // --- keychain -----------------------------------------------------

    fn keychain_entry(server_url: &str) -> Result<keyring::Entry, ClientError> {
        let account = normalize_origin(server_url)?;
        keyring::Entry::new(SERVICE_NAME, &account).map_err(ClientError::Keychain)
    }

    fn load_keychain(server_url: &str) -> Result<Option<McpTokens>, ClientError> {
        let entry = Self::keychain_entry(server_url)?;
        match entry.get_password() {
            Ok(blob) => Ok(Some(serde_json::from_str(&blob)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ClientError::Keychain(e)),
        }
    }

    fn save_keychain(tokens: &McpTokens) -> Result<(), ClientError> {
        let entry = Self::keychain_entry(&tokens.server)?;
        let blob = serde_json::to_string(tokens)?;
        entry.set_password(&blob).map_err(ClientError::Keychain)
    }

    fn delete_keychain(server_url: &str) -> Result<(), ClientError> {
        let entry = Self::keychain_entry(server_url)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ClientError::Keychain(e)),
        }
    }

    // --- file -----------------------------------------------------------

    fn dir() -> Result<PathBuf, ClientError> {
        let dir = dirs::config_dir()
            .ok_or(ClientError::NoConfigDir)?
            .join("pidge")
            .join("mcp");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn path_for(server_url: &str) -> Result<PathBuf, ClientError> {
        let origin = normalize_origin(server_url)?;
        let host = origin.rsplit("://").next().unwrap_or(&origin);
        let filename = format!("{}.json", host.replace(':', "_"));
        Ok(Self::dir()?.join(filename))
    }

    fn load_file(server_url: &str) -> Result<Option<McpTokens>, ClientError> {
        let path = Self::path_for(server_url)?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save_file(tokens: &McpTokens) -> Result<(), ClientError> {
        let path = Self::path_for(&tokens.server)?;
        let json = serde_json::to_string_pretty(tokens)?;
        write_private(&path, &json)?;
        Ok(())
    }

    fn delete_file(server_url: &str) -> Result<(), ClientError> {
        let path = Self::path_for(server_url)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::{Duration, Utc};

    use super::*;

    // File-backend tests mutate process-wide env vars (HOME / XDG_CONFIG_HOME)
    // to point `dirs::config_dir()` at a tempdir, same approach as
    // `auth::file_store`'s tests — they must serialize themselves.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_config_dir<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: serialized by ENV_LOCK above; both vars are restored before
        // the guard drops.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
            std::env::set_var("HOME", tmp.path());
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    fn fake_tokens(server: &str) -> McpTokens {
        McpTokens {
            server: server.to_string(),
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
            client_id: "client-jwt".into(),
        }
    }

    #[test]
    fn file_store_round_trips_in_tempdir() {
        with_temp_config_dir(|| {
            let server = "https://mcp.example.com:8443/mcp";
            let tokens = fake_tokens(server);
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();

            let loaded = McpTokenStore::load(server, TokenStorage::File)
                .unwrap()
                .unwrap();
            assert_eq!(loaded, tokens);

            McpTokenStore::delete(server, TokenStorage::File).unwrap();
            assert!(
                McpTokenStore::load(server, TokenStorage::File)
                    .unwrap()
                    .is_none()
            );
        });
    }

    #[test]
    fn file_store_path_replaces_colon_in_host_port() {
        with_temp_config_dir(|| {
            let server = "https://mcp.example.com:8443/mcp";
            let path = McpTokenStore::path_for(server).unwrap();
            assert_eq!(
                path.file_name().unwrap().to_str().unwrap(),
                "mcp.example.com_8443.json"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn file_store_writes_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_config_dir(|| {
            let server = "https://mode.example.com/mcp";
            let tokens = fake_tokens(server);
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();
            let path = McpTokenStore::path_for(server).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "MCP tokens file must be user-only readable");
        });
    }

    #[test]
    fn file_store_load_returns_none_when_absent() {
        with_temp_config_dir(|| {
            assert!(
                McpTokenStore::load("https://nowhere.example.com/mcp", TokenStorage::File)
                    .unwrap()
                    .is_none()
            );
        });
    }
}
