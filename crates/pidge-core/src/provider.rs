//! The provider seam: what pidge needs from a mail and calendar service.
//!
//! The engine (`pidge-engine`) talks only to [`Provider`]; `pidge-graph`
//! (Microsoft 365 / Outlook.com) and `pidge-gmail` (Gmail + Google Calendar)
//! implement it. Everything here is provider-neutral: a folder is a
//! [`Folder`], a label or Outlook category is a category name, ids are the
//! provider's own *stable* ids (Graph immutable ids, Gmail ids), opaque to the
//! engine.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::{Event, EventTime};
use crate::message::{Attachment, FullMessage, MessageFrom};

/// Which service an account lives at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Microsoft 365 work/school accounts and personal Microsoft accounts.
    #[default]
    Microsoft,
    /// Gmail and Google Calendar.
    Google,
}

impl ProviderKind {
    pub fn is_microsoft(&self) -> bool {
        *self == Self::Microsoft
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::Google => "google",
        }
    }
}

/// A mail folder in provider-neutral terms. Gmail maps these onto labels
/// (`Inbox` = has `INBOX`, `Archive` = none of the system folders).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Folder {
    Inbox,
    Archive,
    Sent,
    Drafts,
    Deleted,
    Junk,
    /// A user folder (Graph folder id) or user label (Gmail label id).
    Other(String),
}

impl Folder {
    /// The name used in the index and in tool input.
    pub fn key(&self) -> String {
        match self {
            Self::Inbox => "inbox".into(),
            Self::Archive => "archive".into(),
            Self::Sent => "sent".into(),
            Self::Drafts => "drafts".into(),
            Self::Deleted => "deleted".into(),
            Self::Junk => "junk".into(),
            Self::Other(id) => format!("folder:{id}"),
        }
    }

    pub fn from_key(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "inbox" => Self::Inbox,
            "archive" => Self::Archive,
            "sent" => Self::Sent,
            "drafts" => Self::Drafts,
            "deleted" | "trash" => Self::Deleted,
            "junk" | "spam" => Self::Junk,
            _ => Self::Other(s.strip_prefix("folder:").unwrap_or(s).to_string()),
        }
    }
}

/// What the provider itself thinks of a message's bulkiness. Graph's
/// Focused/Other and Gmail's inbox categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BulkHint {
    Focused,
    Other,
    Primary,
    Promotions,
    Social,
    Updates,
    Forums,
}

impl BulkHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Focused => "focused",
            Self::Other => "other",
            Self::Primary => "primary",
            Self::Promotions => "promotions",
            Self::Social => "social",
            Self::Updates => "updates",
            Self::Forums => "forums",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "focused" => Self::Focused,
            "other" => Self::Other,
            "primary" => Self::Primary,
            "promotions" => Self::Promotions,
            "social" => Self::Social,
            "updates" => Self::Updates,
            "forums" => Self::Forums,
            _ => return None,
        })
    }
}

/// A message's metadata: what the index stores and what lists show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailItem {
    pub account: String,
    /// The provider's stable id.
    pub id: String,
    pub thread_id: String,
    pub folder: Folder,
    pub from: MessageFrom,
    #[serde(default)]
    pub to: Vec<MessageFrom>,
    #[serde(default)]
    pub cc: Vec<MessageFrom>,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub preview: String,
    pub unread: bool,
    pub flagged: bool,
    pub has_attachments: bool,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub bulk_hint: Option<BulkHint>,
    /// The message carries a `List-Unsubscribe` header (when the provider
    /// returns headers in lists; Gmail does, Graph does not).
    #[serde(default)]
    pub list_unsubscribe: bool,
    #[serde(default)]
    pub is_invite: bool,
}

/// One round of mailbox sync.
#[derive(Debug, Clone, Default)]
pub struct MailSync {
    /// New or changed messages (folder moves included).
    pub upserts: Vec<MailItem>,
    /// Ids that left the synced window (deleted for good, or moved out of
    /// every tracked folder).
    pub removed: Vec<String>,
    /// Opaque cursor for the next call.
    pub cursor: String,
    /// More changes are waiting: call again with `cursor` at once.
    pub more: bool,
}

/// Search beyond the index (and the only search for old mail).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub after: Option<NaiveDate>,
    pub before: Option<NaiveDate>,
    pub has_attachments: Option<bool>,
    pub folder: Option<Folder>,
    pub unread: Option<bool>,
    pub limit: usize,
}

/// A change to one or more messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MailChange {
    MarkRead,
    MarkUnread,
    Flag {
        due: Option<NaiveDate>,
    },
    Unflag,
    Move {
        to: Folder,
    },
    /// Replace the message's categories (labels) with exactly these names.
    SetCategories {
        categories: Vec<String>,
    },
    /// To Deleted Items / Trash. Never permanent.
    Delete,
}

/// What kind of draft to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftKind {
    New,
    Reply { to_id: String },
    ReplyAll { to_id: String },
    Forward { to_id: String },
}

/// Draft content. Recipients are resolved addresses; on a reply they are
/// *added* to what the provider fills in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSpec {
    pub kind: DraftKind,
    #[serde(default)]
    pub to: Vec<MessageFrom>,
    #[serde(default)]
    pub cc: Vec<MessageFrom>,
    #[serde(default)]
    pub bcc: Vec<MessageFrom>,
    /// New mail only; replies and forwards keep the original's.
    pub subject: Option<String>,
    /// Plain text; blank lines separate paragraphs.
    pub body: String,
}

/// A draft as the provider stored it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub account: String,
    pub id: String,
    pub thread_id: Option<String>,
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    pub bcc: Vec<MessageFrom>,
    pub subject: String,
    /// The body as it will go out, quoted history included, as text.
    pub body_text: String,
    pub has_attachments: bool,
}

/// A folder or label, for listing and moving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInfo {
    pub folder: Folder,
    /// Display path, `/`-separated for nested folders.
    pub path: String,
    pub unread: u32,
    pub total: u32,
}

/// `List-Unsubscribe` and `List-Unsubscribe-Post`, raw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnsubscribeHeaders {
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
}

/// A new event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSpec {
    pub title: String,
    pub when: EventWhen,
    pub location: Option<String>,
    pub body: Option<String>,
    #[serde(default)]
    pub attendees: Vec<MessageFrom>,
    #[serde(default)]
    pub online_meeting: bool,
}

/// An event's time: timed in a zone, or whole days (end inclusive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventWhen {
    Timed { start: EventTime, end: EventTime },
    AllDay { first: NaiveDate, last: NaiveDate },
}

/// Fields to change on an event; `None` leaves a field alone. `attendees`
/// is the full new list (the engine computes it from add/remove).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPatch {
    pub title: Option<String>,
    pub when: Option<EventWhen>,
    pub location: Option<String>,
    pub body: Option<String>,
    pub attendees: Option<Vec<MessageFrom>>,
    pub online_meeting: Option<bool>,
}

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rsvp {
    Accept,
    Tentative,
    Decline,
}

/// A busy block for free/busy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusyBlock {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Someone the user corresponds with, from the provider's people API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonHint {
    pub name: String,
    pub address: String,
}

/// Provider failures, mapped onto the categories the engine acts on.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ProviderError {
    /// The account must sign in again.
    #[error("{account} needs to sign in again: {message}")]
    Auth { account: String, message: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("throttled by the provider")]
    Throttled { retry_after: Option<u64> },
    /// The provider cannot do this (e.g. free/busy on a personal account).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The provider refused the input.
    #[error("{0}")]
    Invalid(String),
    /// A sync cursor the provider no longer accepts: re-bootstrap.
    #[error("sync state expired")]
    CursorExpired,
    #[error("{0}")]
    Other(String),
}

pub type ProviderResult<T> = Result<T, ProviderError>;

/// One mailbox (and its calendar) at one provider.
///
/// Implementations hold their own credentials; every call acts as
/// [`Provider::address`]. They must never follow a URL taken from message
/// content, and must keep ids confined to their own id alphabet before
/// putting them in a request path.
#[async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    /// The mailbox address.
    fn address(&self) -> &str;

    // ---- mail -------------------------------------------------------------

    /// Sync the inbox, archive and sent mail received on or after `since`.
    /// `cursor` is `None` to bootstrap, which returns everything in the
    /// window as upserts (paged with `more`).
    async fn sync_mail(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> ProviderResult<MailSync>;

    /// Search the whole mailbox, newest first.
    async fn search(&self, query: &SearchQuery) -> ProviderResult<Vec<MailItem>>;

    /// Metadata of a few messages (a refresh after a change).
    async fn get_items(&self, ids: &[String]) -> ProviderResult<Vec<MailItem>>;

    async fn get_message(&self, id: &str) -> ProviderResult<FullMessage>;

    /// The messages of a conversation, oldest first.
    async fn get_thread(&self, thread_id: &str) -> ProviderResult<Vec<FullMessage>>;

    async fn unsubscribe_headers(&self, id: &str) -> ProviderResult<UnsubscribeHeaders>;

    /// Apply one change to many messages; one result per id, in order.
    async fn apply(&self, ids: &[String], change: &MailChange) -> Vec<ProviderResult<()>>;

    async fn list_folders(&self) -> ProviderResult<Vec<FolderInfo>>;

    /// Find or create a folder by `/`-separated path; returns it.
    async fn ensure_folder(&self, path: &str) -> ProviderResult<Folder>;

    /// The mailbox's category (label) names.
    async fn list_categories(&self) -> ProviderResult<Vec<String>>;

    /// Make sure a category exists (idempotent).
    async fn ensure_category(&self, name: &str) -> ProviderResult<()>;

    async fn list_attachments(&self, id: &str) -> ProviderResult<Vec<Attachment>>;

    async fn attachment_bytes(&self, id: &str, attachment_id: &str) -> ProviderResult<Vec<u8>>;

    async fn create_draft(&self, spec: &DraftSpec) -> ProviderResult<Draft>;

    async fn update_draft(&self, id: &str, spec: &DraftSpec) -> ProviderResult<Draft>;

    async fn get_draft(&self, id: &str) -> ProviderResult<Draft>;

    async fn delete_draft(&self, id: &str) -> ProviderResult<()>;

    async fn send_draft(&self, id: &str) -> ProviderResult<()>;

    /// People the user corresponds with, best first. Empty when the
    /// provider has no such API (the engine also learns from the index).
    async fn people(&self) -> ProviderResult<Vec<PersonHint>> {
        Ok(Vec::new())
    }

    // ---- calendar ---------------------------------------------------------

    /// Events overlapping `[from, to)`, recurring events expanded.
    async fn list_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> ProviderResult<Vec<Event>>;

    async fn get_event(&self, id: &str) -> ProviderResult<Event>;

    /// Create; invitations go out to attendees at once.
    async fn create_event(&self, spec: &EventSpec) -> ProviderResult<Event>;

    async fn update_event(&self, id: &str, patch: &EventPatch) -> ProviderResult<Event>;

    /// Cancel an event the user organizes; attendees are notified.
    async fn cancel_event(&self, id: &str, message: Option<&str>) -> ProviderResult<()>;

    /// Delete an event from the user's calendar without notifying anyone.
    async fn delete_event(&self, id: &str) -> ProviderResult<()>;

    async fn respond(
        &self,
        id: &str,
        response: Rsvp,
        message: Option<&str>,
        notify: bool,
    ) -> ProviderResult<()>;

    /// Busy blocks for other people. `Unsupported` where the account type
    /// cannot see others' calendars.
    async fn free_busy(
        &self,
        people: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> ProviderResult<Vec<(String, Vec<BusyBlock>)>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_keys_round_trip() {
        for f in [
            Folder::Inbox,
            Folder::Archive,
            Folder::Sent,
            Folder::Drafts,
            Folder::Deleted,
            Folder::Junk,
            Folder::Other("AAMk=".into()),
        ] {
            assert_eq!(Folder::from_key(&f.key()), f);
        }
        assert_eq!(Folder::from_key("Trash"), Folder::Deleted);
    }

    #[test]
    fn bulk_hints_round_trip() {
        for h in [
            BulkHint::Focused,
            BulkHint::Other,
            BulkHint::Primary,
            BulkHint::Promotions,
            BulkHint::Social,
            BulkHint::Updates,
            BulkHint::Forums,
        ] {
            assert_eq!(BulkHint::parse(h.as_str()), Some(h));
        }
    }

    #[test]
    fn mail_change_serializes_tagged() {
        let j = serde_json::to_string(&MailChange::Move {
            to: Folder::Archive,
        })
        .unwrap();
        assert_eq!(j, r#"{"op":"move","to":"archive"}"#);
    }
}
