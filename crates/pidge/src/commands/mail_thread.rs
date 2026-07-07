//! `pidge mail thread <fragment>` — the whole conversation, oldest first.

use anyhow::{Result, anyhow};
use colored::Colorize;
use pidge_client::{AuthClient, GraphClient};

use super::mail_fragment::resolve;

pub async fn run(fragment: String, json: bool) -> Result<()> {
    let (_short, cached) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    // The cache stores graph_id + account; conversationId comes from the
    // message itself.
    let message = graph.get_message(&cached.account, &cached.graph_id).await?;
    let conversation_id = &message.conversation_id;
    if conversation_id.is_empty() {
        return Err(anyhow!(
            "message has no conversation id (mailbox may predate threading)"
        ));
    }

    let messages = graph
        .list_conversation(&cached.account, conversation_id)
        .await?;

    if json {
        let out: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": pidge_core::short_hash(&m.id),
                    "graph_id": m.id,
                    "account": m.account,
                    "conversation_id": m.conversation_id,
                    "from": m.from,
                    "subject": m.subject,
                    "received_at": m.received_at,
                    "is_read": m.is_read,
                    "body_text": m.preview,
                })
            })
            .collect();
        crate::output::project::emit_json(serde_json::Value::Array(out))?;
        return Ok(());
    }

    println!(
        "{} {} message(s) in thread",
        "Thread:".bold(),
        messages.len()
    );
    for m in &messages {
        println!();
        println!(
            "{}  {}  {}",
            pidge_core::short_hash(&m.id).dimmed(),
            m.from.address.bold(),
            m.received_at.format("%Y-%m-%d %H:%M").to_string().dimmed()
        );
        println!("{}", m.subject.bold());
        for line in m.preview.lines().take(6) {
            println!("  {line}");
        }
    }
    Ok(())
}
