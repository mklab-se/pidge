//! AI agent skill information for pidge
//!
//! Provides skill file content and reference documentation to help
//! AI coding agents (like Claude Code) work effectively with pidge.

const REFERENCE_DOC: &str = include_str!("../../doc/ai-reference.md");

/// Run the `pidge ai skill` command.
///
/// - No flags: print a human-readable setup guide.
/// - `--emit`: print a Claude Code skill markdown file to stdout.
/// - `--reference`: print comprehensive reference documentation.
pub fn run(emit: bool, reference: bool) {
    if emit {
        print_skill_file();
    } else if reference {
        print_reference();
    } else {
        print_guide();
    }
}

fn print_guide() {
    println!(
        r#"pidge AI Skill Setup
====================

pidge is a CLI for e-mail and calendar. This is a foundation release —
feature commands ship later. The emitted skill explains today's surface
and points the agent at the live reference doc.

To create the skill file, run:

  pidge ai skill --emit > ~/.claude/skills/pidge.md

Or ask your AI agent:

  "Use `pidge ai skill --emit` to set up a skill for pidge"

The skill instructs the AI agent to run `pidge ai skill --reference` at
runtime to fetch full documentation, so the agent always has up-to-date
command details without bloating the skill file itself.
"#
    );
}

fn print_skill_file() {
    print!(
        r#"---
name: pidge
description: A fast CLI for e-mail and calendar. Foundation release — AI configuration, shell completions, and version commands only.
---

# pidge — E-mail and Calendar CLI

pidge is in foundation phase. No e-mail or calendar feature commands ship yet.

## Before you start

Run this command to get full, up-to-date reference documentation:

```bash
pidge ai skill --reference
```

## Quick command reference

- `pidge ai status` — show AI configuration status
- `pidge ai test` — test the configured AI connection
- `pidge ai config` — interactive AI provider/model setup
- `pidge completion <shell>` — generate static shell completion script
- `pidge version` — print banner and version
"#
    );
}

fn print_reference() {
    print!("{REFERENCE_DOC}");
}
