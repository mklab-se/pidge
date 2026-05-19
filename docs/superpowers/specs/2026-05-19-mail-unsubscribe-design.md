# `pidge mail unsubscribe` — design

## Goal

Let the user opt out of a sender's bulk mail with one command — without
opening a browser, without hunting for fine-print unsubscribe links,
without copy-pasting URLs out of a terminal. The driver is that pidge
is operated by an AI agent on the user's behalf, and "please
unsubscribe me from X" is a recurring ask the agent currently can't
fulfill.

## Background: the RFCs

Bulk mail carries opt-out metadata in headers. Two RFCs matter:

**RFC 2369 (1998)** defined `List-Unsubscribe`:

```
List-Unsubscribe: <mailto:unsub-abc123@news.example.com>, <https://example.com/u?token=...>
```

Comma-separated, each value in `<>`. The mailto form sends an empty
e-mail to a per-recipient address; the HTTPS form opens a URL.

**RFC 8058 (2017)** added `List-Unsubscribe-Post`:

```
List-Unsubscribe-Post: List-Unsubscribe=One-Click
```

This declares that POSTing `List-Unsubscribe=One-Click` (as
`application/x-www-form-urlencoded`) to the HTTPS URL will unsubscribe
without a confirmation page. The reason it exists: corporate scanners
and "safelink" wrappers GET every URL in incoming mail, which used to
accidentally trigger unsubscribes. POST semantics keep scanners harmless
while letting clients offer real one-click buttons.

## Surface

```
pidge mail unsubscribe <hash> [-y|--yes]
```

- `<hash>` — the 8-char short hash already used by `mail show`, `mail
  delete`, etc. Resolved against the existing local cache.
- `-y` — skip the confirmation prompt. Default behaviour is to prompt,
  consistent with `mail delete`.

`--json` is intentionally left out of v1 to keep the surface tight; the
human output is enough to script with via exit codes. Add it later if a
real consumer asks for it.

## Method selection

When the message has unsubscribe metadata, pick the first method that
applies:

1. **RFC 8058 one-click POST.** Both `List-Unsubscribe` (with an HTTPS
   entry) and `List-Unsubscribe-Post: List-Unsubscribe=One-Click` are
   present. Fastest, no Sent Items clutter, no browser.
2. **mailto.** `List-Unsubscribe` has a `mailto:` entry. Send via the
   existing Graph send path **from the account that received the
   message** (the unsubscribe address is usually tied to a specific
   recipient, so sending from the wrong mailbox would no-op). Honour
   `?subject=` and `?body=` query parameters from the mailto URL if
   present (RFC 6068); otherwise use subject `"unsubscribe"` and an
   empty body. Leaves an audit trail in Sent Items.
3. **Bail.** Only an HTTPS URL with no one-click marker. A bare GET
   often just opens a "click to confirm" page and may not unsubscribe.
   Print the URL and tell the user to click it.

If `List-Unsubscribe` is absent entirely, error: "no unsubscribe header
on this message".

## Confirmation

The default prompt is:

```
Unsubscribe from "Thomson Carter <info@thomsoncarter.com>"
  using one-click POST to https://ctrk.klclick1.com/l/01...HBHF_2?
  [y/N]
```

`-y` skips. Always prompt by default because the mailto form sends a
real e-mail in the user's name (visible in Sent Items, visible to the
sender) and the POST form is functionally irreversible (no automated
re-subscribe).

## Code layout

| Where | What |
|---|---|
| `crates/pidge-client/src/graph/messages.rs` (or wherever message fetches live) | Add `fetch_message_headers(message_id) -> Vec<(String, String)>` using `$select=internetMessageHeaders` on `/me/messages/{id}` |
| `crates/pidge-client/src/unsubscribe.rs` (new) | Pure parsing of `List-Unsubscribe` + `List-Unsubscribe-Post`, returning an `UnsubscribeMethod` enum (`OneClickPost(Url)`, `Mailto(EmailAddress)`, `HttpsOnly(Url)`, `None`). No I/O. |
| `crates/pidge/src/commands/mail_unsubscribe.rs` (new) | Owns the CLI command: hash → message → header fetch → method pick → confirmation → dispatch. Dispatches POST via the existing `reqwest::Client`, mailto via the existing send path. |
| `crates/pidge/src/cli.rs` | Register `Unsubscribe` under the `Mail` subcommand enum. |

The pure parsing module lives in `pidge-client` so it can be unit-tested
without HTTP and reused if pidge ever grows a bulk surface.

## Error handling

- Header fetch fails → bubble Graph error verbatim.
- One-click POST returns non-2xx → surface status + response body
  prefix, suggest manual click (still print the URL).
- Mailto send fails → bubble send error.
- Header parse fails → print the raw `List-Unsubscribe` value so the
  user can act manually, exit non-zero.

## Tests

**Parser unit tests (in `pidge-client`):**

- Single mailto, single https, both
- One-click POST marker present vs absent
- Multi-line continuation (header folded with leading whitespace)
- Whitespace inside `<>`, trailing semicolons, no-bracket forms (some
  senders skip the brackets despite the RFC)
- URL with commas in query string (don't split inside `<>`)

**Picker unit test:**

- Given a parsed header set, asserts the selected `UnsubscribeMethod`
  matches the preference order.

**Manual integration:**

- Run against the live Thomson Carter mail in the user's inbox after
  the implementation lands.

## Out of scope for v1

- Bulk `--from <sender>` or `--all-from <sender>`
- Interactive inbox-wide unsubscribe picker
- Server-side Outlook rule creation (Graph supports it; different
  surface)
- Tracking which senders we've already unsubscribed from

All easy follow-ups if they earn their keep.

## Open questions

None at design time. The flow is small enough that anything
unexpected will surface during implementation.
