//! Mailbox categories: pidge's categories, when the user turns it on,
//! written to their mailboxes too, as the mailbox's own (master) categories
//! and on the messages a review or `sort_mail` sorted.
//!
//! pidge's list stays the source of truth (one list across the user's
//! mailboxes). Per mailbox, the record links each pidge category to the
//! mailbox category (id, last seen name and colour), so a sync can tell
//! what the user changed in the mailbox: a recolour is adopted everywhere,
//! a removal is respected and asked about, a rename is asked about. The
//! user's own categories, and the colour-only defaults most mailboxes come
//! with, are never touched.
//!
//! Every Graph call goes through `$batch` for the caller's own mailboxes;
//! category names reach Graph only inside JSON bodies or as escaped OData
//! string literals, never in a URL path (paths carry Graph's own ids,
//! checked with `check_id`). Graph error bodies are never surfaced.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use pidge_client::graph::batch::BatchRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::categories::{Category, OTHER};
use super::classify::Candidate;
use super::graph::encode;
use crate::render::one_line;
use crate::state::SharedState;
use crate::tools::mail_read::check_id;
use crate::users::{UserRecord, log_store_error, user_hash};

/// A mailbox's categories are read at most this often (edits force it).
pub const SYNC_EVERY: Duration = Duration::minutes(10);
/// Messages retagged per mailbox when a category is renamed or removed.
pub const RETAG_CAP: usize = 500;
/// Pending questions kept for the user.
const MAX_NOTES: usize = 10;
/// The Graph scope that creating and colouring mailbox categories needs.
pub const SCOPE: &str = "MailboxSettings.ReadWrite";

/// Graph's 25 category colours, `preset0`..`preset24`.
pub const PRESETS: usize = 25;

fn is_false(b: &bool) -> bool {
    !*b
}

/// Mailbox-category sync state on the user record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxSync {
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    /// Mailboxes the user left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    /// The one-time "categories can go on the mail too" note was shown.
    #[serde(default, skip_serializing_if = "is_false")]
    pub offered: bool,
    /// pidge's colour per category (a `presetN`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mailboxes: BTreeMap<String, MailboxState>,
    /// Questions a sync found for the user, each shown once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<DateTime<Utc>>,
    /// The mailbox's sign-in lacks [`SCOPE`]: messages are tagged, but its
    /// categories can't be listed, created or coloured.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_scope: bool,
    /// pidge category name → the mailbox category it is written as.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub links: BTreeMap<String, Link>,
    /// pidge categories the user deleted in this mailbox: not written here
    /// until they say so (`restore`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

/// A pidge category's counterpart in one mailbox, as last seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub name: String,
    pub color: String,
    /// pidge created it (rather than finding one of the user's own).
    #[serde(default, skip_serializing_if = "is_false")]
    pub created: bool,
}

impl MailboxSync {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether pidge writes categories to `mailbox`.
    pub fn active(&self, mailbox: &str) -> bool {
        self.enabled
            && !self
                .excluded
                .iter()
                .any(|m| m.eq_ignore_ascii_case(mailbox))
    }

    /// The name category `name` carries in `mailbox`: the linked mailbox
    /// category's (it may have been renamed there), else pidge's.
    pub fn name_in(&self, mailbox: &str, name: &str) -> String {
        self.mailboxes
            .get(mailbox)
            .and_then(|s| s.links.get(name))
            .map_or_else(|| name.to_string(), |l| l.name.clone())
    }

    fn removed_in(&self, mailbox: &str, name: &str) -> bool {
        self.mailboxes
            .get(mailbox)
            .is_some_and(|s| s.removed.iter().any(|r| r.eq_ignore_ascii_case(name)))
    }

    /// Every name pidge manages in `mailbox` (lower-cased): its categories
    /// and what they are called there.
    fn managed_in(&self, mailbox: &str, categories: &[Category]) -> HashSet<String> {
        let mut names: HashSet<String> = categories.iter().map(|c| c.name.to_lowercase()).collect();
        if let Some(s) = self.mailboxes.get(mailbox) {
            names.extend(s.links.values().map(|l| l.name.to_lowercase()));
        }
        names
    }

    /// The `accounts_list` remark for `mailbox`, when sync can't do
    /// everything there.
    pub fn account_note(&self, mailbox: &str) -> Option<&'static str> {
        (self.active(mailbox) && self.mailboxes.get(mailbox).is_some_and(|s| s.no_scope)).then_some(
            "categories: needs accounts_connect once, so pidge can create and colour its categories there (until then it only tags the mail)",
        )
    }

    fn note(&mut self, text: String) {
        if !self.notes.contains(&text) {
            self.notes.push(text);
        }
        while self.notes.len() > MAX_NOTES {
            self.notes.remove(0);
        }
    }
}

/// The colour-only categories mailboxes come with ("Red category",
/// "Röd kategori", "Blue", …), in English and Swedish. Never offered for
/// adoption, never linked, never touched.
pub fn is_colour_default(name: &str) -> bool {
    const COLOURS: &[&str] = &[
        // The presets in English, and the six classic defaults.
        "red",
        "orange",
        "brown",
        "peach",
        "yellow",
        "green",
        "teal",
        "olive",
        "blue",
        "purple",
        "cranberry",
        "steel",
        "darksteel",
        "gray",
        "grey",
        "darkgray",
        "darkgrey",
        "black",
        "darkred",
        "darkorange",
        "darkbrown",
        "darkyellow",
        "darkgreen",
        "darkteal",
        "darkolive",
        "darkblue",
        "darkpurple",
        "darkcranberry", // Swedish.
        "röd",
        "brun",
        "persika",
        "gul",
        "grön",
        "turkos",
        "blågrön",
        "oliv",
        "olivgrön",
        "blå",
        "lila",
        "tranbär",
        "lingon",
        "stål",
        "mörkstål",
        "grå",
        "mörkgrå",
        "svart",
        "mörkröd",
        "mörkorange",
        "mörkbrun",
        "mörkgul",
        "mörkgrön",
        "mörkturkos",
        "mörkoliv",
        "mörkblå",
        "mörklila",
        "mörktranbär",
    ];
    let words: Vec<String> = name
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|w| !matches!(w.as_str(), "category" | "kategori" | "kategorin"))
        .collect();
    let joined: String = words.concat().replace('-', "");
    !joined.is_empty() && COLOURS.contains(&joined.as_str())
}

/// The colour a preset index names.
fn preset(i: usize) -> String {
    format!("preset{i}")
}

/// A colour for a new mailbox category: one no colour-only default uses,
/// preferably one no other category uses either.
pub fn pick_colour(masters: &[Master]) -> String {
    let used_by = |pred: &dyn Fn(&Master) -> bool| -> HashSet<String> {
        masters
            .iter()
            .filter(|m| pred(m))
            .map(|m| m.color.clone())
            .collect()
    };
    let defaults = used_by(&|m| is_colour_default(&m.name));
    let all = used_by(&|_| true);
    (0..PRESETS)
        .map(preset)
        .find(|c| !all.contains(c))
        .or_else(|| (0..PRESETS).map(preset).find(|c| !defaults.contains(c)))
        .unwrap_or_else(|| preset(0))
}

/// An OData string literal: the value in single quotes, quotes doubled.
pub fn odata_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The filter for messages carrying category `name`.
pub fn category_filter(name: &str) -> String {
    format!("categories/any(c:c eq {})", odata_string(name))
}

/// A mailbox category as Graph lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Master {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// Why a mailbox's categories could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    /// 403: the sign-in lacks [`SCOPE`].
    NoScope,
    Failed,
}

const MASTERS: &str = "/me/outlook/masterCategories";

async fn one(state: &SharedState, account: &str, req: BatchRequest) -> Result<Value, SyncError> {
    let r = state
        .graph
        .batch_all(account, vec![req])
        .await
        .map_err(|_| SyncError::Failed)?;
    match r.into_iter().next() {
        Some(r) if r.is_success() => Ok(r.body),
        Some(r) if r.status == 403 => Err(SyncError::NoScope),
        _ => Err(SyncError::Failed),
    }
}

fn master(v: &Value) -> Option<Master> {
    let id = v.get("id")?.as_str()?.to_string();
    // Ids go back into Graph paths.
    check_id(&id).ok()?;
    Some(Master {
        id,
        name: one_line(v.get("displayName")?.as_str()?),
        color: v
            .get("color")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string(),
    })
}

pub async fn list_masters(state: &SharedState, account: &str) -> Result<Vec<Master>, SyncError> {
    let body = one(state, account, BatchRequest::bare("0", "GET", MASTERS)).await?;
    Ok(body
        .get("value")
        .and_then(Value::as_array)
        .map(|vs| vs.iter().filter_map(master).collect())
        .unwrap_or_default())
}

async fn create_master(
    state: &SharedState,
    account: &str,
    name: &str,
    color: &str,
) -> Result<Master, SyncError> {
    let body = one(
        state,
        account,
        BatchRequest::json(
            "0",
            "POST",
            MASTERS,
            json!({ "displayName": name, "color": color }),
        ),
    )
    .await?;
    master(&body).ok_or(SyncError::Failed)
}

async fn recolour_master(
    state: &SharedState,
    account: &str,
    id: &str,
    color: &str,
) -> Result<(), SyncError> {
    check_id(id).map_err(|_| SyncError::Failed)?;
    one(
        state,
        account,
        BatchRequest::json(
            "0",
            "PATCH",
            format!("{MASTERS}/{id}"),
            json!({ "color": color }),
        ),
    )
    .await
    .map(|_| ())
}

async fn delete_master(state: &SharedState, account: &str, id: &str) -> Result<(), SyncError> {
    check_id(id).map_err(|_| SyncError::Failed)?;
    one(
        state,
        account,
        BatchRequest::bare("0", "DELETE", format!("{MASTERS}/{id}")),
    )
    .await
    .map(|_| ())
}

/// Up to [`RETAG_CAP`] messages carrying category `name`: `(id, categories)`.
async fn messages_with(
    state: &SharedState,
    account: &str,
    name: &str,
) -> Result<Vec<(String, Vec<String>)>, SyncError> {
    let url = format!(
        "/me/messages?$filter={}&$top={RETAG_CAP}&$select=id,categories",
        encode(&category_filter(name))
    );
    let body = one(state, account, BatchRequest::bare("0", "GET", url)).await?;
    Ok(body
        .get("value")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let id = r.get("id")?.as_str()?.to_string();
                    check_id(&id).ok()?;
                    let cats = r
                        .get("categories")?
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    Some((id, cats))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Sets each message's categories, one `$batch` for the mailbox. Returns
/// how many were written.
async fn write_categories(
    state: &SharedState,
    account: &str,
    changes: Vec<(String, Vec<String>)>,
) -> usize {
    if changes.is_empty() {
        return 0;
    }
    let requests = changes
        .iter()
        .enumerate()
        .map(|(i, (id, cats))| {
            BatchRequest::json(
                i.to_string(),
                "PATCH",
                format!("/me/messages/{id}"),
                json!({ "categories": cats }),
            )
        })
        .collect();
    match state.graph.batch_all(account, requests).await {
        Ok(rs) => rs.iter().filter(|r| r.is_success()).count(),
        Err(_) => 0,
    }
}

/// One user's mailbox-category work runs one task at a time, so a rename,
/// a sync and a review's tagging never interleave.
#[derive(Default)]
pub struct Locks {
    map: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Locks {
    pub fn for_user(&self, user: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.map
            .lock()
            .expect("sync locks")
            .entry(user.to_ascii_lowercase())
            .or_default()
            .clone()
    }
}

/// What a sync found, for the response that asked for it.
#[derive(Debug, Default)]
pub struct Report {
    /// `(mailbox, name)`: the user's own named categories, offered for adoption.
    pub own: Vec<(String, String)>,
    pub created: usize,
    pub linked: usize,
}

/// Whether the stored sign-in of `mailbox` (owned by `user`) was granted
/// [`SCOPE`]. `None` when the mailbox isn't the user's or can't be read.
async fn granted(state: &SharedState, user: &str, mailbox: &str) -> Option<bool> {
    match state.users.load_mailbox(mailbox).await {
        Ok(Some(rec)) if rec.owner.eq_ignore_ascii_case(user) => Some(
            rec.tokens
                .scope
                .as_deref()
                .is_some_and(|g| pidge_client::auth::config::grants(g, SCOPE)),
        ),
        Ok(_) => None,
        Err(e) => {
            log_store_error("loading mailbox record", mailbox, &e);
            None
        }
    }
}

/// Brings the user's mailboxes in line with pidge's categories (see the
/// module docs). `force` ignores [`SYNC_EVERY`]; `only` limits it to one
/// mailbox. The caller holds the user's [`Locks`] entry.
pub async fn sync(state: &SharedState, user: &str, force: bool, only: Option<&str>) -> Report {
    let mut report = Report::default();
    let Some(mut record) = load_owned(state, user).await else {
        return report;
    };
    let categories = record.review.categories();
    let now = Utc::now();
    let ms = &mut record.review.mailbox_sync;
    if !ms.enabled {
        return report;
    }

    // Read every due mailbox's categories.
    let mut fetched: Vec<(String, Vec<Master>)> = Vec::new();
    for mailbox in record.mailboxes.clone() {
        if !ms.active(&mailbox) || only.is_some_and(|o| !o.eq_ignore_ascii_case(&mailbox)) {
            continue;
        }
        let st = ms.mailboxes.entry(mailbox.clone()).or_default();
        if !force && st.synced_at.is_some_and(|t| now - t < SYNC_EVERY) {
            continue;
        }
        st.synced_at = Some(now);
        let listed = match granted(state, user, &mailbox).await {
            None => continue,
            Some(false) => Err(SyncError::NoScope),
            Some(true) => list_masters(state, &mailbox).await,
        };
        match listed {
            Ok(masters) => {
                st.no_scope = false;
                fetched.push((mailbox, masters));
            }
            Err(SyncError::NoScope) => {
                if !st.no_scope {
                    st.no_scope = true;
                    ms.note(format!(
                        "note (tell the user once): {} needs accounts_connect once before pidge can create and colour its categories there; until then pidge only tags the mail.",
                        one_line(&mailbox)
                    ));
                }
            }
            Err(SyncError::Failed) => {
                tracing::warn!(user = %user_hash(user), "category sync: a mailbox could not be read");
            }
        }
    }

    // What the user changed in each mailbox since the last sync.
    let mut recoloured: BTreeMap<String, String> = BTreeMap::new();
    for (mailbox, masters) in &fetched {
        let st = ms.mailboxes.entry(mailbox.clone()).or_default();
        let mut notes = Vec::new();
        for (name, link) in st.links.clone() {
            if !categories.iter().any(|c| c.name == name) {
                // No longer a pidge category: stop managing it.
                st.links.remove(&name);
                continue;
            }
            match masters.iter().find(|m| m.id == link.id) {
                None => {
                    st.links.remove(&name);
                    if !st.removed.contains(&name) {
                        st.removed.push(name.clone());
                    }
                    notes.push(format!(
                        "note (ask the user once): the category {name} was removed in {mb}; should it come back there (mail_categories action=mailbox_sync restore={name} account={mb}) or go from pidge entirely (mail_categories action=remove name={name})?",
                        name = one_line(&name),
                        mb = one_line(mailbox)
                    ));
                }
                Some(m) => {
                    let l = st.links.get_mut(&name).expect("present");
                    if m.name != l.name {
                        if !m.name.eq_ignore_ascii_case(&name) {
                            notes.push(format!(
                                "note (ask the user once): the category {old} was renamed to {new} in {mb}; rename it everywhere (mail_categories action=update name={old} new_name={new})?",
                                old = one_line(&name),
                                new = one_line(&m.name),
                                mb = one_line(mailbox)
                            ));
                        }
                        l.name = m.name.clone();
                    }
                    if m.color != l.color {
                        l.color = m.color.clone();
                        recoloured.insert(name.clone(), m.color.clone());
                    }
                }
            }
        }
        for n in notes {
            ms.note(n);
        }
    }
    // Last change wins: pidge adopts the colour, and the other mailboxes follow.
    for (name, color) in &recoloured {
        ms.colors.insert(name.clone(), color.clone());
    }

    for (mailbox, masters) in &fetched {
        for c in &categories {
            let st = ms.mailboxes.entry(mailbox.clone()).or_default();
            if st.removed.contains(&c.name) {
                continue;
            }
            if let Some(link) = st.links.get_mut(&c.name) {
                if let Some(color) = recoloured.get(&c.name)
                    && *color != link.color
                    && recolour_master(state, mailbox, &link.id, color)
                        .await
                        .is_ok()
                {
                    link.color = color.clone();
                }
                continue;
            }
            let same_name = masters
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(&c.name));
            if let Some(m) = same_name {
                if is_colour_default(&m.name) {
                    continue;
                }
                // The user's own category of that name: linked, colour kept.
                st.links.insert(
                    c.name.clone(),
                    Link {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        color: m.color.clone(),
                        created: false,
                    },
                );
                ms.colors
                    .entry(c.name.clone())
                    .or_insert_with(|| m.color.clone());
                report.linked += 1;
                continue;
            }
            let color = ms
                .colors
                .get(&c.name)
                .cloned()
                .unwrap_or_else(|| pick_colour(masters));
            match create_master(state, mailbox, &c.name, &color).await {
                Ok(m) => {
                    let st = ms.mailboxes.entry(mailbox.clone()).or_default();
                    st.links.insert(
                        c.name.clone(),
                        Link {
                            id: m.id,
                            name: m.name,
                            color: color.clone(),
                            created: true,
                        },
                    );
                    ms.colors.insert(c.name.clone(), color);
                    report.created += 1;
                }
                Err(_) => {
                    tracing::warn!(user = %user_hash(user), "category sync: a category could not be created");
                }
            }
        }
        // The user's own named categories, for the adoption offer.
        let st = &ms.mailboxes[mailbox];
        for m in masters {
            let linked = st.links.values().any(|l| l.id == m.id);
            let pidge = categories
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(&m.name));
            if !linked
                && !pidge
                && !is_colour_default(&m.name)
                && !m.name.eq_ignore_ascii_case(OTHER)
            {
                report.own.push((mailbox.clone(), m.name.clone()));
            }
        }
    }

    let synced: Vec<String> = ms.mailboxes.keys().cloned().collect();
    let ms = record.review.mailbox_sync.clone();
    save_sync(state, user, &synced, ms).await;
    report
}

/// The user's record, fresh from the store.
/// [`load`], with `mailboxes` narrowed to those whose stored record names
/// `user` as owner (as `ToolContext` does): the sync, rename and remove
/// loops reach a mailbox's tokens through this list. Never saved as is.
async fn load_owned(state: &SharedState, user: &str) -> Option<UserRecord> {
    let mut record = load(state, user).await?;
    let mut owned = Vec::with_capacity(record.mailboxes.len());
    for mailbox in std::mem::take(&mut record.mailboxes) {
        match state.token_backend.owner_of(&mailbox).await {
            Ok(Some(owner)) if !owner.eq_ignore_ascii_case(user) => {}
            Ok(_) => owned.push(mailbox),
            Err(e) => {
                log_store_error("checking mailbox owner", &mailbox, &e);
                return None;
            }
        }
    }
    record.mailboxes = owned;
    Some(record)
}

async fn load(state: &SharedState, user: &str) -> Option<UserRecord> {
    match state.users.load(user).await {
        Ok(r) => r,
        Err(e) => {
            log_store_error("loading user record", user, &e);
            None
        }
    }
}

/// Saves the sync state of `mailboxes` (and the colours and notes) onto
/// the freshest record, leaving everything else as it is there.
async fn save_sync(state: &SharedState, user: &str, mailboxes: &[String], ms: MailboxSync) {
    let Some(mut rec) = load(state, user).await else {
        return;
    };
    let target = &mut rec.review.mailbox_sync;
    for m in mailboxes {
        match ms.mailboxes.get(m) {
            Some(s) => target.mailboxes.insert(m.clone(), s.clone()),
            None => target.mailboxes.remove(m),
        };
    }
    target.colors = ms.colors;
    for n in ms.notes {
        target.note(n);
    }
    if let Err(e) = state.users.save(&rec).await {
        log_store_error("saving category sync", user, &e);
    }
}

/// Adds each message's category to its `categories` in its mailbox (one
/// `$batch` per mailbox), keeping the user's other categories and replacing
/// a different pidge category. Other, excluded mailboxes and categories
/// the user removed from a mailbox are skipped. Returns how many messages
/// were written.
pub async fn tag(state: &SharedState, user: &str, items: Vec<(Candidate, String)>) -> usize {
    let Some(record) = load_owned(state, user).await else {
        return 0;
    };
    let ms = &record.review.mailbox_sync;
    let categories = record.review.categories();
    let mut by_account: BTreeMap<String, Vec<(String, Vec<String>)>> = BTreeMap::new();
    for (c, category) in items {
        if category == OTHER
            || !record.owns(&c.account)
            || !ms.active(&c.account)
            || ms.removed_in(&c.account, &category)
            || check_id(&c.id).is_err()
        {
            continue;
        }
        let target = ms.name_in(&c.account, &category);
        let managed = ms.managed_in(&c.account, &categories);
        let mut next: Vec<String> = c
            .categories
            .iter()
            .filter(|n| n.eq_ignore_ascii_case(&target) || !managed.contains(&n.to_lowercase()))
            .cloned()
            .collect();
        if !next.iter().any(|n| n.eq_ignore_ascii_case(&target)) {
            next.push(target);
        }
        if next != c.categories {
            by_account.entry(c.account).or_default().push((c.id, next));
        }
    }
    let mut written = 0;
    for (account, changes) in by_account {
        written += write_categories(state, &account, changes).await;
    }
    if written > 0 {
        state.cache.invalidate_user(user);
    }
    written
}

/// Renames category `old` to `new` in the mailboxes (pidge's own list is
/// already renamed): a new mailbox category, the mail retagged, the old
/// category deleted when pidge created or linked it.
pub async fn rename(state: &SharedState, user: &str, old: &str, new: &str) {
    let Some(record) = load_owned(state, user).await else {
        return;
    };
    let mut ms = record.review.mailbox_sync.clone();
    for mailbox in &record.mailboxes {
        if !ms.active(mailbox) {
            continue;
        }
        let st = ms.mailboxes.entry(mailbox.clone()).or_default();
        let old_link = st.links.remove(old);
        if let Some(i) = st.removed.iter().position(|r| r == old) {
            st.removed[i] = new.to_string();
            continue;
        }
        let old_name = old_link
            .as_ref()
            .map_or(old, |l| l.name.as_str())
            .to_string();
        let color = old_link
            .as_ref()
            .map(|l| l.color.clone())
            .or_else(|| ms.colors.get(old).cloned());
        let mut new_id = None;
        if !st.no_scope
            && let Ok(masters) = list_masters(state, mailbox).await
        {
            let link = match masters.iter().find(|m| m.name.eq_ignore_ascii_case(new)) {
                Some(m) => Some(Link {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    color: m.color.clone(),
                    created: old_link.as_ref().is_some_and(|l| l.created && l.id == m.id),
                }),
                None => {
                    let color = color.clone().unwrap_or_else(|| pick_colour(&masters));
                    create_master(state, mailbox, new, &color)
                        .await
                        .ok()
                        .map(|m| Link {
                            id: m.id,
                            name: m.name,
                            color,
                            created: true,
                        })
                }
            };
            if let Some(link) = link {
                new_id = Some(link.id.clone());
                st.links.insert(new.to_string(), link);
            }
        }
        let new_name = ms.name_in(mailbox, new);
        if !old_name.eq_ignore_ascii_case(&new_name)
            && let Ok(msgs) = messages_with(state, mailbox, &old_name).await
        {
            let changes = msgs
                .into_iter()
                .map(|(id, cats)| {
                    let mut next: Vec<String> = cats
                        .into_iter()
                        .filter(|c| !c.eq_ignore_ascii_case(&old_name))
                        .collect();
                    if !next.iter().any(|c| c.eq_ignore_ascii_case(&new_name)) {
                        next.push(new_name.clone());
                    }
                    (id, next)
                })
                .collect();
            write_categories(state, mailbox, changes).await;
        }
        if let Some(l) = old_link
            && Some(&l.id) != new_id.as_ref()
            && delete_master(state, mailbox, &l.id).await.is_err()
        {
            tracing::warn!(user = %user_hash(user), "category sync: an old category could not be deleted");
        }
    }
    if let Some(c) = ms.colors.remove(old) {
        ms.colors.insert(new.to_string(), c);
    }
    let mailboxes = record.mailboxes.clone();
    save_sync(state, user, &mailboxes, ms).await;
    state.cache.invalidate_user(user);
}

/// Removes category `name` from the mailboxes (pidge's list no longer has
/// it): from the mail too when `from_mail`, and the mailbox category itself
/// only when pidge created it.
pub async fn remove(state: &SharedState, user: &str, name: &str, from_mail: bool) {
    let Some(record) = load_owned(state, user).await else {
        return;
    };
    let mut ms = record.review.mailbox_sync.clone();
    for mailbox in &record.mailboxes {
        if !ms.active(mailbox) {
            continue;
        }
        let st = ms.mailboxes.entry(mailbox.clone()).or_default();
        st.removed.retain(|r| r != name);
        let link = st.links.remove(name);
        let shown = link.as_ref().map_or(name, |l| l.name.as_str()).to_string();
        if from_mail && let Ok(msgs) = messages_with(state, mailbox, &shown).await {
            let changes = msgs
                .into_iter()
                .map(|(id, cats)| {
                    let next = cats
                        .into_iter()
                        .filter(|c| !c.eq_ignore_ascii_case(&shown))
                        .collect();
                    (id, next)
                })
                .collect();
            write_categories(state, mailbox, changes).await;
        }
        if let Some(l) = link
            && l.created
            && delete_master(state, mailbox, &l.id).await.is_err()
        {
            tracing::warn!(user = %user_hash(user), "category sync: a category could not be deleted");
        }
    }
    ms.colors.remove(name);
    let mailboxes = record.mailboxes.clone();
    save_sync(state, user, &mailboxes, ms).await;
    state.cache.invalidate_user(user);
}

/// One mailbox category a prune removes (or would remove): its name and
/// how many messages carry it (capped at [`RETAG_CAP`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    pub mailbox: String,
    pub name: String,
    pub messages: usize,
}

/// What [`prune`] found or did.
#[derive(Debug, Default)]
pub struct PruneReport {
    pub pruned: Vec<Pruned>,
    /// pidge categories given pidge's colour in a mailbox.
    pub recoloured: usize,
    /// Mailboxes whose categories could not be read (or need
    /// `accounts_connect` once).
    pub skipped: Vec<String>,
    pub failed: usize,
}

/// Leaves the user's mailboxes with exactly pidge's categories, in pidge's
/// colours: every other mailbox category (the user's own and the
/// colour-only defaults included) is listed, and with `apply` deleted,
/// also from the mail when `from_mail`. Only on the user's explicit
/// request; the caller holds the user's [`Locks`] entry. Run a [`sync`]
/// first so pidge's own categories are linked and never pruned.
pub async fn prune(state: &SharedState, user: &str, apply: bool, from_mail: bool) -> PruneReport {
    let mut report = PruneReport::default();
    let Some(record) = load_owned(state, user).await else {
        return report;
    };
    let ms = &record.review.mailbox_sync;
    let pidge: Vec<String> = record
        .review
        .categories()
        .iter()
        .map(|c| c.name.to_ascii_lowercase())
        .collect();
    for mailbox in &record.mailboxes {
        if !ms.active(mailbox) {
            continue;
        }
        let masters = match list_masters(state, mailbox).await {
            Ok(m) => m,
            Err(_) => {
                report.skipped.push(mailbox.clone());
                continue;
            }
        };
        let links = ms.mailboxes.get(mailbox).map(|st| &st.links);
        let linked_ids: HashSet<&str> = links
            .map(|l| l.values().map(|l| l.id.as_str()).collect())
            .unwrap_or_default();
        for m in &masters {
            let ours =
                linked_ids.contains(m.id.as_str()) || pidge.contains(&m.name.to_ascii_lowercase());
            if ours {
                // One look everywhere: pidge's colour for its own categories.
                let wanted = links
                    .and_then(|l| l.iter().find(|(_, l)| l.id == m.id))
                    .and_then(|(name, _)| ms.colors.get(name));
                if apply
                    && let Some(color) = wanted
                    && *color != m.color
                {
                    match recolour_master(state, mailbox, &m.id, color).await {
                        Ok(()) => report.recoloured += 1,
                        Err(_) => report.failed += 1,
                    }
                }
                continue;
            }
            let carrying = messages_with(state, mailbox, &m.name)
                .await
                .unwrap_or_default();
            report.pruned.push(Pruned {
                mailbox: mailbox.clone(),
                name: m.name.clone(),
                messages: carrying.len(),
            });
            if !apply {
                continue;
            }
            if from_mail && !carrying.is_empty() {
                let changes = carrying
                    .into_iter()
                    .map(|(id, cats)| {
                        let next = cats
                            .into_iter()
                            .filter(|c| !c.eq_ignore_ascii_case(&m.name))
                            .collect();
                        (id, next)
                    })
                    .collect();
                write_categories(state, mailbox, changes).await;
            }
            if delete_master(state, mailbox, &m.id).await.is_err() {
                report.failed += 1;
            }
        }
    }
    if apply {
        state.cache.invalidate_user(user);
        tracing::info!(
            user = %user_hash(user),
            pruned = report.pruned.len(),
            failed = report.failed,
            "mailbox categories pruned"
        );
    }
    report
}

/// The queued notes, cleared from the record: each is shown once.
pub async fn take_notes(state: &SharedState, user: &str) -> Vec<String> {
    let Some(mut rec) = load(state, user).await else {
        return Vec::new();
    };
    let notes = std::mem::take(&mut rec.review.mailbox_sync.notes);
    if !notes.is_empty()
        && let Err(e) = state.users.save(&rec).await
    {
        log_store_error("saving category notes", user, &e);
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_only_defaults_are_recognised_in_english_and_swedish() {
        for n in [
            "Red category",
            "Blue Category",
            "Röd kategori",
            "Grön kategori",
            "Lila kategori",
            "Dark Blue",
            "DarkCranberry",
            "Mörkblå",
            "Mörk-grön",
            "Orange",
        ] {
            assert!(is_colour_default(n), "{n}");
        }
        for n in [
            "Uppdrag",
            "Bills and receipts",
            "Red tape",
            "Blue team",
            "category",
        ] {
            assert!(!is_colour_default(n), "{n}");
        }
    }

    #[test]
    fn a_new_colour_avoids_the_defaults_and_other_categories() {
        let m = |name: &str, color: &str| Master {
            id: "x".into(),
            name: name.into(),
            color: color.into(),
        };
        let masters = vec![
            m("Red category", "preset0"),
            m("Orange category", "preset1"),
            m("Uppdrag", "preset2"),
        ];
        assert_eq!(pick_colour(&masters), "preset3");
        // Every colour taken: still not one a default uses.
        let mut all: Vec<Master> = (0..PRESETS)
            .map(|i| m("Mine", &format!("preset{i}")))
            .collect();
        all.push(m("Red category", "preset0"));
        assert_eq!(pick_colour(&all), "preset1");
    }

    #[test]
    fn category_names_are_escaped_in_filters() {
        assert_eq!(
            category_filter("Anna's things"),
            "categories/any(c:c eq 'Anna''s things')"
        );
        assert_eq!(
            encode(&category_filter("a'b")),
            "categories%2Fany%28c:c%20eq%20%27a%27%27b%27%29"
        );
    }
}
