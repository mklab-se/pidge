# M365 authentication + `pidge inbox list` — design

**Status:** Approved 2026-05-13
**Goal:** Land the first real pidge feature — OAuth-based sign-in to one or more Microsoft 365 / personal Microsoft accounts, with `pidge inbox list` as the validation command that lists messages from every signed-in account merged.
**Scope:** Auth surface (`pidge auth login|list|status|logout|default`), Microsoft Graph client for mail, one read command (`pidge inbox list`). Multi-account from day one. Three-crate workspace split.

## Non-goals

- No mail send/draft/delete/move/reply/search commands. Each is a future feature with its own spec+plan.
- No calendar commands. Same reason. Scopes for calendar **are** requested at sign-in so no re-consent later.
- No folder navigation. Only the Inbox folder is read in this feature.
- No local cache or offline mode.
- No per-account scope override — every account consents to the same scope set.
- No service-principal / app-only / non-delegated auth. Pidge always acts as the signed-in user on their own mailbox/calendar.

## Architecture

### Workspace split (executes the foundation spec's deferred decomposition)

```
crates/
├── pidge/          # CLI binary; clap, banner, output formatting, update checker
├── pidge-core/     # Shared types + I/O: Account, Config, Message. No HTTP, no auth, no clap.
└── pidge-client/   # HTTP layer: Graph client, OAuth device code flow, token store
```

Dependency direction: `pidge` → `pidge-client` → `pidge-core`. The `pidge-core` crate has no networking and no clap; `pidge-client` has no terminal/output code.

### Module layout

```
crates/pidge-core/src/
├── lib.rs               # Re-exports
├── account.rs           # Account struct, AccountSet helpers
├── config.rs            # Config struct, load/save to ~/.config/pidge/config.yaml
├── message.rs           # Normalized Message struct (provider-agnostic)
└── error.rs             # CoreError (config I/O, parsing)

crates/pidge-client/src/
├── lib.rs               # Re-exports + GraphClient as main entry point
├── auth/
│   ├── mod.rs           # AuthClient: device_code_login, refresh, get_valid_token
│   ├── device_code.rs   # Hand-rolled RFC 8628 device authorization grant flow
│   ├── refresh.rs       # Hand-rolled refresh_token grant
│   ├── jwt.rs           # Minimal JWT decoder (base64-decode middle segment, no signature verify)
│   ├── tokens.rs        # TokenSet { access, refresh, expires_at }
│   ├── store.rs         # KeychainStore via the `keyring` crate
│   └── config.rs        # APP_CLIENT_ID constant + scope strings + Microsoft endpoints
├── graph/
│   ├── mod.rs           # GraphClient wrapping reqwest + AuthClient
│   ├── me.rs            # GET /me (used post-login to learn user's email/tenant_id)
│   └── mail.rs          # list_inbox(account, limit, unread_only) -> Vec<Message>
└── error.rs             # ClientError (auth, network, graph)

crates/pidge/src/commands/
├── mod.rs               # (existing — extended)
├── auth.rs              # Top-level dispatch
├── auth_login.rs        # `pidge auth login`
├── auth_list.rs         # `pidge auth list`
├── auth_status.rs       # `pidge auth status`
├── auth_logout.rs       # `pidge auth logout`
├── auth_default.rs      # `pidge auth default [--send <email> | --calendar <email>]`
└── inbox.rs             # `pidge inbox list`
```

### Why three crates now

The foundation spec explicitly said decomposition would happen "when HTTP/provider code lands." That moment is now. Splitting now means:

1. `pidge-core::message::Message` becomes the contract every future provider (Gmail, IMAP, etc.) implements against. Commands depend on `Message`, not on Microsoft Graph JSON.
2. `pidge-client` is testable in isolation with `wiremock` — no clap or output formatting in scope.
3. The release pipeline's `crates-io` job needs to publish the three crates in dependency order (`pidge-core` → `pidge-client` → `pidge`) with the usual 30s sleeps. The release workflow gains this — matches cosq's pattern exactly.

## Auth flow — OAuth 2.0 device authorization grant

### One-time developer setup (you, Kristofer)

Ship `scripts/register-pidge-app.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

if ! command -v az >/dev/null 2>&1; then
  cat <<'EOF' >&2
Azure CLI not found. Install it from https://aka.ms/install-azure-cli, then re-run.
Or follow the manual portal walkthrough in docs/superpowers/specs/2026-05-13-m365-auth-and-inbox-list-design.md § "Manual portal fallback".
EOF
  exit 1
fi

# Confirm logged in
if ! az account show >/dev/null 2>&1; then
  echo "Run 'az login' first." >&2
  exit 1
fi

CLIENT_ID=$(az ad app create \
  --display-name "pidge" \
  --sign-in-audience AzureADandPersonalMicrosoftAccount \
  --is-fallback-public-client true \
  --required-resource-accesses @scripts/pidge-app-permissions.json \
  --query appId -o tsv)

cat <<EOF
✔ pidge app registered.
  client_id: ${CLIENT_ID}

Paste this into crates/pidge-client/src/auth/config.rs:

    pub const APP_CLIENT_ID: &str = "${CLIENT_ID}";

Then commit and continue.
EOF
```

`scripts/pidge-app-permissions.json` (delegated Microsoft Graph permissions):

```json
[
  {
    "resourceAppId": "00000003-0000-0000-c000-000000000000",
    "resourceAccess": [
      {"id": "e1fe6dd8-ba31-4d61-89e7-88639da4683d", "type": "Scope"},
      {"id": "7427e0e9-2fba-42fe-b0c0-848c9e6a8182", "type": "Scope"},
      {"id": "024d486e-b451-40bb-833d-3e66d98c5c73", "type": "Scope"},
      {"id": "e383f46e-2787-4529-855e-0e479a3ffac0", "type": "Scope"},
      {"id": "1ec239c2-d7c9-4623-a91a-a9775856bb36", "type": "Scope"}
    ]
  }
]
```

Permission GUIDs in order: `offline_access`, `User.Read`, `Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite`. Microsoft Graph's resource app ID `00000003-0000-0000-c000-000000000000` is a well-known constant.

**Manual portal fallback** (if `az` is unavailable): documented in `DEVELOPMENT.md`. Steps: portal.azure.com → Entra ID → App registrations → New registration → name `pidge`, account types `Multi-tenant + personal MS accounts` → Authentication → "Allow public client flows" Yes → API permissions → Add Microsoft Graph delegated, the five scopes above → done.

### What gets baked in

```rust
// crates/pidge-client/src/auth/config.rs
pub const APP_CLIENT_ID: &str = ""; // empty until register-pidge-app.sh runs

pub const SCOPES: &[&str] = &[
    "offline_access",
    "User.Read",
    "Mail.ReadWrite",
    "Mail.Send",
    "Calendars.ReadWrite",
];

pub const AUTHORITY: &str = "https://login.microsoftonline.com/common";
pub const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/devicecode";
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

pub fn client_id() -> Option<String> {
    std::env::var("PIDGE_CLIENT_ID")
        .ok()
        .or_else(|| (!APP_CLIENT_ID.is_empty()).then(|| APP_CLIENT_ID.to_string()))
}
```

If `client_id()` returns `None`, every auth command surfaces a single user-facing error: *"pidge has not been provisioned yet. The maintainer needs to run `scripts/register-pidge-app.sh` and update `APP_CLIENT_ID`."*

`PIDGE_CLIENT_ID` env var lets development against a personal test app proceed before the public app is registered.

### Per-user sign-in (every account, every install)

Hand-rolled implementation of [RFC 8628](https://www.rfc-editor.org/rfc/rfc8628) (OAuth 2.0 Device Authorization Grant) using `reqwest` directly. ~125 LOC total across `device_code.rs` + `refresh.rs` + `jwt.rs`. No OAuth library dependency.

```
1. POST to https://login.microsoftonline.com/common/oauth2/v2.0/devicecode
     Content-Type: application/x-www-form-urlencoded
     client_id=<APP_CLIENT_ID>
     scope=offline_access User.Read Mail.ReadWrite Mail.Send Calendars.ReadWrite

2. Microsoft returns { device_code, user_code, verification_uri, interval, expires_in }.
   pidge displays:
     Go to:      https://microsoft.com/devicelogin
     Enter code: ABCD-1234
     (Waiting for sign-in… Ctrl-C to cancel)
   On platforms with a browser opener, pidge also runs `open` / `xdg-open` / `start`
   on the verification_uri (best-effort; failures are silent).

3. pidge enters a poll loop. Every `interval` seconds, POST to the token endpoint:
     POST https://login.microsoftonline.com/common/oauth2/v2.0/token
     grant_type=urn:ietf:params:oauth:grant-type:device_code
     client_id=<APP_CLIENT_ID>
     device_code=<from step 2>

   Response handling per RFC 8628 §3.5:
     - 200 + { access_token, refresh_token, expires_in, id_token } → SUCCESS
     - 400 + { error: "authorization_pending" } → sleep `interval`, retry
     - 400 + { error: "slow_down" } → `interval += 5`, sleep, retry
     - 400 + { error: "access_denied" } → user denied; abort
     - 400 + { error: "expired_token" } → device_code expired; abort
     - Stop unconditionally after `expires_in` seconds elapsed

4. On success: parse the id_token JWT to extract `tid` (tenant_id). The JWT decoder
   is a tiny helper — split on `.`, base64-decode the middle segment, JSON-parse,
   read `tid`. No signature verification: we trust the TLS path to
   login.microsoftonline.com.

5. Call Graph GET /me with the new access_token to learn the user's
   userPrincipalName (email) and id (Microsoft's object id).

6. Write the keychain entry: service="pidge", account="<email>",
   value=JSON({ access_token, refresh_token, expires_at = now + expires_in - 60s }).

7. Append the account to config.yaml. If this is the first account ever,
   set defaults.send = email and defaults.calendar = email.
```

### Token refresh

Every Graph call goes through `AuthClient::get_valid_token(email)`:

1. Load the keychain entry for the email.
2. If `expires_at - now > 60s`, return the access token as-is.
3. Otherwise, POST to the token endpoint:
   ```
   POST https://login.microsoftonline.com/common/oauth2/v2.0/token
   grant_type=refresh_token
   client_id=<APP_CLIENT_ID>
   refresh_token=<stored refresh_token>
   scope=offline_access User.Read Mail.ReadWrite Mail.Send Calendars.ReadWrite
   ```
   Response: `{ access_token, refresh_token, expires_in, ... }`. Microsoft may rotate the refresh token (personal MSA always rotates; Entra org usually doesn't); we persist whatever comes back.
4. Update the keychain entry with the new tokens and expiry.
5. Return the new access token.

If refresh returns `invalid_grant` (refresh token revoked or expired beyond the 90-day rolling window), the client returns `ClientError::SessionExpired { email }`. The CLI prints: *"Session expired for `<email>`. Run `pidge auth login` to re-add this account."* and exits 3. Other accounts are unaffected.

If a Graph API call returns 401 despite the local cache thinking the token is valid (clock skew, server-side revocation), the client forces a refresh once and retries the request.

## Where state lives

| Data | Location | Sensitive |
|---|---|---|
| `access_token`, `refresh_token`, `expires_at` per account | OS keychain via `keyring`: service `pidge`, account is the email, value is a JSON blob | Yes |
| Account metadata: email, tenant_id, home_account_id, added_at | `~/.config/pidge/config.yaml` under `accounts:` | No |
| Defaults: `default_send`, `default_calendar` | Same config file under `defaults:` | No |
| `APP_CLIENT_ID` | Compile-time constant in `pidge-client::auth::config`; runtime env override `PIDGE_CLIENT_ID` | No (public identifier, not a secret) |

### Config file shape

```yaml
accounts:
  - email: kristofer@mklab.se
    tenant_id: 11111111-2222-3333-4444-555555555555
    home_account_id: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
    added_at: "2026-05-13T22:00:00Z"
  - email: kristofer.liljeblad@live.com
    tenant_id: 9188040d-6c67-4c5b-b112-36a304b66dad
    home_account_id: ffffffff-...
    added_at: "2026-05-14T08:30:00Z"
defaults:
  send: kristofer@mklab.se
  calendar: kristofer@mklab.se
```

The well-known tenant ID `9188040d-6c67-4c5b-b112-36a304b66dad` is Microsoft's personal-accounts tenant ("MSA"). Useful for distinguishing personal vs. org accounts at a glance in `pidge auth list`.

If the config file or its directory doesn't exist at first run, pidge creates them with mode `0700` (user-only on POSIX; default ACLs on Windows). No tokens are ever written to this file.

### Keychain entry format

```json
{
  "access_token": "eyJ0eXAi...",
  "refresh_token": "M.C5xx_...",
  "expires_at": "2026-05-13T23:00:00Z"
}
```

Stored as a single JSON string per (service=`pidge`, account=`<email>`) pair. Single blob (vs. separate access/refresh entries) means a keychain miss for either token is impossible — they're written and read atomically.

## Command surface

### `pidge auth login`

```
$ pidge auth login

Adding a new account to pidge.

  Go to:       https://microsoft.com/devicelogin
  Enter code:  ABCD-1234

Waiting for sign-in… (press Ctrl-C to cancel)
✔ Signed in as Kristofer Liljeblad <kristofer@mklab.se>

This is your first account, so pidge has set it as:
  • Default send-from account
  • Default calendar account

Change with `pidge auth default --send <email>` or `--calendar <email>`.
```

If at least one account is already signed in, the "first account" block is replaced with: *"Added kristofer.liljeblad@live.com. Currently signed in: 2 accounts."*

### `pidge auth list`

```
$ pidge auth list

ACCOUNT                          TENANT                            ADDED
kristofer@mklab.se               mklab.se (11111111-…)             5 days ago   [send] [calendar]
kristofer.liljeblad@live.com     personal MSA                      2 days ago

2 accounts signed in.
```

Tenant column shows a friendly label when known (e.g., `personal MSA` for `9188040d-6c67-...`) and the GUID prefix otherwise.

### `pidge auth status`

```
$ pidge auth status

2 accounts signed in.

Defaults:
  send:     kristofer@mklab.se
  calendar: kristofer@mklab.se

All tokens valid.
```

If any account's refresh token is known-bad (last attempt returned `invalid_grant`), an extra line per affected account: `  ⚠ kristofer@live.com: session expired — run \`pidge auth login\` to re-add`.

### `pidge auth logout`

- No args, multiple accounts: interactive picker via `inquire` listing accounts (existing dep).
- No args, single account: prompts confirmation.
- `--account <email>`: non-interactive, errors if not signed in.
- `--all`: removes every account and clears defaults. Confirms unless `-y/--yes`.
- If the removed account was a default, a follow-up prompt asks which remaining account should inherit that default; `--new-default <email>` bypasses the prompt.

### `pidge auth default`

```
$ pidge auth default
send:     kristofer@mklab.se
calendar: kristofer@mklab.se

$ pidge auth default --send kristofer.liljeblad@live.com
✔ Default send account → kristofer.liljeblad@live.com

$ pidge auth default --send foo@bar.com
Error: not signed in to foo@bar.com. Run `pidge auth login` first.
```

`--send` and `--calendar` are independent flags. Both can be set in one invocation.

### `pidge inbox list`

```
$ pidge inbox list

ACCOUNT                       FROM                SUBJECT                                  RECEIVED
kristofer@mklab.se            Maria Lindberg      ● Quarterly numbers ready for review       5m ago
kristofer@live.com            GitHub              [mklab-se/pidge] CI passing for v0.1.1      2h ago
kristofer@mklab.se            Calendly            Meeting confirmed: Pidge demo, Friday     yesterday
…
```

**Defaults:** all signed-in accounts, top 25 merged sorted by `receivedDateTime` desc.

**Flags:**
- `--account <email>` — filter to one account (repeatable for a subset). When the result is one-account-only, the ACCOUNT column is hidden.
- `-n` / `--limit <N>` — max rows in merged output. Default 25. Per-account fetch is `ceil(N * 1.2 / num_accounts).max(10)` to balance over-fetch vs. correctness.
- `--unread` — adds Graph `$filter=isRead eq false` per request.
- `--output text|json` — global flag pattern; `text` (default) is the table above, `json` is an array of message objects.

**Per-row rendering:**
- `●` dimmed magenta glyph before `FROM` when `is_read == false`. Removed with `--no-color`.
- `RECEIVED` is relative: `5m ago` / `2h ago` for the last 24h, `yesterday`, weekday name for within a week, `Mar 12` otherwise. Computed in the user's local timezone.
- Subject column gets remaining terminal width with `…` truncation.

**JSON shape:**

```json
[
  {
    "account": "kristofer@mklab.se",
    "id": "AAMkAGI2T...",
    "from": { "name": "Maria Lindberg", "address": "maria@mklab.se" },
    "subject": "Quarterly numbers ready for review",
    "received_at": "2026-05-13T22:00:00Z",
    "is_read": false,
    "preview": "Hi Kristofer, attached are the Q1 numbers…"
  }
]
```

`preview` is the Graph `bodyPreview` field (first ~255 plain-text chars). Included only in JSON output, not the table.

**Merge implementation:** Fetch happens in parallel across accounts (`futures::future::join_all`). Each request uses `$select=id,subject,from,receivedDateTime,isRead,bodyPreview&$orderby=receivedDateTime desc&$top=<per-account-N>`. After all requests return, results are flattened, sorted by `received_at` desc, sliced to N. A failure on one account does NOT fail the command — it surfaces a `WARNING:` to stderr and the command exits 0 if at least one account succeeded (exit 1 if all failed).

## Error handling

| Condition | UX | Exit code |
|---|---|---|
| `client_id()` returns None (not provisioned) | `pidge has not been provisioned yet. The maintainer needs to run scripts/register-pidge-app.sh and update APP_CLIENT_ID.` | 2 |
| Keychain unavailable | `OS keychain is not available on this system. pidge needs libsecret on Linux (apt install libsecret-1-0) or Keychain on macOS.` | 2 |
| Device code flow user denies | `Sign-in cancelled.` (No keychain or config mutation.) | 1 |
| Device code timeout | `Sign-in timed out. Run \`pidge auth login\` again.` | 1 |
| Refresh `invalid_grant` | `Session expired for <email>. Run \`pidge auth login\` to re-add this account.` Other accounts unaffected. | 3 |
| Graph 401 after refresh | Treated as session-expired (same as above). | 3 |
| Graph 429 | Honor `Retry-After`. Retry up to 3 times. If still failing, surface the error. | 1 |
| Graph 5xx | Retry once with 1s backoff. | 1 |
| Network unreachable | `anyhow` chain via reqwest. | 1 |
| `inbox list` with zero accounts | `No accounts signed in. Run \`pidge auth login\` to add one.` | 1 |
| `inbox list` partial failure | Print successful results, `WARNING:` line per failed account to stderr. | 0 (if any success) / 1 (if all fail) |

## Dependencies

Added to `[workspace.dependencies]`:

| Crate | Version | Purpose |
|---|---|---|
| `keyring` | `3` | OS keychain access |
| `base64` | `0.22` | Decoding the middle segment of the id_token JWT for the `tid` claim |
| `serde_yaml` | `0.9` | Reading/writing `~/.config/pidge/config.yaml` (foundation didn't ship YAML) |
| `wiremock` | `0.6` | Test-only HTTP mock for auth + Graph endpoints |
| `comfy-table` | `7` | Table output (matches cosq) |
| `inquire` | `0.7` | Interactive prompts (matches cosq) |
| `futures` | `0.3` | `join_all` for parallel per-account fetch |
| `url` | `2` | Explicit pin (transitive via reqwest) |

Already in workspace from foundation: `reqwest`, `serde`, `serde_json`, `tokio`, `chrono`, `anyhow`, `thiserror`, `tracing`, `dirs`, `colored`, `semver`.

**No OAuth library.** The device code and refresh-token flows are hand-rolled per RFC 8628 in `pidge-client/src/auth/{device_code,refresh}.rs` (~125 LOC total). The trade-off: more code we own, but one consistent style across the auth crate and no library churn (the popular `oauth2` crate had breaking changes between v3→v4→v5).

## Testing strategy

### `pidge-core`
- Round-trip serialize/deserialize tests for `Config` (with and without `defaults`, with 0/1/N accounts).
- `AccountSet::set_default(...)` rejects unknown emails.
- Tempfile-backed config path used in all I/O tests; no real filesystem state.

### `pidge-client::auth`
- `wiremock` server returns canned responses for the device-code endpoint and token endpoint.
- Happy path: device code → `authorization_pending` 2x → success.
- Refresh: load expired-on-disk token → wiremock returns new token → keychain updated.
- `invalid_grant` path: refresh fails → typed error returned.
- Keychain tests use `keyring`'s in-memory `MockBackend` (the crate ships one for testing). CI never touches the real OS keychain.

### `pidge-client::graph::mail`
- `wiremock` returns fixture Graph JSON for `GET /me/mailFolders/inbox/messages`.
- Verify URL has `$top`, `$select`, `$orderby`, `$filter` when expected.
- 401-then-refresh-then-retry path. 429-with-Retry-After path.

### `pidge`
- Clap round-trip: `pidge inbox list --unread -n 5 --output json --account a@b.com --account c@d.com` parses to the expected struct.
- Relative-time formatter (unit tests).
- Merge logic: given mocked per-account `Vec<Message>`, verify sort + slice + ACCOUNT column visibility.
- Output rendering: table layout snapshot test for a fixed set of fixture rows.

### Manual / end-to-end
- One scripted walkthrough in `crates/pidge-client/TESTING.md`: register a personal test app, set `PIDGE_CLIENT_ID`, run `pidge auth login` with a real account, run `pidge inbox list`. Not automated.

## Release pipeline updates

The release workflow's `crates-io` job currently publishes one crate. Now it publishes three in dependency order with 30s sleeps between (matches cosq's pattern):

```yaml
- name: Publish pidge-core
  run: cargo publish -p pidge-core
- name: Wait
  run: sleep 30
- name: Publish pidge-client
  run: cargo publish -p pidge-client
- name: Wait
  run: sleep 30
- name: Publish pidge
  run: cargo publish -p pidge
```

`crates/pidge-core/Cargo.toml` and `crates/pidge-client/Cargo.toml` need their own metadata (description, readme, keywords) to be publishable. The `pidge` binary crate keeps its existing metadata.

`/release` skill is updated:
- Step 4 (Bump version numbers): now mentions internal-crate dependency versions in `[workspace.dependencies]` need updating in addition to `[workspace.package].version` (cosq's pattern).

## CHANGELOG entry (`[Unreleased]` additions)

```markdown
### Added
- Workspace split into `pidge`, `pidge-core`, `pidge-client`
- OAuth 2.0 device code sign-in for Microsoft 365 and personal Microsoft accounts (`pidge auth login`)
- Multi-account support: `pidge auth list`, `pidge auth status`, `pidge auth logout`, `pidge auth default --send/--calendar`
- Tokens stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux libsecret)
- `pidge inbox list` — list messages across all signed-in accounts, filterable by `--account`, `--unread`, `-n <limit>`, `--output text|json`
- One-time setup script `scripts/register-pidge-app.sh` for registering the pidge app in Entra
```

## Deferred, in roadmap order

1. `pidge mail send`, `pidge mail draft`, `pidge mail reply` — uses `Mail.Send` + `Mail.ReadWrite` already consented
2. `pidge mail delete`, `pidge mail move`, `pidge mail mark-read` — `Mail.ReadWrite`
3. `pidge inbox search` and folder navigation (`pidge mail folders`, `pidge inbox list --folder <name>`)
4. `pidge calendar list`, `pidge calendar add`, `pidge calendar delete` — uses `Calendars.ReadWrite` already consented
5. Per-account scope override (e.g., a "read-only" secondary account)
6. Local cache / offline mode
7. Gmail provider (uses the same `pidge-core::Message` contract)

## Risks and open questions

- **`keyring` headless Linux.** No `libsecret` means no token storage. Acceptable to error with install instructions for now. A future "encrypted file fallback" can be added if real users hit this.
- **Microsoft personal-account tenant ID is a well-known GUID.** `9188040d-6c67-4c5b-b112-36a304b66dad` for personal accounts (MSA) has been stable for years but isn't formally guaranteed. Code uses it only as a friendly label in `pidge auth list`, not as a behavioral assumption.
- **`expires_in` is sometimes missing in refresh responses.** When it is, default to 3600s. The 60s clock-skew buffer absorbs minor inaccuracy.
- **Refresh-token rotation behavior differs per account type.** Personal MSA always rotates the refresh token; Entra org accounts often don't. The code persists whatever comes back either way — no branching needed.
- **RFC 8628 polling subtleties.** The `slow_down` response should bump the interval by 5s (per RFC) and continue polling — common mistake is to treat it as an error. The implementation must distinguish `slow_down` (continue with longer interval) from `access_denied` / `expired_token` (abort). Covered by wiremock unit tests for each of the five documented error codes.
- **JWT base64 padding.** Microsoft's id_token middle segment uses base64url *without* padding. The decoder uses `base64::engine::general_purpose::URL_SAFE_NO_PAD` (not `URL_SAFE`). Trivial but easy to get wrong on first attempt.
