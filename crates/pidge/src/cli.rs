//! The CLI's fixed commands (setup, servers, completion). The mail and
//! calendar commands (`pidge mail ...`, `pidge calendar ...`,
//! `pidge accounts`) are built at runtime from the engine's operations; see
//! `dynamic.rs`.

use clap::{Parser, Subcommand, ValueEnum};

/// The e-mail and calendar client for your AI agent
#[derive(Parser)]
#[command(
    name = "pidge",
    version,
    about = "The e-mail and calendar client for your AI agent",
    long_about = "The e-mail and calendar client for your AI agent.\n\n\
Microsoft 365, Outlook.com and Gmail, merged. Built to be driven by AI agents (see `pidge skill`), \
pleasant by hand. Without a command, pidge shows what's new and today's calendar."
)]
pub struct Cli {
    /// More logging (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Less output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// No colours
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Print the structured result as JSON instead of text
    #[arg(long, global = true)]
    pub json: bool,

    /// For `pidge mcp connect`: show what would happen without doing it
    #[arg(long, global = true, hide = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Sign in to a mailbox (Microsoft or Gmail), list or remove them
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },
    /// Run pidge as a local MCP server, or connect to a hosted one
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
    /// The optional AI key pidge uses to sort mail and check sends (via Ailloy)
    Ai {
        #[command(subcommand)]
        command: Option<AiCommands>,
    },
    /// Print a SKILL.md that teaches an AI agent to use pidge
    Skill {
        /// Build the skill to run pidge with `cargo run` from this source checkout
        #[arg(long)]
        from_source: bool,
    },
    /// Run any operation with its exact JSON input (as the MCP tools take it)
    Op {
        /// The operation, e.g. mail_find (see `pidge op --list`)
        name: Option<String>,
        /// The input as JSON, or `-` to read it from stdin
        #[arg(long)]
        input: Option<String>,
        /// List the operations and their input schemas
        #[arg(long)]
        list: bool,
    },
    /// Send held mail that is due (started by pidge itself; rarely run by hand)
    #[command(hide = true)]
    Outbox {
        #[command(subcommand)]
        command: OutboxCommands,
    },
    /// Generate shell completions
    Completion {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Show version information
    Version,
}

#[derive(Subcommand)]
pub enum AccountCommands {
    /// Add a mailbox (opens your browser to sign in)
    Add {
        /// microsoft (Outlook.com, Microsoft 365) or google (Gmail; needs your own
        /// Google OAuth client, see docs/gmail.md)
        #[arg(long, value_enum, default_value_t = ProviderArg::Microsoft)]
        provider: ProviderArg,
        /// Where to keep the tokens (`keychain` = OS-native, `file` = plaintext JSON in pidge's config dir)
        #[arg(long, value_enum, default_value_t = StorageBackendArg::Keychain)]
        store: StorageBackendArg,
    },
    /// List signed-in mailboxes
    List,
    /// Remove a mailbox (sign out and delete its tokens)
    Remove {
        /// The mailbox (interactive picker when omitted and there are several)
        email: Option<String>,
        /// Remove every mailbox
        #[arg(long, conflicts_with = "email")]
        all: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Move a mailbox's tokens between the keychain and a file
    MigrateStorage {
        email: String,
        #[arg(long = "to", value_enum)]
        to: StorageBackendArg,
    },
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// Serve pidge's tools over stdio to a local MCP client (Claude Desktop, Cowork, Codex, ...)
    Serve,
    /// Sign in to a hosted pidge MCP server and move your local mailboxes onto it
    Connect {
        /// URL of the hosted server (e.g. https://<host>/mcp)
        url: String,
        /// Where to keep the session tokens
        #[arg(long, value_enum, default_value_t = StorageBackendArg::Keychain)]
        store: StorageBackendArg,
        /// No prompts; wait for each new mailbox to show as connected
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show the hosted connection
    Status {
        url: Option<String>,
        #[arg(long, value_enum)]
        store: Option<StorageBackendArg>,
    },
    /// Forget the stored session for a hosted server (local only)
    Logout {
        url: String,
        #[arg(long, value_enum)]
        store: Option<StorageBackendArg>,
    },
}

#[derive(Subcommand)]
pub enum AiCommands {
    /// Configure the AI provider and model interactively
    Config,
    /// Use the configured AI in pidge
    Enable,
    /// Stop using AI in pidge
    Disable,
    /// Show the AI status
    Status,
    /// Send a test message
    Test { message: Option<String> },
}

#[derive(Subcommand)]
pub enum OutboxCommands {
    /// Send due mail, then wait for the next held message until none is left
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderArg {
    Microsoft,
    Google,
}

/// CLI-facing wrapper around `pidge_core::TokenStorage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StorageBackendArg {
    /// OS-native credential store (macOS Keychain / Windows Credential Manager / Secret Service)
    Keychain,
    /// Plaintext JSON file (mode 0600 on Unix) in pidge's config dir
    File,
}

impl From<StorageBackendArg> for pidge_core::TokenStorage {
    fn from(v: StorageBackendArg) -> Self {
        match v {
            StorageBackendArg::Keychain => Self::Keychain,
            StorageBackendArg::File => Self::File,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}
