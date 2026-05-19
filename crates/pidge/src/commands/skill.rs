//! AI agent skill emission.
//!
//! `pidge ai skill --emit` prints a SKILL.md body that AI coding agents
//! (Claude Code, Codex, Copilot, …) can load to drive pidge on the user's
//! behalf. pidge is **designed to be operated by AI agents** — humans can
//! invoke it too, but the primary surface is yours.
//!
//! The emitted skill teaches the *concept* of pidge plus the patterns the
//! agent needs (JSON output, confirmation gates, account context) and then
//! tells the agent to discover the exact command surface at runtime via
//! `pidge --help` / `pidge <subcommand> --help`. That way the skill keeps
//! working as new commands and flags ship — no churn on every release.
//!
//! Two modes:
//!
//! - default: invocation prefix is `pidge` (assumes `cargo install pidge`
//!   put the binary on `$PATH`)
//! - `--from-source`: invocation prefix is `(cd <CWD> && cargo run --quiet --)`
//!   so the agent runs pidge straight from the source tree

use anyhow::{Context, Result};

/// Run the `pidge ai skill` command.
///
/// - No flags: print a short human-readable setup guide.
/// - `--emit`: print SKILL.md to stdout (assumes `pidge` is on $PATH).
/// - `--emit --from-source` or `--from-source` alone: print SKILL.md whose
///   invocation prefix runs pidge via `cargo run` from the current working
///   directory.
pub fn run(emit: bool, from_source: bool) -> Result<()> {
    // `--from-source` implies `--emit` — the flag only makes sense with emit.
    if emit || from_source {
        let prefix = if from_source {
            source_invocation_prefix()?
        } else {
            CommandPrefix::installed()
        };
        print_skill(&prefix);
    } else {
        print_guide();
    }
    Ok(())
}

/// Build a source-based invocation prefix anchored at the current working
/// directory. We use the user's CWD because `--from-source` is meaningful
/// only when the user is sitting in their pidge checkout — that's the path
/// they want the skill to reference.
fn source_invocation_prefix() -> Result<CommandPrefix> {
    let cwd = std::env::current_dir().context("could not resolve current working directory")?;
    Ok(CommandPrefix::from_source(
        cwd.to_string_lossy().into_owned(),
    ))
}

/// How an agent invokes pidge. Captured as a value so the SKILL.md template
/// can stay one piece of text with the prefix substituted in at the call
/// sites the agent will actually copy-paste.
struct CommandPrefix {
    /// What the agent types in place of `pidge` (e.g. `pidge` or
    /// `(cd /path && cargo run --quiet --)`).
    invocation: String,
    /// Where pidge is coming from — used in the "Setup notes" section so
    /// the agent knows whether this is an installed or in-tree pidge.
    setup_note: String,
    /// "Keeping this skill fresh" body — differs between installed (no
    /// source to edit) and source (source path is known and editable).
    evolution_section: String,
}

impl CommandPrefix {
    fn installed() -> Self {
        Self {
            invocation: "pidge".to_string(),
            setup_note: "This pidge skill targets an installed binary on `$PATH` \
                 (typically from `cargo install pidge`). If `pidge` is not \
                 found, ask the user to install it."
                .to_string(),
            evolution_section: "\
This skill is designed to evolve. If you discover a better pattern, a \
missing flag, or a workflow that should be documented, edit this SKILL.md \
directly — future invocations will benefit immediately.

When the friction is in pidge's behavior rather than the skill's \
documentation (a missing command, a confusing error, a feature gap), \
surface it to the user with a concrete suggestion. They can either file an \
issue at https://github.com/mklab-se/pidge/issues or upgrade their \
installation with `cargo install pidge --force` once the fix ships."
                .to_string(),
        }
    }

    fn from_source(source_dir: String) -> Self {
        // The subshell parens isolate the `cd` so it doesn't bleed into
        // subsequent commands the agent runs.
        let invocation = format!("(cd {source_dir} && cargo run --quiet --)");
        let evolution_section = format!(
            "\
This skill is designed to evolve. You can improve it in two ways:

1. **Edit this SKILL.md directly.** When you discover a better pattern, a \
missing flag, or a workflow that should be documented, update this file. \
Future invocations will benefit immediately.

2. **Improve pidge itself.** When the friction is in pidge's behavior \
rather than the skill's documentation — a missing command, a confusing \
error, a feature gap — fix it in the source tree at `{source_dir}`.

   - If your current working directory is already inside `{source_dir}`, \
go ahead and make the change.
   - If you are working in a DIFFERENT repository, **ASK THE USER before \
modifying pidge's source.** They may not want pidge changed for unrelated \
work. If they decline the pidge change, ask whether updating this skill \
instead would address the issue — the documentation fix may be enough \
even without a behavior change in pidge."
        );
        let setup_note = format!(
            "This pidge skill runs pidge directly from a source checkout at \
             `{source_dir}`. Every command is wrapped in a subshell that `cd`s \
             into the source tree and invokes `cargo run --quiet --`. If the \
             path is gone or moved, ask the user where their pidge checkout \
             lives or regenerate the skill with `pidge ai skill --emit \
             --from-source`."
        );
        Self {
            invocation,
            setup_note,
            evolution_section,
        }
    }
}

fn print_guide() {
    println!(
        "\
pidge AI Skill Setup
====================

pidge is a CLI for e-mail (and, soon, calendar) operating against Microsoft
365 / Outlook mailboxes via the Graph API. It is **designed to be operated
by AI coding agents** — Claude Code, Codex, Copilot, etc. — on the user's
behalf. A human can use it directly, but the primary surface is your agent.

To wire pidge into your agent, emit the skill body and save it where the
agent loads skills from. For Claude Code that's usually
`~/.claude/skills/pidge/SKILL.md`:

  pidge ai skill --emit > ~/.claude/skills/pidge/SKILL.md

If you run pidge from source (no `cargo install`), add `--from-source` so
the emitted skill invokes pidge via `cargo run` from your checkout:

  pidge ai skill --emit --from-source > ~/.claude/skills/pidge/SKILL.md

The emitted skill is deliberately small: it teaches the agent the *concept*
plus a handful of patterns (JSON output, confirmation gates, account
context), and tells it to run `pidge --help` and `pidge <subcommand> --help`
to discover the live command surface. That way the skill keeps working as
pidge ships new commands.
"
    );
}

fn print_skill(prefix: &CommandPrefix) {
    let invoke = &prefix.invocation;
    let setup = &prefix.setup_note;
    let evolution = &prefix.evolution_section;
    print!(
        "\
---
name: pidge
description: Operate Microsoft 365 / Outlook mailboxes via the pidge CLI — \
list, search, read, send, reply, forward, flag, archive, and delete e-mail; \
manage drafts and attachments. Use whenever the user mentions their e-mail, \
inbox, outlook, m365, mailbox, drafts, or messages.
---

# pidge — E-mail (and Calendar) CLI

pidge is a fast CLI for Microsoft 365 / Outlook mailboxes built on the
Microsoft Graph API. **It is designed to be operated by you — the AI
agent — on behalf of the user.** Humans can run pidge directly too, but
the primary interaction model is: the user asks you to do something with
their e-mail, and you drive pidge.

## Invoking pidge

Every command in this skill uses the invocation prefix:

```
{invoke}
```

Whenever the skill shows `<prefix>`, substitute that string. Examples:

```
{invoke} mail
{invoke} mail show 4cabda75
{invoke} account list
```

{setup}

## Discover commands at runtime

This skill intentionally does NOT enumerate every command and flag —
pidge ships new functionality regularly, and an enumerated list would
drift out of date. Discover the live surface with `--help`:

```
{invoke} --help                  # top-level subcommands
{invoke} mail --help             # mail subcommands
{invoke} mail list --help        # flags for a specific command
{invoke} account --help          # account subcommands
{invoke} drafts --help           # draft subcommands
```

Read these before assuming a command or flag exists. The help output is
authoritative.

## Parsing output: always prefer --json

Most commands that produce a list or a result accept `--json`. Use it
whenever you need to act on the output (filter, look something up by
field, drive a follow-up command). Example:

```
{invoke} mail --json -n 10
{invoke} mail search \"from:alice budget\" --json
{invoke} account list --json
```

The JSON shapes are stable. The default human card layout (with ANSI
colors and 8-line previews) is for the user's screen; don't parse it.

## Identifying messages

List/search output gives each message an 8-character short hash
(`efa07329`). Use that hash in follow-up commands — never paste full
Graph IDs.

```
{invoke} mail show efa07329
{invoke} mail reply efa07329 --body \"thanks\" -y
{invoke} mail delete efa07329 -y
```

The hash is resolved against a local cache populated by the most recent
list/search. If a hash isn't recognized, run a fresh `mail` or `mail
search` first.

## Confirmation gates

Destructive operations (delete, send, bulk delete) prompt the user by
default. Use `-y` / `--yes` to skip the prompt — but only after the
**user has explicitly confirmed the intent**.

Rule of thumb:

- The user asked you to send an e-mail with full content (to / subject /
  body all specified) → send with `-y` directly. They've already
  authored it.
- The user asked you to delete something → confirm with them in chat
  first, then run with `-y`.
- The user asked for a bulk operation → always confirm first; bulk
  delete *requires* `-y` even after the prompt.

Never inflate the user's confirmation. \"Yes\" to draft a message is not
\"yes\" to send it; \"yes\" to delete one message is not \"yes\" to delete
many.

## Account context

Most users have one signed-in account; some have several (work + personal).

```
{invoke} account list                    # which accounts are signed in
{invoke} account list --json             # parseable form with `is_default_email`
```

If the user has multiple accounts, ask which one to act on (or use
`--account <email>` on list/search). For sending, the default account is
used unless `--from <email>` is given.

If `account list` is empty, sign-in requires opening a browser — ask the
user to run `{invoke} account add` themselves (don't try to drive an
interactive browser flow from your terminal).

## Common workflows

These are sketches, not contracts. Always check `--help` for exact flags.

- **Read the inbox**: `{invoke} mail` (default), or `{invoke} mail --full`
  to see the entire body inline, or `{invoke} mail --compact` for a
  one-line-per-message view.
- **Find a specific message**: `{invoke} mail search \"<KQL query>\" --json`
  then `{invoke} mail show <hash>`.
- **Send a new e-mail**: `{invoke} mail new --to user@example.com
  --subject \"...\" --body \"...\"` (sends immediately when all fields are
  given; pass `--draft` to save instead of send; `--confirm` to open a
  full-screen TUI form for the user to review).
- **Reply / forward**: `{invoke} mail reply <hash> --body \"...\" -y`,
  `{invoke} mail forward <hash> --to user@example.com --body \"...\" -y`.
- **Manage state**: `mail flag`, `mail unflag`, `mail mark-read`,
  `mail mark-unread`, `mail archive`.
- **Unsubscribe from a sender**: `{invoke} mail unsubscribe <hash> -y`.
  Reads the message's RFC 2369 `List-Unsubscribe` header and acts:
  RFC 8058 one-click HTTPS POST when the sender offers it, otherwise a
  `mailto:` send from the receiving account, otherwise prints the URL
  for a manual browser click. Use this when the user asks to
  unsubscribe from a sender or to stop e-mails from someone.

  **Default follow-up:** users who ask to unsubscribe almost always
  want the triggering message gone too. After a successful
  `mail unsubscribe ... -y`, immediately run `mail delete <same-hash>
  -y` on the same message without a separate prompt. Then *offer* (do
  not auto-execute) to sweep the backlog from that sender — search
  `from:<sender>`, summarise, ask, then bulk-delete on confirmation.
  Invoice/receipt senders are still preserved per the invoice rule —
  if any hits look like receipts, surface and skip them.

## Error handling

- `session expired` → the user's tokens are gone. Ask them to re-run
  `{invoke} account add` themselves.
- `No accounts signed in` → same; they need to sign in first.
- Any other Graph error → surface the message verbatim. pidge tries to
  return actionable text.

## What pidge will NOT do for you

- pidge does not yet support calendar (`pidge calendar` doesn't exist).
- pidge does not yet expose itself as an MCP server. The supported
  integration today is via this skill.
- pidge does not store or summarize e-mail content for you; you read and
  reason over it yourself using `mail show` / `mail --json`.

## Keeping this skill fresh

{evolution}
"
    );
}
