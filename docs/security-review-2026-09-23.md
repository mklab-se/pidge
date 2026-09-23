# Security review, 2026-09-23

A full review of pidge after `pidge-mcp` went live on the internet at
`pidge.mklab.se`. The questions: can anyone get at a user's mail or
calendar, or at the server itself; can one user reach another user's
data; and what can an e-mail do to an AI agent operating pidge. Everything
below was read in the source and, where it mattered, reproduced with a
test before being fixed. Every fix ships with a regression test.

## What was checked

**pidge-mcp (hosted server)**

- The OAuth 2.1 authorization server: discovery, dynamic client
  registration, the consent pages and their cookies, `/authorize`,
  `/authorize/go`, `/connect`, `/connect/go`, `/callback`, `/token`
  (authorization-code and refresh grants), PKCE, redirect-URI policy,
  code replay, token kinds, issuer/audience binding, the token-generation
  revocation, allowlist enforcement.
- The bearer gate and how identity reaches tools; rmcp's streamable-HTTP
  session handling (each request's own bearer is what a tool sees; session
  ids are UUID v4).
- Cross-user isolation: `UserRecord`/`MailboxRecord` ownership, identity
  pinning (`tid`/`oid`), every tool's `account` handling, cross-mailbox id
  lookups, the per-user read cache and contact cache, download links,
  `accounts_update`/`accounts_connect`/disconnect, secret naming.
- Prompt injection: what third-party text reaches tool output and whether
  it is inside `<untrusted-email-content>`, whether the wrapper can be
  closed early, whether Graph error bodies leak, what the server
  instructions say.
- Agent abuse: every path that can send mail or otherwise reach a third
  party, the send cap, permanent deletion, forwarding.
- Injection into Microsoft Graph: ids and folder ids in URL paths,
  `$search`/`$filter` strings, `@odata.nextLink` continuation.
- The `/dl` download route, the markitdown sandbox, logging (addresses,
  tokens, query strings, headers), Key Vault access, the container, the
  Azure deployment, the GitHub workflows.

**pidge (CLI) and pidge-client**

- Local sign-in (loopback listener, state, PKCE), token storage (keychain
  and file), the `pidge mcp connect` protocol and what it uploads, the
  browser opener, `mail unsubscribe`, guardrails, terminal rendering of
  e-mail content, the emitted SKILL.md.

## Findings and fixes

Severity is for the deployed server as it was; "who" says who could pull
it off.

1. **High. A Microsoft sign-in link handed to a victim signed the victim
   into the attacker's client.** Anyone could register an OAuth client with
   their own redirect URI, start `/authorize`, click Continue in their own
   browser, and take the `login.microsoftonline.com` URL it redirected to.
   Sent to an allowlisted user as "sign in to pidge", that URL completed the
   attacker's flow: the callback issued the attacker's client a code for the
   victim's mailbox, and stored the victim's refresh token. The same trick on
   a connect link (any pidge user can mint one) bound the victim's other
   mailbox to the attacker's pidge account. The consent page was meant to
   stop this, but it only guarded the step *before* Microsoft.
   **Fixed:** the Continue step of both flows sets a cookie scoped to
   `/callback`; the callback refuses (and burns the pending state) unless
   the same browser presents it. Nothing is redirected to the waiting client
   on refusal, since it may be the attacker's.

2. **Medium. Backslashes let an id escape the mailbox.** Ids were checked
   against a denylist (`/ ? # %` and whitespace). The URL parser folds `\`
   to `/` and collapses `..`, so `..\..\users\x\messages\M` addressed
   `/users/x/messages/M` with the caller's own token: any shared or
   delegated mailbox the Microsoft account can reach, never connected to
   pidge, was readable and movable through a prompt-injected agent.
   **Fixed:** ids must match Graph's token alphabet (letters, digits,
   `-_=+.`), with `..` refused.

3. **Medium. Calendar tools were an uncapped, preview-free way to send
   e-mail.** Creating an event with attendees, changing attendees, a
   cancellation note or an RSVP message all make Outlook send mail with
   agent-written text, outside the draft-preview-send model and its 30/hour
   cap. An invite saying "reply with the finance thread" is the exfiltration
   path. **Fixed:** those calls now charge the send cap, and the server
   instructions tell the agent they are e-mails needing the same approval as
   a send.

4. **Medium. `mail unsubscribe` sent attacker-written mail.** The `mailto:`
   branch took subject and body verbatim from the sender's
   `List-Unsubscribe` header and was gated only by `guardrails.unsubscribe`
   (default allow). With `-y`, as the SKILL.md suggests, an agent cleaning up
   an inbox would send attacker-chosen text to an attacker-chosen address from
   the user's mailbox. **Fixed:** gated by `guardrails.send` too, body is
   always `unsubscribe`, subject is one line capped at 100 characters (the
   same rule pidge-mcp already had, now shared code).

5. **Medium. E-mail content could drive the terminal.** The CLI printed
   subjects, sender names, bodies, attachment and folder names and event text
   with control characters intact, so a message could forge an OSC 8 link
   ("Reset your password at microsoft.com" pointing elsewhere), overwrite
   earlier lines, retitle the window, or on some terminals write the
   clipboard. **Fixed:** control characters (all of `char::is_control` except
   newline, carriage return and tab) are stripped where Graph data becomes
   pidge types, and again on rendered HTML, where `&#27;` decodes late.

6. **Medium. Guardrails failed open.** An unreadable `config.yaml` (a crash
   mid-write, a stray edit) turned `guardrails.send: deny` into allow.
   **Fixed:** the action is refused with the reason, and the config is written
   atomically.

7. **Medium, Windows only. Command injection through the browser opener.**
   `cmd /c start "" <url>` received the URL unquoted; a connect link from a
   hostile MCP server containing `&calc.exe` would run it, and legitimate
   URLs with `&` were cut short. **Fixed:** only plain http(s) URLs without
   quotes or control characters are opened, quoted as one raw argument.

8. **Low. Two addresses could share one mailbox secret.** Secret names folded
   every non-alphanumeric character to `-`, so `jane.doe@` and `jane-doe@`
   named the same Key Vault secret. Ownership checks stopped any token leak,
   but a second allowlisted user connecting the look-alike first would have
   locked the real user out of signing in. **Fixed:** names carry a hash
   suffix; secrets under the old name are still read and move on their next
   refresh, so the live deployment keeps its sessions.

9. **Low. Draft preview trusted a reply's headers.** The recipients and
   subject of a reply are the original sender's text but sat outside the
   untrusted block, right above the "next: mail_send" line. **Fixed:** the
   whole preview is one untrusted block.

10. **Low. Hygiene.** `pidge mcp connect` now requires an https server (http
    only for localhost), and the `--store=file` token directory is created
    `0700` so other local users cannot list which addresses are signed in.

## Checked and found sound

- Identity never comes from tool input; every `account`/`from_account` is
  checked against the caller's record; id lookups iterate the caller's
  mailboxes only; the read and contact caches are keyed per user; a
  mailbox secret is usable only if the caller's record lists it *and* the
  secret names the caller as owner; identity pinning refuses a different
  Microsoft account under a known address. No path was found where user A
  can read, write, send from or download from user B's mailbox.
- Tokens: HS256 with a 256-bit key from the OS RNG, `typ` kept per kind,
  `alg: none` impossible, issuer/audience checked, codes single-use, PKCE
  S256 only, refresh tokens honour the allowlist and sign-out generation,
  the generation check fails closed when the store is down.
- Redirect URIs must be https or loopback http and are matched exactly;
  errors in client identity never redirect; `resource` is validated.
- The untrusted wrapper cannot be closed early (any case of the tag name
  inside content is defused); header fields cannot start a line; Graph
  error bodies are replaced by fixed phrases; the MCP prompts are static.
- Only `mail_send` sends mail by draft id; `mail_act delete` moves to
  Deleted Items; no tool creates rules, forwards or delegates; `$search`
  and `$filter` values are escaped; continuation links are pinned to the
  Graph origin; one-click unsubscribe POSTs only to public https hosts,
  resolved and pinned, with no redirects.
- `/dl` names user and mailbox by hash only, checks the allowlist, the
  record, the generation and the hourly budget, serves with `nosniff`,
  `CSP: sandbox` and `attachment` disposition, and refuses with one neutral
  404. Download tokens never carry an address.
- Logs carry route templates, hashed users and fixed phrases: no query
  strings, headers, tokens, addresses or Microsoft error text.
- markitdown runs with a scrubbed environment, a private work directory as
  `HOME`/`TMPDIR`, a memory limit, a timeout, an output cap and stderr
  discarded; the server is non-dumpable.
- Deployment: non-root container, Key Vault RBAC only with purge
  protection, managed identity, https-only ingress, deploy identity
  federated to `main` of this repository only, deploy gated on CI runs of
  pushes to `main` from this repository, no secret reaches a fork PR.
- Client side: loopback listener on `127.0.0.1` with random state and
  PKCE, Microsoft refresh tokens never leave the machine during `pidge mcp
  connect` (the server gets its own through the browser), `Debug` output
  redacts tokens, `RUST_LOG` cannot switch on header tracing.

## Consider next

None of these is an open hole; they are the places where the next unit
of effort would go.

- **Refresh-token rotation.** A refresh token is valid for 30 days and is
  not rotated on use, so a stolen one works until `sign_out_everywhere`.
  Rotating on every refresh (and refusing a reused one) would shorten that
  window at the cost of a small amount of state.
- **Forward previews should list attachments.** `createForward` copies the
  original's attachments into the draft, but the preview the user approves
  shows only recipients, subject and body.
- **Inline third-party strings outside the wrapper.** A few results still
  quote sender-controlled text on one line (`calendar_respond`'s event
  title, an image caption's file name, `mail_act`'s manual-unsubscribe URL).
  They cannot start a line, so the impact is limited to inline wording.
- **Pin third-party GitHub Actions to commit SHAs** in `release.yml`,
  `ci.yml` and `deploy-mcp.yml` (only `azure/login` is pinned today); the
  release job holds the Homebrew tap token.
- **`PIDGE_MCP_ALLOWED_EMAILS` as a Container Apps secret.** It is a GitHub
  secret but lands in plain text in the ARM deployment history and the app
  spec, readable by anyone with Reader on the resource group.
- **Same-origin check for discovered OAuth endpoints** in `pidge mcp
  connect`: today the client trusts whatever `authorization_servers` and
  `token_endpoint` the chosen server advertises. The user chooses the
  server, so this only matters if that server is itself hostile.
- **Cookie comparisons are not constant-time.** The nonces are 128-bit
  random values that live ten minutes; a timing attack is not practical
  over the network, but `subtle`-style comparison would be tidier.
