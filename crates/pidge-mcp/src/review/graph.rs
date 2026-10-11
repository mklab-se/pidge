//! The Graph calls a review makes, all through `$batch`: one listing of
//! unread inbox mail (plus the inbox's unread count), one round of headers
//! for the mailing-list signal, and the actions. Bodies come from
//! `get_message` one at a time, prefetched while the user listens.

use std::collections::HashMap;

use chrono::{DateTime, SecondsFormat, Utc};
use pidge_client::ClientError;
use pidge_client::graph::batch::{BatchRequest, BatchResponse};
use serde_json::{Value, json};

use super::classify::{Candidate, header_signals, is_noreply};
use crate::state::SharedState;

/// Most candidates listed per mailbox.
pub const MAX_CANDIDATES: usize = 100;

const SELECT: &str = "id,subject,from,receivedDateTime,bodyPreview,hasAttachments,toRecipients,inferenceClassification,internetMessageId,categories";

/// Which inbox mail to list.
#[derive(Debug, Clone)]
pub struct Listing {
    pub since: Option<DateTime<Utc>>,
    /// Unread only (a review), or read and unread (`sort_mail`).
    pub unread_only: bool,
    pub top: usize,
    /// One more `$filter` clause, already OData-escaped (a category filter).
    pub extra: Option<String>,
}

impl Listing {
    /// A review's listing: unread mail since `since`.
    pub fn unread(since: Option<DateTime<Utc>>) -> Self {
        Self {
            since,
            unread_only: true,
            top: MAX_CANDIDATES,
            extra: None,
        }
    }
}

/// Inbox mail received at or after `since` (all when `None`), newest
/// first, and the inbox's total unread count.
pub async fn list_candidates(
    state: &SharedState,
    account: &str,
    listing: Listing,
    my_addresses: &[String],
) -> Result<(Vec<Candidate>, Option<u64>), ClientError> {
    // Graph wants an `$orderby` property first in `$filter` too.
    let since = listing.since.map_or_else(
        || "1900-01-01T00:00:00Z".to_string(),
        |t| t.to_rfc3339_opts(SecondsFormat::Secs, true),
    );
    let mut filter = format!("receivedDateTime ge {since}");
    if listing.unread_only {
        filter.push_str(" and isRead eq false");
    }
    if let Some(extra) = &listing.extra {
        filter.push_str(&format!(" and ({extra})"));
    }
    let list_url = format!(
        "/me/mailFolders/inbox/messages?$filter={}&$orderby=receivedDateTime%20desc&$top={}&$select={SELECT}",
        encode(&filter),
        listing.top
    );
    let requests = vec![
        BatchRequest::bare("list", "GET", list_url),
        BatchRequest::bare(
            "count",
            "GET",
            "/me/mailFolders/inbox?$select=unreadItemCount",
        ),
    ];
    let responses = state.graph.batch_all(account, requests).await?;
    let find = |id: &str| responses.iter().find(|r| r.id == id);
    let list = find("list").ok_or_else(|| missing("listing"))?;
    if !list.is_success() {
        return Err(sub_error(list));
    }
    let unread = find("count")
        .filter(|r| r.is_success())
        .and_then(|r| r.body.get("unreadItemCount"))
        .and_then(Value::as_u64);
    let candidates = list
        .body
        .get("value")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| candidate(account, row, my_addresses))
                .collect()
        })
        .unwrap_or_default();
    Ok((candidates, unread))
}

fn candidate(account: &str, row: &Value, my_addresses: &[String]) -> Option<Candidate> {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    let id = s(row.get("id"));
    // Ids go back into Graph paths: only Graph's own token alphabet.
    if crate::tools::mail_read::check_id(&id).is_err() {
        return None;
    }
    let received_at = row
        .get("receivedDateTime")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())?
        .with_timezone(&Utc);
    let from = row.pointer("/from/emailAddress");
    let from_address = s(from.and_then(|f| f.get("address")));
    let to_me = row
        .get("toRecipients")
        .and_then(Value::as_array)
        .is_some_and(|to| {
            to.iter().any(|r| {
                let a = r
                    .pointer("/emailAddress/address")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                my_addresses.iter().any(|m| m.eq_ignore_ascii_case(a))
            })
        });
    let focused = match row.get("inferenceClassification").and_then(Value::as_str) {
        Some("focused") => Some(true),
        Some("other") => Some(false),
        _ => None,
    };
    Some(Candidate {
        account: account.to_string(),
        automated: is_noreply(&from_address),
        from_name: clean(&s(from.and_then(|f| f.get("name")))),
        from_address: clean(&from_address),
        id,
        to_me,
        subject: clean(&s(row.get("subject"))),
        preview: clean(&s(row.get("bodyPreview"))),
        has_attachments: row
            .get("hasAttachments")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        received_at,
        focused,
        list: false,
        internet_id: row
            .get("internetMessageId")
            .and_then(Value::as_str)
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty()),
        categories: row
            .get("categories")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Adds the header signals (mailing list, automated) to `candidates`, one
/// `$batch` per mailbox. A mailbox whose headers can't be read keeps the
/// sender-based signal only.
pub async fn add_header_signals(state: &SharedState, candidates: &mut [Candidate]) {
    let mut by_account: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in candidates.iter().enumerate() {
        by_account.entry(c.account.clone()).or_default().push(i);
    }
    for (account, indexes) in by_account {
        let requests = indexes
            .iter()
            .map(|&i| {
                BatchRequest::bare(
                    i.to_string(),
                    "GET",
                    format!(
                        "/me/messages/{}?$select=internetMessageHeaders",
                        candidates[i].id
                    ),
                )
            })
            .collect();
        let Ok(responses) = state.graph.batch_all(&account, requests).await else {
            tracing::warn!("review: headers could not be read for a mailbox");
            continue;
        };
        for r in responses.iter().filter(|r| r.is_success()) {
            let Some(c) =
                r.id.parse::<usize>()
                    .ok()
                    .and_then(|i| candidates.get_mut(i))
            else {
                continue;
            };
            let headers: Vec<(String, String)> = r
                .body
                .get("internetMessageHeaders")
                .and_then(Value::as_array)
                .map(|hs| {
                    hs.iter()
                        .filter_map(|h| {
                            Some((
                                h.get("name")?.as_str()?.to_string(),
                                h.get("value")?.as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let (list, automated) = header_signals(&headers);
            c.list |= list;
            c.automated |= automated;
        }
    }
}

/// A review action as Graph sub-requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    MarkRead,
    MarkUnread,
    Flag,
    Unflag,
    Move(&'static str),
}

impl Op {
    fn request(self, key: String, id: &str) -> BatchRequest {
        let patch = |body: Value| {
            BatchRequest::json(key.clone(), "PATCH", format!("/me/messages/{id}"), body)
        };
        match self {
            Op::MarkRead => patch(json!({ "isRead": true })),
            Op::MarkUnread => patch(json!({ "isRead": false })),
            Op::Flag => patch(json!({ "isRead": true, "flag": { "flagStatus": "flagged" } })),
            Op::Unflag => patch(json!({ "isRead": false, "flag": { "flagStatus": "notFlagged" } })),
            Op::Move(folder) => BatchRequest::json(
                key,
                "POST",
                format!("/me/messages/{id}/move"),
                json!({ "destinationId": folder }),
            ),
        }
    }
}

/// How one message fared: `Ok(Some(new_id))` after a move, `Ok(None)`
/// otherwise, or a short reason (never a Graph body).
pub type OpResult = Result<Option<String>, String>;

/// Runs `op` on `ids` of one mailbox in one `$batch`.
pub async fn run(state: &SharedState, account: &str, ids: &[String], op: Op) -> Vec<OpResult> {
    let requests = ids
        .iter()
        .enumerate()
        .map(|(i, id)| op.request(i.to_string(), id))
        .collect();
    let mut out: Vec<OpResult> = vec![Err("no response from Microsoft".into()); ids.len()];
    match state.graph.batch_all(account, requests).await {
        Ok(responses) => {
            for r in responses {
                let Some(slot) = r.id.parse::<usize>().ok().and_then(|i| out.get_mut(i)) else {
                    continue;
                };
                *slot = if r.is_success() {
                    Ok(r.body
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|_| matches!(op, Op::Move(_))))
                } else if r.status == 404 {
                    Err("not found (moved or deleted elsewhere?)".into())
                } else {
                    Err(format!("Microsoft answered {}", r.status))
                };
            }
        }
        Err(e) => {
            let reason = match e {
                ClientError::SessionExpired { .. } => "the mailbox needs reconnecting".to_string(),
                ClientError::Throttled { .. } => "Microsoft is throttling".to_string(),
                _ => "the request to Microsoft failed".to_string(),
            };
            out.iter_mut().for_each(|o| *o = Err(reason.clone()));
        }
    }
    out
}

/// The first of `accounts` holding message `id`.
pub async fn locate(state: &SharedState, accounts: &[String], id: &str) -> Option<String> {
    if accounts.len() == 1 {
        return accounts.first().cloned();
    }
    for account in accounts {
        let req = vec![BatchRequest::bare(
            "0",
            "GET",
            format!("/me/messages/{id}?$select=id"),
        )];
        if let Ok(r) = state.graph.batch_all(account, req).await
            && r.iter().any(|r| r.is_success())
        {
            return Some(account.clone());
        }
    }
    None
}

fn sub_error(r: &BatchResponse) -> ClientError {
    ClientError::Graph {
        status: r.status,
        message: String::new(),
    }
}

fn missing(what: &str) -> ClientError {
    ClientError::Graph {
        status: 502,
        message: format!("no {what} in the batch response"),
    }
}

/// Percent-encodes an OData query value for a `$batch` relative URL.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Header text on one line: control characters dropped, whitespace runs
/// collapsed, so a sender can't start a line of its own.
fn clean(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_percent_encoded() {
        assert_eq!(
            encode("receivedDateTime ge 2026-10-11T00:00:00Z and isRead eq false"),
            "receivedDateTime%20ge%202026-10-11T00:00:00Z%20and%20isRead%20eq%20false"
        );
    }

    #[test]
    fn a_row_becomes_a_candidate() {
        let row = json!({
            "id": "AAMk1",
            "subject": "Weekend\r\nInjected: line",
            "from": { "emailAddress": { "name": "Anna", "address": "anna@example.com" } },
            "receivedDateTime": "2026-10-11T06:14:00Z",
            "bodyPreview": "Hi",
            "hasAttachments": true,
            "toRecipients": [ { "emailAddress": { "address": "JANE@example.com" } } ],
            "inferenceClassification": "focused"
        });
        let c = candidate("jane@example.com", &row, &["jane@example.com".into()]).unwrap();
        assert_eq!(c.subject, "Weekend Injected: line");
        assert!(c.to_me && c.has_attachments && !c.automated);
        assert_eq!(c.focused, Some(true));
        assert!(candidate("jane@example.com", &json!({"id": ""}), &[]).is_none());
    }
}
