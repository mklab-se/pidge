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

use crate::commands::mail::{MessageRow, account_labels, render};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    query: String,
    account_filter: Vec<String>,
    limit: usize,
    compact: bool,
    table: bool,
    full: bool,
    json: bool,
    cursor: Option<String>,
) -> Result<()> {
    // Search continuation is plain nextLink paging — same flow as mail list.
    if let Some(token) = cursor {
        let cursor = pidge_client::Cursor::decode(&token, "mail")?;
        return super::mail::list_at_cursor(cursor, compact, table, full, json).await;
    }
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
    let mut next_links: std::collections::BTreeMap<String, Option<String>> =
        std::collections::BTreeMap::new();
    for (email, result) in join_all(futures).await {
        match result {
            Ok(page) => {
                had_success = true;
                next_links.insert(email.clone(), page.next_link);
                messages.extend(page.messages);
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

    // Graph search returns by relevance; trim to `limit` total across accounts
    // so a wide multi-account search doesn't flood the terminal.
    messages.sort_by_key(|m| std::cmp::Reverse(m.received_at));
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

    if messages.is_empty() && !json {
        println!("No matches.");
        return Ok(());
    }

    let rows: Vec<MessageRow> = messages
        .into_iter()
        .map(|m| {
            let h = short_hash(&m.id);
            MessageRow {
                message: m,
                short_hash: h,
            }
        })
        .collect();

    let single_account = target_emails.len() == 1;
    let labels = account_labels(&target_emails);
    if json {
        let mut next_cursor = pidge_client::Cursor::new("mail");
        for (email, link) in next_links {
            next_cursor.per_account.insert(email, link);
        }
        if !next_cursor.exhausted() {
            return super::mail::render_json_with_cursor(&rows, Some(next_cursor.encode()));
        }
    }
    render(&rows, single_account, &labels, compact, table, full, json)
}
