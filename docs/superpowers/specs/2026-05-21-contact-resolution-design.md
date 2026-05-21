# Contact name resolution — design

**Status:** Approved 2026-05-21
**Goal:** Let the user type `@Dino` instead of `dino.semovic@needefy.se` when inviting attendees or addressing mail. Resolve names against a local index built from the user's own recent correspondence.

## Scope

In:
- `pidge-core::ContactsCache` — JSON store mirroring `MessageCache`/`EventCache`. Keyed by lowercase email; one entry per address.
- `pidge contacts refresh [--days N] [--account <email>]` — scan recent inbox messages (sender only — `list_inbox` `$select`s `from`, not the recipient lists) and calendar events (organizer + attendees) over the window, build/merge the cache. Future revision can add Sent Items scanning for "people I've emailed" — see Non-goals.
- `pidge contacts find <query> [--json]` — substring match on display name or local-part, exact-email match wins.
- Inline `@<query>` syntax in `--invite`, `--invite-optional`, `--to`, `--cc`, `--bcc` on `calendar new`, `calendar edit`, and `mail new`. A token without `@` prefix is treated as a literal email (current behaviour preserved → fully non-breaking).

Out:
- No live Graph query for `find` — purely local index. Refresh is the way to get current data.
- No fuzzy / Levenshtein matching in v1 — substring only.
- No contacts beyond the user's own mail/calendar (no organisation-wide directory lookup via Graph `/users` or People API).
- No editing of the cache from the CLI other than refresh.
- No per-account isolation of contacts — same person across multiple accounts collapses to one entry. Account context comes from `--from`, not the contact.
- No scanning of Sent Items in v1. People I've emailed but who haven't replied won't appear unless they're also on a calendar invite. Easy v2.

## Architecture

### Module layout

```
crates/pidge-core/src/
└── contacts.rs            # ContactsCache type, load/save, merge/upsert, lookup

crates/pidge-client/src/graph/
├── mail.rs                # (existing) — list_inbox already returns to/cc/bcc
└── events.rs              # (existing) — list_calendar_view already returns attendees

crates/pidge/src/commands/
├── contacts.rs            # `pidge contacts` dispatcher
├── contacts_refresh.rs    # `pidge contacts refresh`
├── contacts_find.rs       # `pidge contacts find`
└── name_resolve.rs        # shared `resolve_addresses(&[String]) -> Result<Vec<String>>`
                           # used by calendar_new, calendar_edit, mail_compose
```

### Cache shape

```rust
#[derive(Serialize, Deserialize)]
pub struct ContactsCache {
    /// Map from lowercase email address to Contact.
    by_email: HashMap<String, Contact>,
    last_refreshed: Option<DateTime<Utc>>,
}

pub struct Contact {
    pub email: String,            // canonical lowercase
    pub display_name: Option<String>,
    pub last_seen: DateTime<Utc>,
    pub seen_in_mail: u32,
    pub seen_in_calendar: u32,
}
```

Storage: `dirs::cache_dir()/pidge/contacts.json`. Atomic write via tempfile + rename, same pattern as the existing caches.

### Resolution algorithm

`resolve_one(token: &str, cache: &ContactsCache) -> Result<String, ResolveError>`:

1. Strip leading `@`. If absent, return token as-is (it's a literal email).
2. Lowercase the query.
3. If `cache.by_email.contains(query)` → return that exact email (exact match wins regardless of name).
4. Otherwise collect every `Contact` where `query` is a substring of:
   - `email.to_lowercase()`, or
   - `display_name.unwrap_or_default().to_lowercase()`, or
   - the local-part of `email` (chars before `@`).
5. Zero matches → `ResolveError::Unknown(token)`.
6. One match → return its `email`.
7. Multiple matches → `ResolveError::Ambiguous { token, candidates: Vec<Contact> }` — surfaced as a single anyhow error with all candidates listed.

`resolve_addresses(tokens: &[String]) -> Result<Vec<String>, anyhow::Error>` loads the cache once, calls `resolve_one` per token, aggregates errors so the user sees every ambiguous token in one pass rather than fix-then-retry-then-discover-the-next-one.

### Refresh algorithm

For each account (or just `--account` if given):

1. Mail: `list_inbox(account, limit=1000, skip=0)`. For each message: upsert `from`. `seen_in_mail += 1`, `last_seen = max(...)`. Recipient lists (to/cc/bcc) are not in the `$select` used by `list_inbox` — extending coverage to those requires either widening `Message`'s schema or scanning Sent Items, both deferred to a follow-up.
2. Calendar: `list_calendar_view(account, None, now - days, now + days, limit=500)`. For each event: upsert organizer + every attendee. `seen_in_calendar += 1`, `last_seen = max(...)`.
3. Skip empty addresses, no-reply patterns (`noreply@`, `no-reply@`, `donotreply@`).
4. Save the cache with `last_refreshed = now`.

The 1000/500 fetch ceilings are pragmatic: enough for the typical personal mailbox, capped so refresh doesn't take minutes on heavy accounts. A future revision could paginate.

### Inline-syntax integration

In `calendar_new`, `calendar_edit`, `mail_compose` (the existing `mail new` handler), after collecting the comma-separated `--invite` / `--to` / `--cc` / `--bcc` `Vec<String>`s, call:

```rust
let required = name_resolve::resolve_addresses(&args.invite)?;
let optional = name_resolve::resolve_addresses(&args.invite_optional)?;
```

The error path is a single `anyhow!` formatted to look like:

```
Could not resolve 2 attendee tokens:
  @john — ambiguous: john.smith@acme.com, john.doe@acme.com
  @nope — unknown (run `pidge contacts refresh` to update the index)
```

Existing behaviour (literal emails without `@`) flows through unchanged.

## Error handling

- Missing cache file → `find` and `resolve_addresses` return a clear "run `pidge contacts refresh` first" message.
- Stale cache (older than 30 days) → emit a warning to stderr on each resolution but proceed.
- Refresh failure on one account → surface the error but continue with the remaining accounts.

## Testing

`pidge-core::contacts`:
- Cache roundtrip (serialize/deserialize, atomic write).
- Upsert merges entries by lowercase email, sums counters, keeps latest `last_seen`.
- Display name from latest message wins; never overwrite with empty.

`commands::name_resolve`:
- Token without `@` returns unchanged.
- Token with `@` and exact email match returns that email.
- Substring matches display name.
- Substring matches local-part.
- Ambiguous → error lists every candidate (deterministic order: most-recently-seen first).
- Unknown → error includes the original token verbatim.

`commands::contacts_refresh` is integration-tested manually against the user's live accounts (no mock harness yet — see the "command-level integration tests with a GraphClient trait + mock" item in the improvements backlog).

## CHANGELOG

Single entry under `### Added`:

> - `pidge contacts refresh` / `contacts find` — local name → email index, populated from recent mail and calendar. New inline `@name` syntax in `--invite` / `--to` / `--cc` / `--bcc` resolves against the index; literal emails without `@` are unchanged.

## Release

Patch bump (0.4.2 → 0.4.3) at the end of the implementation cycle, same release flow as v0.4.2.
