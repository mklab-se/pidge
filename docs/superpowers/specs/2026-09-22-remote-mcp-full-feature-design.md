# pidge remote MCP — full feature design

**Date:** 2026-09-22
**Status:** approved in discussion, awaiting spec review
**Supersedes:** the spike design (`2026-09-22-remote-mcp-spike-design.md`), which stays as the record of what was de-risked
**Crate:** `crates/pidge-mcp` · **Infra:** `deploy/azure/`

## Goal

Turn the spike into the product surface: a hosted MCP server that lets an AI
harness (Claude, ChatGPT, Claude Code, …) read, triage, write, reply to and
forward e-mail, respond to meeting invites, and manage the calendar for one or
more Microsoft mailboxes per user, for a two-person allowlist today and for
anyone who self-hosts it tomorrow.

Decisions already taken and not revisited here: Rust on `pidge-client`; the
server is its own OAuth 2.1 authorization server delegating sign-in to
Microsoft; a user's identity comes from the bearer token and never from tool
input; Key Vault via managed identity for secrets; Container Apps.

Decisions taken in this round: timezone defaults to Europe/Stockholm and is
settable per user; deletion only ever moves to Deleted Items; DNS for
`mklab.se` is at GoDaddy; no server-side language model (voice and style are
the harness's job).

## Sub-projects

Three, built and shipped in this order. Each gets its own implementation plan.

| # | Sub-project | Outcome |
|---|---|---|
| 1 | Tools and accounts | The 15 tools below, multi-account onboarding, send-from rules, migration from local pidge |
| 2 | Platform | `https://pidge.mklab.se/mcp`, privacy-safe structured logs, CI/CD via GitHub OIDC |
| 3 | Docs and distribution | Self-hosting guide, published container image, `pidge mcp` CLI commands |

---

## Part 1 — Tools and accounts

### 1.1 Design rules

- **One tool per user intent**, not per Graph call. The harness should
  rarely need more than two calls to satisfy a request.
- **Reads return compact plain text, newest first.** Every list item carries
  the ids the harness needs for the follow-up call (message id, thread id,
  account, event id) and a few cheap facts as flags so the harness can
  triage; the server does not rank.
- **Writes are two-step.** Content is created as a draft and sent by id.
  `mail_send` never accepts raw content. Nothing is irreversible: delete
  means Deleted Items, cancel means Outlook's cancel.
- **The account is chosen server-side** unless the user names one, and a
  named account must belong to the user.
- **E-mail content is untrusted input.** Bodies are wrapped in an explicit
  untrusted block, capped in length, and every tool description says so.
- **All accounts by default.** Every read merges every mailbox and every
  calendar the user has connected unless the user names one; "the next
  meeting" is the next event anywhere, "today's e-mail" spans all inboxes.
  Each item shows which account it belongs to.

### 1.2 Shared conventions

- `account`: optional e-mail address of one of the user's mailboxes. Absent
  means all mailboxes for reads and the rules in §1.5 for writes.
- Times in and out are ISO 8601 with offset, rendered in the user's timezone.
  Range shortcuts (`today`, `tomorrow`, `this_week`, `next_week`, `3d`) are
  interpreted in that timezone; weeks start on Monday.
- Ids are opaque Graph ids passed back verbatim. The server validates that
  an id belongs to the named or inferred account before acting.
- Errors are MCP tool errors with a one-line, actionable message
  (`Mailbox anna@… has expired; ask the user to reconnect it with
  accounts_connect`). Never a stack trace, never a Graph payload.
- Every tool result ends with a short `next:` hint when there is an obvious
  follow-up (e.g. `next: mail_read id=… thread=true`).

### 1.3 Mail tools

**`mail_overview`** — the entry point for "today's e-mail", "let's go through
my inbox", "what needs a reply".
Input: `since` (`today` default, `yesterday`, `Nd`, ISO), `unread_only`,
`folder` (`inbox` default, `drafts`, `sent`, `archive`, or a folder id),
`account`, `limit` (default 20, max 50).
Output: items newest first. Each item: `id`, `thread`, `account`, `from`,
`subject`, `age`, `unread`, `preview`, and `flags` drawn from cheap facts the
server already has: `to-me` (in To, not only Cc), `trusted` (sender on the
user's trusted list), `invite`
(meeting request awaiting response, with the event id so `calendar_respond`
can be called directly), `attachments`, `flagged`, `question` (body preview
contains a question). The `list` flag needs message headers, which Graph
does not return in list queries, so it is only present in `mail_read`.
Triage is the harness's job, using these flags.

**`mail_search`** — "find the mail from Gabriel about the ticket".
Input: `query` (free text, Graph `$search`), `from`, `subject`, `after`,
`before`, `has_attachments`, `folder`, `account`, `limit`.
Output: same item shape as `mail_overview`, newest first.

**`mail_read`** — read one message or a whole conversation.
Input: `id`, `thread` (bool), `account` (optional; inferred from the id's
owner account when omitted by trying the user's accounts in order).
Output: headers (from, to, cc, date, account), attachments (id, name, type,
size; see `mail_attachment`), body as plain text with HTML rendered by pidge's renderer and quoted
history stripped when `thread` is set. Thread mode returns messages newest
first, each trimmed to its own contribution.

**`mail_draft`** — create or revise a draft.
Input: `kind` (`new`, `reply`, `reply_all`, `forward`), `in_reply_to`
(message id, required for reply/forward), `draft_id` (revise instead of
create), `to`/`cc`/`bcc` (addresses or names), `subject` (new only; replies
keep `Re:`), `body` (plain text; blank lines are paragraphs), `from_account`.
Recipient names resolve through pidge's contact resolution against a
per-user contact cache built from the mailbox (§1.6). Ambiguity is an error
listing candidates; the harness asks the user and retries with an address.
Output: `draft_id`, `account`, and a preview (from, to, cc, subject, body)
that the harness is expected to show before sending.

**`mail_send`** — send a draft.
Input: `draft_id`, `account` (optional).
Guardrails: per-user cap of 30 sends per hour; refuses drafts whose `from`
is not one of the user's mailboxes; when a reply would go out from a
different account than the one that received the original, the result says
so. Output: confirmation with recipients and subject.

**`mail_act`** — bulk actions for triage and cleanup.
Input: `ids` (1–100), `action` (`read`, `unread`, `flag`, `unflag`,
`archive`, `move`, `categorize`, `delete`, `unsubscribe`), `folder` (for
move), `categories` (for categorize), `account` (optional).
Semantics: `archive` moves to Archive; `delete` moves to Deleted Items;
`unsubscribe` uses the List-Unsubscribe header (mailto or one-click POST),
never a tracked link. Batched with Graph `$batch`. Output: per-id result.

**`mail_folders`** — list folders with ids and unread counts per account.

**`mail_attachment`** — get at "please see the attached file".
Input: `id` (message), `attachment_id`, `mode` (`read` default, `link`),
`account` (optional).
`read` returns the attachment's content for the harness: documents (PDF,
Word, Excel, PowerPoint, HTML, CSV, text, Markdown) converted to Markdown by
markitdown (§1.10); images returned as an MCP image content block so a
vision-capable harness reads them directly; anything else reports the type
and size and suggests `link`. Text is wrapped as untrusted and capped at
30 000 characters with `offset` for the rest.
`link` returns a download URL valid for 15 minutes (§1.10) that the harness
shows the user to click.

### 1.4 Calendar tools

**`calendar_agenda`** — every "what's on my calendar" question.
Input: `range` (`today` default, `tomorrow`, `this_week`, `next_week`,
`next`, or `from`/`to`), `pending_only`, `account`.
Output: events merged across accounts and calendars, sorted by start, in the
user's timezone: `id`, `account`, `start`–`end` (or all-day), `title`,
`location` or join link, `organizer`, `my_response` (`organizer`, `accepted`,
`tentative`, `declined`, `none`), `attendees` count. `next` returns the first
event that starts after now. `pending_only` returns invites with
`my_response: none`.

**`calendar_respond`** — accept, decline, or propose a new time.
Input: `id`, `response` (`accept`, `tentative`, `decline`), `message`,
`send_response` (default true), `propose` (`start`, `end`; only with
`tentative` or `decline`, sent as Graph `proposedNewTime`), `account`.
Output: what was sent to the organizer.

**`calendar_event`** — create, update, cancel.
Input: `action`, `id` (update/cancel), `title`, `start`, `end` or `all_day`,
`attendees` (addresses or names), `location`, `body`, `online_meeting`
(bool), `account` (default sender account for create), `message` (cancel
note).
Output: the event as `calendar_agenda` would render it.

**`calendar_availability`** — free slots for "when am I free" and as the
input to propose-new-time.
Input: `duration_minutes`, `range` (as agenda; default `this_week`),
`working_hours` (default 08:00–18:00 Monday–Friday in the user's timezone),
`account` (optional; default all).
Output: up to 20 free windows, computed from the user's own calendars only.

### 1.5 Accounts and send-from rules

**User record** — one Key Vault secret per user, `user-<hash of sign-in
address>`, JSON:

```json
{
  "signin": "kristofer.liljeblad@live.com",
  "mailboxes": ["kristofer.liljeblad@live.com", "kristofer@mklab.se"],
  "default_sender": "kristofer.liljeblad@live.com",
  "timezone": "Europe/Stockholm",
  "trusted_senders": ["anna@example.com"]
}
```

**Mailbox record** — `mailbox-<sanitized address>` stays one secret per
mailbox and gains an `owner` field next to the token set. A mailbox can have
exactly one owner. The sign-in callback and the connect callback both refuse
to bind a mailbox that another user owns.

**Rules**
1. Reply, reply-all, forward: from the mailbox that received the original.
2. New mail and new events: from `default_sender`.
3. `from_account` on `mail_draft` / `account` on `calendar_event` overrides
   both, and must be in `mailboxes`.
4. The sign-in mailbox is `default_sender` until the user changes it.

**Tools**

- `accounts_list` — mailboxes, sign-in address, default sender, timezone,
  and each mailbox's health (ok / needs reconnect).
- `accounts_connect` — input `email` (optional hint shown in the account
  picker). Creates a pending connect bound to the current user and returns a
  sign-in URL; where the client supports MCP URL elicitation the server uses
  it, otherwise the harness shows the link. The callback runs the same
  Microsoft exchange as sign-in, checks ownership, and appends the mailbox to
  the user record. Allowlist applies to sign-in identities, not to connected
  mailboxes: Anna may connect any mailbox she can log in to.
- `accounts_update` — `default_sender`, `timezone`, `disconnect` (removes
  the mailbox record and its secret; refuses to disconnect the sign-in
  mailbox), `trusted_senders` add/remove.

**Migration from local pidge** — `pidge mcp connect <url>` (CLI, sub-project
3) signs the CLI in as an MCP client with the same OAuth flow, then for each
local account not yet connected calls `accounts_connect` and opens the URL in
the browser. Default account and trusted senders are copied as settings. No
refresh tokens leave the machine.

### 1.6 Server-side support

- **Contact cache per user**, in memory, built lazily from the mailbox
  (recent senders/recipients via Graph people and sent items), reusing
  `pidge_core::ContactsCache` without its file persistence. Refreshed when
  older than a day.
- **Item flags** (§1.3) are computed by a pure function in `pidge-core`
  over a message plus user context (trusted senders, my addresses), so the
  CLI can adopt it later and it is unit-testable without Graph.
- **HTML rendering** (`render_html_body`) and quoted-history stripping move
  from the CLI crate to `pidge-core` (or a small `pidge-render` module inside
  it) so both binaries share the fixture-tested renderer. The CLI's snapshot
  tests stay where they are and keep passing.
- **Graph additions to `pidge-client`**: people/recent-contacts query,
  `$batch` for the `mail_act` verbs that lack it, calendar view across all
  calendars of an account, `proposedNewTime` on RSVP if not already
  supported, List-Unsubscribe one-click POST.
- **MCP prompts**: `triage_inbox`, `reply_to`, `cleanup_inbox` — short
  recipes naming which tools to call in which order and what to confirm with
  the user.

### 1.7 Security

- Identity: unchanged from the spike. Every tool reads
  `AuthenticatedUser` from the request and resolves mailboxes through the
  user record; an id from another user's mailbox fails as not found.
- Account ownership is enforced on write (bind) and read (every tool).
- Prompt injection: bodies wrapped and capped; tool descriptions and the
  server instructions repeat the rule; `mail_send` only by draft id; sends
  rate-limited; `mail_act` never permanent; `unsubscribe` never follows
  arbitrary links.
- Refresh-token revocation and confidential-client registration remain
  follow-ups, listed in Part 2.

### 1.8 Testing

- Pure logic (item flags, range parsing, send-from rules, availability
  computation, contact resolution, quoted-history stripping): unit tests in
  `pidge-core` / `pidge-mcp`.
- Tool handlers: in-process tests with wiremock Graph, as the spike's OAuth
  flow tests do, one per tool covering the happy path and the
  cross-account refusal.
- Renderer: existing fixture snapshots, unchanged.

### 1.9 Read cache

- Per-user, in memory only, 60-second TTL, keyed by user, tool and
  normalized arguments. Covers `mail_overview`, `mail_search`, `mail_read`,
  `mail_attachment` (read mode), `calendar_agenda`, `calendar_availability`,
  `mail_folders`. Bounded to a few hundred entries per user with
  least-recently-used eviction.
- Any write by the user clears their entire cache: draft, send, act,
  respond, event, account changes.
- Not cached: `folder: drafts` listings, `accounts_*`, error results.
- The mailbox token cache and the daily contact cache are separate and
  unchanged. Nothing is persisted; a cold start pays full price once.
- Graph delta queries are deliberately not used for caching yet; revisit if
  measured latency or throttling justifies per-folder sync state.

### 1.10 Attachments

- **Conversion** runs markitdown as a subprocess inside the server
  container: the image moves from distroless to a slim Python base carrying
  the Rust binary and markitdown. One container keeps self-hosting simple.
  Limits: 25 MB input, 30-second timeout, non-root, temp file deleted after
  the call, never stored anywhere else. Unsupported or failed conversions
  report the type and size instead of erroring.
- **Images** (`image/*`) bypass conversion and return as MCP image content
  (base64 in the tool result), capped at 5 MB.
- **Download links** are signed tokens (`typ: download`, user, message id,
  attachment id, 15-minute expiry) served at `GET /dl/<token>` with no other
  authentication; the server streams the bytes from Graph with the original
  filename and content type. The route is rate-limited per user and logs
  only the user hash and outcome.
- **Sending attachments from the harness is out of scope**; forwarding a
  message carries its attachments already.

---

## Part 2 — Platform

### 2.1 Custom domain `pidge.mklab.se`

- DNS at GoDaddy, managed with the `gddy` CLI, subdomain only: `CNAME pidge → ca-pidge-mcp.<env>.
  swedencentral.azurecontainerapps.io` and `TXT asuid.pidge → <verification
  id>`. Apex and `www` untouched.
- Container Apps managed certificate, declared in Bicep after the hostname
  is bound (two-step, handled by the deploy script).
- `PIDGE_MCP_PUBLIC_URL` becomes `https://pidge.mklab.se`; the issuer,
  resource and callback follow. The Entra app gets the new callback as a
  second redirect URI (owner action). Clients re-add the connector once; the
  old hostname keeps working until the new one is verified, then is removed
  from the allowed hosts.

### 2.2 Logs

- JSON structured logs (`tracing-subscriber` json layer) to stdout, collected
  by Container Apps into Log Analytics.
- One line per HTTP request: method, route template, status, latency. One
  line per tool call: tool, duration, outcome, user hash. Sign-in and connect
  events: outcome and user hash.
- Never logged: addresses, subjects, bodies, recipient lists, tokens. Users
  appear as an 8-character hash of the sign-in address, stable across
  restarts. The spike's `email=` fields are removed.
- Health and readiness unchanged. Metrics stay platform-provided.

### 2.3 CI/CD

- Workflow `deploy-mcp.yml`: on push to `main` touching `crates/pidge-mcp`,
  `crates/pidge-client`, `crates/pidge-core`, `deploy/azure`, or `Cargo.*`.
- Steps: OIDC login to Azure as `id-pidge-deploy` (user-assigned identity
  with a federated credential for `repo:mklab-se/pidge:ref:refs/heads/main`),
  Bicep deploy, `az acr build` tagged with the commit SHA, Bicep deploy with
  the image, smoke test (`/healthz`, protected-resource metadata, 401 on
  `/mcp`).
- `deploy/azure/setup-github-oidc.sh` creates the identity, the federated
  credential, and role assignments once: Contributor and Role Based Access
  Control Administrator on the resource group (the latter because the Bicep
  creates role assignments).
- Concurrency group so two pushes don't race.

### 2.4 Hardening carried over from the spike

- The server stays a public client of the Entra app (PKCE, hosted callback
  registered as a public-client redirect URI). It works, it avoids a secret
  that expires, and the token never leaves the server anyway. Revisit only if
  Microsoft stops accepting https public-client redirects.
- Key Vault purge protection on.
- Refresh-token revocation: keep a per-user `token_generation` counter in
  the user record; refresh tokens carry it; bumping it revokes all sessions
  for that user (`accounts_update sign_out_everywhere`).

---

## Part 3 — Docs and distribution

- `docs/mcp.md` (linked from README): what the server is, connecting from
  Claude (web/desktop/mobile), ChatGPT (plugins page, Create MCP App), Claude
  Code (`claude mcp add --transport http`), what the tools do, what is stored
  where.
- `deploy/azure/README.md` grows into the self-hosting guide: prerequisites,
  the Entra app registration (the existing `register-pidge-app.sh` plus the
  callback URI), `deploy.sh`, custom domain, CI/CD setup, tear-down.
- "Anywhere else": the container with `PIDGE_MCP_SECRETS_DIR` on a persistent
  volume, documented as the non-Azure option with its caveats.
- Release workflow publishes `ghcr.io/mklab-se/pidge-mcp:<version>` and
  `:latest` alongside the CLI binaries.
- CLI: `pidge mcp connect <url>` (§1.5) and `pidge mcp status`.

---

## Out of scope

Sending new attachments from the harness, reminders, category management beyond
`mail_act`, multiple calendars as a write target, any server-side model or
server-side ranking, stored aliases or signature/style helpers (the harness
handles these in a skill or in conversation), users outside the allowlist,
multi-replica deployment.
