//! [`pidge_core::provider::Provider`] for Microsoft Graph.
//!
//! One [`GraphProvider`] per mailbox. It shares the [`GraphClient`] (and so
//! the token backend and HTTP pool) with every other mailbox of the same
//! process. Ids are Graph *immutable* ids (every request asks for them, see
//! `with_immutable_ids`), so a message keeps its id across moves.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use pidge_core::provider::{
    BulkHint, BusyBlock, Draft, DraftKind, DraftSpec, EventPatch, EventSpec, EventWhen, Folder,
    FolderInfo, MailChange, MailItem, MailSync, PersonHint, Provider, ProviderError, ProviderKind,
    ProviderResult, Rsvp, SearchQuery, UnsubscribeHeaders,
};
use pidge_core::{Attachment, Event, FullMessage, MessageFrom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use super::GraphClient;
use super::batch::{BatchRequest, BatchResponse};
use super::events::{NewEvent, Reminder, RsvpKind};
use super::mail::Outgoing;
use crate::error::ClientError;

/// Properties the index needs, nothing more (no bodies).
const ITEM_SELECT: &str = "id,conversationId,parentFolderId,from,toRecipients,ccRecipients,\
subject,receivedDateTime,bodyPreview,isRead,flag,hasAttachments,categories,\
inferenceClassification,isDraft";

/// The folders a mailbox sync follows, by Graph well-known name.
const SYNCED: [&str; 3] = ["inbox", "archive", "sentitems"];

/// Well-known folders pidge names, in [`Folder`] terms.
const WELL_KNOWN: [(&str, Folder); 6] = [
    ("inbox", Folder::Inbox),
    ("archive", Folder::Archive),
    ("sentitems", Folder::Sent),
    ("drafts", Folder::Drafts),
    ("deleteditems", Folder::Deleted),
    ("junkemail", Folder::Junk),
];

/// Graph's id alphabet (base64 and base64url, plus `-`), so an id can never
/// reshape a request path.
pub fn check_graph_id(id: &str) -> ProviderResult<()> {
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '+');
    if id.is_empty() || id.len() > 512 || !id.chars().all(ok) {
        return Err(ProviderError::Invalid(format!("{id:?} is not a valid id")));
    }
    Ok(())
}

/// Map a client error onto what the engine acts on.
pub fn provider_error(account: &str, e: ClientError) -> ProviderError {
    match e {
        ClientError::SessionExpired { .. } | ClientError::MissingAccessToken => {
            ProviderError::Auth {
                account: account.to_string(),
                message: e.to_string(),
            }
        }
        ClientError::Throttled { retry_after } => ProviderError::Throttled { retry_after },
        ClientError::DeltaExpired => ProviderError::CursorExpired,
        ClientError::Graph { status, message } => match status {
            401 => ProviderError::Auth {
                account: account.to_string(),
                message: graph_message(&message),
            },
            404 => ProviderError::NotFound(graph_message(&message)),
            400 | 403 | 409 | 422 => ProviderError::Invalid(graph_message(&message)),
            _ => ProviderError::Other(format!(
                "Microsoft Graph {status}: {}",
                graph_message(&message)
            )),
        },
        other => ProviderError::Other(other.to_string()),
    }
}

/// The `error.message` of a Graph error body, or the body itself, capped.
fn graph_message(body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string());
    msg.chars().take(300).collect()
}

/// A mailbox at Microsoft.
pub struct GraphProvider {
    client: Arc<GraphClient>,
    address: String,
    /// Well-known folder name → folder id, learned once.
    folder_ids: OnceCell<HashMap<String, String>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncCursor {
    v: u8,
    /// Well-known folder → deltaLink.
    links: HashMap<String, String>,
}

impl GraphProvider {
    pub fn new(client: Arc<GraphClient>, address: impl Into<String>) -> Self {
        Self {
            client,
            address: address.into(),
            folder_ids: OnceCell::new(),
        }
    }

    fn err(&self, e: ClientError) -> ProviderError {
        provider_error(&self.address, e)
    }

    async fn token(&self) -> ProviderResult<String> {
        self.client
            .auth
            .get_valid_token(&self.address)
            .await
            .map_err(|e| self.err(e))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.client.base_url)
    }

    async fn get_json(&self, url: &str, query: &[(&str, &str)]) -> ProviderResult<Value> {
        let token = self.token().await?;
        let req = self.client.http.get(url).bearer_auth(&token).query(query);
        self.json_response(super::send_with_retry(req).await).await
    }

    async fn json_response(
        &self,
        resp: Result<reqwest::Response, ClientError>,
    ) -> ProviderResult<Value> {
        let resp = resp.map_err(|e| self.err(e))?;
        let status = resp.status();
        if status.as_u16() == 410 {
            return Err(ProviderError::CursorExpired);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(self.err(ClientError::Graph {
                status: status.as_u16(),
                message: text,
            }));
        }
        if status.as_u16() == 204 {
            return Ok(Value::Null);
        }
        let text = resp.text().await.map_err(|e| self.err(e.into()))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| self.err(e.into()))
    }

    async fn send_json(
        &self,
        method: reqwest::Method,
        url: &str,
        body: &Value,
    ) -> ProviderResult<Value> {
        let token = self.token().await?;
        let req = self
            .client
            .http
            .request(method, url)
            .bearer_auth(&token)
            .json(body);
        self.json_response(super::send_with_retry(req).await).await
    }

    async fn batch(&self, requests: Vec<BatchRequest>) -> ProviderResult<Vec<BatchResponse>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let token = self.token().await?;
        super::batch::batch_all(&self.client.http, &self.client.base_url, &token, requests)
            .await
            .map_err(|e| self.err(e))
    }

    /// Well-known folder ids, fetched once per provider.
    async fn folder_ids(&self) -> ProviderResult<&HashMap<String, String>> {
        self.folder_ids
            .get_or_try_init(|| async {
                let requests = WELL_KNOWN
                    .iter()
                    .map(|(name, _)| {
                        BatchRequest::bare(
                            *name,
                            "GET",
                            format!("/me/mailFolders/{name}?$select=id"),
                        )
                    })
                    .collect();
                let mut ids = HashMap::new();
                for r in self.batch(requests).await? {
                    if let (true, Some(id)) =
                        (r.is_success(), r.body.get("id").and_then(Value::as_str))
                    {
                        ids.insert(r.id.clone(), id.to_string());
                    }
                }
                Ok(ids)
            })
            .await
    }

    async fn folder_of(&self, parent_id: Option<&str>) -> Folder {
        let Some(parent) = parent_id else {
            return Folder::Archive;
        };
        if let Ok(ids) = self.folder_ids().await {
            for (name, folder) in WELL_KNOWN.iter() {
                if ids.get(*name).map(String::as_str) == Some(parent) {
                    return folder.clone();
                }
            }
        }
        Folder::Other(parent.to_string())
    }

    /// The Graph destination for a move: a well-known name or a folder id.
    fn destination(folder: &Folder) -> ProviderResult<String> {
        Ok(match folder {
            Folder::Inbox => "inbox".into(),
            Folder::Archive => "archive".into(),
            Folder::Sent => "sentitems".into(),
            Folder::Drafts => "drafts".into(),
            Folder::Deleted => "deleteditems".into(),
            Folder::Junk => "junkemail".into(),
            Folder::Other(id) => {
                check_graph_id(id)?;
                id.clone()
            }
        })
    }

    async fn item(&self, v: &Value) -> Option<MailItem> {
        let parent = v.get("parentFolderId").and_then(Value::as_str);
        let folder = self.folder_of(parent).await;
        item_from_value(v, &self.address, folder)
    }

    async fn items(&self, values: &[Value]) -> Vec<MailItem> {
        let mut out = Vec::with_capacity(values.len());
        for v in values {
            if let Some(i) = self.item(v).await {
                out.push(i);
            }
        }
        out
    }

    /// Walk `@odata.nextLink`s to the `@odata.deltaLink`.
    async fn drain_delta(&self, first: &str) -> ProviderResult<(Vec<Value>, String)> {
        let token = self.token().await?;
        let mut items = Vec::new();
        let mut next = first.to_string();
        loop {
            super::check_continuation(&next, &self.client.base_url).map_err(|e| self.err(e))?;
            let req = self
                .client
                .http
                .get(&next)
                .bearer_auth(&token)
                .header("Prefer", "odata.maxpagesize=200");
            let body = self
                .json_response(super::send_with_retry(req).await)
                .await?;
            if let Some(page) = body.get("value").and_then(Value::as_array) {
                items.extend(page.iter().cloned());
            }
            if let Some(link) = body.get("@odata.deltaLink").and_then(Value::as_str) {
                return Ok((items, link.to_string()));
            }
            match body.get("@odata.nextLink").and_then(Value::as_str) {
                Some(link) => next = link.to_string(),
                None => return Err(ProviderError::Other("delta without a deltaLink".into())),
            }
        }
    }

    async fn draft_from_graph(&self, id: &str) -> ProviderResult<Draft> {
        check_graph_id(id)?;
        let token = self.token().await?;
        let req = self
            .client
            .http
            .get(self.url(&format!("/me/messages/{id}")))
            .bearer_auth(&token)
            .header("Prefer", "outlook.body-content-type=\"text\"")
            .query(&[(
                "$select",
                "id,conversationId,toRecipients,ccRecipients,bccRecipients,subject,body,hasAttachments",
            )]);
        let v = self
            .json_response(super::send_with_retry(req).await)
            .await?;
        Ok(Draft {
            account: self.address.clone(),
            id: str_of(&v, "id").unwrap_or_else(|| id.to_string()),
            thread_id: str_of(&v, "conversationId"),
            to: recipients(&v, "toRecipients"),
            cc: recipients(&v, "ccRecipients"),
            bcc: recipients(&v, "bccRecipients"),
            subject: str_of(&v, "subject").unwrap_or_default(),
            body_text: v
                .pointer("/body/content")
                .and_then(Value::as_str)
                .map(pidge_core::render::strip_controls)
                .unwrap_or_default(),
            has_attachments: v
                .get("hasAttachments")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn add_recipients(&self, id: &str, spec: &DraftSpec) -> ProviderResult<()> {
        if spec.to.is_empty() && spec.cc.is_empty() && spec.bcc.is_empty() {
            return Ok(());
        }
        let current = self.draft_from_graph(id).await?;
        let merge = |have: &[MessageFrom], add: &[MessageFrom]| -> Value {
            let mut seen = HashSet::new();
            let list: Vec<Value> = have
                .iter()
                .chain(add)
                .filter(|r| seen.insert(r.address.to_ascii_lowercase()))
                .map(recipient_json)
                .collect();
            Value::Array(list)
        };
        let body = json!({
            "toRecipients": merge(&current.to, &spec.to),
            "ccRecipients": merge(&current.cc, &spec.cc),
            "bccRecipients": merge(&current.bcc, &spec.bcc),
        });
        self.send_json(
            reqwest::Method::PATCH,
            &self.url(&format!("/me/messages/{id}")),
            &body,
        )
        .await
        .map(|_| ())
    }

    async fn calendar_ids(&self) -> ProviderResult<Vec<String>> {
        let cals = self
            .client
            .list_calendars(&self.address)
            .await
            .map_err(|e| self.err(e))?;
        Ok(cals.into_iter().map(|c| c.id).collect())
    }
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn address_of(v: &Value) -> MessageFrom {
    MessageFrom {
        name: v
            .pointer("/emailAddress/name")
            .and_then(Value::as_str)
            .map(pidge_core::render::strip_controls)
            .unwrap_or_default(),
        address: v
            .pointer("/emailAddress/address")
            .and_then(Value::as_str)
            .map(pidge_core::render::strip_controls)
            .unwrap_or_default(),
    }
}

fn recipients(v: &Value, key: &str) -> Vec<MessageFrom> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(address_of).collect())
        .unwrap_or_default()
}

fn recipient_json(r: &MessageFrom) -> Value {
    if r.name.is_empty() {
        json!({ "emailAddress": { "address": r.address } })
    } else {
        json!({ "emailAddress": { "address": r.address, "name": r.name } })
    }
}

/// One Graph message (with [`ITEM_SELECT`]) as a [`MailItem`].
pub(crate) fn item_from_value(v: &Value, account: &str, folder: Folder) -> Option<MailItem> {
    let id = str_of(v, "id")?;
    let received_at = v
        .get("receivedDateTime")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())?
        .to_utc();
    let folder = if v.get("isDraft").and_then(Value::as_bool) == Some(true) {
        Folder::Drafts
    } else {
        folder
    };
    let bulk_hint = match v.get("inferenceClassification").and_then(Value::as_str) {
        Some("focused") => Some(BulkHint::Focused),
        Some("other") => Some(BulkHint::Other),
        _ => None,
    };
    Some(MailItem {
        account: account.to_string(),
        thread_id: str_of(v, "conversationId").unwrap_or_else(|| id.clone()),
        id,
        folder,
        from: v.get("from").map(address_of).unwrap_or(MessageFrom {
            name: String::new(),
            address: String::new(),
        }),
        to: recipients(v, "toRecipients"),
        cc: recipients(v, "ccRecipients"),
        subject: v
            .get("subject")
            .and_then(Value::as_str)
            .map(pidge_core::render::strip_controls)
            .unwrap_or_default(),
        received_at,
        preview: v
            .get("bodyPreview")
            .and_then(Value::as_str)
            .map(pidge_core::render::strip_controls)
            .unwrap_or_default(),
        unread: v.get("isRead").and_then(Value::as_bool) == Some(false),
        flagged: v.pointer("/flag/flagStatus").and_then(Value::as_str) == Some("flagged"),
        has_attachments: v
            .get("hasAttachments")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        categories: v
            .get("categories")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        bulk_hint,
        list_unsubscribe: false,
        is_invite: v.get("@odata.type").and_then(Value::as_str)
            == Some("#microsoft.graph.eventMessageRequest"),
    })
}

/// KQL for Graph `$search` from a provider-neutral query.
pub(crate) fn kql(q: &SearchQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    let quote = |s: &str| s.replace(['"', '\\'], " ");
    if let Some(t) = q.text.as_deref().filter(|t| !t.trim().is_empty()) {
        parts.push(quote(t.trim()));
    }
    if let Some(f) = q.from.as_deref().filter(|t| !t.trim().is_empty()) {
        parts.push(format!("from:\"{}\"", quote(f.trim())));
    }
    if let Some(s) = q.subject.as_deref().filter(|t| !t.trim().is_empty()) {
        parts.push(format!("subject:\"{}\"", quote(s.trim())));
    }
    if let Some(d) = q.after {
        parts.push(format!("received>={}", d.format("%Y-%m-%d")));
    }
    if let Some(d) = q.before {
        parts.push(format!("received<={}", d.format("%Y-%m-%d")));
    }
    if q.has_attachments == Some(true) {
        parts.push("hasattachments:true".into());
    }
    parts.join(" AND ")
}

fn when_json(when: &EventWhen) -> (Value, Value, bool) {
    let fmt = |d: DateTime<Utc>| d.format("%Y-%m-%dT%H:%M:%S").to_string();
    match when {
        EventWhen::Timed { start, end } => (
            json!({ "dateTime": fmt(start.at), "timeZone": "UTC" }),
            json!({ "dateTime": fmt(end.at), "timeZone": "UTC" }),
            false,
        ),
        EventWhen::AllDay { first, last } => {
            let end = *last + Duration::days(1);
            (
                json!({ "dateTime": format!("{}T00:00:00", first.format("%Y-%m-%d")), "timeZone": "UTC" }),
                json!({ "dateTime": format!("{}T00:00:00", end.format("%Y-%m-%d")), "timeZone": "UTC" }),
                true,
            )
        }
    }
}

#[async_trait]
impl Provider for GraphProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Microsoft
    }

    fn address(&self) -> &str {
        &self.address
    }

    async fn sync_mail(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> ProviderResult<MailSync> {
        let mut state: SyncCursor = match cursor {
            Some(c) => serde_json::from_str(c).map_err(|_| ProviderError::CursorExpired)?,
            None => SyncCursor {
                v: 1,
                ..Default::default()
            },
        };
        let bootstrap = cursor.is_none();
        let mut out = MailSync::default();
        let mut changed: Vec<String> = Vec::new();
        for name in SYNCED {
            let first = match state.links.get(name) {
                Some(link) => link.clone(),
                None if bootstrap => format!(
                    "{}/me/mailFolders/{name}/messages/delta?$select={ITEM_SELECT}&$filter=receivedDateTime+ge+{}",
                    self.client.base_url,
                    since.format("%Y-%m-%dT%H:%M:%SZ")
                ),
                // A folder that did not exist at bootstrap (no Archive yet).
                None => continue,
            };
            let (values, link) = match self.drain_delta(&first).await {
                Ok(r) => r,
                Err(ProviderError::NotFound(_)) if bootstrap => continue,
                Err(e) => return Err(e),
            };
            state.links.insert(name.to_string(), link);
            for v in values {
                let Some(id) = str_of(&v, "id") else { continue };
                if v.get("@removed").is_some() {
                    out.removed.push(id);
                } else if bootstrap {
                    if let Some(item) = self.item(&v).await {
                        out.upserts.push(item);
                    }
                } else {
                    // Change items can carry only what changed: refetch.
                    changed.push(id);
                }
            }
        }
        if !changed.is_empty() {
            changed.sort();
            changed.dedup();
            out.upserts.extend(self.get_items(&changed).await?);
        }
        out.cursor = serde_json::to_string(&state).unwrap_or_default();
        Ok(out)
    }

    async fn search(&self, query: &SearchQuery) -> ProviderResult<Vec<MailItem>> {
        let limit = query.limit.clamp(1, 100).to_string();
        let kql = kql(query);
        let path = match &query.folder {
            Some(f) => format!("/me/mailFolders/{}/messages", Self::destination(f)?),
            None => "/me/messages".to_string(),
        };
        let url = self.url(&path);
        let body = if kql.is_empty() {
            let mut filter = Vec::new();
            if query.unread == Some(true) {
                filter.push("isRead eq false".to_string());
            }
            let filter = filter.join(" and ");
            let mut q = vec![
                ("$select", ITEM_SELECT),
                ("$top", limit.as_str()),
                ("$orderby", "receivedDateTime desc"),
            ];
            if !filter.is_empty() {
                q.push(("$filter", filter.as_str()));
            }
            self.get_json(&url, &q).await?
        } else {
            let search = format!("\"{kql}\"");
            self.get_json(
                &url,
                &[
                    ("$select", ITEM_SELECT),
                    ("$top", limit.as_str()),
                    ("$search", search.as_str()),
                ],
            )
            .await?
        };
        let values = body
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut items = self.items(&values).await;
        if query.unread == Some(true) {
            items.retain(|i| i.unread);
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.received_at));
        Ok(items)
    }

    async fn get_items(&self, ids: &[String]) -> ProviderResult<Vec<MailItem>> {
        let mut requests = Vec::new();
        for (n, id) in ids.iter().enumerate() {
            check_graph_id(id)?;
            requests.push(BatchRequest::bare(
                n.to_string(),
                "GET",
                format!("/me/messages/{id}?$select={ITEM_SELECT}"),
            ));
        }
        let responses = self.batch(requests).await?;
        let values: Vec<Value> = responses
            .into_iter()
            .filter(BatchResponse::is_success)
            .map(|r| r.body)
            .collect();
        Ok(self.items(&values).await)
    }

    async fn get_message(&self, id: &str) -> ProviderResult<FullMessage> {
        check_graph_id(id)?;
        self.client
            .get_message(&self.address, id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn get_thread(&self, thread_id: &str) -> ProviderResult<Vec<FullMessage>> {
        let list = self
            .client
            .list_conversation(&self.address, thread_id)
            .await
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for m in list
            .iter()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if let Ok(full) = self.get_message(&m.id).await {
                out.push(full);
            }
        }
        Ok(out)
    }

    async fn unsubscribe_headers(&self, id: &str) -> ProviderResult<UnsubscribeHeaders> {
        check_graph_id(id)?;
        let headers = self
            .client
            .fetch_message_headers(&self.address, id)
            .await
            .map_err(|e| self.err(e))?;
        let find = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        Ok(UnsubscribeHeaders {
            list_unsubscribe: find("List-Unsubscribe"),
            list_unsubscribe_post: find("List-Unsubscribe-Post"),
        })
    }

    async fn apply(&self, ids: &[String], change: &MailChange) -> Vec<ProviderResult<()>> {
        let mut results: Vec<ProviderResult<()>> = vec![Ok(()); ids.len()];
        let mut requests = Vec::new();
        for (n, id) in ids.iter().enumerate() {
            if let Err(e) = check_graph_id(id) {
                results[n] = Err(e);
                continue;
            }
            let path = format!("/me/messages/{id}");
            let req = match change {
                MailChange::MarkRead => {
                    BatchRequest::json(n.to_string(), "PATCH", path, json!({ "isRead": true }))
                }
                MailChange::MarkUnread => {
                    BatchRequest::json(n.to_string(), "PATCH", path, json!({ "isRead": false }))
                }
                MailChange::Flag { due } => {
                    let mut flag = json!({ "flagStatus": "flagged" });
                    if let Some(due) = due {
                        let today = Utc::now().date_naive();
                        let fmt = |d: NaiveDate| json!({ "dateTime": format!("{}T00:00:00", d.format("%Y-%m-%d")), "timeZone": "UTC" });
                        flag["startDateTime"] = fmt(today.min(*due));
                        flag["dueDateTime"] = fmt(*due);
                    }
                    BatchRequest::json(n.to_string(), "PATCH", path, json!({ "flag": flag }))
                }
                MailChange::Unflag => BatchRequest::json(
                    n.to_string(),
                    "PATCH",
                    path,
                    json!({ "flag": { "flagStatus": "notFlagged" } }),
                ),
                MailChange::SetCategories { categories } => BatchRequest::json(
                    n.to_string(),
                    "PATCH",
                    path,
                    json!({ "categories": categories }),
                ),
                MailChange::Move { to } => match Self::destination(to) {
                    Ok(dest) => BatchRequest::json(
                        n.to_string(),
                        "POST",
                        format!("{path}/move"),
                        json!({ "destinationId": dest }),
                    ),
                    Err(e) => {
                        results[n] = Err(e);
                        continue;
                    }
                },
                MailChange::Delete => BatchRequest::json(
                    n.to_string(),
                    "POST",
                    format!("{path}/move"),
                    json!({ "destinationId": "deleteditems" }),
                ),
            };
            requests.push(req);
        }
        match self.batch(requests).await {
            Ok(responses) => {
                for r in responses {
                    let Ok(n) = r.id.parse::<usize>() else {
                        continue;
                    };
                    if n < results.len() && !r.is_success() {
                        results[n] = Err(self.err(ClientError::Graph {
                            status: r.status,
                            message: r.body.to_string(),
                        }));
                    }
                }
            }
            Err(e) => {
                for (n, r) in results.iter_mut().enumerate() {
                    if r.is_ok() && n < ids.len() {
                        *r = Err(e.clone());
                    }
                }
            }
        }
        results
    }

    async fn list_folders(&self) -> ProviderResult<Vec<FolderInfo>> {
        let ids = self.folder_ids().await?.clone();
        let well_known: HashMap<String, Folder> = WELL_KNOWN
            .iter()
            .filter_map(|(name, f)| ids.get(*name).map(|id| (id.clone(), f.clone())))
            .collect();
        let top = self
            .client
            .list_mail_folders(&self.address)
            .await
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        let mut stack: Vec<(String, super::MailFolder)> =
            top.into_iter().map(|f| (String::new(), f)).collect();
        while let Some((prefix, f)) = stack.pop() {
            let path = if prefix.is_empty() {
                f.display_name.clone()
            } else {
                format!("{prefix}/{}", f.display_name)
            };
            if f.child_folder_count.unwrap_or(0) > 0
                && let Ok(children) = self.client.list_child_folders(&self.address, &f.id).await
            {
                stack.extend(children.into_iter().map(|c| (path.clone(), c)));
            }
            out.push(FolderInfo {
                folder: well_known
                    .get(&f.id)
                    .cloned()
                    .unwrap_or_else(|| Folder::Other(f.id.clone())),
                path,
                unread: f.unread_item_count.unwrap_or(0) as u32,
                total: f.total_item_count.unwrap_or(0) as u32,
            });
        }
        out.sort_by_key(|f| f.path.to_lowercase());
        Ok(out)
    }

    async fn ensure_folder(&self, path: &str) -> ProviderResult<Folder> {
        let segments: Vec<&str> = path
            .split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if segments.is_empty() {
            return Err(ProviderError::Invalid("empty folder name".into()));
        }
        let mut parent: Option<String> = None;
        for seg in segments {
            let siblings = match &parent {
                None => self.client.list_mail_folders(&self.address).await,
                Some(id) => self.client.list_child_folders(&self.address, id).await,
            }
            .map_err(|e| self.err(e))?;
            let found = siblings
                .into_iter()
                .find(|f| f.display_name.eq_ignore_ascii_case(seg));
            let folder = match found {
                Some(f) => f,
                None => match &parent {
                    None => self.client.create_mail_folder(&self.address, seg).await,
                    Some(id) => {
                        self.client
                            .create_child_folder(&self.address, id, seg)
                            .await
                    }
                }
                .map_err(|e| self.err(e))?,
            };
            parent = Some(folder.id);
        }
        let id = parent.unwrap_or_default();
        let ids = self.folder_ids().await?;
        for (name, f) in WELL_KNOWN.iter() {
            if ids.get(*name) == Some(&id) {
                return Ok(f.clone());
            }
        }
        Ok(Folder::Other(id))
    }

    async fn list_categories(&self) -> ProviderResult<Vec<String>> {
        let v = self
            .get_json(&self.url("/me/outlook/masterCategories"), &[])
            .await?;
        Ok(v.get("value")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|c| str_of(c, "displayName")).collect())
            .unwrap_or_default())
    }

    async fn ensure_category(&self, name: &str) -> ProviderResult<()> {
        let have = self.list_categories().await?;
        if have.iter().any(|c| c.eq_ignore_ascii_case(name)) {
            return Ok(());
        }
        // Presets 0..=24 are Outlook's colours; spread names across them.
        let preset = name
            .bytes()
            .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b.into()))
            % 25;
        self.send_json(
            reqwest::Method::POST,
            &self.url("/me/outlook/masterCategories"),
            &json!({ "displayName": name, "color": format!("preset{preset}") }),
        )
        .await
        .map(|_| ())
    }

    async fn list_attachments(&self, id: &str) -> ProviderResult<Vec<Attachment>> {
        check_graph_id(id)?;
        self.client
            .list_attachments(&self.address, id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn attachment_bytes(&self, id: &str, attachment_id: &str) -> ProviderResult<Vec<u8>> {
        check_graph_id(id)?;
        check_graph_id(attachment_id)?;
        self.client
            .get_attachment_bytes(&self.address, id, attachment_id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn create_draft(&self, spec: &DraftSpec) -> ProviderResult<Draft> {
        let addrs = |l: &[MessageFrom]| l.iter().map(|r| r.address.clone()).collect::<Vec<_>>();
        let id = match &spec.kind {
            DraftKind::New => self
                .client
                .create_draft(
                    &self.address,
                    &Outgoing {
                        subject: spec.subject.clone().unwrap_or_default(),
                        body_text: spec.body.clone(),
                        to: addrs(&spec.to),
                        cc: addrs(&spec.cc),
                        bcc: addrs(&spec.bcc),
                    },
                )
                .await
                .map_err(|e| self.err(e))?,
            DraftKind::Reply { to_id } | DraftKind::ReplyAll { to_id } => {
                check_graph_id(to_id)?;
                let id = if matches!(spec.kind, DraftKind::Reply { .. }) {
                    self.client
                        .create_reply_draft(&self.address, to_id, &spec.body)
                        .await
                } else {
                    self.client
                        .create_reply_all_draft(&self.address, to_id, &spec.body)
                        .await
                }
                .map_err(|e| self.err(e))?;
                self.add_recipients(&id, spec).await?;
                id
            }
            DraftKind::Forward { to_id } => {
                check_graph_id(to_id)?;
                let id = self
                    .client
                    .create_forward_draft(&self.address, to_id, &addrs(&spec.to), &spec.body)
                    .await
                    .map_err(|e| self.err(e))?;
                let rest = DraftSpec {
                    to: Vec::new(),
                    ..spec.clone()
                };
                self.add_recipients(&id, &rest).await?;
                id
            }
        };
        self.draft_from_graph(&id).await
    }

    async fn update_draft(&self, id: &str, spec: &DraftSpec) -> ProviderResult<Draft> {
        check_graph_id(id)?;
        match spec.kind {
            DraftKind::New => {
                let addrs =
                    |l: &[MessageFrom]| l.iter().map(|r| r.address.clone()).collect::<Vec<_>>();
                self.client
                    .update_draft(
                        &self.address,
                        id,
                        &Outgoing {
                            subject: spec.subject.clone().unwrap_or_default(),
                            body_text: spec.body.clone(),
                            to: addrs(&spec.to),
                            cc: addrs(&spec.cc),
                            bcc: addrs(&spec.bcc),
                        },
                    )
                    .await
                    .map_err(|e| self.err(e))?;
                self.draft_from_graph(id).await
            }
            // A reply's body sits above the quoted original; rebuilding it
            // is the reliable way to replace the user's part.
            _ => {
                let fresh = self.create_draft(spec).await?;
                let _ = self.delete_draft(id).await;
                Ok(fresh)
            }
        }
    }

    async fn get_draft(&self, id: &str) -> ProviderResult<Draft> {
        self.draft_from_graph(id).await
    }

    async fn delete_draft(&self, id: &str) -> ProviderResult<()> {
        check_graph_id(id)?;
        self.client
            .delete_message(&self.address, id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn send_draft(&self, id: &str) -> ProviderResult<()> {
        check_graph_id(id)?;
        self.client
            .send_draft(&self.address, id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn people(&self) -> ProviderResult<Vec<PersonHint>> {
        let people = self
            .client
            .list_people(&self.address, 100)
            .await
            .map_err(|e| self.err(e))?;
        Ok(people
            .into_iter()
            .map(|p| PersonHint {
                name: p.display_name,
                address: p.address,
            })
            .collect())
    }

    async fn list_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> ProviderResult<Vec<Event>> {
        let mut out = Vec::new();
        let calendars = self.calendar_ids().await.unwrap_or_default();
        let targets: Vec<Option<String>> = if calendars.is_empty() {
            vec![None]
        } else {
            calendars.into_iter().map(Some).collect()
        };
        let mut first_error = None;
        for cal in targets {
            let mut page = match self
                .client
                .list_calendar_view(&self.address, cal.as_deref(), from, to, 200)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    first_error.get_or_insert(e);
                    continue;
                }
            };
            out.append(&mut page.events);
            let mut next = page.next_link;
            while let Some(link) = next {
                let mut more = self
                    .client
                    .list_events_at(&self.address, &link)
                    .await
                    .map_err(|e| self.err(e))?;
                out.append(&mut more.events);
                next = more.next_link;
            }
        }
        if out.is_empty()
            && let Some(e) = first_error
        {
            return Err(self.err(e));
        }
        let mut seen = HashSet::new();
        out.retain(|e| seen.insert(e.id.clone()));
        out.sort_by_key(|e| e.start.at);
        Ok(out)
    }

    async fn get_event(&self, id: &str) -> ProviderResult<Event> {
        check_graph_id(id)?;
        self.client
            .get_event(&self.address, id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn create_event(&self, spec: &EventSpec) -> ProviderResult<Event> {
        let (start, end, all_day, tz) = match &spec.when {
            EventWhen::Timed { start, end } => (start.at, end.at, false, start.tz.clone()),
            EventWhen::AllDay { first, last } => {
                let s = first.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
                let e = (*last + Duration::days(1))
                    .and_hms_opt(0, 0, 0)
                    .unwrap_or_default()
                    .and_utc();
                (s, e, true, "UTC".to_string())
            }
        };
        let new = NewEvent {
            subject: spec.title.clone(),
            start,
            end,
            tz,
            all_day,
            location: spec.location.clone(),
            body_text: spec.body.clone(),
            body_html: false,
            required_attendees: spec.attendees.iter().map(|a| a.address.clone()).collect(),
            optional_attendees: Vec::new(),
            recurrence: None,
            online_meeting: spec.online_meeting,
            reminder: Reminder::Default,
        };
        let id = self
            .client
            .create_event(&self.address, None, &new)
            .await
            .map_err(|e| self.err(e))?;
        self.get_event(&id).await
    }

    async fn update_event(&self, id: &str, patch: &EventPatch) -> ProviderResult<Event> {
        check_graph_id(id)?;
        let mut body = serde_json::Map::new();
        if let Some(t) = &patch.title {
            body.insert("subject".into(), json!(t));
        }
        if let Some(when) = &patch.when {
            let (start, end, all_day) = when_json(when);
            body.insert("start".into(), start);
            body.insert("end".into(), end);
            body.insert("isAllDay".into(), json!(all_day));
        }
        if let Some(l) = &patch.location {
            body.insert("location".into(), json!({ "displayName": l }));
        }
        if let Some(b) = &patch.body {
            body.insert(
                "body".into(),
                json!({ "contentType": "text", "content": b }),
            );
        }
        if let Some(att) = &patch.attendees {
            let list: Vec<Value> = att
                .iter()
                .map(|a| {
                    let mut v = recipient_json(a);
                    v["type"] = json!("required");
                    v
                })
                .collect();
            body.insert("attendees".into(), Value::Array(list));
        }
        if let Some(online) = patch.online_meeting {
            body.insert("isOnlineMeeting".into(), json!(online));
            if online {
                body.insert("onlineMeetingProvider".into(), json!("teamsForBusiness"));
            }
        }
        if !body.is_empty() {
            self.send_json(
                reqwest::Method::PATCH,
                &self.url(&format!("/me/events/{id}")),
                &Value::Object(body),
            )
            .await?;
        }
        self.get_event(id).await
    }

    async fn cancel_event(&self, id: &str, message: Option<&str>) -> ProviderResult<()> {
        check_graph_id(id)?;
        self.client
            .cancel_event(&self.address, id, message.unwrap_or(""))
            .await
            .map_err(|e| self.err(e))
    }

    async fn delete_event(&self, id: &str) -> ProviderResult<()> {
        check_graph_id(id)?;
        self.client
            .delete_event(&self.address, id)
            .await
            .map_err(|e| self.err(e))
    }

    async fn respond(
        &self,
        id: &str,
        response: Rsvp,
        message: Option<&str>,
        notify: bool,
    ) -> ProviderResult<()> {
        check_graph_id(id)?;
        let kind = match response {
            Rsvp::Accept => RsvpKind::Accept,
            Rsvp::Tentative => RsvpKind::Tentative,
            Rsvp::Decline => RsvpKind::Decline,
        };
        self.client
            .rsvp_event(&self.address, id, kind, message.unwrap_or(""), notify, None)
            .await
            .map_err(|e| self.err(e))
    }

    async fn free_busy(
        &self,
        people: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> ProviderResult<Vec<(String, Vec<BusyBlock>)>> {
        let fmt = |d: DateTime<Utc>| d.format("%Y-%m-%dT%H:%M:%S").to_string();
        let body = json!({
            "schedules": people,
            "startTime": { "dateTime": fmt(from), "timeZone": "UTC" },
            "endTime": { "dateTime": fmt(to), "timeZone": "UTC" },
            "availabilityViewInterval": 30,
        });
        let token = self.token().await?;
        let req = self
            .client
            .http
            .post(self.url("/me/calendar/getSchedule"))
            .bearer_auth(&token)
            .header("Prefer", "outlook.timezone=\"UTC\"")
            .json(&body);
        let v = match self.json_response(super::send_with_retry(req).await).await {
            Ok(v) => v,
            Err(ProviderError::Invalid(m)) | Err(ProviderError::NotFound(m)) => {
                return Err(ProviderError::Unsupported(format!(
                    "this account cannot see other people's calendars ({m})"
                )));
            }
            Err(e) => return Err(e),
        };
        let parse = |t: &Value| {
            t.get("dateTime")
                .and_then(Value::as_str)
                .and_then(|s| {
                    chrono::NaiveDateTime::parse_from_str(
                        s.split('.').next().unwrap_or(s),
                        "%Y-%m-%dT%H:%M:%S",
                    )
                    .ok()
                })
                .map(|n| n.and_utc())
        };
        let mut out = Vec::new();
        for s in v
            .get("value")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let who = str_of(s, "scheduleId").unwrap_or_default();
            let blocks = s
                .get("scheduleItems")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|i| i.get("status").and_then(Value::as_str) != Some("free"))
                .filter_map(|i| {
                    Some(BusyBlock {
                        start: parse(i.get("start")?)?,
                        end: parse(i.get("end")?)?,
                    })
                })
                .collect();
            out.push((who, blocks));
        }
        Ok(out)
    }
}

/// Make a mailbox's first sync window easy to reason about in tests.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthClient, TokenBackend, TokenSet};
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Fixed;
    #[async_trait]
    impl TokenBackend for Fixed {
        async fn load(&self, _email: &str) -> Result<Option<TokenSet>, ClientError> {
            Ok(Some(TokenSet {
                access_token: "AT".into(),
                refresh_token: "RT".into(),
                expires_at: Utc::now() + Duration::hours(1),
                scope: None,
            }))
        }
        async fn save(&self, _email: &str, _tokens: &TokenSet) -> Result<(), ClientError> {
            Ok(())
        }
    }

    fn provider(server: &MockServer) -> GraphProvider {
        let auth = AuthClient::for_test("cid", server.uri()).with_backend(Arc::new(Fixed));
        let client = GraphClient::for_test(auth, server.uri());
        GraphProvider::new(Arc::new(client), "me@example.com")
    }

    async fn mount_folders(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/$batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responses": [
                    { "id": "inbox", "status": 200, "body": { "id": "F-IN" } },
                    { "id": "archive", "status": 200, "body": { "id": "F-AR" } },
                    { "id": "sentitems", "status": 200, "body": { "id": "F-SE" } },
                ]
            })))
            .mount(server)
            .await;
    }

    fn msg(id: &str, folder: &str) -> Value {
        json!({
            "id": id, "conversationId": "c1", "parentFolderId": folder,
            "from": { "emailAddress": { "name": "Anna", "address": "anna@example.com" } },
            "toRecipients": [], "ccRecipients": [],
            "subject": "Hej", "receivedDateTime": "2026-11-01T08:00:00Z",
            "bodyPreview": "Preview", "isRead": false,
            "flag": { "flagStatus": "flagged" }, "hasAttachments": true,
            "categories": ["Uppdrag"], "inferenceClassification": "other", "isDraft": false
        })
    }

    #[test]
    fn ids_outside_graphs_alphabet_are_refused() {
        assert!(check_graph_id("AAMkAGI2T=").is_ok());
        assert!(check_graph_id("../me").is_err());
        assert!(check_graph_id("a/b").is_err());
        assert!(check_graph_id("").is_err());
    }

    #[test]
    fn kql_quotes_and_joins() {
        let q = SearchQuery {
            text: Some("avtal".into()),
            from: Some("Anna \"B\"".into()),
            after: NaiveDate::from_ymd_opt(2026, 10, 1),
            has_attachments: Some(true),
            ..Default::default()
        };
        assert_eq!(
            kql(&q),
            "avtal AND from:\"Anna  B \" AND received>=2026-10-01 AND hasattachments:true"
        );
    }

    #[test]
    fn items_map_graph_fields() {
        let item = item_from_value(&msg("m1", "F-IN"), "me@example.com", Folder::Inbox).unwrap();
        assert_eq!(item.thread_id, "c1");
        assert!(item.unread && item.flagged && item.has_attachments);
        assert_eq!(item.bulk_hint, Some(BulkHint::Other));
        assert_eq!(item.categories, vec!["Uppdrag".to_string()]);
    }

    #[tokio::test]
    async fn bootstrap_sync_maps_folders_and_builds_a_cursor() {
        let server = MockServer::start().await;
        mount_folders(&server).await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/inbox/messages/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [msg("m1", "F-IN")],
                "@odata.deltaLink": format!("{}/delta-inbox?token=1", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/archive/messages/delta"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me/mailFolders/sentitems/messages/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [msg("m2", "F-SE")],
                "@odata.deltaLink": format!("{}/delta-sent?token=1", server.uri()),
            })))
            .mount(&server)
            .await;
        let p = provider(&server);
        let sync = p
            .sync_mail(Utc::now() - Duration::days(90), None)
            .await
            .unwrap();
        assert_eq!(sync.upserts.len(), 2);
        assert_eq!(sync.upserts[0].folder, Folder::Inbox);
        assert_eq!(sync.upserts[1].folder, Folder::Sent);
        let cursor: SyncCursor = serde_json::from_str(&sync.cursor).unwrap();
        assert_eq!(cursor.links.len(), 2);
    }

    #[tokio::test]
    async fn incremental_sync_refetches_changes_and_reports_removals() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/delta-inbox"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [ { "id": "gone", "@removed": { "reason": "deleted" } }, { "id": "m1", "isRead": true } ],
                "@odata.deltaLink": format!("{}/delta-inbox?token=2", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/$batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responses": [
                    { "id": "inbox", "status": 200, "body": { "id": "F-IN" } },
                    { "id": "0", "status": 200, "body": msg("m1", "F-IN") },
                ]
            })))
            .mount(&server)
            .await;
        let p = provider(&server);
        let cursor = serde_json::to_string(&SyncCursor {
            v: 1,
            links: HashMap::from([(
                "inbox".to_string(),
                format!("{}/delta-inbox?token=1", server.uri()),
            )]),
        })
        .unwrap();
        let sync = p.sync_mail(Utc::now(), Some(&cursor)).await.unwrap();
        assert_eq!(sync.removed, vec!["gone".to_string()]);
        assert_eq!(sync.upserts.len(), 1);
        assert_eq!(sync.upserts[0].id, "m1");
    }

    #[tokio::test]
    async fn expired_delta_is_a_cursor_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/delta-inbox$"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;
        let p = provider(&server);
        let cursor = serde_json::to_string(&SyncCursor {
            v: 1,
            links: HashMap::from([(
                "inbox".to_string(),
                format!("{}/delta-inbox?token=1", server.uri()),
            )]),
        })
        .unwrap();
        let err = p.sync_mail(Utc::now(), Some(&cursor)).await.unwrap_err();
        assert_eq!(err, ProviderError::CursorExpired);
    }

    #[tokio::test]
    async fn archive_moves_in_one_batch_with_per_id_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/$batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responses": [
                    { "id": "0", "status": 201, "body": {} },
                    { "id": "1", "status": 404, "body": { "error": { "message": "gone" } } },
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let p = provider(&server);
        let results = p
            .apply(
                &["a".into(), "b".into(), "../x".into()],
                &MailChange::Move {
                    to: Folder::Archive,
                },
            )
            .await;
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(ProviderError::NotFound(_))));
        assert!(matches!(results[2], Err(ProviderError::Invalid(_))));
    }
}
