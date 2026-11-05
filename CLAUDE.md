# pidge

The e-mail and calendar client for your AI agent (Microsoft 365, Outlook.com and Gmail): a CLI, a local MCP server (`pidge mcp serve`) and a hosted MCP server, all over one engine. **Designed to be operated by AI agents** (Claude Code, Codex, Copilot, ChatGPT, Claude.ai, …) on the user's behalf; humans can run it directly. `pidge skill` prints a SKILL.md that wires pidge into an agent; `--from-source` makes it run pidge via `cargo run` from a source checkout.

## Commands

```bash
cargo build                              # Build all crates
cargo test --workspace                   # Run all tests
cargo clippy --workspace -- -D warnings  # Lint (CI-enforced)
cargo fmt --all -- --check               # Format check (CI-enforced)
cargo run -- --help                      # Run the CLI
```

## Architecture

Rust workspace, one engine and three thin frontends. Design: `docs/superpowers/specs/2026-11-05-pidge-2-ground-up-rethink-design.md`.

```
crates/
  pidge-core/     # Plain types (Message, Event, Account, Config), `paths.rs` (where files live), the HTML
                  # renderer (`render.rs`, fixture snapshots in tests/), time ranges, availability,
                  # List-Unsubscribe parsing, and `provider.rs`: the `Provider` trait every mail service implements
  pidge-client/   # Microsoft Graph: OAuth (browser PKCE, refresh, keychain/file `TokenBackend`), `send_with_retry`,
                  # $batch, delta, immutable ids; `graph/provider.rs` = `GraphProvider`; `mcp/` = client side of `pidge mcp connect`
  pidge-gmail/    # Gmail + Google Calendar behind `Provider`: Google OAuth, labels, history sync, RFC 5322 drafts
  pidge-store/    # Per-user SQLite (rusqlite, bundled): short handles (m12, e3, d4, p5, s1), the mail index with
                  # pidge's classification, events, plans/sessions (docs), undo log, outbox, notes, send log
  pidge-engine/   # Every operation, once: `ops/` (mail_brief, mail_find, mail_read, mail_triage, mail_act,
                  # mail_draft, mail_send, mail_attachment, mail_setup, calendar_agenda/event/act/respond/
                  # availability, accounts), `registry.rs` (op defs + JSON Schema, run with notes), `ctx.rs`
                  # (`User`, the `Host` seam, background worker), `sync.rs`, `kinds.rs` + `classify.rs`
                  # (rules, signals, importance, optional Ailloy eval), `outbox.rs` (risk-based send hold),
                  # `prefs.rs`, `render.rs` (untrusted wrapper), `playbooks.rs` (MCP instructions + SKILL.md),
                  # `mcp.rs` (feature `mcp`: the rmcp handler both servers use)
  pidge/          # CLI: `dynamic.rs` builds `pidge mail|calendar|accounts ...` from the registry; `cli.rs` the
                  # fixed commands (account, mcp serve/connect/status/logout, ai, skill, op, completion);
                  # `local.rs` = `LocalHost` (config.yaml mailboxes, Downloads, markitdown if installed,
                  # detached `pidge outbox run`) and the stdio MCP server
  pidge-mcp/      # Hosted MCP server (`publish = false`): own OAuth 2.1 AS with Microsoft sign-in, consent pages,
                  # bearer-guarded /mcp serving the engine (`engine.rs`: per-user engines, `HostedHost`, Blob prefs),
                  # Google connect (`/callback/google`), `/dl` signed downloads, `/ai` key page, Key Vault secrets
deploy/azure/     # Bicep + deploy.sh + Dockerfile for pidge-mcp on Container Apps
```

- `pidge-core` has no HTTP or auth code; the engine depends on traits (`Provider`, `PrefsStore`, `Host`), never on Graph, Gmail or SQLite paths directly.
- A new capability is an engine operation: add it under `crates/pidge-engine/src/ops/`, list it in `ops::all()`, and the CLI command, both MCP tools and the skill follow. Don't add mail or calendar logic to a frontend.
- The `Host` trait is the only place frontends differ (accounts and sign-in, attachment delivery, document conversion, one-click unsubscribe, the outbox runner).

### Engine patterns

- **Handles:** operations take and return short handles; `ops::resolve_message` / `resolve_event` map them through the store. Graph ids are immutable ids (`Prefer: IdType="ImmutableId"`, also on every `$batch` sub-request), so a handle survives moves.
- **Fire-and-forget:** `ops::mail_act::change_now` applies a change to the index, records undo, and enqueues the provider call on the user's worker (`User::enqueue`; inline on the CLI). A failure restores the index and leaves a note; `registry::run` puts notes on top of the next answer.
- **Plans:** selector-based changes, more than `policy.bulk_threshold` messages, and calendar changes that e-mail people return a plan (a doc in the store, 15 min) that the next call applies. Policy lives in prefs (`send: hold|confirm|allow|deny`, `notify`, `bulk`, `delete`).
- **Sending:** only `mail_send` sends, only drafts, through `outbox.rs`: risk assessment (recipients, first contact, forwards, attachments, identity/card numbers, the user's AI) sets a 10 s / 30 s / 2 min hold or approval. The hourly cap (`policy.sends_per_hour`, default 30) counts sends, invitations, cancellations, RSVP messages and unsubscribe e-mails.
- **Triage:** `ops/triage.rs` plans sweeps, batches, then singles; every answer ends with what to do next; `after_act` advances a session when `mail_act` touches its current message.
- **Classification:** kind (fixed set), category (the user's, with rules and examples) and importance (with reasons) per message, stored in the index with their source (rule, signal, ai, harness). Changing categories or rules resets them (`reset_classifications`).
- **Untrusted content:** bodies, previews, converted attachments and quoted text go out inside `<untrusted-email-content>` via `render::untrusted`; header fields through `render::one_line`/`inline`.
- **Time:** inputs are interpreted in the user's timezone (prefs, else the machine's or Europe/Stockholm hosted); `calendar::parse_start` reads `tomorrow 15:00`, `fri 9:30`, ISO, dates.

### pidge-mcp patterns

- **Bearer identity:** no tool takes a user id. The bearer middleware attaches `AuthenticatedUser`; `engine::HttpSource` turns it into that user's engine.
- **Ownership:** `engine::providers` hands the engine only mailboxes the user record lists *and* whose `MailboxRecord { owner, tokens, identity, provider }` names the user. Ids are confined to each provider's id alphabet before a path is built.
- **One browser per flow:** the Continue step (`/authorize/go`, `/connect/go`) sets a cookie that `/callback` and `/callback/google` require. Mailbox secrets are named `mailbox-<sanitized address>-<hash8>`.
- **State:** Key Vault holds the signing key, user records, mailbox tokens and AI keys; preferences are one JSON blob per user (`PIDGE_MCP_PREFS_URL`, Azure Blob via the managed identity; `PIDGE_MCP_PREFS_DIR` in dev); the index is a per-user SQLite file in `PIDGE_MCP_INDEX_DIR` (a cache). First use seeds prefs from the 1.x record (review categories, timezone, default sender, trusted senders as VIPs).
- **markitdown hardening:** scrubbed environment; a fresh per-conversion work dir as `HOME`/`TMPDIR`, removed on every path; 30 s timeout with kill-on-drop; 4 GB address-space limit on Linux; 2 MB output cap; stderr discarded; at most 2 concurrent conversions. The server marks itself non-dumpable on Linux.
- **Deploy:** the `deploy` job of `.github/workflows/release.yml` deploys on every `v*` tag and on manual dispatch, never on a push to `main`. Every env var the running server reads is documented on `Config::from_env` in `crates/pidge-mcp/src/config.rs`; `deploy.sh` also reads deploy-time vars (`PIDGE_RG`, `PIDGE_CLIENT_ID`, `IMAGE_TAG`, `PIDGE_MCP_CUSTOM_DOMAIN`, `PIDGE_MCP_CUTOVER`, `PIDGE_MCP_MIN_REPLICAS`, `PIDGE_GOOGLE_CLIENT_ID`, `PIDGE_GOOGLE_CLIENT_SECRET`).
- **CLI side:** `pidge mcp connect/status/logout` sign a local user into a hosted server and move local Microsoft mailboxes onto it (`crates/pidge-client/src/mcp/`); user guide: `docs/mcp.md`.

## Key Patterns

- CLI: `clap` (derive for fixed commands, runtime-built for operations) + `clap_complete` (static and dynamic)
- Async runtime: `tokio`; logging: `tracing` to stderr (`-v`/`-vv`)
- Error handling: `anyhow` (CLI, server), `thiserror` (library crates); engine errors are `OpError` with an `ErrorKind`
- Exit codes: 0 ok · 1 unexpected · 2 usage · 3 auth expired · 4 not found/ambiguous · 5 throttled · 6 denied by policy; `--json` errors emit an envelope on stderr (`exitcode.rs`)
- Every Graph call goes through `send_with_retry` (429/503/504 with Retry-After); bulk changes use `$batch`; Gmail calls retry the same way
- AI: optional, through Ailloy (`eval` capability). Locally `pidge ai config` (`~/.config/ailloy/config.yaml`, shared with `rigg`, `mdeck`, `cosq`); hosted, the user's own key entered on `/ai` (fixed-endpoint providers only)
- Gmail needs a Google OAuth client brought by whoever runs pidge (`docs/gmail.md`)
- Update checker: background task, cached in the cache dir, skip with `PIDGE_NO_UPDATE_CHECK=1`
- File locations: `pidge_core::paths` is the only place that decides. Linux and macOS share the XDG
  layout, same as Ailloy: config (`config.yaml`, `prefs.json`, file-stored `tokens/`, MCP `mcp/`) in
  `${XDG_CONFIG_HOME:-~/.config}/pidge`, caches (`index.db`, update check) in
  `${XDG_CACHE_HOME:-~/.cache}/pidge`; XDG variables count only when absolute. Windows
  uses `%APPDATA%\pidge` and `%LOCALAPPDATA%\pidge`. Never call `dirs::config_dir()` or
  `dirs::cache_dir()` directly (on macOS they are `~/Library/...`)

## Releasing

Use the `/release` skill (see `.claude/skills/release/SKILL.md`):

1. `/release patch` (or `minor`/`major`)
2. Skill bumps version, updates CHANGELOG, runs pre-flight checks, tags, pushes
3. Release workflow runs CI, then builds [auditable](https://github.com/rust-secure-code/cargo-auditable) binaries (Linux, macOS Intel+ARM, Windows) with a CycloneDX SBOM per target, creates GitHub Release, updates Homebrew tap (`mklab-se/homebrew-tap`), publishes `pidge-core` → `pidge-client` → `pidge-gmail` → `pidge-store` → `pidge-engine` → `pidge` to crates.io, publishes the MCP image, and deploys `pidge-mcp` to Azure: one tag ships the CLI and the server together

**Required GitHub secrets:**
- `CARGO_REGISTRY_TOKEN` (in `crates-io` environment)
- `HOMEBREW_TAP_TOKEN` (GitHub PAT with repo scope for `mklab-se/homebrew-tap`)

## Code Style

- Edition 2024, MSRV 1.89 (floor set by rusqlite 0.41; Ailloy 3 and keyring 4 need 1.88)
- `cargo clippy` with `-D warnings` (zero warnings policy)
- `cargo fmt` enforced in CI
- Building from source on Windows needs NASM and CMake on `PATH`: `aws-lc-rs` (reqwest's TLS crypto
  backend) compiles optimized assembly routines at build time. macOS and Linux need nothing extra.
  The release workflow's Windows leg installs NASM via `ilammy/setup-nasm@v1`; CMake and MSVC are
  already on the `windows-latest` image.

## Writing style

- No em-dashes (U+2014) anywhere: docs, comments, help text, error messages, test strings, commit
  messages. Use a comma; otherwise a colon, parentheses or a new sentence. In Rust code that truly
  needs the character at runtime, write the escape `\u{2014}`. CI enforces this (the
  "No em-dashes" step in `ci.yml`).

## Dependency Policy

We keep this tool's dependencies at their latest compatible versions, not just the versions that
happen to still compile. Staying current is the default, not something we get to eventually;
letting dependencies drift is how technical debt accumulates unnoticed until a security advisory or
a forced breaking upgrade makes it urgent. When a newer major is available and there's no concrete,
documented reason not to take it (see any `# Stays on ...` comments in `Cargo.toml` for the current
exceptions and why), take it during the next maintenance round rather than deferring it. The
cross-repo `maintaining-rust-tools` skill drives this for the whole fleet (ailloy + cosq + deemer +
mdeck + pidge + rigg + rusty-tmpl).

## Token storage

- Tokens default to the OS keychain (`Keychain` variant; `keyring` 4 in `v1` mode: macOS Keychain, Windows Credential Manager, Secret Service on Linux). New sign-ins can opt into a plaintext file at `${XDG_CONFIG_HOME:-~/.config}/pidge/tokens/<email>.json` (mode 0600 on Unix) with `pidge account add --store=file`. Useful when keychain prompts are friction during development.
- `pidge account migrate-storage <email> --to <keychain|file>` moves an existing account's tokens between backends without forcing a re-login.
- The chosen backend is recorded on the account in `config.yaml` (`storage:` field); `AuthClient::get_valid_token` reads it on every call so callers don't need to know which backend was used.
- **Never commit token files.** `.gitignore` has belt-and-suspenders entries for `**/tokens/*.json` and `crates/pidge-core/tests/fixtures/raw/`.

## Workflow: when a real e-mail renders badly

The HTML renderer (`render_html` in `crates/pidge-core/src/render.rs`) is exercised by snapshot tests against anonymized fixtures of real-world e-mails. **Don't fix rendering bugs against a live mailbox; convert the bad e-mail into a fixture first.** That way the regression is caught forever and we never need a live token to repro.

The loop:

1. **Capture the raw HTML.** Find the message (`pidge mail find ...`), take its id (`m12`) and run:
   ```bash
   cargo run -- debug raw-html m12 > crates/pidge-core/tests/fixtures/raw/<name>.html
   ```
   `tests/fixtures/raw/` is gitignored: these files contain real user data and must never be committed.

2. **Anonymize it.** Produce `crates/pidge-core/tests/fixtures/<name>.html` from the raw file:
   - Preserve every HTML tag and attribute (`<table>`, `<tr>`, `<td>`, `<a>`, `<img>`, all `style=`, `class=`, etc.); the renderer's behavior depends on structure.
   - Replace every piece of human-readable prose with Lorem Ipsum of comparable length. Preserve trailing punctuation. For non-English originals, sprinkle in a few accented characters (`ö`, `å`, `é`, …) so the test still exercises non-ASCII paths.
   - Replace personal identifiers (recipient name, e-mail, bio) with `Jane Doe` / `jane.doe@example.com` etc.
   - Replace **every** URL with an `example.com` equivalent. Keep the URL shape (query string structure, length, multiple parameters) but turn each opaque token into a Lorem-style placeholder. Tracking pixels stay (the renderer's job is to suppress them).
   - Verify with `grep -c '<table' raw/<name>.html` and the anonymized version that tag counts match exactly.

3. **Add a snapshot test.** In `crates/pidge-core/tests/render_snapshots.rs`, add an `include_str!` constant for the new fixture (and to `ALL`) and a `render_html_<name>_matches_snapshot` test that calls `render_html_body` + `osc8_to_visible` + `assert_snapshot` with a new `tests/fixtures/<name>.expected.txt` path.

4. **Generate the snapshot, review, commit.** Run `UPDATE_SNAPSHOTS=1 cargo test -p pidge-core --test render_snapshots` to write the initial snapshot, **read it carefully** to confirm the rendering is acceptable, then commit. If it isn't acceptable, iterate on `render_html` until the snapshot looks right and re-run with `UPDATE_SNAPSHOTS=1` to accept.

5. **Re-run without the env var** to confirm the test is stable: `cargo test -p pidge-core --test render_snapshots`.

Snapshots use the visible form `[link=URL]visible text[/link]` for OSC 8 escapes so diffs stay human-readable. The structural-invariant tests (`render_html_emits_no_raw_html_tags`, `render_html_collapses_blank_runs`, `render_html_strips_tracking_pixel_chars`, `render_html_wraps_anchors_with_osc8`, `render_html_suppresses_image_alt_text`) run against every fixture in `ALL`.

## Design Docs

Specs and implementation plans live under `docs/superpowers/`:

- `docs/superpowers/specs/`: design documents
- `docs/superpowers/plans/`: implementation plans
