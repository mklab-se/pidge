# `pidge mail attachments` — download received attachments

## Problem

The Graph client already exposes `list_attachments()` and `get_attachment_bytes()`,
and `mail show` lists a message's attachments (name + size) and uses
`get_attachment_bytes` to render inline images. But there is no user-facing way
to **save** a received attachment to disk. This spec adds one.

No changes to `pidge-core` or `pidge-client` are required — the plumbing exists.

## Command surface

A new subcommand group under `mail`, mirroring the existing
`drafts attachments {list,add,remove}` structure:

```
pidge mail attachments list <fragment>          # name / size / type / inline marker
pidge mail attachments save <fragment> [name]   # download to disk
```

### `save` arguments

- `<fragment>` — fragment of the message's 8-char short hash, resolved via the
  shared `mail_fragment::resolve` helper (same as `mail show`, `mail flag`, …).
- `[name]` — optional. Case-insensitive **substring** match against attachment
  filenames.
  - Omitted → select **all** non-inline file attachments.
  - One match → that attachment.
  - Multiple matches → error listing the candidates (user narrows the name).
  - No match → error listing what the message does contain.
- `-o, --out <path>` — destination.
  - Existing directory, or a path ending in the platform separator → write
    file(s) into it under their **original** names.
  - Otherwise → treated as a target **file path** (rename). Valid only when
    exactly one attachment is selected; error otherwise
    (`--out is a file but N attachments selected`). Parent directories are
    created as needed.
  - Omitted → default destination `~/Downloads` (via `dirs::download_dir()`,
    falling back to the home dir, then `.`).
- `--include-inline` — also include `is_inline` attachments (embedded logos,
  signature images, inline photos). Affects both the all-attachments case and
  name matching.
- `-f, --force` — overwrite an existing target file. Without it, an existing
  target is an error (`<path> exists (use --force)`).

### `list` arguments

- `<fragment>` — as above.
- Honors the global `--json` flag (like `mail show`). Human output is a
  `comfy_table` with NAME / SIZE / TYPE columns and an inline marker.
  By default lists file attachments; `--include-inline` adds inline ones.

## Flow (`commands/mail_attachments.rs`)

`save`:

1. `mail_fragment::resolve(&fragment)` → `(short_hash, MessageRef)`.
2. `GraphClient::list_attachments(account, graph_id)`.
3. Filter out inline unless `--include-inline`.
4. If `name` given, retain attachments whose filename contains it
   (case-insensitive). Resolve ambiguity / no-match as above.
5. Resolve the destination directory-or-file decision; validate the
   file-rename-with-many case.
6. For each selected attachment:
   - `GraphClient::get_attachment_bytes(account, graph_id, att.id)`.
   - Compute the target path; if it exists and not `--force`, error.
   - `std::fs::write(path, bytes)`.
   - Print `+ name (type, size) → path` (matching the upload confirmation in
     `commands/attachments.rs`).

`list`: resolve fragment → `list_attachments` → filter inline per flag →
render table or JSON.

## Wiring

- `cli.rs`: add `Attachments { #[command(subcommand)] command: MailAttachmentCommands }`
  to `MailCommands`, and a new `MailAttachmentCommands { List { … }, Save { … } }`
  enum.
- `commands/mail.rs`: dispatch the new arm to `mail_attachments::run`.
- `commands/mod.rs`: register `pub mod mail_attachments;`.
- `commands/skill.rs`: add a line to the emitted SKILL.md noting the
  download capability so agents discover it.

## Error handling

- Fragment not found / ambiguous → handled by `mail_fragment::resolve`.
- Message has no attachments → clear message.
- Name matches nothing / matches several → clear message listing candidates.
- `--out` is a file path but multiple attachments selected → error.
- Target exists without `--force` → error naming the path.
- Per-attachment fetch failure → `with_context` naming the attachment; abort
  (do not leave a partial set silently).

## Testing

Unit tests for the pure logic (no network):

- name-substring filtering: none / one / many candidates.
- inline filtering with and without `--include-inline`.
- target-path resolution: into-directory (original name), explicit-file
  (rename), default-Downloads, and the "file path but >1 attachment" error.
- collision detection: existing target without `--force` is rejected.

The download itself is a thin `fs::write` over the already-tested
`get_attachment_bytes`, so no new client tests are needed.

## Out of scope

- Resumable/chunked download for very large items (Graph returns
  `contentBytes` inline; the same simple-upload asymmetry as the existing
  attach path).
- Downloading `itemAttachment` (nested message) attachments — `list_attachments`
  already filters to `fileAttachment` only.
