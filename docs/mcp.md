# MCP server

pidge speaks the [Model Context Protocol](https://modelcontextprotocol.io) in
two ways, with the same tools and the same server instructions:

- **Local** (`pidge mcp serve`): a stdio server for MCP clients on your own
  machine (Claude Desktop, Cowork, Codex, ...). It uses the mailboxes you
  signed in to with `pidge account add`; nothing is hosted.
- **Hosted** (`pidge-mcp`): an HTTP server that gives any MCP-capable
  harness (Claude.ai, Cowork, Claude Code, ChatGPT, mobile, ...) your mail
  and calendar without the harness ever holding your Microsoft or Google
  tokens itself. One deployment can serve many people; each signs in with
  their own Microsoft account and only ever sees their own mailboxes.

Both are thin frontends over one engine (`crates/pidge-engine`): every
operation is implemented once and listed once in its registry, and both
servers build their tool lists and instructions from it, so they cannot
drift apart. Microsoft 365, Outlook.com and Gmail mailboxes are merged: "what's
new" and "what's on my calendar" cover all of them. Gmail needs a Google
OAuth client of your own; see [Gmail and Google Calendar](gmail.md).

Full deployment details, the tool reference with behaviour notes, and day-2
operations for the hosted server live in
[`deploy/azure/README.md`](../deploy/azure/README.md). This page is the
connecting/self-hosting overview and links there for the rest.

## Security model

- **Bearer identity, no user id in tool input.** Every call runs as whoever
  is signed in; a tool can only narrow to one of *that* caller's own
  mailboxes, never someone else's.
- **Ownership is checked before the engine sees a mailbox.** A mailbox
  secret records its owner, and the hosted server hands the engine only
  mailboxes that the caller's account lists *and* whose record names them
  as owner. No operation can reach anything else.
- **Short handles, not provider ids.** Tools show and take short handles
  (`m12` a message, `e3` an event, `d4` a draft, `p5` a plan, `s1` a triage
  session), kept per user. Under the hood they map to Microsoft Graph's
  immutable ids (or Gmail ids), so a handle keeps pointing at the same
  message after it is moved.
- **Third-party content is marked untrusted.** Message bodies, previews,
  event text and converted attachments are wrapped so an agent treats them
  as data to summarize, never as instructions to follow.
- **Changes to other people go through the user.** Bulk and selector-based
  changes, and anything that e-mails other people from the calendar
  (invitations, attendee changes, cancellations), come back as a plan the
  user agrees to first. Sending goes through an outbox with a short hold
  the user can cancel, and sensitive mail waits for a second yes. Outgoing
  mail (sends, unsubscribe e-mails, invitations, cancellations and RSVP
  notes) is capped per hour (30 by default).
- **Nothing identifying is logged** by the hosted server. No e-mail address,
  subject, body, recipient or token value is ever written to logs; a user
  shows up only as a stable 8-character hash.

## Local: `pidge mcp serve`

Sign in to your mailboxes with the CLI first (`pidge account add`, or
`pidge account add --provider google` for Gmail), then point your client at
`pidge mcp serve`. For clients configured with an `mcpServers` JSON block
(Claude Desktop, Cowork and others):

```json
{"mcpServers":{"pidge":{"command":"pidge","args":["mcp","serve"]}}}
```

The local server uses the same accounts, preferences and index as the CLI:
accounts and tokens from `config.yaml` (keychain or token files) and
preferences in `prefs.json` next to it, in `${XDG_CONFIG_HOME:-~/.config}/pidge`
(`%APPDATA%\pidge` on Windows), and the index (`index.db`) in
`${XDG_CACHE_HOME:-~/.cache}/pidge` (`%LOCALAPPDATA%\pidge`). `mail_attachment mode=save`
saves into your Downloads folder, and `accounts action=ai` points the user
at `pidge ai config` for their own AI key. No hosting, allowlist or Entra app
registration is involved.

## Connect a client to a hosted server

Any client needs the server's MCP URL: `https://<your-server>/mcp`. The
reference deployment at `https://pidge.mklab.se/mcp` is invite-only for now;
everyone else can [self-host](#self-hosting-on-azure) (or
[run the container anywhere](#run-the-container-anywhere)) and use their own
host instead, run [pidge locally](#local-pidge-mcp-serve), or skip MCP and
use the `pidge` CLI with an agent skill (see the
[README](../README.md#get-started)).

### Claude.ai, Cowork, mobile

Add it as a custom connector and point it at the URL above. The client
discovers the OAuth endpoints on its own, registers itself with the server,
and takes you to Microsoft sign-in the first time you use it. Nothing else
to configure.

### ChatGPT

Go to `chatgpt.com/plugins`, choose **+**, then **Create MCP App**, and give
it the same URL as a public endpoint. ChatGPT drives the same OAuth
discovery and registration flow.

### Claude Code

```bash
claude mcp add --transport http pidge https://<your-server>/mcp
```

Claude Code opens a browser for the Microsoft sign-in the first time a tool
call needs it.

### Every client, on first sign-in

You sign in to the hosted server with a Microsoft account, and only
addresses on the server's allowlist get past the callback. Once signed in,
add further mailboxes from inside the harness with the `accounts` tool
(`action=connect`, with `provider=google` for Gmail) rather than
reconnecting the client: it returns a link, valid 10 minutes, that the user
opens to sign in to that mailbox. Gmail can be connected only on a server
that has a Google client configured (see [Gmail](gmail.md)).

### Make sure the agent picks pidge

A harness with several connectors chooses by tool names, descriptions and
the server's instructions, often before it has looked at any of them
closely. pidge's instructions state that it *is* the user's e-mail and
calendar (Microsoft and Gmail mailboxes, merged) and that no other mail or
calendar tool should be used, and its mail and calendar tool descriptions
lead with "E-mail:" or "Calendar:". Two things on the client side help
further:

- **Disconnect mail and calendar connectors you don't use.** An idle Gmail
  or Google Calendar connector is a decoy: asked to "send an e-mail", a
  harness may reach for the one whose name says "mail". If you want Gmail,
  connect it to pidge instead.
- **Tell the harness once.** One line in the project's `CLAUDE.md`,
  `AGENTS.md`, claude.ai project instructions, or the harness's memory:

  > E-mail and calendar go through the pidge MCP tools (`mail_*`,
  > `calendar_*`, `accounts`), never another mail connector.

  `pidge mcp connect` prints a line like this when it finishes, ready to
  paste.

## Migrate from local `pidge`

If you already sign in to Microsoft accounts with the CLI, `pidge mcp
connect` moves them onto a hosted server. Your local tokens never leave the
machine: each account is connected by signing in to it once in the browser.

```bash
pidge mcp connect https://<your-server>/mcp
```

It signs you in to the server (a browser opens), then walks every account
already signed in locally, skips any that are already connected, and for
the rest prints a connect link (also opened in your browser) and waits for
it to complete. Add `--yes` to skip the interactive per-account prompts and
just poll each one until it shows connected (when stdin is not a terminal,
as for an agent or a script, `connect` does this even without `--yes`), and
`--store file` to keep the session token in a plaintext file (mode 0600 on
Unix) instead of the OS keychain. `--dry-run` only reads your current state
(a single read of the connected mailboxes) and reports what it would do; it
never signs in, refreshes a token, or connects anything.

Check on the connection later, or from another machine:

```bash
pidge mcp status                       # only one server connected: no url needed
pidge mcp status https://<your-server>/mcp
pidge mcp logout https://<your-server>/mcp
```

`status` and `logout` take an optional `--store keychain|file`: it names
which backend to try first, but the other backend is still tried if the
preferred one has nothing stored. Without `--store`, the preferred backend
is whichever one the local server index remembers for that url, or the OS
keychain if there's no index entry for it, so on a machine with no usable
keychain (headless, no Secret Service), pass `--store file` explicitly, or
`status`/`logout` will fail trying the keychain first. `logout` forgets the
stored session for that server, locally only: it removes the session from
both backends and from the local server index, but revokes nothing on the
server. To revoke every session on the server as well, call the `accounts`
tool with `action=sign_out_everywhere` (see Operations below). `--dry-run`
reports what it would remove without touching anything. If the server has
revoked your session (see
[Revoke a user's sessions](../deploy/azure/README.md#revoke-a-users-sessions)
in the deploy README), `status` exits with code 3 and points you back at
`pidge mcp connect`.

All of `pidge mcp connect/status/logout` write progress to stderr and their
final result to stdout (plain text, or JSON with `--json`), so they compose
in scripts the same way the rest of the CLI does. The session itself is
stored per-server:

- Linux and macOS: `${XDG_CONFIG_HOME:-~/.config}/pidge/mcp/`
- Windows: `%APPDATA%\pidge\mcp\`

## Tools

No tool takes a user id. Every call acts as the signed-in caller (or, for
`pidge mcp serve`, the local user), merging all of their mailboxes unless a
call names one with `account`. There are no MCP prompts; the server
instructions carry the rules and a recipe per common request ("do I have any
new e-mail?", "help me go through my inbox", "archive all receipts", "cancel
all appointments today", ...).

| Tool | What it does |
|---|---|
| `mail_brief` | "Any new e-mail?": an inbox map and the mail that matters since the user last looked, with a reason each, and counts for the bulk. Moves "last seen" unless `mark_seen=false`; `importance=high` for "anything important?". |
| `mail_find` | Find mail by words, sender, time (`since`, `before`), folder, kind (`receipt`, `newsletter`, `promotion`, ...), category, importance, unread, flagged or attachments; `count_only` for "how many". |
| `mail_read` | One message, or its conversation with `thread=true`, its internet headers with `headers=true`; shows attachments and, for invitations, the event id. |
| `mail_triage` | Goes through the inbox with the user: sweeps of bulk mail, then small batches, then one by one, with a suggested action each; `decision=...` after every user answer. |
| `mail_act` | Archive, delete, read, unread, flag, unflag, move, categorize, unsubscribe, classify, or undo, on ids or on a selector (`kind=receipt`). Answers at once; bulk and selector changes return a plan first. |
| `mail_draft` | Write or revise a draft (new, reply, reply_all, forward); recipients may be names of people the user mails with. Returns `d4` and a preview. |
| `mail_send` | Sends a draft through the outbox with a short hold (`action=cancel`, `now`, `approve`, `status`). |
| `mail_attachment` | An attachment as text (documents converted to Markdown), or `mode=save` for a download link (hosted) or a file in Downloads (local). |
| `mail_setup` | Categories, rules, examples, VIPs, muted senders, policy (send, notify, bulk, delete, `bulk_threshold`, `sends_per_hour`) and settings (timezone, default sender, working hours, ...); `label_mail` writes pidge's categories onto recent inbox mail. |
| `calendar_agenda` | Events for today, tomorrow, this or next week, N days or just the next one, across all calendars, in the user's timezone; `pending_only` for unanswered invitations. |
| `calendar_event` | Create (also from an e-mail's `.ics`), update, invite or uninvite people, cancel (attendees told) or delete (silently) one event. |
| `calendar_act` | Many events at once: clear a day, cancel, decline, delete or move; always a plan first. |
| `calendar_respond` | Accept, tentatively accept or decline an invitation (by event or by the invitation e-mail), with an optional note. |
| `calendar_availability` | Free windows of a given length within working hours, optionally with other people's free/busy. |
| `accounts` | The user's mailboxes and their health; `connect` (Microsoft or Google), `disconnect`, `ai` (set up the user's own AI key), `ai_off`, `sign_out_everywhere`. |

How the tools behave together:

- **Changes are fire-and-forget.** `mail_act` updates pidge's index and
  answers at once ("Archived."); the call to Microsoft or Google runs in
  the background on a per-user worker. A failure puts the message back and
  is reported once, as a `Note:` line at the top of the next answer.
- **Plans.** A change to more messages than the policy's `bulk_threshold`
  (5 by default), every selector-based change, `calendar_act`, and anything
  in `calendar_event` that e-mails other people return a plan (`p5`) that
  says what will happen and to whom. The agent tells the user and applies
  it with `plan=p5` when they agree. Plans expire after 15 minutes.
  `mail_act action=undo` reverses the last mail change.
- **Sending.** `mail_send` puts a draft in the outbox and it goes out by
  itself after a hold that grows with risk: 10 s for a short reply to one
  known person, 30 s normally, 2 min for a first mail to an address, more
  than 5 recipients, a forward or attachments. Sensitive mail (what looks
  like an identity, card or IBAN number, words such as "password" or
  "confidential", more than 20 recipients, or the user's own AI judging it
  highly sensitive) waits for `action=approve`.
- **Triage keeps going.** Acting on the current triage message with
  `mail_act` (for example after a reply) also returns the next step.

See [`deploy/azure/README.md`](../deploy/azure/README.md#tools) for the full
behaviour notes (thread mode, reply recipients, rate limits, attachment
limits, download links, consent pages, account identity, unsubscribe
handling).

## Self-hosting on Azure

[`deploy/azure/README.md`](../deploy/azure/README.md) is the source of
truth for standing up your own deployment: `az group create` plus
`deploy/azure/deploy.sh`, custom domains, Gmail, CI/CD, logs and operations.
Two things only the deployment's owner can do, and that the script cannot
do for you:

- **Set the sign-in allowlist.** `PIDGE_MCP_ALLOWED_EMAILS` has no default;
  the deploy script refuses to run without it.
- **Register the callback URL on the Microsoft Entra app** the deployment
  signs users in through, so `https://<your-server>/callback` is an
  accepted redirect URI. The deploy script does this automatically on a
  normal run (skip with `--skip-entra`); if you are self-hosting under your
  own Entra app registration rather than the one built into `pidge-client`,
  point at it with `PIDGE_CLIENT_ID` first.

For Gmail, also create a Google "Web application" OAuth client with the
redirect URI `https://<your-server>/callback/google` and pass it to the
deploy script as `PIDGE_GOOGLE_CLIENT_ID` and `PIDGE_GOOGLE_CLIENT_SECRET`
([Gmail](gmail.md) walks through creating it).

## Run the container anywhere

The server ships as a container built from
[`deploy/azure/Dockerfile`](../deploy/azure/Dockerfile) (an unprivileged
user, listening on port 8080, `pidge-mcp` as the entrypoint). Build it
yourself from a checkout of the tag you want to run:

```bash
git clone https://github.com/mklab-se/pidge.git && cd pidge
git checkout v2.0.0   # or any later release tag
docker build -f deploy/azure/Dockerfile -t pidge-mcp:2.0.0 .
```

The release workflow also pushes the same image to
`ghcr.io/mklab-se/pidge-mcp`, but that package is private and used only by
the maintainer; there is no plan to make it public, so do not depend on it.

```bash
docker run -d \
  -p 8080:8080 \
  -e PIDGE_MCP_PUBLIC_URL=https://your-host.example.com \
  -e PIDGE_MCP_ALLOWED_EMAILS=you@example.com,teammate@example.com \
  -e PIDGE_MCP_SECRETS_DIR=/data/secrets \
  -e PIDGE_MCP_INDEX_DIR=/data/index \
  -e PIDGE_CLIENT_ID=<your Entra app id> \
  -v pidge-mcp-data:/data \
  pidge-mcp:2.0.0
```

Add `-e PIDGE_GOOGLE_CLIENT_ID=... -e PIDGE_GOOGLE_CLIENT_SECRET=...` (both
or neither) to allow Gmail mailboxes.

Users sign in to Microsoft through the Entra app that `PIDGE_CLIENT_ID`
names, and Microsoft only redirects to callback URLs registered on it, so
add `https://<host>/callback` as a public-client redirect URI on an app you
control. [`scripts/register-pidge-app.sh`](../scripts/register-pidge-app.sh)
creates such an app, and [Cutover](../deploy/azure/README.md#cutover) in the
deploy README shows the `az ad app update` command that registers a
callback. Without it, sign-in fails with `AADSTS50011`.

Put a TLS-terminating reverse proxy in front of it: `PIDGE_MCP_PUBLIC_URL`
must be `https` (only `http://localhost` is exempt, for local testing).
What the server keeps:

- `PIDGE_MCP_SECRETS_DIR` holds the signing key, every mailbox's tokens and
  users' AI keys as plaintext files, so mount it on a persistent volume with
  restricted permissions rather than the container's writable layer; on
  Azure the same role is filled by Key Vault (`PIDGE_MCP_KEYVAULT_URL`).
- Each user's preferences (categories, rules, VIPs, policy, timezone,
  default sender) are one small JSON file. With a secrets dir and no other
  setting they go to `<secrets dir>/prefs`; `PIDGE_MCP_PREFS_DIR` puts them
  elsewhere, and on Azure they are blobs (`PIDGE_MCP_PREFS_URL`).
- Each user's index (`PIDGE_MCP_INDEX_DIR`, default a temp directory) is a
  cache rebuilt from the mailboxes, but it also holds short-lived state:
  handles, plans, the undo log, the outbox and the hourly send count. Keep
  it on the volume, as above, so a restart does not drop held sends.

See [Configuration](../deploy/azure/README.md#configuration) in the deploy
README for every other environment variable, including
`PIDGE_MCP_LOG_FORMAT`, `PIDGE_MCP_ALT_HOSTS` and `PIDGE_MCP_LEGACY_ISSUERS`
for domain changes.

## Operations

- **Logs.** Every log line is one JSON object on stdout (`PIDGE_MCP_LOG_FORMAT=text`
  for human-readable local logs instead). See
  [Logs](../deploy/azure/README.md#logs) for the two structured events
  (`http_request`, `tool_call`) and example KQL queries against Azure Log
  Analytics.
- **Check markitdown conversion** without starting the server:
  ```bash
  pidge-mcp --convert-check sample.pdf
  ```
- **Revoke a user's sessions.** Call the `accounts` tool with
  `action=sign_out_everywhere` to invalidate every access and refresh token
  already issued to that user, without touching anyone else's. See
  [Revoke a user's sessions](../deploy/azure/README.md#revoke-a-users-sessions)
  for exactly what it does and does not affect.
