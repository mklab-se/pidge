# `pidge inbox show` + trusted-senders — design

**Status:** Approved 2026-05-14
**Goal:** Implement `pidge inbox show <fragment>` — substring-lookup a message by its 8-char short hash, fetch full content from Microsoft Graph, and render headers + body + attachments cleanly in the terminal. Add a small trusted-senders feature: per-account list of email addresses whose inline images pidge will auto-display (using the terminal's image protocol when available, e.g. Ghostty/Kitty/iTerm2). Adds `pidge trust list/add/remove` as a sibling top-level subcommand.

## Non-goals

- `pidge inbox attachment download <fragment> <name>` — its own follow-up feature; the `show` command exposes attachment names and sizes but does not save them.
- Domain wildcards in the trust list (e.g., `*@github.com`). Exact email-address match only in v1.
- Inline-in-original-position image rendering (would require a real HTML renderer). Trusted-sender images are rendered AT THE END of the body in attachment order.
- Markdown rendering. Email body parsing uses `html2text` (HTML → plain text with light list/heading/blockquote formatting); markdown stays deferred until a command genuinely produces markdown content.
- Blocking remote stylesheets, tracking pixels, or other remote content beyond images. Graph doesn't typically surface these for the message endpoint anyway.

## What changes for the user

### `pidge inbox show <fragment>`

Example:

```
$ pidge inbox show 7a3

From:      Maria Lindberg <maria@mklab.se>
To:        kristofer@mklab.se
Cc:        team@mklab.se, anna@mklab.se
Subject:   Quarterly numbers ready for review
Received:  2026-05-14 22:00 (5m ago)

────────────────────────────────────────────────────────────

Hi Kristofer,

Attached are the Q1 numbers for review. Let me know if you have any
questions about the revenue split between accounts.

The big change this quarter is the +18% from EU contracts. Have a look
at the breakdown in the second sheet of q1-numbers.xlsx.

— Maria

────────────────────────────────────────────────────────────

Attachments:
  q1-numbers.xlsx       24.3 KB
  revenue-breakdown.pdf  1.2 MB
```

For trusted senders, after the body and before the attachment list:

```
Inline images:
  [terminal renders image 1 here]
  [terminal renders image 2 here]
```

If the terminal doesn't support inline images (Apple Terminal, basic xterm), the inline-image block prints a placeholder line per image:

```
Inline images:
  [image: signature.png, 4.2 KB] (terminal does not support inline images)
  [image: logo.png, 12.1 KB]    (terminal does not support inline images)
```

### Flags on `pidge inbox show`

- `--mark-read` / `-r` — also PATCH `isRead: true` on the server after rendering. Without this, viewing a message in pidge doesn't change its read state in Outlook on your phone.
- `--show-images` — force inline-image rendering for this one invocation regardless of trust list. Useful for one-off "I trust this sender just this once."
- `--json` (global) — emit JSON instead of formatted text. JSON output never embeds image bytes (would balloon the output) — see "JSON output shape" below.

### `pidge trust`

```
pidge trust list                       # show trusted sender addresses
pidge trust add <email>                # add to trust list
pidge trust remove <email>             # remove from trust list (idempotent)
```

`pidge trust list` honors the global `--json` flag and emits a `Vec<String>` of email addresses when set.

`pidge trust add` is idempotent: adding the same email twice is a no-op (no error, just no-change).

`pidge trust remove` for an unknown email is also idempotent.

### JSON output shape (`pidge inbox show --json`)

```json
{
  "id": "7a3b9c2f",
  "graph_id": "AAMkAGI2T...",
  "account": "kristofer@mklab.se",
  "from": { "name": "Maria Lindberg", "address": "maria@mklab.se" },
  "to": [{ "name": "Kristofer", "address": "kristofer@mklab.se" }],
  "cc": [],
  "bcc": [],
  "subject": "Quarterly numbers ready for review",
  "received_at": "2026-05-14T22:00:00Z",
  "sent_at": "2026-05-14T21:59:30Z",
  "is_read": false,
  "body": {
    "content_type": "html",
    "html": "<html>…</html>",
    "text": "Plain-text rendering used for display"
  },
  "has_attachments": true,
  "attachments": [
    { "name": "q1-numbers.xlsx", "content_type": "application/vnd.openxmlformats-…", "size_bytes": 24576, "is_inline": false, "content_id": null },
    { "name": "logo.png", "content_type": "image/png", "size_bytes": 4200, "is_inline": true, "content_id": "logo@mklab.se" }
  ]
}
```

Notes on the JSON shape:
- `body.html` is the raw Graph body content when `content_type == "html"`; for `text` bodies it is `null` and `text` carries the content.
- `body.text` is always present — either the raw plain-text body or the result of running `html2text` over the HTML.
- `attachments[].content_id` is the `contentId` Microsoft Graph reports for inline attachments (used to match `<img src="cid:...">` references in the HTML). `null` for non-inline attachments.
- `bcc` is included for completeness but will almost always be empty on incoming messages (BCC is stripped before delivery).
- `--show-images` and `--mark-read` flags do not change the JSON shape; they only affect text output and server side-effects.

### Error handling

| Condition | Text UX | Exit |
|---|---|---|
| Fragment matches no cache entry | `No message found for fragment '<x>'. Run \`pidge inbox list\` to refresh the cache.` | 1 |
| Fragment matches multiple cache entries | Mini-table listing each match with short hash + subject + from + account, followed by `Please provide more characters.` | 1 |
| Graph 404 (message deleted between cache + fetch) | `Message not found on server. It may have been deleted.` The matching cache entry is purged. Suggestion to `pidge inbox list`. | 1 |
| Graph 401 / refresh fails | Existing `ClientError::SessionExpired` flow; surfaced as `Session expired for <email>. Run \`pidge auth login\`.` | 3 |
| `--mark-read` set, PATCH fails | Print body normally (rendering already happened), then `eprintln!` a `WARNING:` about the mark-read failure. Don't fail the whole command. | 0 |
| `--show-images` on a non-image-capable terminal | Same fallback as untrusted: per-image placeholder lines. No error. | 0 |
| Trusted sender, attachments fetch fails | Render body normally; eprintln warning that inline images couldn't be loaded. | 0 |

## Architecture

### New types (`pidge-core`)

**`crates/pidge-core/src/message.rs`** — extend the existing module with full-message types:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullMessage {
    pub account: String,
    pub id: String,                              // Graph message ID
    pub from: MessageFrom,
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    pub bcc: Vec<MessageFrom>,
    pub subject: String,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub sent_at: chrono::DateTime<chrono::Utc>,
    pub is_read: bool,
    pub body_content_type: BodyContentType,
    pub body_content: String,                    // raw body — HTML or plain text per content_type
    pub has_attachments: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyContentType {
    Text,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub is_inline: bool,
    pub content_id: Option<String>,
}
```

Pidge-core remains provider-agnostic — these are normalized shapes the Graph mapper produces.

### `pidge-core::config` — trusted senders

Extend `Config`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub defaults: Defaults,
    pub trusted_senders: Vec<String>,
}

impl Config {
    /// Add an email to the trusted senders list. Idempotent.
    pub fn add_trusted_sender(&mut self, email: &str);

    /// Remove an email. Returns true if it was present. Idempotent (false return for unknown).
    pub fn remove_trusted_sender(&mut self, email: &str) -> bool;

    /// Case-insensitive comparison.
    pub fn is_sender_trusted(&self, email: &str) -> bool;
}
```

Email comparison is case-insensitive (Microsoft Graph normalizes addresses but users can still type `Maria@MKLab.se` vs `maria@mklab.se`).

### `pidge-client::graph::mail` — new functions

```rust
pub async fn get_message(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    message_id: &str,
) -> Result<FullMessage, ClientError>;

pub async fn list_attachments(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<Vec<Attachment>, ClientError>;

pub async fn get_attachment_bytes(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>, ClientError>;

pub async fn mark_read(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), ClientError>;
```

- `get_message`: `GET /me/messages/{id}?$select=id,subject,from,toRecipients,ccRecipients,bccRecipients,receivedDateTime,sentDateTime,isRead,body,hasAttachments` and map to `FullMessage`.
- `list_attachments`: `GET /me/messages/{id}/attachments?$select=name,contentType,size,isInline,contentId`. Only called when `hasAttachments == true`. Filters out non-file attachments (item attachments — rare email-with-email-attached case) by checking `@odata.type == "#microsoft.graph.fileAttachment"`.
- `get_attachment_bytes`: `GET /me/messages/{id}/attachments/{att_id}` — returns the full attachment record including `contentBytes` (base64). Decodes to `Vec<u8>`. Used for rendering inline images. Returns 404 → `ClientError::Graph { status: 404, ... }`.
- `mark_read`: `PATCH /me/messages/{id}` with body `{"isRead": true}`. Returns `()` on success.

### `pidge-client::graph::GraphClient` — wrapper methods

Add five methods to `GraphClient`, each acquiring a fresh access token via `auth.get_valid_token(account)`:

```rust
pub async fn get_message(&self, account: &str, message_id: &str) -> Result<FullMessage, ClientError>;
pub async fn list_attachments(&self, account: &str, message_id: &str) -> Result<Vec<Attachment>, ClientError>;
pub async fn get_attachment_bytes(&self, account: &str, message_id: &str, attachment_id: &str) -> Result<Vec<u8>, ClientError>;
pub async fn mark_read(&self, account: &str, message_id: &str) -> Result<(), ClientError>;
```

`list_attachments` returns `Attachment` records but **not** the bytes — those come on demand via `get_attachment_bytes`. This separation matters because attachment bytes are large; we only fetch them when we actually want to render an inline image.

### `pidge` CLI — new commands

`cli.rs` gains:

```rust
// Extend InboxCommands:
pub enum InboxCommands {
    List { ... },                      // existing
    Show {
        /// Fragment of the 8-char short hash (prefix, suffix, or substring)
        fragment: String,
        /// Also mark the message as read on the server
        #[arg(short = 'r', long)]
        mark_read: bool,
        /// Force-render inline images for this invocation, regardless of trust list
        #[arg(long)]
        show_images: bool,
    },
}

// New top-level subcommand:
Commands::Trust {
    #[command(subcommand)]
    command: TrustCommands,
}

#[derive(clap::Subcommand)]
pub enum TrustCommands {
    /// List trusted sender addresses
    List,
    /// Add an email address to the trust list (idempotent)
    Add { email: String },
    /// Remove an email address from the trust list (idempotent)
    Remove { email: String },
}
```

The `Trust` arm in `Cli::run` forwards `self.json` to `commands::trust::run(command, self.json).await` (sync inside but the dispatcher stays async-compatible for consistency).

### Command file layout

```
crates/pidge/src/commands/
├── inbox.rs            (existing — `list` only; the `Show` arm dispatches into inbox_show)
├── inbox_show.rs       (new)
├── trust.rs            (new)
```

`inbox.rs::run` becomes a thin dispatcher:

```rust
pub async fn run(command: InboxCommands, json: bool) -> Result<()> {
    match command {
        InboxCommands::List { ... } => list(...).await,
        InboxCommands::Show { fragment, mark_read, show_images } => {
            inbox_show::run(fragment, mark_read, show_images, json).await
        }
    }
}
```

### `inbox_show.rs` flow

```
1. Load Config. If no accounts: error "Run pidge auth login first." → exit 1.
2. Load MessageCache. Lookup `fragment` via find_by_fragment.
     NotFound → error → exit 1.
     Ambiguous → print mini-table → exit 1.
     One(short_hash, CachedMessageRef { graph_id, account, ... }) → continue.
3. Build GraphClient. Call get_message(account, graph_id).
     Graph 404 → purge the cache entry, save cache, print error → exit 1.
     Other errors → propagate via anyhow.
4. If has_attachments: list_attachments(account, graph_id).
     Errors are non-fatal: eprintln warning, continue with empty attachments.
5. If json: emit JSON shape. Done.
6. Render text:
     a. Header block (From, To, Cc, Subject, Received).
     b. Separator line.
     c. Body: if Html, run html2text::from_read(body_content) → wrap URLs via linkify_text.
              if Text, pass through linkify_text.
     d. Separator line.
     e. If sender is trusted OR --show-images: filter attachments to is_inline + image content_type → for each: get_attachment_bytes → viuer::print_from_buffer or fallback to placeholder line.
     f. Attachment list (non-inline only, with humansize-formatted size).
7. If --mark-read: call graph.mark_read(account, graph_id). On error, eprintln warning.
```

### `trust.rs` flow

```rust
pub async fn run(command: TrustCommands, json: bool) -> Result<()> {
    let mut config = Config::load()?;
    match command {
        TrustCommands::List => render_list(&config, json),
        TrustCommands::Add { email } => {
            config.add_trusted_sender(&email);
            config.save()?;
            println!("✔ Added {email} to trust list.");
            Ok(())
        }
        TrustCommands::Remove { email } => {
            let removed = config.remove_trusted_sender(&email);
            config.save()?;
            if removed {
                println!("✔ Removed {email} from trust list.");
            } else {
                println!("{email} was not in the trust list.");
            }
            Ok(())
        }
    }
}
```

`render_list` honors `--json`: when true emits `["a@b.com", "c@d.com"]` JSON array; otherwise prints one address per line (or "No trusted senders." if empty).

### Inline image rendering details

Library: `viuer = "0.9"`. It auto-detects terminal capability with the order Kitty > iTerm2 > sixel > half-blocks. Ghostty implements the Kitty graphics protocol, so it's covered.

Function used: `viuer::print_from_file` (for file path input) — but our bytes come from Graph, so we use `viuer::print(&dyn_image, &config)` after decoding via the `image` crate.

Wait — the `image` crate has separate features for png/jpeg/etc. Adding it bloats the binary. Alternative: use `viuer::print_from_buffer(buf, &config)` directly if it accepts encoded bytes... it does NOT. viuer wants a `DynamicImage`.

Resolution: depend on `image = "0.25"` with default features OFF and only `png`, `jpeg`, `webp` enabled. That covers ~95% of inline email images and keeps the binary footprint sane.

Inline rendering algorithm:

```rust
async fn render_inline_images(
    graph: &GraphClient,
    account: &str,
    message_id: &str,
    inline_image_attachments: &[Attachment],
) -> Result<()> {
    println!("Inline images:");
    for att in inline_image_attachments {
        if !is_image_content_type(&att.content_type) {
            continue;
        }
        match graph.get_attachment_bytes(account, message_id, &att.id_in_graph).await {
            Ok(bytes) => match image::load_from_memory(&bytes) {
                Ok(img) => {
                    let conf = viuer::Config { absolute_offset: false, width: Some(60), ..Default::default() };
                    if viuer::print(&img, &conf).is_err() {
                        println!("  [image: {} ({})] (terminal does not support inline images)",
                            att.name, humansize::format_size(att.size_bytes, humansize::DECIMAL));
                    }
                }
                Err(e) => eprintln!("  [image: {} — decode failed: {e}]", att.name),
            },
            Err(e) => eprintln!("  [image: {} — fetch failed: {e}]", att.name),
        }
    }
}
```

`is_image_content_type` checks the prefix `image/` and accepts `image/png`, `image/jpeg`, `image/jpg`, `image/webp`, `image/gif`. Other types (`image/svg+xml` etc.) get the fallback placeholder.

**Open caveat for inline images**: the `Attachment` struct from Graph carries an `id` we need for `get_attachment_bytes`. The `list_attachments` response already includes `id`, so we'll add `id: String` to the `Attachment` core type (though it's a Graph-specific opaque string, it's the natural way to fetch the bytes back). Alternative: don't expose it in `pidge_core` and only return it as part of an internal-to-pidge-client type. Going with the simpler approach: add `id` to `pidge_core::Attachment`. Other providers (Gmail) will produce equivalent identifiers.

### Cache invalidation on 404

If `GraphClient::get_message` returns 404, `inbox_show` calls `MessageCache::load → entries.remove(&short_hash) → save` before printing the error. This keeps the cache accurate over time without requiring a manual cleanup.

### `pidge_core::Config` migration

Adding `trusted_senders: Vec<String>` with `#[serde(default)]` means an existing `config.yaml` that doesn't have the field will load with an empty trust list. No migration required.

## Dependencies

Added to `[workspace.dependencies]`:

| Crate | Version | Purpose |
|---|---|---|
| `html2text` | `0.12` | HTML body → plain text (used in inbox_show) |
| `viuer` | `0.9` | Terminal inline image rendering (auto-detects protocol) |
| `image` | `{ version = "0.25", default-features = false, features = ["png", "jpeg", "webp"] }` | Decoding inline images for viuer |
| `humansize` | `2` | Friendly file sizes for attachments |

`base64` is already in workspace deps from earlier work.

## CHANGELOG

Under `[Unreleased] ### Added`:

```markdown
- `pidge inbox show <fragment>` — substring-lookup a message by its 8-char short hash and display headers + body + attachment list
- `pidge inbox show --mark-read` / `-r` to mark the message as read on the server
- `pidge inbox show --show-images` to force inline image rendering for one invocation
- `pidge trust list/add/remove` — manage the trusted-senders list; inline images auto-render for trusted senders in image-capable terminals (Ghostty, Kitty, iTerm2)
- Trusted senders stored at `trusted_senders:` in `~/.config/pidge/config.yaml`
```

## Risks / open considerations

- **`html2text` heuristics aren't always pretty.** Most emails render cleanly; some marketing emails with weird table-based layouts may produce ragged text. Acceptable — the alternative (a full HTML renderer) is way out of scope. Users wanting pixel-perfect rendering can click any URL in the body to open the original in a browser.
- **Inline images don't appear in their original positions.** Tradeoff: we don't have a HTML rendering pipeline that can interleave images with text in the terminal. Showing all inline images at the end is a reasonable simplification.
- **`viuer` falls back to half-block rendering on non-image-capable terminals.** This may look poor at low resolutions; the implementation prints the placeholder text instead when `viuer::print` returns an error, but viuer's heuristic for "this terminal can't do images" is not perfect. If a user reports ugly output on a specific terminal, we'd add an explicit `PIDGE_DISABLE_INLINE_IMAGES=1` opt-out.
- **`image` crate bloat.** Even with minimal features, decoding adds ~300KB to the binary. Acceptable given the feature value.
- **Trust list is per-pidge-install, not per-account.** Adding `maria@mklab.se` to the trust list trusts that address across every account. If you sign in to two M365 tenants and both have a `maria@…` colleague, both inboxes will render her images. Realistic call: most users have one or two trusted senders (newsletter publishers, work colleagues) and don't need per-account scoping.

## Deferred to next features

- `pidge inbox attachment download <fragment> <attachment-name>` — its own feature
- Domain wildcards in trust list (`*@github.com`) — would need refactoring `is_sender_trusted` and the CLI
- "Trust on first view" prompt in interactive mode (`pidge inbox show` with no `--show-images` and an untrusted sender: prompt "trust this sender? [y/N]")
- Image position preservation in body (would require building or vendoring an HTML-to-terminal renderer)
- Per-account trust scoping
