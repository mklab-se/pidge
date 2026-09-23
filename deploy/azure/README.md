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
validation checks them:

| Record | Name | Value |
|---|---|---|
| CNAME | `pidge` (the subdomain) | the Container App's FQDN, e.g. `ca-pidge-mcp.<env>.swedencentral.azurecontainerapps.io` (from the deploy summary, or `az containerapp show -g pidge -n ca-pidge-mcp --query properties.configuration.ingress.fqdn -o tsv`) |
| TXT | `asuid.pidge` | the Container Apps environment's custom domain verification id |

Read the verification id with:

```bash
az containerapp env show -g pidge -n cae-pidge \
  --query properties.customDomainConfiguration.customDomainVerificationId -o tsv
```

The script then runs a three-phase dance, since a
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

Once the domain is `SniEnabled`, re-running the script sees that on the
live app and skips straight past steps 1–3, reusing the live certificate id.
Pass `--skip-certificate` to assert that: the script then fails loudly,
instead of starting the certificate flow, if the domain turns out not to be
`SniEnabled`.

**The script preserves live state.** Before changing anything it reads the
live app (`az containerapp show`): the bound custom domain, its binding type
and certificate id, and the `PIDGE_MCP_PUBLIC_URL` and
`PIDGE_MCP_LEGACY_ISSUERS` env values. An input you leave unset keeps what
is deployed; only an explicit value changes it.

| Input | Unset or empty | Explicit values |
|---|---|---|
| `PIDGE_MCP_CUSTOM_DOMAIN` | Keeps the bound domain and its certificate ("keeping bound custom domain X"), or none if none is bound | `<domain>` binds it (it must match the bound domain, if any). `-` unbinds the bound domain |
| `PIDGE_MCP_CUTOVER` | Stays cut over if the live public URL is `https://<custom domain>`, otherwise stays on the FQDN | `1` cuts over. `0` uses the FQDN, reverting a cutover if there was one |

The script refuses, with an error and before touching anything, to:

- unbind the domain that is the live public URL without also getting
  `PIDGE_MCP_CUTOVER=0`;
- bind a domain other than the one already bound (unbind it first);
- cut over with no custom domain;
- take any `PIDGE_MCP_CUTOVER` value other than `1`, `0` or unset.

`PIDGE_MCP_LEGACY_ISSUERS` is carried over from the live app on every run,
plus the issuer the run retires, if any. The current public URL is never
listed as its own legacy issuer. So a grace given by an earlier cutover or
revert survives later deploys. CI sets neither input, so it can never
change the domain or the cutover state.

### Cutover

Run the script once with `PIDGE_MCP_CUTOVER=1` to make the custom domain
the public URL (the OAuth issuer, resource identifier and callback base)
instead of just an accepted alternate host. Later runs, CI included, stay
cut over without the variable:

- **Without cutover** (default): the Container Apps FQDN stays the public
  URL, and the custom domain is added to `PIDGE_MCP_ALT_HOSTS` — the server
  accepts requests addressed to either host, but issues tokens under the FQDN
  issuer.
- **With cutover**: the custom domain becomes `PIDGE_MCP_PUBLIC_URL` and the
  new issuer. The old FQDN is kept as an alt host (still reachable) and as a
  legacy issuer via `PIDGE_MCP_LEGACY_ISSUERS`.

What the legacy-issuer grace covers, for a connector added before the
cutover (configured with `https://<fqdn>/mcp`):

- **Access tokens** minted under the old issuer keep validating at `/mcp`
  until they expire (at most one hour).
- **Refresh keeps working.** `/token` (and `/authorize`) accept the old
  `resource` value, `https://<fqdn>/mcp`, as well as the current one, for
  every configured legacy issuer. The tokens they hand back are always
  minted for the *current* issuer and resource, and those validate too.
  So a connector that just keeps refreshing keeps working for as long as
  the FQDN stays in `PIDGE_MCP_LEGACY_ISSUERS`, with no re-add and no new
  Microsoft sign-in.

What still needs the connector re-added with the new URL: a client that
re-runs OAuth discovery against the old host. The protected-resource
metadata served there names the *new* resource (`https://<domain>/mcp`),
and a client that follows RFC 9728 rejects metadata whose `resource`
differs from the URL it was configured with. That happens when the
client's refresh token is gone (expired after 30 days unused, revoked by
a sign-out everywhere, or discarded by the client) and it has to start a
fresh authorization. Re-adding the connector with `https://<domain>/mcp`
fixes it for good.

Whichever mode is used, the custom domain needs its own callback registered
on the Entra app (`https://<domain>/callback`) alongside the existing one,
**before** setting `PIDGE_MCP_CUTOVER=1` — otherwise sign-ins against the
new issuer have nowhere valid to redirect to. Phase 4 never runs that
registration automatically when a custom domain is in play — it only prints
the `az ad app update` command, listing every existing redirect URI plus
both hosts' `/callback`, for the app owner to run by hand. It has this
shape (every URI already on the app, plus the missing one(s) appended):

```bash
az ad app update --id <entra-app-id> --public-client-redirect-uris \
  https://<container-apps-fqdn>/callback <any-other-already-registered-uri> \
  https://<custom-domain>/callback
```

The first two are already registered and only being re-listed (Entra
replaces the whole set on every `update`, so anything left out would be
removed); the custom domain's `/callback` is the one actually being added.

**Rollback.** Run the script with `PIDGE_MCP_CUTOVER=0`. Leaving it unset
keeps the cutover. The Container Apps FQDN becomes the public URL and issuer
again, and the custom domain reverts to an accepted alt host, so it keeps
resolving and serving traffic throughout. The custom-domain origin is added
to `PIDGE_MCP_LEGACY_ISSUERS`, so tokens minted under it during the cutover
keep validating, and clients configured with `https://<domain>/mcp` can
still refresh. To unbind the domain as well, pass
`PIDGE_MCP_CUSTOM_DOMAIN=-` in the same run.

## CI/CD

`.github/workflows/deploy-mcp.yml` redeploys `pidge-mcp` whenever CI (the
`CI` workflow) finishes successfully on `main` — every green CI run on
`main` deploys, not every push, so a commit that compiles but fails tests or
clippy is never deployed. It checks out the commit CI just tested
(`github.event.workflow_run.head_sha`) and tags the image with that SHA. It
can also be run manually from the Actions tab (`workflow_dispatch`), which
deploys the checked-out branch's current commit — useful for a redeploy
that doesn't need a new commit, e.g. after rotating
`PIDGE_MCP_ALLOWED_EMAILS` (see below). Runs are serialized: a `deploy-mcp`
concurrency group with `cancel-in-progress: false` means a second trigger
queues behind a deploy already in progress rather than racing or
cancelling it. The `workflow_run` trigger only fires for a `CI` run whose
event was a `push` to `main` in this repository (never a pull-request run,
including one from a fork) — see the comment above the job's `if:` in
`deploy-mcp.yml` for why each clause matters.

The trigger has no path filter, so every green CI run on `main` deploys —
including a commit that only touches the CLI or docs and doesn't change
`pidge-mcp` at all. That's deliberate rather than an oversight, and it costs
one ACR image build per push to `main`. If two pushes land close together,
a "guard against out-of-order completions" step compares the commit it's
about to deploy against the current tip of `main` and skips as superseded
if a newer commit has already landed — so `main`'s tip is always what ends
up deployed, even if an older run's CI happens to finish last.

The workflow authenticates to Azure via GitHub's OIDC federation — no Azure
credential is stored in GitHub. It runs
`deploy/azure/deploy.sh --skip-entra --skip-certificate`, then smoke-tests
the result: `/healthz` must return `ok`, the issuer in
`/.well-known/oauth-authorization-server` must equal the deployed URL, the
`resource` in `/.well-known/oauth-protected-resource` must equal
`<url>/mcp`, and an unauthenticated `POST /mcp` must return 401 — each check
retries a few times to ride out a cold start. `--skip-entra` always skips
Phase 4 (Entra callback registration) entirely: the deploy identity has no
Microsoft Graph directory-read rights, which even reading the app's
existing redirect URIs requires, so CI must not touch that step at all, and
the callback URI is registered once, by hand, during the initial deploy.
`--skip-certificate` asserts the custom domain (if any) already has a
`SniEnabled` certificate binding instead of trying to provision one from CI —
that flow polls for up to 15 minutes and is meant to be run interactively by
a human once, per [Custom domain](#custom-domain). With a live
`SniEnabled` binding the script reuses its certificate id, and with no
custom domain bound or requested the flag does nothing. Since CI leaves
`PIDGE_MCP_CUSTOM_DOMAIN` and `PIDGE_MCP_CUTOVER` unset (unless you create
repository variables for them), every CI deploy keeps the live domain,
certificate and cutover state as they are.

### One-time setup

Before the workflow can run, `deploy/azure/setup-github-oidc.sh` wires up the
trust relationship and the repository configuration it reads. Run it once,
by hand, from a workstation logged in with both `az login` and
`gh auth login`:

```bash
PIDGE_MCP_ALLOWED_EMAILS=a@x,b@y deploy/azure/setup-github-oidc.sh
```

It's idempotent — re-running it after partial setup, or to rotate the
allowlist, is safe. Rotating the allowlist only updates the GitHub secret;
it takes effect on the *next* deploy, so trigger one afterwards with
`workflow_dispatch` if you're not already about to push. It creates, if
missing:

- A user-assigned managed identity, `id-pidge-deploy`, in the resource
  group.
- A federated credential on that identity, `github-main`, trusting GitHub
  Actions runs for `repo:mklab-se/pidge:ref:refs/heads/main` (issuer
  `https://token.actions.githubusercontent.com`, audience
  `api://AzureADTokenExchange`) — this is what lets the workflow get an
  Azure access token with no stored secret.
- Two role assignments for that identity on the resource group:
  `Contributor` (to deploy the Bicep template and build images) and
  `Role Based Access Control Administrator` (because the Bicep template
  itself creates role assignments, e.g. ACR pull and Key Vault Secrets
  Officer, for the app's own identity) — conditioned so it can only
  assign or remove those same two roles (AcrPull, Key Vault Secrets
  Officer), never anything broader like Owner. Re-running the script
  replaces an older, unconditioned assignment from before this condition
  existed.

It then always sets the GitHub repository configuration the workflow reads:

| Name | Kind | Value |
|---|---|---|
| `AZURE_CLIENT_ID` | variable | `id-pidge-deploy`'s client id |
| `AZURE_TENANT_ID` | variable | the Azure AD tenant id |
| `AZURE_SUBSCRIPTION_ID` | variable | the subscription id |
| `PIDGE_MCP_ALLOWED_EMAILS` | secret | from the environment variable of the same name (required; the script refuses to run without it, and never echoes the value) |
| `PIDGE_MCP_CUSTOM_DOMAIN` | variable | from the environment variable of the same name, only if set |

**Resources outside `main.bicep`.** Everything the server runs on is
declared in `main.bicep`, with three exceptions, all created by scripts:

- **The deploy identity**, `id-pidge-deploy`, with its federated credential
  and its two role assignments (`setup-github-oidc.sh`). It is what runs
  the Bicep deployment from CI, so it has to exist before the first CI
  run. Declaring it in the template it deploys would also mean the
  identity re-applies its own federated credential and its own
  Contributor and RBAC Administrator grants on every run, and the RBAC
  condition deliberately forbids it from assigning those roles. It
  carries the same tags as the Bicep resources, with
  `managed-by=script`.
- **The managed certificate**, `pidge-mcp-managed` (`deploy.sh`, Phase
  3b). A managed certificate can only be issued after the hostname is
  bound to the app, and issuing it takes minutes of polling, so the
  script creates it between two Bicep deployments and passes its id back
  in as `customDomainCertificateId`.

`PIDGE_MCP_CUTOVER` isn't set by the script, and doesn't need to be a
repository variable at all: cut over (or back) with one local run of
`deploy.sh`, and CI keeps that state from then on. If you do create
`PIDGE_MCP_CUTOVER` or `PIDGE_MCP_CUSTOM_DOMAIN` as repository variables,
the workflow passes them on and every CI deploy applies them, including a
stale `0` that reverts a cutover. The `PIDGE_MCP_CUSTOM_DOMAIN` variable the
setup script sets is harmless as long as it names the bound domain.

## Logs

`PIDGE_MCP_LOG_FORMAT` (`json` or `text`, default `json`; the deploy script
always sets `json`) picks the tracing subscriber's output format. Every log
line goes to stdout, which the Container Apps environment collects into the
`log-pidge` Log Analytics workspace as `ContainerAppConsoleLogs_CL`
(`Log_s` holds the raw line, `ContainerAppName_s` is `ca-pidge-mcp`).

A JSON line is a flat object: `timestamp`, `level`, `target`, `message`
(the event name), plus that event's own fields at the top level — there is
no nested `fields` object. Two structured events, one line per occurrence:

- **`http_request`**, one per HTTP request: `method`, `route`, `status`,
  `latency_ms`. `route` is the route template the request matched (for
  example `/authorize`, `/mcp`, `/.well-known/oauth-protected-resource`),
  never the raw path: no query string, since `/authorize` and `/callback`
  carry OAuth state and codes there, and nothing a caller typed into the
  path. The download route is logged as `/dl/<redacted>` because its token
  is a bearer credential. A request that matched no route is logged as
  `<unmatched>`.
- **`tool_call`**, one per MCP tool invocation: `tool` (the tool name),
  `user` (an 8-hex-character hash of the caller's sign-in address, stable
  across restarts — never the address), `duration_ms`, `outcome` (`ok`,
  `tool_error` for a tool that reported its own failure, or `error` for a
  transport/handler failure).

Privacy rule, enforced everywhere in `pidge-mcp`: no e-mail address, subject
line, message body, recipient list, or token value is ever logged. A user is
always the 8-character hash above; a download link is always
`/dl/<redacted>`.

Query with KQL, either in the Azure portal (the container app's **Logs**
blade, or **Monitor > Logs** against the `log-pidge` workspace) or
`az monitor log-analytics query --workspace <workspace-customer-id> --analytics-query "<query>"`.
The body is JSON text in `Log_s`, so parse it first with `parse_json`:

p95 latency per route, last hour:

```kql
ContainerAppConsoleLogs_CL
| where ContainerAppName_s == "ca-pidge-mcp" and TimeGenerated > ago(1h)
| extend line = parse_json(Log_s)
| where line.message == "http_request"
| summarize p95_ms = percentile(todouble(line.latency_ms), 95) by route = tostring(line.route)
| order by p95_ms desc
```

Tool calls by outcome, last 24 hours:

```kql
ContainerAppConsoleLogs_CL
| where ContainerAppName_s == "ca-pidge-mcp" and TimeGenerated > ago(24h)
| extend line = parse_json(Log_s)
| where line.message == "tool_call"
| summarize calls = count() by tool = tostring(line.tool), outcome = tostring(line.outcome)
| order by calls desc
```

5xx responses in the last hour:

```kql
ContainerAppConsoleLogs_CL
| where ContainerAppName_s == "ca-pidge-mcp" and TimeGenerated > ago(1h)
| extend line = parse_json(Log_s)
| where line.message == "http_request" and toint(line.status) >= 500
| project TimeGenerated, route = tostring(line.route), status = toint(line.status), latency_ms = toint(line.latency_ms)
| order by TimeGenerated desc
```

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

### Revoke a user's sessions

Call `accounts_update { sign_out_everywhere: true }` from the harness to
sign one user out everywhere, without touching the vault or any other
user. It bumps that user's `token_generation` counter in their user
record; any other settings in the same call (default sender, trust list,
disconnect) are applied and saved first. Every access token and refresh
token already issued to that user carries the old generation and is
rejected from that point on — the bearer check on `/mcp` and the OAuth
refresh grant both compare the token's generation against the stored one
and fail with `invalid_token` / `invalid_grant` on a mismatch, regardless
of expiry. Outstanding `mail_attachment` download links carry the
generation too, and `/dl` refuses one minted before the sign-out with the
same 404 as an expired link. Every client of that user, including the one
that made the call, must sign in again.

The generation lookup is checked in-memory first. A cached value is
trusted for 5 minutes after the store last confirmed it; after that, or on
a miss, the secret store is read again. So a generation changed outside
this process (an edit in Key Vault, or another replica if the app were
ever scaled out, which `main.bicep` rules out with `maxReplicas: 1`) takes
effect within 5 minutes. A sign-out through `accounts_update` takes effect
at once. It's a failing store read (a transient Key Vault error, say) that
the server can't tell apart from an actual sign-out, and an expired cached
value is not used as a fallback. Each of the three places that check it
fails closed, but not identically:

- **The bearer check on `/mcp`** (`oauth/bearer.rs`) returns 401
  `invalid_token` either way — on a real generation mismatch or on a store
  read failure. A client just sees an expired-looking token and
  re-authenticates.
- **A refresh grant with a stale generation** (`oauth/mod.rs`) returns 400
  `invalid_grant`, `"session was signed out"` — an unambiguous "sign in
  again", since the store read succeeded and confirmed the mismatch.
- **A store-read failure during either grant type** (`authorization_code`
  or `refresh_token`, `oauth/mod.rs`) returns 503
  `temporarily_unavailable` before the mismatch can even be checked — a
  signal to retry shortly, not to re-authenticate.

### Key Vault purge protection

`enablePurgeProtection: true` in `main.bicep` turns on Key Vault purge
protection, and it is irreversible for the life of the vault — there is no
parameter or `az` command that turns it back off. With it on, a
soft-deleted secret or a soft-deleted vault cannot be purged during the
30-day retention window; only Azure can remove it once that window elapses.
Practically: deleting the `pidge` resource group does not immediately
destroy the vault or the refresh tokens and signing key inside it — it
soft-deletes the vault, which then sits unrecoverable-but-not-yet-gone for
30 days before Azure purges it on its own.

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
