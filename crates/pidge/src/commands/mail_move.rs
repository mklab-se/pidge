//! `pidge mail move` — move a message (or a bulk selection) into a folder,
//! creating the folder on first use. Single mode takes a fragment; bulk mode
//! mirrors `mail archive`/`mail delete` (`--from` / `--older-than`, gated on
//! `-y`). The destination folder is resolved case-insensitively against the
//! account's existing folders and created at the top level if absent.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use colored::Colorize;
use std::collections::{BTreeMap, HashSet};

use pidge_client::graph::batch::BatchRequest;
use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::Config;

use crate::commands::mail_actions::{
    describe_filter, move_bulk_by_sender_for_account, move_bulk_inbox_for_account, move_with_retry,
};
use crate::commands::mail_delete::parse_older_than;
use crate::commands::mail_folders::ensure_folder;
use crate::commands::mail_fragment::{purge_from_cache, resolve};

/// Dispatch for `pidge mail move`: single/multi (fragments) or bulk (`--from` /
/// `--older-than`). `to` is the destination folder's display name.
pub async fn run(
    fragments: Vec<String>,
    from: Vec<String>,
    older_than: Option<String>,
    accounts: Vec<String>,
    to: String,
    yes: bool,
) -> Result<()> {
    if to.trim().is_empty() {
        return Err(anyhow!("--to <folder> must name a non-empty folder."));
    }
    match (fragments.len(), from.is_empty(), older_than.as_ref()) {
        (1, true, None) => move_single(fragments.into_iter().next().unwrap(), &to).await,
        (n, true, None) if n > 1 => move_multi(fragments, &to, yes).await,
        (0, false, _) | (0, _, Some(_)) => move_bulk(from, older_than, accounts, &to, yes).await,
        (0, true, None) => Err(anyhow!(
            "Specify one or more fragments, `--from <sender>`, or `--older-than <spec>`. \
             Run `pidge mail move --help`."
        )),
        _ => unreachable!("clap enforces conflicts_with"),
    }
}

/// Move an exact, hand-picked set of messages named by hash fragments into
/// `to`, creating the destination folder per account as needed. Unresolvable
/// fragments are reported and skipped. Requires `-y` when moving more than one.
async fn move_multi(fragments: Vec<String>, to: &str, yes: bool) -> Result<()> {
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Bulk,
        &format!("move {} hand-picked messages to {to}", fragments.len()),
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }
    if !yes {
        return Err(anyhow!(
            "Moving multiple messages requires explicit `-y` confirmation. \
             Re-run with `-y` if you really mean it."
        ));
    }

    // Resolve fragments, grouping by account (folder IDs are per-account).
    let mut by_account: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut unresolved: Vec<String> = Vec::new();
    for frag in &fragments {
        match resolve(frag) {
            Ok((short, r)) => {
                if seen.insert(short.clone()) {
                    by_account
                        .entry(r.account)
                        .or_default()
                        .push((short, r.graph_id));
                }
            }
            Err(_) => unresolved.push(frag.clone()),
        }
    }

    if by_account.is_empty() {
        return Err(anyhow!(
            "None of the {} fragment(s) resolved to a cached message. \
             Run `pidge mail` (or `mail search`) to refresh the cache.",
            fragments.len()
        ));
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let mut total = 0usize;
    for (account, items) in &by_account {
        let (folder_id, created) = ensure_folder(&graph, account, to).await?;
        if created {
            println!(
                "{} Created folder {} in {}.",
                "✔".green(),
                to.cyan(),
                account.dimmed()
            );
        }
        let requests: Vec<BatchRequest> = items
            .iter()
            .map(|(short, gid)| {
                BatchRequest::json(
                    short.clone(),
                    "POST",
                    format!("/me/messages/{gid}/move"),
                    serde_json::json!({ "destinationId": folder_id }),
                )
            })
            .collect();
        let responses = match graph.batch_all(account, requests).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  {} move failed for {account}: {e}", "!".red());
                continue;
            }
        };
        let mut ok = 0usize;
        for item in responses {
            if item.is_success() {
                ok += 1;
                let _ = purge_from_cache(&item.id);
            } else if item.status == 404 {
                // already gone
            } else {
                eprintln!(
                    "  {} failed to move {}: HTTP {}",
                    "!".red(),
                    item.id.dimmed(),
                    item.status
                );
            }
        }
        total += ok;
        println!(
            "{} {account}: moved {ok} message{} to {}",
            "✔".green(),
            if ok == 1 { "" } else { "s" },
            to.cyan()
        );
    }
    if !unresolved.is_empty() {
        eprintln!(
            "{} {} fragment(s) didn't resolve and were skipped: {}",
            "!".yellow(),
            unresolved.len(),
            unresolved.join(", ").dimmed()
        );
    }
    println!("{} Total moved: {total}.", "✔".green().bold());
    Ok(())
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
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Bulk,
        &format!("bulk move of matching messages to {to}"),
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }

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
