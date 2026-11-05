<p align="center">
  <img src="https://raw.githubusercontent.com/mklab-se/pidge/main/media/pidge.png" alt="pidge" width="600">
</p>

<h1 align="center">pidge</h1>

<p align="center">
  <strong>The e-mail and calendar client for your AI agent.</strong><br>
  Let Claude Code, Codex, Copilot, Claude.ai or ChatGPT work your real inbox and calendar, safely.<br>
  Microsoft 365, Outlook.com and Gmail.
</p>

<p align="center">
  <a href="https://github.com/mklab-se/pidge/actions/workflows/ci.yml"><img src="https://github.com/mklab-se/pidge/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/pidge"><img src="https://img.shields.io/crates/v/pidge.svg" alt="crates.io"></a>
  <a href="https://github.com/mklab-se/pidge/releases/latest"><img src="https://img.shields.io/github/v/release/mklab-se/pidge" alt="GitHub Release"></a>
  <a href="https://github.com/mklab-se/homebrew-tap/blob/main/Formula/pidge.rb"><img src="https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmklab-se%2Fhomebrew-tap%2Fmain%2FFormula%2Fpidge.rb&search=%5Cd%2B%5C.%5Cd%2B%5C.%5Cd%2B&label=homebrew&prefix=v&color=orange" alt="Homebrew"></a>
  <a href="https://github.com/mklab-se/pidge/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/pidge.svg" alt="License"></a>
</p>

<!-- "What's new" callout: keep it right after the badges and refresh it on every minor/major
     release (version and a one-line summary of the headline changes). Only link CHANGELOG.md. -->
<p align="center">
  <strong>pidge 2.0</strong> is a rebuild around one engine: Gmail next to Microsoft, a local MCP server,
  inbox triage in sweeps and batches, changes that never make you wait, and sending you can still cancel.<br>
  <a href="CHANGELOG.md"><strong>What's new</strong></a>
</p>

<p align="center">
  <a href="INSTALL.md">Install</a>
  &nbsp;·&nbsp; <a href="docs/usage.md">Usage</a>
  &nbsp;·&nbsp; <a href="docs/mcp.md">MCP server</a>
</p>

---

Your AI agent already writes your code. pidge lets it handle the rest of your day too: tell you
what's new, go through the inbox with you, find that one e-mail, clear out receipts and ads, draft
the reply, and run the calendar, across every mailbox you have.

```text
You:    Anything important today?
Agent:  (mail_brief importance=high)
        Two things: Anna asks you to sign the contract before Friday, and Bob's
        invitation for Thursday 14:00 is still unanswered.
You:    Clear out the rest of my inbox.
Agent:  (mail_triage scope=all)
        41 messages. First, 23 newsletters and promotions: archive them all?
You:    Yes.
Agent:  Done. Next, 4 receipts: Telia 449 kr, SL 970 kr, Apple 29 kr, Spotify 119 kr. File them?
```

## Why pidge

- **Made for agents, by asking what people say.** Fifteen operations, each answering something
  people actually ask ("anything new?", "clear my inbox", "archive all invoices", "cancel all my
  appointments today") in one or two calls. Short ids (`m12`, `e3`), compact text output, and a
  `next:` line telling the agent what to do.
- **It remembers.** pidge keeps a small local index of your recent mail, synced from your mailboxes:
  "new" means new since you last looked, and "all receipts" or "all commercials" is one query, not
  a hundred judgements.
- **Snappy.** Archive, delete, flag: pidge answers at once and does it in the background; a failure
  is reported on the next answer.
- **Safe by construction.** Changes to many messages, and anything that e-mails other people, are a
  plan you approve first. Sending is held for a few seconds to two minutes depending on risk, so
  you can still say "cancel", and mail that looks sensitive waits for a second yes. Undo is one
  call. Nothing is deleted permanently, and mail content is marked untrusted so your agent
  summarises it instead of obeying it.
- **All your mailboxes in one view.** Microsoft 365, Outlook.com and Gmail, with calendars, merged.
- **Everywhere your agent is.** A CLI and skill for terminal agents, a local MCP server for desktop
  apps, and a hosted MCP server for Claude.ai and ChatGPT, all one engine with the same behaviour.

## Get started

```bash
brew install mklab-se/tap/pidge        # or: cargo install pidge (see INSTALL.md)
pidge account add                      # Microsoft (opens your browser)
pidge account add --provider google    # Gmail (bring your own Google client: docs/gmail.md)
```

Then connect your agent:

- **Claude Code, Codex, Copilot CLI and other terminal agents:**
  `pidge skill > ~/.claude/skills/pidge/SKILL.md` (or your agent's skill folder).
- **Claude Desktop, Cowork and other local MCP clients:** add a stdio server running
  `pidge mcp serve`:
  ```json
  { "mcpServers": { "pidge": { "command": "pidge", "args": ["mcp", "serve"] } } }
  ```
- **Claude.ai, ChatGPT, phones and voice:** a hosted `pidge-mcp` server. The reference server at
  `https://pidge.mklab.se/mcp` is invite-only; self-host your own from the published image
  `ghcr.io/mklab-se/pidge-mcp` (Bicep templates for Azure included). See [docs/mcp.md](docs/mcp.md).

## Using it yourself

```bash
pidge                                      # what's new, and today's calendar
pidge mail find avtal --from anna          # find a message
pidge mail triage                          # go through the inbox, step by step
pidge mail act archive --kind receipt      # a plan for every receipt; apply it with --plan
pidge calendar agenda this_week
pidge calendar event create --title "Q3 planning" --start "tomorrow 15:00" --end +90m --attendees anna
```

More in [docs/usage.md](docs/usage.md), or `pidge --help`.

## Supported providers

Microsoft 365 work and school accounts, personal Microsoft accounts (Outlook.com, Hotmail, Live),
and Gmail with Google Calendar ([setup](docs/gmail.md)).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [DEVELOPMENT.md](DEVELOPMENT.md). Changes are listed
in [CHANGELOG.md](CHANGELOG.md), newest first.

## License

MIT, see [LICENSE](LICENSE).
