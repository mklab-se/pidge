# Manual Test Checklist

What's left to verify before we tag a release. Items already exercised on
your end are summarized at the top; everything below is still pending.

Run with `cargo run --quiet -- <args>` from the repo root.

Recipient for any send-side tests: `kristofer@mklab.se` (sending to yourself
is the safest target — the message comes right back so you can verify it).

---

## ✓ Already verified

- `pidge --version` / `pidge --help` — top-level surface
- `pidge account list` / `pidge account default` (no args) — storage column, defaults shown
- `pidge mail search "<query>"` — KQL search returns results
- `pidge mail mark-read` / `mark-unread` — toggle reflected in list bold/magenta
- `pidge mail flag` / `unflag` — visible in Outlook
- `pidge mail archive` — message moves to Archive folder in Outlook

---

## TUI compose form — `pidge mail new`

The big interaction surface that needs eyes on it. The wizard is gone; this
is now a full-screen TUI form.

### Layout & cursor

- [X] `pidge mail new` opens the form with `New e-mail` in the title
- [X] **Cursor block visible only on the focused field**, not all of them
  (this just got fixed — confirm)
- [X] Field labels (`From:`, `To:`, `Cc:`, …) are bold light-cyan
- [X] Focused label is reversed-video so you can see at a glance which field
  has focus
- [X] Footer line at the bottom shows colored hotkey legend:
  `Tab next · Shift-Tab prev · Ctrl-S send · Ctrl-D draft · Ctrl-A attach · Esc cancel`

### Navigation

- [X] `Tab` cycles forward: From → To → Cc → Bcc → Subject → Attach → Body → From
- [X] `Shift-Tab` cycles backward
- [X] In single-line fields (To/Cc/Bcc/Subject), pressing `Enter` jumps to the next field
- [X] In Body, pressing `Enter` inserts a newline (does not jump fields)
- [X] From field: `Left`/`Right` (or `Space`) cycle through signed-in accounts
  (only useful with 2+ accounts; single account = no cycling visible)

### Body editing

- [X] Type multi-line text, use `Up`/`Down` arrows to navigate between lines
- [X] `Backspace` at column 0 of line 2+ joins with previous line
- [X] `Home` / `End` jump to start / end of line
- [X] Type something with accents or emoji (`ö`, `é`, `🎉`) — cursor math should stay correct

### Attachments via the form

- [X] Tab to Attach field → press `a` (or `Ctrl-A` from any field) → modal opens
- [X] In modal: type a path like `crates/pidge/tests/fixtures/linkedin_jobs_digest.html`, press `Enter`
- [X] File appears in the Attach: row in the main form
- [X] Tab to Attach → press `x` to remove the last attachment
- [ ] Try a bogus path in the modal (`/tmp/does-not-exist`) → error modal appears, any key dismisses

### Send / draft / cancel

- [X] Empty `To` → press `Ctrl-S` → error modal: "To: at least one recipient is required"
- [ ] Garbage in To (`foo`) → press `Ctrl-S` → error modal: "'foo' doesn't look like an e-mail address"
- [X] Fill the form (To = your address, Subject = "tui test", body = a few lines), press `Ctrl-S` → form exits, see `✔ Sent.`
- [X] Within a minute, the message arrives — `pidge mail` shows it
- [ ] Repeat the form, press `Ctrl-D` → `✔ Saved draft. Use \`pidge drafts edit <hash>\`…`. Note the hash.
- [ ] Repeat, press `Esc` → "Discard this message?" overlay. Press `n` → returns to form. Press `Esc` again, then `y` → form exits, returns to prompt

### Non-interactive (scripting) path

- [ ] `pidge mail new --to kristofer@mklab.se --subject "flag test" --body "one-line body"` → sends immediately, no TUI (no `-y` needed when `--to`, `--subject`, `--body` are all given)
- [ ] Same command with `--confirm` appended → opens the TUI pre-filled with your flags so you can review/edit before pressing Ctrl-S
- [ ] Find that arrived message:
  - [ ] `pidge mail reply <fragment> --body "thanks" -y` → reply sends
  - [ ] `pidge mail forward <fragment> --to kristofer@mklab.se --body "fyi" -y` → forward sends

---

## Drafts — `pidge drafts ...`

Drafts edit now uses the same TUI compose form, pre-filled.

- [X] From the `Ctrl-D` draft you saved above, run `pidge drafts list` → the draft is in the table
- [X] `pidge drafts show <fragment>` → renders the draft body
- [ ] `pidge drafts edit <fragment>` → TUI opens with **all values pre-filled** (To, Subject, Body)
- [ ] Modify the body, press `Ctrl-D` → `✔ Saved.`
- [ ] `pidge drafts show <fragment>` → body reflects the edit
- [ ] `pidge drafts edit <fragment>` again → press `Ctrl-S` this time → `✔ Sent.`
- [ ] `pidge drafts list` → the sent draft is gone, message arrives in inbox

Reply-as-draft round-trip:
- [ ] Pick any mail in `pidge mail`, note its hash
- [ ] `pidge mail reply <fragment> --draft -y` → draft hash printed
- [ ] `pidge drafts edit <draft-fragment>` → wizard pre-fills with the reply
- [ ] `Ctrl-D` to save changes, then `pidge drafts delete <fragment> -y` → cleanup

---

## Attachments — `pidge drafts attachments ...`

- [ ] Save a draft with one attachment:
  `pidge mail new --to kristofer@mklab.se --subject "draft+attach" --body "ditto" --attach crates/pidge/tests/fixtures/speedledger_newsletter.html --draft`
- [ ] `pidge drafts attachments list <fragment>` → shows the speedledger file
- [ ] `pidge drafts attachments add <fragment> crates/pidge/tests/fixtures/linkedin_jobs_digest.html` → adds a second
- [ ] `pidge drafts attachments list <fragment>` → shows both
- [ ] `pidge drafts attachments remove <fragment> linkedin_jobs_digest.html` → removes by filename
- [ ] `pidge drafts attachments list <fragment>` → only speedledger remains
- [X] `pidge drafts delete <fragment> -y` → cleanup

Oversized attachment safety:
- [ ] `dd if=/dev/zero of=/tmp/big.bin bs=1m count=5`
- [ ] `pidge mail new --to kristofer@mklab.se --subject "big" --body "test" --attach /tmp/big.bin` → clean error: "above the 3 MB simple-upload limit"

---

## Pagination

- [ ] `pidge mail list -p 1 -n 10` and `pidge mail list -p 2 -n 10` return **different** messages (no overlap)
- [ ] `pidge mail list -p 99 -n 10` → likely empty table, no crash

---

## Delete — `pidge mail delete`

Single delete:
- [ ] Send a test message to yourself, note its hash once it arrives
- [ ] `pidge mail delete <fragment>` → asks "Delete message <hash> (moves to Deleted Items)? [y/N]"
- [ ] Answer `n` → "Aborted." prints
- [ ] Re-run, answer `y` → `✔ Deleted.`
- [ ] In Outlook → Deleted Items folder, confirm the message is there (not hard-deleted)

Bulk delete safety gate:
- [ ] `pidge mail delete --older-than 2050-01-01` (no `-y`) → errors: "Bulk delete requires explicit `-y` confirmation — there is no interactive prompt. Re-run with `-y` if you really mean it."
- [ ] `pidge mail delete --older-than 2000-01-01 -y` (safe cutoff) → walks accounts, reports 0 deleted

**Do not run** `pidge mail delete --older-than 2050-01-01 -y` — would wipe your inbox.

---

## Visual polish (recently added)

- [ ] `pidge mail list -n 10 --compact` shows `⚑` (yellow) before any flagged subjects, `✓` (green) before completed
- [ ] `pidge mail show <flagged-fragment>` → flag marker prepended to the Subject line of the header
- [ ] `pidge account list` shows a `TYPE` column with `M365` (org tenant) or `Outlook` (personal MSA)
- [ ] `pidge account list` uses the same horizontal-only table style as `pidge mail` (no vertical borders)
- [ ] `pidge --help` does **not** mention "Microsoft 365" — wording is provider-agnostic
- [ ] Any inquire prompt (e.g. `pidge mail delete <fragment>` confirm, or the `pidge mail send` summary confirm if you trip it) shows a bold cyan label and a yellow `?` prefix

---

## Output / JSON

- [ ] `pidge mail --json | jq '.[0].subject'` returns a subject string
- [ ] `pidge mail --json | jq '.[0].flagStatus'` returns one of `"flagged"`, `"notFlagged"`, `"complete"`
- [ ] `pidge account list --json` includes `provider` field (`"m365"` or `"outlook"`) plus `is_default_email` / `is_default_calendar`
- [ ] `pidge mail --no-color | head -3` strips ANSI styling (no escape sequences in the output)

---

## Account management round-trip

- [ ] `pidge account default e-mail kristofer@mklab.se` → no-op confirmation, syntax works
- [ ] `pidge account default calendar kristofer@mklab.se` → no-op confirmation
- [ ] `pidge account migrate-storage kristofer@mklab.se --to keychain` → moves tokens; next Graph call may prompt for keychain access
- [ ] `pidge mail -n 1` runs successfully against the keychain backend
- [ ] `pidge account migrate-storage kristofer@mklab.se --to file` → moves back, no more keychain prompts

---

When everything is checked, delete this file (`rm MANUAL_TESTS.md`) and tag
the release: `/release minor`.
