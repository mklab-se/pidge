//! `pidge mail search <query>` — full-text search using Graph's `$search` KQL.
//!
//! Results come back in Graph's relevance order (not date), and search is
//! across all mail folders, not just Inbox — which is generally what users
//! mean by "search e-mail".

use anyhow::{Result, anyhow};
use colored::Colorize;
use futures::future::join_all;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message, MessageCache, short_hash};

pub async fn run(
    query: String,
    account_filter: Vec<String>,
    limit: usize,
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

    // Search each account in parallel; Graph caps each query at its own limit,
    // so we let every account return up to `limit` results, then trim the union.
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        let q = query.clone();
        async move {
            let result = graph.search_messages(&e, &q, limit).await;
            (e, result)
        }
    });

    let mut had_success = false;
    let mut messages: Vec<Message> = Vec::new();
    for (email, result) in join_all(futures).await {
        match result {
            Ok(msgs) => {
                had_success = true;
                messages.extend(msgs);
            }
            Err(ClientError::SessionExpired { email: e }) => {
                eprintln!(
                    "{} {e}: session expired, run `pidge account add`",
                    "WARNING:".yellow().bold()
                );
            }
            Err(e) => eprintln!("{} {email}: {e}", "WARNING:".yellow().bold()),
        }
    }

    if !had_success {
        return Err(anyhow!("All searched accounts failed."));
    }

    // Graph search returns by relevance, but trim to `limit` total across
    // accounts so the table doesn't blow up on multi-account searches.
    messages.truncate(limit);

    // Cache the IDs so the user can `pidge mail show <fragment>` straight from
    // the search results.
    {
        let mut cache = MessageCache::load()?;
        let pairs: Vec<(String, String)> = messages
            .iter()
            .map(|m| (m.id.clone(), m.account.clone()))
            .collect();
        cache.insert_many(&pairs);
        cache.save()?;
    }

    if json {
        let rows: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": short_hash(&m.id),
                    "graph_id": m.id,
                    "account": m.account,
                    "from": m.from,
                    "subject": m.subject,
                    "received_at": m.received_at,
                    "is_read": m.is_read,
                    "preview": m.preview,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    print_results(&messages, target_emails.len() == 1, compact)
}

fn print_results(messages: &[Message], hide_account: bool, compact: bool) -> Result<()> {
    use comfy_table::{ContentArrangement, Table};

    if messages.is_empty() {
        println!("No matches.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for m in messages {
        let from = if m.from.name.is_empty() {
            m.from.address.clone()
        } else {
            m.from.name.clone()
        };
        let flag = crate::commands::mail::flag_marker(m.flag_status);
        let subject_cell = if compact || m.preview.is_empty() {
            format!("{flag}{}", style_subject(&m.subject, m.is_read))
        } else {
            format!(
                "{flag}{}\n{}",
                style_subject(&m.subject, m.is_read),
                crate::output::linkify_text(&m.preview).dimmed()
            )
        };
        let mut cells = vec![
            short_hash(&m.id).dimmed().to_string(),
            m.account.clone(),
            from,
            subject_cell,
        ];
        if hide_account {
            cells.remove(1);
        }
        table.add_row(cells);
    }

    println!("{table}");
    Ok(())
}

fn style_subject(subject: &str, is_read: bool) -> String {
    let linked = crate::output::linkify_text(subject);
    if is_read {
        linked.cyan().to_string()
    } else {
        linked.bold().magenta().to_string()
    }
}
