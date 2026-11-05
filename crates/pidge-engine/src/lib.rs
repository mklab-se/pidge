//! pidge's engine: every mail and calendar operation, defined once.
//!
//! The frontends (the `pidge` CLI, `pidge mcp serve` on stdio, and the
//! hosted `pidge-mcp` server) are thin: they build a [`ctx::User`] (the
//! user's mailboxes as [`pidge_core::provider::Provider`]s, their index, their
//! preferences, a [`ctx::Host`]) and call operations from the
//! [`registry`]. Behaviour, safety rules and wording are the same
//! everywhere because they live here.

pub mod classify;
pub mod ctx;
pub mod error;
pub mod kinds;
pub mod ops;
pub mod outbox;
pub mod playbooks;
pub mod prefs;
pub mod registry;
pub mod render;
pub mod sync;

pub use ctx::{AccountInfo, Frontend, Host, User};
pub use error::{ErrorKind, OpError, OpResult};
pub use prefs::{FilePrefs, MemoryPrefs, Prefs, PrefsStore};
pub use registry::{OpDef, Output};

#[cfg(test)]
mod tests;
