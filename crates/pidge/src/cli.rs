//! CLI argument definitions using clap

use anyhow::Result;
use clap::Parser;

/// A fast CLI for e-mail and calendar
#[derive(Parser)]
#[command(name = "pidge")]
#[command(author, version, about)]
#[command(long_about = "A fast CLI for e-mail and calendar.\n\n\
    Manage one or more Microsoft 365 accounts and browse, search, send, and \
    reply to e-mail from your terminal.")]
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

    /// Manage Microsoft 365 accounts — add, remove, list, set defaults
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },

    /// View, search and (soon) send messages in your inbox
    Inbox {
        #[command(subcommand)]
        command: InboxCommands,
    },

    /// Manage the trusted-senders list (auto-renders inline images from these senders)
    Trust {
        #[command(subcommand)]
        command: TrustCommands,
    },

    /// List, edit, send, or delete draft e-mails
    Drafts {
        #[command(subcommand)]
        command: DraftsCommands,
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
pub enum AccountCommands {
    /// Add a Microsoft account (interactive device-code sign-in)
    Add {
        /// Where to store credentials (`keychain` = OS-native, `file` = plaintext JSON at ~/.config/pidge/tokens/)
        #[arg(long, value_enum, default_value_t = StorageBackendArg::Keychain)]
        store: StorageBackendArg,
    },
    /// List signed-in accounts with default-account markers
    List,
    /// Remove an account (sign out and delete its tokens)
    Remove {
        /// E-mail of the account to remove (interactive picker if omitted and multiple accounts exist)
        email: Option<String>,
        /// Remove every signed-in account
        #[arg(long, conflicts_with = "email")]
        all: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show or set default accounts. Without a subcommand, prints both current defaults.
    Default {
        #[command(subcommand)]
        command: Option<DefaultCommands>,
    },
    /// Move an existing account's credentials between storage backends
    MigrateStorage {
        /// E-mail of the account whose tokens to migrate
        email: String,
        /// Destination backend
        #[arg(long = "to", value_enum)]
        to: StorageBackendArg,
    },
}

#[derive(clap::Subcommand)]
pub enum DefaultCommands {
    /// Set the default account used for sending and reading e-mail
    #[command(name = "e-mail")]
    EMail {
        /// E-mail address of a signed-in account
        email: String,
    },
    /// Set the default account used for calendar events and meeting invitations
    Calendar {
        /// E-mail address of a signed-in account
        email: String,
    },
}

/// CLI-facing wrapper around `pidge_core::TokenStorage`. Lives in the CLI crate
/// so `pidge-core` stays free of `clap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StorageBackendArg {
    /// OS-native credential store (macOS Keychain / Windows Credential Manager / libsecret)
    Keychain,
    /// Plaintext JSON file at `~/.config/pidge/tokens/<email>.json` (mode 0600 on Unix)
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

#[derive(clap::Subcommand)]
pub enum InboxCommands {
    /// List messages in the inbox, merged across all signed-in accounts
    List {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// Maximum number of messages to show per page
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// Page number (1-based). Skips `(page-1) * limit` messages.
        #[arg(short = 'p', long, default_value = "1")]
        page: usize,

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

        /// Print only the raw HTML body (or plain text, if the message has no HTML).
        /// Useful for capturing a fixture to anonymize and add as a render-test case.
        #[arg(long, hide = true)]
        raw_html: bool,
    },

    /// Search e-mails using Graph's KQL `$search` syntax (e.g. `from:alice subject:budget`)
    Search {
        /// Search query (passed to Microsoft Graph `$search`)
        query: String,

        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// Maximum number of results
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// One row per message (no preview lines)
        #[arg(short = 'c', long)]
        compact: bool,
    },

    /// Mark a message as read
    #[command(name = "mark-read")]
    MarkRead {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Mark a message as unread
    #[command(name = "mark-unread")]
    MarkUnread {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Set the follow-up flag on a message
    Flag {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Clear the follow-up flag on a message
    Unflag {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Move a message to the Archive folder
    Archive {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Delete a message (moves to Deleted Items folder). Single or bulk.
    Delete {
        /// Fragment of a single message's 8-char short hash. Omit when using
        /// a bulk-mode flag like `--older-than`.
        fragment: Option<String>,

        /// BULK: delete every message in the Inbox older than this date or
        /// duration (e.g. `2026-01-01`, `30d`, `6m`, `1y`). Always requires
        /// `-y` to confirm — there is no interactive prompt for bulk delete.
        #[arg(long, conflicts_with = "fragment")]
        older_than: Option<String>,

        /// Filter bulk delete to a specific account (repeatable). Ignored
        /// for single-fragment deletes.
        #[arg(long)]
        account: Vec<String>,

        /// Skip the "Delete? [y/N]" confirmation (single) or grant required
        /// consent for bulk deletes.
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Compose and send a new e-mail (wizard by default; provide flags to skip prompts)
    Send(ComposeArgs),

    /// Reply to a message (the original sender only)
    Reply {
        /// Fragment of the 8-char short hash
        fragment: String,
        #[command(flatten)]
        compose: ReplyArgs,
    },

    /// Reply-all to a message (every recipient on the thread, excluding yourself)
    #[command(name = "reply-all")]
    ReplyAll {
        /// Fragment of the 8-char short hash
        fragment: String,
        #[command(flatten)]
        compose: ReplyArgs,
    },

    /// Forward a message to new recipients
    Forward {
        /// Fragment of the 8-char short hash
        fragment: String,
        #[command(flatten)]
        compose: ForwardArgs,
    },
}

/// Flags shared between `inbox send` and (mostly) the explicit forms of
/// reply/forward. All optional — wizard prompts fill in what's missing.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ComposeArgs {
    /// Account to send from (defaults to `account default e-mail`)
    #[arg(long)]
    pub from: Option<String>,

    /// Recipient e-mail addresses (comma-separated, repeatable)
    #[arg(long, value_delimiter = ',')]
    pub to: Vec<String>,

    /// Cc recipients (comma-separated, repeatable)
    #[arg(long, value_delimiter = ',')]
    pub cc: Vec<String>,

    /// Bcc recipients (comma-separated, repeatable)
    #[arg(long, value_delimiter = ',')]
    pub bcc: Vec<String>,

    /// Subject line
    #[arg(long)]
    pub subject: Option<String>,

    /// Body text (use `--body-file` for long content; both flags are mutually exclusive)
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,

    /// Read body from a file (`-` reads from stdin)
    #[arg(long)]
    pub body_file: Option<String>,

    /// Skip the final "Send? [y/N]" confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Save as a draft instead of sending. The new draft's short hash is
    /// printed; use `pidge drafts edit`, `pidge drafts send`, or
    /// `pidge drafts delete` to act on it later.
    #[arg(long)]
    pub draft: bool,

    /// Attach a file (repeatable). Each file must be < 3 MB — larger
    /// attachments require resumable uploads, not yet implemented.
    #[arg(long)]
    pub attach: Vec<std::path::PathBuf>,
}

/// Reply variants don't need `--to` (Graph fills that in from the original
/// message) and don't take a subject (Graph prepends "Re:" automatically),
/// but they DO need a body comment.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ReplyArgs {
    /// Account to reply from (defaults to the account that received the message)
    #[arg(long)]
    pub from: Option<String>,

    /// Comment text to prepend to Graph's auto-quoted original
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,

    /// Read comment from a file (`-` reads from stdin)
    #[arg(long)]
    pub body_file: Option<String>,

    /// Skip the final "Send? [y/N]" confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Save as a draft instead of sending. The new draft's short hash is
    /// printed; use `pidge drafts edit`, `pidge drafts send`, or
    /// `pidge drafts delete` to act on it later.
    #[arg(long)]
    pub draft: bool,

    /// Attach a file (repeatable). Each file must be < 3 MB — larger
    /// attachments require resumable uploads, not yet implemented.
    #[arg(long)]
    pub attach: Vec<std::path::PathBuf>,
}

/// Forward needs explicit recipients (the user is sending the message to
/// someone new), plus an optional comment.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ForwardArgs {
    /// Account to forward from (defaults to the account that received the message)
    #[arg(long)]
    pub from: Option<String>,

    /// Recipient e-mail addresses (comma-separated, repeatable)
    #[arg(long, value_delimiter = ',')]
    pub to: Vec<String>,

    /// Comment text to prepend to Graph's auto-quoted original
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,

    /// Read comment from a file (`-` reads from stdin)
    #[arg(long)]
    pub body_file: Option<String>,

    /// Skip the final "Send? [y/N]" confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Save as a draft instead of sending. The new draft's short hash is
    /// printed; use `pidge drafts edit`, `pidge drafts send`, or
    /// `pidge drafts delete` to act on it later.
    #[arg(long)]
    pub draft: bool,

    /// Attach a file (repeatable). Each file must be < 3 MB — larger
    /// attachments require resumable uploads, not yet implemented.
    #[arg(long)]
    pub attach: Vec<std::path::PathBuf>,
}

/// Subcommand names that `Inbox` accepts directly. Used by the argv pre-processor
/// in `main.rs` to decide whether `pidge inbox <X>` should route to `inbox show X`
/// (X is a fragment) or pass through to clap (X is a subcommand).
///
/// Keep this list in sync with [`InboxCommands`]. When you add a new variant,
/// add its kebab-case name here too — otherwise users will see "No message
/// found for fragment '<new-subcommand>'" instead of the new behavior.
pub const INBOX_SUBCOMMAND_NAMES: &[&str] = &[
    "list",
    "show",
    "search",
    "mark-read",
    "mark-unread",
    "flag",
    "unflag",
    "archive",
    "send",
    "reply",
    "reply-all",
    "forward",
    "delete",
    "help",
];

#[derive(clap::Subcommand)]
pub enum DraftsCommands {
    /// List drafts across all signed-in accounts (or a filtered subset)
    List {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// Maximum number of drafts to show per page
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// Page number (1-based)
        #[arg(short = 'p', long, default_value = "1")]
        page: usize,

        /// One row per draft (no preview lines)
        #[arg(short = 'c', long)]
        compact: bool,
    },

    /// Display a draft by fragment of its short hash
    Show {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Open the wizard pre-filled with a draft's current values, then save
    Edit {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Send a draft as-is (interactive confirmation; -y to skip)
    Send {
        /// Fragment of the 8-char short hash
        fragment: String,

        /// Skip the "Send draft? [y/N]" confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Delete a draft (moves it to Deleted Items)
    Delete {
        /// Fragment of the 8-char short hash
        fragment: String,

        /// Skip the "Delete draft? [y/N]" confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Manage a draft's attachments
    Attachments {
        #[command(subcommand)]
        command: DraftAttachmentCommands,
    },
}

#[derive(clap::Subcommand)]
pub enum DraftAttachmentCommands {
    /// List the attachments currently on a draft
    List {
        /// Fragment of the draft's 8-char short hash
        fragment: String,
    },
    /// Attach a file to a draft (size limit: 3 MB)
    Add {
        /// Fragment of the draft's 8-char short hash
        fragment: String,
        /// Path to the file to attach
        path: std::path::PathBuf,
    },
    /// Remove an attachment from a draft by name (case-insensitive)
    Remove {
        /// Fragment of the draft's 8-char short hash
        fragment: String,
        /// Attachment filename
        name: String,
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
            Some(Commands::Account { command }) => {
                crate::commands::account::run(command, self.json).await
            }
            Some(Commands::Inbox { command }) => {
                crate::commands::inbox::run(command, self.json).await
            }
            Some(Commands::Trust { command }) => {
                crate::commands::trust::run(command, self.json).await
            }
            Some(Commands::Drafts { command }) => {
                crate::commands::drafts::run(command, self.json).await
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
