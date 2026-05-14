//! `pidge inbox list` — list messages merged across signed-in accounts.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use futures::future::join_all;
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message, MessageCache, short_hash};

use crate::cli::InboxCommands;
use crate::output::linkify_text;

/// Pair of message and its computed short hash, for rendering.
struct MessageRow {
    message: Message,
    short_hash: String,
}

pub async fn run(command: InboxCommands, json: bool) -> Result<()> {
    match command {
        InboxCommands::List {
            account,
            limit,
            unread,
            compact,
        } => list(account, limit, unread, compact, json).await,
        InboxCommands::Show {
            fragment,
            mark_read,
            show_images,
            raw_html,
        } => {
            crate::commands::inbox_show::run(fragment, mark_read, show_images, raw_html, json).await
        }
    }
}

async fn list(
    account_filter: Vec<String>,
    limit: usize,
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
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        async move {
            let result = graph.list_inbox(&e, per_account, unread_only).await;
            (e, result)
        }
    });

    let results = join_all(futures).await;

    let mut all_messages: Vec<Message> = Vec::new();
    let mut had_success = false;
    for (email, result) in results {
        match result {
            Ok(mut msgs) => {
                had_success = true;
                all_messages.append(&mut msgs);
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

    if json {
        return render_json(&rows);
    }
    if compact {
        render_text_compact(&rows, single_account)
    } else {
        render_text_rich(&rows, single_account)
    }
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

fn render_text_rich(rows: &[MessageRow], hide_account: bool) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for row in rows {
        let subject_cell = {
            let styled_subject = style_subject(&row.message.subject, row.message.is_read);
            let preview_linkified = linkify_text(&row.message.preview);
            let preview_styled = preview_linkified.dimmed().to_string();
            if row.message.preview.is_empty() {
                styled_subject
            } else {
                format!("{styled_subject}\n{preview_styled}")
            }
        };

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            row.message.account.clone(),
            from_display(&row.message.from).to_string(),
            subject_cell,
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

fn render_text_compact(rows: &[MessageRow], hide_account: bool) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for row in rows {
        let subject = style_subject(&row.message.subject, row.message.is_read);

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            row.message.account.clone(),
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
