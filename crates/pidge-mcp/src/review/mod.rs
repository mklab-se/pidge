//! Hands-free inbox review (`mail_review_start` / `mail_review_next`):
//! pidge picks the scope, sorts the unread mail into the user's
//! categories, and applies each decision in the background while handing
//! back the next message, so a voice harness needs one call per message.
//! Design: `docs/superpowers/specs/2026-10-11-mcp-inbox-review-design.md`.

pub mod ai;
pub mod categories;
pub mod classify;
pub mod dedupe;
pub mod graph;
pub mod scope;
pub mod session;
pub mod sync;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use categories::Category;

/// Review state on the user record: their categories and when they last
/// went through their mail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSettings {
    /// `None` until the user edits or confirms them: the defaults apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// The user has said yes to the categories in use.
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_review_at: Option<DateTime<Utc>>,
    /// Writing the categories to the mailboxes too (off by default).
    #[serde(default, skip_serializing_if = "sync::MailboxSync::is_default")]
    pub mailbox_sync: sync::MailboxSync,
}

impl ReviewSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The categories in force: the user's own, or the defaults.
    pub fn categories(&self) -> Vec<Category> {
        self.categories.clone().unwrap_or_else(categories::defaults)
    }
}

/// Process-wide review state, part of `AppState`.
#[derive(Default)]
pub struct ReviewState {
    pub sessions: session::Sessions,
    pub classes: Arc<classify::ClassCache>,
    pub ai_clients: ai::AiClients,
    pub ai_links: crate::ai_page::AiLinks,
    /// One mailbox-category task at a time per user.
    pub sync_locks: sync::Locks,
    /// A TypeSafe endpoint override: set only by tests (a mock server).
    pub ai_endpoint: Option<String>,
}
