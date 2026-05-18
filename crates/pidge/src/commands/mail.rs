//! `pidge mail list` — list messages merged across signed-in accounts.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use futures::future::join_all;
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message, MessageCache, short_hash};

use crate::cli::MailCommands;
use crate::output::linkify_text;

/// Pair of message and its computed short hash, for rendering.
struct MessageRow {
    message: Message,
    short_hash: String,
}

pub async fn run(command: MailCommands, json: bool) -> Result<()> {
    match command {
        MailCommands::List {
            account,
            limit,
            page,
            unread,
            compact,
        } => list(account, limit, page, unread, compact, json).await,
        MailCommands::Show {
            fragment,
            mark_read,
            show_images,
            raw_html,
        } => {
            crate::commands::mail_show::run(fragment, mark_read, show_images, raw_html, json).await
        }
        MailCommands::Search {
            query,
            account,
            limit,
            compact,
        } => crate::commands::mail_search::run(query, account, limit, compact, json).await,
        MailCommands::MarkRead { fragment } => {
            crate::commands::mail_actions::mark_read(fragment).await
        }
        MailCommands::MarkUnread { fragment } => {
            crate::commands::mail_actions::mark_unread(fragment).await
        }
        MailCommands::Flag { fragment } => crate::commands::mail_actions::flag(fragment).await,
        MailCommands::Unflag { fragment } => crate::commands::mail_actions::unflag(fragment).await,
        MailCommands::Archive { fragment } => {
            crate::commands::mail_actions::archive(fragment).await
        }
        MailCommands::New(args) => crate::commands::mail_compose::send(args).await,
        MailCommands::Reply { fragment, compose } => {
            crate::commands::mail_compose::reply(fragment, compose, false).await
        }
        MailCommands::ReplyAll { fragment, compose } => {
            crate::commands::mail_compose::reply(fragment, compose, true).await
        }
        MailCommands::Forward { fragment, compose } => {
            crate::commands::mail_compose::forward(fragment, compose).await
        }
        MailCommands::Delete {
            fragment,
            older_than,
            account,
            yes,
        } => crate::commands::mail_delete::run(fragment, older_than, account, yes).await,
    }
}

async fn list(
    account_filter: Vec<String>,
    limit: usize,
    page: usize,
    unread_only: bool,
    compact: bool,
    json: bool,
) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }

    let target_emails: Vec<String> = if account_filter.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        for f in &account_filter {
            if config.find(f).is_none() {
                return Err(anyhow!("not signed in to {f}"));
            }
        }
        account_filter
    };

    let per_account = compute_per_account_fetch(limit, target_emails.len());
    // 1-based page → 0-based skip. `compute_per_account_skip` keeps the merge
    // tidy: every account gets the same skip so received-time interleaving stays
    // intact across pages (yes, multi-account paging is imperfect — see comment
    // on the helper — but it's the right approximation given Graph's per-mailbox
    // ordering).
    let skip = compute_per_account_skip(per_account, page);
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        async move {
            let result = graph.list_inbox(&e, per_account, skip, unread_only).await;
            (e, result)
        }
    });

    let results = join_all(futures).await;

    let mut all_messages: Vec<Message> = Vec::new();
    let mut had_success = false;
    for (email, result) in results {
        match result {
            Ok(page) => {
                had_success = true;
                all_messages.extend(page.messages);
            }
            Err(ClientError::SessionExpired { email: e }) => {
                eprintln!(
                    "{} {e}: session expired, run `pidge account add`",
                    "WARNING:".yellow().bold()
                );
            }
            Err(e) => {
                eprintln!("{} {email}: {e}", "WARNING:".yellow().bold());
            }
        }
    }

    if !had_success {
        return Err(anyhow!("All accounts failed."));
    }

    all_messages.sort_by_key(|b| std::cmp::Reverse(b.received_at));
    all_messages.truncate(limit);

    let rows: Vec<MessageRow> = all_messages
        .into_iter()
        .map(|m| {
            let h = short_hash(&m.id);
            MessageRow {
                message: m,
                short_hash: h,
            }
        })
        .collect();

    update_cache(&rows)?;

    let single_account = target_emails.len() == 1;
    let labels = account_labels(&target_emails);

    if json {
        return render_json(&rows);
    }
    if compact {
        render_text_compact(&rows, single_account, &labels)
    } else {
        render_text_rich(&rows, single_account, &labels)
    }
}

/// Map each signed-in e-mail to its display label for list views. If a
/// domain has only one account signed in, we shorten its label to just the
/// domain (`kristofer@mklab.se` → `mklab.se`) since the local-part is
/// redundant given the inbox is yours. If two accounts share a domain we
/// keep both as full addresses to preserve disambiguation.
fn account_labels(accounts: &[String]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut domain_count: HashMap<&str, usize> = HashMap::new();
    for email in accounts {
        if let Some((_, domain)) = email.split_once('@') {
            *domain_count.entry(domain).or_insert(0) += 1;
        }
    }
    accounts
        .iter()
        .map(|email| {
            let label = email
                .split_once('@')
                .and_then(|(_, d)| {
                    if domain_count.get(d) == Some(&1) {
                        Some(d.to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| email.clone());
            (email.clone(), label)
        })
        .collect()
}

fn update_cache(rows: &[MessageRow]) -> Result<()> {
    let mut cache = MessageCache::load()?;
    let pairs: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r.message.id.clone(), r.message.account.clone()))
        .collect();
    cache.insert_many(&pairs);
    cache.save()?;
    Ok(())
}

fn compute_per_account_fetch(limit: usize, num_accounts: usize) -> usize {
    if num_accounts == 0 {
        return limit;
    }
    let calc = (limit as f64 * 1.2 / num_accounts as f64).ceil() as usize;
    calc.max(10)
}

/// Compute the per-account `$skip` for a given page. Each account is paged
/// independently and the merged result is trimmed to `per_account * accounts`,
/// so page boundaries are approximate when accounts have very different
/// volumes — page 2 across two accounts may include some items from one
/// account that also appeared on page 1 of the other, sorted by received
/// time. This matches users' mental model of "I see the next batch" without
/// requiring a cross-account cursor we can't actually maintain (Graph has no
/// federated paging).
fn compute_per_account_skip(per_account: usize, page: usize) -> usize {
    page.saturating_sub(1) * per_account
}

fn from_display(from: &pidge_core::MessageFrom) -> &str {
    if from.name.is_empty() {
        &from.address
    } else {
        &from.name
    }
}

fn style_subject(subject: &str, is_read: bool) -> String {
    let linked = linkify_text(subject);
    if is_read {
        linked.cyan().to_string()
    } else {
        linked.bold().magenta().to_string()
    }
}

/// Visual prefix for a message's flag state. Empty string when not flagged
/// so unflagged rows stay aligned with the rest. Yellow `⚑` for an active
/// follow-up flag; green `✓` for a completed one — matches Outlook's
/// distinction between "flagged" and "complete".
pub fn flag_marker(status: pidge_core::FlagStatus) -> String {
    match status {
        pidge_core::FlagStatus::Flagged => format!("{} ", "⚑".yellow().bold()),
        pidge_core::FlagStatus::Complete => format!("{} ", "✓".green()),
        pidge_core::FlagStatus::NotFlagged => String::new(),
    }
}

/// Card-style multi-line rendering. Each message gets three lines and a
/// blank separator:
///
///   `<id> · <account> · <from> · <received>`     ← dimmed meta header
///   `<flag> <subject>`                            ← bold magenta when unread
///   `<preview>`                                   ← dimmed body excerpt
///
/// Uses the full terminal width, no truncation. Avoids the narrow-subject
/// column problem of the previous table-based rich layout.
fn render_text_rich(
    rows: &[MessageRow],
    hide_account: bool,
    labels: &std::collections::HashMap<String, String>,
) -> Result<()> {
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let account_label = labels
            .get(&row.message.account)
            .cloned()
            .unwrap_or_else(|| row.message.account.clone());
        let mut header_parts: Vec<String> = Vec::with_capacity(4);
        header_parts.push(row.short_hash.dimmed().to_string());
        if !hide_account {
            header_parts.push(account_label.dimmed().to_string());
        }
        header_parts.push(from_display(&row.message.from).bold().to_string());
        header_parts.push(
            relative_received(row.message.received_at)
                .dimmed()
                .to_string(),
        );
        println!("{}", header_parts.join(&" · ".dimmed().to_string()));

        let flag = flag_marker(row.message.flag_status);
        println!(
            "{flag}{}",
            style_subject(&row.message.subject, row.message.is_read)
        );

        if !row.message.preview.is_empty() {
            let preview = linkify_text(&row.message.preview);
            // Preview is usually 200 chars (Graph's bodyPreview). Wrap it
            // to terminal width minus 2 cols of breathing room. Multi-line
            // previews get rendered as-is — Graph already trims them.
            println!("{}", preview.dimmed());
        }
    }
    Ok(())
}

fn render_text_compact(
    rows: &[MessageRow],
    hide_account: bool,
    labels: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for row in rows {
        let subject = format!(
            "{}{}",
            flag_marker(row.message.flag_status),
            style_subject(&row.message.subject, row.message.is_read)
        );

        let account_label = labels
            .get(&row.message.account)
            .cloned()
            .unwrap_or_else(|| row.message.account.clone());

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            account_label,
            from_display(&row.message.from).to_string(),
            subject,
            relative_received(row.message.received_at),
        ];
        if hide_account {
            cells.remove(1);
        }
        table.add_row(cells);
    }

    println!("{table}");
    Ok(())
}

#[derive(Serialize)]
struct MessageOut<'a> {
    id: &'a str,
    graph_id: &'a str,
    account: &'a str,
    from: &'a pidge_core::MessageFrom,
    subject: &'a str,
    received_at: chrono::DateTime<chrono::Utc>,
    is_read: bool,
    preview: &'a str,
}

fn render_json(rows: &[MessageRow]) -> Result<()> {
    let out: Vec<MessageOut<'_>> = rows
        .iter()
        .map(|r| MessageOut {
            id: &r.short_hash,
            graph_id: &r.message.id,
            account: &r.message.account,
            from: &r.message.from,
            subject: &r.message.subject,
            received_at: r.message.received_at,
            is_read: r.message.is_read,
            preview: &r.message.preview,
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn relative_received(then: DateTime<Utc>) -> String {
    let now = Local::now();
    let then_local: DateTime<Local> = then.with_timezone(&Local);
    let delta = now - then_local;

    if delta.num_seconds() < 60 {
        return "just now".to_string();
    }
    if delta.num_minutes() < 60 {
        return format!("{}m ago", delta.num_minutes());
    }
    if delta.num_hours() < 24 {
        return format!("{}h ago", delta.num_hours());
    }
    if now.date_naive().pred_opt() == Some(then_local.date_naive()) {
        return "yesterday".to_string();
    }
    if delta.num_days() < 7 {
        return then_local.format("%a").to_string();
    }
    if now.year() == then_local.year() {
        return then_local.format("%b %-d").to_string();
    }
    then_local.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_account_fetch_at_least_10() {
        assert_eq!(compute_per_account_fetch(5, 1), 10);
    }

    #[test]
    fn per_account_fetch_scales_with_limit_and_accounts() {
        // ceil(25 * 1.2 / 3) = ceil(10) = 10 → 10 (because max(10) wins)
        assert_eq!(compute_per_account_fetch(25, 3), 10);
        // ceil(100 * 1.2 / 3) = ceil(40) = 40
        assert_eq!(compute_per_account_fetch(100, 3), 40);
    }
}
