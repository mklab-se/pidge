//! `pidge inbox show <fragment>` — display a single message with full body.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{
    Attachment, BodyContentType, CacheLookup, Config, FullMessage, MessageCache, MessageFrom,
};

use crate::output::linkify_text;

pub async fn run(fragment: String, mark_read: bool, show_images: bool, json: bool) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge auth login` to add one."
        ));
    }

    // Look up the fragment in the cache.
    let cache = MessageCache::load()?;
    let (short_hash, message_ref) = match cache.find_by_fragment(&fragment) {
        CacheLookup::NotFound => {
            return Err(anyhow!(
                "No message found for fragment '{fragment}'. Run `pidge inbox list` to refresh the cache."
            ));
        }
        CacheLookup::Ambiguous(matches) => {
            print_ambiguous(&matches);
            return Err(anyhow!("Please provide more characters."));
        }
        CacheLookup::One(h, r) => (h, r),
    };

    // Fetch full message from Graph.
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let full = match graph
        .get_message(&message_ref.account, &message_ref.graph_id)
        .await
    {
        Ok(m) => m,
        Err(ClientError::Graph { status: 404, .. }) => {
            // Purge stale cache entry.
            purge_from_cache(&short_hash)?;
            return Err(anyhow!(
                "Message not found on server. It may have been deleted. Run `pidge inbox list` to refresh."
            ));
        }
        Err(e) => return Err(e.into()),
    };

    // Fetch attachments if the message has any. Non-fatal if it fails.
    let attachments: Vec<Attachment> = if full.has_attachments {
        match graph.list_attachments(&message_ref.account, &full.id).await {
            Ok(atts) => atts,
            Err(e) => {
                eprintln!(
                    "{} could not list attachments: {e}",
                    "WARNING:".yellow().bold()
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // Decide whether to render inline images (trusted sender OR --show-images flag).
    let render_inline = config.is_sender_trusted(&full.from.address) || show_images;

    // Render.
    if json {
        render_json(&short_hash, &full, &attachments)?;
    } else {
        render_header_and_body(&full)?;
        if render_inline {
            render_inline_images_block(&graph, &message_ref.account, &full, &attachments).await;
        }
        render_attachments_block(&attachments)?;
    }

    // Optional: mark as read.
    if mark_read {
        if let Err(e) = graph.mark_read(&message_ref.account, &full.id).await {
            eprintln!(
                "{} could not mark message as read: {e}",
                "WARNING:".yellow().bold()
            );
        }
    }

    Ok(())
}

fn purge_from_cache(short_hash: &str) -> Result<()> {
    let mut cache = MessageCache::load()?;
    cache.entries.remove(short_hash);
    cache.save()?;
    Ok(())
}

fn print_ambiguous(matches: &[(String, pidge_core::CachedMessageRef)]) {
    println!("Fragment matches multiple messages:");
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["ID", "ACCOUNT", "GRAPH ID"]);
    for (hash, r) in matches {
        table.add_row(vec![
            hash.dimmed().to_string(),
            r.account.clone(),
            r.graph_id.chars().take(20).collect::<String>() + "…",
        ]);
    }
    println!("{table}");
}

fn render_header_and_body(full: &FullMessage) -> Result<()> {
    // Header block
    println!("{}      {}", "From:".bold(), format_recipient(&full.from));
    if !full.to.is_empty() {
        println!(
            "{}        {}",
            "To:".bold(),
            format_recipient_list(&full.to)
        );
    }
    if !full.cc.is_empty() {
        println!(
            "{}        {}",
            "Cc:".bold(),
            format_recipient_list(&full.cc)
        );
    }
    println!("{}   {}", "Subject:".bold(), full.subject);
    println!(
        "{}  {} ({})",
        "Received:".bold(),
        format_local_datetime(full.received_at),
        relative_time(full.received_at),
    );
    println!();
    println!("{}", separator());
    println!();

    let body_text = render_body(full);
    let body_linkified = linkify_text(&body_text);
    println!("{}", body_linkified);
    Ok(())
}

fn render_attachments_block(attachments: &[Attachment]) -> Result<()> {
    let visible_attachments: Vec<&Attachment> =
        attachments.iter().filter(|a| !a.is_inline).collect();
    if visible_attachments.is_empty() {
        return Ok(());
    }
    println!();
    println!("{}", separator());
    println!();
    println!("{}", "Attachments:".bold());
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    for att in visible_attachments {
        table.add_row(vec![
            format!("  {}", att.name),
            humansize::format_size(att.size_bytes, humansize::DECIMAL),
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn render_inline_images_block(
    graph: &GraphClient,
    account: &str,
    full: &FullMessage,
    attachments: &[Attachment],
) {
    let inline_images: Vec<&Attachment> = attachments
        .iter()
        .filter(|a| a.is_inline && is_image_content_type(&a.content_type))
        .collect();
    if inline_images.is_empty() {
        return;
    }
    println!();
    println!("{}", separator());
    println!();
    println!("{}", "Inline images:".bold());

    for att in inline_images {
        match graph.get_attachment_bytes(account, &full.id, &att.id).await {
            Ok(bytes) => {
                if !try_render_image(&bytes) {
                    println!(
                        "  [image: {} ({})] (terminal does not support inline images)",
                        att.name,
                        humansize::format_size(att.size_bytes, humansize::DECIMAL)
                    );
                }
            }
            Err(e) => {
                eprintln!("  [image: {} — fetch failed: {e}]", att.name);
            }
        }
    }
}

fn try_render_image(bytes: &[u8]) -> bool {
    let img = match image::load_from_memory(bytes) {
        Ok(i) => i,
        Err(_) => return false,
    };
    let conf = viuer::Config {
        absolute_offset: false,
        width: Some(60),
        ..Default::default()
    };
    viuer::print(&img, &conf).is_ok()
}

fn is_image_content_type(ct: &str) -> bool {
    let ct = ct.to_lowercase();
    matches!(
        ct.as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp" | "image/gif"
    )
}

fn render_body(full: &FullMessage) -> String {
    match full.body_content_type {
        BodyContentType::Text => full.body_content.clone(),
        BodyContentType::Html => {
            let width = terminal_width().min(100);
            html2text::from_read(full.body_content.as_bytes(), width)
        }
    }
}

fn terminal_width() -> usize {
    use std::process::Command;
    if let Ok(out) = Command::new("tput").arg("cols").output() {
        if let Ok(s) = std::str::from_utf8(&out.stdout) {
            if let Ok(n) = s.trim().parse::<usize>() {
                return n;
            }
        }
    }
    80
}

fn separator() -> String {
    "─".repeat(60).dimmed().to_string()
}

fn format_recipient(r: &MessageFrom) -> String {
    if r.name.is_empty() {
        r.address.clone()
    } else {
        format!("{} <{}>", r.name, r.address)
    }
}

fn format_recipient_list(rs: &[MessageFrom]) -> String {
    rs.iter()
        .map(format_recipient)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_local_datetime(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn relative_time(then: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now - then;
    if delta.num_minutes() < 60 {
        format!("{}m ago", delta.num_minutes().max(0))
    } else if delta.num_hours() < 24 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_days() < 7 {
        format!("{}d ago", delta.num_days())
    } else {
        format_local_datetime(then)
    }
}

#[derive(Serialize)]
struct ShowOut<'a> {
    id: &'a str,
    graph_id: &'a str,
    account: &'a str,
    from: &'a MessageFrom,
    to: &'a [MessageFrom],
    cc: &'a [MessageFrom],
    bcc: &'a [MessageFrom],
    subject: &'a str,
    received_at: DateTime<Utc>,
    sent_at: DateTime<Utc>,
    is_read: bool,
    body: BodyOut<'a>,
    has_attachments: bool,
    attachments: &'a [Attachment],
}

#[derive(Serialize)]
struct BodyOut<'a> {
    content_type: BodyContentType,
    html: Option<&'a str>,
    text: String,
}

fn render_json(short_hash: &str, full: &FullMessage, attachments: &[Attachment]) -> Result<()> {
    let body_text = render_body(full);
    let body = BodyOut {
        content_type: full.body_content_type,
        html: if matches!(full.body_content_type, BodyContentType::Html) {
            Some(full.body_content.as_str())
        } else {
            None
        },
        text: body_text,
    };
    let out = ShowOut {
        id: short_hash,
        graph_id: &full.id,
        account: &full.account,
        from: &full.from,
        to: &full.to,
        cc: &full.cc,
        bcc: &full.bcc,
        subject: &full.subject,
        received_at: full.received_at,
        sent_at: full.sent_at,
        is_read: full.is_read,
        body,
        has_attachments: full.has_attachments,
        attachments,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_content_type_recognizes_common_image_types() {
        assert!(is_image_content_type("image/png"));
        assert!(is_image_content_type("image/jpeg"));
        assert!(is_image_content_type("image/jpg"));
        assert!(is_image_content_type("image/webp"));
        assert!(is_image_content_type("image/gif"));
        assert!(is_image_content_type("IMAGE/PNG"));
    }

    #[test]
    fn is_image_content_type_rejects_non_image_types() {
        assert!(!is_image_content_type("application/pdf"));
        assert!(!is_image_content_type("image/svg+xml"));
        assert!(!is_image_content_type("text/html"));
        assert!(!is_image_content_type(""));
    }
}
