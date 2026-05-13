//! Shell completion generation
//!
//! - Static (AOT): `pidge completion <shell>` generates a static completion script
//! - Dynamic: `source <(COMPLETE=<shell> pidge)` enables dynamic completions
//!   (handled in main.rs via clap_complete::CompleteEnv)

use std::io;

use clap::CommandFactory;
use clap_complete::generate;
use colored::Colorize;

use crate::cli::{Cli, Shell};

/// Generate shell completions and write them to stdout.
pub fn generate_completions(shell: Shell) {
    let shell_name = match shell {
        Shell::Bash => "bash",
        Shell::Zsh => "zsh",
        Shell::Fish => "fish",
        Shell::Powershell => "powershell",
    };

    let clap_shell = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
        Shell::Powershell => clap_complete::Shell::PowerShell,
    };

    let mut cmd = Cli::command();
    generate(clap_shell, &mut cmd, "pidge", &mut io::stdout());

    eprintln!();
    eprintln!("{} For dynamic completions, use instead:", "Tip:".bold());
    eprintln!(
        "  {}",
        format!("source <(COMPLETE={shell_name} pidge)").cyan()
    );
}
