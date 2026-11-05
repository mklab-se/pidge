//! Gmail search strings (`q`) from pidge's [`SearchQuery`].

use chrono::{DateTime, Utc};
use pidge_core::provider::{Folder, SearchQuery};

/// What a [`SearchQuery`] becomes in a `messages.list` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GmailSearch {
    pub q: String,
    /// A user label to restrict to (`labelIds`), for `Folder::Other`.
    pub label_id: Option<String>,
    /// Trash and spam are only searched when asked for.
    pub include_spam_trash: bool,
}

/// A value for an operator such as `from:`: bare when it is one word,
/// otherwise quoted. Quotes inside are dropped (Gmail has no escape).
fn operand(v: &str) -> String {
    let v: String = v
        .chars()
        .filter(|c| *c != '"')
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let v = v.trim();
    if v.is_empty() {
        String::new()
    } else if v.contains(|c: char| c.is_whitespace() || "(){}".contains(c)) {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

/// The `q` for a sync bootstrap: everything received since `since` outside
/// spam, trash and drafts.
pub(crate) fn sync_q(since: DateTime<Utc>) -> String {
    format!(
        "after:{} -in:spam -in:trash -in:drafts",
        since.timestamp().max(0)
    )
}

pub(crate) fn build(query: &SearchQuery) -> GmailSearch {
    let mut parts: Vec<String> = Vec::new();
    if let Some(text) = query
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        // Free text is passed through: Gmail's own operators in it work.
        parts.push(text.replace(['\r', '\n'], " "));
    }
    if let Some(from) = query.from.as_deref().map(operand).filter(|s| !s.is_empty()) {
        parts.push(format!("from:{from}"));
    }
    if let Some(subject) = query
        .subject
        .as_deref()
        .map(operand)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("subject:{subject}"));
    }
    if let Some(after) = query.after {
        parts.push(format!("after:{}", after.format("%Y/%m/%d")));
    }
    if let Some(before) = query.before {
        parts.push(format!("before:{}", before.format("%Y/%m/%d")));
    }
    match query.has_attachments {
        Some(true) => parts.push("has:attachment".into()),
        Some(false) => parts.push("-has:attachment".into()),
        None => {}
    }
    match query.unread {
        Some(true) => parts.push("is:unread".into()),
        Some(false) => parts.push("-is:unread".into()),
        None => {}
    }
    let mut label_id = None;
    let mut include_spam_trash = false;
    match &query.folder {
        None => {}
        Some(Folder::Inbox) => parts.push("in:inbox".into()),
        Some(Folder::Archive) => {
            parts.push("-in:inbox -in:sent -in:drafts -in:spam -in:trash".into())
        }
        Some(Folder::Sent) => parts.push("in:sent".into()),
        Some(Folder::Drafts) => parts.push("in:drafts".into()),
        Some(Folder::Deleted) => {
            parts.push("in:trash".into());
            include_spam_trash = true;
        }
        Some(Folder::Junk) => {
            parts.push("in:spam".into());
            include_spam_trash = true;
        }
        Some(Folder::Other(id)) => label_id = Some(id.clone()),
    }
    GmailSearch {
        q: parts.join(" "),
        label_id,
        include_spam_trash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn every_field_becomes_an_operator() {
        let q = SearchQuery {
            text: Some("invoice".into()),
            from: Some("Jane Doe".into()),
            subject: Some("Q3".into()),
            after: NaiveDate::from_ymd_opt(2026, 1, 2),
            before: NaiveDate::from_ymd_opt(2026, 3, 4),
            has_attachments: Some(true),
            folder: Some(Folder::Inbox),
            unread: Some(true),
            limit: 10,
        };
        let s = build(&q);
        assert_eq!(
            s.q,
            "invoice from:\"Jane Doe\" subject:Q3 after:2026/01/02 before:2026/03/04 \
             has:attachment is:unread in:inbox"
        );
        assert_eq!(s.label_id, None);
        assert!(!s.include_spam_trash);
    }

    #[test]
    fn folders_map_to_gmail_scopes() {
        let with = |f: Folder| {
            build(&SearchQuery {
                folder: Some(f),
                ..Default::default()
            })
        };
        assert_eq!(
            with(Folder::Archive).q,
            "-in:inbox -in:sent -in:drafts -in:spam -in:trash"
        );
        let t = with(Folder::Deleted);
        assert_eq!(t.q, "in:trash");
        assert!(t.include_spam_trash);
        assert!(with(Folder::Junk).include_spam_trash);
        let o = with(Folder::Other("Label_7".into()));
        assert_eq!(o.q, "");
        assert_eq!(o.label_id.as_deref(), Some("Label_7"));
    }

    #[test]
    fn negations_and_quotes() {
        let s = build(&SearchQuery {
            from: Some("a\"b@example.com".into()),
            has_attachments: Some(false),
            unread: Some(false),
            ..Default::default()
        });
        assert_eq!(s.q, "from:ab@example.com -has:attachment -is:unread");
    }

    #[test]
    fn sync_window_uses_unix_seconds() {
        let since = DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            sync_q(since),
            "after:1793491200 -in:spam -in:trash -in:drafts"
        );
    }
}
