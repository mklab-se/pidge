//! pidge — A fast CLI for e-mail and calendar

use anyhow::Result;
use clap::{CommandFactory, Parser};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod banner;
mod cli;
mod commands;
mod exitcode;
mod guardrail;
mod output;
mod update;

use cli::Cli;

/// Pre-process argv to make `pidge mail` ergonomic.
///
/// Clap can't natively decide whether `pidge mail 3515` means "show message
/// with fragment 3515" or "run the (non-existent) subcommand named 3515", so
/// we rewrite argv before clap sees it:
///
/// - `pidge mail`                          → `pidge mail list`
/// - `pidge mail --account a@b.com -n 50`  → `pidge mail list --account a@b.com -n 50`
/// - `pidge mail 3515`                     → `pidge mail show 3515`
/// - `pidge mail list ...` / `mail show ...` / `mail --help` → unchanged
///
/// The known-subcommand list lives in [`cli::MAIL_SUBCOMMAND_NAMES`]; when
/// new subcommands land (search, flag, archive, new, …) add their kebab-case
/// names there too, or users will see "No message found for fragment '<name>'"
/// instead of the new behavior.
fn preprocess_args(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    if args.len() < 2 {
        return args;
    }
    match args[1].to_string_lossy().as_ref() {
        "mail" => preprocess_subcommand(args, cli::MAIL_SUBCOMMAND_NAMES),
        "calendar" => preprocess_subcommand(args, cli::CALENDAR_SUBCOMMAND_NAMES),
        _ => args,
    }
}

/// Apply the shared "bare word → show" rewrite used by both `pidge mail` and
/// `pidge calendar`:
///
/// - `pidge <ns>`                          → `pidge <ns> list`
/// - `pidge <ns> --account a@b.com -n 50`  → `pidge <ns> list --account ...`
/// - `pidge <ns> <fragment>`               → `pidge <ns> show <fragment>`
/// - `pidge <ns> list ...` / explicit subcommand / `--help` → unchanged
fn preprocess_subcommand(
    mut args: Vec<std::ffi::OsString>,
    known: &[&str],
) -> Vec<std::ffi::OsString> {
    if args.len() == 2 {
        args.push("list".into());
        return args;
    }

    let arg2 = args[2].to_string_lossy();

    if matches!(arg2.as_ref(), "-h" | "--help" | "-V" | "--version") {
        return args;
    }
    if arg2.starts_with('-') {
        args.insert(2, "list".into());
        return args;
    }
    if known.contains(&arg2.as_ref()) {
        return args;
    }
    args.insert(2, "show".into());
    args
}

#[tokio::main]
async fn main() -> Result<()> {
    // Handle dynamic shell completions (when invoked via COMPLETE=<shell> pidge)
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse_from(preprocess_args(std::env::args_os().collect()));

    // Initialize logging
    let filter = if cli.verbose > 0 {
        match cli.verbose {
            1 => "pidge=debug",
            _ => "pidge=trace",
        }
    } else if cli.quiet {
        "error"
    } else {
        "pidge=info"
    };

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).without_time())
        .with(EnvFilter::new(filter))
        .init();

    if cli.no_color {
        colored::control::set_override(false);
        output::set_no_color(true);
    }

    // Style every interactive prompt label (To:, Subject:, ...) so users can
    // tell at a glance that they're being asked for input. Reads the no-color
    // flag we just set; called once before any prompt runs.
    output::install_inquire_theme();

    // Spawn background update check (skip in quiet mode or if disabled via env)
    let update_handle = if !cli.quiet && std::env::var("PIDGE_NO_UPDATE_CHECK").is_err() {
        Some(tokio::spawn(update::check_for_updates()))
    } else {
        None
    };

    let json_errors = cli.json;
    if cli.dry_run {
        crate::guardrail::set_dry_run();
    }
    let result = cli.run().await;

    // The update check is best-effort. Bound the wait so a slow or hung
    // crates.io request (e.g. flaky DNS) can never delay process exit beyond
    // a moment — previously a hung check could wedge the CLI indefinitely.
    if let Some(handle) = update_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let (kind, envelope) = exitcode::classify(&err);
            if json_errors {
                eprintln!("{}", serde_json::json!({ "error": envelope }));
            } else {
                eprintln!("Error: {err:#}");
            }
            std::process::exit(kind as i32);
        }
    }
}

#[cfg(test)]
mod preprocess_tests {
    use super::preprocess_args;
    use std::ffi::OsString;

    fn pp(input: &[&str]) -> Vec<String> {
        let argv: Vec<OsString> = input.iter().map(|s| OsString::from(*s)).collect();
        preprocess_args(argv)
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect()
    }

    #[test]
    fn non_mail_commands_pass_through() {
        assert_eq!(
            pp(&["pidge", "account", "list"]),
            ["pidge", "account", "list"]
        );
        assert_eq!(pp(&["pidge"]), ["pidge"]);
    }

    #[test]
    fn bare_mail_inserts_list() {
        assert_eq!(pp(&["pidge", "mail"]), ["pidge", "mail", "list"]);
    }

    #[test]
    fn mail_with_flags_inserts_list_before_flags() {
        assert_eq!(
            pp(&["pidge", "mail", "--account", "a@b.com", "-n", "50"]),
            ["pidge", "mail", "list", "--account", "a@b.com", "-n", "50"]
        );
        assert_eq!(
            pp(&["pidge", "mail", "-c"]),
            ["pidge", "mail", "list", "-c"]
        );
    }

    #[test]
    fn mail_help_passes_through() {
        assert_eq!(
            pp(&["pidge", "mail", "--help"]),
            ["pidge", "mail", "--help"]
        );
        assert_eq!(pp(&["pidge", "mail", "-h"]), ["pidge", "mail", "-h"]);
    }

    #[test]
    fn mail_with_known_subcommand_passes_through() {
        assert_eq!(
            pp(&["pidge", "mail", "list", "-n", "50"]),
            ["pidge", "mail", "list", "-n", "50"]
        );
        assert_eq!(
            pp(&["pidge", "mail", "show", "3515"]),
            ["pidge", "mail", "show", "3515"]
        );
        // `attachments` is a subcommand group, not a fragment — it must not be
        // rewritten to `mail show attachments`.
        assert_eq!(
            pp(&["pidge", "mail", "attachments", "list", "3515"]),
            ["pidge", "mail", "attachments", "list", "3515"]
        );
    }

    #[test]
    fn mail_with_bare_word_inserts_show() {
        assert_eq!(
            pp(&["pidge", "mail", "3515"]),
            ["pidge", "mail", "show", "3515"]
        );
        // Even a fragment that looks like garbage routes to show — the show
        // command will then surface a clear "no message found" error.
        assert_eq!(
            pp(&["pidge", "mail", "lsit"]),
            ["pidge", "mail", "show", "lsit"]
        );
    }

    // The calendar surface uses the same arg-preprocessor as mail, so we
    // exercise the same rewrites against the `calendar` namespace.

    #[test]
    fn bare_calendar_inserts_list() {
        assert_eq!(pp(&["pidge", "calendar"]), ["pidge", "calendar", "list"]);
    }

    #[test]
    fn calendar_with_flags_inserts_list_before_flags() {
        assert_eq!(
            pp(&["pidge", "calendar", "--week"]),
            ["pidge", "calendar", "list", "--week"]
        );
    }

    #[test]
    fn calendar_known_subcommand_passes_through() {
        assert_eq!(
            pp(&["pidge", "calendar", "new", "--title", "Sync"]),
            ["pidge", "calendar", "new", "--title", "Sync"]
        );
        assert_eq!(
            pp(&["pidge", "calendar", "move-time", "abc", "--start", "10:00"]),
            ["pidge", "calendar", "move-time", "abc", "--start", "10:00"]
        );
    }

    #[test]
    fn calendar_bare_word_inserts_show() {
        assert_eq!(
            pp(&["pidge", "calendar", "abcd"]),
            ["pidge", "calendar", "show", "abcd"]
        );
    }

    #[test]
    fn calendar_help_passes_through() {
        assert_eq!(
            pp(&["pidge", "calendar", "--help"]),
            ["pidge", "calendar", "--help"]
        );
    }
}
