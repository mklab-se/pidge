//! File-based fallback for OAuth tokens — an opt-in alternative to the OS keychain.
//!
//! Tokens are written as JSON at `${XDG_CONFIG_HOME:-~/.config}/pidge/tokens/<email>.json`
//! (the same parent directory as `config.yaml`). On Unix the file is created with mode 0600
//! so only the owning user can read or write it; on Windows we rely on the default ACL.
//!
//! This backend is less secure than [`crate::auth::store::KeychainStore`] — refresh tokens
//! sit in plaintext on disk. It exists so headless or repeated-build scenarios (where the
//! OS keychain prompts for approval on every binary hash change) stay usable. Choose this
//! deliberately via `pidge auth login --store=file`.

use std::path::{Path, PathBuf};

use crate::auth::tokens::TokenSet;
use crate::error::ClientError;

pub struct FileStore;

impl FileStore {
    fn dir() -> Result<PathBuf, ClientError> {
        let dir = crate::base_config_dir()?.join("pidge").join("tokens");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn path_for(email: &str) -> Result<PathBuf, ClientError> {
        Ok(Self::dir()?.join(safe_filename(email)))
    }

    /// Load tokens for an email. Returns `None` if the file doesn't exist.
    pub fn load(email: &str) -> Result<Option<TokenSet>, ClientError> {
        let path = Self::path_for(email)?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Save tokens for an email, overwriting any existing file. Creates the file
    /// with mode 0600 on Unix.
    pub fn save(email: &str, tokens: &TokenSet) -> Result<(), ClientError> {
        let path = Self::path_for(email)?;
        let json = serde_json::to_string_pretty(tokens)?;
        write_private(&path, &json)?;
        Ok(())
    }

    /// Remove tokens for an email. No-op if the file doesn't exist.
    pub fn delete(email: &str) -> Result<(), ClientError> {
        let path = Self::path_for(email)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Reduce an email to a filename-safe form. Keeps alphanumerics, `.`, `-`, `_`, `@`, `+`;
/// replaces anything else with `_`. Appends `.json`.
fn safe_filename(email: &str) -> String {
    let mut s: String = email
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.push_str(".json");
    s
}

/// Write `contents` to `path`, creating it if necessary, with mode 0600 on
/// Unix (a no-op permissions-wise on other platforms — we rely on the
/// default ACL there). Shared by [`FileStore`] and `crate::mcp::store`.
#[cfg(unix)]
pub(crate) fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `.mode(0o600)` only takes effect when the file is newly created (per
    // `open(2)`'s handling of the mode argument). Tighten permissions
    // explicitly so a pre-existing file with looser permissions — left over
    // from before this hardening, or created some other way — gets locked
    // down too, not just newly-created ones.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn with_temp_config_dir<F: FnOnce(&std::path::Path)>(f: F) {
        let tmp = tempfile::tempdir().unwrap();
        crate::test_support::with_base_dir(tmp.path(), || f(tmp.path()));
    }

    fn fake_tokens() -> TokenSet {
        TokenSet {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        }
    }

    #[test]
    fn safe_filename_keeps_typical_emails_intact() {
        assert_eq!(safe_filename("me@example.com"), "me@example.com.json");
        assert_eq!(
            safe_filename("first.last+tag@sub.example.co"),
            "first.last+tag@sub.example.co.json"
        );
    }

    #[test]
    fn safe_filename_replaces_unsafe_chars() {
        assert_eq!(safe_filename("a/b\\c:d?e"), "a_b_c_d_e.json");
    }

    #[test]
    fn save_load_delete_roundtrips_via_tmpdir() {
        with_temp_config_dir(|_| {
            let email = "test@example.com";
            let tokens = fake_tokens();
            FileStore::save(email, &tokens).unwrap();
            let loaded = FileStore::load(email).unwrap().unwrap();
            assert_eq!(loaded, tokens);
            FileStore::delete(email).unwrap();
            assert!(FileStore::load(email).unwrap().is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_has_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_config_dir(|_| {
            let email = "mode@example.com";
            let tokens = fake_tokens();
            FileStore::save(email, &tokens).unwrap();
            let path = FileStore::path_for(email).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "tokens file must be user-only readable");
            FileStore::delete(email).unwrap();
        });
    }

    #[cfg(unix)]
    #[test]
    fn write_private_tightens_pre_existing_looser_permissions() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_config_dir(|_| {
            let path = FileStore::path_for("loose@example.com").unwrap();
            std::fs::write(&path, "stale").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

            write_private(&path, "{}").unwrap();

            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "write_private must tighten a pre-existing file's permissions, not just set them on create"
            );
        });
    }
}
