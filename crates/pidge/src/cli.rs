//! CLI argument definitions using clap

use anyhow::Result;
use clap::Parser;

/// A fast CLI for e-mail and calendar
#[derive(Parser)]
#[command(name = "pidge")]
#[command(author, version, about)]
#[command(long_about = "A fast CLI for e-mail and calendar.\n\n\
    Manage one or more e-mail accounts and browse, search, send, and reply \
    to e-mail from your terminal.")]
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

    /// Manage e-mail accounts — add, remove, list, set defaults
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },

    /// Read, search, send, reply, forward, flag, archive, or delete e-mail
    Mail {
        #[command(subcommand)]
        command: MailCommands,
    },

    /// Read, create, edit, search, and manage calendar events
    Calendar {
        #[command(subcommand)]
        command: Option<CalendarCommands>,
    },

    /// Manage the trusted-senders list (auto-renders inline images from these senders)
    Trust {
        #[command(subcommand)]
        command: TrustCommands,
    },

    /// Local name → email index. Use `@name` in --invite / --to / --cc / --bcc
    /// to resolve against it.
    Contacts {
        #[command(subcommand)]
        command: ContactsCommands,
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
    /// AI agent skill information — emits a SKILL.md so AI agents (Claude
    /// Code, Codex, Copilot, etc.) can drive pidge on the user's behalf
    Skill {
        /// Output the SKILL.md content (ready to save as a skill file)
        #[arg(long)]
        emit: bool,

        /// When emitting, build a skill that invokes pidge via `cargo run`
        /// from the current working directory (the pidge source tree).
        /// Useful when you're developing against pidge from source instead
        /// of `cargo install pidge`. Implies --emit.
        #[arg(long)]
        from_source: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum AccountCommands {
    /// Add an e-mail account (interactive sign-in)
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
pub enum MailCommands {
    /// List messages in the inbox, merged across all signed-in accounts
    List {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// List a specific folder instead of the Inbox. Accepts a nested path
        /// like `Kvitton/MKLab` (matched case-insensitively).
        #[arg(long)]
        folder: Option<String>,

        /// Maximum number of messages to show per page
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// Page number (1-based). Skips `(page-1) * limit` messages.
        #[arg(short = 'p', long, default_value = "1")]
        page: usize,

        /// Show only unread messages
        #[arg(long)]
        unread: bool,

        /// Card layout with a single-line preview per message
        #[arg(short = 'c', long, conflicts_with_all = ["table", "full"])]
        compact: bool,

        /// Tabular layout (one row per message; useful for piping to other tools)
        #[arg(short = 't', long, conflicts_with = "full")]
        table: bool,

        /// Show the entire message body for each result instead of a preview
        #[arg(short = 'f', long)]
        full: bool,
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
        /// Search query (KQL syntax — `from:alice`, `subject:"q4 review"`, etc.)
        query: String,

        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,

        /// Maximum number of results
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,

        /// Card layout with a single-line preview per message
        #[arg(short = 'c', long, conflicts_with_all = ["table", "full"])]
        compact: bool,

        /// Tabular layout (one row per message; useful for piping to other tools)
        #[arg(short = 't', long, conflicts_with = "full")]
        table: bool,

        /// Show the entire message body for each result instead of a preview
        #[arg(short = 'f', long)]
        full: bool,
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

    /// Move a message to the Archive folder. Single or bulk.
    Archive {
        /// Fragment of a single message's 8-char short hash. Omit when
        /// using a bulk-mode flag like `--from` or `--older-than`.
        fragment: Option<String>,

        /// BULK: archive every Inbox message from this sender address
        /// (repeatable). Combine with `--older-than` to also constrain by
        /// date. Requires `-y`.
        #[arg(long, conflicts_with = "fragment")]
        from: Vec<String>,

        /// BULK: archive every Inbox message older than this date or
        /// duration (e.g. `2026-01-01`, `30d`, `6m`, `1y`). Can be combined
        /// with `--from` to filter further. Requires `-y`.
        #[arg(long, conflicts_with = "fragment")]
        older_than: Option<String>,

        /// Filter bulk archive to a specific account (repeatable). Ignored
        /// for single-fragment archives.
        #[arg(long)]
        account: Vec<String>,

        /// Grant required consent for bulk archives. Has no effect on
        /// single-fragment archives (they don't prompt anyway).
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Move a message into a folder, creating the folder if needed. Single or bulk.
    Move {
        /// Fragment of a single message's 8-char short hash. Omit when using
        /// a bulk-mode flag like `--from` or `--older-than`.
        fragment: Option<String>,

        /// Destination folder. Matched case-insensitively against existing
        /// folders; created if absent. Use `/` for nested folders, e.g.
        /// `--to "Kvitton/MKLab"` files under the `MKLab` child of `Kvitton`
        /// (both levels created as needed).
        #[arg(long)]
        to: String,

        /// BULK: move every message from this sender address across all
        /// folders (repeatable). Combine with `--older-than` to also
        /// constrain by date. Requires `-y`.
        #[arg(long, conflicts_with = "fragment")]
        from: Vec<String>,

        /// BULK: move every Inbox message older than this date or duration
        /// (e.g. `2026-01-01`, `30d`, `6m`, `1y`). Requires `-y`.
        #[arg(long, conflicts_with = "fragment")]
        older_than: Option<String>,

        /// Filter bulk move to a specific account (repeatable). Ignored for
        /// single-fragment moves.
        #[arg(long)]
        account: Vec<String>,

        /// Grant required consent for bulk moves. Has no effect on
        /// single-fragment moves (they don't prompt anyway).
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// List the mail folders in each account (name + message counts).
    Folders {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,
    },

    /// Create a folder (idempotent) in each account. Useful for setting up a
    /// consistent folder per account before sorting mail into it. Use `/` for
    /// nested folders, e.g. `mail mkdir "Kvitton/MKLab"`.
    Mkdir {
        /// Display name of the folder to create. `/` denotes nesting
        /// (`Kvitton/MKLab` creates `MKLab` under `Kvitton`).
        name: String,

        /// Create only in a specific account (repeatable). Defaults to all
        /// signed-in accounts.
        #[arg(long)]
        account: Vec<String>,
    },

    /// Delete a folder (its contents move to Deleted Items) in each account.
    /// Use `/` for nested folders. Requires `-y`.
    Rmdir {
        /// Folder to delete. `/` denotes nesting (`Kvitton/MKLab`).
        name: String,

        /// Delete only in a specific account (repeatable). Defaults to all
        /// signed-in accounts.
        #[arg(long)]
        account: Vec<String>,

        /// Confirm the deletion. Required — there is no interactive prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Delete a message (moves to Deleted Items folder). Single or bulk.
    Delete {
        /// Fragment of a single message's 8-char short hash. Omit when using
        /// a bulk-mode flag like `--from` or `--older-than`.
        fragment: Option<String>,

        /// BULK: delete every message from this sender address across all
        /// folders (repeatable). Combine with `--older-than` to also
        /// constrain by date. Requires `-y`.
        #[arg(long, conflicts_with = "fragment")]
        from: Vec<String>,

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

    /// Unsubscribe from the sender of a message using its RFC 2369
    /// `List-Unsubscribe` header (RFC 8058 one-click POST when offered;
    /// otherwise mailto; otherwise prints the URL).
    Unsubscribe {
        /// Fragment of the 8-char short hash
        fragment: String,

        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Compose a new e-mail (full-screen form; pass flags + `-y` to skip the form for scripting)
    New(ComposeArgs),

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

    /// List or download a message's file attachments
    Attachments {
        #[command(subcommand)]
        command: MailAttachmentCommands,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum MailAttachmentCommands {
    /// List the attachments on a message (name, size, type)
    List {
        /// Fragment of the 8-char short hash
        fragment: String,

        /// Also list inline attachments (embedded images, signature logos)
        #[arg(long)]
        include_inline: bool,
    },

    /// Download a message's attachments to disk
    Save {
        /// Fragment of the 8-char short hash
        fragment: String,

        /// Case-insensitive substring of a single attachment's filename.
        /// Omit to download every (non-inline) attachment.
        name: Option<String>,

        /// Destination. A directory (existing, or ending in a path separator)
        /// receives files under their original names; otherwise this is the
        /// target file path, valid only when one attachment is selected.
        /// Defaults to your Downloads folder.
        #[arg(short = 'o', long)]
        out: Option<std::path::PathBuf>,

        /// Also include inline attachments (embedded images, signature logos)
        #[arg(long)]
        include_inline: bool,

        /// Overwrite existing files instead of erroring
        #[arg(short = 'f', long)]
        force: bool,
    },
}

/// Flags shared between `mail new` and (mostly) the explicit forms of
/// reply/forward. All optional — the TUI form fills in what's missing.
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

    /// Open the TUI compose form pre-filled with your flags so you can
    /// review (and edit) before sending. Without this, a fully-specified
    /// invocation (`--to`, `--subject`, `--body`/`--body-file`) sends
    /// immediately — convenient for scripts and one-liners.
    #[arg(long)]
    pub confirm: bool,

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

/// Subcommand names that `Mail` accepts directly. Used by the argv pre-processor
/// in `main.rs` to decide whether `pidge mail <X>` should route to `mail show X`
/// (X is a fragment) or pass through to clap (X is a subcommand).
///
/// Keep this list in sync with [`MailCommands`]. When you add a new variant,
/// add its kebab-case name here too — otherwise users will see "No message
/// found for fragment '<new-subcommand>'" instead of the new behavior.
pub const MAIL_SUBCOMMAND_NAMES: &[&str] = &[
    "list",
    "show",
    "search",
    "mark-read",
    "mark-unread",
    "flag",
    "unflag",
    "archive",
    "move",
    "folders",
    "mkdir",
    "rmdir",
    "new",
    "reply",
    "reply-all",
    "forward",
    "delete",
    "unsubscribe",
    "attachments",
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
pub enum ContactsCommands {
    /// Rebuild the local index from inbox senders and calendar attendees.
    Refresh {
        /// Window of history to scan for both mail and calendar
        #[arg(long, default_value = "365")]
        days: i64,
        /// Limit refresh to a specific signed-in account (repeatable)
        #[arg(long)]
        account: Vec<String>,
    },
    /// Search the local index. Case-insensitive substring match on name,
    /// email, and local-part. Exact email match wins.
    Find {
        /// Query — substring of name, email, or local-part. Omit to list all.
        #[arg(default_value = "")]
        query: String,
        /// Maximum number of matches to print
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,
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

#[derive(clap::Subcommand)]
pub enum CalendarCommands {
    /// List upcoming events (default: today + next 7 days)
    List {
        /// Filter to a specific account (repeatable for a subset)
        #[arg(long)]
        account: Vec<String>,
        /// Window start (ISO date or natural string)
        #[arg(long)]
        from: Option<String>,
        /// Window end (ISO date or natural string)
        #[arg(long)]
        to: Option<String>,
        #[arg(long, conflicts_with_all = ["tomorrow", "week", "month"])]
        today: bool,
        #[arg(long, conflicts_with_all = ["today", "week", "month"])]
        tomorrow: bool,
        #[arg(long, conflicts_with_all = ["today", "tomorrow", "month"])]
        week: bool,
        #[arg(long, conflicts_with_all = ["today", "tomorrow", "week"])]
        month: bool,
        /// Restrict to a specific calendar (name or id)
        #[arg(long)]
        calendar: Option<String>,
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
        #[arg(short = 'c', long, conflicts_with = "table")]
        compact: bool,
        #[arg(short = 't', long)]
        table: bool,
    },

    /// Display a single event identified by a fragment of its short hash
    Show {
        /// Fragment of the 8-char short hash
        fragment: String,
    },

    /// Substring search across calendar events (case-insensitive, matches
    /// subject, body preview, location, organizer, and attendees).
    Search {
        /// Search query
        query: String,
        #[arg(long)]
        account: Vec<String>,
        #[arg(long)]
        calendar: Option<String>,
        /// Maximum number of matches to return after filtering
        #[arg(short = 'n', long, default_value = "25")]
        limit: usize,
        /// Earliest event start to consider (default: 365 days ago)
        #[arg(long)]
        from: Option<String>,
        /// Latest event start to consider (default: 365 days from now)
        #[arg(long)]
        to: Option<String>,
    },

    /// List calendars on each signed-in account
    Calendars {
        #[command(subcommand)]
        command: Option<CalendarsCommands>,
    },

    /// Create a new event
    New(CalendarNewArgs),

    /// Edit an existing event
    Edit {
        /// Fragment of the 8-char short hash
        fragment: String,
        #[command(flatten)]
        new: CalendarEditArgs,
    },

    /// Reschedule an event without editing other fields
    #[command(name = "move-time")]
    MoveTime {
        /// Fragment of the 8-char short hash
        fragment: String,
        /// New start time
        #[arg(long)]
        start: String,
        /// New end time (defaults to preserving the existing duration)
        #[arg(long)]
        end: Option<String>,
        #[arg(long, conflicts_with = "no_notify")]
        notify: bool,
        #[arg(long)]
        no_notify: bool,
        /// Apply to the whole recurring series instead of this occurrence
        #[arg(long)]
        series: bool,
    },

    /// Use an existing event as a template for a new one
    Duplicate {
        /// Fragment of the 8-char short hash
        fragment: String,
        /// Start time of the new event (default: 7 days after the original)
        #[arg(long)]
        start: Option<String>,
        /// Override the new event's title
        #[arg(long)]
        title: Option<String>,
        /// Create the new event in a different calendar
        #[arg(long)]
        calendar: Option<String>,
    },

    /// Delete an event silently (no attendee notification)
    Delete {
        /// Fragment of the 8-char short hash
        fragment: String,
        #[arg(short = 'y', long)]
        yes: bool,
        /// Delete the whole series instead of just this occurrence
        #[arg(long)]
        series: bool,
    },

    /// Cancel an event you organized (sends notices to all attendees)
    Cancel {
        /// Fragment of the 8-char short hash
        fragment: String,
        /// Cancellation comment included in the notification
        #[arg(long)]
        comment: Option<String>,
        #[arg(short = 'y', long)]
        yes: bool,
        /// Cancel the whole series instead of just this occurrence
        #[arg(long)]
        series: bool,
    },

    /// Move an event to a different calendar
    Move {
        /// Fragment of the 8-char short hash
        fragment: String,
        /// Destination calendar (name or id)
        #[arg(long = "to")]
        to: String,
    },

    /// Respond to a meeting invitation
    Rsvp {
        /// Fragment of the 8-char short hash
        fragment: String,
        #[arg(long, conflicts_with_all = ["tentative", "decline"])]
        accept: bool,
        #[arg(long, conflicts_with_all = ["accept", "decline"])]
        tentative: bool,
        #[arg(long, conflicts_with_all = ["accept", "tentative"])]
        decline: bool,
        /// Optional comment included with the response
        #[arg(long)]
        comment: Option<String>,
        /// Don't email the organizer with the response
        #[arg(long)]
        no_notify: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum CalendarsCommands {
    /// List calendars (default)
    List,
}

#[derive(clap::Args, Debug, Clone, Default)]
pub struct CalendarNewArgs {
    /// Account to create the event on (defaults to `account default calendar`)
    #[arg(long)]
    pub from: Option<String>,
    /// Event title
    #[arg(long)]
    pub title: Option<String>,
    /// Start time
    #[arg(long)]
    pub start: Option<String>,
    /// End time (defaults to start + 1 hour)
    #[arg(long)]
    pub end: Option<String>,
    /// All-day event (start/end are dates)
    #[arg(long)]
    pub all_day: bool,
    /// Location (free text)
    #[arg(long)]
    pub location: Option<String>,
    /// Body text
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read body from a file (`-` reads from stdin)
    #[arg(long)]
    pub body_file: Option<String>,
    /// Required attendees (comma-separated, repeatable)
    #[arg(long, value_delimiter = ',')]
    pub invite: Vec<String>,
    /// Optional attendees (comma-separated, repeatable)
    #[arg(long = "invite-optional", value_delimiter = ',')]
    pub invite_optional: Vec<String>,
    /// Recurrence frequency: daily | weekly | monthly | yearly
    #[arg(long)]
    pub repeat: Option<String>,
    /// Weekdays (weekly only): mon,tue,wed,thu,fri,sat,sun
    #[arg(long, value_delimiter = ',')]
    pub on: Vec<String>,
    /// Stop repeating on this date (YYYY-MM-DD)
    #[arg(long, conflicts_with = "count")]
    pub until: Option<String>,
    /// Stop after N occurrences
    #[arg(long)]
    pub count: Option<u32>,
    /// Recurrence interval (1 = every freq, 2 = every other, …)
    #[arg(long, default_value_t = 1)]
    pub interval: u32,
    /// Add a Microsoft Teams meeting URL
    #[arg(long)]
    pub online: bool,
    /// Create in a non-default calendar (name or id)
    #[arg(long)]
    pub calendar: Option<String>,
    /// Override the local time zone for input/display (IANA)
    #[arg(long)]
    pub tz: Option<String>,
    /// Open the TUI compose form pre-filled with your flags
    #[arg(long)]
    pub confirm: bool,
    /// Skip the final confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(clap::Args, Debug, Clone, Default)]
pub struct CalendarEditArgs {
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub start: Option<String>,
    #[arg(long)]
    pub end: Option<String>,
    #[arg(long)]
    pub location: Option<String>,
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    #[arg(long)]
    pub body_file: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub invite: Vec<String>,
    #[arg(long = "invite-optional", value_delimiter = ',')]
    pub invite_optional: Vec<String>,
    #[arg(long, conflicts_with = "no_notify")]
    pub notify: bool,
    #[arg(long)]
    pub no_notify: bool,
    /// Apply to the whole recurring series instead of this occurrence
    #[arg(long)]
    pub series: bool,
    #[arg(long)]
    pub tz: Option<String>,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Calendar subcommand names recognized by the arg-preprocessor in main.rs.
/// Keep in sync with [`CalendarCommands`].
pub const CALENDAR_SUBCOMMAND_NAMES: &[&str] = &[
    "list",
    "show",
    "search",
    "calendars",
    "new",
    "edit",
    "move-time",
    "duplicate",
    "delete",
    "cancel",
    "move",
    "rsvp",
    "help",
];

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Some(Commands::Ai { command }) => crate::commands::ai::run(command).await,
            Some(Commands::Account { command }) => {
                crate::commands::account::run(command, self.json).await
            }
            Some(Commands::Mail { command }) => {
                crate::commands::mail::run(command, self.json).await
            }
            Some(Commands::Calendar { command }) => {
                crate::commands::calendar::run(command, self.json).await
            }
            Some(Commands::Trust { command }) => {
                crate::commands::trust::run(command, self.json).await
            }
            Some(Commands::Contacts { command }) => {
                crate::commands::contacts::run(command, self.json).await
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
