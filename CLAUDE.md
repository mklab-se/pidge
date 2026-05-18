# pidge

A fast CLI for e-mail and calendar. Foundation release — AI configuration and version commands only.

## Commands

```bash
cargo build                              # Build all crates
cargo test --workspace                   # Run all tests
cargo clippy --workspace -- -D warnings  # Lint (CI-enforced)
cargo fmt --all -- --check               # Format check (CI-enforced)
cargo run -- --help                      # Run the CLI
```

## Architecture

Rust workspace with three crates:

```
crates/
  pidge/                # CLI binary (package and binary name: pidge)
    src/
      main.rs           # Entry point
      cli.rs            # Clap CLI definitions
      banner.rs         # ASCII logo
      update.rs         # Crates.io update checker
      commands/         # `pidge ai`, `pidge account`, `pidge mail`, `pidge completion`, etc.
  pidge-core/           # Provider-agnostic types: Account, Config, Message
  pidge-client/         # Microsoft Graph client, OAuth flows, keychain token storage
    src/
      auth/             # Device code flow, refresh, JWT, keychain
      graph/            # Graph API endpoints (currently /me, inbox)
```

- Workspace root `Cargo.toml` defines shared dependencies and version
- `pidge-core` has no HTTP or auth code — it's safe to depend on from any consumer
- `pidge-client` knows nothing about clap or terminal output

## Key Patterns

- CLI built with `clap` derive macros + `clap_complete` for shell completions (static and dynamic)
- Async runtime: `tokio`
- Logging: `tracing` + `tracing-subscriber` with `-v`/`-vv` verbosity levels
- Colored output via `colored` crate (respects `--no-color`)
- Error handling: `anyhow` (CLI), `thiserror` (library crates: `pidge-core`, `pidge-client`)
- AI integration: delegates entirely to `ailloy::config_tui` with tool name `"pidge"` and capability slice `&["chat"]`. Config lives at `~/.config/ailloy/config.yaml`, shared with `rigg`, `mdeck`, `cosq`.
- Update checker: background task, cached at `~/.cache/pidge/`, skip with `PIDGE_NO_UPDATE_CHECK=1`

## Releasing

Use the `/release` slash command (see `.claude/commands/release.md`):

1. `/release patch` (or `minor`/`major`)
2. Skill bumps version, updates CHANGELOG, runs pre-flight checks, tags, pushes
3. Release workflow builds binaries (Linux, macOS Intel+ARM, Windows), creates GitHub Release, updates Homebrew tap (`mklab-se/homebrew-tap`), publishes to crates.io

**Required GitHub secrets:**
- `CARGO_REGISTRY_TOKEN` (in `crates-io` environment)
- `HOMEBREW_TAP_TOKEN` (GitHub PAT with repo scope for `mklab-se/homebrew-tap`)

## Code Style

- Edition 2024, MSRV 1.85
- `cargo clippy` with `-D warnings` (zero warnings policy)
- `cargo fmt` enforced in CI

## Token storage

- Tokens default to the OS keychain (`Keychain` variant). New sign-ins can opt into a plaintext file at `~/.config/pidge/tokens/<email>.json` (mode 0600 on Unix) with `pidge account add --store=file`. Useful when keychain prompts are friction during development.
- `pidge account migrate-storage <email> --to <keychain|file>` moves an existing account's tokens between backends without forcing a re-login.
- The chosen backend is recorded on the account in `config.yaml` (`storage:` field); `AuthClient::get_valid_token` reads it on every call so callers don't need to know which backend was used.
- **Never commit token files.** `.gitignore` has belt-and-suspenders entries for `**/tokens/*.json` and `crates/pidge/tests/fixtures/raw/`.

## Workflow: when a real e-mail renders badly

The HTML renderer (`render_html_body` in `commands/mail_show.rs`) is exercised by snapshot tests against anonymized fixtures of real-world e-mails. **Don't fix rendering bugs against a live mailbox — convert the bad e-mail into a fixture first.** That way the regression is caught forever and we never need a live token to repro.

The loop:

1. **Capture the raw HTML.** From the bad message's row in `pidge mail`, copy its short hash and run:
   ```bash
   cargo run -- mail show <fragment> --raw-html > crates/pidge/tests/fixtures/raw/<name>.html
   ```
   `tests/fixtures/raw/` is gitignored — these files contain real user data and must never be committed.

2. **Anonymize it.** Produce `crates/pidge/tests/fixtures/<name>.html` from the raw file:
   - Preserve every HTML tag and attribute (`<table>`, `<tr>`, `<td>`, `<a>`, `<img>`, all `style=`, `class=`, etc.) — the renderer's behavior depends on structure.
   - Replace every piece of human-readable prose with Lorem Ipsum of comparable length. Preserve trailing punctuation. For non-English originals, sprinkle in a few accented characters (`ö`, `å`, `é`, …) so the test still exercises non-ASCII paths.
   - Replace personal identifiers (recipient name, e-mail, bio) with `Jane Doe` / `jane.doe@example.com` etc.
   - Replace **every** URL with an `example.com` equivalent. Keep the URL shape (query string structure, length, multiple parameters) but turn each opaque token into a Lorem-style placeholder. Tracking pixels stay (the renderer's job is to suppress them).
   - Verify with `grep -c '<table' raw/<name>.html` and the anonymized version that tag counts match exactly.

3. **Add a snapshot test.** In `commands/inbox_show.rs::tests`, add an `include_str!` constant for the new fixture and a `render_html_<name>_matches_snapshot` test that calls `render_html_body` + `osc8_to_visible` + `assert_snapshot` with a new `tests/fixtures/<name>.expected.txt` path.

4. **Generate the snapshot, review, commit.** Run `UPDATE_SNAPSHOTS=1 cargo test -p pidge render_html_` to write the initial snapshot, **read it carefully** to confirm the rendering is acceptable, then commit. If it isn't acceptable, iterate on `render_html_body` until the snapshot looks right and re-run with `UPDATE_SNAPSHOTS=1` to accept.

5. **Re-run without the env var** to confirm the test is stable: `cargo test -p pidge render_html_`.

Snapshots use the visible form `[link=URL]visible text[/link]` for OSC 8 escapes so diffs stay human-readable. The structural-invariant tests (`render_html_emits_no_raw_html_tags`, `render_html_collapses_blank_runs`, `render_html_strips_tracking_pixel_chars`, `render_html_wraps_anchors_with_osc8`, `render_html_suppresses_image_alt_text`) run against every fixture automatically — no extra wiring needed.

## Design Docs

Specs and implementation plans live under `docs/superpowers/`:

- `docs/superpowers/specs/` — design documents
- `docs/superpowers/plans/` — implementation plans
