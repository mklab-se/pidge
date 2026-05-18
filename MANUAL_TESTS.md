# Manual Test Checklist — Phases 1-6

A walk-through that exercises everything that automated tests don't reach: interactive
wizards, real Graph behavior, editor opening, terminal styling, confirmation prompts.

Run from the repo root with `cargo run --quiet --` in front of each command (or
install the binary first with `cargo install --path crates/pidge`).

Each line is a discrete check — tick the box when it behaves as described. Where
a step creates state (a draft, an unread flip, an archived message), the next step
relies on it; do them in order.

For send-side tests, the **suggested recipient is your own address**
(`kristofer@mklab.se`). It's the safest target and the easiest to verify in
Outlook afterward.

---

## Setup

- [X] `pidge --version` prints `pidge 0.2.0` (or newer if you've released since)
- [X] `pidge --help` lists `account`, `inbox`, `drafts`, `trust`, `ai`, `completion`, `version`
- [X] `pidge account list` shows your signed-in account(s), the storage backend column, and `[e-mail]` / `[calendar]` markers
- [X] `pidge account default` (no args) prints `e-mail:` and `calendar:` lines with the current defaults

## Phase 1 — account commands & inbox shortcut

- [ ] `pidge account --help` shows: add, list, remove, default, migrate-storage (and **no** `status` — that was folded into `list`)
- [ ] `pidge account default --help` shows `e-mail` and `calendar` subcommands
- [ ] `pidge inbox` (no args, no subcommand) prints the recent inbox table — same as `pidge inbox list`
- [ ] `pidge inbox -n 5 --compact` works (flags route through to `list`)
- [ ] `pidge inbox <some-hash-fragment>` opens the message (shortcut for `inbox show`). Try a 3-4 char prefix from the list.
- [ ] `pidge inbox --help` shows the inbox subcommand help (not `list` help)

## Phase 2 — read-side power

- [ ] `pidge inbox list -p 1 -n 10` and `pidge inbox list -p 2 -n 10` return different messages (pagination)
- [X] `pidge inbox search "github"` returns results, sorted by Graph relevance not date
- [X] `pidge inbox search "from:noreply@github.com"` filters to GitHub mails only
- [X] Pick an unread-looking message from `pidge inbox`. Its ID column shows the hash.
- [X] `pidge inbox mark-read <fragment>` — re-run `pidge inbox` and confirm the row is no longer bold/magenta
- [X] `pidge inbox mark-unread <fragment>` — re-run `pidge inbox` and confirm it's bold/magenta again
- [X] `pidge inbox flag <fragment>` — open Outlook (web or app) and confirm the message has a follow-up flag
- [X] `pidge inbox unflag <fragment>` — confirm the flag disappears in Outlook
- [X] `pidge inbox archive <fragment>` — confirm the message moves to the Archive folder in Outlook
  - **Note:** the cache entry is purged; this fragment will no longer be findable via `pidge inbox show`.

## Phase 3 — compose (interactive wizard)

- [ ] `pidge inbox send` — wizard runs:
  - Prompts for `To` (enter your own e-mail)
  - Prompts for `Cc` (leave empty, just press Enter)
  - Prompts for `Bcc` (leave empty)
  - Prompts for `Subject` (e.g. "pidge wizard test")
  - Opens `$EDITOR` for body — type a couple of lines, save & exit
  - Shows a summary box (From, To, Subject in bold bright-yellow, body preview)
  - Asks "Send this message? [y/N]" — answer `n` first to confirm "Aborted." prints
- [ ] Re-run `pidge inbox send`, complete the wizard, answer `y`. See `✔ Sent.`
- [ ] Within a minute, `pidge inbox` shows the new message arriving from yourself

## Phase 3 — compose (power-user flags)

- [ ] `pidge inbox send --to kristofer@mklab.se --subject "flag test" --body "one-line body" -y` — sends immediately (no prompts, no editor)
- [ ] Wait for arrival, then pick its short hash:
  - [ ] `pidge inbox reply <fragment> --body "thanks" -y` succeeds
  - [ ] `pidge inbox forward <fragment> --to kristofer@mklab.se --body "fyi" -y` succeeds

## Phase 4 — drafts lifecycle

- [ ] `pidge inbox send --to kristofer@mklab.se --subject "draft test" --body "first version" --draft -y`
  - Output ends with: `✔ Saved draft. Use \`pidge drafts edit <hash>\` or \`pidge drafts send <hash>\`.`
- [ ] Copy that draft's short hash for the next few steps.
- [ ] `pidge drafts list` — shows the new draft in the table
- [ ] `pidge drafts show <fragment>` — displays the draft body
- [ ] `pidge drafts edit <fragment>`:
  - All current values pre-fill the prompts (To, Cc, Bcc, Subject, Body)
  - Change the body to "second version", save & exit, see `✔ Saved.`
- [ ] `pidge drafts show <fragment>` — body now reads "second version"
- [ ] `pidge drafts send <fragment>` — asks "Send draft? [y/N]", answer `y`. See `✔ Sent.` and confirm arrival.
- [ ] `pidge drafts list` — the sent draft is gone

Reply draft:
- [ ] Pick any message in `pidge inbox`. Note its short hash.
- [ ] `pidge inbox reply <inbox-fragment> --draft -y` (no body — Graph keeps the auto-quoted content). Note the draft hash.
- [ ] `pidge drafts edit <draft-fragment>` — the wizard pre-fills with the auto-quoted body
- [ ] Add your own line at the top, save
- [ ] `pidge drafts delete <draft-fragment>` — asks for confirmation, answer `y`

## Phase 5 — attachments

Pick a small file to attach (under 3 MB). For example, `crates/pidge/tests/fixtures/linkedin_jobs_digest.html` is ~50 KB.

- [ ] `pidge inbox send --to kristofer@mklab.se --subject "attach test" --body "see attached" --attach crates/pidge/tests/fixtures/linkedin_jobs_digest.html -y`
  - Output shows: `Uploading attachments:` then `+ linkedin_jobs_digest.html (text/html, ...)` then `✔ Sent.`
- [ ] In Outlook, confirm the message arrived with the HTML file attached
- [ ] Try a too-large file (create one: `dd if=/dev/zero of=/tmp/big.bin bs=1m count=5`):
  - `pidge inbox send --to kristofer@mklab.se --subject "big" --body "test" --attach /tmp/big.bin -y` should error with a clear "above the 3 MB simple-upload limit" message
- [ ] Save a draft with an attachment: `pidge inbox send --to kristofer@mklab.se --subject "draft+attach" --body "ditto" --attach crates/pidge/tests/fixtures/speedledger_newsletter.html --draft -y`
- [ ] `pidge drafts attachments list <fragment>` — shows the attached file
- [ ] `pidge drafts attachments add <fragment> crates/pidge/tests/fixtures/linkedin_jobs_digest.html` — adds a second attachment
- [ ] `pidge drafts attachments list <fragment>` — shows both
- [ ] `pidge drafts attachments remove <fragment> linkedin_jobs_digest.html` — removes by filename
- [ ] `pidge drafts attachments list <fragment>` — only the speedledger one remains
- [ ] `pidge drafts delete <fragment> -y` — clean up

## Phase 6 — destructive ops (single + bulk)

Single delete:
- [ ] Send another test message to yourself, let it arrive, get its short hash from `pidge inbox`
- [ ] `pidge inbox delete <fragment>` — asks "Delete message <hash> (moves to Deleted Items)? [y/N]"
  - Answer `n` once to confirm abort works
  - Run again, answer `y`, see `✔ Deleted.`
- [ ] In Outlook → Deleted Items folder, confirm the message landed there (not hard-deleted)

Bulk delete safety:
- [ ] `pidge inbox delete --older-than 2050-01-01` (without `-y`) — errors: "Bulk delete requires explicit `-y` confirmation"
- [ ] `pidge inbox delete --older-than 2050-01-01 -y` — would delete everything in your inbox dated before 2050. **DO NOT RUN THIS** unless you're OK losing every message. Listed here only so you see the help works.

Safe bulk-delete dry-fire:
- [ ] Pick a cutoff that excludes everything you care about (e.g. 10 years ago). `pidge inbox delete --older-than 2000-01-01 -y` — should report 0 deleted for each account.

## Account management (re-test after the restructure)

- [ ] `pidge account default e-mail kristofer@mklab.se` (set to itself, no-op but confirms the syntax works)
- [ ] `pidge account default calendar kristofer@mklab.se`
- [ ] `pidge account migrate-storage kristofer@mklab.se --to keychain` — moves tokens back to keychain
- [ ] Run a Graph call (`pidge inbox -n 1`) — expect a keychain prompt the first time
- [ ] `pidge account migrate-storage kristofer@mklab.se --to file` — moves back to file (no more keychain prompts)

## Output options sanity

- [ ] `pidge inbox --json | jq '.[0].subject'` returns a subject string
- [ ] `pidge account list --json` returns valid JSON with `is_default_email` and `is_default_calendar` fields
- [ ] `pidge inbox --no-color` strips ANSI styling

---

When everything is checked, delete this file (`rm MANUAL_TESTS.md`) and tag the
release: `/release minor`.
