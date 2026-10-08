//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.

pub mod auth;
mod error;
pub mod graph;
pub mod mcp;
#[cfg(test)]
pub(crate) mod test_support;
pub mod unsubscribe;

pub use auth::AuthClient;
pub use error::ClientError;
pub use graph::{GraphClient, MailFolder, Outgoing};
pub use unsubscribe::{UnsubscribeMethod, mailto_subject, parse_unsubscribe};

pub mod cursor;

pub use cursor::{Cursor, CursorError};

/// Resolve pidge's config directory, which the config-dir-rooted stores
/// (Microsoft tokens in `auth::file_store`, MCP tokens in `mcp::store`)
/// live under: `${XDG_CONFIG_HOME:-~/.config}/pidge` on Linux and macOS,
/// `%APPDATA%\pidge` on Windows (see `pidge_core::paths`).
///
/// In tests, honors a thread-local override set via
/// [`test_support::with_base_dir`] so tests never mutate process-wide
/// `HOME`/`XDG_CONFIG_HOME` env vars: two independent stores' tests
/// overriding those globally, each behind its own lock, can still race
/// across threads in the same test binary. A thread-local sidesteps that
/// entirely: it's invisible to every thread but the one that set it, so
/// parallel `#[test]` functions never interfere and no lock is needed.
///
/// In production this is always `pidge_core::paths::config_dir()`.
pub(crate) fn config_dir() -> Result<std::path::PathBuf, ClientError> {
    #[cfg(test)]
    if let Some(base) = test_support::base_dir_override() {
        return Ok(base);
    }
    pidge_core::paths::config_dir().ok_or(ClientError::NoConfigDir)
}

#[cfg(test)]
mod tests {
    #[test]
    fn config_dir_is_pidge_core_paths_config_dir() {
        // No thread-local override on this thread: production resolution.
        assert_eq!(
            super::config_dir().ok(),
            pidge_core::paths::config_dir(),
            "client stores must share the one XDG-style config dir"
        );
    }
}
