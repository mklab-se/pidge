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
pub use unsubscribe::{UnsubscribeMethod, parse_unsubscribe};

pub mod cursor;

pub use cursor::{Cursor, CursorError};

/// Resolve the base directory pidge's config-dir-rooted stores (Microsoft
/// tokens in `auth::file_store`, MCP tokens in `mcp::store`) live under.
///
/// In tests, honors a thread-local override set via
/// [`test_support::with_base_dir`] so tests never mutate process-wide
/// `HOME`/`XDG_CONFIG_HOME` env vars — two independent stores' tests
/// overriding those globally, each behind its own lock, can still race
/// across threads in the same test binary. A thread-local sidesteps that
/// entirely: it's invisible to every thread but the one that set it, so
/// parallel `#[test]` functions never interfere and no lock is needed.
///
/// In production this is always `dirs::config_dir()`.
pub(crate) fn base_config_dir() -> Result<std::path::PathBuf, ClientError> {
    #[cfg(test)]
    if let Some(base) = test_support::base_dir_override() {
        return Ok(base);
    }
    dirs::config_dir().ok_or(ClientError::NoConfigDir)
}
