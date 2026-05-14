//! `pidge inbox send` / `inbox reply` / `inbox reply-all` / `inbox forward`.
//!
//! Two paths through each command:
//!
//! 1. **Wizard** (default) — `inquire` prompts walk the user through To, Cc,
//!    Bcc, Subject, and body, then show a summary and a final yes/no Confirm.
//! 2. **Power-user / scripting** — pass every value as a flag; with `-y` the
//!    confirmation is skipped too.
//!
//! Bodies are plain text (HTML composition is out of scope until drafts in
//! Phase 4). Reply and forward send a "comment" that Graph prepends to its
//! auto-generated quote of the original — i.e. we never compose the quoted
//! text ourselves; Graph owns that.
//!
//! Safety:
//! - We always show a summary before sending; `-y` is the explicit opt-out.
//! - `--to` values are validated to look like e-mail addresses (contain `@`).
//! - Body and `--body-file` are mutually exclusive (clap enforces).

use std::io::Read;

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use inquire::{Confirm, Editor, Text};

use pidge_client::{AuthClient, GraphClient, Outgoing};
use pidge_core::{Config, MessageCache, short_hash};

use crate::cli::{ComposeArgs, ForwardArgs, ReplyArgs};
use crate::commands::inbox_fragment::resolve;

// ---- inbox send -----------------------------------------------------------

pub async fn send(args: ComposeArgs) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }

    let from = resolve_from_account(&config, args.from.as_deref())?;
    let to = collect_addresses("To", args.to, true)?;
    let cc = collect_addresses("Cc (optional, leave empty to skip)", args.cc, false)?;
    let bcc = collect_addresses("Bcc (optional, leave empty to skip)", args.bcc, false)?;
    let subject = resolve_subject(args.subject)?;
    let body = resolve_body(args.body, args.body_file, "Body")?;

    print_summary(&from, &to, &cc, &bcc, &subject, &body);
    let prompt = if args.draft {
        "Save as draft?"
    } else {
        "Send this message?"
    };
    if !confirm_send(args.yes, prompt)? {
        return Ok(());
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let outgoing = Outgoing {
        subject,
        body_text: body,
        to,
        cc,
        bcc,
    };
    if args.draft {
        let id = graph
            .create_draft(&from, &outgoing)
            .await
            .context("Microsoft Graph rejected the draft")?;
        cache_and_report_draft(&from, &id, "Saved draft")?;
    } else {
        graph
            .send_mail(&from, &outgoing)
            .await
            .context("Microsoft Graph rejected the message")?;
        println!("{} Sent.", "✔".green());
    }
    Ok(())
}

/// Insert a freshly-created draft into the message cache (so future fragment
/// lookups find it) and tell the user its short hash.
fn cache_and_report_draft(account: &str, message_id: &str, action: &str) -> Result<()> {
    let mut cache = MessageCache::load()?;
    cache.insert_many(&[(message_id.to_string(), account.to_string())]);
    cache.save()?;
    let hash = short_hash(message_id);
    println!(
        "{} {}. Use `pidge drafts edit {}` or `pidge drafts send {}`.",
        "✔".green(),
        action,
        hash,
        hash,
    );
    Ok(())
}

// ---- inbox reply / reply-all ---------------------------------------------

pub async fn reply(fragment: String, args: ReplyArgs, reply_all: bool) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let from = args.from.clone().unwrap_or(msg.account.clone());
    let body = resolve_body(args.body, args.body_file, "Your comment (optional)")?;

    println!();
    println!(
        "{} {} (id {})",
        if reply_all {
            "Reply-all to"
        } else {
            "Reply to"
        }
        .bold(),
        short.dimmed(),
        msg.account.dimmed(),
    );
    println!("{} {}", "From:".bold(), from);
    if body.trim().is_empty() {
        println!("{} (none — only Graph's auto-quote)", "Comment:".bold());
    } else {
        let preview: String = body.lines().take(3).collect::<Vec<_>>().join("\n");
        println!("{}\n{}", "Comment:".bold(), preview);
    }
    println!();

    let prompt = if args.draft {
        "Save as draft?"
    } else {
        "Send this reply?"
    };
    if !confirm_send(args.yes, prompt)? {
        return Ok(());
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    if args.draft {
        let id = if reply_all {
            graph
                .create_reply_all_draft(&from, &msg.graph_id, &body)
                .await
        } else {
            graph.create_reply_draft(&from, &msg.graph_id, &body).await
        }
        .context("Microsoft Graph rejected the draft")?;
        cache_and_report_draft(&from, &id, "Saved reply draft")?;
    } else if reply_all {
        graph
            .reply_all_message(&from, &msg.graph_id, &body)
            .await
            .context("Microsoft Graph rejected the reply")?;
        println!("{} Sent.", "✔".green());
    } else {
        graph
            .reply_message(&from, &msg.graph_id, &body)
            .await
            .context("Microsoft Graph rejected the reply")?;
        println!("{} Sent.", "✔".green());
    }
    Ok(())
}

// ---- inbox forward --------------------------------------------------------

pub async fn forward(fragment: String, args: ForwardArgs) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let from = args.from.clone().unwrap_or(msg.account.clone());
    let to = collect_addresses("Forward to", args.to, true)?;
    let body = resolve_body(args.body, args.body_file, "Your comment (optional)")?;

    println!();
    println!(
        "{} {} (id {})",
        "Forward".bold(),
        short.dimmed(),
        msg.account.dimmed()
    );
    println!("{} {}", "From:".bold(), from);
    println!("{} {}", "To:".bold(), to.join(", "));
    if body.trim().is_empty() {
        println!("{} (none — only Graph's auto-quote)", "Comment:".bold());
    } else {
        let preview: String = body.lines().take(3).collect::<Vec<_>>().join("\n");
        println!("{}\n{}", "Comment:".bold(), preview);
    }
    println!();

    let prompt = if args.draft {
        "Save as draft?"
    } else {
        "Send this forward?"
    };
    if !confirm_send(args.yes, prompt)? {
        return Ok(());
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    if args.draft {
        let id = graph
            .create_forward_draft(&from, &msg.graph_id, &to, &body)
            .await
            .context("Microsoft Graph rejected the draft")?;
        cache_and_report_draft(&from, &id, "Saved forward draft")?;
    } else {
        graph
            .forward_message(&from, &msg.graph_id, &to, &body)
            .await
            .context("Microsoft Graph rejected the forward")?;
        println!("{} Forwarded.", "✔".green());
    }
    Ok(())
}

// ---- shared helpers -------------------------------------------------------

fn resolve_from_account(config: &Config, requested: Option<&str>) -> Result<String> {
    if let Some(req) = requested {
        if config.find(req).is_none() {
            return Err(anyhow!("not signed in to {req}"));
        }
        return Ok(req.to_string());
    }
    if let Some(d) = &config.defaults.send {
        return Ok(d.clone());
    }
    if config.accounts.len() == 1 {
        return Ok(config.accounts[0].email.clone());
    }
    Err(anyhow!(
        "No default e-mail account set. Pass `--from <email>` or run \
         `pidge account default e-mail <email>` first."
    ))
}

/// Collect recipients. If `provided` is non-empty, validate and return it.
/// Otherwise prompt interactively; `required` controls whether an empty
/// answer is accepted.
fn collect_addresses(label: &str, provided: Vec<String>, required: bool) -> Result<Vec<String>> {
    let raw = if provided.is_empty() {
        Text::new(label)
            .with_help_message("Comma-separated e-mail addresses")
            .prompt()
            .map_err(|e| anyhow!("input cancelled: {e}"))?
    } else {
        provided.join(",")
    };

    let addrs: Vec<String> = raw
        .split([',', ';'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if addrs.is_empty() {
        if required {
            return Err(anyhow!("at least one recipient is required for {label}"));
        }
        return Ok(Vec::new());
    }

    for a in &addrs {
        if !a.contains('@') {
            return Err(anyhow!(
                "'{a}' doesn't look like an e-mail address (no '@'). Aborting."
            ));
        }
    }
    Ok(addrs)
}

fn resolve_subject(provided: Option<String>) -> Result<String> {
    match provided {
        Some(s) => Ok(s),
        None => Text::new("Subject")
            .prompt()
            .map_err(|e| anyhow!("input cancelled: {e}")),
    }
}

/// Resolve the body text. Precedence (highest first):
/// 1. `--body "text"`
/// 2. `--body-file path` (or `-` for stdin)
/// 3. Interactive — open the user's `$EDITOR` for multi-line input.
fn resolve_body(inline: Option<String>, body_file: Option<String>, label: &str) -> Result<String> {
    if let Some(b) = inline {
        return Ok(b);
    }
    if let Some(path) = body_file {
        if path == "-" {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read body from stdin")?;
            return Ok(buf);
        }
        return std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read body from {path}"));
    }
    // Interactive editor; inquire opens $EDITOR (falls back to nano/notepad).
    Editor::new(label)
        .prompt()
        .map_err(|e| anyhow!("editor cancelled: {e}"))
}

fn print_summary(
    from: &str,
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
) {
    println!();
    println!("{}", "─".repeat(60).dimmed());
    println!("{} {}", "From:".bold(), from);
    println!("{} {}", "To:".bold(), to.join(", "));
    if !cc.is_empty() {
        println!("{} {}", "Cc:".bold(), cc.join(", "));
    }
    if !bcc.is_empty() {
        println!("{} {}", "Bcc:".bold(), bcc.join(", "));
    }
    println!("{} {}", "Subject:".bold(), subject.bold().bright_yellow());
    println!("{}", "─".repeat(60).dimmed());
    let body_preview: String = body.lines().take(10).collect::<Vec<_>>().join("\n");
    println!("{body_preview}");
    if body.lines().count() > 10 {
        println!(
            "{}",
            format!("... ({} more lines)", body.lines().count() - 10).dimmed()
        );
    }
    println!("{}", "─".repeat(60).dimmed());
    println!();
}

fn confirm_send(skip: bool, prompt: &str) -> Result<bool> {
    if skip {
        return Ok(true);
    }
    let yes = Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt cancelled: {e}"))?;
    if !yes {
        println!("Aborted.");
    }
    Ok(yes)
}
