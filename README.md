<p align="center">
  <img src="https://raw.githubusercontent.com/mklab-se/pidge/main/media/pidge.png" alt="pidge" width="600">
</p>

<h1 align="center">pidge</h1>

<p align="center">
  A fast CLI for e-mail and calendar, designed to be operated by AI agents.
</p>

<p align="center">
  <a href="https://github.com/mklab-se/pidge/actions/workflows/ci.yml"><img src="https://github.com/mklab-se/pidge/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/pidge"><img src="https://img.shields.io/crates/v/pidge.svg" alt="crates.io"></a>
  <a href="https://github.com/mklab-se/pidge/releases/latest"><img src="https://img.shields.io/github/v/release/mklab-se/pidge" alt="GitHub Release"></a>
  <a href="https://github.com/mklab-se/homebrew-tap/blob/main/Formula/pidge.rb"><img src="https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmklab-se%2Fhomebrew-tap%2Fmain%2FFormula%2Fpidge.rb&search=%5Cd%2B%5C.%5Cd%2B%5C.%5Cd%2B&label=homebrew&prefix=v&color=orange" alt="Homebrew"></a>
  <a href="https://github.com/mklab-se/pidge/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/pidge.svg" alt="License"></a>
</p>

## Status

**Early days.** pidge can sign in to one or more Microsoft 365 / personal Microsoft accounts, browse / search / send / reply to e-mail, manage drafts + attachments, and **manage calendars** — create / edit / search / reschedule / cancel / duplicate events, move between calendars, RSVP, and create recurring meetings.

## Quick Start

```bash
# 1. Install (macOS / Linux)
brew install mklab-se/tap/pidge
# ...or via cargo: cargo install pidge
# ...other options (pre-built binaries, cargo binstall): see INSTALL.md

# 2. Sign in (interactive device code flow)
pidge account add

# 3. Try it
pidge mail
pidge calendar
```

See [INSTALL.md](INSTALL.md) for all installation methods and shell completions.

## Built for AI agents

pidge is designed to be operated by AI coding agents — Claude Code, Codex, Copilot, etc. — on your behalf. You can run it directly too, but the primary surface is your agent.

Wire it into your agent with one command:

```bash
pidge ai skill --emit > ~/.claude/skills/pidge/SKILL.md
# or, if you run pidge from a source checkout instead of `cargo install`:
pidge ai skill --emit --from-source > ~/.claude/skills/pidge/SKILL.md
```

The emitted skill is deliberately small: it teaches the agent the concept plus a handful of patterns (JSON output, confirmation gates, account context) and tells it to use `pidge --help` for live command discovery — so the skill keeps working as pidge ships new functionality.

## Account setup

```bash
# Add an account (interactive device code flow)
pidge account add

# List signed-in accounts and which one is default for e-mail / calendar
pidge account list

# Remove an account (interactive picker if more than one is signed in)
pidge account remove
```

The first account you add becomes the default for both e-mail and calendar; change either with:

```bash
pidge account default e-mail   <email>
pidge account default calendar <email>
pidge account default              # prints both currents
```

Sign in to multiple accounts and pidge merges reads across all of them by default.

## Reading the inbox

```bash
# Shortcut: list 25 most recent across every signed-in account
pidge mail

# Shortcut: open a specific message by a fragment of its 8-char ID
pidge mail 3515

# Explicit forms
pidge mail list --account kristofer@mklab.se --unread -n 50
pidge mail show 3515 --mark-read

# Pipe to scripts
pidge mail --json | jq '.[].subject'
```

## Calendar

```bash
# Shortcut: list events for today + next 7 days across every account
pidge calendar

# Canned windows
pidge calendar --today
pidge calendar --tomorrow
pidge calendar --week
pidge calendar --month

# Open one event by fragment of its 8-char hash
pidge calendar 4cabda75

# Schedule a meeting with attendees and a Teams URL
pidge calendar new \
  --title "Q3 planning" \
  --start "tomorrow 15:00" --end "+90m" \
  --invite alice@example.com,bob@example.com \
  --location "Office" --online

# Recurring weekly team sync
pidge calendar new \
  --title "Team sync" \
  --start "next mon 09:00" --end "+30m" \
  --repeat weekly --on mon --until 2026-12-31

# Reschedule
pidge calendar move-time 4cabda75 --start "fri 14:00"

# Cancel (organizer-only; sends notices to attendees)
pidge calendar cancel 4cabda75 --comment "Postponed to next week"

# RSVP to someone else's invite
pidge calendar rsvp 4cabda75 --accept

# Pipe to scripts
pidge calendar --json --week | jq '.[] | .subject'
```

## AI Integration

pidge delegates AI configuration to [ailloy](https://github.com/mklab-se/ailloy), a unified AI provider library shared by the MKLab CLI suite (`rigg`, `mdeck`, `cosq`, `pidge`). Configure your provider once:

```bash
pidge ai config    # configure your AI provider
pidge ai status    # check status
```

and it's available to every ailloy-based tool.

## Development

```bash
cargo build              # Build
cargo test --workspace   # Run tests
cargo clippy             # Lint
cargo fmt                # Format
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contributor guide.

## Changelog

Curious what's new? See [CHANGELOG.md](CHANGELOG.md) for the full history of
changes, newest first.

## License

MIT — see [LICENSE](LICENSE).
