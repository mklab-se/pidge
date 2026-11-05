//! Every operation, and the pieces they share: selectors, handles, item
//! lines, recipient resolution.

pub mod accounts;
pub mod calendar;
pub mod mail_act;
pub mod mail_read;
pub mod mail_setup;
pub mod mail_write;
pub mod triage;

use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use pidge_core::MessageFrom;
use pidge_core::provider::Folder;
use pidge_core::timerange::parse_point;
use pidge_store::{Filter, HandleKind, Stored, Target};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ctx::User;
use crate::error::{ErrorKind, OpError, OpResult};
use crate::kinds::Kind;
use crate::registry::OpDef;
use crate::render;

pub fn all() -> Vec<OpDef> {
    vec![
        mail_read::brief(),
        mail_read::find(),
        mail_read::read(),
        triage::triage(),
        mail_act::act(),
        mail_write::draft(),
        mail_write::send(),
        mail_read::attachment(),
        mail_setup::setup(),
        calendar::agenda(),
        calendar::event(),
        calendar::act(),
        calendar::respond(),
        calendar::availability(),
        accounts::accounts(),
    ]
}

/// A set of messages, described rather than listed. Shared by `mail_find`,
/// `mail_act`, `mail_triage` and the plans they make.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, serde::Serialize, JsonSchema)]
pub struct Selector {
    /// Words in the subject, preview or sender.
    pub text: Option<String>,
    /// Sender: a name or an address (part of one is enough).
    pub from: Option<String>,
    /// Only mail sent directly to the user (in To, not only Cc).
    pub to_me: Option<bool>,
    /// `inbox` (default for most uses), `archive`, `sent`, `drafts`,
    /// `deleted`, `junk`, or `any`.
    pub folder: Option<String>,
    /// Received on or after: `last_seen` (since the user last looked),
    /// `today`, `yesterday`, `this_week`, `Nd` (e.g. `7d`), or a date.
    pub since: Option<String>,
    /// Received before this date (`YYYY-MM-DD`).
    pub before: Option<String>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub has_attachments: Option<bool>,
    /// One of the user's categories.
    pub category: Option<String>,
    /// What the mail is: `receipt` (receipts, invoices, bills),
    /// `newsletter`, `promotion` (ads, "commercials"), `notification`,
    /// `social`, `invite`, `personal`. Several with commas.
    pub kind: Option<String>,
    /// `high` (only what matters) or `low` (only bulk).
    pub importance: Option<String>,
    /// One of the user's mailboxes; all when absent.
    pub account: Option<String>,
}

impl Selector {
    pub fn is_empty(&self) -> bool {
        *self == Selector::default()
    }

    /// The kinds named, validated.
    pub fn kinds(&self) -> OpResult<Vec<Kind>> {
        let Some(k) = &self.kind else {
            return Ok(Vec::new());
        };
        k.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                Kind::parse(s).ok_or_else(|| {
                    OpError::usage(format!(
                        "unknown kind {s:?}; use receipt, newsletter, promotion, notification, social, invite or personal"
                    ))
                })
            })
            .collect()
    }

    /// A short description, for plans: "promotions in the inbox from shop.se".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Ok(kinds) = self.kinds()
            && !kinds.is_empty()
        {
            parts.push(
                kinds
                    .iter()
                    .map(|k| k.plural())
                    .collect::<Vec<_>>()
                    .join(" and "),
            );
        } else if self.unread == Some(true) {
            parts.push("unread mail".into());
        } else {
            parts.push("mail".into());
        }
        if let Some(c) = &self.category {
            parts.push(format!("in {}", render::inline(c)));
        }
        if let Some(f) = &self.from {
            parts.push(format!("from {}", render::inline(f)));
        }
        if let Some(t) = &self.text {
            parts.push(format!("about \"{}\"", render::inline(t)));
        }
        match self.folder.as_deref() {
            Some("any") => {}
            Some(f) => parts.push(format!("in {f}")),
            None => parts.push("in the inbox".into()),
        }
        if let Some(s) = &self.since {
            parts.push(format!("since {s}"));
        }
        if let Some(b) = &self.before {
            parts.push(format!("before {b}"));
        }
        if let Some(a) = &self.account {
            parts.push(format!("({a})"));
        }
        parts.join(" ")
    }
}

/// A point in time from `since`-style input, looking back from `now`.
pub fn since_point(
    s: &str,
    tz: Tz,
    now: DateTime<Utc>,
    seen: Option<DateTime<Utc>>,
) -> OpResult<DateTime<Utc>> {
    let s = s.trim();
    let today = now.with_timezone(&tz).date_naive();
    let midnight = |d: NaiveDate| parse_point(&d.to_string(), tz, false).map_err(OpError::usage);
    match s {
        "last_seen" | "new" | "unseen" => Ok(seen.unwrap_or_else(|| now - Duration::days(1))),
        "today" => midnight(today),
        "yesterday" => midnight(today - Duration::days(1)),
        "this_week" | "week" => {
            use chrono::Datelike;
            midnight(today - Duration::days(today.weekday().num_days_from_monday().into()))
        }
        "this_month" | "month" => {
            use chrono::Datelike;
            midnight(today.with_day(1).unwrap_or(today))
        }
        _ => {
            if let Some(n) = s
                .strip_suffix('d')
                .and_then(|n| n.trim().parse::<i64>().ok())
            {
                return midnight(today - Duration::days(n.clamp(1, 3650) - 1));
            }
            if let Some(n) = s
                .strip_suffix('h')
                .and_then(|n| n.trim().parse::<i64>().ok())
            {
                return Ok(now - Duration::hours(n.clamp(1, 24 * 365)));
            }
            parse_point(s, tz, false).map_err(|_| {
                OpError::usage(format!(
                    "cannot read {s:?} as a time; use last_seen, today, yesterday, this_week, 7d or YYYY-MM-DD"
                ))
            })
        }
    }
}

/// Turn a selector into an index filter. `default_folder` applies when the
/// selector names none.
pub async fn to_filter(
    user: &Arc<User>,
    sel: &Selector,
    default_folder: Option<Folder>,
) -> OpResult<Filter> {
    let tz = user.tz().await;
    let now = Utc::now();
    let prefs = user.prefs().await;
    let accounts: Vec<String> = user
        .scope(sel.account.as_deref())?
        .iter()
        .map(|p| p.address().to_string())
        .collect();
    let seen = prefs.seen_since(&accounts);
    let folder = match sel.folder.as_deref().map(str::trim) {
        Some("any" | "all") => None,
        Some(f) if !f.is_empty() => Some(Folder::from_key(f)),
        _ => default_folder,
    };
    let mut f = Filter {
        accounts,
        folder,
        unread: sel.unread,
        flagged: sel.flagged,
        has_attachments: sel.has_attachments,
        from: sel.from.clone().filter(|s| !s.trim().is_empty()),
        text: sel.text.clone().filter(|s| !s.trim().is_empty()),
        kinds: sel
            .kinds()?
            .iter()
            .map(|k| k.as_str().to_string())
            .collect(),
        category: sel.category.clone(),
        ..Default::default()
    };
    if let Some(s) = &sel.since {
        f.since = Some(since_point(s, tz, now, seen)?);
    }
    if let Some(b) = &sel.before {
        f.before = Some(since_point(b, tz, now, seen)?);
    }
    match sel.importance.as_deref().map(str::trim) {
        Some("high") | Some("important") => f.min_importance = Some(2),
        Some("low") => {}
        Some("") | None => {}
        Some(other) => {
            return Err(OpError::usage(format!(
                "importance is high or low, not {other:?}"
            )));
        }
    }
    if let Some(c) = &sel.category {
        let cats = prefs.categories();
        if !c.eq_ignore_ascii_case(crate::prefs::OTHER)
            && !cats.iter().any(|x| x.name.eq_ignore_ascii_case(c))
        {
            return Err(OpError::usage(format!(
                "{c:?} is not one of your categories ({})",
                cats.iter()
                    .map(|x| x.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    Ok(f)
}

/// Apply the parts of a selector the index filter cannot express.
pub fn post_filter(sel: &Selector, items: Vec<Stored>, own: &[String]) -> Vec<Stored> {
    let own: Vec<String> = own.iter().map(|a| a.to_ascii_lowercase()).collect();
    items
        .into_iter()
        .filter(|s| match sel.to_me {
            Some(true) => s
                .item
                .to
                .iter()
                .any(|r| own.contains(&r.address.to_ascii_lowercase())),
            _ => true,
        })
        .filter(|s| match sel.importance.as_deref() {
            Some("low") => s.class.importance == 0,
            _ => true,
        })
        .filter(|s| match sel.category.as_deref() {
            Some(c) if c.eq_ignore_ascii_case(crate::prefs::OTHER) => s.class.category.is_none(),
            _ => true,
        })
        .collect()
}

/// Resolve message references: handles (`m12`) or provider ids.
pub fn resolve_message(user: &User, r: &str) -> OpResult<Target> {
    let r = r.trim();
    if let Some(t) = user.store.resolve(r)? {
        if t.kind != HandleKind::Message {
            return Err(OpError::usage(format!("{r} is not a message")));
        }
        return Ok(t);
    }
    if r.len() > 12
        && let Some(t) = user.store.find_by_pid(HandleKind::Message, r)?
    {
        return Ok(t);
    }
    Err(OpError::not_found(format!("no message {r:?}"))
        .with_hint("use a handle such as m12 from a pidge list"))
}

pub fn resolve_event(user: &User, r: &str) -> OpResult<Target> {
    let r = r.trim();
    if let Some(t) = user.store.resolve(r)? {
        if t.kind != HandleKind::Event {
            return Err(OpError::usage(format!("{r} is not an event")));
        }
        return Ok(t);
    }
    if r.len() > 12
        && let Some(t) = user.store.find_by_pid(HandleKind::Event, r)?
    {
        return Ok(t);
    }
    Err(OpError::not_found(format!("no event {r:?}"))
        .with_hint("use a handle such as e3 from calendar_agenda"))
}

/// Short account label when the user has several mailboxes.
pub fn account_tag(user: &User, account: &str) -> String {
    if user.providers().len() > 1 {
        format!(" · {account}")
    } else {
        String::new()
    }
}

/// One message as a list line (and a preview line).
pub fn item_line(user: &User, s: &Stored, tz: Tz, now: DateTime<Utc>, preview: bool) -> String {
    let i = &s.item;
    let mut facts: Vec<String> = Vec::new();
    if let Some(c) = &s.class.category {
        facts.push(render::inline(c));
    } else if let Some(k) = &s.class.kind {
        facts.push(k.clone());
    }
    if i.unread {
        facts.push("unread".into());
    }
    if i.flagged {
        facts.push("flagged".into());
    }
    if i.has_attachments {
        facts.push("attachment".into());
    }
    for r in &s.class.reasons {
        if r != "flagged" {
            facts.push(r.clone());
        }
    }
    let mut line = format!(
        "{} {}  {}: {}  [{}]{}",
        s.handle,
        render::when(i.received_at, tz, now),
        render::name_of(&i.from),
        render::cap_inline(&render::one_line(&i.subject), 100),
        facts.join(", "),
        account_tag(user, &i.account),
    );
    if preview {
        let p = render::cap_inline(&render::one_line(&i.preview), 140);
        if !p.is_empty() {
            line.push_str(&format!("\n    {p}"));
        }
    }
    line
}

pub fn item_json(s: &Stored) -> Value {
    let i = &s.item;
    json!({
        "id": s.handle,
        "account": i.account,
        "from": { "name": i.from.name, "address": i.from.address },
        "subject": i.subject,
        "received": i.received_at,
        "folder": i.folder.key(),
        "unread": i.unread,
        "flagged": i.flagged,
        "has_attachments": i.has_attachments,
        "kind": s.class.kind,
        "category": s.class.category,
        "importance": match s.class.importance { 2 => "high", 0 => "low", _ => "normal" },
        "reasons": s.class.reasons,
        "thread": i.thread_id,
        "preview": i.preview,
    })
}

/// Resolve recipients: addresses pass through, names are looked up among
/// the people the user mails with. Ambiguous or unknown names are errors
/// that list the candidates.
pub fn resolve_people(user: &User, tokens: &[String]) -> OpResult<Vec<MessageFrom>> {
    let own = user.addresses();
    let mut out = Vec::new();
    for t in tokens {
        let t = t.trim().trim_start_matches('@');
        if t.is_empty() {
            continue;
        }
        if let Some((name, addr)) = t
            .rsplit_once('<')
            .map(|(n, a)| (n.trim(), a.trim_end_matches('>').trim()))
        {
            out.push(MessageFrom {
                name: name.trim_matches('"').to_string(),
                address: addr.to_string(),
            });
            continue;
        }
        if t.contains('@') {
            if !valid_address(t) {
                return Err(OpError::usage(format!("{t:?} is not an e-mail address")));
            }
            out.push(MessageFrom {
                name: String::new(),
                address: t.to_string(),
            });
            continue;
        }
        let found = user.store.people(t, &own, 6)?;
        let exact: Vec<_> = found
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(t))
            .collect();
        let pick = if exact.len() == 1 {
            Some(exact[0])
        } else if found.len() == 1 {
            Some(&found[0])
        } else if found.len() > 1
            && (found[0].sent_to * 3 + found[0].received_from)
                >= 4 * (found[1].sent_to * 3 + found[1].received_from).max(1)
        {
            // One person clearly dominates.
            Some(&found[0])
        } else {
            None
        };
        match pick {
            Some(p) => out.push(MessageFrom {
                name: p.name.clone(),
                address: p.address.clone(),
            }),
            None if found.is_empty() => {
                return Err(OpError::new(
                    ErrorKind::NotFound,
                    format!("nobody called {t:?} in your mail; ask the user for the address"),
                ));
            }
            None => {
                let list = found
                    .iter()
                    .map(|p| format!("{} <{}>", render::inline(&p.name), p.address))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(
                    OpError::new(ErrorKind::Ambiguous, format!("{t:?} could be: {list}"))
                        .with_hint("ask the user which one and pass the address"),
                );
            }
        }
    }
    Ok(out)
}

pub fn valid_address(a: &str) -> bool {
    let a = a.trim();
    let Some((local, domain)) = a.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !a.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_understands_people_words() {
        let tz: Tz = "Europe/Stockholm".parse().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-11-05T12:00:00Z")
            .unwrap()
            .to_utc();
        let seen = DateTime::parse_from_rfc3339("2026-11-05T07:30:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(since_point("last_seen", tz, now, Some(seen)).unwrap(), seen);
        assert_eq!(
            since_point("today", tz, now, None).unwrap(),
            DateTime::parse_from_rfc3339("2026-11-04T23:00:00Z")
                .unwrap()
                .to_utc()
        );
        // 7d includes today: six days back at midnight.
        assert_eq!(
            since_point("7d", tz, now, None).unwrap(),
            DateTime::parse_from_rfc3339("2026-10-29T23:00:00Z")
                .unwrap()
                .to_utc()
        );
        assert!(since_point("whenever", tz, now, None).is_err());
    }

    #[test]
    fn addresses_are_checked() {
        assert!(valid_address("a@b.se"));
        assert!(!valid_address("a@b"));
        assert!(!valid_address("a b@c.se"));
    }

    #[test]
    fn selectors_describe_themselves() {
        let s = Selector {
            kind: Some("promotion".into()),
            from: Some("shop.se".into()),
            ..Default::default()
        };
        assert_eq!(s.describe(), "promotions from shop.se in the inbox");
    }
}
