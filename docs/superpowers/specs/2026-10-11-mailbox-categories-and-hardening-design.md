# pidge 1.7.2: mailbox categories, review fixes, security hardening

**Date:** 2026-10-11
**Status:** approved in discussion
**Builds on:** `2026-10-11-mcp-inbox-review-design.md`

Three parts ship together in pidge 1.7.2. Each part lists what was decided;
details not listed are the implementer's call, following CLAUDE.md patterns.

---

## Part A: security hardening (from the 2026-10-11 security review)

### A1. OAuth (crates/pidge-mcp/src/oauth)
1. **Redirect host allowlist.** `/register` and `/authorize` accept a
   redirect URI only if its host is allowlisted: `claude.ai`, `chatgpt.com`,
   plus loopback (`localhost`, `127.0.0.1`, `[::1]`, any port, http allowed
   only there). Extra hosts from an env var `PIDGE_MCP_REDIRECT_HOSTS`
   (comma-separated, exact host match, documented on `Config::from_env`).
   Self-hosters can widen it; the default is the list above.
2. **Refresh-token rotation with reuse detection.** Each refresh grant
   invalidates the presented refresh token. Store, per user and client
   (`sub` + `client_id`), the current refresh token's `jti` in the user
   record (bounded map, e.g. at most 20 clients, oldest dropped). A refresh
   presenting a jti that is not current is reuse: bump `token_generation`
   (signs out everywhere) and refuse. Tokens issued before this release
   (no stored jti for that client) are accepted once and start the chain.
3. **No redirect before consent.** Errors detected in `/authorize` before
   the consent page (bad response_type, missing PKCE, bad resource, …)
   render an error page instead of redirecting to the registered URI.
4. **`__Host-` cookie names** for the consent and callback cookies
   (`Path=/`, `Secure`, no `Domain`), keeping each flow's cookie distinct
   by name. Loopback dev over http may keep plain names.
5. **`/register` logging:** log the number of redirect URIs and their hosts,
   and `client_name` truncated to 80 characters; refuse more than 10
   redirect URIs.
6. **`accounts_connect` without `email`:** the consent nonce is set once; a
   second open of the same link shows "already opened" (connect links are
   not prefetched by chat apps in a way that matters: they need Microsoft
   sign-in anyway). Keep the existing behaviour when `email` is given.

### A2. Pages (oauth/pages.rs, ai_page.rs, download.rs)
- Every HTML page sends `Content-Security-Policy: default-src 'none';
  style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'`,
  `X-Frame-Options: DENY`, `Cache-Control: no-store`,
  `Referrer-Policy: no-referrer`. (Consent pages that link to Microsoft use
  plain links, which CSP `default-src` does not block.)

### A3. Tools
1. **Owner check on use:** a mailbox is used by a tool only if its
   `MailboxRecord.owner` is the caller (enforce in `SecretTokenBackend` or in
   `ToolContext::accounts`/`sender`).
2. **Time-range panics** (`pidge-core/src/timerange.rs`, availability): cap
   `Nd` at 3650, use checked date arithmetic, cap explicit ranges at 10
   years; return a usage error instead of panicking.
3. **Send cap:** charge `calendar_event action=update` whenever the event
   has attendees (Outlook mails them the change), and `calendar_respond`
   `propose` even without a message.
4. **Inline untrusted text:** the List-Unsubscribe web URL is shown as
   `Url::parse(..).as_str()` (percent-encoded, no spaces); attachment names
   in captions and errors, the organizer's subject in calendar_respond,
   calendar names and folder names go through `one_line` and are capped at
   120 characters.
5. **Unsubscribe mailto:** the address must look like an address (reuse the
   `unusable` check from recipient resolution).

### A4. Deployment
1. GitHub `production` environment: deployment refs limited to `main` and
   tags `v*`, required reviewer `krist00fer`. Tag ruleset: only admins may
   create or update `v*` tags. `setup-github-oidc.sh` creates these instead
   of a bare PUT.
2. `release.yml`: workflow-level `permissions: contents: read`; `contents:
   write` only on the jobs that need it (GitHub release, Homebrew is a PAT).
   Every third-party action pinned to a full commit SHA with a `# vX`
   comment; Dependabot (`github-actions` ecosystem, weekly) keeps them
   current.
3. `setup-github-oidc.sh`: the RBAC Administrator condition also pins the
   principal to the app's managed identity.
4. `main.bicep`: production log level `info`.

---

## Part B: inbox review fixes (from the 2026-10-11 test drive and logs)

Logs showed ChatGPT left the review after one message and fell back to
`mail_overview` (5.8 s) and `mail_act`. Server-side, `mail_review_next` takes
36-584 ms.

1. **Steer the harness:** `mail_overview`'s description no longer claims "go
   through my inbox"; it says to use `mail_review_start` for going through
   mail. Server instructions open with the review rule. Every review
   response ends with one line: "Read this to the user. When they answer,
   call mail_review_next (session=…, action=…). During a review don't use
   mail_overview, mail_read or mail_act."
2. **Smaller responses:** review bodies are rendered without link targets
   (link text only), capped at 900 characters, and lead with the gist
   (sender, subject, first meaningful lines). Review items carry a short id
   (`r1`, `r2`, …, scoped to the session); `mail_review_next id=` accepts
   the short id (full ids still accepted).
3. After a bulk action, the response is the receipt plus a one-line
   reminder of the current message ("Still on: Microsoft invoice, 441 kr"),
   not the whole message again.
4. Position reads "9 left" (remaining unhandled), not "item 11 of 19".
5. `needs reply` only when the AI probability is at least 0.7 and the
   message is not in a category with a `signal` (bulk/automated) and not
   from an automated sender.
6. **Duplicates:** items with the same normalized subject and sender
   received within 10 minutes, or the same `internetMessageId`, or a
   forward of one of them by the user, are merged into one item; an action
   applies to all of them. The item says "(3 copies)".
7. `mail_categories action=add` accepts `rules`, `example_ids` and
   `position` in the same call.
8. Faster start: the AI budget ends as soon as every request has answered
   (it already does); move the header `$batch` and the AI requests to run
   concurrently where possible, and start fetching the first body while
   classifying.

---

## Part C: mailbox categories

pidge's category list stays the source of truth (one list across the user's
mailboxes). When the user turns it on, pidge also writes categories to the
mailboxes.

### C1. Setting and permission
- Off by default. Turned on/off with `mail_categories action=mailbox_sync
  enabled=true|false`, and per mailbox with `exclude`/`include` (`account`).
  When on, it applies to all the user's mailboxes except excluded ones.
- When the user edits categories and sync is off, the response carries a
  one-time note that the categories can also be put on the mail in their
  mailboxes (remembered on the record, shown once).
- Creating and recolouring master categories needs Graph
  `MailboxSettings.ReadWrite`. Add it to `SCOPES` in pidge-client (CLI and
  server share it). A mailbox connected before this lacks it: pidge then
  still tags messages (Mail.ReadWrite suffices) but cannot create or colour
  master categories; `accounts_list` and the sync note say that mailbox
  needs `accounts_connect` once.

### C2. Sync, per mailbox
Runs when sync is on, at review start and after category edits, at most
every 10 minutes per mailbox (`GET /me/outlook/masterCategories`).
State per mailbox on the user record: pidge category name → Outlook
category id, last seen display name and colour; plus per-category
"removed by the user in this mailbox" and pending questions.

- pidge category missing in the mailbox: if one with the same name exists
  (case-insensitive), link it and keep its colour; else create it with
  pidge's colour for that category.
- Linked category recoloured in the mailbox: pidge adopts the colour and
  sets it in the other mailboxes (last change wins).
- Linked category gone from the mailbox: respect it. Stop writing that
  category in that mailbox, and note once: "Uppdrag was removed in
  <mailbox>; should it come back, or go from pidge entirely?" The user's
  answer: `mail_categories action=mailbox_sync restore=<name>
  account=<mailbox>`, or a normal `remove`.
- Linked category renamed in the mailbox (same id, new name): note once:
  "Uppdrag was renamed to Projekt in <mailbox>; rename it everywhere?" On
  yes (`mail_categories action=update name=Uppdrag new_name=Projekt`), pidge
  renames in pidge and in the other mailboxes.
- The user's own categories are never touched. On first enable, pidge lists
  the mailboxes' named categories (not the colour-only defaults) and offers
  to adopt them as pidge categories. Colour-only defaults are recognised in
  English and Swedish ("Red category", "Röd kategori", …, and the
  six/twenty-five preset colour names) and never offered or touched.
- New master categories get a colour not used by the mailbox's colour-only
  defaults where possible (presets `preset0`..`preset24`).

### C3. Tagging messages
- At review start, when sync is on, every classified item (source rule or
  AI; not the plain-signal fallback into Other) gets its pidge category
  added to the message's `categories` in one `$batch` per mailbox, in the
  background. The message's other categories are kept; a different pidge
  category already on it is replaced (pidge-managed names only).
- Renaming a category in pidge: Graph can't rename, so pidge creates the
  new master category, retags messages carrying the old name (Graph
  `$filter=categories/any(c:c eq 'Old')`, capped at 500 per mailbox),
  and deletes the old one only if pidge created or linked it.
- Removing a category in pidge: the harness asks whether to remove it from
  the mail too (`remove_from_mail=true`); pidge deletes the master category
  only if it created it.
- `mail_search` and `mail_overview` gain a `category` filter
  (`categories/any(c:c eq '…')`, value escaped).
- A new explicit request, `mail_categories action=sort_mail days=N` (N up to
  31), classifies read and unread inbox mail of the last N days and tags it.
  Costs a few hundredths of a cent per 100 messages with Jev.

### C4. Security
- Category names are user-chosen: escape them in OData filters (single quote
  doubled) and in Graph bodies; refuse control characters (already done by
  `clean_name`).
- Only the caller's own mailboxes, as everywhere.
- Writes invalidate the read cache.
