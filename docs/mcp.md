# Remote MCP server

`pidge-mcp` is a hosted [Model Context Protocol](https://modelcontextprotocol.io)
server that gives any MCP-capable harness (Claude.ai, Cowork, Claude Code,
ChatGPT, ...) the same mail and calendar tools the `pidge` CLI has, over
HTTP, without the harness ever holding your Microsoft tokens itself. One
deployment can serve many people; each signs in with their own Microsoft
account and only ever sees their own mailboxes.

Full deployment details, the tool and prompt reference, and day-2 operations
live in [`deploy/azure/README.md`](../deploy/azure/README.md). This page is
the connecting/self-hosting overview and links there for the rest.

## Security model

- **Bearer identity, no user id in tool input.** Every call runs as whoever
  is signed in; a tool can only narrow to one of *that* caller's own
  mailboxes, never someone else's.
- **Ownership is checked on every mailbox lookup.** A mailbox secret records
  its owner, and the server only uses one that both the caller's account
  lists and the record names them as owner of.
- **Third-party content is marked untrusted.** Message bodies, previews,
  event text and converted attachments are wrapped so an agent treats them
  as data to summarize, never as instructions to follow.
- **Sending is deliberately narrow.** Only one tool sends mail, only by an
  existing draft id, capped at 30 sends per hour per user (unsubscribe
  e-mails count too).
- **Nothing identifying is logged.** No e-mail address, subject, body,
  recipient or token value is ever written to logs; a user shows up only as
  a stable 8-character hash.

## Connect a client

Any client needs the server's MCP URL: `https://<your-server>/mcp`. The
reference deployment for this repository's maintainer runs at
`https://pidge.mklab.se/mcp`; if you are self-hosting, use your own host
instead.

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

Only addresses on the server's allowlist get past the callback. Once
signed in, add further mailboxes from inside the harness with the
`accounts_connect` tool rather than reconnecting the client.

## Migrate from local `pidge`

If you already sign in to Microsoft accounts with the CLI, `pidge mcp
connect` moves them onto a hosted server. Your local Microsoft tokens never
leave the machine: each account is connected by signing in to Microsoft
once in the browser.

```bash
pidge mcp connect https://<your-server>/mcp
```

It signs you in to the server (a browser opens), then walks every account
already signed in locally, skips any that are already connected, and for
the rest prints a connect link (also opened in your browser) and waits for
it to complete. Add `--yes` to skip the interactive per-account prompts and
just poll each one until it shows connected, and `--store file` to keep the
session token in a plaintext file (mode 0600 on Unix) instead of the OS
keychain. `--dry-run` only reads your current state (a single
`accounts_list` call) and reports what it would do; it never signs in,
refreshes a token, or connects anything.

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
keychain if there's no index entry for it — so on a machine with no usable
keychain (headless, no Secret Service), pass `--store file` explicitly, or
`status`/`logout` will fail trying the keychain first. `logout` forgets the
stored session for that server, locally only: it removes the session from
both backends and from the local server index, but revokes nothing on the
server. To revoke every session on the server as well, call the
`accounts_update` tool with `sign_out_everywhere: true` (see Operations
below). `--dry-run` reports what it would remove without touching
anything. If the server has revoked your session (see
[Revoke a user's sessions](../deploy/azure/README.md#revoke-a-users-sessions)
in the deploy README), `status` exits with code 3 and points you back at
`pidge mcp connect`.

All of `pidge mcp connect/status/logout` write progress to stderr and their
final result to stdout (plain text, or JSON with `--json`), so they compose
in scripts the same way the rest of the CLI does. The session itself is
stored per-server:

- macOS: `~/Library/Application Support/pidge/mcp/`
- Linux: `~/.config/pidge/mcp/`

## Tools and prompts

No tool takes a user id. Every call acts as the signed-in caller, merging
all of their connected mailboxes unless a call names one with `account`.

| Tool | What it does |
|---|---|
| `accounts_list` | Connected mailboxes and their health, sign-in address, default sender, timezone, trusted senders. |
| `accounts_connect` | Returns a link (valid 10 minutes) to connect another mailbox. |
| `accounts_update` | Default sender, timezone, disconnect a mailbox, trust or untrust a sender. |
| `mail_overview` | Recent mail, newest first, with ids and triage flags. |
| `mail_search` | Free-text search plus from, subject, date range, attachments and folder filters. |
| `mail_read` | One message, or its conversation with `thread=true`. |
| `mail_folders` | Top-level folders per mailbox with ids and unread/total counts. |
| `mail_draft` | Create or revise a draft: new, reply, reply_all or forward. |
| `mail_send` | Send a draft by its id, after the user approved the preview. |
| `mail_act` | Bulk triage on 1-100 ids: read, unread, flag, unflag, archive, move, categorize, delete, unsubscribe. |
| `mail_attachment` | An attachment as Markdown or an image, or a download link. |
| `calendar_agenda` | Events across all calendars for a range, in the user's timezone. |
| `calendar_availability` | Free slots of a given length within working hours. |
| `calendar_respond` | Accept, tentatively accept or decline an invite, optionally proposing a new time. |
| `calendar_event` | Create, update or cancel an event the user organizes. |

Prompts (no arguments): `triage_inbox`, `reply_to`, `cleanup_inbox`.

See [`deploy/azure/README.md`](../deploy/azure/README.md#tools) for the full
behaviour notes (thread mode, reply recipients, rate limits, attachment
limits, download links, consent pages, account identity, unsubscribe
handling).

## Self-hosting on Azure

[`deploy/azure/README.md`](../deploy/azure/README.md) is the source of
truth for standing up your own deployment: `az group create` plus
`deploy/azure/deploy.sh`, custom domains, CI/CD, logs and operations. Two
things only the deployment's owner can do, and that the script cannot do
for you:

- **Set the sign-in allowlist.** `PIDGE_MCP_ALLOWED_EMAILS` has no default;
  the deploy script refuses to run without it.
- **Register the callback URL on the Microsoft Entra app** the deployment
  signs users in through, so `https://<your-server>/callback` is an
  accepted redirect URI. The deploy script does this automatically on a
  normal run (skip with `--skip-entra`); if you are self-hosting under your
  own Entra app registration rather than the one built into `pidge-client`,
  point at it with `PIDGE_CLIENT_ID` first.

## Run the container anywhere

Every tagged release publishes a container image to
`ghcr.io/mklab-se/pidge-mcp`, built from
[`deploy/azure/Dockerfile`](../deploy/azure/Dockerfile) (an unprivileged
user, listening on port 8080, `pidge-mcp` as the entrypoint). `:latest`
always points at the newest non-prerelease version; a version tag like
`:1.5.0` pins to that release. The package is private until the repository
owner makes it public in GHCR's package settings. Until then, pulling it
needs a GitHub token with read access.

```bash
docker run -d \
  -p 8080:8080 \
  -e PIDGE_MCP_PUBLIC_URL=https://your-host.example.com \
  -e PIDGE_MCP_ALLOWED_EMAILS=you@example.com,teammate@example.com \
  -e PIDGE_MCP_SECRETS_DIR=/data/secrets \
  -e PIDGE_CLIENT_ID=<your Entra app id> \
  -v pidge-mcp-secrets:/data/secrets \
  ghcr.io/mklab-se/pidge-mcp:latest
```

Users sign in to Microsoft through the Entra app that `PIDGE_CLIENT_ID`
names, and Microsoft only redirects to callback URLs registered on it, so
add `https://<host>/callback` as a public-client redirect URI on an app you
control. [`scripts/register-pidge-app.sh`](../scripts/register-pidge-app.sh)
creates such an app, and [Cutover](../deploy/azure/README.md#cutover) in the
deploy README shows the `az ad app update` command that registers a
callback. Without it, sign-in fails with `AADSTS50011`.

Put a TLS-terminating reverse proxy in front of it: `PIDGE_MCP_PUBLIC_URL`
must be `https` (only `http://localhost` is exempt, for local testing).
`PIDGE_MCP_SECRETS_DIR` holds the signing key and every mailbox's tokens as
plaintext files, so mount it on a persistent volume with restricted
permissions rather than the container's writable layer; on Azure the same
role is filled by Key Vault (`PIDGE_MCP_KEYVAULT_URL`) instead. See
[Configuration](../deploy/azure/README.md#configuration) in the deploy
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
- **Revoke a user's sessions.** Call the `accounts_update` tool with
  `sign_out_everywhere: true` to invalidate every access and refresh token
  already issued to that user, without touching anyone else's. See
  [Revoke a user's sessions](../deploy/azure/README.md#revoke-a-users-sessions)
  for exactly what it does and does not affect.
