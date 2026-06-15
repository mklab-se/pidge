//! `pidge mail move` — move a message (or a bulk selection) into a folder,
//! creating the folder on first use. Single mode takes a fragment; bulk mode
//! mirrors `mail archive`/`mail delete` (`--from` / `--older-than`, gated on
//! `-y`). The destination folder is resolved case-insensitively against the
//! account's existing folders and created at the top level if absent.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use colored::Colorize;
use std::collections::HashSet;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::Config;

use crate::commands::mail_actions::{
    describe_filter, move_bulk_by_sender_for_account, move_bulk_inbox_for_account, move_with_retry,
};
use crate::commands::mail_delete::parse_older_than;
use crate::commands::mail_folders::ensure_folder;
use crate::commands::mail_fragment::{purge_from_cache, resolve};

/// Dispatch for `pidge mail move`: single (fragment) or bulk (`--from` /
/// `--older-than`). `to` is the destination folder's display name.
pub async fn run(
    fragment: Option<String>,
    from: Vec<String>,
    older_than: Option<String>,
    accounts: Vec<String>,
    to: String,
    yes: bool,
) -> Result<()> {
    if to.trim().is_empty() {
        return Err(anyhow!("--to <folder> must name a non-empty folder."));
    }
    match (fragment, from.is_empty(), older_than.as_ref()) {
        (Some(f), true, None) => move_single(f, &to).await,
        (None, false, _) | (None, _, Some(_)) => {
            move_bulk(from, older_than, accounts, &to, yes).await
        }
        (None, true, None) => Err(anyhow!(
            "Specify a fragment, `--from <sender>`, or `--older-than <spec>`. \
             Run `pidge mail move --help`."
        )),
        (Some(_), _, _) => unreachable!("clap enforces conflicts_with"),
    }
}

async fn move_single(fragment: String, to: &str) -> Result<()> {
    let (short_hash, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let (folder_id, created) = ensure_folder(&graph, &msg.account, to).await?;
    if created {
        println!(
            "{} Created folder {} in {}.",
            "✔".green(),
            to.cyan(),
            msg.account.dimmed()
        );
    }
    match move_with_retry(&graph, &msg.account, &msg.graph_id, &folder_id).await {
        Ok(()) => {
            // Moved messages get a new ID in the target folder; the old cache
            // entry is stale.
            let _ = purge_from_cache(&short_hash);
            println!(
                "{} Moved {} to {}.",
                "✔".green(),
                short_hash.dimmed(),
                to.cyan()
            );
            Ok(())
        }
        Err(ClientError::Graph { status: 404, .. }) => {
            let _ = purge_from_cache(&short_hash);
            Err(anyhow!(
                "Message not found on server (it may have been deleted or moved). \
                 Run `pidge mail` to refresh the cache."
            ))
        }
        Err(e) => Err(e.into()),
    }
}

async fn move_bulk(
    from: Vec<String>,
    older_than: Option<String>,
    account_filter: Vec<String>,
    to: &str,
    yes: bool,
) -> Result<()> {
    if !yes {
        return Err(anyhow!(
            "Bulk move requires explicit `-y` confirmation — there is no \
             interactive prompt. Re-run with `-y` if you really mean it."
        ));
    }

    let cutoff: Option<DateTime<Utc>> = older_than.as_deref().map(parse_older_than).transpose()?;
    let from_set: HashSet<String> = from.iter().map(|s| s.to_ascii_lowercase()).collect();

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

    let filter_desc = describe_filter(&from_set, cutoff.as_ref(), older_than.as_deref());
    let scope = if from_set.is_empty() {
        "Inbox"
    } else {
        "mailbox"
    };
    println!(
        "{} Moving {scope} messages where {} → {}…",
        "Bulk".yellow().bold(),
        filter_desc,
        to.cyan()
    );

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    const PAGE_SIZE: usize = 50;
    const MAX_PAGES: usize = 400;

    let mut total = 0usize;
    for email in &target_emails {
        let (folder_id, created) = ensure_folder(&graph, email, to).await?;
        if created {
            println!(
                "{} Created folder {} in {}.",
                "✔".green(),
                to.cyan(),
                email.dimmed()
            );
        }
        let count = if from_set.is_empty() {
            move_bulk_inbox_for_account(
                &graph, email, &folder_id, "move", cutoff, PAGE_SIZE, MAX_PAGES,
            )
            .await?
        } else {
            move_bulk_by_sender_for_account(&graph, email, &folder_id, "move", &from_set, cutoff)
                .await?
        };
        total += count;
        println!(
            "{} {}: moved {count} message{}",
            "✔".green(),
            email,
            if count == 1 { "" } else { "s" }
        );
    }
    println!("{} Total: {total}.", "✔".green().bold());
    Ok(())
}
