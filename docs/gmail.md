# Gmail and Google Calendar

pidge works with Gmail and Google Calendar next to Microsoft 365 and Outlook.com. Mailboxes from both
are merged: "what's new" and "what's on my calendar" cover all of them.

## Why you need your own Google OAuth client

pidge needs to read and change your mail (`gmail.modify`) and calendar events. Google treats Gmail
scopes as *restricted*: an app anyone can use with them must pass Google's verification and a
security assessment. pidge is not a service offered to the public, so it does not ship a Google
client. Instead, whoever runs pidge creates their own in a few minutes, and the consent screen says
it is them.

Scopes pidge asks for: `openid email`, `https://www.googleapis.com/auth/gmail.modify`,
`https://www.googleapis.com/auth/calendar.events` and
`https://www.googleapis.com/auth/calendar.freebusy`.

## Create the client

1. Open the [Google Cloud console](https://console.cloud.google.com/) and create a project (or pick
   one).
2. **APIs & Services → Library:** enable the **Gmail API** and the **Google Calendar API**.
3. **Google Auth Platform → Branding / Audience:** set up the consent screen. User type
   **External** is fine for a personal Gmail account. Add yourself (and anyone else who will use
   it) as a test user while the app is in *Testing*.
4. **Google Auth Platform → Clients → Create client:**
   - for the CLI and local MCP: type **Desktop app**;
   - for a hosted `pidge-mcp`: type **Web application**, with the authorized redirect URI
     `https://<your server>/callback/google`.
5. Copy the client id and client secret.

### Testing vs. in production

While the consent screen's publishing status is **Testing**, Google issues refresh tokens that
expire after **7 days**, so pidge would ask you to sign in again every week. For lasting use, press
**Publish app**. An unverified app in production shows a warning screen at sign-in ("Google hasn't
verified this app"; choose *Advanced* and continue) and is limited to 100 users, which suits
personal use. Check Google's current documentation if this behaves differently for you: Google
changes these rules from time to time.

## Local: the CLI and `pidge mcp serve`

Give pidge the Desktop client, either in the environment:

```bash
export PIDGE_GOOGLE_CLIENT_ID=1234-abc.apps.googleusercontent.com
export PIDGE_GOOGLE_CLIENT_SECRET=GOCSPX-...
```

or in `config.yaml` (`~/.config/pidge/config.yaml`, `%APPDATA%\pidge\config.yaml` on Windows):

```yaml
google:
  client_id: 1234-abc.apps.googleusercontent.com
  client_secret: GOCSPX-...
```

Google's documentation says a desktop client's secret is not treated as confidential; it still
belongs in your own config, not in a repository.

Then sign in:

```bash
pidge account add --provider google
```

A browser opens on Google's consent screen; pidge listens on `127.0.0.1` for the answer, as it does
for Microsoft. Tokens go to the OS keychain (or a file with `--store file`).

## Hosted: `pidge-mcp`

Set the Web application client on the server:

| Variable | Value |
|---|---|
| `PIDGE_GOOGLE_CLIENT_ID` | the client id |
| `PIDGE_GOOGLE_CLIENT_SECRET` | the client secret (a Container App secret on Azure) |

With `deploy/azure/deploy.sh` (or the release workflow), set `PIDGE_GOOGLE_CLIENT_ID` and
`PIDGE_GOOGLE_CLIENT_SECRET` before deploying; the Bicep template stores the secret as a Container
App secret. A user then asks their agent to connect Gmail (`accounts action=connect
provider=google`) and opens the link it returns. The same rules as for Microsoft mailboxes apply:
the link works once, only in the browser that opened it, and a mailbox another user owns cannot be
connected.

## How Gmail maps onto pidge

| pidge | Gmail |
|---|---|
| inbox | messages with the `INBOX` label |
| archive | remove `INBOX` |
| delete | move to Trash (never permanent) |
| move to a folder | add that label, remove `INBOX` |
| categories | your user labels |
| flag | star |
| bulk signals | Gmail's Promotions, Social, Updates and Forums tabs |
| calendar | your primary Google Calendar |
| online meeting | a Google Meet link |

Things that differ from Microsoft:

- Folders and categories are both labels in Gmail, so a label used as a folder also shows up as a
  category.
- A cancellation cannot carry a note: Google sends attendees its own notice.
- Forwards are text only; the original's attachments are not attached again.
- Free/busy of other people works where Google lets your account see their calendars.
