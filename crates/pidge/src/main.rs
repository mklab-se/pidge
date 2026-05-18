//! pidge — A fast CLI for e-mail and calendar

use anyhow::Result;
use clap::{CommandFactory, Parser};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod banner;
mod cli;
mod commands;
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
fn preprocess_args(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    // Only intervene on `pidge mail <something>` — every other top-level
    // command has a required, non-shortcut subcommand and passes through.
    if args.len() < 2 || args[1] != "mail" {
        return args;
    }

    // `pidge mail` → `pidge mail list`
    if args.len() == 2 {
        args.push("list".into());
        return args;
    }

    let arg2 = args[2].to_string_lossy();

    // Help / version flags pass through so clap prints `mail` help, not `list` help.
    if matches!(arg2.as_ref(), "-h" | "--help" | "-V" | "--version") {
        return args;
    }

    // Any other flag means the user wants `list` with those flags.
    if arg2.starts_with('-') {
        args.insert(2, "list".into());
        return args;
    }

    // A bare word that matches a known subcommand passes through.
    if cli::MAIL_SUBCOMMAND_NAMES.contains(&arg2.as_ref()) {
        return args;
    }

    // Otherwise treat the bare word as a message fragment.
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

    let result = cli.run().await;

    if let Some(handle) = update_handle {
        let _ = handle.await;
    }

    result
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
}
