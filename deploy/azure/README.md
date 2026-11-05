# pidge-mcp on Azure

Remote MCP server for pidge: the user's Microsoft 365, Outlook.com and Gmail
mail and calendars, merged, for any MCP-capable harness. Since 2.0 the server
is a thin HTTP frontend over `pidge-engine`, the same engine the CLI and the
local stdio server (`pidge mcp serve`, see [`docs/mcp.md`](../../docs/mcp.md))
run: it signs people in, checks which mailboxes are theirs, and hands each
signed-in user an engine of their own. Original design:
`docs/superpowers/specs/2026-09-22-remote-mcp-full-feature-design.md` (the
tool set it describes was replaced in 2.0; the OAuth, ownership and secret
handling still hold).

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

`main.bicep` creates a Key Vault (tokens, the signing key, users' AI keys),
a storage account with a private `prefs` container (one JSON blob of
preferences per user, written only by the app's managed identity, which gets
Storage Blob Data Contributor on it), the container registry, the Log
Analytics workspace and the Container App. Other inputs the script reads:

| Input | Default | Meaning |
|---|---|---|
| `PIDGE_MCP_MIN_REPLICAS` | `1` | Bicep `minReplicas`. `1` keeps a replica warm: no cold start on the first call of a conversation, and each user's in-memory engine and on-disk index (handles, plans, undo, held sends) survive between conversations. `0` scales to zero when idle and costs less. `maxReplicas` is always 1. |
| `PIDGE_GOOGLE_CLIENT_ID`, `PIDGE_GOOGLE_CLIENT_SECRET` | empty | Bicep `googleClientId` and `googleClientSecret` (secure): a Google "Web application" OAuth client for Gmail mailboxes, with the authorized redirect URI `<public url>/callback/google`. Empty: Gmail cannot be connected. The secret is stored as a Container App secret. |

Unlike the custom domain below, these are not read back from the live app:
every run sets them, so a run without the Google variables removes Gmail
support, and one without `PIDGE_MCP_MIN_REPLICAS` goes back to 1.

### Gmail

Create the Google client as described in [`docs/gmail.md`](../../docs/gmail.md)
(type **Web application**, redirect URI `https://<public url>/callback/google`;
after a [cutover](#cutover), register the custom domain's
`/callback/google` too), then deploy with both variables set:

```bash
PIDGE_GOOGLE_CLIENT_ID=<id>.apps.googleusercontent.com \
PIDGE_GOOGLE_CLIENT_SECRET=<secret> \
deploy/azure/deploy.sh --skip-entra
```

Users still sign in to pidge itself with Microsoft; a Gmail mailbox is added
afterwards with `accounts action=connect provider=google`, which returns a
`/connect` link that goes to Google. The callback requires the cookie set by
that link's Continue step, like every other flow, and a connect for a named
address refuses a Google account with another address.

### Custom domain

Set `PIDGE_MCP_CUSTOM_DOMAIN=pidge.mklab.se` to bind a custom domain to the
Container App. Its CNAME and `asuid.<subdomain>` TXT record must already
exist at the DNS provider before running the script, since the certificate
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
  URL, and the custom domain is added to `PIDGE_MCP_ALT_HOSTS`: the server
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
**before** setting `PIDGE_MCP_CUTOVER=1`, otherwise sign-ins against the
new issuer have nowhere valid to redirect to. Phase 4 never runs that
registration automatically when a custom domain is in play; it only prints
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

Releasing and deploying are one workflow with one trigger. `.github/workflows/release.yml`
runs when the release skill pushes a `v*` tag: its `CI` job (format, clippy,
tests) gates everything else, then the CLI binaries, SBOMs, GitHub Release,
Homebrew formula, crates.io crates and the GHCR image are published and the
`Deploy MCP` job deploys `pidge-mcp` from that same tagged commit. The
deploy doesn't build the image a second time: it waits for the GHCR image
(`ghcr.io/mklab-se/pidge-mcp:v1.2.3`) and copies it into the Azure registry
with `az acr import` (`IMPORT_IMAGE` in `deploy.sh`), which takes seconds
instead of a full ACR build. A push to
`main` runs the `CI` workflow but deploys nothing: the tag is the deploy
intent for both the CLI and the server. The image is tagged with the
release tag (`v1.2.3`).

The workflow can also be run manually from the Actions tab
(`workflow_dispatch`), which runs CI and the deploy from the chosen ref and
nothing else: a redeploy that doesn't need a new version, e.g. after
rotating `PIDGE_MCP_ALLOWED_EMAILS` (see below); the image is then tagged
with the commit SHA. Deploys are serialized: a `deploy-mcp` concurrency
group with `cancel-in-progress: false` means a second trigger queues behind
a deploy already in progress rather than racing or cancelling it. The
deploy job is skipped in forks (`github.repository` check): a fork has no
Azure identity to federate with. The publishing jobs and the deploy are
independent after the CI gate, so a failed crates.io or Homebrew step never
holds back a deploy; a re-pushed tag skips crate versions that are already
published.

The deploy is the one job that can reach the server's secrets, so it is
gated three ways. The `production` environment (which the Azure federated
credential trusts) accepts deployments only from `main` and `v*` tags. It has
no required reviewer while the owner is the only one who can push: the
admin-only `v*` tag is the deploy decision (add a reviewer once others
contribute). A tag ruleset lets only repository admins create, move or
delete `v*` tags. And the deploy identity's RBAC Administrator grant is
conditioned to assign only AcrPull, Key Vault Secrets Officer and Storage
Blob Data Contributor, and only to the server's own identity. `setup-github-oidc.sh` sets all three. The
`crates-io` environment accepts only `v*` tags. Every third-party action is
pinned to a commit SHA (Dependabot proposes updates), and the workflow's
`GITHUB_TOKEN` is read-only except in the GitHub Release job.

The deploy job runs in the repository's `production` GitHub environment,
which fixes its OIDC subject to `repo:mklab-se/pidge:environment:production`
regardless of the tag or branch that started the run; that is the subject
the deploy identity's federated credential trusts. No Azure credential is
stored in GitHub.

**One-time setup.** `deploy/azure/setup-github-oidc.sh`, run once by hand
from a workstation with `az login` and `gh auth login` (and
`PIDGE_MCP_ALLOWED_EMAILS` set), creates the deploy identity
`id-pidge-deploy`, the `production` GitHub environment, the federated
credential for that environment's subject, the two conditioned role
assignments, and the repository variables and secret the deploy job reads.
It is idempotent, so re-run it after changing any of those. It runs
`deploy/azure/deploy.sh --skip-entra --skip-certificate`, then smoke-tests
the result: `/healthz` must return `ok`, the issuer in
`/.well-known/oauth-authorization-server` must equal the deployed URL, the
`resource` in `/.well-known/oauth-protected-resource` must equal
`<url>/mcp`, and an unauthenticated `POST /mcp` must return 401, and each check
retries a few times to ride out a cold start. `--skip-entra` always skips
Phase 4 (Entra callback registration) entirely: the deploy identity has no
Microsoft Graph directory-read rights, which even reading the app's
existing redirect URIs requires, so CI must not touch that step at all, and
the callback URI is registered once, by hand, during the initial deploy.
`--skip-certificate` asserts the custom domain (if any) already has a
`SniEnabled` certificate binding instead of trying to provision one from CI;
that flow polls for up to 15 minutes and is meant to be run interactively by
a human once, per [Custom domain](#custom-domain). With a live
`SniEnabled` binding the script reuses its certificate id, and with no
custom domain bound or requested the flag does nothing. Since CI leaves
`PIDGE_MCP_CUSTOM_DOMAIN` and `PIDGE_MCP_CUTOVER` unset (unless you create
repository variables for them), every CI deploy keeps the live domain,
certificate and cutover state as they are.

The deploy job also passes `vars.PIDGE_MCP_MIN_REPLICAS`,
`vars.PIDGE_GOOGLE_CLIENT_ID` and `secrets.PIDGE_GOOGLE_CLIENT_SECRET` to the
script. `setup-github-oidc.sh` does not create these; set them by hand
(`gh variable set` / `gh secret set`) if you want something other than one
warm replica, or Gmail. Since the script does not carry them over from the
live app (see [Deploy](#deploy)), a deployment that should support Gmail
needs them in the repository, or the next CI deploy removes it.

**Upgrading from 1.x.** 2.0 adds a third role the template assigns (Storage
Blob Data Contributor on the preferences storage account). Re-run
`setup-github-oidc.sh` once before the first 2.0 CI deploy: it notices the
changed condition and replaces the RBAC Administrator assignment. Without
that, ARM refuses the new role assignment. A deploy run by hand with your
own `az login` (as owner) is not affected.

## Logs

`PIDGE_MCP_LOG_FORMAT` (`json` or `text`, default `json`; the deploy script
always sets `json`) picks the tracing subscriber's output format. Every log
line goes to stdout, which the Container Apps environment collects into the
`log-pidge` Log Analytics workspace as `ContainerAppConsoleLogs_CL`
(`Log_s` holds the raw line, `ContainerAppName_s` is `ca-pidge-mcp`).

A JSON line is a flat object: `timestamp`, `level`, `target`, `message`
(the event name), plus that event's own fields at the top level; there is
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
  across restarts, never the address), `duration_ms`, `outcome` (`ok`, or
  `tool_error` for a call that returned an error to the harness, including
  a caller whose account could not be loaded).

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
mailboxes are added from inside the harness with `accounts action=connect`
(`provider=google` for Gmail).

## Tools

The tools, their descriptions and input schemas, and the server instructions
are not defined in this crate: they come from `pidge_engine::registry`
(`crates/pidge-engine/src/ops/*.rs`) and `crates/pidge-engine/src/playbooks.rs`
through `crates/pidge-engine/src/mcp.rs`, exactly as `pidge mcp serve`
exposes them. No tool takes a user id: every call acts as the signed-in user.
Reads merge all of the user's connected mailboxes unless `account` names one
of them. Ids are short handles: `m12` (message), `e3` (event), `d4` (draft),
`p5` (plan), `s1` (triage session).

| Tool | What it does |
|---|---|
| `mail_brief` | "Any new e-mail?" / "anything important?" (`importance=high`): an inbox map (totals, unread, new per category, invites, flagged), then the new mail that matters since the user last looked, with a reason each, and counts for the bulk. Moves "last seen" unless `mark_seen=false`. |
| `mail_find` | Mail across all mailboxes, newest first, by `text`, `from`, `to_me`, `folder`, `since`/`before`, `unread`, `flagged`, `has_attachments`, `category`, `kind` (receipt, newsletter, promotion, notification, social, invite, personal) and `importance`; `count_only` for counts. Reaches past the indexed window through the provider's search when needed. |
| `mail_read` | One message (sender, recipients, date, attachments, and for invitations the event id), or its conversation with `thread=true`. `mark_read` also marks it read. |
| `mail_triage` | Goes through the inbox with the user: sweeps of bulk mail (one yes clears a group), batches of up to five routine messages, then what matters one by one with a suggested action. `scope` (`unread`, `all`, `new`), `importance`, `category`, `since`; then `decision=` yes, archive, delete, read, keep, flag, file, unsubscribe, done, skip, one_by_one, undo or stop. |
| `mail_act` | archive, delete, read, unread, flag (with `due`), unflag, move (`to`), categorize, unsubscribe, classify (`set_kind`, `set_category`: no mailbox change) or undo, on `ids` or on selector fields. `plan=p5` applies a plan, with `include`/`exclude`. |
| `mail_draft` | Write or revise (`draft=d4`) a draft: `kind` new, reply, reply_all or forward (`in_reply_to=m12`). Recipients may be names of people the user mails with. Returns the draft id and a preview; `send=true` also sends it through the outbox. |
| `mail_send` | Puts a draft in the outbox (see behaviour notes). `action=cancel`, `now` (skip the hold), `approve` (send one held for approval), `status` (list the outbox). |
| `mail_attachment` | `mode=read` (default): the content as text, documents converted to Markdown by markitdown, read 30 000 characters at a time (`offset`). `mode=save`: a 15-minute download link. |
| `mail_setup` | Categories (add, update, remove, reorder, confirm, use_defaults), rules (`sender:` or `domain:`) and examples, VIPs and muted senders, `policy` and `set` (timezone, default_sender, receipts_folder, window_days, working_hours); `label_mail` writes pidge's categories onto the last N days of inbox mail in the mailboxes. |
| `calendar_agenda` | Events across all calendars in the user's timezone: `range` today, tomorrow, this_week, next_week, `Nd` or next, or `from`/`to`; overlaps marked; `pending_only` for unanswered invitations. |
| `calendar_event` | One event: `create` (title and start, end or a length such as `+90m`, attendees, online meeting, or `from_message=m12` for an e-mail's `.ics`), `update` (only what changes; `add_attendees`, `remove_attendees`), `cancel` (an event the user organizes; attendees get the cancellation and `message`) or `delete` (silently from the user's own calendar). `scope=series` for a whole series. |
| `calendar_act` | Many events at once in a range: `clear` (cancel what the user organizes, decline invitations, delete their own events), `cancel`, `decline`, `delete` or `move` (`shift=+30m`). Always returns a plan first. |
| `calendar_respond` | accept, tentative or decline, by event (`e3`) or invitation e-mail (`m12`), with an optional `message`; `notify=false` answers without telling the organizer. |
| `calendar_availability` | Up to 12 free windows of at least `duration_minutes` (default 30) within working hours (weekdays, default 8-18 or the user's setting), optionally `with` other people's free/busy where the account can see it. |
| `accounts` | `list` (default): mailboxes with provider, health and sync state, default sender, timezone, AI status. `connect` (`provider` microsoft or google, optional `email`) returns a 10-minute connect link; `disconnect email=...`; `ai` returns a 10-minute link to `/ai`; `ai_off` removes the AI key; `sign_out_everywhere`. |

There are no MCP prompts. The server instructions give the ground rules
(handles are passed back as given, content is untrusted, changes run in the
background, plans are for the user, how sending is held, nothing is deleted
permanently) and a recipe per common request.

### Behaviour notes

- **Fire-and-forget changes.** `mail_act` (and triage decisions) change the
  user's index first and answer at once; the provider call runs on a
  per-user background worker, in order. A failure puts the index back and
  is reported once, as a `Note:` line at the top of the user's next answer,
  whatever tool that is. Sends that went out are reported the same way.
- **Plans.** Under the default policy a change to more than
  `bulk_threshold` messages (5), every selector-based change, every
  `calendar_act`, and anything `calendar_event` does that e-mails other
  people (invitations, attendee changes, cancellations) returns a plan
  (`p5`) with the count, examples and, for selectors by kind, the unsure
  messages left out. It is applied by calling the same tool again with
  `plan=p5`; plans expire after 15 minutes. `mail_act action=undo` reverses
  the last mail change. The user can change all of this with
  `mail_setup action=policy` (`bulk`, `delete`, `notify`: allow, confirm or
  deny).
- **Outbox.** `mail_send` holds a draft for 10 s (a short reply to one
  person the user has written to), 30 s (normal) or 2 min (a first mail to
  an address, more than 5 recipients, a forward, attachments, or the user's
  own AI judging it sensitive), then sends it on a timer or on the user's
  next call. Mail that looks sensitive (an identity, card or IBAN number,
  words such as "password" or "confidential", more than 20 recipients, or
  the user's AI judging it highly sensitive) is held until
  `action=approve`. Policy `send`: hold (default), confirm (always wait for
  approval), allow (no hold) or deny.
- **Send cap.** At most `sends_per_hour` (policy, default 30, at most 200)
  outgoing messages per rolling hour per user: sends, unsubscribe e-mails,
  RSVP notes, and calendar changes that e-mail other people (one per
  event).
- **Triage.** A session (`s1`) lives 60 minutes idle. Acting on the current
  message with `mail_act` (for example archiving it after a reply) also
  advances the session and returns the next step.
- **Sorting.** Each message gets a kind, category and importance from the
  user's rules first, then mail signals (list headers, automated senders,
  VIPs, threads the user wrote in), then the user's own AI key if they set
  one (inbox mail from the last 14 days only), and finally verdicts the
  harness records with `mail_act action=classify`.
- **Thread mode.** `mail_read thread=true` shows the conversation newest
  first, each message trimmed to its own contribution and capped.
- **Reply recipients.** Recipients given to a reply are added to the ones
  the reply already goes to. For `kind=new` they are the full list. Replies
  go out from the mailbox that received the original.
- **Delete** moves to Deleted Items (Gmail: Trash) only. Nothing is deleted
  permanently, and `calendar_event action=cancel` uses the calendar's own
  cancel.
- **Download cap.** 60 downloads per hour per user.
- **Attachment limits.** Documents over 25 MB are not converted. Images are
  not returned inline; `mode=save` gets them.
- **Download links** are valid 15 minutes. They name the user and mailbox only
  by hash, and every refusal is a plain 404. The download goes through the
  user's engine, so it works for Microsoft and Gmail mailboxes alike, and
  only for a mailbox the user still owns.
- **Connect links** show a consent page naming the pidge account the mailbox
  will be attached to before sending the user to Microsoft or Google.
- **Sign-in consent.** A client's `/authorize` shows a page naming the client
  and the host the sign-in goes back to. Only its Continue, from the same
  browser, goes to Microsoft.
- **One browser per flow.** The Continue step of every flow sets a cookie
  that `/callback` and `/callback/google` require, so a sign-in URL lifted
  from one browser and opened in another (say, sent to someone as a "sign
  in here" link) completes nothing: no code is issued and no mailbox is
  bound.
- **Account identity.** A Microsoft account is its principal name, never the
  editable `mail` attribute. The tenant and object id from the ID token are
  pinned on first sign-in, and a later sign-in or connect under the same name
  with another account is refused. A Gmail mailbox is pinned to the address
  Google verifies.
- **AI keys.** Entered only on the `/ai` page (each form bound to its browser
  by a cookie plus form token; opening the page claims nothing, since chat
  apps prefetch links; the first successful save uses the link up), tested
  with one tiny request before saving, stored in a per-user secret
  `ai-<hash>`, never shown or logged. Only TypeSafe, OpenAI and Anthropic,
  at their own endpoints: no URL comes from input. The key sharpens sorting
  and the outbox's sensitivity check; while its status is `no_credits`,
  `key_rejected` or `failing`, pidge sorts without it. `accounts` shows the
  status.
- **Unsubscribe.** One-click POSTs go over https to public hosts only, and
  redirects are not followed. Unsubscribe e-mails use the list's own
  subject (at most 100 characters, "unsubscribe" when it has none). A
  web-only unsubscribe link is handed to the user instead.

## Configuration

`Config::from_env` (parsing in `Config::from_map`) in
`crates/pidge-mcp/src/config.rs` is the source of truth.

| Variable | Required | Meaning |
|---|---|---|
| `PIDGE_MCP_PUBLIC_URL` | yes | External origin: OAuth issuer, resource id, callback base. https except for localhost. |
| `PIDGE_MCP_ALLOWED_EMAILS` | yes | Comma-separated sign-in allowlist. |
| `PIDGE_MCP_KEYVAULT_URL` | one of | Key Vault for secrets (Azure). |
| `PIDGE_MCP_SECRETS_DIR` | one of | Plaintext file secrets (development only). |
| `PIDGE_MCP_PREFS_URL` | no | Azure Blob container for preferences (one `<hash>.json` per user), https, written with the managed identity. Set by `main.bicep`. |
| `PIDGE_MCP_PREFS_DIR` | no | Preferences as files in a directory instead (development). Only one of the two; with neither, `<secrets dir>/prefs` when `PIDGE_MCP_SECRETS_DIR` is used, else a temp directory. |
| `PIDGE_MCP_INDEX_DIR` | no | Directory for each user's SQLite index (`<hash>.db`), default `<temp>/pidge-index`. `main.bicep` sets `/tmp/pidge-index`. |
| `PIDGE_GOOGLE_CLIENT_ID`, `PIDGE_GOOGLE_CLIENT_SECRET` | no | Google "Web application" OAuth client (redirect URI `<public url>/callback/google`) for Gmail mailboxes. Both or neither. |
| `PORT` | no | Listen port, default 8080. |
| `PIDGE_MCP_MARKITDOWN` | no | markitdown executable, default `markitdown` on `PATH`. |
| `AZURE_CLIENT_ID` | Azure | Client id of the user-assigned managed identity used for Key Vault and Blob storage. |
| `PIDGE_CLIENT_ID` | no | Overrides the pidge Entra app id compiled into `pidge-client`. |
| `PIDGE_MCP_ALT_HOSTS` | no | Comma-separated extra hostnames the server accepts requests for, besides the public URL host. Set by the deploy script during a custom-domain cutover. |
| `PIDGE_MCP_LEGACY_ISSUERS` | no | Comma-separated issuer URLs whose previously issued tokens still validate. Set by the deploy script when cutting over to a new public URL. |
| `PIDGE_MCP_LOG_FORMAT` | no | `json` or `text`, default `json`. The deploy script always sets `json`. |
| `PIDGE_MCP_REDIRECT_HOSTS` | no | Comma-separated extra hosts an OAuth client may use as its https redirect target (exact host match, no scheme, port or wildcard). Always allowed: `claude.ai`, `claude.com`, `chatgpt.com`, and loopback (`localhost`, `127.0.0.1`, `[::1]`, any port, http too). Checked at `/register` and `/authorize`. |
| `RUST_LOG` | no | Log filter, default `info`. |

### Where state lives

| What | Where | Lost on restart? |
|---|---|---|
| Signing key, user records (mailbox list, token generation, refresh chains), mailbox records (`owner`, `provider`, tokens), AI keys | Key Vault (`jwt-signing-key`, `user-*`, `mailbox-*`, `ai-*`) | No |
| Preferences: categories, rules, examples, VIPs, muted senders, policy, timezone, default sender, last seen, AI status | One blob per user in the `prefs` container | No |
| Index: messages and classifications, handles, plans, undo log, triage sessions, outbox, send count, pending notes | One SQLite file per user under `PIDGE_MCP_INDEX_DIR` | Yes, on Azure (`/tmp`): it is rebuilt from the mailboxes, but held sends, plans, handles and the hourly send count start over |
| Each user's engine (worker queue, outbox timer) | Process memory, refreshed from the user record every 2 minutes and after connect, disconnect or an AI key change | Yes |

On a user's first call after 2.0, if they have no preferences yet, their
1.x review categories (with rules and examples), timezone, default sender
and trusted senders (as VIPs) are carried over from the user record.

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
az keyvault secret list --vault-name <kv-name> -o table      # jwt-signing-key, user-*, mailbox-*, ai-*
az storage blob list --auth-mode login --account-name <storage-name> -c prefs -o table
```

Disconnect a mailbox with `accounts action=disconnect email=...` from the
harness rather than by deleting its secret, so the user record stays
consistent (the sign-in mailbox itself cannot be disconnected). A user's
preferences blob can be deleted to reset them to the defaults; their index
is only a cache, and the next call rebuilds it.

Rotating `jwt-signing-key` invalidates every client registration, token and
outstanding download link; clients simply re-register and sign in again.

### Revoke a user's sessions

Call `accounts { action: "sign_out_everywhere" }` from the harness to
sign one user out everywhere, without touching the vault or any other
user. It bumps that user's `token_generation` counter in their user
record. Every access token and refresh
token already issued to that user carries the old generation and is
rejected from that point on: the bearer check on `/mcp` and the OAuth
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
effect within 5 minutes. A sign-out through the `accounts` tool takes effect
at once. It's a failing store read (a transient Key Vault error, say) that
the server can't tell apart from an actual sign-out, and an expired cached
value is not used as a fallback. Each of the three places that check it
fails closed, but not identically:

- **The bearer check on `/mcp`** (`oauth/bearer.rs`) returns 401
  `invalid_token` either way, on a real generation mismatch or on a store
  read failure. A client just sees an expired-looking token and
  re-authenticates.
- **A refresh grant with a stale generation** (`oauth/mod.rs`) returns 400
  `invalid_grant`, `"session was signed out"`, an unambiguous "sign in
  again", since the store read succeeded and confirmed the mismatch.
- **A store-read failure during either grant type** (`authorization_code`
  or `refresh_token`, `oauth/mod.rs`) returns 503
  `temporarily_unavailable` before the mismatch can even be checked, a
  signal to retry shortly, not to re-authenticate.

Refresh tokens rotate: each refresh grant returns a new refresh token and
retires the one presented. The user record keeps the current refresh
token id per client (at most 20 clients). Presenting a retired refresh
token is treated as theft of that client's chain: every refresh token of
that client is refused from then on (its access tokens run out within the
hour), and it must sign in again, which starts a new chain. The user's
other clients are not affected. One exception covers a client that sends the same refresh twice at
once: the token retired in the last 30 seconds is still answered, with a
refresh token that continues the current chain. A refresh token from
before rotation existed is accepted once and starts the chain.

### Key Vault purge protection

`enablePurgeProtection: true` in `main.bicep` turns on Key Vault purge
protection, and it is irreversible for the life of the vault: there is no
parameter or `az` command that turns it back off. With it on, a
soft-deleted secret or a soft-deleted vault cannot be purged during the
30-day retention window; only Azure can remove it once that window elapses.
Practically: deleting the `pidge` resource group does not immediately
destroy the vault or the refresh tokens and signing key inside it; it
soft-deletes the vault, which then sits unrecoverable-but-not-yet-gone for
30 days before Azure purges it on its own.

## Published image

Every `v*` release tag also publishes the server image to GitHub Container
Registry as `ghcr.io/mklab-se/pidge-mcp:<version>` and `:latest` (linux/amd64,
built from `deploy/azure/Dockerfile` by the `image` job in
`.github/workflows/release.yml`; `latest` never moves to a prerelease). The
Azure deployment does not use it: `deploy.sh` builds into the private ACR
from the commit being deployed. The GHCR package is private and stays so;
anyone else who wants to run the server builds the image themselves from
`deploy/azure/Dockerfile` (see `docs/mcp.md`, "Run the container anywhere").

## Run locally

```bash
PIDGE_MCP_PUBLIC_URL=http://localhost:8080 \
PIDGE_MCP_ALLOWED_EMAILS=you@example.com \
PIDGE_MCP_SECRETS_DIR=/tmp/pidge-mcp-secrets \
PIDGE_MCP_MARKITDOWN=/path/to/markitdown-wrapper \
cargo run -p pidge-mcp
```

Preferences then go to `/tmp/pidge-mcp-secrets/prefs` and the index to
`<temp>/pidge-index`; set `PIDGE_MCP_PREFS_DIR` and `PIDGE_MCP_INDEX_DIR` to
move them. Add `PIDGE_GOOGLE_CLIENT_ID` and `PIDGE_GOOGLE_CLIENT_SECRET` (a Web
application client with `http://localhost:8080/callback/google` as a
redirect URI) to try Gmail. `PIDGE_MCP_KEYVAULT_URL=https://<kv>.vault.azure.net/`
instead of the secrets dir uses Key Vault through your `az login`. A secret
store from the spike (bare token sets per mailbox) is adopted on first use:
the user record is created on the first tool call. To try the tools without
any of this, `pidge mcp serve` runs the same engine over stdio against your
local accounts.
