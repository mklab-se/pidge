//! `pidge inbox list` — list messages merged across signed-in accounts.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Local, Utc};
use colored::Colorize;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};
use futures::future::join_all;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message};

use crate::cli::{InboxCommands, OutputFormat};

pub async fn run(command: InboxCommands) -> Result<()> {
    match command {
        InboxCommands::List {
            account,
            limit,
            unread,
            output,
        } => list(account, limit, unread, output).await,
    }
}

async fn list(
    account_filter: Vec<String>,
    limit: usize,
    unread_only: bool,
    output: OutputFormat,
) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge auth login` to add one."
        ));
    }

    // Resolve which accounts to query
    let target_emails: Vec<String> = if account_filter.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        // Validate filter — every requested email must be signed in
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
                    "{} {e}: session expired, run `pidge auth login`",
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

    // Sort by received_at desc, slice to limit
    all_messages.sort_by_key(|b| std::cmp::Reverse(b.received_at));
    all_messages.truncate(limit);

    let single_account = target_emails.len() == 1;

    match output {
        OutputFormat::Text => render_text(&all_messages, single_account),
        OutputFormat::Json => render_json(&all_messages),
    }
}

fn compute_per_account_fetch(limit: usize, num_accounts: usize) -> usize {
    if num_accounts == 0 {
        return limit;
    }
    let calc = (limit as f64 * 1.2 / num_accounts as f64).ceil() as usize;
    calc.max(10)
}

fn render_text(messages: &[Message], hide_account_column: bool) -> Result<()> {
    let mut table = Table::new();
    // Unread-marker cell is in its own narrow column so ANSI styling on the
    // bullet doesn't throw off column-width math.
    let mut header: Vec<Cell> = vec![
        Cell::new(""),
        Cell::new("ACCOUNT"),
        Cell::new("FROM"),
        Cell::new("SUBJECT"),
        Cell::new("RECEIVED"),
    ];
    if hide_account_column {
        header.remove(1);
    }
    table
        .set_header(header)
        .set_content_arrangement(ContentArrangement::Dynamic);

    for m in messages {
        let marker = if !m.is_read {
            Cell::new("●")
                .fg(Color::Magenta)
                .add_attribute(Attribute::Dim)
        } else {
            Cell::new("")
        };
        let from_name: &str = if m.from.name.is_empty() {
            &m.from.address
        } else {
            &m.from.name
        };

        let mut row = vec![
            marker,
            Cell::new(&m.account),
            Cell::new(from_name),
            Cell::new(&m.subject),
            Cell::new(relative_received(m.received_at)),
        ];
        if hide_account_column {
            row.remove(1);
        }
        table.add_row(row);
    }

    println!("{table}");
    Ok(())
}

fn render_json(messages: &[Message]) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(messages)?);
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
