# Output refresh + message-ID UX — design

**Status:** Approved 2026-05-14
**Goal:** Refresh `pidge inbox list` with a clean Scandinavian terminal layout that shows more context per message (subject + preview), introduce a global `--json` flag for machine output across all data commands, give every message a stable human-typable short-hash ID that future `pidge inbox show` can target via substring matching, and lay down a small output utility module for OSC 8 hyperlinks.

## Non-goals

- `pidge inbox show <fragment>` — its own follow-up feature; uses the cache built here.
- Markdown rendering (`termimad`) — deferred until the first command that displays a body needs it.
- Subject→Outlook webLink wrapping — deferred. (User noted real URL value is in body content, displayed by `inbox show`.)
- Adding folders, search, send, draft, calendar, or any other inbox/calendar feature.

## What changes for the user

### Default `pidge inbox list`

```
ID        FROM         SUBJECT                                    RECEIVED
─────────────────────────────────────────────────────────────────────────
7a3b9c2f  Maria L.     Quarterly numbers ready for review         5m ago
                       Hi Kristofer, attached are the Q1 numbers
                       for review. Let me know if you have any
f2a8d1c0  GitHub       [GitHub] Claude requesting permissions     3h ago
                       You have a pending request to authorize
                       "Claude Code" to access your repositories
```

- Single horizontal line under header. No vertical separators. No outer borders.
- Subject is **bold + magenta** when unread, **cyan** when read. No bullet column.
- Preview is `bodyPreview` from Graph, *dimmed*. Capped at Graph's ~255-char default; comfy-table wraps naturally to fit the SUBJECT column.
- ID column shows the 8-char hex short hash, dimmed.
- ACCOUNT column appears between ID and FROM when more than one account is signed in (or no `--account` filter is set). Hidden when single-account view (existing logic).
- URLs in subject or preview text are wrapped in OSC 8 hyperlinks (clickable in modern terminals; plain text in legacy terminals).

### `pidge inbox list --compact` (or `-c`)

```
ID        FROM         SUBJECT                                  RECEIVED
─────────────────────────────────────────────────────────────────────
7a3b9c2f  Maria L.     Quarterly numbers ready for review       5m ago
f2a8d1c0  GitHub       [GitHub] Claude requesting permissions   3h ago
8c1d2e3a  Anna         Re: lunch wednesday                      Mon
```

- One row per message. No preview line.
- Same subject styling (bold magenta if unread, cyan if read). No bullet column.
- Same ID column.

### `pidge inbox list --json`

```json
[
  {
    "id": "7a3b9c2f",
    "graph_id": "AAMkAGI2T…",
    "account": "kristofer@mklab.se",
    "from": { "name": "Maria L.", "address": "maria@mklab.se" },
    "subject": "Quarterly numbers ready for review",
    "received_at": "2026-05-14T08:30:00Z",
    "is_read": false,
    "preview": "Hi Kristofer, attached are the Q1 numbers…"
  }
]
```

- `id` is the 8-char short hash humans type. `graph_id` is the full opaque Microsoft Graph ID for scripts that need it.
- Same shape regardless of `--compact` (which only affects text rendering).

### `--json` on other commands

- `pidge auth list --json` → array of accounts: `[{ "email", "tenant_id", "home_account_id", "added_at", "is_default_send", "is_default_calendar" }, …]`
- `pidge auth status --json` → `{ "accounts": N, "defaults": { "send": "…", "calendar": "…" }}`
- Other commands (`auth login`, `auth logout`, `auth default`, `auth status` interactive parts) are not changed — they have side effects, not data output. `--json` is silently ignored on those.

## Architecture

### Workspace dependency changes

| Crate | Change |
|---|---|
| `comfy-table` (workspace) | Extend entry to `{ version = "7", features = ["custom_styling"] }`. The `custom_styling` feature uses the `console` crate to compute visible width of strings containing ANSI escape codes — fixes the column-alignment bug for free and enables mixed-styled multi-line cells. |
| `linkify` (workspace) | New: `linkify = "0.10"`. URL detection inside arbitrary text. |
| `sha2` (workspace) | New: `sha2 = "0.10"`. Hashing Graph IDs to produce the 8-char short ID. |

No new top-level Rust crates need to be created. The new code lives in two new modules:

```
crates/pidge-core/src/
└── cache.rs              # NEW — MessageCache, CachedMessageRef, CacheLookup

crates/pidge/src/output/
├── mod.rs                # NEW — re-exports
├── hyperlink.rs          # NEW — OSC 8 sequence helper
└── linkify.rs            # NEW — wrap URLs in arbitrary text with OSC 8
```

### `pidge-core::cache::MessageCache`

```rust
//! Persistent cache mapping short message hashes to Graph IDs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::CoreError;

const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMessageRef {
    pub graph_id: String,
    pub account: String,
    pub cached_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MessageCache {
    pub entries: HashMap<String, CachedMessageRef>,
}

pub enum CacheLookup {
    NotFound,
    One(String, CachedMessageRef),                  // short_hash, ref
    Ambiguous(Vec<(String, CachedMessageRef)>),
}

impl MessageCache {
    pub fn default_path() -> Result<PathBuf, CoreError> { /* ... */ }
    pub fn load() -> Result<Self, CoreError> { /* tolerant of missing file */ }
    pub fn load_from(p: &Path) -> Result<Self, CoreError> { /* ... */ }
    pub fn save(&self) -> Result<(), CoreError> { /* ... */ }
    pub fn save_to(&self, p: &Path) -> Result<(), CoreError> { /* ... */ }

    /// Compute the 8-char hex short hash for a Graph ID.
    pub fn short_hash(graph_id: &str) -> String {
        let mut h = Sha256::new();
        h.update(graph_id.as_bytes());
        let result = h.finalize();
        format!("{:02x}{:02x}{:02x}{:02x}", result[0], result[1], result[2], result[3])
    }

    /// Insert messages, updating cached_at. Evicts oldest if over MAX_ENTRIES.
    pub fn insert_many(&mut self, msgs: &[(String, String, String)]) {
        // tuples: (graph_id, account, short_hash_computed_outside)
        // ... insert + LRU eviction by cached_at
    }

    /// Find a message by a fragment of its short hash. Returns:
    ///   NotFound — zero matches
    ///   One — exactly one match
    ///   Ambiguous — 2+ matches (return up to 10 with their hashes)
    pub fn find_by_fragment(&self, fragment: &str) -> CacheLookup {
        let matches: Vec<_> = self
            .entries
            .iter()
            .filter(|(k, _)| k.contains(fragment))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        match matches.len() {
            0 => CacheLookup::NotFound,
            1 => {
                let (k, v) = matches.into_iter().next().unwrap();
                CacheLookup::One(k, v)
            }
            _ => CacheLookup::Ambiguous(matches.into_iter().take(10).collect()),
        }
    }
}
```

Path: `${XDG_CACHE_HOME:-~/.cache}/pidge/messages.json` (via `dirs::cache_dir()`). File format is JSON for easy debuggability — not security-sensitive, just an index.

Tests:
- short_hash is deterministic and stable
- insert + save + load round-trip
- LRU eviction kicks in at MAX_ENTRIES, evicts oldest
- find_by_fragment returns NotFound / One / Ambiguous correctly
- Substring matching works at start, middle, end of hash

### `crates/pidge/src/output/`

`hyperlink.rs`:

```rust
//! OSC 8 hyperlink helper.
//!
//! Modern terminals interpret the OSC 8 escape sequence as a hyperlink:
//!   ESC ] 8 ; ; URL BEL TEXT ESC ] 8 ; ; BEL
//! Older terminals strip the escapes and show plain TEXT.
//! See https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda

const OSC8_START: &str = "\x1b]8;;";
const ST: &str = "\x1b\\";

/// Wrap `text` so clicking it in a supporting terminal opens `url`.
pub fn hyperlink(url: &str, text: &str) -> String {
    format!("{OSC8_START}{url}{ST}{text}{OSC8_START}{ST}")
}
```

`linkify.rs`:

```rust
//! Scan text for URLs and wrap each with an OSC 8 hyperlink.

use linkify::{LinkFinder, LinkKind};

use crate::output::hyperlink::hyperlink;

/// Find URLs in `text` and wrap each with an OSC 8 link to itself.
/// Text without URLs is returned unchanged.
pub fn linkify_text(text: &str) -> String {
    let finder = LinkFinder::new();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for link in finder.links(text) {
        if !matches!(link.kind(), LinkKind::Url) {
            continue;
        }
        let (start, end) = (link.start(), link.end());
        out.push_str(&text[cursor..start]);
        out.push_str(&hyperlink(link.as_str(), link.as_str()));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}
```

Tests:
- Plain text returns unchanged
- One URL gets wrapped
- Multiple URLs each get wrapped
- Email addresses (`mailto:`) are NOT wrapped (only URLs) — `LinkFinder::default()` finds both kinds; we filter to `LinkKind::Url`

`mod.rs`:

```rust
pub mod hyperlink;
pub mod linkify;

pub use hyperlink::hyperlink;
pub use linkify::linkify_text;
```

Wire into `crates/pidge/src/main.rs`: `mod output;` (alongside the existing `mod commands;` etc.).

### CLI changes

`crates/pidge/src/cli.rs`:

```rust
// Add to Cli struct:
/// Output as machine-readable JSON instead of formatted text
#[arg(long, global = true)]
pub json: bool,

// InboxCommands::List — remove `output: OutputFormat`, add `compact: bool`:
List {
    #[arg(long)]
    account: Vec<String>,

    #[arg(short = 'n', long, default_value = "25")]
    limit: usize,

    #[arg(long)]
    unread: bool,

    /// One row per message (no preview lines)
    #[arg(short = 'c', long)]
    compact: bool,
}

// Remove OutputFormat enum entirely.
```

Dispatch threading in `Cli::run`:

```rust
// Each data-output handler now takes a `json: bool`.
Some(Commands::Auth { command }) => crate::commands::auth::run(command, self.json).await,
Some(Commands::Inbox { command }) => crate::commands::inbox::run(command, self.json).await,
```

`commands/auth.rs::run` signature changes to accept `json` and forwards to leaf handlers:

```rust
pub async fn run(command: AuthCommands, json: bool) -> Result<()> {
    match command {
        AuthCommands::Login => auth_login::run().await,
        AuthCommands::List => auth_list::run(json),
        AuthCommands::Status => auth_status::run(json),
        AuthCommands::Logout { account, all, yes } => auth_logout::run(account, all, yes),
        AuthCommands::Default { send, calendar } => auth_default::run(send, calendar),
    }
}
```

### `inbox.rs` changes

- Drop `OutputFormat` import and the `output` match arm.
- Run signature: `pub async fn run(command: InboxCommands, json: bool) -> Result<()>`.
- Top of `list(...)`: compute `short_hash` per fetched message; build a vec of `(graph_id, account, short_hash)` and update the MessageCache (`load → insert_many → save`).
- After cache update, dispatch render:
  - `json` → `render_json(messages_with_short_hash)` (extends current json shape with the `id` short-hash field)
  - else if `compact` → `render_text_compact(...)`
  - else → `render_text_rich(...)`

`render_text_rich`:

```rust
fn render_text_rich(messages: &[MessageRow], hide_account: bool) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account { header.remove(1); }
    table.set_header(header);

    for row in messages {
        let subject_styled = if row.message.is_read {
            row.message.subject.cyan().to_string()
        } else {
            row.message.subject.bold().magenta().to_string()
        };
        let preview_linkified = linkify_text(&row.message.preview);
        let preview_styled = preview_linkified.dimmed().to_string();
        let subject_cell = format!("{subject_styled}\n{preview_styled}");

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            row.message.account.clone(),
            from_display(&row.message.from),
            subject_cell,
            relative_received(row.message.received_at),
        ];
        if hide_account { cells.remove(1); }
        table.add_row(cells);
    }
    println!("{table}");
    Ok(())
}
```

`render_text_compact` is identical except no preview line in the subject cell:

```rust
let subject_cell = subject_styled; // no "\n{preview}"
```

`MessageRow` is a small local struct that pairs `pidge_core::Message` with the computed `short_hash`. The `pidge_core::Message` struct does NOT gain a hash field — the hash lives only in the cache and in the rendered output.

URLs in the subject: also pass `subject` through `linkify_text` before styling. The OSC 8 escape sequences are part of the cell content; `custom_styling` measures visible width correctly.

### `auth_list.rs` and `auth_status.rs` changes

Each gains a `json: bool` parameter. When true, emit JSON via `println!("{}", serde_json::to_string_pretty(...)?)`. Otherwise existing behavior unchanged.

For `auth_list`, JSON shape:

```json
[
  {
    "email": "kristofer@mklab.se",
    "tenant_id": "11111111-2222-3333-4444-555555555555",
    "home_account_id": "...",
    "added_at": "2026-05-14T08:30:00Z",
    "is_default_send": true,
    "is_default_calendar": true
  }
]
```

For `auth_status`, JSON shape:

```json
{
  "accounts": 2,
  "defaults": {
    "send": "kristofer@mklab.se",
    "calendar": "kristofer@mklab.se"
  }
}
```

`pidge_core::Account` already derives `Serialize`/`Deserialize`, so we can build the array inline; the `is_default_*` booleans are computed from `Config::defaults`.

## Testing strategy

- **`pidge-core::cache`** unit tests:
  - `short_hash` is deterministic
  - round-trip load/save through tempfile
  - `insert_many` LRU eviction at `MAX_ENTRIES`
  - `find_by_fragment`: NotFound / One / Ambiguous; substring match at start, middle, end
- **`pidge::output::hyperlink`** unit tests:
  - The escape sequence is wellformed
- **`pidge::output::linkify`** unit tests:
  - Plain text returns unchanged
  - One URL gets wrapped
  - Multiple URLs each get wrapped
  - Email addresses are not wrapped
- **`pidge::commands::inbox`** (existing) — keep the two `compute_per_account_fetch` tests. Render functions are hard to snapshot meaningfully because of ANSI codes; we rely on visual smoke-testing for `text` output and structural assertions for JSON.

## CHANGELOG entry

Under `[Unreleased] ### Added`:

- Global `--json` flag (replaces per-command `--output`) on `pidge inbox list`, `pidge auth list`, `pidge auth status`
- `pidge inbox list` shows a stable 8-char short hash ID per message; cached at `~/.cache/pidge/messages.json` for substring lookup by future `pidge inbox show`
- `pidge inbox list` rich layout: subject + 2-line preview, bold+magenta for unread, cyan for read; `--compact`/`-c` for the one-row-per-message style
- URLs in subject and preview text are OSC 8 hyperlinks (clickable in modern terminals)
- Cleaner table style — horizontal line under header only, no vertical borders

## Risks / open considerations

- **`custom_styling` feature interactions:** Enabling `comfy-table`'s `custom_styling` feature pulls in the `console` crate transitively. This adds compile time and a non-trivial dependency, but it's the correct fix for ANSI-in-cell width measurement. The trade-off is accepted.
- **Hash collision probability:** 8 hex chars = 32 bits of state. For a mailbox of ~10,000 messages, birthday-paradox collision probability is ~10⁻³. Acceptable for the cache-fragment-lookup use case; on collision, `find_by_fragment` returns `Ambiguous` and asks the user to provide more characters. We don't try to extend the hash for collided messages (rare enough).
- **Linkify finds URLs greedily:** Some characters that look like URLs (`example.com.` with trailing period) get edge-cased by `linkify`. We accept its behavior; pidge doesn't try to post-process.
- **OSC 8 in non-supporting terminals:** Terminals that don't understand OSC 8 typically strip the escape sequences entirely on rendering, showing only the visible text. No corruption expected. A few legacy terminals might display literal `]8;;URL\` text — acceptable.
- **Cache file growth:** 1000 entries × ~300 bytes per entry = ~300KB max. Negligible.
- **Future `pidge inbox show` interaction:** Will read the cache, find by fragment, GET `/me/messages/{graph_id}` for full body. The cache is the bridge. Not in scope for this spec.

## Deferred to next features

- `pidge inbox show <fragment>` — uses cache from this spec
- `termimad` + markdown rendering — added when `inbox show` lands and the body needs rich rendering
- Subject→Outlook webLink wrapping — deferred
- `auth login` / `auth logout` / `auth default` JSON output — these are interactive commands without data output; `--json` is silently ignored on them
- `pidge inbox search` and folder navigation
