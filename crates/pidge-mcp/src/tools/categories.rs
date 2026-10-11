//! `mail_categories`: the user's review categories, talked through with
//! the harness: list, confirm the defaults, add, rename, describe, remove,
//! reorder, and teach by rule (a sender or domain) or by example (a real
//! message, copied by id from the user's own mailbox). With mailbox sync
//! on, the categories are also written to the user's mailboxes
//! ([`crate::review::sync`]), and `sort_mail` tags recent mail.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, schemars, tool, tool_router};
use serde::{Deserialize, Serialize};

use super::PidgeMcp;
use super::mail_read::check_id;
use crate::context::{ToolContext, store_error, tool_error};
use crate::render::{one_line, untrusted, who};
use crate::review::categories::{
    self as cats, Category, Example, MAX_CATEGORIES, MAX_EXAMPLES, MAX_RULES, OTHER,
};
use crate::review::session::parse_short;
use crate::review::sync;
use crate::users::{UserRecord, user_hash};

/// The user record must stay well under Key Vault's 25 KB secret limit.
const MAX_RECORD_BYTES: usize = 14_000;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CategoriesAction {
    #[default]
    List,
    /// The user is happy with the categories in use.
    Confirm,
    /// Back to the default categories (drops the user's own).
    UseDefaults,
    /// A new category `name`, optionally with `description`, `rules`,
    /// `example_ids` and `position` in the same call.
    Add,
    /// Rename (`new_name`), describe (`description`) or toggle
    /// `bulk_suggestion` on `name`.
    Update,
    Remove,
    AddRule,
    RemoveRule,
    /// Add the message `id` as an example of `name`.
    AddExample,
    /// Remove example number `example` (as listed) from `name`.
    RemoveExample,
    /// Put the categories in `order` (names); unnamed ones keep their order after.
    Reorder,
    /// Writing the categories to the mailboxes: `enabled` turns it on or
    /// off (with `account`: for that mailbox only), `restore` brings back a
    /// category the user deleted in `account`; with neither, the status.
    MailboxSync,
    /// Classify the inbox mail (read and unread) of the last `days` days
    /// and put the categories on it in the mailboxes.
    SortMail,
    /// Leave the mailboxes with exactly pidge's categories, in the same
    /// colours everywhere: lists every other mailbox category (the user's
    /// own and the colour-only defaults included) with how many messages
    /// carry it. Only with `apply=true` are they deleted (`remove_from_mail`
    /// also takes them off the mail). Show the list and ask first.
    PruneMailboxes,
}

#[derive(Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CategoriesArgs {
    #[serde(default)]
    pub action: Option<CategoriesAction>,
    /// The category acted on.
    #[serde(default)]
    pub name: Option<String>,
    /// For `update`: the new name.
    #[serde(default)]
    pub new_name: Option<String>,
    /// For `add`/`update`: what belongs in the category, in the user's words.
    #[serde(default)]
    pub description: Option<String>,
    /// For `add_rule`/`remove_rule`: `sender:<address>` or `domain:<domain>`.
    #[serde(default)]
    pub rule: Option<String>,
    /// For `add`: rules for the new category, as for `add_rule`.
    #[serde(default)]
    pub rules: Option<Vec<String>>,
    /// For `add_example`: a message id (a review's short id such as `r3`,
    /// or an id from mail_overview).
    #[serde(default)]
    pub id: Option<String>,
    /// For `add`: message ids to copy as examples of the new category.
    #[serde(default)]
    pub example_ids: Option<Vec<String>>,
    /// The review session a short id (`r3`) belongs to; the latest when absent.
    #[serde(default)]
    pub session: Option<String>,
    /// For `remove_example`: the example's number as listed.
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub example: Option<usize>,
    /// For `add`/`update`: offer "archive all of these" at review start.
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub bulk_suggestion: Option<bool>,
    /// For `add`: where the new category goes in the list (1 = first);
    /// last when absent.
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub position: Option<usize>,
    /// For `reorder`: category names in the new order.
    #[serde(default)]
    pub order: Option<Vec<String>>,
    /// For `remove` with mailbox sync on: also take the category off the
    /// mail in the mailboxes. Ask the user.
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub remove_from_mail: Option<bool>,
    /// For `mailbox_sync`: on or off.
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub enabled: Option<bool>,
    /// For `mailbox_sync`: the one mailbox `enabled` or `restore` is about.
    /// For `sort_mail`: only this mailbox.
    #[serde(default)]
    pub account: Option<String>,
    /// For `mailbox_sync` with `account`: a category the user deleted in
    /// that mailbox, to write there again.
    #[serde(default)]
    pub restore: Option<String>,
    /// For `sort_mail`: how many days back, today included (1-31).
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub days: Option<u32>,
    /// For `prune_mailboxes`: really delete (after the user has seen the list).
    #[serde(default, deserialize_with = "crate::context::lenient")]
    pub apply: Option<bool>,
}

#[tool_router(router = categories_router, vis = "pub(crate)")]
impl PidgeMcp {
    #[tool(
        description = "E-mail: the categories mail is sorted into during a review (mail_review_start), in review order. Talk them through with the user: `confirm` keeps the ones in use, `add` (with optional description, rules, example_ids and position in one call)/`update`/`remove`/`reorder` edit them, `add_rule` pins a sender or domain to a category (e.g. \"everything from the climbing club is Leisure\" → rule domain:climbingclub.se), `add_example` teaches by a real message id (\"mail like this one\"; a review's short id such as r3 works), `use_defaults` goes back to the standard set. `Other` is built in for whatever fits nowhere. `mailbox_sync enabled=true` also puts the categories in the user's mailboxes (as the mailbox's own categories, and on the mail a review sorts); `prune_mailboxes` (only when the user asks to clean up the mailboxes' categories) lists, and with apply=true removes, every mailbox category that isn't pidge's so all mailboxes look the same; `sort_mail days=N` (1 = today, up to 31) classifies and tags the inbox mail of the last N days without a review, for \"classify today's mail\" (with the user's AI key it costs a few hundredths of a cent per 100 messages). Returns the updated list."
    )]
    pub(crate) async fn mail_categories(
        &self,
        Parameters(args): Parameters<CategoriesArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tc = ToolContext::from_request(&self.state, &ctx).await?;
        let action = args.action.unwrap_or_default();
        match action {
            CategoriesAction::List => return Ok(ok(render(&tc.record))),
            CategoriesAction::SortMail => {
                return self
                    .sort_mail(&tc, args.days.unwrap_or(1), args.account.as_deref())
                    .await
                    .map(ok);
            }
            CategoriesAction::MailboxSync => return self.mailbox_sync(&tc, &args).await.map(ok),
            CategoriesAction::PruneMailboxes => {
                return self.prune_mailboxes(&tc, &args).await.map(ok);
            }
            _ => {}
        }
        let syncing = tc.record.review.mailbox_sync.enabled;
        if action == CategoriesAction::Remove && syncing && args.remove_from_mail.is_none() {
            let name = args.name.as_deref().map(one_line).unwrap_or_default();
            return Ok(ok(format!(
                "Nothing changed yet. Ask the user whether {name} should also come off the mail in their mailboxes, then call again with remove_from_mail=true or remove_from_mail=false."
            )));
        }

        // Examples are read before the record is changed, so a missing
        // message changes nothing.
        let ids: Vec<String> = match action {
            CategoriesAction::AddExample => vec![
                args.id
                    .clone()
                    .ok_or_else(|| tool_error("add_example needs `id`"))?,
            ],
            CategoriesAction::Add => args.example_ids.clone().unwrap_or_default(),
            _ => Vec::new(),
        };
        if ids.len() > MAX_EXAMPLES {
            return Err(tool_error(format!(
                "At most {MAX_EXAMPLES} examples per category"
            )));
        }
        let mut examples = Vec::new();
        for id in &ids {
            let (accounts, full) = self.message_target(&tc, id, args.session.as_deref())?;
            let m = self.find_message(&accounts, &full).await?;
            examples.push(Example::new(&who(&m.from), &m.subject));
        }

        let signin = tc.record.signin.clone();
        let mut record = self
            .state
            .users
            .load(&signin)
            .await
            .map_err(|e| store_error("loading user record", &signin, e))?
            .unwrap_or_else(|| tc.record.clone());
        let mut list = record.review.categories();
        let before: Vec<String> = list.iter().map(|c| c.name.clone()).collect();
        let receipt = edit(&mut list, action, &args, examples)?;
        // An update keeps the category's place, so a rename shows there.
        let renamed = (action == CategoriesAction::Update)
            .then(|| {
                before
                    .iter()
                    .zip(&list)
                    .find(|(b, c)| **b != c.name)
                    .map(|(b, c)| (b.clone(), c.name.clone()))
            })
            .flatten();
        let removed: Vec<String> = if action == CategoriesAction::Remove {
            before
                .iter()
                .filter(|b| !list.iter().any(|c| &c.name == *b))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        record.review.categories = match action {
            CategoriesAction::UseDefaults => None,
            _ => Some(list),
        };
        record.review.confirmed = true;
        // Once, while sync is off: the categories can go on the mail too.
        let offer = !syncing && !record.review.mailbox_sync.offered;
        if offer {
            record.review.mailbox_sync.offered = true;
        }
        let size = serde_json::to_string(&record).map(|s| s.len()).unwrap_or(0);
        if size > MAX_RECORD_BYTES {
            return Err(tool_error(
                "That would make the categories too large to store; remove some examples or rules first",
            ));
        }
        self.state
            .users
            .save(&record)
            .await
            .map_err(|e| store_error("saving categories", &signin, e))?;
        self.state.cache.invalidate_user(&signin);
        tracing::info!(user = %user_hash(&signin), action = ?action, "categories updated");

        let mut out = format!("{receipt}\n\n{}", render(&record));
        if syncing {
            // The mailboxes follow in the background: a rename or removal
            // can retag hundreds of messages.
            let state = self.state.clone();
            let from_mail = args.remove_from_mail.unwrap_or(false);
            let user = signin.clone();
            tokio::spawn(async move {
                let lock = state.review.sync_locks.for_user(&user);
                let _guard = lock.lock().await;
                if let Some((old, new)) = &renamed {
                    sync::rename(&state, &user, old, new).await;
                }
                for name in &removed {
                    sync::remove(&state, &user, name, from_mail).await;
                }
                sync::sync(&state, &user, true, None).await;
            });
            out.push_str("\nThe mailboxes are being updated in the background.");
        } else if offer {
            out.push_str(
                "\nnote (mention once): pidge can also put these categories on the mail in the user's mailboxes, so they show in their mail app; if they want that: mail_categories action=mailbox_sync enabled=true.",
            );
        }
        Ok(ok(out))
    }
}

impl PidgeMcp {
    /// The mailboxes to look in and the full id for a message id from the
    /// harness: a review's short id (`r3`), or a full id in any of the
    /// user's mailboxes.
    fn message_target(
        &self,
        tc: &ToolContext,
        id: &str,
        session: Option<&str>,
    ) -> Result<(Vec<String>, String), McpError> {
        check_id(id)?;
        if parse_short(id).is_none() {
            return Ok((tc.accounts(None)?, id.to_string()));
        }
        let sessions = &self.state.review.sessions;
        let s = match session {
            Some(s) => sessions.get(&tc.user.email, s.trim()),
            None => sessions.latest(&tc.user.email),
        };
        let (account, full) = s
            .and_then(|s| s.index_of(id).and_then(|i| s.message_ref(i)))
            .ok_or_else(|| {
                tool_error(format!(
                    "{id} is not in a running review; pass the message's full id"
                ))
            })?;
        Ok((tc.accounts(Some(&account))?, full))
    }

    /// `mail_categories action=mailbox_sync`.
    async fn prune_mailboxes(
        &self,
        tc: &ToolContext,
        args: &CategoriesArgs,
    ) -> Result<String, McpError> {
        if !tc.record.review.mailbox_sync.enabled {
            return Ok(
                "Mailbox categories are off, so pidge doesn't manage the mailboxes' categories. Turn them on first (mail_categories action=mailbox_sync enabled=true)."
                    .into(),
            );
        }
        let signin = tc.record.signin.clone();
        let apply = args.apply == Some(true);
        let from_mail = args.remove_from_mail == Some(true);
        let lock = self.state.review.sync_locks.for_user(&signin);
        let report = {
            let _guard = lock.lock().await;
            // Link pidge's own first, so none of them is taken for a stranger.
            sync::sync(&self.state, &signin, true, None).await;
            sync::prune(&self.state, &signin, apply, from_mail).await
        };
        let mut out = String::new();
        if report.pruned.is_empty() {
            out.push_str("The mailboxes have only pidge's categories.");
        } else {
            out.push_str(if apply {
                "Removed from the mailboxes:"
            } else {
                "These mailbox categories are not pidge's and would be removed:"
            });
            let mut mailboxes: Vec<&str> =
                report.pruned.iter().map(|p| p.mailbox.as_str()).collect();
            mailboxes.dedup();
            for mb in mailboxes {
                let names: Vec<String> = report
                    .pruned
                    .iter()
                    .filter(|p| p.mailbox == mb)
                    .map(|p| {
                        let n = if p.messages >= sync::RETAG_CAP {
                            format!("{}+", sync::RETAG_CAP)
                        } else {
                            p.messages.to_string()
                        };
                        format!("{} ({n} messages)", one_line(&p.name))
                    })
                    .collect();
                out.push_str(&format!("\n- {mb}: {}", names.join(", ")));
            }
            if apply {
                out.push_str(if from_mail {
                    "\nTheir names were also taken off the mail."
                } else {
                    "\nMail that carried them keeps the name, without a colour."
                });
            } else {
                out.push_str(
                    "\nNothing changed yet. Show the user this list; if they agree, call again with apply=true, and remove_from_mail=true to also take these names off the mail that carries them.",
                );
            }
        }
        if report.recoloured > 0 {
            out.push_str(&format!(
                "\n{} of pidge's categories were given the same colour as in the other mailboxes.",
                report.recoloured
            ));
        }
        if !report.skipped.is_empty() {
            out.push_str(&format!(
                "\nCould not read the categories of: {} (accounts_connect once may be needed).",
                report.skipped.join(", ")
            ));
        }
        if report.failed > 0 {
            out.push_str(&format!(
                "\n{} changes failed; try again later.",
                report.failed
            ));
        }
        Ok(out)
    }

    async fn mailbox_sync(
        &self,
        tc: &ToolContext,
        args: &CategoriesArgs,
    ) -> Result<String, McpError> {
        let signin = tc.record.signin.clone();
        let account = args
            .account
            .as_deref()
            .map(|a| tc.accounts(Some(a)).map(|mut v| v.remove(0)))
            .transpose()?;
        let restore = args
            .restore
            .as_deref()
            .map(|n| {
                let account = account
                    .clone()
                    .ok_or_else(|| tool_error("restore needs `account`"))?;
                let list = tc.record.review.categories();
                let i = cats::position(&list, n)
                    .ok_or_else(|| tool_error(format!("No category named {}", one_line(n))))?;
                Ok::<_, McpError>((account, list[i].name.clone()))
            })
            .transpose()?;
        let was_on = tc.record.review.mailbox_sync.enabled;
        let mut record = self
            .state
            .users
            .load(&signin)
            .await
            .map_err(|e| store_error("loading user record", &signin, e))?
            .unwrap_or_else(|| tc.record.clone());
        let ms = &mut record.review.mailbox_sync;
        let mut receipt = String::new();
        match (args.enabled, &account) {
            (Some(on), Some(a)) => {
                ms.excluded.retain(|m| !m.eq_ignore_ascii_case(a));
                if !on {
                    ms.excluded.push(a.clone());
                }
                receipt = format!("{a}: {}.", if on { "included" } else { "left out" });
            }
            (Some(on), None) => {
                ms.enabled = on;
                ms.offered = true;
                receipt = format!(
                    "Mailbox categories are {}.",
                    if on {
                        "on"
                    } else {
                        "off (the mailboxes keep what is there)"
                    }
                );
            }
            (None, _) => {}
        }
        if let Some((a, name)) = &restore {
            if let Some(st) = ms.mailboxes.get_mut(a) {
                st.removed.retain(|r| r != name);
            }
            receipt = format!("{name} will be written to {a} again.");
        }
        let changed = args.enabled.is_some() || restore.is_some();
        if changed {
            self.state
                .users
                .save(&record)
                .await
                .map_err(|e| store_error("saving mailbox sync", &signin, e))?;
            self.state.cache.invalidate_user(&signin);
            tracing::info!(user = %user_hash(&signin), "mailbox sync changed");
        }

        let mut out = receipt;
        if record.review.mailbox_sync.enabled {
            let lock = self.state.review.sync_locks.for_user(&signin);
            let report = {
                let _guard = lock.lock().await;
                sync::sync(&self.state, &signin, changed, None).await
            };
            if report.created + report.linked > 0 {
                out.push_str(&format!(
                    "\nIn the mailboxes: {} created, {} of the user's own linked by name.",
                    report.created, report.linked
                ));
            }
            // First time on: the user's own named categories, to adopt.
            if !was_on && !report.own.is_empty() {
                let own: Vec<String> = report
                    .own
                    .iter()
                    .map(|(m, n)| format!("{} ({m})", one_line(n)))
                    .collect();
                out.push_str(&format!(
                    "\nThe user's mailboxes have their own categories: {}. pidge leaves them alone; offer to adopt any as a pidge category (mail_categories action=add name=<name>), so reviews sort into it.",
                    own.join(", ")
                ));
            }
            for note in sync::take_notes(&self.state, &signin).await {
                out.push('\n');
                out.push_str(&note);
            }
            let rec = self
                .state
                .users
                .load(&signin)
                .await
                .map_err(|e| store_error("loading user record", &signin, e))?
                .unwrap_or(record);
            out.push('\n');
            out.push_str(&sync_status(&rec));
        } else {
            out.push('\n');
            out.push_str(&sync_status(&record));
        }
        Ok(out.trim_start().to_string())
    }
}

/// The mailbox-sync state, one line per mailbox.
fn sync_status(record: &UserRecord) -> String {
    let ms = &record.review.mailbox_sync;
    if !ms.enabled {
        return "Mailbox categories: off (turn on with mail_categories action=mailbox_sync enabled=true).".into();
    }
    let mut out = String::from("Mailbox categories: on");
    for m in &record.mailboxes {
        let state = if !ms.active(m) {
            "left out".to_string()
        } else if let Some(note) = ms.account_note(m) {
            note.to_string()
        } else {
            match ms.mailboxes.get(m) {
                Some(st) if !st.removed.is_empty() => {
                    format!("ok; removed there by the user: {}", st.removed.join(", "))
                }
                Some(st) if st.synced_at.is_some() => "ok".to_string(),
                _ => "not synced yet".to_string(),
            }
        };
        out.push_str(&format!("\n  - {m}: {state}"));
    }
    out
}

/// Applies one edit to `list`; returns a one-line receipt.
fn edit(
    list: &mut Vec<Category>,
    action: CategoriesAction,
    args: &CategoriesArgs,
    examples: Vec<Example>,
) -> Result<String, McpError> {
    let name = || {
        args.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| tool_error("this action needs `name`"))
    };
    let find = |list: &[Category], n: &str| {
        cats::position(list, n).ok_or_else(|| {
            tool_error(format!(
                "No category named {}; the list shows the current ones",
                one_line(n)
            ))
        })
    };
    let description = args
        .description
        .as_deref()
        .map(cats::clean_description)
        .transpose()
        .map_err(tool_error)?;
    Ok(match action {
        CategoriesAction::List
        | CategoriesAction::MailboxSync
        | CategoriesAction::SortMail
        | CategoriesAction::PruneMailboxes => String::new(),
        CategoriesAction::Confirm => "Confirmed.".into(),
        CategoriesAction::UseDefaults => "Back to the default categories.".into(),
        CategoriesAction::Add => {
            let n = cats::clean_name(name()?).map_err(tool_error)?;
            if cats::position(list, &n).is_some() {
                return Err(tool_error(format!(
                    "{n} already exists; use update to change it"
                )));
            }
            if list.len() >= MAX_CATEGORIES {
                return Err(tool_error(format!(
                    "At most {MAX_CATEGORIES} categories; remove one first"
                )));
            }
            let rules = args
                .rules
                .iter()
                .flatten()
                .map(|r| cats::parse_rule(r))
                .collect::<Result<Vec<_>, _>>()
                .map_err(tool_error)?;
            if rules.len() > MAX_RULES {
                return Err(tool_error(format!(
                    "At most {MAX_RULES} rules per category"
                )));
            }
            // A sender or domain belongs to one category only.
            for c in list.iter_mut() {
                c.rules.retain(|r| !rules.contains(r));
            }
            let mut c: Category = serde_json::from_value(serde_json::json!({ "name": n }))
                .expect("a name-only category deserializes");
            c.description = description.unwrap_or_default();
            c.bulk_suggestion = args.bulk_suggestion.unwrap_or(false);
            c.rules = rules;
            c.examples = examples;
            let at = args
                .position
                .map_or(list.len(), |p| p.saturating_sub(1).min(list.len()));
            let mut receipt = format!("Added {n}");
            if !c.rules.is_empty() || !c.examples.is_empty() {
                receipt.push_str(&format!(
                    " with {} and {}",
                    count(c.rules.len(), "rule"),
                    count(c.examples.len(), "example")
                ));
            }
            list.insert(at, c);
            receipt.push('.');
            receipt
        }
        CategoriesAction::Update => {
            let i = find(list, name()?)?;
            if let Some(new) = args.new_name.as_deref() {
                let new = cats::clean_name(new).map_err(tool_error)?;
                if cats::position(list, &new).is_some_and(|j| j != i) {
                    return Err(tool_error(format!("{new} already exists")));
                }
                list[i].name = new;
            }
            if let Some(d) = description {
                list[i].description = d;
            }
            if let Some(b) = args.bulk_suggestion {
                list[i].bulk_suggestion = b;
            }
            format!("Updated {}.", list[i].name)
        }
        CategoriesAction::Remove => {
            let i = find(list, name()?)?;
            let removed = list.remove(i);
            format!(
                "Removed {}; its mail now goes to {OTHER} or another category.",
                removed.name
            )
        }
        CategoriesAction::AddRule | CategoriesAction::RemoveRule => {
            let i = find(list, name()?)?;
            let rule =
                cats::parse_rule(args.rule.as_deref().unwrap_or_default()).map_err(tool_error)?;
            if action == CategoriesAction::RemoveRule {
                list[i].rules.retain(|r| *r != rule);
                format!("Removed the rule {rule} from {}.", list[i].name)
            } else {
                // A sender or domain belongs to one category only.
                for c in list.iter_mut() {
                    c.rules.retain(|r| *r != rule);
                }
                if list[i].rules.len() >= MAX_RULES {
                    return Err(tool_error(format!(
                        "At most {MAX_RULES} rules per category; remove one first"
                    )));
                }
                list[i].rules.push(rule.clone());
                format!("Mail matching {rule} now goes to {}.", list[i].name)
            }
        }
        CategoriesAction::AddExample => {
            let i = find(list, name()?)?;
            let c = &mut list[i];
            for example in examples {
                if c.examples.len() >= MAX_EXAMPLES {
                    c.examples.remove(0);
                }
                c.examples.push(example);
            }
            format!("Added the example to {}.", c.name)
        }
        CategoriesAction::RemoveExample => {
            let i = find(list, name()?)?;
            let k = args
                .example
                .ok_or_else(|| tool_error("remove_example needs `example`"))?;
            if k == 0 || k > list[i].examples.len() {
                return Err(tool_error(format!("{} has no example {k}", list[i].name)));
            }
            list[i].examples.remove(k - 1);
            format!("Removed example {k} from {}.", list[i].name)
        }
        CategoriesAction::Reorder => {
            let order = args
                .order
                .as_ref()
                .ok_or_else(|| tool_error("reorder needs `order`"))?;
            let mut sorted = Vec::new();
            for n in order {
                let i = find(list, n)?;
                sorted.push(list.remove(i));
            }
            sorted.append(list);
            *list = sorted;
            "Reordered.".into()
        }
    })
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn render(record: &UserRecord) -> String {
    let list = record.review.categories();
    let state = match (&record.review.categories, record.review.confirmed) {
        (None, false) => " (the defaults; the user has not confirmed them yet)",
        (None, true) => " (the defaults)",
        _ => "",
    };
    let mut out = format!("Categories{state}:");
    for (n, c) in list.iter().enumerate() {
        out.push_str(&format!("\n{}. {}", n + 1, c.name));
        if !c.description.is_empty() {
            out.push_str(&format!(": {}", c.description));
        }
        if c.bulk_suggestion {
            out.push_str(" [offered for bulk clean-up]");
        }
        if !c.rules.is_empty() {
            out.push_str(&format!("\n   rules: {}", c.rules.join(", ")));
        }
        if !c.examples.is_empty() {
            out.push_str(&format!("\n   examples: {}", c.examples.len()));
        }
    }
    out.push_str(&format!(
        "\n{OTHER}: whatever fits no other category (built in)"
    ));
    if record.review.mailbox_sync.enabled {
        out.push_str(
            "\nAlso written to the mailboxes (mail_categories action=mailbox_sync for details).",
        );
    }
    // Examples are copied from received mail: third-party text.
    let examples: Vec<String> = list
        .iter()
        .flat_map(|c| {
            c.examples.iter().enumerate().map(move |(k, e)| {
                format!(
                    "{} example {}: {} / {}",
                    c.name,
                    k + 1,
                    one_line(&e.from),
                    one_line(&e.subject)
                )
            })
        })
        .collect();
    if !examples.is_empty() {
        out.push_str("\n\nExamples:\n");
        out.push_str(&untrusted(&examples.join("\n")));
    }
    out
}

fn ok(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::{ToolHarness, text};

    async fn call(h: &ToolHarness, args: CategoriesArgs) -> Result<String, McpError> {
        h.mcp
            .mail_categories(Parameters(args), h.ctx())
            .await
            .map(|r| text(&r))
    }

    fn args(action: CategoriesAction) -> CategoriesArgs {
        CategoriesArgs {
            action: Some(action),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_defaults_are_listed_until_confirmed() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        let out = call(&h, CategoriesArgs::default()).await.unwrap();
        assert!(out.contains("has not confirmed them yet"), "{out}");
        assert!(out.contains("1. Important"), "{out}");
        assert!(out.contains("Other: whatever fits"), "{out}");
        call(&h, args(CategoriesAction::Confirm)).await.unwrap();
        let rec = h.record().await;
        assert!(rec.review.confirmed);
        assert!(rec.review.categories.is_some());
    }

    #[tokio::test]
    async fn add_rule_and_rename_flow() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        let mut a = args(CategoriesAction::Add);
        a.name = Some("Leisure".into());
        a.description = Some("Climbing, photography and travel".into());
        call(&h, a).await.unwrap();

        let mut r = args(CategoriesAction::AddRule);
        r.name = Some("leisure".into());
        r.rule = Some("@climbingclub.se".into());
        let out = call(&h, r).await.unwrap();
        assert!(
            out.contains("domain:climbingclub.se now goes to Leisure"),
            "{out}"
        );

        // The same rule on another category moves it there.
        let mut r = args(CategoriesAction::AddRule);
        r.name = Some("Personal".into());
        r.rule = Some("domain:climbingclub.se".into());
        call(&h, r).await.unwrap();
        let cats = h.record().await.review.categories.unwrap();
        let leisure = cats.iter().find(|c| c.name == "Leisure").unwrap();
        assert!(leisure.rules.is_empty());

        let mut u = args(CategoriesAction::Update);
        u.name = Some("Leisure".into());
        u.new_name = Some("Other".into());
        assert!(call(&h, u).await.is_err(), "Other is reserved");
    }

    #[tokio::test]
    async fn an_example_is_copied_from_the_users_own_message() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        let h = ToolHarness::new(&["jane@example.com"]).await;
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages/AAMk1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "AAMk1",
                "subject": "Training on Tuesday",
                "from": { "emailAddress": { "name": "Climbing Club", "address": "info@climbingclub.se" } },
                "toRecipients": [], "ccRecipients": [], "bccRecipients": [],
                "receivedDateTime": "2026-10-11T06:00:00Z",
                "sentDateTime": "2026-10-11T06:00:00Z",
                "isRead": false, "hasAttachments": false,
                "body": { "contentType": "text", "content": "Hi" }
            })))
            .mount(&h.graph)
            .await;
        let mut e = args(CategoriesAction::AddExample);
        e.name = Some("Personal".into());
        e.id = Some("AAMk1".into());
        let out = call(&h, e).await.unwrap();
        assert!(
            out.contains(
                "Personal example 1: Climbing Club <info@climbingclub.se> / Training on Tuesday"
            ),
            "{out}"
        );

        let mut bad = args(CategoriesAction::AddExample);
        bad.name = Some("Personal".into());
        bad.id = Some("../x".into());
        assert!(call(&h, bad).await.is_err());
    }
}
