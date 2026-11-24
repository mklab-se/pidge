//! `settings`: the user's mailboxes, categories, standing rules, VIPs, muted
//! senders, policy and preferences.

use std::sync::Arc;

use chrono::Utc;
use pidge_core::provider::{Folder, MailChange, ProviderKind};
use pidge_store::Filter;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::ctx::User;
use crate::error::{OpError, OpResult};
use crate::ops::resolve_message;
use crate::prefs::{Category, Example, Mode, OTHER, Prefs, SendMode, default_categories};
use crate::registry::{Meta, OpDef, Output, op};
use crate::render;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SettingsInput {
    /// `show` (default: mailboxes, categories, rules, policy).
    /// Mailboxes: `connect` (with `provider`), `disconnect` (with `email`),
    /// `ai` / `ai_off` (the user's own AI key), `sign_out_everywhere`.
    /// Categories: `add`, `update`, `remove`, `reorder`, `add_rule`,
    /// `remove_rule`, `add_example`, `confirm`, `use_defaults`,
    /// `label_mail` (write them onto recent mail in the mailboxes).
    /// Senders: `vip` / `unvip`, `mute` / `unmute`.
    /// Standing rules ("always archive ..."): `forget` (with `rule`: its
    /// number). `policy` and `set` with `key` and `value`.
    pub action: Option<String>,
    /// For `connect`: `microsoft` (default; Outlook.com and Microsoft 365)
    /// or `google` (Gmail).
    pub provider: Option<String>,
    /// For `connect`: the address to connect (a hint); for `disconnect`:
    /// the mailbox.
    pub email: Option<String>,
    /// The category acted on.
    pub name: Option<String>,
    /// For `add`/`update`: what belongs there, in the user's words.
    pub description: Option<String>,
    /// For `update`: a new name.
    pub new_name: Option<String>,
    /// For `add`/`add_rule`/`remove_rule`: `sender:<address>` or
    /// `domain:<domain>` (a bare address or domain works too). For `forget`:
    /// the number of the standing rule.
    pub rule: Option<String>,
    /// For `add_example`: a message that belongs in the category (m12).
    pub example: Option<String>,
    /// For `add`/`update`: offer the category as one decision in the inbox.
    pub sweep: Option<bool>,
    /// For `reorder`: category names in the new order.
    pub order: Option<Vec<String>>,
    /// For `vip`/`unvip`/`mute`/`unmute`: an address or `domain:x`.
    pub who: Option<String>,
    /// For `policy`: `send` (hold, confirm, allow, deny), `notify`,
    /// `bulk`, `delete` (allow, confirm, deny), `ask_over`,
    /// `sends_per_hour`. For `set`: `timezone`, `default_sender`,
    /// `receipts_folder`, `window_days`, `working_hours` (e.g. 8-17).
    pub key: Option<String>,
    pub value: Option<String>,
    /// For `label_mail`: days back (default 7, max 31).
    pub days: Option<u32>,
}

pub fn settings() -> OpDef {
    op(
        Meta {
            name: "settings",
            cli: &["settings"],
            summary: "Mailboxes, categories, standing rules, VIPs, policy and settings",
            description: "How pidge is set up, for when the user asks to see or change that; never needed to \
read or handle mail. Mailboxes: show, connect provider=microsoft|google (give the user the link it \
returns), disconnect, ai (their own AI key). Categories and what belongs in them (add, add_rule \
rule=domain:klatterklubben.se, ...; in daily use mail_act categorize teaches pidge). VIPs always \
matter (action=vip who=anna@x.se), muted senders never do. Standing rules made with always in \
mail_act: shown here, forget removes one. Policy: how sending, notifications to others, changes to \
very many messages and deletes are allowed. Settings: timezone, default_sender, receipts_folder, \
window_days, working_hours.",
            positional: &["action"],
            read_only: false,
        },
        run_settings,
    )
}

fn normalize_rule(r: &str) -> OpResult<String> {
    let r = r.trim().to_ascii_lowercase();
    let out = if r.starts_with("sender:") || r.starts_with("domain:") {
        r
    } else if r.contains('@') {
        format!("sender:{r}")
    } else if r.contains('.') {
        format!("domain:{}", r.trim_start_matches('@'))
    } else {
        return Err(OpError::usage(
            "a rule is sender:<address> or domain:<domain>",
        ));
    };
    if out.len() > 200 || out.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(OpError::usage("that rule is not an address or domain"));
    }
    Ok(out)
}

fn clean_name(n: &str) -> OpResult<String> {
    let n = render::one_line(n);
    if n.is_empty() || n.chars().count() > 40 {
        return Err(OpError::usage("a category name is 1-40 characters"));
    }
    if n.eq_ignore_ascii_case(OTHER) {
        return Err(OpError::usage(
            "Other is built in: it holds whatever fits nowhere else",
        ));
    }
    Ok(n)
}

fn find<'a>(cats: &'a mut [Category], name: &str) -> OpResult<&'a mut Category> {
    let names: Vec<String> = cats.iter().map(|c| c.name.clone()).collect();
    cats.iter_mut()
        .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| {
            OpError::not_found(format!(
                "no category {name:?} (there are: {})",
                names.join(", ")
            ))
        })
}

fn show(p: &Prefs) -> String {
    let cats = p.categories();
    let mut text = format!(
        "Categories{}:\n",
        if p.categories.is_none() && !p.categories_confirmed {
            " (pidge's standard set; not confirmed by the user)"
        } else {
            ""
        }
    );
    for (n, c) in cats.iter().enumerate() {
        let mut facts = Vec::new();
        if !c.rules.is_empty() {
            facts.push(format!("rules: {}", c.rules.join(", ")));
        }
        if !c.examples.is_empty() {
            facts.push(format!("{} examples", c.examples.len()));
        }
        if c.sweep {
            facts.push("one decision in the inbox".into());
        }
        let facts = if facts.is_empty() {
            String::new()
        } else {
            format!(" [{}]", facts.join("; "))
        };
        text.push_str(&format!(
            "{}. {}: {}{facts}\n",
            n + 1,
            c.name,
            render::inline(&c.description)
        ));
    }
    text.push_str(&format!(
        "{}. Other: whatever fits nowhere else\n",
        cats.len() + 1
    ));
    if !p.vips.is_empty() {
        text.push_str(&format!("VIPs: {}\n", p.vips.join(", ")));
    }
    if !p.muted.is_empty() {
        text.push_str(&format!("Muted: {}\n", p.muted.join(", ")));
    }
    if !p.always.is_empty() {
        text.push_str("Standing rules:\n");
        for (n, rule) in p.always.iter().enumerate() {
            text.push_str(&format!("{}. {}\n", n + 1, crate::rules::describe(rule)));
        }
    }
    let pol = &p.policy;
    text.push_str(&format!(
        "Policy: send {:?}, notify {:?}, bulk {:?} (over {}), delete {:?}, at most {} outgoing per hour\n",
        pol.send, pol.notify, pol.bulk, pol.ask_over, pol.delete, pol.sends_per_hour
    ).to_lowercase());
    text.push_str(&format!(
        "Settings: timezone {}, default sender {}, receipts filed to {}, {} days indexed\n",
        p.timezone.as_deref().unwrap_or("(system)"),
        p.default_sender.as_deref().unwrap_or("(first mailbox)"),
        p.receipts_folder.as_deref().unwrap_or("Archive"),
        p.window_days()
    ));
    if !p.ai.status.is_empty() && p.ai.status != "off" {
        text.push_str(&format!("Your AI: {}\n", p.ai.status));
    }
    text
}

/// The mailboxes and their health.
async fn accounts(user: &Arc<User>) -> OpResult<String> {
    let prefs = user.prefs().await;
    let tz = user.tz().await;
    let list = user.host.accounts().await;
    if list.is_empty() {
        return Ok(
                    "No mailbox is connected yet.\nnext: settings action=connect provider=microsoft (or google)\n"
                        .into(),
                );
    }
    let mut text = String::from("Mailboxes:\n");
    for a in &list {
        let st = user.store.sync_state(&a.address)?;
        let synced = match (st.complete, st.synced_at) {
            (true, Some(t)) => format!("synced {} ago", crate::render::age(t, Utc::now())),
            (false, _) if st.cursor.is_some() => "loading".into(),
            _ => "not synced yet".into(),
        };
        let default = prefs.default_sender.as_deref() == Some(&a.address)
            || (prefs.default_sender.is_none()
                && Some(&a.address) == list.first().map(|x| &x.address));
        text.push_str(&format!(
            "- {} ({}): {}, {synced}{}\n",
            a.address,
            match a.provider {
                ProviderKind::Microsoft => "Microsoft",
                ProviderKind::Google => "Gmail",
            },
            a.status,
            if default { ", default sender" } else { "" }
        ));
    }
    text.push_str(&format!("Timezone: {tz}\n"));
    let ai = if user.ai().is_some() {
        format!(
            "Your AI: {}",
            if prefs.ai.status.is_empty() {
                "ok"
            } else {
                &prefs.ai.status
            }
        )
    } else {
        "Your AI: off (optional; settings action=ai)".into()
    };
    text.push_str(&ai);
    text.push('\n');
    Ok(text)
}

/// The actions on mailboxes and the AI key.
async fn mailboxes(
    user: &Arc<User>,
    action: &str,
    input: &SettingsInput,
) -> OpResult<Option<Output>> {
    match action {
        "connect" => {
            let provider = match input
                .provider
                .as_deref()
                .unwrap_or("microsoft")
                .to_ascii_lowercase()
                .as_str()
            {
                "microsoft" | "outlook" | "m365" | "office365" | "hotmail" => {
                    ProviderKind::Microsoft
                }
                "google" | "gmail" => ProviderKind::Google,
                other => {
                    return Err(OpError::usage(format!(
                        "provider is microsoft or google, not {other:?}"
                    )));
                }
            };
            let text = user.host.connect(provider, input.email.as_deref()).await?;
            Ok(Some(Output::text(text)))
        }
        "disconnect" => {
            let email = input
                .email
                .clone()
                .ok_or_else(|| OpError::usage("disconnect needs email"))?;
            let p = user.provider(&email)?;
            let address = p.address().to_string();
            let text = user.host.disconnect(&address).await?;
            user.store.forget_account(&address)?;
            Ok(Some(Output::text(text)))
        }
        "ai" => Ok(Some(Output::text(user.host.ai_setup().await?))),
        "ai_off" => {
            let text = user.host.remove_ai_key().await?;
            user.set_ai(None);
            Ok(Some(Output::text(text)))
        }
        "sign_out_everywhere" => Ok(Some(Output::text(user.host.sign_out_everywhere().await?))),
        _ => Ok(None),
    }
}

fn mode(v: &str) -> OpResult<Mode> {
    match v.trim().to_ascii_lowercase().as_str() {
        "allow" => Ok(Mode::Allow),
        "confirm" => Ok(Mode::Confirm),
        "deny" => Ok(Mode::Deny),
        other => Err(OpError::usage(format!(
            "{other:?}: use allow, confirm or deny"
        ))),
    }
}

async fn run_settings(user: Arc<User>, input: SettingsInput) -> OpResult<Output> {
    let action = input
        .action
        .as_deref()
        .unwrap_or("show")
        .trim()
        .to_ascii_lowercase();
    if let Some(out) = mailboxes(&user, &action, &input).await? {
        return Ok(out);
    }
    let need = |v: &Option<String>, what: &str| {
        v.clone()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| OpError::usage(format!("{action} needs `{what}`")))
    };
    let mut resort = false;
    let said: String = match action.as_str() {
        "show" | "list" => String::new(),
        "confirm" => {
            user.update_prefs(|p| {
                if p.categories.is_none() {
                    p.categories = Some(default_categories());
                }
                p.categories_confirmed = true;
            })
            .await?;
            "Confirmed.".into()
        }
        "use_defaults" => {
            resort = true;
            user.update_prefs(|p| {
                p.categories = Some(default_categories());
                p.categories_confirmed = true;
            })
            .await?;
            "Back to the standard categories.".into()
        }
        "add" => {
            let name = clean_name(&need(&input.name, "name")?)?;
            let rule = input.rule.as_deref().map(normalize_rule).transpose()?;
            resort = true;
            user.update_prefs(|p| -> OpResult<()> {
                let mut cats = p.categories();
                if cats.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
                    return Err(OpError::usage(format!("{name} already exists")));
                }
                let mut c = Category::new(
                    &name,
                    input.description.as_deref().unwrap_or(""),
                    &[],
                    input.sweep.unwrap_or(false),
                );
                c.description = render::cap_inline(&render::one_line(&c.description), 300);
                if let Some(r) = rule {
                    c.rules.push(r);
                }
                cats.push(c);
                p.categories = Some(cats);
                p.categories_confirmed = true;
                Ok(())
            })
            .await??;
            format!("Added {name}.")
        }
        "update" => {
            let name = need(&input.name, "name")?;
            let new_name = input.new_name.as_deref().map(clean_name).transpose()?;
            resort = true;
            user.update_prefs(|p| -> OpResult<()> {
                let mut cats = p.categories();
                let c = find(&mut cats, &name)?;
                if let Some(n) = new_name {
                    c.name = n;
                }
                if let Some(d) = &input.description {
                    c.description = render::cap_inline(&render::one_line(d), 300);
                }
                if let Some(s) = input.sweep {
                    c.sweep = s;
                }
                p.categories = Some(cats);
                Ok(())
            })
            .await??;
            "Updated.".into()
        }
        "remove" => {
            let name = need(&input.name, "name")?;
            resort = true;
            user.update_prefs(|p| -> OpResult<()> {
                let mut cats = p.categories();
                find(&mut cats, &name)?;
                cats.retain(|c| !c.name.eq_ignore_ascii_case(name.trim()));
                p.categories = Some(cats);
                Ok(())
            })
            .await??;
            format!("Removed {name}; its mail goes to Other or another category.")
        }
        "reorder" => {
            let order = input
                .order
                .clone()
                .ok_or_else(|| OpError::usage("reorder needs `order`"))?;
            user.update_prefs(|p| {
                let mut cats = p.categories();
                cats.sort_by_key(|c| {
                    order
                        .iter()
                        .position(|o| o.eq_ignore_ascii_case(&c.name))
                        .unwrap_or(usize::MAX)
                });
                p.categories = Some(cats);
            })
            .await?;
            "Reordered.".into()
        }
        "add_rule" | "remove_rule" => {
            let name = need(&input.name, "name")?;
            let rule = normalize_rule(&need(&input.rule, "rule")?)?;
            resort = true;
            let add = action == "add_rule";
            user.update_prefs(|p| -> OpResult<()> {
                let mut cats = p.categories();
                // A sender belongs to one category: remove it elsewhere.
                if add {
                    for c in cats.iter_mut() {
                        c.rules.retain(|r| r != &rule);
                    }
                }
                let c = find(&mut cats, &name)?;
                if add {
                    c.rules.push(rule.clone());
                } else {
                    c.rules.retain(|r| r != &rule);
                }
                p.categories = Some(cats);
                p.categories_confirmed = true;
                Ok(())
            })
            .await??;
            if add {
                format!("From now on {rule} goes to {name}.")
            } else {
                "Rule removed.".into()
            }
        }
        "add_example" => {
            let name = need(&input.name, "name")?;
            let t = resolve_message(&user, &need(&input.example, "example")?)?;
            let s = user
                .store
                .get(&t.account, &t.id)?
                .ok_or_else(|| OpError::not_found("that message is not in the index"))?;
            resort = true;
            let ex = Example {
                from: render::cap_inline(&render::who(&s.item.from), 120),
                subject: render::cap_inline(&render::one_line(&s.item.subject), 160),
            };
            let cat_name = user
                .update_prefs(|p| -> OpResult<String> {
                    let mut cats = p.categories();
                    let c = find(&mut cats, &name)?;
                    if c.examples.len() >= 20 {
                        c.examples.remove(0);
                    }
                    c.examples.push(ex);
                    let n = c.name.clone();
                    p.categories = Some(cats);
                    p.categories_confirmed = true;
                    Ok(n)
                })
                .await??;
            crate::classify::record_verdict(&user, &s, None, Some(&cat_name))?;
            format!("Noted: mail like {} is {cat_name}.", t.handle)
        }
        "vip" | "unvip" | "mute" | "unmute" => {
            let who = need(&input.who, "who")?.trim().to_ascii_lowercase();
            if !who.contains('@') && !who.starts_with("domain:") {
                return Err(OpError::usage("who is an address or domain:<domain>"));
            }
            resort = true;
            user.update_prefs(|p| {
                let list = if action.ends_with("vip") {
                    &mut p.vips
                } else {
                    &mut p.muted
                };
                list.retain(|x| x != &who);
                if !action.starts_with("un") {
                    list.push(who.clone());
                }
            })
            .await?;
            "OK.".into()
        }
        "forget" => {
            let n: usize = need(&input.rule, "rule")?
                .trim()
                .parse()
                .map_err(|_| OpError::usage("rule is the number of the standing rule"))?;
            let gone = user
                .update_prefs(|p| {
                    (1..=p.always.len())
                        .contains(&n)
                        .then(|| p.always.remove(n - 1))
                })
                .await?
                .ok_or_else(|| OpError::not_found(format!("no standing rule {n}")))?;
            format!("Forgotten: {}.", crate::rules::describe(&gone))
        }
        "policy" => {
            let key = need(&input.key, "key")?;
            let value = need(&input.value, "value")?;
            user.update_prefs(|p| -> OpResult<()> {
                let pol = &mut p.policy;
                match key.as_str() {
                    "send" => {
                        pol.send = match value.as_str() {
                            "hold" => SendMode::Hold,
                            "confirm" => SendMode::Confirm,
                            // Sending without a hold or approval is not
                            // something an agent can switch on for itself:
                            // the user edits prefs.json for that.
                            "allow" => {
                                return Err(OpError::usage(
                                    "send=allow (no hold, no approval) can only be set by the user in prefs.json; hold is the default",
                                ));
                            }
                            "deny" => SendMode::Deny,
                            _ => return Err(OpError::usage("send is hold, confirm, allow or deny")),
                        }
                    }
                    "notify" => {
                        pol.notify = match mode(&value)? {
                            // The same goes for e-mailing others without a plan.
                            Mode::Allow => {
                                return Err(OpError::usage(
                                    "notify=allow (no plan before others are e-mailed) can only be set by the user in prefs.json; confirm is the default",
                                ));
                            }
                            m => m,
                        }
                    }
                    "bulk" => pol.bulk = mode(&value)?,
                    "delete" => pol.delete = mode(&value)?,
                    "ask_over" => pol.ask_over = value.parse().map_err(|_| OpError::usage("a number"))?,
                    "sends_per_hour" => {
                        pol.sends_per_hour = value
                            .parse::<usize>()
                            .map_err(|_| OpError::usage("a number"))?
                            .clamp(1, 60)
                    }
                    _ => return Err(OpError::usage("policy keys: send, notify, bulk, delete, ask_over, sends_per_hour")),
                }
                Ok(())
            })
            .await??;
            "Policy updated.".into()
        }
        "set" => {
            let key = need(&input.key, "key")?;
            let value = input.value.clone().unwrap_or_default();
            let value = value.trim().to_string();
            match key.as_str() {
                "timezone" => {
                    value
                        .parse::<chrono_tz::Tz>()
                        .map_err(|_| OpError::usage("an IANA zone such as Europe/Stockholm"))?;
                }
                "default_sender" => {
                    user.provider(&value)?;
                }
                "window_days" => {
                    value
                        .parse::<u32>()
                        .map_err(|_| OpError::usage("a number of days"))?;
                }
                _ => {}
            }
            user.update_prefs(|p| -> OpResult<()> {
                let v = (!value.is_empty()).then(|| value.clone());
                match key.as_str() {
                    "timezone" => p.timezone = v,
                    "default_sender" => p.default_sender = v,
                    "receipts_folder" => p.receipts_folder = v,
                    "window_days" => p.window_days = v.and_then(|v| v.parse().ok()),
                    "working_hours" => {
                        let (a, b) = value
                            .split_once('-')
                            .ok_or_else(|| OpError::usage("working_hours like 8-17"))?;
                        let a: u32 = a.trim().parse().map_err(|_| OpError::usage("working_hours like 8-17"))?;
                        let b: u32 = b.trim().parse().map_err(|_| OpError::usage("working_hours like 8-17"))?;
                        if a >= b || b > 24 {
                            return Err(OpError::usage("working_hours like 8-17"));
                        }
                        p.working_hours = Some([a, b]);
                    }
                    _ => return Err(OpError::usage("settings: timezone, default_sender, receipts_folder, window_days, working_hours")),
                }
                Ok(())
            })
            .await??;
            "Saved.".into()
        }
        "label_mail" => {
            let days = i64::from(input.days.unwrap_or(7).clamp(1, 31));
            let items = user.store.query(&Filter {
                folder: Some(Folder::Inbox),
                since: Some(chrono::Utc::now() - chrono::Duration::days(days)),
                ..Default::default()
            })?;
            let mut n = 0;
            let mut by: std::collections::BTreeMap<(String, Vec<String>), Vec<String>> =
                Default::default();
            for s in items {
                let Some(cat) = s.class.category.clone() else {
                    continue;
                };
                if s.item.categories.iter().any(|c| c == &cat) {
                    continue;
                }
                let mut cats = s.item.categories.clone();
                cats.push(cat);
                by.entry((s.item.account.clone(), cats))
                    .or_default()
                    .push(s.item.id.clone());
                n += 1;
            }
            for ((account, cats), ids) in by {
                if let Ok(p) = user.provider(&account) {
                    for c in &cats {
                        let _ = p.ensure_category(c).await;
                    }
                }
                crate::ops::mail_act::change_now(
                    &user,
                    &account,
                    &ids,
                    MailChange::SetCategories { categories: cats },
                    "label",
                )
                .await;
            }
            format!("Labelling {n} messages in the mailboxes.")
        }
        other => {
            return Err(OpError::usage(format!("unknown action {other:?}")));
        }
    };
    if resort {
        user.store.reset_classifications()?;
    }
    let p = user.prefs().await;
    let text = if said.is_empty() {
        format!("{}\n{}", accounts(&user).await?, show(&p))
    } else {
        format!("{said}\n{}", show(&p))
    };
    Ok(Output::new(
        text,
        json!({
            "categories": p.categories(),
            "confirmed": p.categories.is_some() || p.categories_confirmed,
            "vips": p.vips,
            "muted": p.muted,
            "policy": p.policy,
            "always": p.always,
            "accounts": user.host.accounts().await.iter().map(|a| json!({ "address": a.address, "provider": a.provider.as_str(), "status": a.status })).collect::<Vec<_>>(),
        }),
    ))
}
