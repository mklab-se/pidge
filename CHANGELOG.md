# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.2.0] - 2026-05-14

### Added

- Initial repository scaffold: Cargo workspace with single-crate layout
- CLI skeleton with `clap` derive macros, ASCII banner, and version command
- Async runtime with `tokio`
- Logging via `tracing` with `-v`/`-vv` verbosity levels
- Colored output via `colored` (respects `--no-color`)
- AI integration via `ailloy` v0.7: `pidge ai status|test|enable|disable|config`
- AI agent skill emitter: `pidge ai skill --emit` and `--reference`
- Shell completions: static (`pidge completion <shell>`) and dynamic (`COMPLETE=<shell> pidge`)
- Background update checker (cached 24h, opt-out via `PIDGE_NO_UPDATE_CHECK`)
- CI/CD pipeline: GitHub Actions for build/test/lint, release workflow for cross-platform binaries, Homebrew tap, and crates.io publishing
- Workspace split into `pidge`, `pidge-core`, `pidge-client`
- OAuth 2.0 device code sign-in for Microsoft 365 and personal Microsoft accounts (`pidge auth login`)
- Multi-account support: `pidge auth list`, `pidge auth status`, `pidge auth logout`, `pidge auth default --send/--calendar`
- Tokens stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux libsecret)
- `pidge inbox list` — list messages across all signed-in accounts, filterable by `--account`, `--unread`, `-n <limit>`, with both rich (default) and `-c`/`--compact` text rendering plus global `--json` output
- One-time setup script `scripts/register-pidge-app.sh` for registering the pidge app in Entra
- Global `--json` flag honored by `pidge inbox list`, `pidge auth list`, `pidge auth status`
- `pidge inbox list` shows a stable 8-char short hash ID per message; cached at `~/.cache/pidge/messages.json` for substring lookup by future `pidge inbox show`
- `pidge inbox list` rich layout: subject + 2-line preview, bold+magenta for unread, cyan for read; `--compact`/`-c` for the one-row-per-message style
- URLs in subject and preview text are OSC 8 hyperlinks (clickable in modern terminals)
- Cleaner table style — horizontal line under header only, no vertical borders
- `pidge inbox show <fragment>` — substring-lookup a message by its 8-char short hash and display headers, body (HTML rendered via `html2text`), and attachment list
- `pidge inbox show --mark-read` / `-r` to mark the message as read on the server after rendering
- `pidge inbox show --show-images` to force inline image rendering for one invocation regardless of trust list
- `pidge trust list/add/remove` — manage the trusted-senders list; inline images auto-render for trusted senders in image-capable terminals (Ghostty, Kitty, iTerm2) via the `viuer` crate
- Trusted senders stored at `trusted_senders:` in `~/.config/pidge/config.yaml` (case-insensitive matching)
- HTML body rendering rewritten on top of `html2text` rich + `raw_mode`: layout tables flatten to paragraphs, every `<a href>` span is wrapped inline with an OSC 8 hyperlink, `<img>` alt-text and tracking-pixel zero-width chars are suppressed, blank-line runs collapse to at most two
- Link text inside the OSC 8 wrap is now styled with ANSI underline + cyan so it's identifiable at a glance without hovering (respects `--no-color` / `NO_COLOR`)
- Subject value in `pidge inbox show` header rendered as bold bright-yellow for at-a-glance scanning
- Opt-in file-based token storage: `pidge auth login --store=file` writes refresh tokens to `~/.config/pidge/tokens/<email>.json` (mode 0600 on Unix) instead of the OS keychain. Useful for headless or repeated-build scenarios where keychain prompts are friction; the keychain remains the default and more secure backend.
- `pidge auth migrate-storage <email> --to <keychain|file>` moves an existing account's credentials between backends without forcing a re-login
- Snapshot-based unit tests for HTML body rendering: anonymized fixtures of real-world marketing emails (LinkedIn jobs digest, SpeedLedger newsletter) live in `crates/pidge/tests/fixtures/`. Five structural-invariant assertions plus byte-level snapshot diffs catch rendering regressions without hitting Microsoft Graph. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p pidge render_html_`.
- Hidden `--raw-html` debug flag on `pidge inbox show` to capture an e-mail's HTML body verbatim for fixture-building when reporting bad rendering
