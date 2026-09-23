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
//!
//! The OS keychain can't be enumerated, so [`save`](McpTokenStore::save) and
//! [`delete`](McpTokenStore::delete) also maintain a small index —
//! `${XDG_CONFIG_HOME:-~/.config}/pidge/mcp/servers.json`, mode 0600 — of
//! every server this user has a stored session with, so `pidge mcp status`
//! (with no url) and similar commands can discover what's connected without
//! the caller having to already know a url. [`list`](McpTokenStore::list)
//! reads it back.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use pidge_core::TokenStorage;

use super::{McpTokens, normalize_origin};
use crate::auth::file_store::write_private;
use crate::error::ClientError;

const SERVICE_NAME: &str = "pidge-mcp";

/// One entry in the on-disk server index (see the module docs). `server` is
/// always the normalized origin (`scheme://host[:port]`), regardless of
/// what form the url was in when it was signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredServer {
    pub server: String,
    pub storage: TokenStorage,
}

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

    /// Save `tokens`, overwriting any existing entry for its server, and
    /// best-effort upsert the server index so it can be discovered by
    /// [`Self::list`]. The tokens are the source of truth for whether this
    /// call succeeded — a failure to update the index (e.g. a permissions
    /// problem, a concurrent write) is logged and swallowed rather than
    /// failing a token write that already landed.
    pub fn save(tokens: &McpTokens, storage: TokenStorage) -> Result<(), ClientError> {
        match storage {
            TokenStorage::Keychain => Self::save_keychain(tokens)?,
            TokenStorage::File => Self::save_file(tokens)?,
        }
        if let Err(e) = Self::upsert_index(&tokens.server, storage) {
            tracing::warn!("could not update the MCP server index after saving tokens: {e}");
        }
        Ok(())
    }

    /// Remove the stored session for `server_url` and its index entry.
    /// No-op if none exists. As with [`Self::save`], the token deletion is
    /// authoritative — a failure to update the index afterwards is logged,
    /// not propagated.
    pub fn delete(server_url: &str, storage: TokenStorage) -> Result<(), ClientError> {
        match storage {
            TokenStorage::Keychain => Self::delete_keychain(server_url)?,
            TokenStorage::File => Self::delete_file(server_url)?,
        }
        if let Err(e) = Self::remove_from_index(server_url) {
            tracing::warn!("could not update the MCP server index after deleting tokens: {e}");
        }
        Ok(())
    }

    /// Every server this user has a stored session with, per the on-disk
    /// index. Empty (not an error) if the index file doesn't exist yet —
    /// e.g. before the first `pidge mcp connect` — or if it exists but
    /// can't be parsed. The index is only a discovery aid (the keychain and
    /// token files remain the source of truth for what's actually signed
    /// in), so a corrupt file must never block `status`, `connect`, or a
    /// token refresh; it's logged and treated as empty. The next
    /// [`Self::save`] then rewrites it from scratch, which does mean a
    /// corrupt index silently drops any *other* servers it used to list —
    /// an acceptable trade for never wedging sign-in on a damaged file that
    /// hand-editing (or a crash mid-write) could produce.
    pub fn list() -> Result<Vec<StoredServer>, ClientError> {
        let path = Self::servers_index_path()?;
        match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(entries) => Ok(entries),
                Err(e) => {
                    tracing::warn!(
                        "ignoring corrupt MCP server index at {}: {e}",
                        path.display()
                    );
                    Ok(Vec::new())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    // --- server index ---------------------------------------------------

    fn servers_index_path() -> Result<PathBuf, ClientError> {
        Ok(Self::dir()?.join("servers.json"))
    }

    fn upsert_index(server_url: &str, storage: TokenStorage) -> Result<(), ClientError> {
        let origin = normalize_origin(server_url)?;
        let mut entries = Self::list()?;
        entries.retain(|e| e.server != origin);
        entries.push(StoredServer {
            server: origin,
            storage,
        });
        write_private(
            &Self::servers_index_path()?,
            &serde_json::to_string_pretty(&entries)?,
        )?;
        Ok(())
    }

    fn remove_from_index(server_url: &str) -> Result<(), ClientError> {
        let origin = normalize_origin(server_url)?;
        let mut entries = Self::list()?;
        let before = entries.len();
        entries.retain(|e| e.server != origin);
        if entries.len() != before {
            write_private(
                &Self::servers_index_path()?,
                &serde_json::to_string_pretty(&entries)?,
            )?;
        }
        Ok(())
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
        let dir = crate::base_config_dir()?.join("pidge").join("mcp");
        std::fs::create_dir_all(&dir)?;
        // `create_dir_all` leaves the default umask (typically 0755) —
        // tighten it to user-only. The token and index files inside are
        // already 0600, so this closes only the smaller exposure of the
        // directory listing itself revealing which hosts are connected.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
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

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn with_temp_config_dir<F: FnOnce()>(f: F) {
        let tmp = tempfile::tempdir().unwrap();
        crate::test_support::with_base_dir(tmp.path(), f);
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

    // --- server index -----------------------------------------------------

    #[test]
    fn list_is_empty_before_anything_is_saved() {
        with_temp_config_dir(|| {
            assert!(McpTokenStore::list().unwrap().is_empty());
        });
    }

    #[test]
    fn save_adds_the_server_to_the_index_normalized_and_with_its_backend() {
        with_temp_config_dir(|| {
            let tokens = fake_tokens("https://mcp.example.com/mcp");
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();

            let servers = McpTokenStore::list().unwrap();
            assert_eq!(
                servers,
                vec![StoredServer {
                    server: "https://mcp.example.com".to_string(),
                    storage: TokenStorage::File,
                }]
            );
        });
    }

    #[test]
    fn saving_the_same_server_again_upserts_rather_than_duplicates() {
        with_temp_config_dir(|| {
            let tokens = fake_tokens("https://mcp.example.com/mcp");
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();
            // A refreshed token gets re-saved under the same backend on
            // every use (see `RefreshingRpc`) — this must not accumulate a
            // second index entry for the same server.
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();

            let servers = McpTokenStore::list().unwrap();
            assert_eq!(servers.len(), 1, "{servers:?}");
            assert_eq!(servers[0].storage, TokenStorage::File);
        });
    }

    #[test]
    fn saving_two_servers_lists_both() {
        with_temp_config_dir(|| {
            McpTokenStore::save(
                &fake_tokens("https://a.example.com/mcp"),
                TokenStorage::File,
            )
            .unwrap();
            McpTokenStore::save(
                &fake_tokens("https://b.example.com/mcp"),
                TokenStorage::File,
            )
            .unwrap();

            let mut servers: Vec<String> = McpTokenStore::list()
                .unwrap()
                .into_iter()
                .map(|s| s.server)
                .collect();
            servers.sort();
            assert_eq!(
                servers,
                vec![
                    "https://a.example.com".to_string(),
                    "https://b.example.com".to_string()
                ]
            );
        });
    }

    #[test]
    fn delete_removes_the_server_from_the_index() {
        with_temp_config_dir(|| {
            let server = "https://mcp.example.com/mcp";
            McpTokenStore::save(&fake_tokens(server), TokenStorage::File).unwrap();
            assert_eq!(McpTokenStore::list().unwrap().len(), 1);

            McpTokenStore::delete(server, TokenStorage::File).unwrap();
            assert!(McpTokenStore::list().unwrap().is_empty());
        });
    }

    #[test]
    fn delete_of_unknown_server_leaves_the_index_untouched() {
        with_temp_config_dir(|| {
            McpTokenStore::save(
                &fake_tokens("https://a.example.com/mcp"),
                TokenStorage::File,
            )
            .unwrap();

            McpTokenStore::delete("https://nowhere.example.com/mcp", TokenStorage::File).unwrap();

            assert_eq!(McpTokenStore::list().unwrap().len(), 1);
        });
    }

    #[cfg(unix)]
    #[test]
    fn servers_index_is_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_config_dir(|| {
            McpTokenStore::save(
                &fake_tokens("https://mode.example.com/mcp"),
                TokenStorage::File,
            )
            .unwrap();
            let path = McpTokenStore::servers_index_path().unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the server index must be user-only readable");
        });
    }

    #[cfg(unix)]
    #[test]
    fn mcp_dir_is_mode_0700_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_config_dir(|| {
            McpTokenStore::save(
                &fake_tokens("https://mode.example.com/mcp"),
                TokenStorage::File,
            )
            .unwrap();
            let dir = McpTokenStore::dir().unwrap();
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o700,
                "the mcp directory must not be listable by anyone else"
            );
        });
    }

    // --- corrupt index (I1) ------------------------------------------------

    #[test]
    fn list_treats_a_corrupt_index_file_as_empty_instead_of_erroring() {
        with_temp_config_dir(|| {
            let path = McpTokenStore::servers_index_path().unwrap();
            std::fs::write(&path, "not valid json").unwrap();

            assert_eq!(McpTokenStore::list().unwrap(), Vec::new());
        });
    }

    #[test]
    fn save_succeeds_and_repairs_a_corrupt_index() {
        with_temp_config_dir(|| {
            let path = McpTokenStore::servers_index_path().unwrap();
            std::fs::write(&path, "not valid json").unwrap();

            let tokens = fake_tokens("https://mcp.example.com/mcp");
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();

            // The token write itself must not fail, and the index is now
            // readable again (even though the corrupt file meant any other
            // servers it might have listed were lost — see `list`'s docs).
            assert_eq!(
                McpTokenStore::load(&tokens.server, TokenStorage::File)
                    .unwrap()
                    .unwrap(),
                tokens
            );
            assert_eq!(
                McpTokenStore::list().unwrap(),
                vec![StoredServer {
                    server: "https://mcp.example.com".to_string(),
                    storage: TokenStorage::File,
                }]
            );
        });
    }

    #[test]
    fn delete_succeeds_even_with_a_corrupt_index() {
        with_temp_config_dir(|| {
            let tokens = fake_tokens("https://mcp.example.com/mcp");
            McpTokenStore::save(&tokens, TokenStorage::File).unwrap();

            let path = McpTokenStore::servers_index_path().unwrap();
            std::fs::write(&path, "not valid json").unwrap();

            // The token deletion must succeed regardless of the index being
            // unreadable.
            McpTokenStore::delete(&tokens.server, TokenStorage::File).unwrap();
            assert!(
                McpTokenStore::load(&tokens.server, TokenStorage::File)
                    .unwrap()
                    .is_none()
            );
        });
    }
}
