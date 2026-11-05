//! pidge: the e-mail and calendar client for your AI agent.
//!
//! The mail and calendar commands are the engine's operations, built into
//! clap at runtime (`dynamic.rs`); the rest (sign-in, servers, completion)
//! are fixed (`cli.rs`).

use anyhow::Result;
use clap::{Command, CommandFactory, FromArgMatches};
use pidge_engine::{Frontend, registry};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod banner;
mod cli;
mod commands;
mod dynamic;
mod exitcode;
mod guardrail;
mod local;
mod update;

use cli::{Cli, Commands, DebugCommands, McpCommands, OutboxCommands};

/// The whole command tree: fixed commands plus the engine's operations.
pub fn build_command() -> Command {
    let mut cmd = Cli::command();
    for c in dynamic::commands(&registry::all()) {
        cmd = cmd.subcommand(c);
    }
    cmd
}

#[tokio::main]
async fn main() {
    clap_complete::CompleteEnv::with_factory(build_command).complete();
    let matches = build_command().get_matches();
    let json = matches.get_flag("json");
    let verbose = matches.get_count("verbose");
    let quiet = matches.get_flag("quiet");

    let filter = match verbose {
        0 if quiet => "error",
        0 => "pidge=warn,pidge_engine=warn",
        1 => "pidge=debug,pidge_engine=debug,pidge_client=debug,pidge_gmail=debug",
        _ => "trace",
    };
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .without_time(),
        )
        .with(EnvFilter::new(filter))
        .init();
    if matches.get_flag("no_color") {
        colored::control::set_override(false);
    }
    if matches.get_flag("dry_run") {
        guardrail::set_dry_run();
    }

    let quiet_command = matches!(
        matches.subcommand_name(),
        Some("mcp" | "outbox" | "op" | "completion")
    );
    let update_handle =
        (!quiet && !json && !quiet_command && std::env::var("PIDGE_NO_UPDATE_CHECK").is_err())
            .then(|| tokio::spawn(update::check_for_updates()));

    let result = dispatch(&matches, json).await;

    if let Some(handle) = update_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
    if let Err(err) = result {
        let (kind, envelope) = exitcode::classify(&err);
        if json {
            eprintln!("{}", serde_json::json!({ "error": envelope }));
        } else {
            match &envelope.hint {
                Some(h) => eprintln!("Error: {err:#} ({h})"),
                None => eprintln!("Error: {err:#}"),
            }
        }
        std::process::exit(kind as i32);
    }
}

async fn dispatch(matches: &clap::ArgMatches, json: bool) -> Result<()> {
    let ops = registry::all();
    match matches.subcommand() {
        None => return home(json).await,
        Some((group, sub))
            if dynamic::default_for(group).is_some() && sub.subcommand().is_none() =>
        {
            let name = dynamic::default_for(group).unwrap_or_default();
            return run_op(name, serde_json::json!({}), json).await;
        }
        Some(_) => {
            if let Some((op, input)) = dynamic::resolve(&ops, matches) {
                return run_op(op.name, input, json).await;
            }
        }
    }
    let cli = Cli::from_arg_matches(matches)?;
    match cli.command {
        None => home(json).await,
        Some(Commands::Account { command }) => commands::account::run(command, json).await,
        Some(Commands::Mcp {
            command: McpCommands::Serve,
        }) => local::serve_stdio().await,
        Some(Commands::Mcp { command }) => commands::mcp::run(command, json).await,
        Some(Commands::Ai { command }) => commands::ai::run(command).await,
        Some(Commands::Skill { from_source }) => commands::skill::run(from_source),
        Some(Commands::Op { name, input, list }) => {
            if list || name.is_none() {
                for o in &ops {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({ "name": o.name, "description": o.description, "input": o.schema })
                        );
                    } else {
                        println!("{:<24} {}", o.name, o.summary);
                    }
                }
                return Ok(());
            }
            let raw = match input.as_deref() {
                Some("-") => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    s
                }
                Some(s) => s.to_string(),
                None => "{}".into(),
            };
            let input: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| pidge_engine::OpError::usage(format!("--input is not JSON: {e}")))?;
            run_op(name.as_deref().unwrap_or_default(), input, json).await
        }
        Some(Commands::Outbox {
            command: OutboxCommands::Run,
        }) => {
            let user = local::open_user(Frontend::Cli).await?;
            pidge_engine::outbox::run_until_empty(&user).await;
            Ok(())
        }
        Some(Commands::Debug {
            command: DebugCommands::RawHtml { id },
        }) => {
            let user = local::open_user(Frontend::Cli).await?;
            let t = pidge_engine::ops::resolve_message(&user, &id)?;
            let msg = user.provider(&t.account)?.get_message(&t.id).await.map_err(pidge_engine::OpError::from)?;
            print!("{}", msg.body_content);
            Ok(())
        }
        Some(Commands::Completion { shell }) => {
            commands::completion::generate_completions(shell);
            Ok(())
        }
        Some(Commands::Version) => {
            banner::print_banner_with_version();
            Ok(())
        }
    }
}

/// Run one operation and print its answer.
async fn run_op(name: &str, input: serde_json::Value, json: bool) -> Result<()> {
    let op = registry::find(name).ok_or_else(|| {
        pidge_engine::OpError::usage(format!("no operation {name:?}; see `pidge op --list`"))
    })?;
    let user = local::open_user(Frontend::Cli).await?;
    let out = registry::run(&user, &op, input).await?;
    user.drain().await;
    if json {
        println!("{}", serde_json::to_string_pretty(&out.data)?);
    } else {
        println!("{}", out.text);
    }
    Ok(())
}

/// `pidge` alone: what's new, and today's calendar.
async fn home(json: bool) -> Result<()> {
    let config = pidge_core::Config::load()?;
    if config.accounts.is_empty() {
        banner::print_banner();
        println!(
            "No mailbox yet. Start with `pidge account add` (or `--provider google` for Gmail)."
        );
        return Ok(());
    }
    let user = local::open_user(Frontend::Cli).await?;
    let brief = registry::run(
        &user,
        &registry::find("mail_brief").expect("mail_brief"),
        serde_json::json!({}),
    )
    .await?;
    let agenda = registry::run(
        &user,
        &registry::find("calendar_agenda").expect("calendar_agenda"),
        serde_json::json!({}),
    )
    .await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "mail": brief.data, "calendar": agenda.data })
            )?
        );
    } else {
        let strip = |t: &str| {
            t.lines()
                .filter(|l| !l.starts_with("next:"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        println!("{}\n\n{}", strip(&brief.text), strip(&agenda.text));
    }
    Ok(())
}
