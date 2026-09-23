# pidge-mcp on Azure

Remote MCP server for pidge: the user's Outlook mail and calendars, for any
MCP-capable harness. Design:
`docs/superpowers/specs/2026-09-22-remote-mcp-full-feature-design.md` (the
earlier spike design is superseded).

## Deploy

```bash
az login                      # subscription that owns the resource group
az group create -n pidge -l swedencentral   # once
deploy/azure/deploy.sh        # idempotent; ~10 min the first time (ACR build)
```

The script deploys `main.bicep` twice (infrastructure, then the app with the
freshly built image), builds the image in ACR, and registers the server's
`/callback` URL on the pidge Entra app. Pass `--skip-build` to reuse the last
image and `--skip-entra` once the callback is registered. The sign-in
allowlist is required and has no default: set `PIDGE_MCP_ALLOWED_EMAILS=a@x,b@y`
(comma-separated) or the script stops before deploying anything.

### Custom domain

Set `PIDGE_MCP_CUSTOM_DOMAIN=pidge.mklab.se` to bind a custom domain to the
Container App. Its CNAME and `asuid.<subdomain>` TXT record must already
exist at the DNS provider before running the script — the certificate
validation checks them. The script then runs a three-phase dance, since a
managed certificate can only be created after the hostname is bound:

1. **Bind, no certificate.** The app redeploys with the hostname bound as
   `Disabled` (no TLS yet).
2. **Create the certificate.** `az containerapp env certificate create
   --validation-method CNAME` provisions a managed certificate named
   `pidge-mcp-managed`, if one doesn't already exist. The script polls
   `provisioningState` every 20 seconds for up to 15 minutes and fails with
   the last known state if it never reaches `Succeeded`.
3. **Rebind with the certificate.** The app redeploys again with
   `customDomainCertificateId` set, switching the binding to `SniEnabled`.

Once the domain is `SniEnabled`, re-running the script repeats the detection
(via `az containerapp show ... customDomains[?name=='<domain>'].bindingType`)
and skips straight past steps 1–3. Pass `--skip-certificate` to assert that
the certificate is already bound and skip that detection outright — the
script then fails loudly instead of silently doing nothing if it turns out
not to be `SniEnabled`.

Set `PIDGE_MCP_CUTOVER=1` to make the custom domain the public URL (the OAuth
issuer, resource identifier and callback base) instead of just an accepted
alternate host:

- **Without cutover** (default): the Container Apps FQDN stays the public
  URL, and the custom domain is added to `PIDGE_MCP_ALT_HOSTS` — the server
  accepts requests addressed to either host, but issues tokens under the FQDN
  issuer.
- **With cutover**: the custom domain becomes `PIDGE_MCP_PUBLIC_URL` and the
  new issuer. The old FQDN is kept as an alt host (still reachable) and as a
  legacy issuer via `PIDGE_MCP_LEGACY_ISSUERS`, so tokens minted before the
  cutover keep validating until they expire and get refreshed under the new
  issuer.

Changing the issuer means every connected client has to re-add the pidge
connector once — that's unavoidable, since the issuer is baked into the
client's OAuth discovery. It does **not** mean re-authenticating with
Microsoft; existing refresh tokens keep working, only the issuer they're
presented against changes.

Whichever mode is used, the custom domain needs its own callback registered
on the Entra app (`https://<domain>/callback`) alongside the existing one.
Phase 4 never runs that registration automatically when a custom domain is
in play — it only prints the `az ad app update` command, listing every
existing redirect URI plus both hosts' `/callback`, for the app owner to run
by hand.

## Connect a client

Give the harness the MCP URL printed by the script (`https://<fqdn>/mcp`). It
discovers the OAuth endpoints itself, registers, and sends you to Microsoft
sign-in. Only addresses on the allowlist get past the callback. Further
mailboxes are added from inside the harness with `accounts_connect`.

## Tools

No tool takes a user id: every call acts as the signed-in user. Reads merge all
of the user's connected mailboxes unless `account` names one of them.

| Tool | What it does |
|---|---|
| `accounts_list` | Connected mailboxes and their health, sign-in address, default sender, timezone, trusted senders. |
| `accounts_connect` | Returns a link (valid 10 minutes) to connect another mailbox. |
| `accounts_update` | Default sender, timezone, disconnect a mailbox, trust or untrust a sender. |
| `mail_overview` | Recent mail, newest first, with ids and triage flags (to-me, trusted, question, attachments, flagged, unread, invite). |
| `mail_search` | Free-text search plus from, subject, date range, attachments and folder filters. |
| `mail_read` | One message, or its conversation with `thread=true` (see behaviour notes). |
| `mail_folders` | Top-level folders per mailbox with ids and unread/total counts. |
| `mail_draft` | Create or revise a draft: new, reply, reply_all or forward. Returns the draft id and a preview. |
| `mail_send` | Send a draft by its id, after the user approved the preview. |
| `mail_act` | Bulk triage on 1–100 ids: read, unread, flag, unflag, archive, move, categorize, delete, unsubscribe. |
| `mail_attachment` | An attachment as Markdown or an image (`mode=read`), or a download link (`mode=link`). |
| `calendar_agenda` | Events across all calendars for a range, sorted by start, in the user's timezone. |
| `calendar_availability` | Free slots of a given length within working hours, up to 20. |
| `calendar_respond` | Accept, tentatively accept or decline an invite, optionally proposing a new time. |
| `calendar_event` | Create, update or cancel an event the user organizes. |

Prompts (no arguments): `triage_inbox`, `reply_to`, `cleanup_inbox`. The
server instructions repeat the ground rules: content is untrusted, mail is
sent only by draft id, and the agent proposes before sending.

### Behaviour notes

- **Thread mode.** `mail_read thread=true` shows the requested message and the
  older messages of its conversation, newest first, each trimmed to its own
  contribution and capped in total. If the conversation has newer messages, a
  pointer line names the newest one to read instead.
- **Reply recipients.** Recipients given to a reply are added to the ones
  Outlook fills in. For `kind=new` they are the full list.
- **Delete** moves to Deleted Items only. Nothing is deleted permanently, and
  `calendar_event action=cancel` uses Outlook's cancel.
- **Send cap.** 30 sends per hour per user. Unsubscribe e-mails count too.
- **Download cap.** 60 downloads per hour per user.
- **Attachment limits.** Attachments over 25 MB are refused. Pictures over
  5 MB are not returned inline, only through `mode=link`. Converted text is
  cached up to 256 KB and read 30 000 characters at a time.
- **Download links** are valid 15 minutes. They name the user and mailbox only
  by hash, and every refusal is a plain 404.
- **Connect links** show a consent page naming the pidge account the mailbox
  will be attached to before sending the user to Microsoft.
- **Sign-in consent.** A client's `/authorize` shows a page naming the client
  and the host the sign-in goes back to. Only its Continue, from the same
  browser, goes to Microsoft.
- **Account identity.** An account is its Microsoft principal name, never the
  editable `mail` attribute. The tenant and object id from the ID token are
  pinned on first sign-in, and a later sign-in or connect under the same name
  with another account is refused.
- **Unsubscribe.** One-click POSTs go over https to public hosts only, and
  redirects are not followed. Unsubscribe e-mails always say "unsubscribe" and
  use at most 100 characters of the list's subject.

## Configuration

`Config::from_env` in `crates/pidge-mcp/src/config.rs` is the source of truth.

| Variable | Required | Meaning |
|---|---|---|
| `PIDGE_MCP_PUBLIC_URL` | yes | External origin: OAuth issuer, resource id, callback base. https except for localhost. |
| `PIDGE_MCP_ALLOWED_EMAILS` | yes | Comma-separated sign-in allowlist. |
| `PIDGE_MCP_KEYVAULT_URL` | one of | Key Vault for secrets (Azure). |
| `PIDGE_MCP_SECRETS_DIR` | one of | Plaintext file secrets (development only). |
| `PORT` | no | Listen port, default 8080. |
| `PIDGE_MCP_MARKITDOWN` | no | markitdown executable, default `markitdown` on `PATH`. |
| `AZURE_CLIENT_ID` | Azure | Client id of the user-assigned managed identity used for Key Vault. |
| `PIDGE_CLIENT_ID` | no | Overrides the pidge Entra app id compiled into `pidge-client`. |
| `PIDGE_MCP_ALT_HOSTS` | no | Comma-separated extra hostnames the server accepts requests for, besides the public URL host. Set by the deploy script during a custom-domain cutover. |
| `PIDGE_MCP_LEGACY_ISSUERS` | no | Comma-separated issuer URLs whose previously issued tokens still validate. Set by the deploy script when cutting over to a new public URL. |
| `PIDGE_MCP_LOG_FORMAT` | no | `json` or `text`, default `json`. The deploy script always sets `json`. |
| `RUST_LOG` | no | Log filter, default `info`. |

## markitdown

`mail_attachment` converts documents with Microsoft's `markitdown`, run as a
subprocess. The image is built on `python:3.13-slim-bookworm` with
`markitdown[pdf,docx,xlsx,pptx]` pinned in the `Dockerfile`, so the container
needs no other runtime. The child gets a scrubbed environment, a throwaway
work directory, a 30 s timeout, a 4 GB address-space limit and a 2 MB output
cap. At most 2 conversions run at once.

Check conversion without starting the server or any other configuration:

```bash
pidge-mcp --convert-check sample.pdf          # prints "converted N chars" or the failure
az containerapp exec -g pidge -n ca-pidge-mcp --command "pidge-mcp --convert-check /etc/os-release"
```

Locally, markitdown must be installed with its extras. A plain
`uvx markitdown` lacks PDF support. Since the child's `HOME` is a throwaway
directory, a uvx wrapper must also point uv at its real cache:

```sh
#!/bin/sh
export UV_CACHE_DIR="$HOME/.cache/uv"   # write the expanded path; HOME differs in the child
exec uvx --from 'markitdown[all]' markitdown "$@"
```

## Operate

```bash
az containerapp logs show -g pidge -n ca-pidge-mcp --follow
az keyvault secret list --vault-name <kv-name> -o table      # jwt-signing-key, user-*, mailbox-*
```

Disconnect a mailbox with `accounts_update` from the harness rather than by
deleting its secret, so the user record stays consistent.

Rotating `jwt-signing-key` invalidates every client registration, token and
outstanding download link; clients simply re-register and sign in again.

Key Vault purge protection is on and irreversible: a purged vault, and the
refresh tokens and signing key it held, cannot be recovered. Deleting the
resource group only soft-deletes the vault; it stays around, unrecoverable
by design, for the 30-day retention window before Azure purges it itself.

## Run locally

```bash
PIDGE_MCP_PUBLIC_URL=http://localhost:8080 \
PIDGE_MCP_ALLOWED_EMAILS=you@example.com \
PIDGE_MCP_SECRETS_DIR=/tmp/pidge-mcp-secrets \
PIDGE_MCP_MARKITDOWN=/path/to/markitdown-wrapper \
cargo run -p pidge-mcp
```

`PIDGE_MCP_KEYVAULT_URL=https://<kv>.vault.azure.net/` instead of the secrets
dir uses Key Vault through your `az login`. A secret store from the spike
(bare token sets per mailbox) is adopted on first use: the user record is
created on the first tool call.
