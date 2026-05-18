//! `pidge drafts ...` dispatcher.

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use futures::future::join_all;
use inquire::Confirm;

use pidge_client::{AuthClient, ClientError, GraphClient, Outgoing};
use pidge_core::{Config, Message, MessageCache, short_hash};

use crate::cli::{DraftAttachmentCommands, DraftsCommands};
use crate::commands::attachments::upload_files;
use crate::commands::mail_fragment::{purge_from_cache, resolve};

pub async fn run(command: DraftsCommands, json: bool) -> Result<()> {
    match command {
        DraftsCommands::List {
            account,
            limit,
            page,
            compact,
        } => list(account, limit, page, compact, json).await,
        DraftsCommands::Show { fragment } => {
            // Drafts are stored in the same /me/messages namespace as mail
            // messages, so the existing `mail show` rendering works as-is.
            crate::commands::mail_show::run(fragment, false, false, false, json).await
        }
        DraftsCommands::Edit { fragment } => edit(fragment).await,
        DraftsCommands::Send { fragment, yes } => send(fragment, yes).await,
        DraftsCommands::Delete { fragment, yes } => delete(fragment, yes).await,
        DraftsCommands::Attachments { command } => match command {
            DraftAttachmentCommands::List { fragment } => attachments_list(fragment, json).await,
            DraftAttachmentCommands::Add { fragment, path } => {
                attachments_add(fragment, path).await
            }
            DraftAttachmentCommands::Remove { fragment, name } => {
                attachments_remove(fragment, name).await
            }
        },
    }
}

async fn list(
    account_filter: Vec<String>,
    limit: usize,
    page: usize,
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

    let per_account = (limit as f64 * 1.2 / target_emails.len() as f64).ceil() as usize;
    let per_account = per_account.max(10);
    let skip = page.saturating_sub(1) * per_account;

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        async move {
            let result = graph.list_drafts(&e, per_account, skip).await;
            (e, result)
        }
    });

    let mut messages: Vec<Message> = Vec::new();
    let mut had_success = false;
    for (email, result) in join_all(futures).await {
        match result {
            Ok(p) => {
                had_success = true;
                messages.extend(p.messages);
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
        return Err(anyhow!("All accounts failed."));
    }

    messages.sort_by_key(|m| std::cmp::Reverse(m.received_at));
    messages.truncate(limit);

    // Cache so the user can `pidge drafts show/edit/send/delete <fragment>`
    let mut cache = MessageCache::load()?;
    let pairs: Vec<(String, String)> = messages
        .iter()
        .map(|m| (m.id.clone(), m.account.clone()))
        .collect();
    cache.insert_many(&pairs);
    cache.save()?;

    if json {
        let rows: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": short_hash(&m.id),
                    "graph_id": m.id,
                    "account": m.account,
                    "subject": m.subject,
                    "last_modified": m.received_at,
                    "preview": m.preview,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    print_drafts_table(&messages, target_emails.len() == 1, compact)
}

fn print_drafts_table(messages: &[Message], hide_account: bool, compact: bool) -> Result<()> {
    use comfy_table::{ContentArrangement, Table};

    if messages.is_empty() {
        println!("No drafts.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "SUBJECT", "MODIFIED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for m in messages {
        let subject_cell = if compact || m.preview.is_empty() {
            style_draft_subject(&m.subject)
        } else {
            format!(
                "{}\n{}",
                style_draft_subject(&m.subject),
                crate::output::linkify_text(&m.preview).dimmed()
            )
        };
        let mut cells = vec![
            short_hash(&m.id).dimmed().to_string(),
            m.account.clone(),
            subject_cell,
            relative_time(m.received_at),
        ];
        if hide_account {
            cells.remove(1);
        }
        table.add_row(cells);
    }

    println!("{table}");
    Ok(())
}

fn style_draft_subject(subject: &str) -> String {
    let s = if subject.is_empty() {
        "(no subject)".italic().to_string()
    } else {
        subject.to_string()
    };
    s.bold().bright_yellow().to_string()
}

fn relative_time(then: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let delta = now - then;
    if delta.num_minutes() < 60 {
        format!("{}m ago", delta.num_minutes().max(0))
    } else if delta.num_hours() < 24 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_days() < 7 {
        format!("{}d ago", delta.num_days())
    } else {
        then.with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    }
}

// ---- edit -----------------------------------------------------------------

async fn edit(fragment: String) -> Result<()> {
    let (_short, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    // Fetch the current draft state to pre-fill the TUI compose form.
    let current = graph
        .get_message(&msg.account, &msg.graph_id)
        .await
        .context("failed to fetch draft from Microsoft Graph")?;

    let initial = crate::commands::compose_form::Compose {
        from: msg.account.clone(),
        to: current.to.iter().map(|r| r.address.clone()).collect(),
        cc: current.cc.iter().map(|r| r.address.clone()).collect(),
        bcc: current.bcc.iter().map(|r| r.address.clone()).collect(),
        subject: current.subject.clone(),
        body: current.body_content.clone(),
        attachments: Vec::new(),
    };
    let accounts: Vec<String> = Config::load()?
        .accounts
        .iter()
        .map(|a| a.email.clone())
        .collect();
    let outcome = crate::commands::compose_form::run(
        initial,
        accounts,
        crate::commands::compose_form::Context::EditDraft,
    )?;

    match outcome {
        crate::commands::compose_form::Outcome::Send(c) => {
            patch_draft(&graph, &msg.account, &msg.graph_id, &c).await?;
            if !c.attachments.is_empty() {
                crate::commands::attachments::upload_files(
                    &graph,
                    &msg.account,
                    &msg.graph_id,
                    &c.attachments,
                )
                .await?;
            }
            graph
                .send_draft(&msg.account, &msg.graph_id)
                .await
                .context("Microsoft Graph rejected /send for the draft")?;
            let _ = purge_from_cache(&pidge_core::short_hash(&msg.graph_id));
            println!("{} Sent.", "✔".green());
        }
        crate::commands::compose_form::Outcome::Draft(c) => {
            patch_draft(&graph, &msg.account, &msg.graph_id, &c).await?;
            if !c.attachments.is_empty() {
                crate::commands::attachments::upload_files(
                    &graph,
                    &msg.account,
                    &msg.graph_id,
                    &c.attachments,
                )
                .await?;
            }
            println!("{} Saved.", "✔".green());
        }
        crate::commands::compose_form::Outcome::Cancel => {
            println!("Aborted.");
        }
    }
    Ok(())
}

async fn patch_draft(
    graph: &GraphClient,
    account: &str,
    message_id: &str,
    c: &crate::commands::compose_form::Compose,
) -> Result<()> {
    let outgoing = Outgoing {
        subject: c.subject.clone(),
        body_text: c.body.clone(),
        to: c.to.clone(),
        cc: c.cc.clone(),
        bcc: c.bcc.clone(),
    };
    graph
        .update_draft(account, message_id, &outgoing)
        .await
        .context("Microsoft Graph rejected the update")
}

// ---- send -----------------------------------------------------------------

async fn send(fragment: String, yes: bool) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    if !yes {
        let confirmed = Confirm::new(&format!("Send draft {}?", short.dimmed()))
            .with_default(false)
            .prompt()
            .map_err(|e| anyhow!("prompt cancelled: {e}"))?;
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    graph
        .send_draft(&msg.account, &msg.graph_id)
        .await
        .context("Microsoft Graph rejected /send")?;
    // After send, the draft is gone — purge from cache.
    let _ = purge_from_cache(&short);
    println!("{} Sent.", "✔".green());
    Ok(())
}

// ---- delete ---------------------------------------------------------------

async fn delete(fragment: String, yes: bool) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    if !yes {
        let confirmed = Confirm::new(&format!("Delete draft {}?", short.dimmed()))
            .with_default(false)
            .prompt()
            .map_err(|e| anyhow!("prompt cancelled: {e}"))?;
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    graph
        .delete_message(&msg.account, &msg.graph_id)
        .await
        .context("Microsoft Graph rejected DELETE")?;
    let _ = purge_from_cache(&short);
    println!("{} Deleted.", "✔".green());
    Ok(())
}

// ---- attachments ----------------------------------------------------------

async fn attachments_list(fragment: String, json: bool) -> Result<()> {
    use comfy_table::{ContentArrangement, Table};
    use humansize::{DECIMAL, format_size};

    let (short, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let attachments = graph
        .list_attachments(&msg.account, &msg.graph_id)
        .await
        .context("failed to list attachments")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&attachments)?);
        return Ok(());
    }

    if attachments.is_empty() {
        println!("Draft {} has no attachments.", short.dimmed());
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["NAME", "TYPE", "SIZE"]);
    for a in &attachments {
        table.add_row(vec![
            a.name.clone(),
            a.content_type.clone(),
            format_size(a.size_bytes, DECIMAL),
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn attachments_add(fragment: String, path: std::path::PathBuf) -> Result<()> {
    let (_, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    upload_files(&graph, &msg.account, &msg.graph_id, &[path]).await?;
    Ok(())
}

async fn attachments_remove(fragment: String, name: String) -> Result<()> {
    let (_, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let attachments = graph
        .list_attachments(&msg.account, &msg.graph_id)
        .await
        .context("failed to list attachments before remove")?;

    let lower = name.to_lowercase();
    let matches: Vec<&pidge_core::Attachment> = attachments
        .iter()
        .filter(|a| a.name.to_lowercase() == lower)
        .collect();

    match matches.as_slice() {
        [] => Err(anyhow!(
            "No attachment named '{name}' on this draft. Run `pidge drafts attachments list <fragment>` to see what's there."
        )),
        [a] => {
            graph
                .delete_attachment(&msg.account, &msg.graph_id, &a.id)
                .await
                .context("Microsoft Graph rejected DELETE attachment")?;
            println!("{} Removed {}.", "✔".green(), a.name);
            Ok(())
        }
        many => Err(anyhow!(
            "Multiple attachments named '{}' on this draft ({}). Remove by Graph ID instead — \
             not yet supported in v0.3; remove via Outlook for now.",
            name,
            many.len()
        )),
    }
}
