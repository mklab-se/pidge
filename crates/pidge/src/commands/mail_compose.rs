//! `pidge mail new` / `mail reply` / `mail reply-all` / `mail forward`.
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
use crate::commands::attachments::upload_files;
use crate::commands::compose_form;
use crate::commands::mail_fragment::resolve;

// ---- mail new -------------------------------------------------------------

pub async fn send(args: ComposeArgs) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }

    let from_default = resolve_from_account(&config, args.from.as_deref())?;

    // Non-interactive scripting path: if every required field is on the
    // command line and `--confirm` wasn't requested, send straight through
    // without launching the TUI. This is the common path for scripts and
    // one-liners — the user already spelled out the whole message, no
    // need for a review step they didn't ask for.
    let fully_specified = !args.to.is_empty()
        && args.subject.is_some()
        && (args.body.is_some() || args.body_file.is_some());
    if fully_specified && !args.confirm {
        let body = resolve_body(args.body.clone(), args.body_file.clone(), "Body", false)?;
        let to = parse_addrs(&args.to)?;
        let cc = parse_addrs(&args.cc)?;
        let bcc = parse_addrs(&args.bcc)?;
        return send_or_draft(
            &from_default,
            compose_form::Compose {
                from: from_default.clone(),
                to,
                cc,
                bcc,
                subject: args.subject.unwrap_or_default(),
                body,
                attachments: args.attach,
            },
            args.draft,
        )
        .await;
    }

    // Interactive: launch the TUI form pre-filled with any flags the user
    // did pass. The form handles validation, attachments, and the final
    // send/draft/cancel decision; we just route the outcome.
    let initial = compose_form::Compose {
        from: from_default.clone(),
        to: args.to,
        cc: args.cc,
        bcc: args.bcc,
        subject: args.subject.unwrap_or_default(),
        body: resolve_body_or_empty(args.body, args.body_file)?,
        attachments: args.attach,
    };
    let accounts: Vec<String> = config.accounts.iter().map(|a| a.email.clone()).collect();
    let outcome = compose_form::run(initial, accounts, compose_form::Context::NewSend)?;
    match outcome {
        compose_form::Outcome::Send(c) => send_or_draft(&c.from.clone(), c, false).await,
        compose_form::Outcome::Draft(c) => send_or_draft(&c.from.clone(), c, true).await,
        compose_form::Outcome::Cancel => {
            println!("Aborted.");
            Ok(())
        }
    }
}

/// Push a fully-specified compose through Graph — either send-and-go or
/// create-draft (then either send or stop). Shared by the non-interactive
/// path and the TUI's `Send` / `Draft` outcomes.
async fn send_or_draft(from: &str, c: compose_form::Compose, save_as_draft: bool) -> Result<()> {
    if !save_as_draft {
        let gate = crate::guardrail::gate(
            crate::guardrail::GuardrailAction::Send,
            &format!(
                "send '{}' from {from} to {} recipient(s){}",
                c.subject,
                c.to.len() + c.cc.len() + c.bcc.len(),
                if c.attachments.is_empty() {
                    String::new()
                } else {
                    format!(" with {} attachment(s)", c.attachments.len())
                }
            ),
        )?;
        if gate == crate::guardrail::Gate::DryRun {
            return Ok(());
        }
    }
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let outgoing = Outgoing {
        subject: c.subject,
        body_text: c.body,
        to: c.to,
        cc: c.cc,
        bcc: c.bcc,
    };

    // Same rule as before: attachments force the draft-then-send pathway
    // because Graph's /sendMail can't carry uploads in one call.
    let need_draft = save_as_draft || !c.attachments.is_empty();
    if !need_draft {
        graph
            .send_mail(from, &outgoing)
            .await
            .context("Microsoft Graph rejected the message")?;
        println!("{} Sent.", "✔".green());
        return Ok(());
    }

    let id = graph
        .create_draft(from, &outgoing)
        .await
        .context("Microsoft Graph rejected the draft")?;
    if !c.attachments.is_empty() {
        println!();
        println!("{}", "Uploading attachments:".bold());
        upload_files(&graph, from, &id, &c.attachments).await?;
    }
    if save_as_draft {
        cache_and_report_draft(from, &id, "Saved draft")?;
    } else {
        graph
            .send_draft(from, &id)
            .await
            .context("Microsoft Graph rejected /send for the draft")?;
        println!("{} Sent.", "✔".green());
    }
    Ok(())
}

fn parse_addrs(raw: &[String]) -> Result<Vec<String>> {
    let joined = raw.join(",");
    let parsed = compose_form::parse_addresses(&joined)?;
    let contacts = pidge_core::ContactsCache::load()?;
    crate::commands::name_resolve::resolve_addresses(&parsed, &contacts)
}

/// `--body`/`--body-file` flag resolution that returns "" if neither was set,
/// instead of dropping to the interactive editor (the TUI form will handle
/// the body field itself).
fn resolve_body_or_empty(inline: Option<String>, body_file: Option<String>) -> Result<String> {
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
    Ok(String::new())
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

// ---- mail reply / reply-all ----------------------------------------------

pub async fn reply(fragment: String, args: ReplyArgs, reply_all: bool) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let from = args.from.clone().unwrap_or(msg.account.clone());
    let body = resolve_body(args.body, args.body_file, "Comment", true)?;

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

    // Attachments → must use createReply/createReplyAll then upload then send,
    // because Graph's /reply takes a comment string only, not attachments.
    let need_draft = args.draft || !args.attach.is_empty();

    if !args.draft {
        let gate = crate::guardrail::gate(
            crate::guardrail::GuardrailAction::Send,
            &format!(
                "reply{} to message {short} from {from}",
                if reply_all { "-all" } else { "" }
            ),
        )?;
        if gate == crate::guardrail::Gate::DryRun {
            return Ok(());
        }
    }

    if !need_draft {
        if reply_all {
            graph
                .reply_all_message(&from, &msg.graph_id, &body)
                .await
                .context("Microsoft Graph rejected the reply")?;
        } else {
            graph
                .reply_message(&from, &msg.graph_id, &body)
                .await
                .context("Microsoft Graph rejected the reply")?;
        }
        println!("{} Sent.", "✔".green());
        return Ok(());
    }

    let id = if reply_all {
        graph
            .create_reply_all_draft(&from, &msg.graph_id, &body)
            .await
    } else {
        graph.create_reply_draft(&from, &msg.graph_id, &body).await
    }
    .context("Microsoft Graph rejected the draft")?;
    if !args.attach.is_empty() {
        println!();
        println!("{}", "Uploading attachments:".bold());
        upload_files(&graph, &from, &id, &args.attach).await?;
    }
    if args.draft {
        let label = if reply_all {
            "Saved reply-all draft"
        } else {
            "Saved reply draft"
        };
        cache_and_report_draft(&from, &id, label)?;
    } else {
        graph
            .send_draft(&from, &id)
            .await
            .context("Microsoft Graph rejected /send for the draft")?;
        println!("{} Sent.", "✔".green());
    }
    Ok(())
}

// ---- mail forward ---------------------------------------------------------

pub async fn forward(fragment: String, args: ForwardArgs) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let from = args.from.clone().unwrap_or(msg.account.clone());
    let to = collect_addresses("Forward to", args.to, true)?;
    let body = resolve_body(args.body, args.body_file, "Comment", true)?;

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
    if !crate::guardrail::dry_run_active() && !confirm_send(args.yes, prompt)? {
        return Ok(());
    }
    if !args.draft {
        let gate = crate::guardrail::gate(
            crate::guardrail::GuardrailAction::Send,
            &format!("forward message {short} from {from} to {}", to.join(", ")),
        )?;
        if gate == crate::guardrail::Gate::DryRun {
            return Ok(());
        }
    }

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let need_draft = args.draft || !args.attach.is_empty();

    if !need_draft {
        graph
            .forward_message(&from, &msg.graph_id, &to, &body)
            .await
            .context("Microsoft Graph rejected the forward")?;
        println!("{} Forwarded.", "✔".green());
        return Ok(());
    }

    let id = graph
        .create_forward_draft(&from, &msg.graph_id, &to, &body)
        .await
        .context("Microsoft Graph rejected the draft")?;
    if !args.attach.is_empty() {
        println!();
        println!("{}", "Uploading attachments:".bold());
        upload_files(&graph, &from, &id, &args.attach).await?;
    }
    if args.draft {
        cache_and_report_draft(&from, &id, "Saved forward draft")?;
    } else {
        graph
            .send_draft(&from, &id)
            .await
            .context("Microsoft Graph rejected /send for the draft")?;
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
        let help = if required {
            "Comma-separated e-mail addresses"
        } else {
            "Comma-separated e-mail addresses. Optional — leave empty to skip"
        };
        Text::new(&format!("{label}:"))
            .with_help_message(help)
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

/// Resolve the body text. Precedence (highest first):
/// 1. `--body "text"`
/// 2. `--body-file path` (or `-` for stdin)
/// 3. Interactive — open the in-pidge multi-line editor; user can press
///    Ctrl-X from inside it to hand off to `$EDITOR` (nano, vim, code …).
fn resolve_body(
    inline: Option<String>,
    body_file: Option<String>,
    label: &str,
    _optional: bool,
) -> Result<String> {
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
    interactive_body(label, "")
}

/// Open the user's `$EDITOR` (or system default) for multi-line text input.
/// Used by the reply / forward flows for the comment field; the new-send
/// and draft-edit flows use the full TUI compose form instead.
pub(crate) fn interactive_body(label: &str, initial: &str) -> Result<String> {
    let label = format!("{label}:");
    let mut e = Editor::new(&label);
    if !initial.is_empty() {
        e = e.with_predefined_text(initial);
    }
    e.with_help_message("Opens $EDITOR (set the $EDITOR env var to change the editor)")
        .prompt()
        .map_err(|e| anyhow!("editor cancelled: {e}"))
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
