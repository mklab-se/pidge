//! CLI argument definitions using clap

use anyhow::Result;
use clap::Parser;

/// A fast CLI for e-mail and calendar
#[derive(Parser)]
#[command(name = "pidge")]
#[command(author, version, about)]
#[command(long_about = "A fast CLI for e-mail and calendar.\n\n\
    Foundation release — AI configuration, shell completions, and version info only. \
    E-mail and calendar feature commands ship in future releases.")]
#[command(propagate_version = true)]
pub struct Cli {
    /// Increase output verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Output as machine-readable JSON instead of formatted text
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(clap::Subcommand)]
pub enum Commands {
    /// Manage AI features (shows status when run without a subcommand)
    Ai {
        #[command(subcommand)]
        command: Option<AiCommands>,
    },

    /// Manage Microsoft 365 account authentication
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// View messages in your inbox
    Inbox {
        #[command(subcommand)]
        command: InboxCommands,
    },

    /// Manage the trusted-senders list (auto-renders inline images from these senders)
    Trust {
        #[command(subcommand)]
        command: TrustCommands,
    },

    /// Generate shell completions
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Show version information
    Version,
}

#[derive(clap::Subcommand)]
pub enum AiCommands {
    /// Test AI integration by sending a message
    Test {
        /// Message to send (default: "Say hello in one sentence.")
        message: Option<String>,
    },
    /// Enable AI features for pidge
    Enable,
    /// Disable AI features for pidge
    Disable,
    /// Interactively configure AI provider and model settings
    Config,
    /// Show AI status (same as running `pidge ai` without a subcommand)
    Status,
    /// AI agent skill information — helps set up Claude Code skills for pidge
    Skill {
        /// Output the skill markdown content (ready to save as a skill file)
        #[arg(long)]
        emit: bool,

        /// Output detailed reference documentation for AI agents
        #[arg(long)]
        reference: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum AuthCommands {
    /// Sign in to a Microsoft account (interactive device code flow)
    Login,
    /// List signed-in accounts
    List,
    /// Show authentication status and defaults
    Status,
    /// Sign out of one or all accounts
    Logout {
        /// Email of the account to sign out
        #[arg(long)]
        account: Option<String>,
        /// Sign out of every signed-in account
        #[arg(long, conflicts_with = "account")]
        all: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show or set default accounts
    Default {
        /// Set the default sender account
        #[arg(long)]
        send: Option<String>,
        /// Set the default calendar account
        #[arg(long)]
        calendar: Option<String>,
    },
}

#[derive(clap::Subcommand)]
pub enum InboxCommands {
    /// List messages in the inbox, merged across all signed-in accounts
    List {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// Maximum number of messages to show
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// Show only unread messages
        #[arg(long)]
        unread: bool,

        /// One row per message (no preview lines)
        #[arg(short = 'c', long)]
        compact: bool,
    },

    /// Display a single message identified by a fragment of its short hash
    Show {
        /// Fragment of the 8-char short hash (prefix, suffix, or substring)
        fragment: String,

        /// Also mark the message as read on the server
        #[arg(short = 'r', long)]
        mark_read: bool,

        /// Force inline image rendering for this invocation, regardless of trust list
        #[arg(long)]
        show_images: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum TrustCommands {
    /// List trusted sender addresses
    List,
    /// Add an email address to the trust list (idempotent)
    Add {
        /// Email address to add
        email: String,
    },
    /// Remove an email address from the trust list (idempotent)
    Remove {
        /// Email address to remove
        email: String,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Some(Commands::Ai { command }) => crate::commands::ai::run(command).await,
            Some(Commands::Auth { command }) => {
                crate::commands::auth::run(command, self.json).await
            }
            Some(Commands::Inbox { command }) => {
                crate::commands::inbox::run(command, self.json).await
            }
            Some(Commands::Trust { command }) => {
                crate::commands::trust::run(command, self.json).await
            }
            Some(Commands::Completion { shell }) => {
                crate::commands::completion::generate_completions(shell);
                Ok(())
            }
            Some(Commands::Version) => {
                crate::banner::print_banner_with_version();
                Ok(())
            }
            None => {
                use clap::CommandFactory;
                let mut cmd = Self::command();
                cmd.print_help()?;
                println!();
                Ok(())
            }
        }
    }
}
