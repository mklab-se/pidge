//! Simple mail actions that operate on a single message by fragment:
//! `mark-read`, `mark-unread`, `flag`, `unflag`, `archive`. Archive also
//! has a bulk mode (`--from` / `--older-than`) — see `archive_bulk`.
//!
//! All five share the same shape — resolve the fragment, hit a Graph endpoint,
//! print a one-line confirmation. Stale 404s purge the cache entry so a future
//! list refresh picks up the new server state.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use colored::Colorize;
use std::collections::HashSet;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::Config;

use crate::commands::mail_delete::parse_older_than;
use crate::commands::mail_fragment::{purge_from_cache, resolve};

pub async fn mark_read(fragment: String) -> Result<()> {
    let (short_hash, msg) = resolve(&fragment)?;
    with_graph(
        |graph| async move { graph.mark_read(&msg.account, &msg.graph_id).await },
        &short_hash,
    )
    .await?;
    println!("{} Marked {} as read.", "✔".green(), short_hash.dimmed());
    Ok(())
}

pub async fn mark_unread(fragment: String) -> Result<()> {
    let (short_hash, msg) = resolve(&fragment)?;
    with_graph(
        |graph| async move { graph.mark_unread(&msg.account, &msg.graph_id).await },
        &short_hash,
    )
    .await?;
    println!("{} Marked {} as unread.", "✔".green(), short_hash.dimmed());
    Ok(())
}

pub async fn flag(fragment: String) -> Result<()> {
    let (short_hash, msg) = resolve(&fragment)?;
    with_graph(
        |graph| async move { graph.set_flag(&msg.account, &msg.graph_id, true).await },
        &short_hash,
    )
    .await?;
    println!("{} Flagged {}.", "✔".green(), short_hash.dimmed());
    Ok(())
}

pub async fn unflag(fragment: String) -> Result<()> {
    let (short_hash, msg) = resolve(&fragment)?;
    with_graph(
        |graph| async move { graph.set_flag(&msg.account, &msg.graph_id, false).await },
        &short_hash,
    )
    .await?;
    println!("{} Unflagged {}.", "✔".green(), short_hash.dimmed());
    Ok(())
}

pub async fn archive(fragment: String) -> Result<()> {
    let (short_hash, msg) = resolve(&fragment)?;
    with_graph(
        |graph| async move {
            graph
                .move_message(&msg.account, &msg.graph_id, "archive")
                .await
        },
        &short_hash,
    )
    .await?;
    // Archived messages get a new ID in the Archive folder; the old cache
    // entry is stale.
    let _ = purge_from_cache(&short_hash);
    println!("{} Archived {}.", "✔".green(), short_hash.dimmed());
    Ok(())
}

/// Dispatch for `pidge mail archive`: single (fragment) or bulk (`--from` /
/// `--older-than`). Mirrors `mail delete` so the two destructive surfaces
/// share their safety model — bulk always needs `-y` and one filter.
pub async fn archive_dispatch(
    fragment: Option<String>,
    from: Vec<String>,
    older_than: Option<String>,
    accounts: Vec<String>,
    yes: bool,
) -> Result<()> {
    match (fragment, from.is_empty(), older_than.as_ref()) {
        (Some(f), true, None) => archive(f).await,
        (None, false, _) | (None, _, Some(_)) => {
            archive_bulk(from, older_than, accounts, yes).await
        }
        (None, true, None) => Err(anyhow!(
            "Specify a fragment, `--from <sender>`, or `--older-than <spec>`. \
             Run `pidge mail archive --help`."
        )),
        (Some(_), _, _) => unreachable!("clap enforces conflicts_with"),
    }
}

async fn archive_bulk(
    from: Vec<String>,
    older_than: Option<String>,
    account_filter: Vec<String>,
    yes: bool,
) -> Result<()> {
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Bulk,
        "bulk archive of matching messages",
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }

    if !yes {
        return Err(anyhow!(
            "Bulk archive requires explicit `-y` confirmation — there is no \
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
        "{} Archiving {scope} messages where {}…",
        "Bulk".yellow().bold(),
        filter_desc
    );

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    const PAGE_SIZE: usize = 50;
    const MAX_PAGES: usize = 400;

    let mut total = 0usize;
    for email in &target_emails {
        let count = if from_set.is_empty() {
            // Date-only mode: walk the Inbox sorted by date and stop once
            // we've passed the cutoff.
            move_bulk_inbox_for_account(
                &graph, email, "archive", "archive", cutoff, PAGE_SIZE, MAX_PAGES,
            )
            .await?
        } else {
            // Sender-filter mode: run one Graph search per sender so we
            // sweep ALL folders (marketing senders frequently land in
            // Junk, not Inbox), then archive everything that came back.
            move_bulk_by_sender_for_account(&graph, email, "archive", "archive", &from_set, cutoff)
                .await?
        };
        total += count;
        println!(
            "{} {}: archived {count} message{}",
            "✔".green(),
            email,
            if count == 1 { "" } else { "s" }
        );
    }
    println!("{} Total: {total}.", "✔".green().bold());
    Ok(())
}

pub(crate) fn describe_filter(
    from: &HashSet<String>,
    cutoff: Option<&DateTime<Utc>>,
    older_than_spec: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !from.is_empty() {
        let mut senders: Vec<&str> = from.iter().map(String::as_str).collect();
        senders.sort_unstable();
        parts.push(format!("from is one of [{}]", senders.join(", ")));
    }
    if let (Some(c), Some(spec)) = (cutoff, older_than_spec) {
        parts.push(format!(
            "received before {} (cutoff {})",
            spec,
            c.format("%Y-%m-%d %H:%M UTC")
        ));
    }
    parts.join(" AND ")
}

/// Date-only bulk: walk the Inbox newest-first and stop once we cross
/// the cutoff, moving matched messages to `destination`. Mirrors
/// `delete_bulk_for_account` — see that function's comments for the
/// rationale on PAGE_SIZE / MAX_PAGES / sort-and-stop. Shared by
/// `mail archive` (destination `"archive"`) and `mail move`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn move_bulk_inbox_for_account(
    graph: &GraphClient,
    account: &str,
    destination: &str,
    verb: &str,
    cutoff: Option<DateTime<Utc>>,
    page_size: usize,
    max_pages: usize,
) -> Result<usize> {
    let mut archived = 0usize;
    let mut skip = 0usize;
    for _ in 0..max_pages {
        let result = graph
            .list_inbox(account, page_size, skip, false)
            .await
            .context("listing inbox for bulk move")?;
        if result.messages.is_empty() {
            break;
        }

        let to_archive: Vec<&pidge_core::Message> = result
            .messages
            .iter()
            .filter(|m| cutoff.is_none_or(|c| m.received_at < c))
            .collect();
        let matched_now = to_archive.len();
        archived += move_many(graph, account, &to_archive, destination, verb).await;

        // Advance skip past kept items (the matched ones got moved out of
        // the Inbox, so the same skip now points at fresh messages).
        let page_len = result.messages.len();
        skip += page_len - matched_now;

        // Sorted desc by received_at — once the oldest on the page is
        // already newer than the cutoff, nothing further back can match.
        if let Some(c) = cutoff {
            let oldest = result
                .messages
                .iter()
                .map(|m| m.received_at)
                .min()
                .unwrap_or(c);
            if oldest >= c {
                break;
            }
        }

        if !result.has_more && matched_now == 0 {
            break;
        }
    }
    Ok(archived)
}

/// Sender-filter bulk: search the entire mailbox (all folders) for each
/// sender, moving matches to `destination`. Marketing senders frequently
/// get auto-classified into the Junk Email folder, so an Inbox-only sweep
/// would miss them. We use Graph's `$search` because that endpoint is
/// folder-agnostic. Shared by `mail archive` and `mail move`.
pub(crate) async fn move_bulk_by_sender_for_account(
    graph: &GraphClient,
    account: &str,
    destination: &str,
    verb: &str,
    from_set: &HashSet<String>,
    cutoff: Option<DateTime<Utc>>,
) -> Result<usize> {
    // Graph $search has a hard ~1000-result cap per query. One query per
    // sender keeps each search comfortably under that and lets us report
    // per-sender numbers if we ever want to.
    const SEARCH_LIMIT: usize = 1000;
    let mut archived = 0usize;
    for sender in from_set {
        let messages = match graph
            .search_messages(account, &format!("from:{sender}"), SEARCH_LIMIT)
            .await
        {
            Ok(page) => page.messages,
            Err(e) => {
                eprintln!("  {} search failed for {}: {e}", "!".red(), sender.dimmed());
                continue;
            }
        };
        let matches: Vec<&pidge_core::Message> = messages
            .iter()
            .filter(|m| {
                // Graph $search is fuzzy — re-check exact sender match
                // before moving anything. Also apply the date cutoff if set.
                m.from.address.to_ascii_lowercase() == *sender
                    && cutoff.is_none_or(|c| m.received_at < c)
            })
            .collect();
        archived += move_many(graph, account, &matches, destination, verb).await;
    }
    Ok(archived)
}

/// Concurrently move a batch of messages to `destination` (a Graph folder ID
/// or well-known name) and report how many succeeded. Cache entries are
/// purged for moved messages so the next list refresh sees the new state.
///
/// `verb` is only used to phrase the per-failure error line ("failed to
/// archive …" vs "failed to move …").
///
/// Concurrency is capped at `MAX_INFLIGHT` because Graph's per-mailbox
/// move endpoint trips an `ApplicationThrottled` (HTTP 429) on aggressive
/// parallelism — we saw it as low as ~8 inflight moves. On 429 we retry
/// with exponential backoff up to `MAX_RETRIES` times.
pub(crate) async fn move_many(
    graph: &GraphClient,
    account: &str,
    messages: &[&pidge_core::Message],
    destination: &str,
    verb: &str,
) -> usize {
    use pidge_client::graph::batch::BatchRequest;
    let requests: Vec<BatchRequest> = messages
        .iter()
        .map(|m| {
            BatchRequest::json(
                pidge_core::short_hash(&m.id),
                "POST",
                format!("/me/messages/{}/move", m.id),
                serde_json::json!({"destinationId": destination}),
            )
        })
        .collect();
    let responses = match graph.batch_all(account, requests).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  {} bulk {verb} failed: {e}", "!".red());
            return 0;
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
                "  {} failed to {verb} {}: HTTP {}",
                "!".red(),
                item.id.dimmed(),
                item.status
            );
        }
    }
    ok
}

/// Move a single message to `destination`, retrying with exponential
/// backoff on `429 ApplicationThrottled` since Graph's per-mailbox move
/// limit is hit easily on bulk operations. Retry/backoff now lives in the
/// client's send_with_retry seam; this wrapper remains for call-site clarity.
pub(crate) async fn move_with_retry(
    graph: &GraphClient,
    account: &str,
    message_id: &str,
    destination: &str,
) -> Result<(), ClientError> {
    graph.move_message(account, message_id, destination).await
}

/// Run an async closure with a configured GraphClient. On a 404 from Graph,
/// purges the cache entry so the user's next list refresh sees the new state,
/// and converts the error into a user-friendly message.
async fn with_graph<F, Fut>(op: F, short_hash: &str) -> Result<()>
where
    F: FnOnce(GraphClient) -> Fut,
    Fut: std::future::Future<Output = Result<(), ClientError>>,
{
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    match op(graph).await {
        Ok(()) => Ok(()),
        Err(ClientError::Graph { status: 404, .. }) => {
            let _ = purge_from_cache(short_hash);
            Err(anyhow!(
                "Message not found on server (it may have been deleted or moved). \
                 Run `pidge mail` to refresh the cache."
            ))
        }
        Err(e) => Err(e.into()),
    }
}
