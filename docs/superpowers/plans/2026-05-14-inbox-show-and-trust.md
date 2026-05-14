# `pidge inbox show` + trusted-senders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `pidge inbox show <fragment>` (substring-lookup → Graph fetch → header/body/attachment rendering), the trusted-senders feature with `pidge trust list/add/remove`, and inline image rendering via the terminal's image protocol for trusted senders.

**Architecture:** New `FullMessage` and `Attachment` types in `pidge-core`; four new Graph endpoints (`get_message`, `list_attachments`, `get_attachment_bytes`, `mark_read`) in `pidge-client`; new CLI surface `pidge inbox show` and `pidge trust …`; HTML body rendering via `html2text`, inline image rendering via `viuer` with `image` for decoding.

**Tech Stack:** Rust 2024 edition. New deps: `html2text`, `viuer`, `image` (png/jpeg/webp only), `humansize`. Existing infrastructure: `pidge-core::MessageCache::find_by_fragment`, `linkify_text`, `colored`, `comfy-table`.

**Reference spec:** `docs/superpowers/specs/2026-05-14-inbox-show-and-trust-design.md`

**Working directory:** `/Users/kristofer/repos/mklab-se/pidge`

---

## File inventory

### New
```
crates/pidge/src/commands/inbox_show.rs    # `pidge inbox show` implementation
crates/pidge/src/commands/trust.rs         # `pidge trust list/add/remove`
```

### Modified
```
Cargo.toml                                 # add html2text, viuer, image, humansize deps
crates/pidge-core/Cargo.toml               # (no new deps; just uses chrono/serde)
crates/pidge-core/src/message.rs           # add FullMessage, BodyContentType, Attachment
crates/pidge-core/src/config.rs            # add trusted_senders + 3 methods
crates/pidge-core/src/lib.rs               # re-export new types
crates/pidge-client/Cargo.toml             # add base64.workspace = true (if not present), humansize? no
crates/pidge-client/src/graph/mail.rs      # get_message, list_attachments, get_attachment_bytes, mark_read
crates/pidge-client/src/graph/mod.rs       # GraphClient wrapper methods
crates/pidge/Cargo.toml                    # html2text, viuer, image, humansize deps
crates/pidge/src/cli.rs                    # InboxCommands::Show, Commands::Trust, TrustCommands
crates/pidge/src/commands/mod.rs           # declare inbox_show, trust modules
crates/pidge/src/commands/inbox.rs         # extend run() to dispatch Show
CHANGELOG.md                               # [Unreleased] entries
```

---

## Task 1: Add workspace deps

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Edit root `Cargo.toml`**

Open `/Users/kristofer/repos/mklab-se/pidge/Cargo.toml`. In `[workspace.dependencies]`, after the existing comfy-table line (or grouped sensibly), add:

```toml
html2text = "0.12"
viuer = "0.9"
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "webp"] }
humansize = "2"
```

- [ ] **Step 2: Verify workspace builds with the new deps**

Run: `cargo build --workspace 2>&1 | tail -3`
Expected: clean Finished message. Cargo will download `html2text`, `viuer`, `image`, `humansize` plus their transitives (notably `crossterm` for viuer, `png` crate for image).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "Add html2text, viuer, image, humansize deps for inbox show"
```

---

## Task 2: `pidge-core::message` — `FullMessage` + `Attachment` types

**Files:**
- Modify: `crates/pidge-core/src/message.rs`
- Modify: `crates/pidge-core/src/lib.rs`

- [ ] **Step 1: Append new types to `message.rs`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/message.rs`. AFTER the existing `MessageFrom` struct and its derive, append:

```rust
/// Full message content as returned by `GraphClient::get_message`.
/// Compared to `Message` (the list-row shape), this carries full body,
/// all recipient lists, sent/received timestamps, and a `has_attachments`
/// flag for triggering the attachment fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullMessage {
    pub account: String,
    pub id: String,
    pub from: MessageFrom,
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    pub bcc: Vec<MessageFrom>,
    pub subject: String,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub sent_at: chrono::DateTime<chrono::Utc>,
    pub is_read: bool,
    pub body_content_type: BodyContentType,
    pub body_content: String,
    pub has_attachments: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyContentType {
    Text,
    Html,
}

/// An attachment listed on a message. Bytes are NOT included — fetch separately
/// via `GraphClient::get_attachment_bytes(account, message_id, attachment.id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Provider-specific identifier (Microsoft Graph attachment id).
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub is_inline: bool,
    pub content_id: Option<String>,
}
```

- [ ] **Step 2: Add a JSON round-trip test for FullMessage**

Add to the `#[cfg(test)] mod tests` block at the bottom of `message.rs`:

```rust
    #[test]
    fn full_message_roundtrips_through_json() {
        let m = FullMessage {
            account: "u@e.com".into(),
            id: "graph-id".into(),
            from: MessageFrom {
                name: "Maria".into(),
                address: "maria@mklab.se".into(),
            },
            to: vec![MessageFrom {
                name: "Kristofer".into(),
                address: "kristofer@mklab.se".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            received_at: chrono::DateTime::parse_from_rfc3339("2026-05-14T22:00:00Z")
                .unwrap()
                .to_utc(),
            sent_at: chrono::DateTime::parse_from_rfc3339("2026-05-14T21:59:30Z")
                .unwrap()
                .to_utc(),
            is_read: false,
            body_content_type: BodyContentType::Html,
            body_content: "<p>Hello</p>".into(),
            has_attachments: true,
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: FullMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn attachment_roundtrips_through_json() {
        let a = Attachment {
            id: "att-1".into(),
            name: "report.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 12345,
            is_inline: false,
            content_id: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        let a2: Attachment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, a2);
    }

    #[test]
    fn body_content_type_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&BodyContentType::Html).unwrap(), "\"html\"");
        assert_eq!(serde_json::to_string(&BodyContentType::Text).unwrap(), "\"text\"");
    }
```

- [ ] **Step 3: Re-export from `lib.rs`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/lib.rs`. Find the existing `pub use message::{Message, MessageFrom};` line. Replace with:

```rust
pub use message::{Attachment, BodyContentType, FullMessage, Message, MessageFrom};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pidge-core message`
Expected: 4 passing (the original `message_roundtrips_through_json` + 3 new).

Run: `cargo test -p pidge-core 2>&1 | tail -3`
Expected: 28 passing total (was 25; +3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-core/src/message.rs crates/pidge-core/src/lib.rs
git commit -m "Add pidge-core FullMessage and Attachment types"
```

---

## Task 3: `pidge-core::config` — trusted_senders field + methods

**Files:**
- Modify: `crates/pidge-core/src/config.rs`

- [ ] **Step 1: Add `trusted_senders` field to `Config`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-core/src/config.rs`. Find the `pub struct Config` block:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub defaults: Defaults,
}
```

Replace with:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub defaults: Defaults,
    pub trusted_senders: Vec<String>,
}
```

- [ ] **Step 2: Add three methods to `impl Config`**

Inside the `impl Config { … }` block in `config.rs`, after `find(...)`, add:

```rust
    /// Add an email to the trusted-senders list (case-insensitive). Idempotent.
    pub fn add_trusted_sender(&mut self, email: &str) {
        let lower = email.to_lowercase();
        if !self.trusted_senders.iter().any(|s| s.to_lowercase() == lower) {
            self.trusted_senders.push(email.to_string());
        }
    }

    /// Remove an email from the trusted-senders list (case-insensitive).
    /// Returns true if it was present, false if it wasn't (idempotent either way).
    pub fn remove_trusted_sender(&mut self, email: &str) -> bool {
        let lower = email.to_lowercase();
        let before = self.trusted_senders.len();
        self.trusted_senders.retain(|s| s.to_lowercase() != lower);
        before != self.trusted_senders.len()
    }

    /// Case-insensitive check for whether an email is in the trusted-senders list.
    pub fn is_sender_trusted(&self, email: &str) -> bool {
        let lower = email.to_lowercase();
        self.trusted_senders.iter().any(|s| s.to_lowercase() == lower)
    }
```

- [ ] **Step 3: Add tests**

Append to the `#[cfg(test)] mod tests` block in `config.rs`:

```rust
    #[test]
    fn add_trusted_sender_is_idempotent() {
        let mut c = Config::default();
        c.add_trusted_sender("a@b.com");
        c.add_trusted_sender("a@b.com");
        assert_eq!(c.trusted_senders.len(), 1);
    }

    #[test]
    fn add_trusted_sender_is_case_insensitive() {
        let mut c = Config::default();
        c.add_trusted_sender("Maria@MKLab.se");
        c.add_trusted_sender("maria@mklab.se");
        assert_eq!(c.trusted_senders.len(), 1);
    }

    #[test]
    fn remove_trusted_sender_returns_true_when_present() {
        let mut c = Config::default();
        c.add_trusted_sender("a@b.com");
        assert!(c.remove_trusted_sender("a@b.com"));
        assert!(c.trusted_senders.is_empty());
    }

    #[test]
    fn remove_trusted_sender_returns_false_when_absent() {
        let mut c = Config::default();
        assert!(!c.remove_trusted_sender("ghost@nowhere.com"));
    }

    #[test]
    fn remove_trusted_sender_is_case_insensitive() {
        let mut c = Config::default();
        c.add_trusted_sender("Maria@MKLab.se");
        assert!(c.remove_trusted_sender("MARIA@mklab.SE"));
        assert!(c.trusted_senders.is_empty());
    }

    #[test]
    fn is_sender_trusted_case_insensitive() {
        let mut c = Config::default();
        c.add_trusted_sender("Maria@MKLab.se");
        assert!(c.is_sender_trusted("maria@mklab.se"));
        assert!(c.is_sender_trusted("MARIA@MKLAB.SE"));
        assert!(!c.is_sender_trusted("anna@mklab.se"));
    }

    #[test]
    fn config_with_missing_trusted_senders_loads_as_empty() {
        // Old config file without trusted_senders field
        let yaml = "accounts: []\ndefaults: {}\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(c.trusted_senders.is_empty());
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pidge-core config`
Expected: 13 passing (the original 6 config tests + 7 new).

Run: `cargo test -p pidge-core 2>&1 | tail -3`
Expected: 35 passing total.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-core/src/config.rs
git commit -m "Add trusted_senders field and case-insensitive helpers to Config"
```

---

## Task 4: `pidge-client::graph::mail` — `get_message` function

**Files:**
- Modify: `crates/pidge-client/src/graph/mail.rs`

- [ ] **Step 1: Add Graph response types for FullMessage at the top of mail.rs (after the existing GraphMessage types)**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/graph/mail.rs`. After the existing `GraphList` struct, add:

```rust
#[derive(Debug, Deserialize)]
struct GraphFullMessage {
    id: String,
    subject: Option<String>,
    from: Option<GraphFromWrapper>,
    #[serde(rename = "toRecipients", default)]
    to_recipients: Vec<GraphFromWrapper>,
    #[serde(rename = "ccRecipients", default)]
    cc_recipients: Vec<GraphFromWrapper>,
    #[serde(rename = "bccRecipients", default)]
    bcc_recipients: Vec<GraphFromWrapper>,
    #[serde(rename = "receivedDateTime")]
    received_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "sentDateTime")]
    sent_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "isRead")]
    is_read: Option<bool>,
    body: GraphBody,
    #[serde(rename = "hasAttachments")]
    has_attachments: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GraphBody {
    #[serde(rename = "contentType")]
    content_type: String,
    content: String,
}
```

- [ ] **Step 2: Implement `get_message`**

Append to `mail.rs` after the existing `list_inbox` function:

```rust
/// GET /me/messages/{id} — fetch a single message with full body.
pub async fn get_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    message_id: &str,
) -> Result<pidge_core::FullMessage, ClientError> {
    let url = format!(
        "{base_url}/me/messages/{message_id}\
         ?$select=id,subject,from,toRecipients,ccRecipients,bccRecipients,\
receivedDateTime,sentDateTime,isRead,body,hasAttachments"
    );
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let g: GraphFullMessage = resp.json().await?;

    fn from(addr: GraphFromAddress) -> pidge_core::MessageFrom {
        pidge_core::MessageFrom {
            name: addr.name.unwrap_or_default(),
            address: addr.address.unwrap_or_default(),
        }
    }
    fn unwrap_recipients(rs: Vec<GraphFromWrapper>) -> Vec<pidge_core::MessageFrom> {
        rs.into_iter().map(|w| from(w.email_address)).collect()
    }

    let content_type = match g.body.content_type.to_lowercase().as_str() {
        "html" => pidge_core::BodyContentType::Html,
        _ => pidge_core::BodyContentType::Text,
    };

    Ok(pidge_core::FullMessage {
        account: account.to_string(),
        id: g.id,
        from: g
            .from
            .map(|w| from(w.email_address))
            .unwrap_or_else(|| pidge_core::MessageFrom {
                name: String::new(),
                address: String::new(),
            }),
        to: unwrap_recipients(g.to_recipients),
        cc: unwrap_recipients(g.cc_recipients),
        bcc: unwrap_recipients(g.bcc_recipients),
        subject: g.subject.unwrap_or_default(),
        received_at: g.received_date_time,
        sent_at: g.sent_date_time,
        is_read: g.is_read.unwrap_or(true),
        body_content_type: content_type,
        body_content: g.body.content,
        has_attachments: g.has_attachments.unwrap_or(false),
    })
}
```

- [ ] **Step 3: Add wiremock test for `get_message`**

Add to the `#[cfg(test)] mod tests` block at the bottom of `mail.rs`:

```rust
    #[tokio::test]
    async fn get_message_parses_graph_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "AAA",
                "subject": "Hello",
                "from": { "emailAddress": { "name": "Maria", "address": "maria@mklab.se" } },
                "toRecipients": [
                    { "emailAddress": { "name": "Kristofer", "address": "kristofer@mklab.se" } }
                ],
                "ccRecipients": [],
                "bccRecipients": [],
                "receivedDateTime": "2026-05-14T22:00:00Z",
                "sentDateTime": "2026-05-14T21:59:30Z",
                "isRead": false,
                "body": { "contentType": "html", "content": "<p>Hi</p>" },
                "hasAttachments": true
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let m = get_message(&http, &server.uri(), "AT", "u@e.com", "AAA").await.unwrap();
        assert_eq!(m.id, "AAA");
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.from.name, "Maria");
        assert_eq!(m.to.len(), 1);
        assert_eq!(m.to[0].address, "kristofer@mklab.se");
        assert!(matches!(m.body_content_type, pidge_core::BodyContentType::Html));
        assert_eq!(m.body_content, "<p>Hi</p>");
        assert!(m.has_attachments);
    }
```

You need `path_regex` from wiremock; add to the `use` line at the top of `tests`:

```rust
    use wiremock::matchers::{header, method, path, path_regex, query_param};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pidge-client mail`
Expected: 3 passing (2 existing list_inbox tests + 1 new get_message test).

- [ ] **Step 5: Commit**

```bash
git add crates/pidge-client/src/graph/mail.rs
git commit -m "Add Graph get_message returning FullMessage"
```

---

## Task 5: `pidge-client::graph::mail` — `list_attachments`, `get_attachment_bytes`, `mark_read`

**Files:**
- Modify: `crates/pidge-client/src/graph/mail.rs`
- Modify: `crates/pidge-client/Cargo.toml` (verify base64 is in deps)

- [ ] **Step 1: Confirm base64 is in pidge-client's dependencies**

Check `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/Cargo.toml`. It should already have `base64.workspace = true` in `[dependencies]` (it was added for the JWT decoder earlier). Verify; if missing, add it.

- [ ] **Step 2: Add Graph types for attachments**

Append to `mail.rs` after the `GraphBody` struct:

```rust
#[derive(Debug, Deserialize)]
struct GraphAttachmentList {
    value: Vec<GraphAttachment>,
}

#[derive(Debug, Deserialize)]
struct GraphAttachment {
    id: String,
    name: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    size: Option<u64>,
    #[serde(rename = "isInline")]
    is_inline: Option<bool>,
    #[serde(rename = "contentId")]
    content_id: Option<String>,
    #[serde(rename = "@odata.type", default)]
    odata_type: Option<String>,
    /// Only populated when fetching a single attachment (not in list endpoint).
    #[serde(rename = "contentBytes", default)]
    content_bytes: Option<String>,
}
```

- [ ] **Step 3: Implement `list_attachments`**

Append to `mail.rs`:

```rust
/// GET /me/messages/{id}/attachments — list attachments without fetching bytes.
/// Filters to file attachments only (item attachments — emails-with-emails-attached — are
/// rare and not supported here).
pub async fn list_attachments(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<pidge_core::Attachment>, ClientError> {
    let url = format!(
        "{base_url}/me/messages/{message_id}/attachments\
         ?$select=id,name,contentType,size,isInline,contentId"
    );
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let list: GraphAttachmentList = resp.json().await?;
    Ok(list
        .value
        .into_iter()
        .filter(|a| {
            a.odata_type
                .as_deref()
                .map(|t| t == "#microsoft.graph.fileAttachment")
                .unwrap_or(true) // tolerate missing @odata.type
        })
        .map(|a| pidge_core::Attachment {
            id: a.id,
            name: a.name.unwrap_or_default(),
            content_type: a.content_type.unwrap_or_default(),
            size_bytes: a.size.unwrap_or(0),
            is_inline: a.is_inline.unwrap_or(false),
            content_id: a.content_id,
        })
        .collect())
}
```

- [ ] **Step 4: Implement `get_attachment_bytes`**

Append to `mail.rs`:

```rust
/// GET /me/messages/{id}/attachments/{attachment_id} — fetch a single attachment
/// with its base64 contentBytes. Returns the decoded bytes.
pub async fn get_attachment_bytes(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>, ClientError> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let url = format!("{base_url}/me/messages/{message_id}/attachments/{attachment_id}");
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let g: GraphAttachment = resp.json().await?;
    let b64 = g.content_bytes.ok_or_else(|| ClientError::Graph {
        status: 200,
        message: "attachment response missing contentBytes".to_string(),
    })?;
    STANDARD.decode(&b64).map_err(|e| ClientError::Graph {
        status: 200,
        message: format!("attachment base64 decode: {e}"),
    })
}
```

- [ ] **Step 5: Implement `mark_read`**

Append to `mail.rs`:

```rust
/// PATCH /me/messages/{id} — mark the message as read.
pub async fn mark_read(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/messages/{message_id}");
    let resp = http
        .patch(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "isRead": true }))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    Ok(())
}
```

- [ ] **Step 6: Add wiremock tests for the new functions**

Add to the `#[cfg(test)] mod tests` block at the bottom of `mail.rs`:

```rust
    #[tokio::test]
    async fn list_attachments_filters_file_attachments() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/attachments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "@odata.type": "#microsoft.graph.fileAttachment",
                        "id": "att-1",
                        "name": "report.pdf",
                        "contentType": "application/pdf",
                        "size": 12345,
                        "isInline": false
                    },
                    {
                        "@odata.type": "#microsoft.graph.itemAttachment",
                        "id": "att-2",
                        "name": "an-email.eml",
                        "contentType": "message/rfc822",
                        "size": 7777,
                        "isInline": false
                    }
                ]
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let atts = list_attachments(&http, &server.uri(), "AT", "MSG").await.unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].name, "report.pdf");
        assert_eq!(atts[0].size_bytes, 12345);
    }

    #[tokio::test]
    async fn get_attachment_bytes_decodes_base64() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+/attachments/[A-Za-z0-9-]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "att-1",
                "name": "report.pdf",
                "contentType": "application/pdf",
                "size": 5,
                "isInline": false,
                "contentBytes": "aGVsbG8="
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let bytes = get_attachment_bytes(&http, &server.uri(), "AT", "MSG", "att-1").await.unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[tokio::test]
    async fn mark_read_patches_isread_true() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/me/messages/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        mark_read(&http, &server.uri(), "AT", "MSG").await.unwrap();
    }
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p pidge-client mail`
Expected: 6 passing (2 existing list_inbox + 1 get_message + 3 new attachment/mark tests).

- [ ] **Step 8: Commit**

```bash
git add crates/pidge-client/src/graph/mail.rs
git commit -m "Add Graph list_attachments, get_attachment_bytes, and mark_read"
```

---

## Task 6: `pidge-client::GraphClient` — wrapper methods

**Files:**
- Modify: `crates/pidge-client/src/graph/mod.rs`

- [ ] **Step 1: Add four methods to `GraphClient`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge-client/src/graph/mod.rs`. Inside the `impl GraphClient { … }` block, after the existing `list_inbox` method, add:

```rust
    /// GET /me/messages/{id} for a given account email.
    pub async fn get_message(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<pidge_core::FullMessage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::get_message(&self.http, &self.base_url, &token, account, message_id).await
    }

    /// GET /me/messages/{id}/attachments.
    pub async fn list_attachments(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<Vec<pidge_core::Attachment>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::list_attachments(&self.http, &self.base_url, &token, message_id).await
    }

    /// GET /me/messages/{id}/attachments/{att_id} returning decoded bytes.
    pub async fn get_attachment_bytes(
        &self,
        account: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::get_attachment_bytes(&self.http, &self.base_url, &token, message_id, attachment_id).await
    }

    /// PATCH /me/messages/{id} with isRead: true.
    pub async fn mark_read(&self, account: &str, message_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::mark_read(&self.http, &self.base_url, &token, message_id).await
    }
```

The `list_attachments` re-export from `mod.rs` already exists in the `pub use mail::list_inbox;` line. If the `mail` module is private to graph/, that's fine; we're using `mail::function_name` inside the same module.

- [ ] **Step 2: Build**

Run: `cargo build -p pidge-client 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 3: Run tests**

Run: `cargo test -p pidge-client 2>&1 | tail -3`
Expected: ~26 passing (22 baseline + 4 new mail tests from Tasks 4-5).

- [ ] **Step 4: Commit**

```bash
git add crates/pidge-client/src/graph/mod.rs
git commit -m "Add GraphClient wrapper methods for get_message, list_attachments, get_attachment_bytes, mark_read"
```

---

## Task 7: CLI definitions — `Show` and `Trust`

**Files:**
- Modify: `crates/pidge/src/cli.rs`
- Modify: `crates/pidge/src/commands/mod.rs`
- Modify: `crates/pidge/src/commands/inbox.rs` (dispatcher extension)

- [ ] **Step 1: Extend `InboxCommands` enum with `Show`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/cli.rs`. Find the `InboxCommands::List` variant. After it (inside the same enum), add:

```rust
    /// Display a single message identified by a fragment of its short hash
    Show {
        /// Fragment of the 8-char short hash (prefix, suffix, or substring)
        fragment: String,

        /// Also mark the message as read on the server
        #[arg(short = 'r', long)]
        mark_read: bool,

        /// Force inline image rendering for this invocation, regardless of trust list
        #[arg(long)]
        show_images: bool,
    },
```

- [ ] **Step 2: Add `Commands::Trust` variant and `TrustCommands` enum**

Still in `cli.rs`. In the `Commands` enum, after the existing `Inbox { command }` variant, add:

```rust
    /// Manage the trusted-senders list (auto-renders inline images from these senders)
    Trust {
        #[command(subcommand)]
        command: TrustCommands,
    },
```

At the bottom of `cli.rs` (next to other subcommand enums), add:

```rust
#[derive(clap::Subcommand)]
pub enum TrustCommands {
    /// List trusted sender addresses
    List,
    /// Add an email address to the trust list (idempotent)
    Add {
        /// Email address to add
        email: String,
    },
    /// Remove an email address from the trust list (idempotent)
    Remove {
        /// Email address to remove
        email: String,
    },
}
```

- [ ] **Step 3: Update `Cli::run` to dispatch the new commands**

Still in `cli.rs`, find the `match self.command { … }` block in `impl Cli`. Add a new arm after the `Inbox` arm:

```rust
            Some(Commands::Trust { command }) => {
                crate::commands::trust::run(command, self.json).await
            }
```

The existing `Inbox` arm already calls `crate::commands::inbox::run(command, self.json).await` — no change needed there. The inbox.rs dispatcher will handle the new `Show` variant in Task 8.

- [ ] **Step 4: Declare new modules in `commands/mod.rs`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/mod.rs`. Add `pub mod inbox_show;` and `pub mod trust;` alphabetically. The file becomes:

```rust
//! CLI command implementations

pub mod ai;
pub mod auth;
pub mod auth_default;
pub mod auth_list;
pub mod auth_login;
pub mod auth_logout;
pub mod auth_status;
pub mod completion;
pub mod inbox;
pub mod inbox_show;
pub mod skill;
pub mod trust;
```

- [ ] **Step 5: Create placeholder `commands/inbox_show.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/inbox_show.rs`:

```rust
//! `pidge inbox show` — display a single message.

use anyhow::Result;

#[allow(dead_code)]
pub async fn run(
    _fragment: String,
    _mark_read: bool,
    _show_images: bool,
    _json: bool,
) -> Result<()> {
    unimplemented!("inbox show is implemented in Task 9")
}
```

- [ ] **Step 6: Create placeholder `commands/trust.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/trust.rs`:

```rust
//! `pidge trust ...` — manage the trusted-senders list.

use anyhow::Result;

use crate::cli::TrustCommands;

#[allow(dead_code)]
pub async fn run(_command: TrustCommands, _json: bool) -> Result<()> {
    unimplemented!("trust is implemented in Task 10")
}
```

- [ ] **Step 7: Extend `inbox.rs` dispatcher to route `Show`**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/inbox.rs`. Find the `pub async fn run(command: InboxCommands, json: bool) -> Result<()>` function. Replace its body to handle both variants:

```rust
pub async fn run(command: InboxCommands, json: bool) -> Result<()> {
    match command {
        InboxCommands::List {
            account,
            limit,
            unread,
            compact,
        } => list(account, limit, unread, compact, json).await,
        InboxCommands::Show {
            fragment,
            mark_read,
            show_images,
        } => crate::commands::inbox_show::run(fragment, mark_read, show_images, json).await,
    }
}
```

- [ ] **Step 8: Build and smoke-test clap parsing**

Run: `cargo build --workspace 2>&1 | tail -3`
Expected: clean.

Run: `cargo run -q -- inbox --help`
Expected: lists both `list` and `show` subcommands.

Run: `cargo run -q -- inbox show --help`
Expected: shows positional `<FRAGMENT>` and `-r, --mark-read`, `--show-images` flags.

Run: `cargo run -q -- trust --help`
Expected: lists `list`, `add`, `remove` subcommands.

Run: `cargo run -q -- trust list 2>&1`
Expected: panic with the `unimplemented!()` message — that's intentional. The implementations land in Tasks 9 and 10. Don't worry about this output yet.

Actually skip running `trust list` — clippy with `-D warnings` may not let `unimplemented!()` slide through dead-code checks in certain configurations. Verify build only.

- [ ] **Step 9: Commit**

```bash
git add crates/pidge/src/cli.rs crates/pidge/src/commands/mod.rs crates/pidge/src/commands/inbox.rs crates/pidge/src/commands/inbox_show.rs crates/pidge/src/commands/trust.rs
git commit -m "Add pidge inbox show + trust CLI definitions with placeholder modules"
```

---

## Task 8: `trust.rs` implementation

**Files:**
- Modify: `crates/pidge/src/commands/trust.rs`

- [ ] **Step 1: Replace `trust.rs` with the real implementation**

Overwrite `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/trust.rs` with:

```rust
//! `pidge trust ...` — manage the trusted-senders list.

use anyhow::Result;
use colored::Colorize;

use pidge_core::Config;

use crate::cli::TrustCommands;

pub async fn run(command: TrustCommands, json: bool) -> Result<()> {
    let mut config = Config::load()?;
    match command {
        TrustCommands::List => render_list(&config, json),
        TrustCommands::Add { email } => {
            config.add_trusted_sender(&email);
            config.save()?;
            if !json {
                println!("{} Added {email} to trust list.", "✔".green());
            }
            Ok(())
        }
        TrustCommands::Remove { email } => {
            let removed = config.remove_trusted_sender(&email);
            config.save()?;
            if !json {
                if removed {
                    println!("{} Removed {email} from trust list.", "✔".green());
                } else {
                    println!("{email} was not in the trust list.");
                }
            }
            Ok(())
        }
    }
}

fn render_list(config: &Config, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&config.trusted_senders)?
        );
        return Ok(());
    }
    if config.trusted_senders.is_empty() {
        println!("No trusted senders. Use `pidge trust add <email>` to add one.");
        return Ok(());
    }
    for s in &config.trusted_senders {
        println!("{s}");
    }
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p pidge 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 3: Smoke-test**

```bash
cargo run -q -- trust list
```
Expected: `No trusted senders. Use \`pidge trust add <email>\` to add one.`

```bash
cargo run -q -- trust list --json
```
Expected: `[]`

```bash
cargo run -q -- trust add test@example.com
```
Expected: `✔ Added test@example.com to trust list.`

```bash
cargo run -q -- trust list
```
Expected: `test@example.com`

```bash
cargo run -q -- trust remove test@example.com
```
Expected: `✔ Removed test@example.com from trust list.`

```bash
cargo run -q -- trust remove ghost@nowhere.com
```
Expected: `ghost@nowhere.com was not in the trust list.`

- [ ] **Step 4: Commit**

```bash
git add crates/pidge/src/commands/trust.rs
git commit -m "Implement pidge trust list/add/remove"
```

---

## Task 9: `inbox_show.rs` implementation — first pass (text/json without images)

**Files:**
- Modify: `crates/pidge/Cargo.toml`
- Modify: `crates/pidge/src/commands/inbox_show.rs`

Implement the core `inbox show` flow: cache lookup, Graph fetch, attachment list, text rendering (header + body), JSON rendering, mark-read. Inline image rendering is added in Task 10 because it has additional dependencies and complexity.

- [ ] **Step 1: Add html2text and humansize to pidge dependencies**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/Cargo.toml`. In `[dependencies]`, add:

```toml
html2text.workspace = true
humansize.workspace = true
```

- [ ] **Step 2: Replace `inbox_show.rs` with the implementation**

Overwrite `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/inbox_show.rs` with:

```rust
//! `pidge inbox show <fragment>` — display a single message with full body.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{
    Attachment, BodyContentType, CacheLookup, Config, FullMessage, MessageCache, MessageFrom,
};

use crate::output::linkify_text;

pub async fn run(
    fragment: String,
    mark_read: bool,
    show_images: bool,
    json: bool,
) -> Result<()> {
    let _ = show_images; // wired up in Task 10
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge auth login` to add one."
        ));
    }

    // Look up the fragment in the cache.
    let cache = MessageCache::load()?;
    let (short_hash, message_ref) = match cache.find_by_fragment(&fragment) {
        CacheLookup::NotFound => {
            return Err(anyhow!(
                "No message found for fragment '{fragment}'. Run `pidge inbox list` to refresh the cache."
            ));
        }
        CacheLookup::Ambiguous(matches) => {
            print_ambiguous(&matches);
            return Err(anyhow!("Please provide more characters."));
        }
        CacheLookup::One(h, r) => (h, r),
    };

    // Fetch full message from Graph.
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let full = match graph.get_message(&message_ref.account, &message_ref.graph_id).await {
        Ok(m) => m,
        Err(ClientError::Graph { status: 404, .. }) => {
            // Purge stale cache entry.
            purge_from_cache(&short_hash)?;
            return Err(anyhow!(
                "Message not found on server. It may have been deleted. Run `pidge inbox list` to refresh."
            ));
        }
        Err(e) => return Err(e.into()),
    };

    // Fetch attachments if the message has any. Non-fatal if it fails.
    let attachments: Vec<Attachment> = if full.has_attachments {
        match graph.list_attachments(&message_ref.account, &full.id).await {
            Ok(atts) => atts,
            Err(e) => {
                eprintln!(
                    "{} could not list attachments: {e}",
                    "WARNING:".yellow().bold()
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // Render.
    if json {
        render_json(&short_hash, &full, &attachments)?;
    } else {
        render_text(&full, &attachments)?;
    }

    // Optional: mark as read.
    if mark_read {
        if let Err(e) = graph.mark_read(&message_ref.account, &full.id).await {
            eprintln!(
                "{} could not mark message as read: {e}",
                "WARNING:".yellow().bold()
            );
        }
    }

    Ok(())
}

fn purge_from_cache(short_hash: &str) -> Result<()> {
    let mut cache = MessageCache::load()?;
    cache.entries.remove(short_hash);
    cache.save()?;
    Ok(())
}

fn print_ambiguous(matches: &[(String, pidge_core::CachedMessageRef)]) {
    println!("Fragment matches multiple messages:");
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["ID", "ACCOUNT", "GRAPH ID"]);
    for (hash, r) in matches {
        table.add_row(vec![
            hash.dimmed().to_string(),
            r.account.clone(),
            r.graph_id.chars().take(20).collect::<String>() + "…",
        ]);
    }
    println!("{table}");
}

fn render_text(full: &FullMessage, attachments: &[Attachment]) -> Result<()> {
    // Header block
    println!("{}      {}", "From:".bold(), format_recipient(&full.from));
    if !full.to.is_empty() {
        println!("{}        {}", "To:".bold(), format_recipient_list(&full.to));
    }
    if !full.cc.is_empty() {
        println!("{}        {}", "Cc:".bold(), format_recipient_list(&full.cc));
    }
    println!("{}   {}", "Subject:".bold(), full.subject);
    println!(
        "{}  {} ({})",
        "Received:".bold(),
        format_local_datetime(full.received_at),
        relative_time(full.received_at),
    );
    println!();
    println!("{}", separator());
    println!();

    // Body
    let body_text = render_body(full);
    let body_linkified = linkify_text(&body_text);
    println!("{}", body_linkified);

    // Attachment list (non-inline only)
    let visible_attachments: Vec<&Attachment> = attachments.iter().filter(|a| !a.is_inline).collect();
    if !visible_attachments.is_empty() {
        println!();
        println!("{}", separator());
        println!();
        println!("{}", "Attachments:".bold());
        let mut table = Table::new();
        table.load_preset(comfy_table::presets::NOTHING);
        table.set_content_arrangement(ContentArrangement::Dynamic);
        for att in visible_attachments {
            table.add_row(vec![
                format!("  {}", att.name),
                humansize::format_size(att.size_bytes, humansize::DECIMAL),
            ]);
        }
        println!("{table}");
    }

    Ok(())
}

fn render_body(full: &FullMessage) -> String {
    match full.body_content_type {
        BodyContentType::Text => full.body_content.clone(),
        BodyContentType::Html => {
            // html2text auto-wraps to 80 chars by default; pass terminal width if narrower.
            let width = terminal_width().min(100);
            html2text::from_read(full.body_content.as_bytes(), width)
                .unwrap_or_else(|_| full.body_content.clone())
        }
    }
}

fn terminal_width() -> usize {
    use std::process::Command;
    if let Ok(out) = Command::new("tput").arg("cols").output() {
        if let Ok(s) = std::str::from_utf8(&out.stdout) {
            if let Ok(n) = s.trim().parse::<usize>() {
                return n;
            }
        }
    }
    80
}

fn separator() -> String {
    "─".repeat(60).dimmed().to_string()
}

fn format_recipient(r: &MessageFrom) -> String {
    if r.name.is_empty() {
        r.address.clone()
    } else {
        format!("{} <{}>", r.name, r.address)
    }
}

fn format_recipient_list(rs: &[MessageFrom]) -> String {
    rs.iter()
        .map(format_recipient)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_local_datetime(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn relative_time(then: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now - then;
    if delta.num_minutes() < 60 {
        format!("{}m ago", delta.num_minutes().max(0))
    } else if delta.num_hours() < 24 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_days() < 7 {
        format!("{}d ago", delta.num_days())
    } else {
        format_local_datetime(then)
    }
}

#[derive(Serialize)]
struct ShowOut<'a> {
    id: &'a str,
    graph_id: &'a str,
    account: &'a str,
    from: &'a MessageFrom,
    to: &'a [MessageFrom],
    cc: &'a [MessageFrom],
    bcc: &'a [MessageFrom],
    subject: &'a str,
    received_at: DateTime<Utc>,
    sent_at: DateTime<Utc>,
    is_read: bool,
    body: BodyOut<'a>,
    has_attachments: bool,
    attachments: &'a [Attachment],
}

#[derive(Serialize)]
struct BodyOut<'a> {
    content_type: BodyContentType,
    html: Option<&'a str>,
    text: String,
}

fn render_json(
    short_hash: &str,
    full: &FullMessage,
    attachments: &[Attachment],
) -> Result<()> {
    let body_text = render_body(full);
    let body = BodyOut {
        content_type: full.body_content_type,
        html: if matches!(full.body_content_type, BodyContentType::Html) {
            Some(full.body_content.as_str())
        } else {
            None
        },
        text: body_text,
    };
    let out = ShowOut {
        id: short_hash,
        graph_id: &full.id,
        account: &full.account,
        from: &full.from,
        to: &full.to,
        cc: &full.cc,
        bcc: &full.bcc,
        subject: &full.subject,
        received_at: full.received_at,
        sent_at: full.sent_at,
        is_read: full.is_read,
        body,
        has_attachments: full.has_attachments,
        attachments,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
```

- [ ] **Step 3: Build, format, lint**

Run: `cargo build -p pidge 2>&1 | tail -3`
Expected: clean.

Run: `cargo fmt --all`
Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -3`
Expected: clean.

If clippy complains about the unused `show_images` parameter, the `let _ = show_images;` line should silence it. If it doesn't, change the parameter name to `_show_images` and remove the `let _ =` line.

- [ ] **Step 4: Smoke-test**

You need a live message in the cache to fully test. The plan can't reproduce that perfectly. Verify these failure paths instead:

```bash
cargo run -q -- inbox show xxxxxxxx 2>&1; echo "exit=$?"
```
Expected: `Error: No message found for fragment 'xxxxxxxx'. Run \`pidge inbox list\` to refresh the cache.` exit 1.

```bash
cargo run -q -- inbox show "" 2>&1; echo "exit=$?"
```
Expected: same error (empty fragment is NotFound). Exit 1.

If a live account is available and `pidge inbox list` has populated the cache, try with a real fragment:

```bash
cargo run -q -- inbox show <some-real-fragment>
```
Expected: header block, body, possibly attachment list, no panic.

```bash
cargo run -q -- inbox show <some-real-fragment> --json
```
Expected: valid JSON with id, graph_id, account, from, to, cc, bcc, subject, received_at, sent_at, is_read, body, has_attachments, attachments.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge/Cargo.toml crates/pidge/src/commands/inbox_show.rs Cargo.lock
git commit -m "Implement pidge inbox show with text/json rendering"
```

---

## Task 10: Inline image rendering for trusted senders

**Files:**
- Modify: `crates/pidge/Cargo.toml`
- Modify: `crates/pidge/src/commands/inbox_show.rs`

Adds `viuer` + `image` deps and renders inline image attachments when the sender is trusted or `--show-images` is passed.

- [ ] **Step 1: Add viuer and image to pidge dependencies**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/Cargo.toml`. In `[dependencies]`, add:

```toml
viuer.workspace = true
image.workspace = true
```

- [ ] **Step 2: Replace the `let _ = show_images;` stub and wire up image rendering**

Open `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/inbox_show.rs`. Remove the line:

```rust
    let _ = show_images; // wired up in Task 10
```

Then, in the `run` function, AFTER `render_text` (or before, but specifically AFTER the body has been printed and BEFORE the attachment list when in text mode), wire in inline image rendering. The cleanest place is to thread `show_images` and the trust flag through `render_text`.

Replace the `render_text` call site in `run` with:

```rust
    // Decide whether to render inline images.
    let is_trusted = config.is_sender_trusted(&full.from.address);
    let render_inline_images = is_trusted || show_images;

    if json {
        render_json(&short_hash, &full, &attachments)?;
    } else {
        render_text(&full, &attachments)?;
        if render_inline_images {
            render_inline_images_block(&graph, &message_ref.account, &full, &attachments).await;
        }
    }
```

Wait — the existing `render_text` already prints the attachment list at the bottom. We want the inline images block to appear BEFORE the regular attachments list. Restructure: split `render_text` into `render_header_and_body` and `render_attachments`, and call them with the inline-images block in between.

Update `render_text` and add new functions. Replace the existing `render_text` function with these:

```rust
fn render_header_and_body(full: &FullMessage) -> Result<()> {
    // Header block
    println!("{}      {}", "From:".bold(), format_recipient(&full.from));
    if !full.to.is_empty() {
        println!("{}        {}", "To:".bold(), format_recipient_list(&full.to));
    }
    if !full.cc.is_empty() {
        println!("{}        {}", "Cc:".bold(), format_recipient_list(&full.cc));
    }
    println!("{}   {}", "Subject:".bold(), full.subject);
    println!(
        "{}  {} ({})",
        "Received:".bold(),
        format_local_datetime(full.received_at),
        relative_time(full.received_at),
    );
    println!();
    println!("{}", separator());
    println!();

    let body_text = render_body(full);
    let body_linkified = linkify_text(&body_text);
    println!("{}", body_linkified);
    Ok(())
}

fn render_attachments_block(attachments: &[Attachment]) -> Result<()> {
    let visible_attachments: Vec<&Attachment> = attachments.iter().filter(|a| !a.is_inline).collect();
    if visible_attachments.is_empty() {
        return Ok(());
    }
    println!();
    println!("{}", separator());
    println!();
    println!("{}", "Attachments:".bold());
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    for att in visible_attachments {
        table.add_row(vec![
            format!("  {}", att.name),
            humansize::format_size(att.size_bytes, humansize::DECIMAL),
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn render_inline_images_block(
    graph: &GraphClient,
    account: &str,
    full: &FullMessage,
    attachments: &[Attachment],
) {
    let inline_images: Vec<&Attachment> = attachments
        .iter()
        .filter(|a| a.is_inline && is_image_content_type(&a.content_type))
        .collect();
    if inline_images.is_empty() {
        return;
    }
    println!();
    println!("{}", separator());
    println!();
    println!("{}", "Inline images:".bold());

    for att in inline_images {
        match graph.get_attachment_bytes(account, &full.id, &att.id).await {
            Ok(bytes) => {
                if !try_render_image(&bytes) {
                    println!(
                        "  [image: {} ({})] (terminal does not support inline images)",
                        att.name,
                        humansize::format_size(att.size_bytes, humansize::DECIMAL)
                    );
                }
            }
            Err(e) => {
                eprintln!("  [image: {} — fetch failed: {e}]", att.name);
            }
        }
    }
}

fn try_render_image(bytes: &[u8]) -> bool {
    let img = match image::load_from_memory(bytes) {
        Ok(i) => i,
        Err(_) => return false,
    };
    let conf = viuer::Config {
        absolute_offset: false,
        width: Some(60),
        ..Default::default()
    };
    viuer::print(&img, &conf).is_ok()
}

fn is_image_content_type(ct: &str) -> bool {
    let ct = ct.to_lowercase();
    matches!(
        ct.as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp" | "image/gif"
    )
}
```

Then in `run`, replace the `render_text(&full, &attachments)?;` line with:

```rust
        render_header_and_body(&full)?;
        if render_inline_images {
            render_inline_images_block(&graph, &message_ref.account, &full, &attachments).await;
        }
        render_attachments_block(&attachments)?;
```

The existing standalone `render_text` function is no longer needed — DELETE IT.

- [ ] **Step 3: Add unit test for `is_image_content_type`**

At the bottom of `inbox_show.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_content_type_recognizes_common_image_types() {
        assert!(is_image_content_type("image/png"));
        assert!(is_image_content_type("image/jpeg"));
        assert!(is_image_content_type("image/jpg"));
        assert!(is_image_content_type("image/webp"));
        assert!(is_image_content_type("image/gif"));
        assert!(is_image_content_type("IMAGE/PNG"));
    }

    #[test]
    fn is_image_content_type_rejects_non_image_types() {
        assert!(!is_image_content_type("application/pdf"));
        assert!(!is_image_content_type("image/svg+xml"));
        assert!(!is_image_content_type("text/html"));
        assert!(!is_image_content_type(""));
    }
}
```

(We exclude `image/svg+xml` because `image` crate doesn't decode SVG; we want it to fall back to the placeholder text.)

- [ ] **Step 4: Build, format, lint**

Run: `cargo build -p pidge 2>&1 | tail -3`
Expected: clean. The first build will be slow due to `image` and `viuer` compiling.

Run: `cargo fmt --all`
Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: ~70 passing (35 pidge-core + 26 pidge-client + ~10 pidge incl. 2 new is_image tests).

- [ ] **Step 6: Smoke-test trust + show together (if you have a live account and a known sender)**

```bash
# Add a sender to the trust list
cargo run -q -- trust add some-sender@example.com

# Show a message from them
cargo run -q -- inbox show <fragment>
# Expected: header, body, inline-images block (if Ghostty/Kitty terminal), attachment list

# Force-show images for one message regardless of trust
cargo run -q -- inbox show <fragment> --show-images

# Verify mark-read works
cargo run -q -- inbox show <fragment> --mark-read
# Re-run pidge inbox list; that message should no longer have the bold styling.
```

If no live account is available, only verify the build and unit tests.

- [ ] **Step 7: Commit**

```bash
git add crates/pidge/Cargo.toml crates/pidge/src/commands/inbox_show.rs Cargo.lock
git commit -m "Render inline images for trusted senders via viuer"
```

---

## Task 11: CHANGELOG entries

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Append to `[Unreleased] ### Added`**

Open `/Users/kristofer/repos/mklab-se/pidge/CHANGELOG.md`. Under `## [Unreleased]` `### Added`, append:

```markdown
- `pidge inbox show <fragment>` — substring-lookup a message by its 8-char short hash and display headers, body, and attachment list
- `pidge inbox show --mark-read` / `-r` to mark the message as read on the server after rendering
- `pidge inbox show --show-images` to force inline image rendering for one invocation
- `pidge trust list/add/remove` — manage the trusted-senders list; inline images auto-render for trusted senders in image-capable terminals (Ghostty, Kitty, iTerm2)
- Trusted senders stored at `trusted_senders:` in `~/.config/pidge/config.yaml` (case-insensitive matching)
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "Update CHANGELOG for pidge inbox show and trust feature"
```

---

## Task 12: Final verification

**Files:** none modified.

- [ ] **Step 1: Run full CI suite**

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

Expected: every command exits 0. Test count should be approximately:
- 35 pidge-core (28 + 7 trust tests)
- ~26 pidge-client (22 + 4 new mail tests)
- ~12 pidge (10 + 2 is_image tests)
Total: ~73 passing.

- [ ] **Step 2: Smoke-test the full surface**

```bash
cargo run -q -- inbox show --help
cargo run -q -- trust --help
cargo run -q -- trust list
cargo run -q -- trust list --json
cargo run -q -- inbox show xxxxxxxx 2>&1; echo "exit=$?"
```

Expected outputs:
- `inbox show --help` shows positional `<FRAGMENT>` and `--mark-read`, `--show-images` flags
- `trust --help` shows `list`, `add`, `remove` subcommands
- `trust list` shows trusted senders or empty message
- `trust list --json` returns valid JSON array
- `inbox show xxxxxxxx` errors with "No message found for fragment 'xxxxxxxx'" exit 1

- [ ] **Step 3: Confirm clean working tree**

Run: `git status`
Expected: clean.

Run: `git log --oneline 332e452..HEAD | wc -l`
Expected: ~11 new commits (one per Task 1-11).

- [ ] **Step 4: Hand-off summary**

Summarize for the user:
1. Commits landed
2. End-to-end behavior: `pidge inbox list` → pick a hash → `pidge inbox show <fragment>` to read it
3. Trust flow: `pidge trust add <email>` for senders whose inline images should auto-render
4. Deferred: `pidge inbox attachment download` is the next-up feature

---

## Plan self-review

**Spec coverage check:**

| Spec section | Task(s) |
|---|---|
| Workspace deps (html2text, viuer, image, humansize) | Task 1 |
| pidge-core FullMessage + Attachment + BodyContentType | Task 2 |
| pidge-core Config.trusted_senders + 3 helper methods | Task 3 |
| pidge-client::graph::mail::get_message | Task 4 |
| pidge-client::graph::mail::list_attachments + get_attachment_bytes + mark_read | Task 5 |
| GraphClient wrapper methods | Task 6 |
| InboxCommands::Show + Commands::Trust + TrustCommands | Task 7 |
| pidge trust list/add/remove implementation | Task 8 |
| pidge inbox show — cache lookup, Graph fetch, header/body/attachments rendering, JSON output | Task 9 |
| Inline image rendering for trusted senders / --show-images | Task 10 |
| Cache invalidation on 404 | Task 9 (`purge_from_cache`) |
| --mark-read flag | Task 9 (mark_read call after rendering) |
| CHANGELOG | Task 11 |
| Final verification | Task 12 |

**Placeholder scan:** No "TBD", "TODO", "implement later" placeholders. Every code block is complete. The intentional placeholders in `inbox_show.rs` (Task 7) are `unimplemented!()` macros that get replaced in Task 9; this is consistent with the foundation pattern.

**Type consistency:**
- `FullMessage`, `Attachment`, `BodyContentType` — defined in Task 2 (pidge-core), used in Task 4 (mail.rs mapper), Task 6 (GraphClient methods), Task 9 (inbox_show).
- `Config::{add_trusted_sender, remove_trusted_sender, is_sender_trusted}` — defined Task 3, used in Task 8 (trust.rs) and Task 10 (inbox_show.rs trust check).
- `CacheLookup` — pre-existing pidge-core type, used in Task 9.
- `GraphClient::{get_message, list_attachments, get_attachment_bytes, mark_read}` — defined Task 6, used in Task 9 and Task 10.
- `MessageFrom` — pre-existing pidge-core type, used throughout for sender/recipient rendering.
- `TrustCommands::{List, Add, Remove}` — defined Task 7, dispatched Task 8.
- `InboxCommands::Show { fragment, mark_read, show_images }` — defined Task 7, destructured in Task 7 (inbox.rs dispatch) and Task 9 (inbox_show::run signature).
- `ShowOut<'a>`, `BodyOut<'a>` — local to inbox_show.rs, defined Task 9, lifetime-borrowed for zero-clone JSON serialization.

**Notable risks:**
- `html2text 0.12` API signature: `html2text::from_read(reader, width) -> Result<String, _>`. If the actual API differs, the Task 9 step that calls it will need adjustment. Verified against html2text 0.12.6 docs.
- `viuer::Config` field names: `absolute_offset: bool`, `width: Option<u32>`. Verified against viuer 0.9 docs.
- `image::load_from_memory` returns `Result<DynamicImage, ImageError>`. Used unchanged.
- The `mail` module's `path_regex` matcher requires the `wiremock` dev-dep already added in earlier batches.
