# `pidge mail unsubscribe` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `pidge mail unsubscribe <hash>` — parses RFC 2369 `List-Unsubscribe` + RFC 8058 `List-Unsubscribe-Post` and actions the opt-out via one-click POST, mailto, or graceful bail.

**Architecture:** A pure parser in `pidge-client` returns an `UnsubscribeMethod` enum. A thin command module in `pidge` fetches the message's Graph headers, runs the parser, prompts for confirmation, and dispatches (HTTPS POST via `reqwest`, mailto via the existing `GraphClient::send_mail`).

**Tech Stack:** Rust 2024, clap, tokio, reqwest, anyhow, inquire (prompts), url, wiremock (tests).

---

## File map

| File | Purpose | New / Modified |
|---|---|---|
| `crates/pidge-client/src/unsubscribe.rs` | Pure parser: header pairs → `UnsubscribeMethod` | **New** |
| `crates/pidge-client/src/lib.rs` | Re-export `unsubscribe::*` | Modify |
| `crates/pidge-client/src/graph/mail.rs` | New `fetch_message_headers` free fn + wiremock test | Modify |
| `crates/pidge-client/src/graph/mod.rs` | Re-export `fetch_message_headers`; add `GraphClient::fetch_message_headers` wrapper | Modify |
| `crates/pidge/src/cli.rs` | Add `Unsubscribe { fragment, yes }` variant + add `"unsubscribe"` to `MAIL_SUBCOMMAND_NAMES` | Modify |
| `crates/pidge/src/commands/mail_unsubscribe.rs` | Command implementation: resolve → fetch → parse → confirm → dispatch | **New** |
| `crates/pidge/src/commands/mod.rs` | Declare new module | Modify |
| `crates/pidge/src/commands/mail.rs` | Route `MailCommands::Unsubscribe` to the new module | Modify |

---

## Task 1: Add `unsubscribe` parser module

**Files:**
- Create: `crates/pidge-client/src/unsubscribe.rs`
- Modify: `crates/pidge-client/src/lib.rs`

The parser is pure — no I/O, no `tokio`, no `reqwest`. That lets us cover every weird header shape with fast unit tests.

- [ ] **Step 1: Write the parser module with tests first.**

Create `crates/pidge-client/src/unsubscribe.rs`:

```rust
//! Parsing of RFC 2369 `List-Unsubscribe` and RFC 8058
//! `List-Unsubscribe-Post` headers — no I/O.
//!
//! See:
//! - <https://www.rfc-editor.org/rfc/rfc2369> (List-Unsubscribe)
//! - <https://www.rfc-editor.org/rfc/rfc8058> (one-click POST)
//! - <https://www.rfc-editor.org/rfc/rfc6068> (mailto: URI)

use url::Url;

/// The opt-out method picked from a message's unsubscribe headers, in
/// preference order: `OneClickPost` → `Mailto` → `HttpsOnly` → `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsubscribeMethod {
    /// RFC 8058 one-click: POST `List-Unsubscribe=One-Click`
    /// (`application/x-www-form-urlencoded`) to this URL. No browser
    /// interaction needed.
    OneClickPost(String),

    /// RFC 2369 `mailto:` — send an e-mail to this address. Per RFC 6068
    /// the URL may carry `?subject=` / `?body=` that override our defaults.
    Mailto {
        address: String,
        subject: Option<String>,
        body: Option<String>,
    },

    /// HTTPS URL exists but no one-click marker. Won't auto-drive; the
    /// caller should surface the URL for a manual click.
    HttpsOnly(String),

    /// No `List-Unsubscribe` header at all.
    None,
}

/// Pick the best `UnsubscribeMethod` for the given message headers.
///
/// Header name comparison is case-insensitive (RFC 5322).
pub fn parse_unsubscribe(headers: &[(String, String)]) -> UnsubscribeMethod {
    let Some(raw) = find_header(headers, "List-Unsubscribe") else {
        return UnsubscribeMethod::None;
    };
    let post = find_header(headers, "List-Unsubscribe-Post");

    let mut https_url: Option<String> = None;
    let mut mailto_entry: Option<(String, Option<String>, Option<String>)> = None;

    for entry in split_entries(raw) {
        if let Some(rest) = entry.strip_prefix("mailto:") {
            if mailto_entry.is_none() {
                mailto_entry = parse_mailto(rest);
            }
        } else if entry.starts_with("https://") || entry.starts_with("http://") {
            if https_url.is_none() {
                https_url = Some(entry.to_string());
            }
        }
    }

    let one_click = post
        .map(|v| v.trim().eq_ignore_ascii_case("List-Unsubscribe=One-Click"))
        .unwrap_or(false);

    if let (Some(url), true) = (https_url.clone(), one_click) {
        return UnsubscribeMethod::OneClickPost(url);
    }
    if let Some((address, subject, body)) = mailto_entry {
        return UnsubscribeMethod::Mailto {
            address,
            subject,
            body,
        };
    }
    if let Some(url) = https_url {
        return UnsubscribeMethod::HttpsOnly(url);
    }
    UnsubscribeMethod::None
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Split a comma-separated `List-Unsubscribe` value, respecting `<>` so URLs
/// with commas in their query string survive intact. Strips the brackets.
fn split_entries(raw: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let bytes = raw.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(strip_brackets(&raw[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < raw.len() {
        out.push(strip_brackets(&raw[start..]));
    }
    out.into_iter().filter(|e| !e.is_empty()).collect()
}

fn strip_brackets(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('<').unwrap_or(s);
    let s = s.strip_suffix('>').unwrap_or(s);
    s.trim()
}

fn parse_mailto(rest: &str) -> Option<(String, Option<String>, Option<String>)> {
    // Prepend the scheme back and let `url` handle percent-decoding for us.
    let full = format!("mailto:{rest}");
    let url = Url::parse(&full).ok()?;
    if url.scheme() != "mailto" {
        return None;
    }
    let address = url.path().to_string();
    if address.is_empty() {
        return None;
    }
    let mut subject = None;
    let mut body = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "subject" => subject = Some(v.into_owned()),
            "body" => body = Some(v.into_owned()),
            _ => {}
        }
    }
    Some((address, subject, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    #[test]
    fn no_header_returns_none() {
        assert_eq!(parse_unsubscribe(&[]), UnsubscribeMethod::None);
    }

    #[test]
    fn only_mailto_picks_mailto() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<mailto:unsub-abc@news.example.com>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::Mailto {
                address: "unsub-abc@news.example.com".into(),
                subject: None,
                body: None,
            }
        );
    }

    #[test]
    fn only_https_without_one_click_returns_https_only() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<https://example.com/u?token=abc>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::HttpsOnly("https://example.com/u?token=abc".into())
        );
    }

    #[test]
    fn https_with_one_click_picks_post() {
        let h = vec![
            hdr("List-Unsubscribe", "<https://example.com/u?token=abc>"),
            hdr("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://example.com/u?token=abc".into())
        );
    }

    #[test]
    fn both_mailto_and_one_click_prefers_one_click() {
        let h = vec![
            hdr(
                "List-Unsubscribe",
                "<mailto:unsub@example.com>, <https://example.com/u?t=a>",
            ),
            hdr("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://example.com/u?t=a".into())
        );
    }

    #[test]
    fn mailto_with_subject_and_body_query_params() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<mailto:unsub@example.com?subject=unsub&body=Please%20remove%20me>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::Mailto {
                address: "unsub@example.com".into(),
                subject: Some("unsub".into()),
                body: Some("Please remove me".into()),
            }
        );
    }

    #[test]
    fn header_name_is_case_insensitive() {
        let h = vec![
            hdr("list-unsubscribe", "<https://x/u>"),
            hdr("LIST-UNSUBSCRIBE-POST", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://x/u".into())
        );
    }

    #[test]
    fn commas_inside_url_brackets_do_not_split_entries() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<https://example.com/u?token=a,b,c>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::HttpsOnly("https://example.com/u?token=a,b,c".into())
        );
    }

    #[test]
    fn one_click_marker_is_case_insensitive() {
        let h = vec![
            hdr("List-Unsubscribe", "<https://x/u>"),
            hdr("List-Unsubscribe-Post", "list-unsubscribe=one-click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://x/u".into())
        );
    }

    #[test]
    fn mailto_with_no_address_is_rejected() {
        let h = vec![hdr("List-Unsubscribe", "<mailto:>")];
        assert_eq!(parse_unsubscribe(&h), UnsubscribeMethod::None);
    }

    #[test]
    fn malformed_header_with_only_whitespace_is_none() {
        let h = vec![hdr("List-Unsubscribe", "   ")];
        assert_eq!(parse_unsubscribe(&h), UnsubscribeMethod::None);
    }
}
```

- [ ] **Step 2: Wire the new module into `lib.rs`.**

Modify `crates/pidge-client/src/lib.rs` — add the module and re-export:

```rust
//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.

pub mod auth;
mod error;
pub mod graph;
pub mod unsubscribe;

pub use auth::AuthClient;
pub use error::ClientError;
pub use graph::{GraphClient, Outgoing};
pub use unsubscribe::{UnsubscribeMethod, parse_unsubscribe};
```

- [ ] **Step 3: Run the new tests and check they pass.**

Run: `cargo test -p pidge-client unsubscribe`
Expected: 11 tests pass.

- [ ] **Step 4: Commit.**

```bash
git add crates/pidge-client/src/unsubscribe.rs crates/pidge-client/src/lib.rs
git commit -m "$(cat <<'EOF'
Add `List-Unsubscribe` header parser

Pure parsing of RFC 2369 + RFC 8058 unsubscribe headers, with a
preference picker (one-click POST > mailto > https-only). No I/O —
fully unit-tested.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Fetch message headers from Graph

**Files:**
- Modify: `crates/pidge-client/src/graph/mail.rs` (add `fetch_message_headers` + wiremock test)
- Modify: `crates/pidge-client/src/graph/mod.rs` (re-export + `GraphClient::fetch_message_headers`)

Microsoft Graph exposes `internetMessageHeaders` on messages as an array of `{ name, value }`. We request just that property to keep the response small.

- [ ] **Step 1: Write the failing wiremock test.**

Add to the `tests` module at the bottom of `crates/pidge-client/src/graph/mail.rs` (immediately before the closing `}` of `mod tests`):

```rust
    #[tokio::test]
    async fn fetch_message_headers_parses_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/me/messages/.+$"))
            .and(query_param("$select", "internetMessageHeaders"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "internetMessageHeaders": [
                    { "name": "List-Unsubscribe", "value": "<mailto:u@x>, <https://x/u>" },
                    { "name": "List-Unsubscribe-Post", "value": "List-Unsubscribe=One-Click" }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let headers = fetch_message_headers(&http, &server.uri(), "AT", "MSGID")
            .await
            .unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "List-Unsubscribe");
        assert_eq!(headers[0].1, "<mailto:u@x>, <https://x/u>");
        assert_eq!(headers[1].0, "List-Unsubscribe-Post");
    }
```

- [ ] **Step 2: Run the test to confirm it fails.**

Run: `cargo test -p pidge-client fetch_message_headers_parses_array`
Expected: FAIL — `fetch_message_headers` is not defined.

- [ ] **Step 3: Implement `fetch_message_headers`.**

Add the function near the other GET helpers in `crates/pidge-client/src/graph/mail.rs` (right after `get_message`'s closing brace at line 379 is a natural spot):

```rust
/// GET /me/messages/{id}?$select=internetMessageHeaders — fetch just the
/// raw RFC 5322 headers for a message. Used by `pidge mail unsubscribe`
/// to locate `List-Unsubscribe` / `List-Unsubscribe-Post`.
pub async fn fetch_message_headers(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<(String, String)>, ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}?$select=internetMessageHeaders");
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let body: GraphHeadersResponse = resp.json().await?;
    Ok(body
        .internet_message_headers
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.name, h.value))
        .collect())
}

#[derive(serde::Deserialize)]
struct GraphHeadersResponse {
    #[serde(rename = "internetMessageHeaders", default)]
    internet_message_headers: Option<Vec<GraphHeader>>,
}

#[derive(serde::Deserialize)]
struct GraphHeader {
    name: String,
    value: String,
}
```

- [ ] **Step 4: Re-export and add the `GraphClient` wrapper.**

Modify `crates/pidge-client/src/graph/mod.rs` — add `fetch_message_headers` to the `pub use mail::{...}` list:

```rust
pub use mail::{
    InboxPage, Outgoing, add_attachment, create_draft, create_forward_draft,
    create_reply_all_draft, create_reply_draft, delete_attachment, delete_message,
    fetch_message_headers, forward_message, get_attachment_bytes, get_message,
    list_attachments, list_drafts, list_inbox, mark_read, mark_unread, move_message,
    reply_all_message, reply_message, search_messages, send_draft, send_mail, set_flag,
    update_draft,
};
```

Then add a method on `GraphClient` right after `get_message` (around line 284):

```rust
    /// GET /me/messages/{id}?$select=internetMessageHeaders.
    pub async fn fetch_message_headers(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<Vec<(String, String)>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::fetch_message_headers(&self.http, &self.base_url, &token, message_id).await
    }
```

- [ ] **Step 5: Run the test to confirm it passes.**

Run: `cargo test -p pidge-client fetch_message_headers_parses_array`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/pidge-client/src/graph/mail.rs crates/pidge-client/src/graph/mod.rs
git commit -m "$(cat <<'EOF'
Add `fetch_message_headers` to Graph client

Selects just `internetMessageHeaders` from /me/messages/{id} and
returns them as (name, value) pairs. Used by upcoming
`pidge mail unsubscribe`.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Register the `mail unsubscribe` subcommand

**Files:**
- Modify: `crates/pidge/src/cli.rs` (add enum variant + add to preprocess list)

This is just clap wiring — no real logic yet. The next task implements the command.

- [ ] **Step 1: Add the `Unsubscribe` variant to `MailCommands`.**

In `crates/pidge/src/cli.rs`, find the `MailCommands` enum (starts around line 180). Insert the new variant immediately after `Delete { ... }` (around line 310, just before `New(ComposeArgs)`):

```rust
    /// Unsubscribe from the sender of a message using its RFC 2369
    /// `List-Unsubscribe` header (RFC 8058 one-click POST when offered;
    /// otherwise mailto; otherwise prints the URL).
    Unsubscribe {
        /// Fragment of the 8-char short hash
        fragment: String,

        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
```

- [ ] **Step 2: Add `"unsubscribe"` to `MAIL_SUBCOMMAND_NAMES`.**

In the same file, the `MAIL_SUBCOMMAND_NAMES` constant (around line 465). Insert `"unsubscribe"` so the array stays roughly alphabetised within the existing groupings — between `"delete"` and `"help"` works:

```rust
pub const MAIL_SUBCOMMAND_NAMES: &[&str] = &[
    "list",
    "show",
    "search",
    "mark-read",
    "mark-unread",
    "flag",
    "unflag",
    "archive",
    "new",
    "reply",
    "reply-all",
    "forward",
    "delete",
    "unsubscribe",
    "help",
];
```

- [ ] **Step 3: Build to confirm the enum compiles (the match in `commands/mail.rs` will fail — that's expected and the next task fixes it).**

Run: `cargo build -p pidge`
Expected: FAIL with non-exhaustive match in `commands/mail.rs::run` — Task 4 closes that.

(No commit here; Task 4 picks up immediately and we commit the two together at the end of Task 4.)

---

## Task 4: Implement the command module

**Files:**
- Create: `crates/pidge/src/commands/mail_unsubscribe.rs`
- Modify: `crates/pidge/src/commands/mod.rs` (declare module)
- Modify: `crates/pidge/src/commands/mail.rs` (route)

The command resolves the fragment, fetches headers, runs the parser, prompts for confirmation, then dispatches:
- `OneClickPost(url)` → POST to that URL with body `List-Unsubscribe=One-Click` (form-urlencoded)
- `Mailto { address, subject, body }` → `GraphClient::send_mail` from the **receiving** account
- `HttpsOnly(url)` → print the URL, exit Ok
- `None` → error

- [ ] **Step 1: Create the command module.**

Create `crates/pidge/src/commands/mail_unsubscribe.rs`:

```rust
//! `pidge mail unsubscribe` — opt out of a sender using the message's
//! `List-Unsubscribe` / `List-Unsubscribe-Post` headers.
//!
//! Preference order: RFC 8058 one-click POST → RFC 2369 mailto → bail with
//! the HTTPS URL. See `pidge-client::unsubscribe` for the parser.

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use inquire::Confirm;

use pidge_client::{AuthClient, GraphClient, Outgoing, UnsubscribeMethod, parse_unsubscribe};

use crate::commands::mail_fragment::resolve;

pub async fn run(fragment: String, yes: bool) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    let headers = graph
        .fetch_message_headers(&msg.account, &msg.graph_id)
        .await
        .context("fetching message headers from Microsoft Graph")?;

    let method = parse_unsubscribe(&headers);

    match method {
        UnsubscribeMethod::None => Err(anyhow!(
            "Message {short} has no `List-Unsubscribe` header — there is no \
             standard way to unsubscribe from this sender. Look for an \
             unsubscribe link in the body or contact the sender directly."
        )),
        UnsubscribeMethod::HttpsOnly(url) => {
            // GET on a bare HTTPS unsubscribe URL usually shows a "click to
            // confirm" page; pidge can't reliably drive that without a
            // browser. Surface the URL so the user can click it.
            println!(
                "{} Message {short}: only an HTTPS unsubscribe URL is offered, \
                 and there is no `List-Unsubscribe-Post: List-Unsubscribe=One-Click` \
                 marker.\n  Open this URL in a browser to finish:\n  {}",
                "⚠".yellow(),
                url.cyan().underline(),
            );
            Ok(())
        }
        UnsubscribeMethod::OneClickPost(url) => {
            if !confirm(yes, &format!(
                "Unsubscribe from {} via one-click POST to {url}?",
                msg.account
            ))? {
                println!("Aborted.");
                return Ok(());
            }
            one_click_post(&url).await?;
            println!(
                "{} Unsubscribed via one-click POST ({})",
                "✔".green(),
                url.dimmed()
            );
            Ok(())
        }
        UnsubscribeMethod::Mailto { address, subject, body } => {
            let subject = subject.unwrap_or_else(|| "unsubscribe".to_string());
            let body = body.unwrap_or_default();
            if !confirm(yes, &format!(
                "Send unsubscribe e-mail to <{address}> from {} (subject: \"{subject}\")?",
                msg.account
            ))? {
                println!("Aborted.");
                return Ok(());
            }
            let outgoing = Outgoing {
                subject,
                body_text: body,
                to: vec![address.clone()],
                cc: vec![],
                bcc: vec![],
            };
            graph
                .send_mail(&msg.account, &outgoing)
                .await
                .context("Graph rejected the unsubscribe e-mail")?;
            println!(
                "{} Sent unsubscribe e-mail to {} (audit copy in Sent Items)",
                "✔".green(),
                address.dimmed()
            );
            Ok(())
        }
    }
}

fn confirm(yes_flag: bool, prompt: &str) -> Result<bool> {
    if yes_flag {
        return Ok(true);
    }
    Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt cancelled: {e}"))
}

/// POST `List-Unsubscribe=One-Click` to the given URL per RFC 8058. The
/// body is form-urlencoded (the RFC says so explicitly).
///
/// We use a fresh `reqwest::Client` rather than the Graph client's — this
/// request goes to an arbitrary third-party host, not Microsoft, so it
/// must not carry the Graph bearer token.
async fn one_click_post(url: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building HTTP client")?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .context("POST failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let trimmed: String = body.chars().take(200).collect();
        return Err(anyhow!(
            "Unsubscribe endpoint returned HTTP {status}. Response (first 200 chars): {trimmed}\n\
             Try the URL in a browser instead: {url}"
        ));
    }
    Ok(())
}
```

- [ ] **Step 2: Declare the module.**

Modify `crates/pidge/src/commands/mod.rs` — add the declaration in alphabetical order with the other `mail_*` modules:

```rust
//! CLI command implementations

pub mod account;
pub mod account_add;
pub mod account_default;
pub mod account_list;
pub mod account_migrate;
pub mod account_remove;
pub mod ai;
pub mod attachments;
pub mod completion;
pub mod compose_form;
pub mod drafts;
pub mod mail;
pub mod mail_actions;
pub mod mail_compose;
pub mod mail_delete;
pub mod mail_fragment;
pub mod mail_search;
pub mod mail_show;
pub mod mail_unsubscribe;
pub mod skill;
pub mod trust;
```

- [ ] **Step 3: Route the new subcommand.**

Modify `crates/pidge/src/commands/mail.rs` — add an arm to the `match command { ... }` inside `pub async fn run`. Put it after the `Delete { ... }` arm, just before the closing brace of the match:

```rust
        MailCommands::Unsubscribe { fragment, yes } => {
            crate::commands::mail_unsubscribe::run(fragment, yes).await
        }
```

- [ ] **Step 4: Build the workspace.**

Run: `cargo build --workspace`
Expected: clean build (warnings count as failures only under clippy — that comes later).

- [ ] **Step 5: Run the full test suite.**

Run: `cargo test --workspace`
Expected: all tests pass (the only new tests are the parser tests from Task 1 and the wiremock from Task 2).

- [ ] **Step 6: Commit the CLI + command-module changes together.**

```bash
git add crates/pidge/src/cli.rs \
        crates/pidge/src/commands/mod.rs \
        crates/pidge/src/commands/mail.rs \
        crates/pidge/src/commands/mail_unsubscribe.rs
git commit -m "$(cat <<'EOF'
Add `pidge mail unsubscribe <hash>`

Resolves the message, fetches its `List-Unsubscribe` headers, picks
the best method (one-click POST > mailto > https-only bail), prompts
for confirmation, then acts. The mailto path sends from the account
that received the message via the existing send_mail path; the
one-click path uses a fresh reqwest::Client so the Graph bearer
token never leaves Microsoft hosts.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Pre-flight + manual integration test

**Files:** none (verification only)

- [ ] **Step 1: Format check.**

Run: `cargo fmt --all -- --check`
Expected: no diff.

If it fails, run `cargo fmt --all` and commit the formatting change:
```bash
git add -A
git commit -m "fmt"
```

- [ ] **Step 2: Clippy with `-D warnings`.**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean.

If clippy complains, fix it (don't `#[allow(...)]` past it) and commit:
```bash
git add -A
git commit -m "clippy fixes for mail unsubscribe"
```

- [ ] **Step 3: Confirm the new subcommand appears in help.**

Run: `cargo run --quiet -- mail unsubscribe --help`
Expected output starts with:
```
Unsubscribe from the sender of a message using its RFC 2369
`List-Unsubscribe` header (RFC 8058 one-click POST when offered;
otherwise mailto; otherwise prints the URL).

Usage: pidge mail unsubscribe [OPTIONS] <FRAGMENT>
```

- [ ] **Step 4: Manual integration test against the live Thomson Carter mail.**

Find the Thomson Carter message in the user's inbox:
```bash
cargo run --quiet -- mail search "from:Thomson" --json | jq -r '.[0].id'
```

Then dry-run with the prompt to inspect what method is picked:
```bash
cargo run --quiet -- mail unsubscribe <hash>
```

When prompted, answer `n` first to see which method the parser chose for this sender. Report back what method appeared. If `OneClickPost` or `Mailto`, re-run with `-y` to actually unsubscribe:
```bash
cargo run --quiet -- mail unsubscribe <hash> -y
```

If the picked method is `HttpsOnly`, that's the bail path — note the URL and we can open it manually (or via Chrome automation as a separate follow-up).

- [ ] **Step 5: Final sweep — make sure everything is still green.**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: all three commands succeed without output that breaks the gate.

---

## Self-review notes

Run this checklist before declaring the plan done:

**Spec coverage** — each spec section maps to:
- *RFC explanation* → Task 1 (the parser module + its docs codify the RFCs)
- *Surface (`pidge mail unsubscribe <hash> [-y] [--json]`)* — `--json` is **not** in the plan. **Drop it from the spec**, or add a step here. Re-reading the spec it's listed under "Surface" but never elaborated; the spec is over-promising. Plan keeps the v1 small; if the user wants `--json` we add it as a follow-up after seeing the live thing work.
- *Method selection (one-click → mailto → bail)* → Task 1 parser + Task 4 dispatch
- *Confirmation* → Task 4 step 1 (`confirm` helper)
- *Code layout table* → matches Tasks 1–4 file map exactly
- *Error handling cases* → Task 4 covers Graph fetch failure (`context`), POST non-2xx (`anyhow!`), mailto send failure (`context`), missing header (`anyhow!`). Header parse failure: in practice the parser returns `None` for unparseable input, which is treated as "no unsubscribe header" — acceptable, since the alternative is to fail noisily on a class of senders that includes some legitimate but quirky bulk lists.
- *Tests* → Task 1 has the parser table; Task 2 has the wiremock for `fetch_message_headers`. There is intentionally no picker test separate from the parser tests — the picker IS the parser's job in this design, and the parser's tests cover every branch.

**Placeholder scan** — no TBDs, no "implement appropriate", no "similar to". Every step shows real code or a real shell command.

**Type consistency** — `UnsubscribeMethod` variant names match between the parser tests, the lib re-export, and the dispatch match in `commands/mail_unsubscribe.rs`. `fetch_message_headers` signature matches between the free fn, the wiremock test, and the `GraphClient` wrapper.

**Scope check** — single feature, ~250 LOC across four files, single plan.
