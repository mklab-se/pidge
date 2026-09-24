# Using pidge

pidge is built for AI agents, but every command works just as well by hand. This page is a tour of
the everyday commands; `pidge --help` and `pidge <command> --help` are always the complete,
up-to-date reference. Add `--json` to any command for machine-readable output, and `--dry-run` to
any mutating command to see what it would do.

## Account setup

```bash
# Add an account (opens your browser — auth code + PKCE)
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

## Writing mail

```bash
# Reply, reply-all or forward (asks "Send? [y/N]"; -y skips, --draft saves instead)
pidge mail reply 3515 --body "Thanks, see you Tuesday."
pidge mail reply-all 3515 --draft --body-file notes.txt
pidge mail forward 3515 --to bob@example.com

# Compose: a full-screen form, or send directly for scripting
pidge mail new
pidge mail new --to alice@example.com --subject "Hello" --body "Hi Alice"

# Drafts
pidge drafts list
pidge drafts send 7a2c

# File and clean up
pidge mail archive 3515
pidge mail move 3515 --to "Receipts"
pidge mail unsubscribe 3515
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

# Appointment with a real calendar reminder the day before
pidge calendar new \
  --title "Car service" \
  --start "2026-10-07T11:25" --end "+35m" \
  --location "Volvo Car Kista" --reminder 1d

# Change or switch off the reminder on an existing event
pidge calendar edit 4cabda75 --reminder 2h
pidge calendar edit 4cabda75 --reminder off

# Reschedule
pidge calendar move-time 4cabda75 --start "fri 14:00"

# Cancel (organizer-only; sends notices to attendees)
pidge calendar cancel 4cabda75 --comment "Postponed to next week"

# RSVP to someone else's invite
pidge calendar rsvp 4cabda75 --accept

# Pipe to scripts
pidge calendar --json --week | jq '.[] | .subject'
```


## Guardrails

Every mutating action belongs to a class — `send`, `delete`, `cancel`, `rsvp`, `bulk`,
`unsubscribe` — and each class can be set to `allow` (the default), `confirm` or `deny`:

```bash
pidge config set guardrails.send confirm
pidge config set guardrails.delete deny
pidge config show
```

## AI provider

A few features (such as `pidge ai classify`) call an AI model directly. pidge delegates that
configuration to [ailloy](https://github.com/mklab-se/ailloy), shared with the other MKLab tools,
so you configure a provider once:

```bash
pidge ai config    # configure your AI provider
pidge ai status    # check status
```
