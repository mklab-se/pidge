# Using pidge

pidge is built for AI agents, and every command works by hand too. The mail and calendar commands
are the same operations the MCP tools are, with the same names and inputs: `pidge mail find` is
`mail_find`, `--set-kind` is `set_kind`. `pidge <area> <verb> --help` is always the complete
reference; add `--json` to any command for the structured result.

## Mailboxes

```bash
pidge account add                     # Microsoft 365 or Outlook.com (opens your browser)
pidge account add --provider google   # Gmail (needs your own Google client: docs/gmail.md)
pidge account list
pidge account remove
pidge accounts                        # health, sync state, default sender, timezone
```

Mailboxes are merged: every read covers all of them unless you pass `--account`.

## What's new

```bash
pidge                         # what's new since you last looked, and today's calendar
pidge mail brief              # the inbox map and what matters, with a reason for each
pidge mail brief --importance high
pidge mail brief --since 7d --mark-seen=false   # peek without moving "last seen"
```

pidge remembers what you have seen, so "what's new" at 16:00 does not repeat what you went through
at 09:00.

## Finding and reading

Every message, event, draft and plan has a short id (`m12`, `e3`, `d4`, `p5`).

```bash
pidge mail find avtal --from anna             # words and a sender
pidge mail find --kind receipt --since this_month
pidge mail find --category Uppdrag --unread
pidge mail find --text "Q3" --before 2025-06-01 --folder any   # searches the mailbox past the index
pidge mail read m12
pidge mail read m12 --thread
pidge mail attachment m12 faktura             # the attachment as text (markitdown when installed)
pidge mail attachment m12 --mode save         # into Downloads
```

Kinds are `receipt` (receipts, invoices, bills), `newsletter`, `promotion` (ads), `notification`,
`social`, `invite` and `personal`.

## Going through the inbox

```bash
pidge mail triage                     # start: the map, the plan, the first step
pidge mail triage yes                 # do what pidge suggests for this step
pidge mail triage archive             # or: delete, read, keep, flag, file, unsubscribe, skip, one_by_one, undo, stop
pidge mail triage --scope all         # the whole inbox ("clear my inbox")
```

A triage runs in passes: sweeps of bulk mail first (one decision clears a group), then small
batches of routine mail such as receipts, then what matters one by one with a suggestion.

## Acting on mail

```bash
pidge mail act archive m12 m14
pidge mail act delete --kind promotion        # a plan: count, examples, unsure ones
pidge mail act delete --plan p5               # apply it
pidge mail act move m12 --to "Kvitton/2026"   # folders are created as needed
pidge mail act flag m12 --due 2026-11-20
pidge mail act unsubscribe m12
pidge mail act classify m12 --set-kind receipt   # teach pidge about one message
pidge mail act undo
```

Changes to more than five messages, and every change described by a selector, become a plan you
apply after looking at it. Delete always means Deleted Items or Trash.

## Writing

```bash
pidge mail draft reply --in-reply-to m12 --body "Absolut, jag skriver under."
pidge mail draft new --to anna --subject "Lunch?" --body -     # body from stdin
pidge mail send d4                    # held 10 s to 2 min depending on risk
pidge mail send d4 --action cancel
pidge mail send --action status       # the outbox
```

Recipients can be names of people you mail with. Sending is held briefly so you can cancel: about
10 seconds for a short reply to someone you know, longer for a first mail to an address, a forward
or attachments. Mail that looks sensitive (identity or card numbers, passwords, many recipients)
waits until you approve it with `--action approve`. A short-lived CLI hands held mail to a small
background `pidge outbox run` process; anything overdue also goes out on the next pidge command.

## Calendar

```bash
pidge calendar                        # today
pidge calendar agenda this_week
pidge calendar agenda next            # the next event
pidge calendar agenda --pending-only  # unanswered invitations
pidge calendar event create --title "Q3 planning" --start "tomorrow 15:00" --end +90m \
  --attendees anna,bob@example.com --online-meeting
pidge calendar event create --from-message m12          # from an e-mail's .ics
pidge calendar event update --id e3 --add-attendees anna  # "invite this person": a plan first
pidge calendar event cancel --id e3 --message "Sorry, something came up"
pidge calendar act clear today        # cancel / decline / delete everything today: a plan first
pidge calendar respond e3 accept
pidge calendar free next_week --with anna --duration-minutes 60
```

## Teaching pidge

```bash
pidge mail setup                                        # categories, VIPs, policy, settings
pidge mail setup add --name Leisure --description "Climbing, choir"
pidge mail setup add_rule --name Leisure --rule domain:klatterklubben.se
pidge mail setup add_example --name Leisure --example m12
pidge mail setup vip --who anna@example.com
pidge mail setup mute --who news@shop.se
pidge mail setup policy --key bulk --value allow        # bulk changes without a plan
pidge mail setup set --key timezone --value Europe/Stockholm
```

Preferences live in `prefs.json` next to `config.yaml`; the index (a cache rebuilt from your
mailboxes) is `index.db` in pidge's cache directory.

## Your own AI (optional)

pidge sorts mail with your rules and mail signals on its own. With your own AI key it also judges
what rules and signals leave open, and checks how sensitive an outgoing mail is:

```bash
pidge ai config      # provider and model, through Ailloy (shared with rigg, mdeck, cosq)
pidge ai enable
```

Set `PIDGE_NO_AI=1` to run with rules and signals only, whatever is configured.

## For agents

```bash
pidge skill > ~/.claude/skills/pidge/SKILL.md   # Claude Code, Codex, Copilot CLI, ...
pidge mcp serve                                  # a local MCP server on stdio
pidge op --list                                  # every operation
pidge op mail_find --input '{"kind":"receipt"}'  # any operation with its exact MCP input
```
