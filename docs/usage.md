# Using pidge

pidge is built for AI agents, and every command works by hand too. The mail and calendar commands
are the same operations the MCP tools are, with the same names and inputs: `pidge mail find` is
`mail_find`, `--in-reply-to` is `in_reply_to`. `pidge <area> <verb> --help` is always the complete
reference; add `--json` to any command for the structured result.

## Mailboxes

```bash
pidge account add                     # Microsoft 365 or Outlook.com (opens your browser)
pidge account add --provider google   # Gmail (needs your own Google client: docs/gmail.md)
pidge account list
pidge account remove
pidge settings                        # mailboxes and their health, categories, rules, policy
```

Mailboxes are merged: every read covers all of them unless you pass `--account`.

## The inbox

```bash
pidge                               # what's new since you last looked, and today's calendar
pidge mail inbox                    # what's new, in groups with a suggestion each
pidge mail inbox --importance high  # only what matters
pidge mail inbox unread             # or all: go through the inbox, a deck at a time
pidge mail inbox --next             # the next deck
pidge mail inbox --mark-seen=false  # peek without moving "last seen"
```

pidge remembers what you have seen, so "what's new" at 16:00 does not repeat what you went through
at 09:00. The answer is a deck: what needs you, what happens at a time (with your calendar: free,
clashes, already booked), parcels, receipts, newsletters and so on. Each group has an id (`g4`) that
stands for its messages, so one call settles a whole deck:

```bash
pidge mail act archive g4 g5 --except m12
pidge mail act --actions '[{"action":"delete","ids":["g6"]},{"action":"flag","ids":["m3"]}]' --next
```

## Finding and reading

Every message, group, event, draft, plan and change has a short id (`m12`, `g4`, `e3`, `d4`, `p5`,
`q7`). An id is never reused for something else.

```bash
pidge mail find avtal --from anna             # words and a sender
pidge mail find --kind receipt --since this_month
pidge mail find --category Uppdrag --unread
pidge mail find --text "Q3" --before 2025-06-01 --folder any   # searches the mailbox past the index
pidge mail find --text "faktura OR invoice"   # alternatives
pidge mail find --kind delivery               # where are my parcels?
pidge mail find --happening                   # mail about something that happens at a time
pidge mail read m12 m14
pidge mail read m12 --thread
pidge mail read m12 --headers          # delivery path, SPF, DKIM and DMARC results
pidge mail attachment m12 faktura             # the attachment as text (markitdown when installed)
pidge mail attachment m12 --mode save         # into Downloads
```

Kinds are `receipt` (receipts, invoices, bills), `newsletter`, `promotion` (ads), `notification`,
`delivery` (parcels), `social`, `invite` and `personal`.

## Acting on mail

```bash
pidge mail act archive m12 m14
pidge mail act delete --kind promotion        # done at once, and says whom it was from
pidge mail act move m12 --to "Kvitton/2026"   # folders are created as needed
pidge mail act flag m12 --due 2026-11-20
pidge mail act categorize m12 --categories Uppdrag   # pidge learns from it
pidge mail act archive g7 --always kind       # and from now on: mail of this kind
pidge mail act unsubscribe m12
pidge undo                                    # the latest change; undo q7 for another
pidge undo q7 --instead archive
```

What you name is done without further ado. A description that matches more than 50 messages becomes
a plan you apply after looking at it (`--plan p5`). Delete always means Deleted Items or Trash. Rules
made with `--always` apply to mail that comes later, never to mail that matters; `pidge settings`
lists them and `pidge settings forget --rule 1` removes one.

## Writing

```bash
pidge mail draft reply --in-reply-to m12 --body "Absolut, jag skriver under."
pidge mail draft new --to anna --subject "Lunch?" --body -     # body from stdin
pidge mail send d4
pidge undo                            # stops it, for a little while
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
pidge calendar act create --title "Q3 planning" --start "tomorrow 15:00" --end +90m \
  --attendees anna,bob@example.com --online-meeting
pidge calendar act create --from-message m12            # from the time a mail says, or its .ics
pidge calendar act update --id e3 --add-attendees anna  # "invite this person": a plan first
pidge calendar act cancel --id e3 --message "Sorry, something came up"
pidge calendar act accept --id e3
pidge calendar act clear --range today  # cancel / decline / delete everything today: a plan first
pidge calendar free next_week --with anna --duration-minutes 60
```

## Teaching pidge

```bash
pidge settings                                        # mailboxes, categories, rules, VIPs, policy
pidge settings add --name Leisure --description "Climbing, choir"
pidge settings add_rule --name Leisure --rule domain:klatterklubben.se
pidge settings vip --who anna@example.com
pidge settings mute --who news@shop.se
pidge settings policy --key ask_over --value 100      # ask only above this many
pidge settings set --key timezone --value Europe/Stockholm
```

Preferences live in `prefs.json` next to `config.yaml`; the index (a cache rebuilt from your
mailboxes) is `index.db` in pidge's cache directory.

## Your own AI (optional)

pidge sorts mail with your rules and mail signals on its own. With your own AI key it also judges
what rules and signals leave open, tells appointments from dates that only happen to be mentioned
(at most 150 questions a day in all), and checks how sensitive an outgoing mail is:

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

Agents say what the user asked in the `intent` field of each call. pidge keeps every call, and what
it led to, one JSON line each, in `~/.cache/pidge/activity/<date>.jsonl` (hosted: the server's
`activity` container), so the tools can be tuned to how agents actually use them. Every line names
the pidge version.
