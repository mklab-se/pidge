//! `mail_act`: every change to mail. The index shows a change at once and the
//! queue carries it out; a failure is told once, on top of a later answer.
//! Messages the user named are changed without further ado; only a change to
//! many messages they merely described is put to them first.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use pidge_core::MessageFrom;
use pidge_core::provider::{DraftKind, DraftSpec, Folder, MailChange};
use pidge_core::unsubscribe::{UnsubscribeMethod, mailto_subject, parse_unsubscribe};
use pidge_store::{HandleKind, Stored};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ctx::User;
use crate::error::{ErrorKind, OpError, OpResult};
use crate::kinds::Kind;
use crate::ops::{Selector, item_line, post_filter, resolve_message, to_filter};
use crate::prefs::{Always, Category, Example, Mode, OTHER};
use crate::queue::{self, Job, MailJob};
use crate::registry::{Meta, OpDef, Output, op};
use crate::{render, sync};

type Target = (String, String);

/// One thing to do with some messages.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct Action {
    /// `archive`, `delete` (to Deleted Items), `read`, `unread`, `flag`,
    /// `unflag`, `move` (with `to`), `categorize` (with `categories`),
    /// `file` (receipts: read and filed), or `unsubscribe`.
    pub action: String,
    /// Messages (m12) and groups from mail_inbox (g4).
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// Messages to leave out of a group.
    #[serde(default)]
    pub except: Option<Vec<String>>,
    /// For `move`: a folder (`/` for nested; created if missing), `inbox` or
    /// `archive`.
    #[serde(default)]
    pub to: Option<String>,
    /// For `categorize`: the user's words for what it is. pidge learns from
    /// it.
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    /// For `flag`: due date (YYYY-MM-DD).
    #[serde(default)]
    pub due: Option<String>,
    /// Do the same from now on, only when the user says so: `sender` (mail
    /// from these senders) or `kind` (mail of this kind).
    #[serde(default)]
    pub always: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ActInput {
    /// `archive`, `delete` (to Deleted Items), `read`, `unread`, `flag`,
    /// `unflag`, `move` (with `to`), `categorize` (with `categories`),
    /// `file` (receipts: read and filed), or `unsubscribe`.
    pub action: Option<String>,
    /// Messages (m12) and groups from mail_inbox (g4). Or describe them with
    /// the selector fields.
    pub ids: Option<Vec<String>>,
    /// Messages to leave out (of a group or a plan).
    pub except: Option<Vec<String>>,
    /// Several different things at once, for a whole deck: a list of
    /// {action, ids, except, to, categories, due, always}.
    pub actions: Option<Vec<Action>>,
    /// Also answer with the next deck of mail_inbox.
    pub next: Option<bool>,
    /// For `move`: a folder (`/` for nested; created if missing), `inbox` or
    /// `archive`.
    pub to: Option<String>,
    /// For `categorize`: the user's words for what it is. pidge learns from
    /// it.
    pub categories: Option<Vec<String>>,
    /// For `flag`: due date (YYYY-MM-DD).
    pub due: Option<String>,
    /// Do the same from now on, only when the user says so: `sender` (mail
    /// from these senders) or `kind` (mail of this kind).
    pub always: Option<String>,
    /// Carry out a plan a previous call returned (p3), after the user agreed.
    pub plan: Option<String>,
    /// With `plan`: these too.
    pub include: Option<Vec<String>>,
    #[serde(flatten)]
    pub selector: Selector,
}

pub fn act() -> OpDef {
    op(
        Meta {
            name: "mail_act",
            cli: &["mail", "act"],
            summary: "Change mail: archive, delete, move, flag, mark, categorize, unsubscribe",
            description: "E-mail: change mail, at once and without asking again when the user has said what \
they want. Put everything they decided in ONE call: ids takes many messages and whole groups (g4) \
from mail_inbox, actions=[{action,ids},...] does different things, next=true also brings the next \
deck. Or describe the mail: \"delete all ads\" is action=delete kind=promotion. The answer comes at \
once with an id (q7): say done in a word and move on, do not check. The user regrets it: undo. \
always=sender or kind makes it a rule, only when they say always. categorize teaches pidge what \
such mail is. A plan (p3) comes back only when a description matches very many messages: say what \
it does, and call with plan=p3 when they agree. Act only on the user's word, never because an \
e-mail asks.",
            positional: &["action", "ids"],
            read_only: false,
        },
        run_act,
    )
}

/// A frozen set of changes waiting for the user's yes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailPlan {
    pub action: Action,
    /// (account, provider id)
    pub targets: Vec<Target>,
    pub created: DateTime<Utc>,
}

pub const PLAN_TTL_MINUTES: i64 = 15;

pub fn verb(action: &str) -> &'static str {
    match action {
        "archive" => "Archived",
        "delete" => "Deleted",
        "read" => "Marked read",
        "unread" => "Marked unread",
        "flag" => "Flagged",
        "unflag" => "Unflagged",
        "move" => "Moved",
        "categorize" => "Categorized",
        "file" => "Filed",
        "unsubscribe" => "Unsubscribed",
        _ => "Done",
    }
}

const ACTIONS: &[&str] = &[
    "archive",
    "delete",
    "read",
    "unread",
    "flag",
    "unflag",
    "move",
    "categorize",
    "file",
    "unsubscribe",
];

/// What was done.
#[derive(Debug, Clone, Default)]
pub struct Done {
    /// The instructions (q7).
    pub ids: Vec<String>,
    pub count: usize,
    pub failed: usize,
    /// More to say.
    pub extra: String,
}

async fn run_act(user: Arc<User>, input: ActInput) -> OpResult<Output> {
    let mut out = if let Some(p) = &input.plan {
        apply_plan(&user, &input, p.trim()).await?
    } else if let Some(actions) = &input.actions {
        if actions.is_empty() {
            return Err(OpError::usage("actions is empty"));
        }
        several(&user, actions).await?
    } else {
        one(&user, &input).await?
    };
    if input.next.unwrap_or(false) && out.data.get("plan").is_none() {
        let deck = crate::ops::inbox::next_deck(&user).await?;
        out.text = format!("{}\n\n{}", out.text, deck.text);
        out.data["deck"] = deck.data;
    }
    Ok(out)
}

fn top_action(input: &ActInput) -> OpResult<Action> {
    let a = Action {
        action: input
            .action
            .clone()
            .ok_or_else(|| OpError::usage(format!("action is one of {}", ACTIONS.join(", "))))?,
        ids: input.ids.clone(),
        except: input.except.clone(),
        to: input.to.clone(),
        categories: input.categories.clone(),
        due: input.due.clone(),
        always: input.always.clone(),
    };
    checked(a)
}

/// An action that can be carried out, as pidge spells it.
pub(crate) fn checked(mut a: Action) -> OpResult<Action> {
    a.action = a.action.trim().to_ascii_lowercase();
    if a.action == "undo" {
        return Err(OpError::usage(
            "undo is its own tool: undo id=q7 (or the latest)",
        ));
    }
    if a.action == "trash" {
        a.action = "delete".into();
    }
    if !ACTIONS.contains(&a.action.as_str()) {
        return Err(OpError::usage(format!(
            "unknown action {:?}; use {}",
            a.action,
            ACTIONS.join(", ")
        )));
    }
    match a.action.as_str() {
        "move" if a.to.as_deref().is_none_or(|t| t.trim().is_empty()) => {
            return Err(OpError::usage("move needs `to`: a folder name"));
        }
        "categorize" if a.categories.as_ref().is_none_or(|c| c.is_empty()) => {
            return Err(OpError::usage("categorize needs `categories`"));
        }
        _ => {}
    }
    if let Some(d) = &a.due {
        NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .map_err(|_| OpError::usage("due is a date: YYYY-MM-DD"))?;
    }
    if let Some(s) = a.always.as_deref()
        && !matches!(s, "sender" | "kind")
    {
        return Err(OpError::usage("always is sender or kind"));
    }
    Ok(a)
}

/// The messages some ids stand for (messages and groups), and the ids that
/// stand for nothing.
pub fn expand(
    user: &User,
    ids: &[String],
    except: &[String],
) -> OpResult<(Vec<Target>, Vec<String>)> {
    let mut targets: Vec<Target> = Vec::new();
    let mut seen: BTreeSet<Target> = BTreeSet::new();
    let mut unknown = Vec::new();
    for r in ids.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        let found: Vec<Target> = match user.store.get_doc::<Vec<Target>>("group", r)? {
            Some(group) if r.starts_with('g') => group,
            _ => match resolve_message(user, r) {
                Ok(t) => vec![(t.account, t.id)],
                Err(e) if e.kind == ErrorKind::NotFound => {
                    unknown.push(r.to_string());
                    continue;
                }
                Err(e) => return Err(e),
            },
        };
        for t in found {
            if seen.insert(t.clone()) {
                targets.push(t);
            }
        }
    }
    let leave: BTreeSet<Target> = except
        .iter()
        .filter_map(|r| resolve_message(user, r).ok())
        .map(|t| (t.account, t.id))
        .collect();
    targets.retain(|t| !leave.contains(t));
    Ok((targets, unknown))
}

fn unknown_note(unknown: &[String]) -> String {
    if unknown.is_empty() {
        String::new()
    } else {
        format!(
            " pidge does not know {}: list the mail again.",
            unknown.join(", ")
        )
    }
}

/// The messages an action names.
fn named(user: &User, a: &Action) -> OpResult<(Vec<Target>, Vec<String>)> {
    let ids = a.ids.as_deref().unwrap_or_default();
    let (targets, unknown) = expand(user, ids, a.except.as_deref().unwrap_or_default())?;
    if targets.is_empty() && !unknown.is_empty() {
        return Err(
            OpError::not_found(format!("no message {}", unknown.join(", ")))
                .with_hint("handles change when pidge is updated; list the mail again"),
        );
    }
    Ok((targets, unknown))
}

async fn several(user: &Arc<User>, actions: &[Action]) -> OpResult<Output> {
    // Everything is checked before anything is done.
    let mut work = Vec::new();
    for a in actions {
        let a = checked(a.clone())?;
        if a.ids.as_ref().is_none_or(|i| i.is_empty()) {
            return Err(OpError::usage(format!(
                "{}: which messages? give ids",
                a.action
            )));
        }
        let (targets, unknown) = named(user, &a)?;
        if !allowed(user, &a, targets.len(), true).await? {
            return Err(OpError::new(
                ErrorKind::GuardrailDenied,
                format!(
                    "the user's policy asks before this: nothing was done, {} must be confirmed",
                    a.action
                ),
            )
            .with_hint("send it as its own mail_act action=... ids=[...]; it returns a plan"));
        }
        work.push((a, targets, unknown));
    }
    let mut lines = Vec::new();
    let mut data = Vec::new();
    for (a, targets, unknown) in work {
        let done = carry_out(user, &a, &targets).await?;
        lines.push(format!("{}{}", said(&a, &done), unknown_note(&unknown)));
        data.push(json!({
            "action": a.action, "count": done.count, "failed": done.failed, "ids": done.ids,
        }));
    }
    Ok(Output::new(lines.join("\n"), json!({ "actions": data })))
}

/// Does the user's policy let this go ahead at once?
pub(crate) async fn allowed(user: &User, a: &Action, n: usize, named: bool) -> OpResult<bool> {
    let policy = user.prefs().await.policy;
    let mode = if a.action == "delete" && policy.delete != Mode::Allow {
        policy.delete
    } else if !named && n > policy.ask_over {
        policy.bulk
    } else {
        Mode::Allow
    };
    match mode {
        Mode::Allow => Ok(true),
        Mode::Confirm => Ok(false),
        Mode::Deny => Err(OpError::new(
            ErrorKind::GuardrailDenied,
            "the user's policy does not allow this change",
        )),
    }
}

async fn one(user: &Arc<User>, input: &ActInput) -> OpResult<Output> {
    let a = top_action(input)?;
    let by_ids = a.ids.as_ref().is_some_and(|v| !v.is_empty());
    let by_selector = !input.selector.is_empty();
    if by_ids && by_selector {
        return Err(OpError::usage("give ids or selector fields, not both"));
    }
    if by_ids {
        let (targets, unknown) = named(user, &a)?;
        if !allowed(user, &a, targets.len(), true).await? {
            let stored: Vec<Stored> = targets
                .iter()
                .filter_map(|(x, id)| user.store.get(x, id).ok().flatten())
                .collect();
            return make_plan(user, &a, &stored, "selected messages").await;
        }
        let done = carry_out(user, &a, &targets).await?;
        return Ok(Output::new(
            format!("{}{}", said(&a, &done), unknown_note(&unknown)),
            json!({ "action": a.action, "count": done.count, "failed": done.failed, "ids": done.ids }),
        ));
    }
    if !by_selector {
        return Err(OpError::usage(
            "which messages? give ids=[m12] or selector fields such as kind=promotion",
        ));
    }
    let providers = user.scope(input.selector.account.as_deref())?;
    let report = sync::ensure(user, &providers, sync::budget(user)).await;
    let filter = to_filter(user, &input.selector, Some(Folder::Inbox)).await?;
    let own = user.addresses();
    let found = post_filter(&input.selector, user.store.query(&filter)?, &own);
    // A description of a kind of mail is not a decision about mail that
    // matters.
    let by_kind = input.selector.kind.is_some() && input.selector.importance.is_none();
    let (matched, spared): (Vec<Stored>, Vec<Stored>) = found
        .into_iter()
        .partition(|s| !by_kind || (s.class.importance < 2 && !s.item.flagged));
    let (matched, _) = leave_out(user, matched, input.except.as_deref().unwrap_or_default());
    let description = input.selector.describe();
    if matched.is_empty() {
        let mut text = format!("No {description}.");
        for n in report.notes() {
            text.push_str(&format!(" {n}"));
        }
        return Ok(Output::new(text, json!({ "count": 0 })));
    }
    if !allowed(user, &a, matched.len(), false).await? {
        return make_plan(user, &a, &matched, &description).await;
    }
    let targets: Vec<Target> = matched
        .iter()
        .map(|s| (s.item.account.clone(), s.item.id.clone()))
        .collect();
    let done = carry_out(user, &a, &targets).await?;
    let mut text = said(&a, &done);
    if matched.len() > 1 {
        text.push_str(&format!(" From {}.", senders(&matched, 12)));
    }
    if !spared.is_empty() {
        text.push_str(&format!(
            " Left alone because they matter or are flagged: {}.",
            spared
                .iter()
                .take(10)
                .map(|s| s.handle.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Output::new(
        text,
        json!({ "action": a.action, "count": done.count, "failed": done.failed, "ids": done.ids }),
    ))
}

fn leave_out(user: &User, items: Vec<Stored>, except: &[String]) -> (Vec<Stored>, usize) {
    let leave: Vec<String> = except
        .iter()
        .filter_map(|r| resolve_message(user, r).ok())
        .map(|t| t.handle)
        .collect();
    let before = items.len();
    let kept: Vec<Stored> = items
        .into_iter()
        .filter(|s| !leave.contains(&s.handle))
        .collect();
    let left = before - kept.len();
    (kept, left)
}

/// Who some mail is from, most first.
pub fn senders(items: &[Stored], max: usize) -> String {
    let groups = crate::classify::count_by(items.iter(), |s| Some(render::name_of(&s.item.from)));
    let more = groups.len().saturating_sub(max);
    let mut names: Vec<String> = groups
        .into_iter()
        .take(max)
        .map(|(n, c)| if c > 1 { format!("{n} ({c})") } else { n })
        .collect();
    if more > 0 {
        names.push(format!("{more} more"));
    }
    names.join(", ")
}

/// What was done, in a sentence.
pub(crate) fn said(a: &Action, done: &Done) -> String {
    let what = match a.action.as_str() {
        "move" => format!("Moved to {}", render::inline(a.to.as_deref().unwrap_or(""))),
        "categorize" => format!(
            "Categorized as {}",
            render::inline(&a.categories.clone().unwrap_or_default().join(", "))
        ),
        other => verb(other).to_string(),
    };
    let n = done.count;
    let mut text = if n == 0 {
        "Nothing to do: none of those messages are left.".to_string()
    } else if done.failed > 0 {
        format!("{what} {} of {n}; {} failed", n - done.failed, done.failed)
    } else if n == 1 {
        what
    } else if matches!(a.action.as_str(), "move" | "categorize") {
        format!("{what}: {n}")
    } else {
        format!("{what} {n}")
    };
    if n > 0 {
        if !done.ids.is_empty() {
            text.push_str(&format!(" ({})", done.ids.join(", ")));
        }
        text.push('.');
    }
    if !done.extra.is_empty() {
        text.push(' ');
        text.push_str(&done.extra);
    }
    text
}

async fn apply_plan(user: &Arc<User>, input: &ActInput, p: &str) -> OpResult<Output> {
    let plan: MailPlan = user.store.get_doc("mailplan", p)?.ok_or_else(|| {
        OpError::not_found(format!("plan {p} has expired or was already applied"))
            .with_hint("make it again with the same mail_act call")
    })?;
    let mut targets = plan.targets.clone();
    let (more, _) = expand(user, input.include.as_deref().unwrap_or_default(), &[])?;
    for t in more {
        if !targets.contains(&t) {
            targets.push(t);
        }
    }
    let leave: Vec<Target> = input
        .except
        .iter()
        .flatten()
        .filter_map(|r| resolve_message(user, r).ok())
        .map(|t| (t.account, t.id))
        .collect();
    targets.retain(|t| !leave.contains(t));
    user.store.delete_doc("mailplan", p)?;
    let done = carry_out(user, &plan.action, &targets).await?;
    Ok(Output::new(
        said(&plan.action, &done),
        json!({ "action": plan.action.action, "count": done.count, "failed": done.failed, "ids": done.ids }),
    ))
}

async fn make_plan(
    user: &Arc<User>,
    a: &Action,
    matched: &[Stored],
    description: &str,
) -> OpResult<Output> {
    let tz = user.tz().await;
    let now = Utc::now();
    let pid = format!("{}-{:08x}", now.timestamp_micros(), rand::random::<u32>());
    let handle = user.store.handle(HandleKind::Plan, "", &pid)?;
    user.store.put_doc(
        "mailplan",
        &handle,
        &MailPlan {
            action: a.clone(),
            targets: matched
                .iter()
                .map(|s| (s.item.account.clone(), s.item.id.clone()))
                .collect(),
            created: now,
        },
        Some(now + chrono::Duration::minutes(PLAN_TTL_MINUTES)),
    )?;
    let what = match a.action.as_str() {
        "move" => format!("move to {}", render::inline(a.to.as_deref().unwrap_or(""))),
        other => other.to_string(),
    };
    let mut text = format!(
        "Plan {handle}: {what} {} {description}, from {}. For example:\n",
        matched.len(),
        senders(matched, 10)
    );
    for s in matched.iter().take(5) {
        text.push_str(&item_line(user, s, tz, now, false));
        text.push('\n');
    }
    text.push_str(&format!(
        "next: tell the user and ask; when they agree, mail_act plan={handle}"
    ));
    Ok(Output::new(
        text,
        json!({ "plan": handle, "action": a.action, "count": matched.len() }),
    ))
}

/// The changes an action means in one mailbox (a folder may have to be
/// found or made there first).
async fn changes_for(user: &User, a: &Action, account: &str) -> OpResult<Vec<MailChange>> {
    Ok(match a.action.as_str() {
        "archive" => vec![MailChange::Move {
            to: Folder::Archive,
        }],
        "delete" => vec![MailChange::Delete],
        "read" => vec![MailChange::MarkRead],
        "unread" => vec![MailChange::MarkUnread],
        "flag" => vec![MailChange::Flag {
            due: a
                .due
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        }],
        "unflag" => vec![MailChange::Unflag],
        "move" => {
            let to = a.to.as_deref().unwrap_or("").trim();
            let folder = match Folder::from_key(to) {
                Folder::Other(_) => user.provider(account)?.ensure_folder(to).await?,
                known => known,
            };
            vec![MailChange::Move { to: folder }]
        }
        "file" => {
            let to = match &user.prefs().await.receipts_folder {
                Some(path) => user.provider(account)?.ensure_folder(path).await?,
                None => Folder::Archive,
            };
            vec![MailChange::MarkRead, MailChange::Move { to }]
        }
        "categorize" => vec![MailChange::SetCategories {
            categories: a.categories.clone().unwrap_or_default(),
        }],
        _ => Vec::new(),
    })
}

/// Do one thing with some messages.
pub async fn carry_out(user: &Arc<User>, a: &Action, targets: &[Target]) -> OpResult<Done> {
    carry_out_as(user, a, targets, false).await
}

/// [`carry_out`] for a standing rule.
pub async fn carry_out_by_rule(user: &Arc<User>, a: &Action, targets: &[Target]) -> OpResult<Done> {
    carry_out_as(user, a, targets, true).await
}

async fn carry_out_as(
    user: &Arc<User>,
    a: &Action,
    targets: &[Target],
    by_rule: bool,
) -> OpResult<Done> {
    let mut done = Done {
        count: targets.len(),
        ..Default::default()
    };
    if targets.is_empty() {
        return Ok(done);
    }
    let stored: Vec<Stored> = targets
        .iter()
        .filter_map(|(x, id)| user.store.get(x, id).ok().flatten())
        .collect();
    // Before the change: what is said about always has to make sense.
    let rules = match a.always.as_deref() {
        Some(how) => rules_for(a, how, &stored)?,
        None => Vec::new(),
    };
    if a.action == "unsubscribe" {
        done.extra = unsubscribe(user, targets).await?;
        return Ok(done);
    }
    let mut by: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (x, id) in targets {
        by.entry(x).or_default().push(id.clone());
    }
    let mut job = Job {
        by_rule,
        always: rules.clone(),
        ..Default::default()
    };
    for (account, ids) in by {
        for change in changes_for(user, a, account).await? {
            job.mail.push(MailJob {
                account: account.to_string(),
                ids: ids.clone(),
                change,
            });
        }
    }
    let label = format!("{} {}", a.action, plural(targets.len(), "message"));
    let sent = queue::submit(user, &label, job).await?;
    done.ids.push(sent.id);
    done.failed = sent.failed.min(targets.len());
    if a.action == "categorize" {
        teach(user, a, &stored).await?;
    }
    if !rules.is_empty() {
        done.extra = remember(user, rules).await?;
    }
    Ok(done)
}

/// What the user calls this mail: a category (made if it is new) with these
/// messages as examples.
async fn teach(user: &Arc<User>, a: &Action, stored: &[Stored]) -> OpResult<()> {
    let Some(name) = a.categories.as_ref().and_then(|c| c.first()) else {
        return Ok(());
    };
    let name = render::cap_inline(&render::one_line(name), 40);
    if name.is_empty() || name.eq_ignore_ascii_case(OTHER) {
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    let examples: Vec<Example> = stored
        .iter()
        .filter(|s| seen.insert(s.item.from.address.clone()))
        .take(3)
        .map(|s| Example {
            from: render::cap_inline(&render::who(&s.item.from), 120),
            subject: render::cap_inline(&render::one_line(&s.item.subject), 160),
        })
        .collect();
    let by_sender = a.always.as_deref() == Some("sender");
    let senders: Vec<String> = seen.iter().map(|s| format!("sender:{s}")).collect();
    let canonical = user
        .update_prefs(|p| {
            let mut cats = p.categories();
            let n = match cats.iter().position(|c| c.name.eq_ignore_ascii_case(&name)) {
                Some(n) => n,
                None => {
                    cats.push(Category::new(&name, "", &[], false));
                    cats.len() - 1
                }
            };
            if by_sender {
                for c in cats.iter_mut() {
                    c.rules.retain(|r| !senders.contains(r));
                }
                cats[n].rules.extend(senders);
            }
            let c = &mut cats[n];
            for e in examples {
                if !c.examples.contains(&e) {
                    c.examples.push(e);
                }
            }
            let excess = c.examples.len().saturating_sub(20);
            c.examples.drain(..excess);
            let canonical = c.name.clone();
            p.categories = Some(cats);
            p.categories_confirmed = true;
            canonical
        })
        .await?;
    for s in stored {
        crate::classify::record_verdict(user, s, None, Some(&canonical))?;
    }
    Ok(())
}

const MAX_RULES: usize = 50;
const MAX_SENDERS: usize = 10;

/// The rules "always do this" means for these messages.
fn rules_for(a: &Action, how: &str, stored: &[Stored]) -> OpResult<Vec<Always>> {
    if a.action == "unsubscribe" || a.action == "flag" || a.action.starts_with("un") {
        return Err(OpError::usage(format!(
            "always does not go with {}",
            a.action
        )));
    }
    let rule = Always {
        action: a.action.clone(),
        to: a.to.clone(),
        categories: a.categories.clone().unwrap_or_default(),
        since: Some(Utc::now()),
        ..Default::default()
    };
    if how == "sender" {
        let from: BTreeSet<String> = stored
            .iter()
            .map(|s| s.item.from.address.to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if from.len() > MAX_SENDERS {
            return Err(OpError::usage(format!(
                "always=sender is for at most {MAX_SENDERS} senders at a time"
            )));
        }
        return Ok(from
            .into_iter()
            .map(|s| Always {
                sender: Some(format!("sender:{s}")),
                ..rule.clone()
            })
            .collect());
    }
    let kinds: BTreeSet<&str> = stored
        .iter()
        .filter_map(|s| s.class.kind.as_deref())
        .collect();
    let kind = match (kinds.len(), kinds.first().and_then(|k| Kind::parse(k))) {
        (1, Some(k)) if k.is_bulk() || k == Kind::Delivery => k,
        _ => {
            return Err(OpError::usage(
                "always=kind needs messages of one kind (newsletters, promotions, notifications, social or deliveries); use always=sender",
            ));
        }
    };
    let statuses: BTreeSet<&str> = stored
        .iter()
        .filter_map(|s| crate::delivery::status(&s.item).map(|x| x.as_str()))
        .collect();
    let status = match (kind, statuses.len()) {
        (Kind::Delivery, 1) => statuses.first().map(|s| s.to_string()),
        (Kind::Delivery, _) => {
            return Err(OpError::usage(
                "always=kind for deliveries needs parcels that are at the same stage (for example all delivered)",
            ));
        }
        _ => None,
    };
    Ok(vec![Always {
        kind: Some(kind.as_str().into()),
        status,
        ..rule
    }])
}

async fn remember(user: &Arc<User>, rules: Vec<Always>) -> OpResult<String> {
    let words: Vec<String> = rules.iter().map(crate::rules::describe).collect();
    user.update_prefs(|p| -> OpResult<()> {
        for r in rules {
            p.always
                .retain(|x| (&x.sender, &x.kind, &x.status) != (&r.sender, &r.kind, &r.status));
            p.always.push(r);
        }
        if p.always.len() > MAX_RULES {
            let n = p.always.len() - MAX_RULES;
            p.always.drain(..n);
        }
        Ok(())
    })
    .await??;
    Ok(format!("From now on: {}.", words.join("; ")))
}

/// Make one change in one mailbox, through the queue.
pub async fn change_now(
    user: &Arc<User>,
    account: &str,
    ids: &[String],
    change: MailChange,
    label: &str,
) {
    let job = Job {
        mail: vec![MailJob {
            account: account.to_string(),
            ids: ids.to_vec(),
            change,
        }],
        ..Default::default()
    };
    if let Err(e) = queue::submit(user, label, job).await {
        tracing::warn!(error = %e.message, "a change could not be queued");
    }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Check the hourly cap for `n` more outgoing messages.
pub async fn check_cap(user: &User, n: usize) -> OpResult<()> {
    let cap = user.prefs().await.policy.sends_per_hour;
    let sent = user
        .store
        .sends_since(Utc::now() - chrono::Duration::hours(1))?;
    if sent + n > cap {
        return Err(OpError::new(
            ErrorKind::GuardrailDenied,
            format!("that would exceed {cap} outgoing messages in an hour ({sent} so far)"),
        )
        .with_hint("wait a while, or raise policy sends_per_hour in settings"));
    }
    Ok(())
}

/// Unsubscribe: one-click POST where offered, else an e-mail to the list
/// address; a web-only link is handed to the user. Returns a summary.
async fn unsubscribe(user: &Arc<User>, targets: &[(String, String)]) -> OpResult<String> {
    let mut done = 0;
    let mut mailed = 0;
    let mut manual: Vec<String> = Vec::new();
    let mut none = 0;
    let mut failed = 0;
    for (account, id) in targets {
        let p = user.provider(account)?;
        let h = p.unsubscribe_headers(id).await?;
        let mut headers = Vec::new();
        if let Some(v) = h.list_unsubscribe {
            headers.push(("List-Unsubscribe".to_string(), v));
        }
        if let Some(v) = h.list_unsubscribe_post {
            headers.push(("List-Unsubscribe-Post".to_string(), v));
        }
        match parse_unsubscribe(&headers) {
            UnsubscribeMethod::OneClickPost(url) => {
                match user.host.one_click_unsubscribe(&url).await {
                    Ok(()) => done += 1,
                    Err(_) => failed += 1,
                }
            }
            UnsubscribeMethod::Mailto {
                address,
                subject,
                body,
            } => {
                check_cap(user, 1).await?;
                let spec = DraftSpec {
                    kind: DraftKind::New,
                    to: vec![MessageFrom {
                        name: String::new(),
                        address,
                    }],
                    cc: vec![],
                    bcc: vec![],
                    subject: Some(mailto_subject(subject.as_deref())),
                    body: body.unwrap_or_else(|| "unsubscribe".into()),
                };
                match p.create_draft(&spec).await {
                    Ok(d) => match p.send_draft(&d.id).await {
                        Ok(()) => {
                            user.store.record_sends(1)?;
                            mailed += 1;
                        }
                        Err(_) => failed += 1,
                    },
                    Err(_) => failed += 1,
                }
            }
            UnsubscribeMethod::HttpsOnly(url) => manual.push(url),
            UnsubscribeMethod::None => none += 1,
        }
    }
    let mut parts = Vec::new();
    if done + mailed > 0 {
        parts.push(format!(
            "{} unsubscribed{}",
            done + mailed,
            if mailed > 0 {
                format!(" ({mailed} by e-mail)")
            } else {
                String::new()
            }
        ));
    }
    if !manual.is_empty() {
        parts.push(format!(
            "{} need the user to open a page: {}",
            manual.len(),
            manual.join(" ")
        ));
    }
    if none > 0 {
        parts.push(format!("{none} offer no unsubscribe"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    Ok(parts.join("; "))
}
