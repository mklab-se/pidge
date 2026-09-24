<p align="center">
  <img src="https://raw.githubusercontent.com/mklab-se/pidge/main/media/pidge.png" alt="pidge" width="600">
</p>

<h1 align="center">pidge</h1>

<p align="center">
  <strong>The e-mail and calendar client for your AI agent.</strong><br>
  Let Claude Code, Codex, Copilot, Claude.ai or ChatGPT work your real inbox and calendar, safely.
</p>

<p align="center">
  <a href="https://github.com/mklab-se/pidge/actions/workflows/ci.yml"><img src="https://github.com/mklab-se/pidge/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/pidge"><img src="https://img.shields.io/crates/v/pidge.svg" alt="crates.io"></a>
  <a href="https://github.com/mklab-se/pidge/releases/latest"><img src="https://img.shields.io/github/v/release/mklab-se/pidge" alt="GitHub Release"></a>
  <a href="https://github.com/mklab-se/homebrew-tap/blob/main/Formula/pidge.rb"><img src="https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmklab-se%2Fhomebrew-tap%2Fmain%2FFormula%2Fpidge.rb&search=%5Cd%2B%5C.%5Cd%2B%5C.%5Cd%2B&label=homebrew&prefix=v&color=orange" alt="Homebrew"></a>
  <a href="https://github.com/mklab-se/pidge/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/pidge.svg" alt="License"></a>
</p>

<p align="center">
  <a href="CHANGELOG.md"><strong>What's new →</strong></a>
  &nbsp;·&nbsp; <a href="INSTALL.md">Install</a>
  &nbsp;·&nbsp; <a href="docs/usage.md">Usage</a>
  &nbsp;·&nbsp; <a href="docs/mcp.md">MCP server</a>
</p>

---

Your AI agent already writes your code. pidge lets it handle the rest of your day too:
triage the inbox, draft replies, find a free slot, book the meeting and answer the invite,
across every mailbox you have, from one tool.

```text
You:    Go through my unread mail, archive the newsletters, and draft a reply to
        Bob proposing a slot next week when we're both free.

Agent:  $ pidge mail list --unread --json
        $ pidge mail archive 3f1a
        $ pidge mail archive 9c02
        $ pidge calendar --week --json
        $ pidge mail reply 4d8e --draft --body-file -
        Archived 2 newsletters. Drafted a reply to Bob proposing Tue 14:00 or
        Thu 10:00. Want me to send it?
```

## Why pidge

- **Built for agents first.** Every command speaks `--json`, messages and events have short
  stable ids, lists page with cursors, and `mail delta` / `calendar delta` / `pidge watch` let
  long-running agents follow changes instead of re-reading everything.
- **You stay in control.** Sends ask before they go, and agents can save drafts for you to
  approve. Lock things down further with per-action guardrails
  (`pidge config set guardrails.send confirm`, also `delete`, `cancel`, `rsvp`, `bulk`,
  `unsubscribe`; each `allow` / `confirm` / `deny`) or preview anything with `--dry-run`.
  The MCP server goes further: sends only from an approved draft, a per-user send cap, and
  third-party mail content marked as untrusted so your agent summarises it instead of obeying it.
- **All your mailboxes in one view.** Sign in to several accounts; reads are merged across all
  of them, and writes go out from the right one.
- **Mail and calendar in one tool.** Search, read, reply, forward, file, unsubscribe; create,
  reschedule, duplicate, cancel and RSVP to events, including recurring meetings.
- **Fast and self-contained.** A single Rust binary. Tokens live in your OS keychain.

## Get started

### Option 1: CLI + agent skill (Claude Code, Codex, Copilot, …)

```bash
brew install mklab-se/tap/pidge        # or: cargo install pidge (see INSTALL.md)
pidge account add                      # sign in (opens your browser)
pidge ai skill --emit > ~/.claude/skills/pidge/SKILL.md
```

That's it, ask your agent about your mail. The emitted skill is deliberately small: it teaches
the agent the patterns (JSON output, confirmation gates, account context) and points it at
`pidge --help` for everything else, so it keeps working as pidge grows.

### Option 2: Remote MCP server (Claude.ai, ChatGPT, Cowork, …)

For harnesses that speak [MCP](https://modelcontextprotocol.io) rather than run shell commands,
`pidge-mcp` exposes the same mail and calendar tools over HTTP. Each user signs in with their
own account and only ever sees their own mailboxes.

- **Self-host it:** a ready-made image is published at `ghcr.io/mklab-se/pidge-mcp`, with
  Bicep templates for Azure Container Apps.
- **The reference server** at `https://pidge.mklab.se/mcp` is currently invite-only.

See [docs/mcp.md](docs/mcp.md) for connecting clients, self-hosting, and moving an existing
local setup over with `pidge mcp connect`.

## Using it yourself

pidge is a pleasant CLI for humans too:

```bash
pidge mail                                   # latest mail, all accounts
pidge mail 3515                              # open a message by id fragment
pidge mail search 'from:alice subject:budget'
pidge calendar --week                        # the week ahead
pidge calendar new --title "Q3 planning" --start "tomorrow 15:00" --end "+90m" \
  --invite alice@example.com --online
```

More in [docs/usage.md](docs/usage.md), or run `pidge --help`.

## Supported providers

Microsoft 365 work/school accounts and personal Microsoft accounts (Outlook.com, Hotmail, Live)
today. pidge's core is provider-neutral, and more providers may follow.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [DEVELOPMENT.md](DEVELOPMENT.md). Changes are listed
in [CHANGELOG.md](CHANGELOG.md), newest first.

## License

MIT, see [LICENSE](LICENSE).
