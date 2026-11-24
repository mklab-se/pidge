//! pidge's local state for one user: an SQLite database holding
//!
//! - **handles**: short, stable, per-user names (`m12`, `e3`, `d4`, `p7`) for
//!   provider ids, so tool output never carries 150-character Graph ids;
//! - **the index**: message metadata for the synced window and pidge's own
//!   classification of each message (kind, category, importance);
//! - **events** seen in agendas (for handles);
//! - short-lived working state: **plans**, the **queue** of changes on their
//!   way to the provider (and what undoes them), the **outbox**, the current
//!   **deck**, pending **notes**, and the **send log** for caps.
//!
//! Everything here is a cache or short-lived: the provider is the source of
//! truth and the user's durable preferences live elsewhere (see
//! `pidge-engine`'s prefs). Deleting the database loses nothing that matters
//! beyond the last few minutes of undo.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use pidge_core::MessageFrom;
use pidge_core::provider::{BulkHint, Folder, MailChange, MailItem};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Bump when the schema changes: an older database is dropped and rebuilt
/// (it is a cache).
const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("index: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("index data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("index io: {0}")]
    Io(#[from] std::io::Error),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// What a handle names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HandleKind {
    Message,
    Event,
    Draft,
    Plan,
    /// A group of messages in a deck.
    Group,
    /// An instruction: a change the user asked for.
    Queue,
}

impl HandleKind {
    pub fn prefix(self) -> char {
        match self {
            Self::Message => 'm',
            Self::Event => 'e',
            Self::Draft => 'd',
            Self::Plan => 'p',
            Self::Group => 'g',
            Self::Queue => 'q',
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Event => "event",
            Self::Draft => "draft",
            Self::Plan => "plan",
            Self::Group => "group",
            Self::Queue => "queue",
        }
    }

    pub fn from_handle(handle: &str) -> Option<Self> {
        let mut chars = handle.chars();
        let kind = match chars.next()? {
            'm' => Self::Message,
            'e' => Self::Event,
            'd' => Self::Draft,
            'p' => Self::Plan,
            'g' => Self::Group,
            'q' => Self::Queue,
            _ => return None,
        };
        let rest = chars.as_str();
        (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric())).then_some(kind)
    }
}

/// A resolved handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub handle: String,
    pub kind: HandleKind,
    pub account: String,
    pub id: String,
}

/// pidge's own judgement of a message, stored next to the provider's data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    /// What the message is (receipt, newsletter, ...), when known.
    pub kind: Option<String>,
    /// Where the judgement came from: rule, signal, ai, harness.
    pub kind_source: Option<String>,
    /// The user's category, when known.
    pub category: Option<String>,
    pub category_source: Option<String>,
    /// 0 low, 1 normal, 2 high.
    pub importance: u8,
    /// Why it is high (vip, reply-to-you, ...).
    pub reasons: Vec<String>,
}

/// Something that happens at a time, found in a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Happening {
    pub start: DateTime<Utc>,
    /// When the message says so.
    #[serde(default)]
    pub end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub all_day: bool,
}

/// A message in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub handle: String,
    pub item: MailItem,
    pub class: Classification,
    /// When pidge classified it (None: not yet).
    pub classified_at: Option<DateTime<Utc>>,
    /// It is about something that happens to the user at this time.
    pub happening: Option<Happening>,
    /// pidge has looked for a time in it.
    pub time_checked: bool,
}

/// A query over the index. Empty fields do not filter.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub accounts: Vec<String>,
    pub folder: Option<Folder>,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub has_attachments: Option<bool>,
    /// Part of the sender's name or address; alternatives with ` OR `, `|`
    /// or commas.
    pub from: Option<String>,
    /// Any of these sender addresses (exact, lowercase).
    pub from_any: Vec<String>,
    /// Words in subject, preview or sender; alternatives with ` OR ` or `|`.
    pub text: Option<String>,
    /// Only messages about something that happens at a time.
    pub happening: bool,
    /// Only messages not yet searched for a time.
    pub time_unchecked: bool,
    pub kinds: Vec<String>,
    pub category: Option<String>,
    pub min_importance: Option<u8>,
    pub thread_id: Option<String>,
    /// Only messages pidge has not classified yet.
    pub unclassified: bool,
    pub limit: Option<usize>,
    /// Oldest first instead of newest first.
    pub oldest_first: bool,
}

/// Sync bookkeeping for one mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    pub account: String,
    pub cursor: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
    /// The bootstrap finished (the whole window is in the index).
    pub complete: bool,
}

/// What an undo entry restores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoEntry {
    pub account: String,
    pub id: String,
    pub folder: Folder,
    pub unread: bool,
    pub flagged: bool,
    pub categories: Vec<String>,
}

/// An instruction in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueRow {
    pub seq: i64,
    /// `q7`.
    pub id: String,
    pub label: String,
    /// queued, running, done, failed, undone
    pub status: String,
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    pub json: String,
}

/// One outbox entry: a draft waiting to go out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub draft: String,
    pub account: String,
    pub draft_id: String,
    pub send_at: Option<DateTime<Utc>>,
    /// held, approval, sending, sent, cancelled, failed
    pub status: String,
    pub summary: String,
    pub reasons: Vec<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One person seen in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonRow {
    pub name: String,
    pub address: String,
    /// Messages the user sent them.
    pub sent_to: u32,
    /// Messages received from them.
    pub received_from: u32,
    pub last_contact: Option<DateTime<Utc>>,
}

/// The per-user database.
pub struct Store {
    conn: Mutex<Connection>,
}

fn ts(t: DateTime<Utc>) -> i64 {
    t.timestamp()
}

fn from_ts(t: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(t, 0).unwrap_or_default()
}

fn to_base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS handles (
    handle TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    account TEXT NOT NULL,
    pid TEXT NOT NULL,
    UNIQUE (kind, account, pid)
);
CREATE TABLE IF NOT EXISTS messages (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    handle TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    folder TEXT NOT NULL,
    from_name TEXT NOT NULL,
    from_addr TEXT NOT NULL,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    subject TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    preview TEXT NOT NULL,
    unread INTEGER NOT NULL,
    flagged INTEGER NOT NULL,
    has_attachments INTEGER NOT NULL,
    categories_json TEXT NOT NULL,
    bulk_hint TEXT,
    list_unsubscribe INTEGER NOT NULL,
    is_invite INTEGER NOT NULL,
    message_id TEXT NOT NULL DEFAULT '',
    found INTEGER NOT NULL DEFAULT 0,
    happening_json TEXT,
    time_checked INTEGER NOT NULL DEFAULT 0,
    kind TEXT,
    kind_source TEXT,
    category TEXT,
    category_source TEXT,
    importance INTEGER NOT NULL DEFAULT 1,
    reasons_json TEXT NOT NULL DEFAULT '[]',
    classified_at INTEGER,
    PRIMARY KEY (account, id)
);
CREATE INDEX IF NOT EXISTS messages_received ON messages (received_at);
CREATE INDEX IF NOT EXISTS messages_folder ON messages (folder, received_at);
CREATE INDEX IF NOT EXISTS messages_thread ON messages (thread_id);
CREATE INDEX IF NOT EXISTS messages_from ON messages (from_addr);
CREATE INDEX IF NOT EXISTS messages_msgid ON messages (message_id);
CREATE TABLE IF NOT EXISTS sync (
    account TEXT PRIMARY KEY,
    cursor TEXT,
    synced_at INTEGER,
    window_start INTEGER,
    complete INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS events (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    handle TEXT NOT NULL,
    start_at INTEGER NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (account, id)
);
CREATE TABLE IF NOT EXISTS docs (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER,
    json TEXT NOT NULL,
    PRIMARY KEY (kind, id)
);
CREATE TABLE IF NOT EXISTS queue (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    due INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pending (
    seq INTEGER NOT NULL,
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    change_json TEXT
);
CREATE INDEX IF NOT EXISTS pending_target ON pending (account, id);
CREATE TABLE IF NOT EXISTS outbox (
    draft TEXT PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notes (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    text TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sends (at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS people_hints (
    address TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    rank INTEGER NOT NULL
);
";

const MSG_COLS: &str = "account, id, handle, thread_id, folder, from_name, from_addr, to_json, \
cc_json, subject, received_at, preview, unread, flagged, has_attachments, categories_json, \
bulk_hint, list_unsubscribe, is_invite, kind, kind_source, category, category_source, \
importance, reasons_json, classified_at, message_id, happening_json, time_checked";

fn row_to_stored(r: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    let to_json: String = r.get(7)?;
    let cc_json: String = r.get(8)?;
    let cats: String = r.get(15)?;
    let reasons: String = r.get(24)?;
    let hint: Option<String> = r.get(16)?;
    let classified_at: Option<i64> = r.get(25)?;
    let happening: Option<String> = r.get(27)?;
    Ok(Stored {
        handle: r.get(2)?,
        item: MailItem {
            account: r.get(0)?,
            id: r.get(1)?,
            thread_id: r.get(3)?,
            folder: Folder::from_key(&r.get::<_, String>(4)?),
            from: MessageFrom {
                name: r.get(5)?,
                address: r.get(6)?,
            },
            to: serde_json::from_str(&to_json).unwrap_or_default(),
            cc: serde_json::from_str(&cc_json).unwrap_or_default(),
            subject: r.get(9)?,
            received_at: from_ts(r.get(10)?),
            preview: r.get(11)?,
            unread: r.get(12)?,
            flagged: r.get(13)?,
            has_attachments: r.get(14)?,
            categories: serde_json::from_str(&cats).unwrap_or_default(),
            bulk_hint: hint.as_deref().and_then(BulkHint::parse),
            list_unsubscribe: r.get(17)?,
            is_invite: r.get(18)?,
            message_id: r.get(26)?,
        },
        class: Classification {
            kind: r.get(19)?,
            kind_source: r.get(20)?,
            category: r.get(21)?,
            category_source: r.get(22)?,
            importance: r.get::<_, i64>(23)?.clamp(0, 2) as u8,
            reasons: serde_json::from_str(&reasons).unwrap_or_default(),
        },
        classified_at: classified_at.map(from_ts),
        happening: happening.and_then(|j| serde_json::from_str(&j).ok()),
        time_checked: r.get(28)?,
    })
}

/// ` OR ` and `|` separate alternatives; an alternative is all its words.
fn alternatives(text: &str) -> Vec<Vec<String>> {
    text.replace(" OR ", "|")
        .split('|')
        .map(|alt| {
            alt.split_whitespace()
                .map(|w| w.trim_matches('"').to_lowercase())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

/// The alternatives in a text query, as written.
pub fn text_alternatives(text: &str) -> Vec<String> {
    alternatives(text).iter().map(|a| a.join(" ")).collect()
}

/// The alternatives in a sender query: ` OR `, `|` or commas.
pub fn from_alternatives(from: &str) -> Vec<String> {
    from.replace(" OR ", "|")
        .split(['|', ','])
        .map(|f| f.trim().to_lowercase())
        .filter(|f| !f.is_empty())
        .collect()
}

/// Make one change to one row.
fn change_row(conn: &Connection, account: &str, id: &str, change: &MailChange) -> StoreResult<()> {
    let set = |sql: &str, v: rusqlite::types::Value| -> rusqlite::Result<()> {
        conn.execute(
            &format!("UPDATE messages SET {sql} = ?3 WHERE account = ?1 AND id = ?2"),
            params![account, id, v],
        )?;
        Ok(())
    };
    use rusqlite::types::Value as V;
    match change {
        MailChange::MarkRead => set("unread", V::Integer(0))?,
        MailChange::MarkUnread => set("unread", V::Integer(1))?,
        MailChange::Flag { .. } => set("flagged", V::Integer(1))?,
        MailChange::Unflag => set("flagged", V::Integer(0))?,
        MailChange::Move { to } => set("folder", V::Text(to.key()))?,
        MailChange::Delete => set("folder", V::Text(Folder::Deleted.key()))?,
        MailChange::SetCategories { categories } => set(
            "categories_json",
            V::Text(serde_json::to_string(categories)?),
        )?,
    }
    Ok(())
}

impl Store {
    /// Open (or create) the database at `path`.
    pub fn open(path: &Path) -> StoreResult<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// A private in-memory database (tests, and hosted users before their
    /// file is ready).
    pub fn in_memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> StoreResult<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            // A cache: rebuild rather than migrate.
            conn.execute_batch(
                "DROP TABLE IF EXISTS meta; DROP TABLE IF EXISTS handles; \
                 DROP TABLE IF EXISTS messages; DROP TABLE IF EXISTS sync; \
                 DROP TABLE IF EXISTS events; DROP TABLE IF EXISTS docs; \
                 DROP TABLE IF EXISTS undo; DROP TABLE IF EXISTS outbox; \
                 DROP TABLE IF EXISTS queue; DROP TABLE IF EXISTS pending; \
                 DROP TABLE IF EXISTS notes; DROP TABLE IF EXISTS sends; \
                 DROP TABLE IF EXISTS people_hints;",
            )?;
        }
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---- handles ----------------------------------------------------------

    fn handle_in(
        conn: &Connection,
        kind: HandleKind,
        account: &str,
        pid: &str,
    ) -> StoreResult<String> {
        if let Some(h) = conn
            .query_row(
                "SELECT handle FROM handles WHERE kind = ?1 AND account = ?2 AND pid = ?3",
                params![kind.key(), account, pid],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(h);
        }
        let key = format!("next_{}", kind.key());
        let n: i64 = conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [&key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let handle = format!("{}{}", kind.prefix(), to_base36(n as u64));
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, (n + 1).to_string()],
        )?;
        conn.execute(
            "INSERT INTO handles (handle, kind, account, pid) VALUES (?1, ?2, ?3, ?4)",
            params![handle, kind.key(), account, pid],
        )?;
        Ok(handle)
    }

    /// Whether this database has never handed out a handle.
    pub fn is_fresh(&self) -> StoreResult<bool> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))?;
        Ok(n == 0)
    }

    /// Start every kind of handle at `base`, so that no handle from an
    /// earlier database comes back meaning something else.
    pub fn start_handles_at(&self, base: u64) -> StoreResult<()> {
        let conn = self.conn();
        for kind in [
            HandleKind::Message,
            HandleKind::Event,
            HandleKind::Draft,
            HandleKind::Plan,
            HandleKind::Group,
            HandleKind::Queue,
        ] {
            conn.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
                params![format!("next_{}", kind.key()), base.to_string()],
            )?;
        }
        Ok(())
    }

    /// The handle for a provider id, created on first sight.
    pub fn handle(&self, kind: HandleKind, account: &str, pid: &str) -> StoreResult<String> {
        Self::handle_in(&self.conn(), kind, account, pid)
    }

    /// What a handle names, if this user has it.
    pub fn resolve(&self, handle: &str) -> StoreResult<Option<Target>> {
        let handle = handle.trim();
        let Some(kind) = HandleKind::from_handle(handle) else {
            return Ok(None);
        };
        Ok(self
            .conn()
            .query_row(
                "SELECT account, pid FROM handles WHERE handle = ?1 AND kind = ?2",
                params![handle, kind.key()],
                |r| {
                    Ok(Target {
                        handle: handle.to_string(),
                        kind,
                        account: r.get(0)?,
                        id: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// The handle a provider id already has, without creating one.
    pub fn lookup(
        &self,
        kind: HandleKind,
        account: &str,
        pid: &str,
    ) -> StoreResult<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT handle FROM handles WHERE kind = ?1 AND account = ?2 AND pid = ?3",
                params![kind.key(), account, pid],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Point a handle at a new provider id (a draft rebuilt by the provider).
    pub fn rebind(&self, handle: &str, pid: &str) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE handles SET pid = ?2 WHERE handle = ?1",
            params![handle, pid],
        )?;
        Ok(())
    }

    /// Find a handle by a provider id in any account (ids passed in by
    /// callers that still hold raw ids).
    pub fn find_by_pid(&self, kind: HandleKind, pid: &str) -> StoreResult<Option<Target>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT handle, account FROM handles WHERE kind = ?1 AND pid = ?2",
                params![kind.key(), pid],
                |r| {
                    Ok(Target {
                        handle: r.get(0)?,
                        kind,
                        account: r.get(1)?,
                        id: pid.to_string(),
                    })
                },
            )
            .optional()?)
    }

    // ---- messages ---------------------------------------------------------

    /// Insert or refresh messages from a sync; pidge's classification is
    /// kept, and changes still on their way to the provider stay visible.
    pub fn upsert_items(&self, items: &[MailItem]) -> StoreResult<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut handles = Vec::with_capacity(items.len());
        for i in items {
            // A search may have seen it first, under another id.
            if !i.message_id.is_empty() {
                let twin: Option<String> = tx
                    .query_row(
                        "SELECT id FROM messages WHERE account = ?1 AND message_id = ?2 \
                         AND id != ?3 AND found = 1 AND (folder = 'sent') = ?4",
                        params![i.account, i.message_id, i.id, i.folder == Folder::Sent],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(old) = twin {
                    Self::rename_in(&tx, &i.account, &old, &i.id)?;
                }
            }
            handles.push(Self::upsert_in(&tx, i, false)?);
        }
        tx.commit()?;
        Ok(handles)
    }

    /// Take in what a provider search found; returns the ids the messages
    /// have in the index. A message the index knows (by id or Message-ID)
    /// is left as the sync reported it. A hit in a synced folder, inside the
    /// synced window, that the sync does not know is stale and left out: the
    /// provider's search index lags behind the mailbox.
    pub fn adopt_found(&self, items: &[MailItem]) -> StoreResult<Vec<String>> {
        let mut states: std::collections::HashMap<String, SyncState> = Default::default();
        for i in items {
            if !states.contains_key(&i.account) {
                states.insert(i.account.clone(), self.sync_state(&i.account)?);
            }
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut ids = Vec::new();
        for i in items {
            let known: Option<(String, bool)> = tx
                .query_row(
                    "SELECT id, found FROM messages WHERE account = ?1 AND (id = ?2 OR \
                     (message_id != '' AND message_id = ?3 AND (folder = 'sent') = ?4)) \
                     ORDER BY found, id != ?2 LIMIT 1",
                    params![i.account, i.id, i.message_id, i.folder == Folder::Sent],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match known {
                Some((id, found)) => {
                    if found && id == i.id {
                        Self::upsert_in(&tx, i, true)?;
                    }
                    ids.push(id);
                }
                None => {
                    let synced = matches!(i.folder, Folder::Inbox | Folder::Archive | Folder::Sent);
                    let stale = states.get(&i.account).is_some_and(|st| {
                        synced
                            && st.complete
                            && st
                                .window_start
                                .is_some_and(|w| i.received_at > w + chrono::Duration::days(1))
                            && st
                                .synced_at
                                .is_some_and(|t| i.received_at < t - chrono::Duration::minutes(2))
                    });
                    if !stale {
                        Self::upsert_in(&tx, i, true)?;
                        ids.push(i.id.clone());
                    }
                }
            }
        }
        tx.commit()?;
        Ok(ids)
    }

    fn upsert_in(tx: &Connection, i: &MailItem, found: bool) -> StoreResult<String> {
        let handle = Self::handle_in(tx, HandleKind::Message, &i.account, &i.id)?;
        tx.execute(
            "INSERT INTO messages (account, id, handle, thread_id, folder, from_name, from_addr, \
             to_json, cc_json, subject, received_at, preview, unread, flagged, has_attachments, \
             categories_json, bulk_hint, list_unsubscribe, is_invite, message_id, found) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
             ?18, ?19, ?20, ?21) \
             ON CONFLICT(account, id) DO UPDATE SET thread_id = excluded.thread_id, \
             folder = excluded.folder, from_name = excluded.from_name, \
             from_addr = excluded.from_addr, to_json = excluded.to_json, \
             cc_json = excluded.cc_json, subject = excluded.subject, \
             received_at = excluded.received_at, preview = excluded.preview, \
             unread = excluded.unread, flagged = excluded.flagged, \
             has_attachments = excluded.has_attachments, \
             categories_json = excluded.categories_json, bulk_hint = excluded.bulk_hint, \
             list_unsubscribe = MAX(messages.list_unsubscribe, excluded.list_unsubscribe), \
             is_invite = excluded.is_invite, message_id = excluded.message_id, \
             found = MIN(messages.found, excluded.found)",
            params![
                i.account,
                i.id,
                handle,
                i.thread_id,
                i.folder.key(),
                i.from.name,
                i.from.address.to_ascii_lowercase(),
                serde_json::to_string(&i.to)?,
                serde_json::to_string(&i.cc)?,
                i.subject,
                ts(i.received_at),
                i.preview,
                i.unread,
                i.flagged,
                i.has_attachments,
                serde_json::to_string(&i.categories)?,
                i.bulk_hint.map(BulkHint::as_str),
                i.list_unsubscribe,
                i.is_invite,
                i.message_id,
                found,
            ],
        )?;
        let waiting: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT change_json FROM pending WHERE account = ?1 AND id = ?2 \
                 AND change_json IS NOT NULL ORDER BY seq, rowid",
            )?;
            stmt.query_map(params![i.account, i.id], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        for json in waiting {
            change_row(tx, &i.account, &i.id, &serde_json::from_str(&json)?)?;
        }
        Ok(handle)
    }

    /// The provider calls this message something else now.
    pub fn rename_item(&self, account: &str, old: &str, new: &str) -> StoreResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        Self::rename_in(&tx, account, old, new)?;
        tx.commit()?;
        Ok(())
    }

    fn rename_in(tx: &Connection, account: &str, old: &str, new: &str) -> StoreResult<()> {
        // Keep the handle the user's agent has seen, unless the new id has
        // its own already.
        tx.execute(
            "UPDATE OR IGNORE handles SET pid = ?3 WHERE kind = 'message' AND account = ?1 AND pid = ?2",
            params![account, old, new],
        )?;
        tx.execute(
            "UPDATE OR IGNORE messages SET id = ?3 WHERE account = ?1 AND id = ?2",
            params![account, old, new],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE account = ?1 AND id = ?2",
            params![account, old],
        )?;
        tx.execute(
            "UPDATE pending SET id = ?3 WHERE account = ?1 AND id = ?2",
            params![account, old, new],
        )?;
        Ok(())
    }

    pub fn remove_items(&self, account: &str, ids: &[String]) -> StoreResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for id in ids {
            tx.execute(
                "DELETE FROM messages WHERE account = ?1 AND id = ?2",
                params![account, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop messages received before `cutoff` (the window moved on).
    pub fn prune_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
        Ok(self
            .conn()
            .execute("DELETE FROM messages WHERE received_at < ?1", [ts(cutoff)])?)
    }

    /// Forget a mailbox entirely (signed out).
    pub fn forget_account(&self, account: &str) -> StoreResult<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM messages WHERE account = ?1", [account])?;
        conn.execute("DELETE FROM sync WHERE account = ?1", [account])?;
        conn.execute("DELETE FROM events WHERE account = ?1", [account])?;
        Ok(())
    }

    pub fn get(&self, account: &str, id: &str) -> StoreResult<Option<Stored>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {MSG_COLS} FROM messages WHERE account = ?1 AND id = ?2"),
                params![account, id],
                row_to_stored,
            )
            .optional()?)
    }

    pub fn get_by_handle(&self, handle: &str) -> StoreResult<Option<Stored>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {MSG_COLS} FROM messages WHERE handle = ?1"),
                [handle],
                row_to_stored,
            )
            .optional()?)
    }

    /// Messages matching `f`, newest first unless asked otherwise.
    pub fn query(&self, f: &Filter) -> StoreResult<Vec<Stored>> {
        let mut sql = format!("SELECT {MSG_COLS} FROM messages WHERE 1 = 1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut push = |sql: &mut String, clause: &str, v: rusqlite::types::Value| {
            args.push(v);
            sql.push_str(&clause.replace('?', &format!("?{}", args.len())));
        };
        use rusqlite::types::Value as V;
        if !f.accounts.is_empty() {
            sql.push_str(" AND account IN (");
            for (n, a) in f.accounts.iter().enumerate() {
                if n > 0 {
                    sql.push(',');
                }
                push(&mut sql, "?", V::Text(a.clone()));
            }
            sql.push(')');
        }
        if let Some(folder) = &f.folder {
            push(&mut sql, " AND folder = ?", V::Text(folder.key()));
        }
        if let Some(t) = f.since {
            push(&mut sql, " AND received_at >= ?", V::Integer(ts(t)));
        }
        if let Some(t) = f.before {
            push(&mut sql, " AND received_at < ?", V::Integer(ts(t)));
        }
        if let Some(u) = f.unread {
            push(&mut sql, " AND unread = ?", V::Integer(u as i64));
        }
        if let Some(u) = f.flagged {
            push(&mut sql, " AND flagged = ?", V::Integer(u as i64));
        }
        if let Some(u) = f.has_attachments {
            push(&mut sql, " AND has_attachments = ?", V::Integer(u as i64));
        }
        if !f.from_any.is_empty() {
            sql.push_str(" AND from_addr IN (");
            for (n, a) in f.from_any.iter().enumerate() {
                if n > 0 {
                    sql.push(',');
                }
                push(&mut sql, "?", V::Text(a.to_lowercase()));
            }
            sql.push(')');
        }
        if !f.kinds.is_empty() {
            sql.push_str(" AND kind IN (");
            for (n, k) in f.kinds.iter().enumerate() {
                if n > 0 {
                    sql.push(',');
                }
                push(&mut sql, "?", V::Text(k.clone()));
            }
            sql.push(')');
        }
        if let Some(c) = &f.category {
            push(
                &mut sql,
                " AND lower(category) = ?",
                V::Text(c.to_lowercase()),
            );
        }
        if let Some(i) = f.min_importance {
            push(&mut sql, " AND importance >= ?", V::Integer(i.into()));
        }
        if let Some(t) = &f.thread_id {
            push(&mut sql, " AND thread_id = ?", V::Text(t.clone()));
        }
        if f.unclassified {
            sql.push_str(" AND classified_at IS NULL");
        }
        if f.happening {
            sql.push_str(" AND happening_json IS NOT NULL");
        }
        if f.time_unchecked {
            sql.push_str(" AND time_checked = 0");
        }
        sql.push_str(if f.oldest_first {
            " ORDER BY received_at ASC"
        } else {
            " ORDER BY received_at DESC"
        });
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), row_to_stored)?;
        let mut out = rows.collect::<Result<Vec<_>, _>>()?;
        // Words are matched here: SQLite lower-cases ASCII only.
        let from = f.from.as_deref().map(from_alternatives).unwrap_or_default();
        if !from.is_empty() {
            out.retain(|m| {
                let who = format!("{} {}", m.item.from.name, m.item.from.address).to_lowercase();
                from.iter().any(|f| who.contains(f))
            });
        }
        let text = f.text.as_deref().map(alternatives).unwrap_or_default();
        if !text.is_empty() {
            out.retain(|m| {
                let i = &m.item;
                let all = format!(
                    "{}\n{}\n{}\n{}",
                    i.subject, i.preview, i.from.name, i.from.address
                )
                .to_lowercase();
                text.iter()
                    .any(|words| words.iter().all(|w| all.contains(w)))
            });
        }
        if let Some(limit) = f.limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    /// Count per folder / unread for the inbox map.
    pub fn count(&self, f: &Filter) -> StoreResult<usize> {
        let mut f = f.clone();
        f.limit = None;
        Ok(self.query(&f)?.len())
    }

    pub fn set_classification(
        &self,
        account: &str,
        id: &str,
        c: &Classification,
    ) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE messages SET kind = ?3, kind_source = ?4, category = ?5, category_source = ?6, \
             importance = ?7, reasons_json = ?8, classified_at = ?9 WHERE account = ?1 AND id = ?2",
            params![
                account,
                id,
                c.kind,
                c.kind_source,
                c.category,
                c.category_source,
                i64::from(c.importance),
                serde_json::to_string(&c.reasons)?,
                ts(Utc::now()),
            ],
        )?;
        Ok(())
    }

    /// This sender is a mailing list: true for all their messages.
    pub fn mark_list_sender(&self, address: &str) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE messages SET list_unsubscribe = 1 WHERE from_addr = ?1",
            [address.to_ascii_lowercase()],
        )?;
        Ok(())
    }

    /// This one message came through a mailing list.
    pub fn mark_list_message(&self, account: &str, id: &str) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE messages SET list_unsubscribe = 1 WHERE account = ?1 AND id = ?2",
            params![account, id],
        )?;
        Ok(())
    }

    /// What pidge found when it looked for a time in a message.
    pub fn set_happening(
        &self,
        account: &str,
        id: &str,
        happening: Option<&Happening>,
    ) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE messages SET happening_json = ?3, time_checked = 1 WHERE account = ?1 AND id = ?2",
            params![
                account,
                id,
                happening.map(serde_json::to_string).transpose()?
            ],
        )?;
        Ok(())
    }

    /// Forget every classification (the user's categories or rules changed).
    pub fn reset_classifications(&self) -> StoreResult<()> {
        self.conn()
            .execute("UPDATE messages SET classified_at = NULL", [])?;
        Ok(())
    }

    /// Reflect a change in the index at once, before the provider has done
    /// it. With `seq` (the instruction it belongs to) it also stays visible
    /// through syncs until the instruction is finished. Returns what undo
    /// needs, per message.
    pub fn apply_local(
        &self,
        account: &str,
        ids: &[String],
        change: &MailChange,
        seq: Option<i64>,
    ) -> StoreResult<Vec<UndoEntry>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut before = Vec::new();
        for id in ids {
            if let Some(seq) = seq {
                tx.execute(
                    "INSERT INTO pending (seq, account, id, change_json) VALUES (?1, ?2, ?3, ?4)",
                    params![seq, account, id, serde_json::to_string(change)?],
                )?;
            }
            let row = tx
                .query_row(
                    &format!("SELECT {MSG_COLS} FROM messages WHERE account = ?1 AND id = ?2"),
                    params![account, id],
                    row_to_stored,
                )
                .optional()?;
            let Some(row) = row else { continue };
            before.push(UndoEntry {
                account: account.to_string(),
                id: id.clone(),
                folder: row.item.folder,
                unread: row.item.unread,
                flagged: row.item.flagged,
                categories: row.item.categories,
            });
            change_row(&tx, account, id, change)?;
        }
        tx.commit()?;
        Ok(before)
    }

    /// Put the index back the way an undo entry recorded it.
    pub fn restore_local(&self, e: &UndoEntry) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE messages SET folder = ?3, unread = ?4, flagged = ?5, categories_json = ?6 \
             WHERE account = ?1 AND id = ?2",
            params![
                e.account,
                e.id,
                e.folder.key(),
                e.unread,
                e.flagged,
                serde_json::to_string(&e.categories)?
            ],
        )?;
        Ok(())
    }

    /// [`Store::restore_local`], keeping what later instructions than `seq`
    /// have changed since.
    pub fn restore_under(&self, e: &UndoEntry, seq: i64) -> StoreResult<()> {
        self.restore_local(e)?;
        let conn = self.conn();
        let later: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT change_json FROM pending WHERE account = ?1 AND id = ?2 AND seq > ?3 \
                 AND change_json IS NOT NULL ORDER BY seq, rowid",
            )?;
            stmt.query_map(params![e.account, e.id, seq], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        for json in later {
            change_row(&conn, &e.account, &e.id, &serde_json::from_str(&json)?)?;
        }
        Ok(())
    }

    // ---- sync -------------------------------------------------------------

    pub fn sync_state(&self, account: &str) -> StoreResult<SyncState> {
        Ok(self
            .conn()
            .query_row(
                "SELECT cursor, synced_at, window_start, complete FROM sync WHERE account = ?1",
                [account],
                |r| {
                    Ok(SyncState {
                        account: account.to_string(),
                        cursor: r.get(0)?,
                        synced_at: r.get::<_, Option<i64>>(1)?.map(from_ts),
                        window_start: r.get::<_, Option<i64>>(2)?.map(from_ts),
                        complete: r.get(3)?,
                    })
                },
            )
            .optional()?
            .unwrap_or(SyncState {
                account: account.to_string(),
                cursor: None,
                synced_at: None,
                window_start: None,
                complete: false,
            }))
    }

    pub fn save_sync(&self, s: &SyncState) -> StoreResult<()> {
        self.conn().execute(
            "INSERT INTO sync (account, cursor, synced_at, window_start, complete) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(account) DO UPDATE SET \
             cursor = excluded.cursor, synced_at = excluded.synced_at, \
             window_start = excluded.window_start, complete = excluded.complete",
            params![
                s.account,
                s.cursor,
                s.synced_at.map(ts),
                s.window_start.map(ts),
                s.complete
            ],
        )?;
        Ok(())
    }

    // ---- events -----------------------------------------------------------

    /// Remember events (for handles); returns their handles in order.
    pub fn upsert_events(&self, events: &[pidge_core::Event]) -> StoreResult<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            let handle = Self::handle_in(&tx, HandleKind::Event, &e.account, &e.id)?;
            tx.execute(
                "INSERT INTO events (account, id, handle, start_at, json) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(account, id) DO UPDATE SET start_at = excluded.start_at, json = excluded.json",
                params![e.account, e.id, handle, ts(e.start.at), serde_json::to_string(e)?],
            )?;
            out.push(handle);
        }
        tx.commit()?;
        Ok(out)
    }

    /// The calendar of one mailbox between `from` and `to`, as the provider
    /// has it now: what it no longer lists is gone.
    pub fn replace_events(
        &self,
        account: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        events: &[pidge_core::Event],
    ) -> StoreResult<()> {
        let ids: std::collections::HashSet<&str> = events.iter().map(|e| e.id.as_str()).collect();
        let gone: Vec<String> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT id FROM events WHERE account = ?1 AND start_at >= ?2 AND start_at < ?3",
            )?;
            let rows = stmt.query_map(params![account, ts(from), ts(to)], |r| {
                r.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for id in gone.iter().filter(|id| !ids.contains(id.as_str())) {
            self.remove_event(account, id)?;
        }
        self.upsert_events(events)?;
        Ok(())
    }

    /// Known events of the mailboxes that overlap `[from, to)`, with their
    /// handles, earliest first.
    pub fn events_between(
        &self,
        accounts: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> StoreResult<Vec<(String, pidge_core::Event)>> {
        let conn = self.conn();
        // Two weeks back covers what started earlier and is still on.
        let mut stmt = conn.prepare(
            "SELECT account, handle, json FROM events WHERE start_at >= ?1 AND start_at < ?2 \
             ORDER BY start_at",
        )?;
        let rows = stmt.query_map(params![ts(from) - 14 * 24 * 3600, ts(to)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (account, handle, json) = row?;
            if !accounts.contains(&account) {
                continue;
            }
            if let Ok(e) = serde_json::from_str::<pidge_core::Event>(&json)
                && e.end.at > from
            {
                out.push((handle, e));
            }
        }
        Ok(out)
    }

    pub fn event(&self, account: &str, id: &str) -> StoreResult<Option<pidge_core::Event>> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT json FROM events WHERE account = ?1 AND id = ?2",
                params![account, id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    pub fn remove_event(&self, account: &str, id: &str) -> StoreResult<()> {
        self.conn().execute(
            "DELETE FROM events WHERE account = ?1 AND id = ?2",
            params![account, id],
        )?;
        Ok(())
    }

    // ---- documents: plans, sessions, anything small and short-lived --------

    pub fn put_doc<T: Serialize>(
        &self,
        kind: &str,
        id: &str,
        value: &T,
        expires_at: Option<DateTime<Utc>>,
    ) -> StoreResult<()> {
        self.conn().execute(
            "INSERT INTO docs (kind, id, updated_at, expires_at, json) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(kind, id) DO UPDATE SET updated_at = excluded.updated_at, \
             expires_at = excluded.expires_at, json = excluded.json",
            params![
                kind,
                id,
                ts(Utc::now()),
                expires_at.map(ts),
                serde_json::to_string(value)?
            ],
        )?;
        Ok(())
    }

    pub fn get_doc<T: DeserializeOwned>(&self, kind: &str, id: &str) -> StoreResult<Option<T>> {
        let row: Option<(String, Option<i64>)> = self
            .conn()
            .query_row(
                "SELECT json, expires_at FROM docs WHERE kind = ?1 AND id = ?2",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((_, Some(exp))) if exp < ts(Utc::now()) => Ok(None),
            Some((json, _)) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    /// The most recently updated live document of a kind.
    pub fn latest_doc<T: DeserializeOwned>(&self, kind: &str) -> StoreResult<Option<(String, T)>> {
        let row: Option<(String, String)> = self
            .conn()
            .query_row(
                "SELECT id, json FROM docs WHERE kind = ?1 AND (expires_at IS NULL OR expires_at >= ?2) \
                 ORDER BY updated_at DESC, rowid DESC LIMIT 1",
                params![kind, ts(Utc::now())],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((id, json)) => Ok(Some((id, serde_json::from_str(&json)?))),
            None => Ok(None),
        }
    }

    pub fn delete_doc(&self, kind: &str, id: &str) -> StoreResult<()> {
        self.conn().execute(
            "DELETE FROM docs WHERE kind = ?1 AND id = ?2",
            params![kind, id],
        )?;
        Ok(())
    }

    /// Drop expired documents and old undo entries, notes and send records.
    pub fn housekeeping(&self) -> StoreResult<()> {
        let now = ts(Utc::now());
        let conn = self.conn();
        conn.execute(
            "DELETE FROM docs WHERE expires_at IS NOT NULL AND expires_at < ?1",
            [now],
        )?;
        conn.execute(
            "DELETE FROM queue WHERE created_at < ?1 AND status NOT IN ('new', 'queued', 'running')",
            [now - 24 * 3600],
        )?;
        conn.execute(
            "DELETE FROM pending WHERE seq NOT IN (SELECT seq FROM queue)",
            [],
        )?;
        conn.execute("DELETE FROM notes WHERE created_at < ?1", [now - 24 * 3600])?;
        conn.execute("DELETE FROM sends WHERE at < ?1", [now - 24 * 3600])?;
        Ok(())
    }

    // ---- queue ------------------------------------------------------------

    /// Note an instruction; returns its sequence number. Nothing carries it
    /// out until [`Store::release_queued`].
    pub fn enqueue(
        &self,
        id: &str,
        label: &str,
        due: DateTime<Utc>,
        json: &str,
    ) -> StoreResult<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO queue (id, label, status, due, created_at, json) \
             VALUES (?1, ?2, 'new', ?3, ?4, ?5)",
            params![id, label, due.timestamp_millis(), ts(Utc::now()), json],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// An instruction touches this event (or whatever `id` names): a later
    /// one that does too waits for it.
    pub fn touch(&self, seq: i64, account: &str, id: &str) -> StoreResult<()> {
        self.conn().execute(
            "INSERT INTO pending (seq, account, id, change_json) VALUES (?1, ?2, ?3, NULL)",
            params![seq, account, id],
        )?;
        Ok(())
    }

    fn queue_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<QueueRow> {
        Ok(QueueRow {
            seq: r.get(0)?,
            id: r.get(1)?,
            label: r.get(2)?,
            status: r.get(3)?,
            attempts: r.get::<_, i64>(4)?.max(0) as u32,
            created_at: from_ts(r.get(5)?),
            json: r.get(6)?,
        })
    }

    /// Claim the next instruction due at `now` (queued → running). One that
    /// touches a message an earlier unfinished one touches waits for it.
    pub fn claim_queued(&self, now: DateTime<Utc>) -> StoreResult<Option<QueueRow>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let row = tx
            .query_row(
                "SELECT seq, id, label, status, attempts, created_at, json FROM queue q \
                 WHERE status = 'queued' AND due <= ?1 AND NOT EXISTS ( \
                   SELECT 1 FROM pending p JOIN pending o ON o.account = p.account AND o.id = p.id \
                   JOIN queue e ON e.seq = o.seq \
                   WHERE p.seq = q.seq AND o.seq < q.seq AND e.status IN ('new', 'queued', 'running')) \
                 ORDER BY due, seq LIMIT 1",
                [now.timestamp_millis()],
                Self::queue_row,
            )
            .optional()?;
        if let Some(row) = &row {
            tx.execute(
                "UPDATE queue SET status = 'running' WHERE seq = ?1",
                [row.seq],
            )?;
        }
        tx.commit()?;
        Ok(row)
    }

    /// The instruction is complete: it is carried out when it is due.
    pub fn release_queued(&self, seq: i64) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE queue SET status = 'queued' WHERE seq = ?1 AND status = 'new'",
            [seq],
        )?;
        Ok(())
    }

    /// Replace what an instruction carries (its undo entries are known only
    /// after the index has been changed).
    pub fn set_queued_json(&self, seq: i64, json: &str) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE queue SET json = ?2 WHERE seq = ?1",
            params![seq, json],
        )?;
        Ok(())
    }

    /// Try again at `due`.
    pub fn requeue(&self, seq: i64, due: DateTime<Utc>, json: &str) -> StoreResult<()> {
        self.conn().execute(
            "UPDATE queue SET status = 'queued', attempts = attempts + 1, due = ?2, json = ?3 \
             WHERE seq = ?1",
            params![seq, due.timestamp_millis(), json],
        )?;
        Ok(())
    }

    /// The instruction is finished, one way or another.
    pub fn finish_queued(&self, seq: i64, status: &str) -> StoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE queue SET status = ?2 WHERE seq = ?1",
            params![seq, status],
        )?;
        conn.execute("DELETE FROM pending WHERE seq = ?1", [seq])?;
        Ok(())
    }

    /// Stop an instruction that has not started. False when it has.
    pub fn cancel_queued(&self, seq: i64) -> StoreResult<bool> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE queue SET status = 'undone' WHERE seq = ?1 AND status = 'queued'",
            [seq],
        )?;
        if n > 0 {
            conn.execute("DELETE FROM pending WHERE seq = ?1", [seq])?;
        }
        Ok(n > 0)
    }

    pub fn queued(&self, id: &str) -> StoreResult<Option<QueueRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT seq, id, label, status, attempts, created_at, json FROM queue WHERE id = ?1",
                [id],
                Self::queue_row,
            )
            .optional()?)
    }

    /// Instructions, newest first.
    pub fn queue(&self, limit: usize) -> StoreResult<Vec<QueueRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, id, label, status, attempts, created_at, json FROM queue \
             ORDER BY seq DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], Self::queue_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// When the next queued instruction is due.
    pub fn next_queued(&self) -> StoreResult<Option<DateTime<Utc>>> {
        let due: Option<i64> = self.conn().query_row(
            "SELECT MIN(due) FROM queue WHERE status = 'queued'",
            [],
            |r| r.get(0),
        )?;
        Ok(due.and_then(DateTime::from_timestamp_millis))
    }

    /// Give up on instructions a process left half done `since` or earlier.
    /// They would hold up later ones on the same messages.
    pub fn abandon_stuck(&self, since: DateTime<Utc>) -> StoreResult<usize> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE queue SET status = 'failed' WHERE status IN ('new', 'running') AND created_at <= ?1",
            [ts(since)],
        )?;
        conn.execute(
            "DELETE FROM pending WHERE seq IN (SELECT seq FROM queue WHERE status = 'failed')",
            [],
        )?;
        Ok(n)
    }

    /// These instructions are due now (the process that took them is ending).
    pub fn hurry_queue(&self, seqs: &[i64]) -> StoreResult<()> {
        let conn = self.conn();
        for seq in seqs {
            conn.execute(
                "UPDATE queue SET due = 0 WHERE seq = ?1 AND status = 'queued'",
                [seq],
            )?;
        }
        Ok(())
    }

    // ---- outbox -----------------------------------------------------------

    pub fn put_outbox(&self, e: &OutboxEntry) -> StoreResult<()> {
        self.conn().execute(
            "INSERT INTO outbox (draft, json) VALUES (?1, ?2) \
             ON CONFLICT(draft) DO UPDATE SET json = excluded.json",
            params![e.draft, serde_json::to_string(e)?],
        )?;
        Ok(())
    }

    pub fn outbox(&self) -> StoreResult<Vec<OutboxEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT json FROM outbox")?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out: Vec<OutboxEntry> = rows
            .iter()
            .filter_map(|j| serde_json::from_str(j).ok())
            .collect();
        out.sort_by_key(|e| e.created_at);
        Ok(out)
    }

    pub fn outbox_entry(&self, draft: &str) -> StoreResult<Option<OutboxEntry>> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT json FROM outbox WHERE draft = ?1", [draft], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// Everything held is due at `now`. Entries being sent, or removed
    /// meanwhile, are left alone.
    pub fn hurry_outbox(&self, now: DateTime<Utc>) -> StoreResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rows: Vec<String> = {
            let mut stmt = tx.prepare("SELECT json FROM outbox")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        for json in rows {
            let Ok(mut e) = serde_json::from_str::<OutboxEntry>(&json) else {
                continue;
            };
            if e.status == "held" {
                e.send_at = Some(now);
                tx.execute(
                    "UPDATE outbox SET json = ?2 WHERE draft = ?1",
                    params![e.draft, serde_json::to_string(&e)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Claim a due entry for sending (status held → sending), atomically, so
    /// two processes never send the same draft.
    pub fn claim_due(&self, now: DateTime<Utc>) -> StoreResult<Option<OutboxEntry>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let rows: Vec<String> = {
            let mut stmt = tx.prepare("SELECT json FROM outbox")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        let mut due: Vec<OutboxEntry> = rows
            .iter()
            .filter_map(|j| serde_json::from_str::<OutboxEntry>(j).ok())
            .filter(|e| e.status == "held" && e.send_at.is_some_and(|t| t <= now))
            .collect();
        due.sort_by_key(|e| e.send_at);
        let Some(mut e) = due.into_iter().next() else {
            return Ok(None);
        };
        e.status = "sending".into();
        tx.execute(
            "UPDATE outbox SET json = ?2 WHERE draft = ?1",
            params![e.draft, serde_json::to_string(&e)?],
        )?;
        tx.commit()?;
        Ok(Some(e))
    }

    /// The earliest time a held entry is due.
    pub fn next_due(&self) -> StoreResult<Option<DateTime<Utc>>> {
        Ok(self
            .outbox()?
            .into_iter()
            .filter(|e| e.status == "held")
            .filter_map(|e| e.send_at)
            .min())
    }

    pub fn remove_outbox(&self, draft: &str) -> StoreResult<()> {
        self.conn()
            .execute("DELETE FROM outbox WHERE draft = ?1", [draft])?;
        Ok(())
    }

    // ---- notes ------------------------------------------------------------

    /// A line to tell the user once, at the top of the next answer.
    pub fn push_note(&self, text: &str) -> StoreResult<()> {
        self.conn().execute(
            "INSERT INTO notes (created_at, text) VALUES (?1, ?2)",
            params![ts(Utc::now()), text],
        )?;
        Ok(())
    }

    pub fn take_notes(&self) -> StoreResult<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT text FROM notes ORDER BY seq")?;
        let notes: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        conn.execute("DELETE FROM notes", [])?;
        Ok(notes)
    }

    // ---- send log ---------------------------------------------------------

    pub fn record_sends(&self, n: usize) -> StoreResult<()> {
        let conn = self.conn();
        for _ in 0..n {
            conn.execute("INSERT INTO sends (at) VALUES (?1)", [ts(Utc::now())])?;
        }
        Ok(())
    }

    pub fn sends_since(&self, t: DateTime<Utc>) -> StoreResult<usize> {
        let n: i64 =
            self.conn()
                .query_row("SELECT COUNT(*) FROM sends WHERE at >= ?1", [ts(t)], |r| {
                    r.get(0)
                })?;
        Ok(n as usize)
    }

    // ---- people -----------------------------------------------------------

    pub fn set_people_hints(&self, hints: &[(String, String)]) -> StoreResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM people_hints", [])?;
        for (rank, (name, address)) in hints.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO people_hints (address, name, rank) VALUES (?1, ?2, ?3)",
                params![address.to_lowercase(), name, rank as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// People in the index whose name or address contains `q` (all people
    /// when `q` is empty), best known first.
    pub fn people(&self, q: &str, own: &[String], limit: usize) -> StoreResult<Vec<PersonRow>> {
        let own: Vec<String> = own.iter().map(|a| a.to_lowercase()).collect();
        let conn = self.conn();
        let mut map: std::collections::HashMap<String, PersonRow> =
            std::collections::HashMap::new();
        // Received from.
        {
            let mut stmt = conn.prepare(
                "SELECT from_addr, from_name, COUNT(*), MAX(received_at) FROM messages \
                 WHERE from_addr != '' AND folder != 'sent' GROUP BY from_addr",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (addr, name, n, last) = row?;
                let e = map.entry(addr.clone()).or_insert(PersonRow {
                    name: name.clone(),
                    address: addr,
                    sent_to: 0,
                    received_from: 0,
                    last_contact: None,
                });
                e.received_from += n as u32;
                if e.name.is_empty() {
                    e.name = name;
                }
                e.last_contact = e.last_contact.max(Some(from_ts(last)));
            }
        }
        // Sent to.
        {
            let mut stmt = conn.prepare(
                "SELECT to_json, cc_json, received_at FROM messages WHERE folder = 'sent'",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (to, cc, at) = row?;
                let mut all: Vec<MessageFrom> = serde_json::from_str(&to).unwrap_or_default();
                all.extend(serde_json::from_str::<Vec<MessageFrom>>(&cc).unwrap_or_default());
                for r in all {
                    let addr = r.address.to_lowercase();
                    if addr.is_empty() {
                        continue;
                    }
                    let e = map.entry(addr.clone()).or_insert(PersonRow {
                        name: r.name.clone(),
                        address: addr,
                        sent_to: 0,
                        received_from: 0,
                        last_contact: None,
                    });
                    e.sent_to += 1;
                    if e.name.is_empty() {
                        e.name = r.name.clone();
                    }
                    e.last_contact = e.last_contact.max(Some(from_ts(at)));
                }
            }
        }
        // Provider hints (Graph people).
        {
            let mut stmt = conn.prepare("SELECT address, name, rank FROM people_hints")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (addr, name, rank) = row?;
                let e = map.entry(addr.clone()).or_insert(PersonRow {
                    name: name.clone(),
                    address: addr,
                    sent_to: 0,
                    received_from: 0,
                    last_contact: None,
                });
                // A people-API rank counts like a few sent messages.
                e.sent_to += (5 - rank.min(5)) as u32;
                if e.name.is_empty() {
                    e.name = name;
                }
            }
        }
        let q = q.trim().to_lowercase();
        let mut out: Vec<PersonRow> = map
            .into_values()
            .filter(|p| !own.contains(&p.address))
            .filter(|p| {
                q.is_empty() || p.address.contains(&q) || p.name.to_lowercase().contains(&q)
            })
            .collect();
        out.sort_by(|a, b| {
            (b.sent_to * 3 + b.received_from)
                .cmp(&(a.sent_to * 3 + a.received_from))
                .then(b.last_contact.cmp(&a.last_contact))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// How many messages the user has sent to `address` in the index.
    pub fn sent_to_count(&self, address: &str) -> StoreResult<usize> {
        let like = format!("%\"{}\"%", address.to_lowercase());
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM messages WHERE folder = 'sent' AND (lower(to_json) LIKE ?1 OR lower(cc_json) LIKE ?1)",
            [like],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Threads the user has written in (sent a message to), for
    /// "reply to you" importance.
    pub fn threads_with_sent(&self) -> StoreResult<std::collections::HashSet<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT DISTINCT thread_id FROM messages WHERE folder = 'sent'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn item(
        account: &str,
        id: &str,
        folder: Folder,
        from: &str,
        subject: &str,
        ago_h: i64,
    ) -> MailItem {
        MailItem {
            account: account.into(),
            id: id.into(),
            thread_id: format!("t-{id}"),
            folder,
            from: MessageFrom {
                name: from.split('@').next().unwrap_or("").into(),
                address: from.into(),
            },
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            received_at: Utc::now() - Duration::hours(ago_h),
            preview: "preview text".into(),
            unread: true,
            flagged: false,
            has_attachments: false,
            categories: vec![],
            bulk_hint: Some(BulkHint::Other),
            list_unsubscribe: false,
            is_invite: false,
            message_id: String::new(),
        }
    }

    #[test]
    fn handles_are_short_stable_and_per_kind() {
        let s = Store::in_memory().unwrap();
        let a = s.handle(HandleKind::Message, "me@x", "AAA").unwrap();
        let b = s.handle(HandleKind::Message, "me@x", "BBB").unwrap();
        let again = s.handle(HandleKind::Message, "me@x", "AAA").unwrap();
        let e = s.handle(HandleKind::Event, "me@x", "AAA").unwrap();
        assert_eq!(
            (a.as_str(), b.as_str(), again.as_str(), e.as_str()),
            ("m1", "m2", "m1", "e1")
        );
        let t = s.resolve("m2").unwrap().unwrap();
        assert_eq!((t.account.as_str(), t.id.as_str()), ("me@x", "BBB"));
        assert!(s.resolve("m99").unwrap().is_none());
        assert!(s.resolve("x/../1").unwrap().is_none());
    }

    #[test]
    fn handles_count_past_nine_in_base36() {
        assert_eq!(to_base36(10), "a");
        assert_eq!(to_base36(36), "10");
    }

    #[test]
    fn upsert_keeps_classification_and_query_filters() {
        let s = Store::in_memory().unwrap();
        s.upsert_items(&[
            item("me@x", "1", Folder::Inbox, "news@shop.se", "Rea!", 1),
            item("me@x", "2", Folder::Inbox, "anna@x.se", "Avtal", 2),
            item("me@x", "3", Folder::Archive, "anna@x.se", "Gammalt", 3),
        ])
        .unwrap();
        s.set_classification(
            "me@x",
            "1",
            &Classification {
                kind: Some("promotion".into()),
                kind_source: Some("signal".into()),
                importance: 0,
                ..Default::default()
            },
        )
        .unwrap();
        // A refresh from the provider keeps pidge's judgement.
        s.upsert_items(&[item("me@x", "1", Folder::Inbox, "news@shop.se", "Rea!", 1)])
            .unwrap();
        let promos = s
            .query(&Filter {
                kinds: vec!["promotion".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(promos.len(), 1);
        assert_eq!(promos[0].class.importance, 0);
        let inbox = s
            .query(&Filter {
                folder: Some(Folder::Inbox),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            inbox.iter().map(|m| m.item.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "2"]
        );
        let anna = s
            .query(&Filter {
                from: Some("ANNA".into()),
                text: Some("avt".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(anna.len(), 1);
        let unclassified = s
            .query(&Filter {
                unclassified: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(unclassified.len(), 2);
    }

    #[test]
    fn local_changes_are_recorded_and_restored() {
        let s = Store::in_memory().unwrap();
        s.upsert_items(&[item("me@x", "1", Folder::Inbox, "a@b", "x", 1)])
            .unwrap();
        let to = MailChange::Move {
            to: Folder::Archive,
        };
        let before = s.apply_local("me@x", &["1".into()], &to, None).unwrap();
        assert_eq!(
            s.get("me@x", "1").unwrap().unwrap().item.folder,
            Folder::Archive
        );
        for e in &before {
            s.restore_local(e).unwrap();
        }
        assert_eq!(
            s.get("me@x", "1").unwrap().unwrap().item.folder,
            Folder::Inbox
        );
    }

    #[test]
    fn a_change_on_its_way_survives_a_sync_until_it_is_finished() {
        let s = Store::in_memory().unwrap();
        let i = item("me@x", "1", Folder::Inbox, "a@b", "x", 1);
        s.upsert_items(std::slice::from_ref(&i)).unwrap();
        let seq = s.enqueue("q1", "archive", Utc::now(), "{}").unwrap();
        let to = MailChange::Move {
            to: Folder::Archive,
        };
        s.apply_local("me@x", &["1".into()], &to, Some(seq))
            .unwrap();
        s.upsert_items(std::slice::from_ref(&i)).unwrap();
        let folder = || s.get("me@x", "1").unwrap().unwrap().item.folder;
        assert_eq!(folder(), Folder::Archive);

        s.finish_queued(seq, "done").unwrap();
        s.upsert_items(std::slice::from_ref(&i)).unwrap();
        assert_eq!(folder(), Folder::Inbox);
    }

    #[test]
    fn an_instruction_waits_for_an_earlier_one_on_the_same_message() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let late = now + Duration::seconds(60);
        let one = s.enqueue("q1", "delete", late, "1").unwrap();
        s.touch(one, "me@x", "1").unwrap();
        s.release_queued(one).unwrap();
        let two = s.enqueue("q2", "archive", now, "2").unwrap();
        s.touch(two, "me@x", "1").unwrap();
        s.release_queued(two).unwrap();
        let other = s.enqueue("q3", "archive", now, "3").unwrap();
        s.touch(other, "me@x", "2").unwrap();
        s.release_queued(other).unwrap();

        assert_eq!(s.claim_queued(now).unwrap().unwrap().id, "q3");
        assert!(s.claim_queued(now).unwrap().is_none());
        assert_eq!(s.claim_queued(late).unwrap().unwrap().id, "q1");
        assert!(s.claim_queued(late).unwrap().is_none());
        s.finish_queued(one, "done").unwrap();
        assert_eq!(s.claim_queued(late).unwrap().unwrap().id, "q2");
        assert!(!s.cancel_queued(two).unwrap(), "it has started");
    }

    #[test]
    fn what_a_stopped_process_left_half_done_holds_nothing_up() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let one = s.enqueue("q1", "archive", now, "1").unwrap();
        s.touch(one, "me@x", "1").unwrap();
        s.release_queued(one).unwrap();
        s.claim_queued(now).unwrap().unwrap();
        let two = s.enqueue("q2", "archive", now, "2").unwrap();
        s.touch(two, "me@x", "1").unwrap();
        s.release_queued(two).unwrap();
        assert!(s.claim_queued(now).unwrap().is_none());
        assert_eq!(s.abandon_stuck(now).unwrap(), 1);
        assert_eq!(s.claim_queued(now).unwrap().unwrap().id, "q2");
    }

    fn synced(s: &Store) {
        s.save_sync(&SyncState {
            account: "me@x".into(),
            cursor: Some("c".into()),
            synced_at: Some(Utc::now()),
            window_start: Some(Utc::now() - Duration::days(90)),
            complete: true,
        })
        .unwrap();
    }

    fn with_message_id(mut i: MailItem, id: &str) -> MailItem {
        i.message_id = id.into();
        i
    }

    #[test]
    fn a_search_hit_the_index_knows_by_message_id_is_that_message() {
        let s = Store::in_memory().unwrap();
        let known = with_message_id(item("me@x", "A", Folder::Inbox, "a@b", "x", 5), "<1@b>");
        s.upsert_items(std::slice::from_ref(&known)).unwrap();
        synced(&s);
        let mut hit = known.clone();
        hit.id = "B".into();
        hit.unread = false;
        assert_eq!(s.adopt_found(&[hit]).unwrap(), vec!["A".to_string()]);
        assert_eq!(s.query(&Filter::default()).unwrap().len(), 1);
        assert!(s.get("me@x", "A").unwrap().unwrap().item.unread);
    }

    #[test]
    fn a_stale_search_hit_is_left_out_and_an_old_one_is_taken_in() {
        let s = Store::in_memory().unwrap();
        synced(&s);
        let stale = with_message_id(item("me@x", "S", Folder::Inbox, "a@b", "x", 5), "<s@b>");
        let old = with_message_id(
            item("me@x", "O", Folder::Inbox, "a@b", "x", 24 * 200),
            "<o@b>",
        );
        let filed = with_message_id(
            item("me@x", "F", Folder::Other("f".into()), "a@b", "x", 5),
            "<f@b>",
        );
        assert_eq!(
            s.adopt_found(&[stale, old, filed]).unwrap(),
            vec!["O".to_string(), "F".to_string()]
        );
    }

    #[test]
    fn a_sync_takes_over_a_row_a_search_found_first_and_keeps_its_handle() {
        let s = Store::in_memory().unwrap();
        let hit = with_message_id(item("me@x", "B", Folder::Inbox, "a@b", "x", 5), "<1@b>");
        s.adopt_found(std::slice::from_ref(&hit)).unwrap();
        let mut real = hit.clone();
        real.id = "A".into();
        let h = s.upsert_items(&[real]).unwrap();
        assert_eq!(h, vec!["m1".to_string()]);
        assert_eq!(s.query(&Filter::default()).unwrap().len(), 1);
        assert_eq!(s.resolve("m1").unwrap().unwrap().id, "A");
    }

    #[test]
    fn a_new_database_never_repeats_an_old_handle() {
        let s = Store::in_memory().unwrap();
        assert!(s.is_fresh().unwrap());
        s.start_handles_at(40_000).unwrap();
        assert!(!s.is_fresh().unwrap());
        let h = s.handle(HandleKind::Message, "me@x", "AAA").unwrap();
        assert_eq!(h, format!("m{}", to_base36(40_000)));
    }

    #[test]
    fn words_match_in_any_alternative_and_any_case() {
        let s = Store::in_memory().unwrap();
        s.upsert_items(&[
            item("me@x", "1", Folder::Inbox, "no@dhl.se", "Åter på väg", 1),
            item("me@x", "2", Folder::Inbox, "npm@npm.io", "Published", 2),
            item("me@x", "3", Folder::Inbox, "a@b.se", "Hej", 3),
        ])
        .unwrap();
        let ids = |f: Filter| -> Vec<String> {
            s.query(&f)
                .unwrap()
                .into_iter()
                .map(|m| m.item.id)
                .collect()
        };
        assert_eq!(
            ids(Filter {
                text: Some("åter väg OR published".into()),
                ..Default::default()
            }),
            vec!["1", "2"]
        );
        assert_eq!(
            ids(Filter {
                from: Some("npm, dhl".into()),
                ..Default::default()
            }),
            vec!["1", "2"]
        );
    }

    #[test]
    fn outbox_claims_each_due_entry_once() {
        let s = Store::in_memory().unwrap();
        let entry = OutboxEntry {
            draft: "d1".into(),
            account: "me@x".into(),
            draft_id: "D".into(),
            send_at: Some(Utc::now() - Duration::seconds(1)),
            status: "held".into(),
            summary: "to Anna".into(),
            reasons: vec![],
            error: None,
            created_at: Utc::now(),
        };
        s.put_outbox(&entry).unwrap();
        assert!(s.claim_due(Utc::now()).unwrap().is_some());
        assert!(s.claim_due(Utc::now()).unwrap().is_none());
    }

    #[test]
    fn docs_expire_and_notes_are_taken_once() {
        let s = Store::in_memory().unwrap();
        s.put_doc(
            "plan",
            "p1",
            &serde_json::json!({"a": 1}),
            Some(Utc::now() - Duration::seconds(1)),
        )
        .unwrap();
        assert!(
            s.get_doc::<serde_json::Value>("plan", "p1")
                .unwrap()
                .is_none()
        );
        s.put_doc("plan", "p2", &serde_json::json!({"a": 2}), None)
            .unwrap();
        assert_eq!(
            s.latest_doc::<serde_json::Value>("plan")
                .unwrap()
                .unwrap()
                .0,
            "p2"
        );
        s.push_note("archiving failed").unwrap();
        assert_eq!(
            s.take_notes().unwrap(),
            vec!["archiving failed".to_string()]
        );
        assert!(s.take_notes().unwrap().is_empty());
    }

    #[test]
    fn people_rank_sent_to_above_received_from() {
        let s = Store::in_memory().unwrap();
        let mut sent = item("me@x", "s1", Folder::Sent, "me@x", "Hej", 1);
        sent.to = vec![MessageFrom {
            name: "Bob".into(),
            address: "bob@x.se".into(),
        }];
        s.upsert_items(&[
            sent,
            item("me@x", "1", Folder::Inbox, "news@shop.se", "a", 1),
            item("me@x", "2", Folder::Inbox, "news@shop.se", "b", 1),
        ])
        .unwrap();
        let people = s.people("", &["me@x".into()], 10).unwrap();
        assert_eq!(people[0].address, "bob@x.se");
        assert_eq!(s.sent_to_count("BOB@x.se").unwrap(), 1);
        assert!(s.threads_with_sent().unwrap().contains("t-s1"));
    }
}
