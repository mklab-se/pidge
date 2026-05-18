//! Simple mail actions that operate on a single message by fragment:
//! `mark-read`, `mark-unread`, `flag`, `unflag`, `archive`.
//!
//! All five share the same shape — resolve the fragment, hit a Graph endpoint,
//! print a one-line confirmation. Stale 404s purge the cache entry so a future
//! list refresh picks up the new server state.

use anyhow::{Result, anyhow};
use colored::Colorize;

use pidge_client::{AuthClient, ClientError, GraphClient};

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
