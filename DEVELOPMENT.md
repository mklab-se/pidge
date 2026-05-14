# pidge — Development setup

Most workflows are documented in [CONTRIBUTING.md](CONTRIBUTING.md). This file covers two developer-only concerns:

1. One-time Entra app registration (the `client_id` the binary needs)
2. Local development without the registered app (env-var override)

## 1. Register the pidge app in Entra

pidge talks to Microsoft Graph as a public client (no client secret) using OAuth 2.0 device authorization grant. Microsoft requires every such app to be registered in an Entra tenant. **This is a one-time setup done by the maintainer**, never by end-users.

### Automated (recommended)

```bash
bash scripts/register-pidge-app.sh
```

Requires the Azure CLI (`az`). The script will:

1. Confirm you're logged in with `az login`.
2. Run `az ad app create` with the right settings (multi-tenant + personal MS accounts, fallback public client enabled, the five Microsoft Graph delegated permissions in `scripts/pidge-app-permissions.json`).
3. Print the resulting `client_id` GUID.

Paste the GUID into `crates/pidge-client/src/auth/config.rs`:

```rust
pub const APP_CLIENT_ID: &str = "<GUID-from-script>";
```

Commit and you're done. End-users of pidge installed via brew/cargo will never need to register anything.

### Manual portal fallback

If you can't or don't want to use the Azure CLI:

1. Open <https://portal.azure.com> → **Microsoft Entra ID** → **App registrations** → **New registration**.
2. Name: `pidge`.
3. Supported account types: **Accounts in any organizational directory (Any Microsoft Entra ID tenant — Multitenant) and personal Microsoft accounts (e.g. Skype, Xbox)**.
4. Redirect URI: leave empty (we use device code flow, no redirect needed).
5. Click **Register**.
6. From the app overview, copy the **Application (client) ID** — this is your `APP_CLIENT_ID`.
7. Go to **Authentication** → enable **Allow public client flows** (Yes) → Save.
8. Go to **API permissions** → **Add a permission** → **Microsoft Graph** → **Delegated permissions** → check:
   - `offline_access`
   - `User.Read`
   - `Mail.ReadWrite`
   - `Mail.Send`
   - `Calendars.ReadWrite`
   → **Add permissions**.
9. (Optional, for org tenants: click **Grant admin consent**. Personal MSA users will consent on first sign-in either way.)
10. Paste the client_id into `crates/pidge-client/src/auth/config.rs`, commit.

## 2. Developing without (or before) the public registration

Until `APP_CLIENT_ID` is populated, `pidge account add` errors with a clear message. To develop against your own test app:

```bash
export PIDGE_CLIENT_ID="<your-test-app-client-id>"
cargo run -- account add
```

The env var overrides the compile-time constant. Unset it to use the baked-in value.
