//! The user's durable preferences: categories, rules, VIPs, what "seen"
//! means, policy, timezone. Small, JSON, and the only state pidge must not
//! lose. Locally a file next to `config.yaml`; hosted, a blob per user.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kinds::Kind;

/// Where preferences are kept.
#[async_trait]
pub trait PrefsStore: Send + Sync {
    async fn load(&self) -> Result<Option<Prefs>, String>;
    async fn save(&self, prefs: &Prefs) -> Result<(), String>;
}

/// Preferences in a JSON file (the CLI and local MCP).
pub struct FilePrefs {
    path: PathBuf,
}

impl FilePrefs {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl PrefsStore for FilePrefs {
    async fn load(&self) -> Result<Option<Prefs>, String> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| format!("{}: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    async fn save(&self, prefs: &Prefs) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json)
            .await
            .map_err(|e| e.to_string())?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Preferences kept in memory only (tests).
#[derive(Default)]
pub struct MemoryPrefs(std::sync::Mutex<Option<Prefs>>);

#[async_trait]
impl PrefsStore for MemoryPrefs {
    async fn load(&self) -> Result<Option<Prefs>, String> {
        Ok(self.0.lock().map_err(|e| e.to_string())?.clone())
    }
    async fn save(&self, prefs: &Prefs) -> Result<(), String> {
        *self.0.lock().map_err(|e| e.to_string())? = Some(prefs.clone());
        Ok(())
    }
}

/// An example message for a category, kept as the classifier sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Example {
    pub from: String,
    pub subject: String,
}

/// One of the user's categories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Category {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `sender:<address>` or `domain:<domain>`, lower-cased.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Example>,
    /// Built-in kinds that land here when nothing more specific decides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<Kind>,
    /// Offered as one decision in a triage sweep.
    #[serde(default)]
    pub sweep: bool,
}

impl Category {
    pub fn new(name: &str, description: &str, kinds: &[Kind], sweep: bool) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            rules: Vec::new(),
            examples: Vec::new(),
            kinds: kinds.to_vec(),
            sweep,
        }
    }
}

/// The built-in name for mail that fits no category.
pub const OTHER: &str = "Other";

/// The categories offered until the user makes their own.
pub fn default_categories() -> Vec<Category> {
    vec![
        Category::new(
            "Important",
            "Needs the user's own attention or action soon: a question to them, a decision, a deadline, a problem",
            &[],
            false,
        ),
        Category::new(
            "Personal",
            "From friends and family, about private life, not work or commerce",
            &[Kind::Personal],
            false,
        ),
        Category::new(
            "Bills and receipts",
            "Invoices, payment reminders, receipts and order confirmations with an amount",
            &[Kind::Receipt],
            false,
        ),
        Category::new(
            "Newsletters",
            "Subscribed editorial content and digests: articles, updates from organisations the user follows",
            &[Kind::Newsletter],
            true,
        ),
        Category::new(
            "Promotions",
            "Marketing, offers, sales, discounts and advertising",
            &[Kind::Promotion],
            true,
        ),
        Category::new(
            "Notifications",
            "Automated messages from services and systems: alerts, confirmations of account activity, status and shipping updates, social network notices",
            &[Kind::Notification, Kind::Social],
            true,
        ),
    ]
}

/// What a policy class allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Act at once.
    Allow,
    /// Return a plan; act when it is applied.
    Confirm,
    /// Never.
    Deny,
}

/// How sending works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    /// Held in the outbox for a time that grows with risk (the default).
    Hold,
    /// Only an applied plan sends.
    Confirm,
    /// At once.
    Allow,
    Deny,
}

/// The user's policy, one line per action class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Policy {
    pub send: SendMode,
    /// Calendar changes that e-mail other people: invitations, attendee
    /// changes, time changes of meetings, cancellations. (RSVPs and
    /// unsubscribe e-mails are not planned, but count against the cap.)
    pub notify: Mode,
    /// Changes to more than `bulk_threshold` messages or events, and every
    /// selector-based change.
    pub bulk: Mode,
    /// Deleting a single message (always to Deleted Items).
    pub delete: Mode,
    pub bulk_threshold: usize,
    /// Outgoing messages (sends and notifications) per rolling hour.
    pub sends_per_hour: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            send: SendMode::Hold,
            notify: Mode::Confirm,
            bulk: Mode::Confirm,
            delete: Mode::Allow,
            bulk_threshold: 5,
            sends_per_hour: 30,
        }
    }
}

/// Non-secret state of the user's own AI key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiState {
    /// off, ok, no_credits, key_rejected, failing, unavailable
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
}

/// Everything durable about one user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// IANA zone; the machine's (locally) or Europe/Stockholm (hosted) when absent.
    pub timezone: Option<String>,
    /// Address new mail and events go out from.
    pub default_sender: Option<String>,
    /// The user's categories; `None` until they make their own (defaults apply).
    pub categories: Option<Vec<Category>>,
    pub categories_confirmed: bool,
    /// Senders (addresses or `domain:x`) that always matter.
    pub vips: Vec<String>,
    /// Senders that never matter.
    pub muted: Vec<String>,
    /// Per mailbox: everything received before this was reported as seen.
    pub seen: BTreeMap<String, DateTime<Utc>>,
    pub last_triage: Option<DateTime<Utc>>,
    pub policy: Policy,
    /// Write pidge's categories onto the mail in the mailboxes.
    pub mailbox_sync: bool,
    /// Folder receipts are filed into (`/`-separated); archive when absent.
    pub receipts_folder: Option<String>,
    /// Working hours for availability: [start hour, end hour].
    pub working_hours: Option<[u32; 2]>,
    pub ai: AiState,
    /// Days of mail kept in the index.
    pub window_days: Option<u32>,
}

impl Prefs {
    pub fn categories(&self) -> Vec<Category> {
        self.categories.clone().unwrap_or_else(default_categories)
    }

    pub fn window_days(&self) -> i64 {
        i64::from(self.window_days.unwrap_or(90).clamp(7, 365))
    }

    /// The earliest "seen" watermark of the given mailboxes.
    pub fn seen_since(&self, accounts: &[String]) -> Option<DateTime<Utc>> {
        accounts
            .iter()
            .filter_map(|a| self.seen.get(a))
            .min()
            .copied()
    }

    pub fn is_vip(&self, address: &str) -> bool {
        matches_any(&self.vips, address)
    }

    pub fn is_muted(&self, address: &str) -> bool {
        matches_any(&self.muted, address)
    }
}

/// Does `address` match one of `rules` (`addr`, `sender:addr`, `domain:d`)?
pub fn matches_any(rules: &[String], address: &str) -> bool {
    let address = address.to_ascii_lowercase();
    let domain = address.rsplit('@').next().unwrap_or("");
    rules.iter().any(|r| {
        let r = r.trim().to_ascii_lowercase();
        if let Some(d) = r.strip_prefix("domain:") {
            domain == d || domain.ends_with(&format!(".{d}"))
        } else {
            r.strip_prefix("sender:").unwrap_or(&r) == address
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_match_senders_and_domains() {
        let rules = vec!["domain:klatterklubben.se".into(), "anna@x.se".into()];
        assert!(matches_any(&rules, "info@klatterklubben.se"));
        assert!(matches_any(&rules, "a@news.klatterklubben.se"));
        assert!(matches_any(&rules, "ANNA@x.se"));
        assert!(!matches_any(&rules, "bob@x.se"));
        assert!(!matches_any(&rules, "a@notklatterklubben.se"));
    }

    #[tokio::test]
    async fn file_prefs_round_trip() {
        let dir = std::env::temp_dir().join(format!("pidge-prefs-{}", std::process::id()));
        let store = FilePrefs::new(dir.join("prefs.json"));
        assert!(store.load().await.unwrap().is_none());
        let p = Prefs {
            vips: vec!["anna@x.se".into()],
            ..Default::default()
        };
        store.save(&p).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap(), p);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn defaults_sweep_bulk_kinds() {
        let sweep: Vec<String> = default_categories()
            .into_iter()
            .filter(|c| c.sweep)
            .map(|c| c.name)
            .collect();
        assert_eq!(sweep, ["Newsletters", "Promotions", "Notifications"]);
    }
}
