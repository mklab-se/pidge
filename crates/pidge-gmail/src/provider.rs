//! [`GmailProvider`]: pidge's [`Provider`] for Gmail and Google Calendar.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use pidge_client::auth::browser_flow::make_random;
use pidge_core::provider::{
    BusyBlock, Draft, DraftKind, DraftSpec, EventPatch, EventSpec, Folder, FolderInfo, MailChange,
    MailItem, MailSync, Provider, ProviderError, ProviderKind, ProviderResult, Rsvp, SearchQuery,
    UnsubscribeHeaders,
};
use pidge_core::{Attachment, Event, FullMessage};
use reqwest::{Method, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::GoogleAuth;
use crate::calendar::{self, EventList, FreeBusyResponse, GEvent, PRIMARY};
use crate::http::{check, check_id, send_with_retry};
use crate::mime;
use crate::model::{
    GmailMessage, HistoryList, Label, METADATA_HEADERS, MessageList, Part, Profile, mail_item,
};
use crate::query;

/// Gmail API base for the signed-in user.
pub const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
/// Google Calendar API base.
pub const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Messages fetched at once (metadata or raw).
const FETCH_CONCURRENCY: usize = 8;
/// Page size of a sync bootstrap.
const SYNC_PAGE: u32 = 100;
/// `messages.batchModify` takes at most this many ids.
const BATCH_MODIFY_MAX: usize = 1000;

/// The sync cursor, serialized as JSON. Versioned so a format change
/// re-bootstraps instead of misreading an old cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SyncCursor {
    v: u32,
    #[serde(flatten)]
    state: CursorState,
}

const CURSOR_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum CursorState {
    /// Mid-bootstrap: the next `messages.list` page, and the mailbox's
    /// history id from before the listing began (changes made during the
    /// bootstrap are picked up by the first incremental round).
    Bootstrap {
        history_id: String,
        page_token: String,
    },
    /// Incremental: `history.list` from `history_id`, continuing at
    /// `page_token` when a round was cut into pages.
    History {
        history_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_token: Option<String>,
    },
}

impl SyncCursor {
    fn encode(state: CursorState) -> String {
        serde_json::to_string(&SyncCursor {
            v: CURSOR_VERSION,
            state,
        })
        .expect("cursor serializes")
    }

    fn decode(s: &str) -> ProviderResult<CursorState> {
        match serde_json::from_str::<SyncCursor>(s) {
            Ok(c) if c.v == CURSOR_VERSION => Ok(c.state),
            _ => Err(ProviderError::CursorExpired),
        }
    }
}

#[derive(Deserialize)]
struct DraftResource {
    id: String,
    #[serde(default)]
    message: Option<GmailMessage>,
}

#[derive(Deserialize, Default)]
struct LabelList {
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct AttachmentData {
    #[serde(default)]
    data: String,
}

#[derive(Deserialize, Default)]
struct ThreadResource {
    #[serde(default)]
    messages: Vec<GmailMessage>,
}

/// One Gmail mailbox with its primary Google calendar.
pub struct GmailProvider {
    auth: GoogleAuth,
    gmail_base: String,
    calendar_base: String,
    /// Label id to label, loaded on first use and refreshed when a message
    /// carries a label id pidge has not seen.
    labels: tokio::sync::Mutex<Option<HashMap<String, Label>>>,
    /// The primary calendar's time zone (all-day events start at its
    /// midnight).
    calendar_tz: tokio::sync::Mutex<Option<String>>,
}

impl GmailProvider {
    pub fn new(auth: GoogleAuth) -> Self {
        Self::for_test(auth, GMAIL_BASE, CALENDAR_BASE)
    }

    /// A provider with other API bases (a mock server in tests).
    pub fn for_test(
        auth: GoogleAuth,
        gmail_base: impl Into<String>,
        calendar_base: impl Into<String>,
    ) -> Self {
        Self {
            auth,
            gmail_base: gmail_base.into().trim_end_matches('/').to_string(),
            calendar_base: calendar_base.into().trim_end_matches('/').to_string(),
            labels: tokio::sync::Mutex::new(None),
            calendar_tz: tokio::sync::Mutex::new(None),
        }
    }

    fn account(&self) -> &str {
        self.auth.email()
    }

    fn gmail(&self, path: &str) -> String {
        format!("{}/{path}", self.gmail_base)
    }

    fn cal(&self, path: &str) -> String {
        format!("{}/{path}", self.calendar_base)
    }

    /// Authenticate and send, retrying throttling; the status is not checked.
    async fn send(
        &self,
        method: Method,
        url: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> ProviderResult<Response> {
        let token = self.auth.access_token().await?;
        let mut req = self
            .auth
            .http()
            .request(method.clone(), url)
            .bearer_auth(token);
        if !query.is_empty() {
            req = req.query(query);
        }
        req = match body {
            Some(b) => req.json(b),
            // Google's front end wants a length on every POST/PUT/PATCH.
            None if method != Method::GET && method != Method::DELETE => req.body(Vec::new()),
            None => req,
        };
        send_with_retry(req).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        what: &str,
    ) -> ProviderResult<T> {
        let resp = self.send(method, url, query, body).await?;
        let resp = check(resp, self.account(), what).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::Other(format!("unreadable answer from Google: {e}")))
    }

    async fn call_empty(
        &self,
        method: Method,
        url: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        what: &str,
    ) -> ProviderResult<()> {
        let resp = self.send(method, url, query, body).await?;
        check(resp, self.account(), what).await.map(|_| ())
    }

    // ---- labels -----------------------------------------------------------

    async fn load_labels(&self) -> ProviderResult<HashMap<String, Label>> {
        let list: LabelList = self
            .call(Method::GET, &self.gmail("labels"), &[], None, "labels")
            .await?;
        let map: HashMap<String, Label> =
            list.labels.into_iter().map(|l| (l.id.clone(), l)).collect();
        *self.labels.lock().await = Some(map.clone());
        Ok(map)
    }

    async fn label_map(&self) -> ProviderResult<HashMap<String, Label>> {
        if let Some(m) = self.labels.lock().await.as_ref() {
            return Ok(m.clone());
        }
        self.load_labels().await
    }

    /// The user label named `name` (case-insensitive), created if missing.
    async fn find_or_create_label(&self, name: &str) -> ProviderResult<Label> {
        let name = name.trim().trim_matches('/').trim();
        if name.is_empty() {
            return Err(ProviderError::Invalid("a label needs a name".into()));
        }
        let find = |m: &HashMap<String, Label>| {
            m.values()
                .find(|l| l.is_user() && l.name.eq_ignore_ascii_case(name))
                .cloned()
        };
        if let Some(l) = find(&self.label_map().await?) {
            return Ok(l);
        }
        let body = json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show",
        });
        let resp = self
            .send(Method::POST, &self.gmail("labels"), &[], Some(&body))
            .await?;
        if resp.status().as_u16() == 409 {
            // Created elsewhere since the cache was loaded.
            return find(&self.load_labels().await?).ok_or_else(|| {
                ProviderError::Invalid(format!("Gmail refused the label name {name:?}"))
            });
        }
        let resp = check(resp, self.account(), "label").await?;
        let mut label: Label = resp
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("unreadable answer from Google: {e}")))?;
        if label.kind.is_empty() {
            label.kind = "user".into();
        }
        if let Some(m) = self.labels.lock().await.as_mut() {
            m.insert(label.id.clone(), label.clone());
        }
        Ok(label)
    }

    // ---- messages ---------------------------------------------------------

    async fn get_metadata(&self, id: &str, headers: &[&str]) -> ProviderResult<GmailMessage> {
        let id = check_id(id, "message")?;
        let mut q: Vec<(&str, String)> = vec![("format", "metadata".into())];
        q.extend(headers.iter().map(|h| ("metadataHeaders", h.to_string())));
        self.call(
            Method::GET,
            &self.gmail(&format!("messages/{id}")),
            &q,
            None,
            "message",
        )
        .await
    }

    async fn get_raw(&self, id: &str) -> ProviderResult<GmailMessage> {
        let id = check_id(id, "message")?;
        self.call(
            Method::GET,
            &self.gmail(&format!("messages/{id}")),
            &[("format", "raw".into())],
            None,
            "message",
        )
        .await
    }

    async fn get_labels_of(&self, id: &str) -> ProviderResult<GmailMessage> {
        let id = check_id(id, "message")?;
        self.call(
            Method::GET,
            &self.gmail(&format!("messages/{id}")),
            &[("format", "minimal".into())],
            None,
            "message",
        )
        .await
    }

    /// Metadata for `ids`, at most [`FETCH_CONCURRENCY`] at a time, in order.
    async fn fetch_metadata(&self, ids: &[String]) -> Vec<(String, ProviderResult<GmailMessage>)> {
        stream::iter(ids.iter().cloned())
            .map(|id| async move {
                let r = self.get_metadata(&id, METADATA_HEADERS).await;
                (id, r)
            })
            .buffered(FETCH_CONCURRENCY)
            .collect()
            .await
    }

    /// Labels for mapping `msgs`, reloaded once if one carries an unknown
    /// user label.
    async fn labels_for(&self, msgs: &[&GmailMessage]) -> ProviderResult<HashMap<String, Label>> {
        let map = self.label_map().await?;
        let unknown = msgs
            .iter()
            .flat_map(|m| m.label_ids.iter())
            .any(|l| l.starts_with("Label_") && !map.contains_key(l));
        if unknown {
            self.load_labels().await
        } else {
            Ok(map)
        }
    }

    /// [`MailItem`]s for `ids`; messages that no longer exist are skipped.
    async fn items(&self, ids: &[String]) -> ProviderResult<Vec<MailItem>> {
        let mut msgs = Vec::with_capacity(ids.len());
        for (_, r) in self.fetch_metadata(ids).await {
            match r {
                Ok(m) => msgs.push(m),
                Err(ProviderError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let labels = self.labels_for(&msgs.iter().collect::<Vec<_>>()).await?;
        Ok(msgs
            .iter()
            .map(|m| mail_item(self.account(), m, &labels))
            .collect())
    }

    async fn list_messages(
        &self,
        q: &str,
        page_token: Option<&str>,
        max: u32,
        label_id: Option<&str>,
        include_spam_trash: bool,
    ) -> ProviderResult<MessageList> {
        let mut query: Vec<(&str, String)> = vec![("maxResults", max.to_string())];
        if !q.is_empty() {
            query.push(("q", q.to_string()));
        }
        if let Some(t) = page_token {
            query.push(("pageToken", t.to_string()));
        }
        if let Some(l) = label_id {
            query.push(("labelIds", check_id(l, "label")?.to_string()));
        }
        if include_spam_trash {
            query.push(("includeSpamTrash", "true".into()));
        }
        self.call(
            Method::GET,
            &self.gmail("messages"),
            &query,
            None,
            "messages",
        )
        .await
    }

    async fn bootstrap_page(
        &self,
        since: DateTime<Utc>,
        history_id: String,
        page_token: Option<String>,
    ) -> ProviderResult<MailSync> {
        let list = self
            .list_messages(
                &query::sync_q(since),
                page_token.as_deref(),
                SYNC_PAGE,
                None,
                false,
            )
            .await?;
        let ids: Vec<String> = list.messages.into_iter().map(|m| m.id).collect();
        let upserts = self.items(&ids).await?;
        let (cursor, more) = match list.next_page_token {
            Some(t) => (
                SyncCursor::encode(CursorState::Bootstrap {
                    history_id,
                    page_token: t,
                }),
                true,
            ),
            None => (
                SyncCursor::encode(CursorState::History {
                    history_id,
                    page_token: None,
                }),
                false,
            ),
        };
        Ok(MailSync {
            upserts,
            removed: Vec::new(),
            cursor,
            more,
        })
    }

    async fn history_page(
        &self,
        since: DateTime<Utc>,
        history_id: String,
        page_token: Option<String>,
    ) -> ProviderResult<MailSync> {
        check_id(&history_id, "history")?;
        let mut q: Vec<(&str, String)> = vec![
            ("startHistoryId", history_id.clone()),
            ("maxResults", "500".into()),
        ];
        for t in [
            "messageAdded",
            "messageDeleted",
            "labelAdded",
            "labelRemoved",
        ] {
            q.push(("historyTypes", t.into()));
        }
        if let Some(t) = &page_token {
            q.push(("pageToken", t.clone()));
        }
        let resp = self
            .send(Method::GET, &self.gmail("history"), &q, None)
            .await?;
        if resp.status().as_u16() == 404 {
            return Err(ProviderError::CursorExpired);
        }
        let resp = check(resp, self.account(), "history").await?;
        let list: HistoryList = resp
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("unreadable answer from Google: {e}")))?;

        let mut deleted: Vec<String> = Vec::new();
        let mut deleted_set: HashSet<String> = HashSet::new();
        let mut changed: Vec<String> = Vec::new();
        let mut changed_set: HashSet<String> = HashSet::new();
        for h in &list.history {
            for d in &h.messages_deleted {
                if deleted_set.insert(d.message.id.clone()) {
                    deleted.push(d.message.id.clone());
                }
            }
            for m in h
                .messages_added
                .iter()
                .chain(&h.labels_added)
                .chain(&h.labels_removed)
            {
                if changed_set.insert(m.message.id.clone()) {
                    changed.push(m.message.id.clone());
                }
            }
        }
        changed.retain(|id| !deleted_set.contains(id));

        let mut removed = deleted;
        let mut kept: Vec<GmailMessage> = Vec::new();
        for (id, r) in self.fetch_metadata(&changed).await {
            match r {
                Ok(m) => {
                    let outside = ["TRASH", "SPAM", "DRAFT"].iter().any(|l| m.has_label(l))
                        || m.received_at() < since;
                    if outside {
                        removed.push(id);
                    } else {
                        kept.push(m);
                    }
                }
                // Gone between the history record and now; an id that is
                // not a message id is not ours to keep either.
                Err(ProviderError::NotFound(_)) | Err(ProviderError::Invalid(_)) => {
                    removed.push(id)
                }
                Err(e) => return Err(e),
            }
        }
        let labels = self.labels_for(&kept.iter().collect::<Vec<_>>()).await?;
        let upserts = kept
            .iter()
            .map(|m| mail_item(self.account(), m, &labels))
            .collect();

        let (state, more) = match list.next_page_token {
            Some(t) => (
                CursorState::History {
                    history_id,
                    page_token: Some(t),
                },
                true,
            ),
            None => (
                CursorState::History {
                    history_id: list.history_id.unwrap_or(history_id),
                    page_token: None,
                },
                false,
            ),
        };
        Ok(MailSync {
            upserts,
            removed,
            cursor: SyncCursor::encode(state),
            more,
        })
    }

    /// `messages.batchModify` for `ids` (already checked), in chunks.
    async fn batch_modify(
        &self,
        ids: &[String],
        add: &[&str],
        remove: &[&str],
    ) -> Vec<ProviderResult<()>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH_MODIFY_MAX) {
            let body = json!({
                "ids": chunk,
                "addLabelIds": add,
                "removeLabelIds": remove,
            });
            let r = self
                .call_empty(
                    Method::POST,
                    &self.gmail("messages/batchModify"),
                    &[],
                    Some(&body),
                    "message",
                )
                .await;
            out.extend(std::iter::repeat_n(r, chunk.len()));
        }
        out
    }

    async fn trash(&self, id: &str) -> ProviderResult<()> {
        let id = check_id(id, "message")?;
        self.call_empty(
            Method::POST,
            &self.gmail(&format!("messages/{id}/trash")),
            &[],
            None,
            "message",
        )
        .await
    }

    async fn trash_all(&self, ids: &[String]) -> Vec<ProviderResult<()>> {
        stream::iter(ids.to_vec())
            .map(|id| async move { self.trash(&id).await })
            .buffered(FETCH_CONCURRENCY)
            .collect()
            .await
    }

    async fn set_categories_all(
        &self,
        ids: &[String],
        wanted: HashSet<String>,
    ) -> Vec<ProviderResult<()>> {
        let wanted = &wanted;
        stream::iter(ids.to_vec())
            .map(|id| async move { self.set_categories(&id, wanted).await })
            .buffered(FETCH_CONCURRENCY)
            .collect()
            .await
    }

    async fn set_categories(&self, id: &str, wanted: &HashSet<String>) -> ProviderResult<()> {
        let m = self.get_labels_of(id).await?;
        let labels = self.labels_for(&[&m]).await?;
        let current: HashSet<String> = m
            .label_ids
            .iter()
            .filter(|l| labels.get(*l).is_some_and(Label::is_user))
            .cloned()
            .collect();
        let add: Vec<&String> = wanted.difference(&current).collect();
        let remove: Vec<&String> = current.difference(wanted).collect();
        if add.is_empty() && remove.is_empty() {
            return Ok(());
        }
        self.call_empty(
            Method::POST,
            &self.gmail(&format!("messages/{}/modify", check_id(id, "message")?)),
            &[],
            Some(&json!({ "addLabelIds": add, "removeLabelIds": remove })),
            "message",
        )
        .await
    }

    // ---- drafts -----------------------------------------------------------

    async fn build(&self, spec: &DraftSpec) -> ProviderResult<mime::BuiltDraft> {
        let orig = match &spec.kind {
            DraftKind::New => None,
            DraftKind::Reply { to_id }
            | DraftKind::ReplyAll { to_id }
            | DraftKind::Forward { to_id } => Some(mime::original(&self.get_raw(to_id).await?)?),
        };
        mime::build_draft(self.account(), spec, orig.as_ref())
    }

    fn draft_of(&self, res: DraftResource, built: mime::BuiltDraft) -> Draft {
        Draft {
            account: self.account().to_string(),
            id: res.id,
            thread_id: res
                .message
                .map(|m| m.thread_id)
                .filter(|t| !t.is_empty())
                .or(built.thread_id),
            to: built.to,
            cc: built.cc,
            bcc: built.bcc,
            subject: built.subject,
            body_text: built.body_text,
            has_attachments: false,
        }
    }

    fn draft_body(built: &mime::BuiltDraft, id: Option<&str>) -> Value {
        let mut message = json!({ "raw": mime::encode_b64url(built.raw.as_bytes()) });
        if let Some(t) = &built.thread_id {
            message["threadId"] = json!(t);
        }
        let mut body = json!({ "message": message });
        if let Some(id) = id {
            body["id"] = json!(id);
        }
        body
    }

    // ---- calendar ---------------------------------------------------------

    fn event_url(&self, id: &str) -> ProviderResult<String> {
        let id = check_id(id, "event")?;
        Ok(self.cal(&format!("calendars/{PRIMARY}/events/{id}")))
    }

    async fn calendar_tz(&self) -> ProviderResult<String> {
        if let Some(tz) = self.calendar_tz.lock().await.clone() {
            return Ok(tz);
        }
        let list: EventList = self
            .call(
                Method::GET,
                &self.cal(&format!("calendars/{PRIMARY}/events")),
                &[("maxResults", "1".into())],
                None,
                "calendar",
            )
            .await?;
        let tz = list.time_zone.unwrap_or_else(|| "UTC".into());
        *self.calendar_tz.lock().await = Some(tz.clone());
        Ok(tz)
    }

    async fn get_gevent(&self, id: &str) -> ProviderResult<GEvent> {
        let url = self.event_url(id)?;
        self.call(Method::GET, &url, &[], None, "event").await
    }
}

fn all_ok_or(r: &ProviderResult<()>, n: usize) -> Vec<ProviderResult<()>> {
    vec![r.clone(); n]
}

#[async_trait]
impl Provider for GmailProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Google
    }

    fn address(&self) -> &str {
        self.account()
    }

    async fn sync_mail(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> ProviderResult<MailSync> {
        match cursor.map(SyncCursor::decode).transpose()? {
            None => {
                let profile: Profile = self
                    .call(Method::GET, &self.gmail("profile"), &[], None, "profile")
                    .await?;
                self.bootstrap_page(since, profile.history_id, None).await
            }
            Some(CursorState::Bootstrap {
                history_id,
                page_token,
            }) => {
                self.bootstrap_page(since, history_id, Some(page_token))
                    .await
            }
            Some(CursorState::History {
                history_id,
                page_token,
            }) => self.history_page(since, history_id, page_token).await,
        }
    }

    async fn search(&self, query: &SearchQuery) -> ProviderResult<Vec<MailItem>> {
        let s = query::build(query);
        let limit = if query.limit == 0 { 50 } else { query.limit };
        let mut ids: Vec<String> = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let want = (limit - ids.len()).min(500) as u32;
            let list = self
                .list_messages(
                    &s.q,
                    page.as_deref(),
                    want,
                    s.label_id.as_deref(),
                    s.include_spam_trash,
                )
                .await?;
            ids.extend(list.messages.into_iter().map(|m| m.id));
            match list.next_page_token {
                Some(t) if ids.len() < limit => page = Some(t),
                _ => break,
            }
        }
        ids.truncate(limit);
        self.items(&ids).await
    }

    async fn get_items(&self, ids: &[String]) -> ProviderResult<Vec<MailItem>> {
        for id in ids {
            check_id(id, "message")?;
        }
        self.items(ids).await
    }

    async fn get_message(&self, id: &str) -> ProviderResult<FullMessage> {
        mime::full_message(self.account(), &self.get_raw(id).await?)
    }

    async fn get_thread(&self, thread_id: &str) -> ProviderResult<Vec<FullMessage>> {
        let tid = check_id(thread_id, "thread")?;
        let thread: ThreadResource = self
            .call(
                Method::GET,
                &self.gmail(&format!("threads/{tid}")),
                &[("format", "minimal".into())],
                None,
                "thread",
            )
            .await?;
        let mut refs = thread.messages;
        refs.sort_by_key(|m| m.received_at());
        let fetched: Vec<ProviderResult<GmailMessage>> = stream::iter(refs)
            .map(|m| async move { self.get_raw(&m.id).await })
            .buffered(FETCH_CONCURRENCY)
            .collect()
            .await;
        fetched
            .into_iter()
            .map(|r| r.and_then(|m| mime::full_message(self.account(), &m)))
            .collect()
    }

    async fn message_headers(&self, id: &str) -> ProviderResult<Vec<(String, String)>> {
        // Naming no header asks Gmail for all of them.
        let m = self.get_metadata(id, &[]).await?;
        Ok(m.payload
            .map(|p| p.headers.into_iter().map(|h| (h.name, h.value)).collect())
            .unwrap_or_default())
    }

    async fn unsubscribe_headers(&self, id: &str) -> ProviderResult<UnsubscribeHeaders> {
        let m = self
            .get_metadata(id, &["List-Unsubscribe", "List-Unsubscribe-Post"])
            .await?;
        Ok(UnsubscribeHeaders {
            list_unsubscribe: m.header("List-Unsubscribe").map(str::to_string),
            list_unsubscribe_post: m.header("List-Unsubscribe-Post").map(str::to_string),
        })
    }

    async fn apply(&self, ids: &[String], change: &MailChange) -> Vec<ProviderResult<()>> {
        let mut results: Vec<Option<ProviderResult<()>>> = ids
            .iter()
            .map(|id| check_id(id, "message").err().map(Err))
            .collect();
        let valid: Vec<(usize, String)> = ids
            .iter()
            .enumerate()
            .filter(|(i, _)| results[*i].is_none())
            .map(|(i, id)| (i, id.clone()))
            .collect();
        let valid_ids: Vec<String> = valid.iter().map(|(_, id)| id.clone()).collect();

        let n = valid_ids.len();
        let ids = &valid_ids;
        let outcome: Vec<ProviderResult<()>> = match change {
            MailChange::MarkRead => self.batch_modify(ids, &[], &["UNREAD"]).await,
            MailChange::MarkUnread => self.batch_modify(ids, &["UNREAD"], &[]).await,
            // Gmail stars have no due date.
            MailChange::Flag { .. } => self.batch_modify(ids, &["STARRED"], &[]).await,
            MailChange::Unflag => self.batch_modify(ids, &[], &["STARRED"]).await,
            MailChange::Move { to } => match to {
                Folder::Inbox => self.batch_modify(ids, &["INBOX"], &["SPAM", "TRASH"]).await,
                Folder::Archive => {
                    self.batch_modify(ids, &[], &["INBOX", "SPAM", "TRASH"])
                        .await
                }
                Folder::Junk => self.batch_modify(ids, &["SPAM"], &["INBOX"]).await,
                Folder::Other(label) => match check_id(label, "label") {
                    Ok(l) => {
                        self.batch_modify(ids, &[l], &["INBOX", "SPAM", "TRASH"])
                            .await
                    }
                    Err(e) => all_ok_or(&Err(e), n),
                },
                Folder::Deleted => self.trash_all(ids).await,
                Folder::Sent | Folder::Drafts => all_ok_or(
                    &Err(ProviderError::Invalid(
                        "Gmail cannot move mail into Sent or Drafts".into(),
                    )),
                    n,
                ),
            },
            MailChange::Delete => self.trash_all(ids).await,
            MailChange::SetCategories { categories } => {
                let mut wanted: HashSet<String> = HashSet::new();
                let mut failed = None;
                for name in categories {
                    match self.find_or_create_label(name).await {
                        Ok(l) => {
                            wanted.insert(l.id);
                        }
                        Err(e) => {
                            failed = Some(e);
                            break;
                        }
                    }
                }
                match failed {
                    Some(e) => all_ok_or(&Err(e), n),
                    None => self.set_categories_all(ids, wanted).await,
                }
            }
        };

        for ((i, _), r) in valid.iter().zip(outcome) {
            results[*i] = Some(r);
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(ProviderError::Other("no result".into()))))
            .collect()
    }

    async fn list_folders(&self) -> ProviderResult<Vec<FolderInfo>> {
        let labels = self.load_labels().await?;
        let system = [
            ("INBOX", Folder::Inbox, "Inbox"),
            ("SENT", Folder::Sent, "Sent"),
            ("DRAFT", Folder::Drafts, "Drafts"),
            ("TRASH", Folder::Deleted, "Trash"),
            ("SPAM", Folder::Junk, "Spam"),
        ];
        let mut wanted: Vec<(String, Folder, String)> = system
            .iter()
            .filter(|(id, _, _)| labels.contains_key(*id))
            .map(|(id, f, name)| (id.to_string(), f.clone(), name.to_string()))
            .collect();
        let mut user: Vec<&Label> = labels.values().filter(|l| l.is_user()).collect();
        user.sort_by_key(|l| l.name.to_lowercase());
        wanted.extend(
            user.into_iter()
                .map(|l| (l.id.clone(), Folder::Other(l.id.clone()), l.name.clone())),
        );
        let fetched: Vec<ProviderResult<FolderInfo>> = stream::iter(wanted)
            .map(|(id, folder, path)| async move {
                let id = check_id(&id, "label")?.to_string();
                let l: Label = self
                    .call(
                        Method::GET,
                        &self.gmail(&format!("labels/{id}")),
                        &[],
                        None,
                        "label",
                    )
                    .await?;
                Ok(FolderInfo {
                    folder,
                    path,
                    unread: l.messages_unread.unwrap_or(0),
                    total: l.messages_total.unwrap_or(0),
                })
            })
            .buffered(FETCH_CONCURRENCY)
            .collect()
            .await;
        fetched.into_iter().collect()
    }

    async fn ensure_folder(&self, path: &str) -> ProviderResult<Folder> {
        match Folder::from_key(path.trim()) {
            Folder::Other(_) => {}
            known => return Ok(known),
        }
        // Gmail label names nest with `/` natively.
        Ok(Folder::Other(self.find_or_create_label(path).await?.id))
    }

    async fn list_categories(&self) -> ProviderResult<Vec<String>> {
        let labels = self.load_labels().await?;
        let mut names: Vec<String> = labels
            .values()
            .filter(|l| l.is_user())
            .map(|l| l.name.clone())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        Ok(names)
    }

    async fn ensure_category(&self, name: &str) -> ProviderResult<()> {
        self.find_or_create_label(name).await.map(|_| ())
    }

    async fn list_attachments(&self, id: &str) -> ProviderResult<Vec<Attachment>> {
        let id = check_id(id, "message")?;
        let m: GmailMessage = self
            .call(
                Method::GET,
                &self.gmail(&format!("messages/{id}")),
                &[("format", "full".into())],
                None,
                "message",
            )
            .await?;
        let mut out = Vec::new();
        if let Some(p) = &m.payload {
            collect_attachments(p, &mut out);
        }
        Ok(out)
    }

    async fn attachment_bytes(&self, id: &str, attachment_id: &str) -> ProviderResult<Vec<u8>> {
        let id = check_id(id, "message")?;
        let att = check_id(attachment_id, "attachment")?;
        let a: AttachmentData = self
            .call(
                Method::GET,
                &self.gmail(&format!("messages/{id}/attachments/{att}")),
                &[],
                None,
                "attachment",
            )
            .await?;
        mime::decode_b64url(&a.data)
    }

    async fn create_draft(&self, spec: &DraftSpec) -> ProviderResult<Draft> {
        let built = self.build(spec).await?;
        let res: DraftResource = self
            .call(
                Method::POST,
                &self.gmail("drafts"),
                &[],
                Some(&Self::draft_body(&built, None)),
                "draft",
            )
            .await?;
        Ok(self.draft_of(res, built))
    }

    async fn update_draft(&self, id: &str, spec: &DraftSpec) -> ProviderResult<Draft> {
        let id = check_id(id, "draft")?;
        let built = self.build(spec).await?;
        let res: DraftResource = self
            .call(
                Method::PUT,
                &self.gmail(&format!("drafts/{id}")),
                &[],
                Some(&Self::draft_body(&built, Some(id))),
                "draft",
            )
            .await?;
        Ok(self.draft_of(res, built))
    }

    async fn get_draft(&self, id: &str) -> ProviderResult<Draft> {
        let id = check_id(id, "draft")?;
        let res: DraftResource = self
            .call(
                Method::GET,
                &self.gmail(&format!("drafts/{id}")),
                &[("format", "raw".into())],
                None,
                "draft",
            )
            .await?;
        let msg = res.message.unwrap_or_default();
        let p = mime::parse_draft(msg.raw.as_deref().unwrap_or(""))?;
        Ok(Draft {
            account: self.account().to_string(),
            id: res.id,
            thread_id: Some(msg.thread_id).filter(|t| !t.is_empty()),
            to: p.to,
            cc: p.cc,
            bcc: p.bcc,
            subject: p.subject,
            body_text: p.body_text,
            has_attachments: p.has_attachments,
        })
    }

    async fn delete_draft(&self, id: &str) -> ProviderResult<()> {
        let id = check_id(id, "draft")?;
        self.call_empty(
            Method::DELETE,
            &self.gmail(&format!("drafts/{id}")),
            &[],
            None,
            "draft",
        )
        .await
    }

    async fn send_draft(&self, id: &str) -> ProviderResult<()> {
        let id = check_id(id, "draft")?;
        self.call_empty(
            Method::POST,
            &self.gmail("drafts/send"),
            &[],
            Some(&json!({ "id": id })),
            "draft",
        )
        .await
    }

    async fn list_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> ProviderResult<Vec<Event>> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        let mut tz: Option<String> = None;
        loop {
            let mut q: Vec<(&str, String)> = vec![
                ("singleEvents", "true".into()),
                ("orderBy", "startTime".into()),
                (
                    "timeMin",
                    from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
                (
                    "timeMax",
                    to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
                ("maxResults", "250".into()),
            ];
            if let Some(t) = &page {
                q.push(("pageToken", t.clone()));
            }
            let list: EventList = self
                .call(
                    Method::GET,
                    &self.cal(&format!("calendars/{PRIMARY}/events")),
                    &q,
                    None,
                    "calendar",
                )
                .await?;
            if tz.is_none() {
                tz = list.time_zone.clone();
                if let Some(z) = &tz {
                    *self.calendar_tz.lock().await = Some(z.clone());
                }
            }
            let zone = tz.clone().unwrap_or_else(|| "UTC".into());
            out.extend(
                list.items
                    .iter()
                    .filter(|e| e.status.as_deref() != Some("cancelled"))
                    .map(|e| calendar::to_event(self.account(), e, &zone)),
            );
            match list.next_page_token {
                Some(t) => page = Some(t),
                None => break,
            }
        }
        Ok(out)
    }

    async fn get_event(&self, id: &str) -> ProviderResult<Event> {
        let g = self.get_gevent(id).await?;
        let tz = self.calendar_tz().await?;
        Ok(calendar::to_event(self.account(), &g, &tz))
    }

    async fn create_event(&self, spec: &EventSpec) -> ProviderResult<Event> {
        let body = calendar::create_body(spec, &make_random(16));
        let mut q: Vec<(&str, String)> = vec![("sendUpdates", "all".into())];
        if spec.online_meeting {
            q.push(("conferenceDataVersion", "1".into()));
        }
        let g: GEvent = self
            .call(
                Method::POST,
                &self.cal(&format!("calendars/{PRIMARY}/events")),
                &q,
                Some(&body),
                "calendar",
            )
            .await?;
        let tz = self.calendar_tz().await?;
        Ok(calendar::to_event(self.account(), &g, &tz))
    }

    async fn update_event(&self, id: &str, patch: &EventPatch) -> ProviderResult<Event> {
        let url = self.event_url(id)?;
        let current = if patch.attendees.is_some() {
            self.get_gevent(id).await?.attendees
        } else {
            Vec::new()
        };
        let body = calendar::patch_body(patch, &current, &make_random(16));
        let mut q: Vec<(&str, String)> = vec![("sendUpdates", "all".into())];
        if calendar::touches_conference(patch) {
            q.push(("conferenceDataVersion", "1".into()));
        }
        let g: GEvent = self
            .call(Method::PATCH, &url, &q, Some(&body), "event")
            .await?;
        let tz = self.calendar_tz().await?;
        Ok(calendar::to_event(self.account(), &g, &tz))
    }

    /// Google has no cancellation message: attendees get Google's own
    /// cancellation notice and `message` is not sent.
    async fn cancel_event(&self, id: &str, _message: Option<&str>) -> ProviderResult<()> {
        let url = self.event_url(id)?;
        self.call_empty(
            Method::DELETE,
            &url,
            &[("sendUpdates", "all".into())],
            None,
            "event",
        )
        .await
    }

    async fn delete_event(&self, id: &str) -> ProviderResult<()> {
        let url = self.event_url(id)?;
        self.call_empty(
            Method::DELETE,
            &url,
            &[("sendUpdates", "none".into())],
            None,
            "event",
        )
        .await
    }

    async fn respond(
        &self,
        id: &str,
        response: Rsvp,
        message: Option<&str>,
        notify: bool,
    ) -> ProviderResult<()> {
        let url = self.event_url(id)?;
        let g = self.get_gevent(id).await?;
        let attendees = calendar::rsvp_attendees(&g.attendees, response, message)
            .ok_or_else(|| ProviderError::Invalid("you are not invited to this event".into()))?;
        let send = if notify { "all" } else { "none" };
        self.call_empty(
            Method::PATCH,
            &url,
            &[("sendUpdates", send.into())],
            Some(&json!({ "attendees": attendees })),
            "event",
        )
        .await
    }

    async fn free_busy(
        &self,
        people: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> ProviderResult<Vec<(String, Vec<BusyBlock>)>> {
        if people.is_empty() {
            return Ok(Vec::new());
        }
        let body = json!({
            "timeMin": from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "timeMax": to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "items": people.iter().map(|p| json!({ "id": p })).collect::<Vec<_>>(),
        });
        let r: FreeBusyResponse = self
            .call(
                Method::POST,
                &self.cal("freeBusy"),
                &[],
                Some(&body),
                "free/busy",
            )
            .await?;
        let mut failed = 0;
        let out: Vec<(String, Vec<BusyBlock>)> = people
            .iter()
            .map(|p| {
                let cal = r
                    .calendars
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(p))
                    .map(|(_, v)| v);
                let blocks = match cal {
                    Some(c) if c.errors.is_empty() => c
                        .busy
                        .iter()
                        .map(|b| BusyBlock {
                            start: b.start,
                            end: b.end,
                        })
                        .collect(),
                    _ => {
                        failed += 1;
                        Vec::new()
                    }
                };
                (p.clone(), blocks)
            })
            .collect();
        if failed == people.len() {
            return Err(ProviderError::Unsupported(
                "Google shares no free/busy for these people".into(),
            ));
        }
        Ok(out)
    }
}

/// Parts that carry an `attachmentId`, depth first. Note Gmail mints a new
/// attachment id on every fetch of a message; any of them stays usable.
fn collect_attachments(p: &Part, out: &mut Vec<Attachment>) {
    if let Some(att_id) = p.body.as_ref().and_then(|b| b.attachment_id.clone()) {
        let disposition = p.header("Content-Disposition").unwrap_or("");
        let content_id = p
            .header("Content-ID")
            .map(|c| {
                c.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            })
            .filter(|c| !c.is_empty());
        let name = if p.filename.is_empty() {
            "attachment".to_string()
        } else {
            p.filename.clone()
        };
        out.push(Attachment {
            id: att_id,
            name,
            content_type: p.mime_type.clone(),
            size_bytes: p.body.as_ref().map(|b| b.size).unwrap_or(0),
            is_inline: disposition
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("inline"),
            content_id,
        });
    }
    for c in &p.parts {
        collect_attachments(c, out);
    }
}
