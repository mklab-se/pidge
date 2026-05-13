# pidge foundation — repository scaffold

**Status:** Approved 2026-05-13
**Goal:** Establish the initial `pidge` repository with the same shape, AI integration, and release pipeline as the sibling MKLab CLIs (`ailloy`, `rigg`, `mdeck`, `cosq`), shipping `0.1.0` to crates.io and Homebrew with **no email/calendar feature commands yet**.

## Non-goals

- No mail/calendar provider integration (Gmail, Microsoft Graph, IMAP, etc.).
- No persistent config schema beyond what `ailloy` already provides for AI nodes.
- No MCP server. Can be added later (see `rigg` for the pattern).
- No `init`, `auth`, or domain commands. Only the cross-cutting scaffolding commands ship in this release.

## Repository layout

```
pidge/
├── .claude/skills/release/SKILL.md
├── .github/workflows/{ci,release}.yml
├── .gitignore                    (extend current rust template)
├── CHANGELOG.md                  ([Unreleased] only at first commit)
├── CLAUDE.md                     (project overview for AI agents)
├── CONTRIBUTING.md               (mirror cosq)
├── INSTALL.md                    (brew / cargo install / cargo binstall)
├── LICENSE                       (existing MIT, keep)
├── README.md                     (replace placeholder)
├── Cargo.toml                    (workspace root)
├── Cargo.lock                    (committed)
├── docs/superpowers/specs/       (design docs live here)
└── crates/pidge/
    ├── Cargo.toml
    ├── doc/ai-reference.md       (placeholder for `pidge ai skill --reference`)
    └── src/
        ├── main.rs
        ├── cli.rs
        ├── banner.rs
        ├── update.rs
        └── commands/
            ├── mod.rs
            ├── ai.rs
            ├── completion.rs
            └── skill.rs
```

## Cargo workspace

### Root `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/pidge"]

[workspace.package]
version = "0.1.0"
edition = "2024"
authors = ["Kristofer Liljeblad <kristofer@mklab.se>"]
license = "MIT"
repository = "https://github.com/mklab-se/pidge"
rust-version = "1.85"

[workspace.dependencies]
# CLI
clap = { version = "4.5", features = ["derive", "env", "wrap_help"] }
clap_complete = { version = "4.5", features = ["unstable-dynamic"] }

# Async
tokio = { version = "1.40", features = ["full"] }

# HTTP (for update checker)
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls-native-roots"] }

# Errors
anyhow = "1.0"
thiserror = "2.0"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = { version = "1.0", features = ["preserve_order"] }

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Terminal
colored = "2.1"

# Versioning & time
semver = "1.0"
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }

# Directories
dirs = "6.0"

# AI integration
ailloy = { version = "0.7", default-features = false, features = ["config-tui"] }
```

### `crates/pidge/Cargo.toml`

```toml
[package]
name = "pidge"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "A fast CLI for e-mail and calendar"
readme = "../../README.md"
keywords = ["email", "calendar", "inbox", "cli", "productivity"]
categories = ["command-line-utilities", "email"]

[[bin]]
name = "pidge"
path = "src/main.rs"

[dependencies]
ailloy.workspace = true
clap.workspace = true
clap_complete.workspace = true
tokio.workspace = true
anyhow.workspace = true
colored.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
semver.workspace = true
chrono.workspace = true
dirs.workspace = true

[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/pidge-v{ version }-{ target }.{ archive-format }"
bin-dir = "pidge{ binary-ext }"
pkg-fmt = "tgz"

[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]
pkg-fmt = "zip"
```

**Crate-name note:** User has confirmed `pidge` is available on crates.io. If reservation fails at publish time, project halts and is renamed — there is no fallback name in this design.

## CLI surface (foundation)

```
pidge ai                    # show AI status (alias for `pidge ai status`)
pidge ai test [<message>]
pidge ai enable
pidge ai disable
pidge ai config             # interactive ailloy config TUI
pidge ai status
pidge ai skill              # human-readable setup guide
pidge ai skill --emit       # emit Claude Code skill markdown
pidge ai skill --reference  # emit full reference doc

pidge completion <bash|zsh|fish|powershell>
pidge version
```

**Global flags** (mirroring siblings): `-v` / `--verbose` (count, `-v` → debug, `-vv` → trace), `-q` / `--quiet`, `--no-color`.

**Behavior when invoked with no subcommand:** print help (matches `cosq`).

## Module responsibilities

### `main.rs`
- `clap_complete::CompleteEnv::with_factory(Cli::command).complete()` first — handles `COMPLETE=<shell> pidge` dynamic completions.
- Parse `Cli` with `clap`.
- Initialize `tracing_subscriber` with filter derived from `verbose`/`quiet`: `pidge=info` (default), `pidge=debug` (-v), `pidge=trace` (-vv), `error` (--quiet).
- Disable colors if `--no-color` is set: `colored::control::set_override(false)`.
- Spawn background update check unless `--quiet` or `PIDGE_NO_UPDATE_CHECK` is set in env.
- Run the command, await the update handle, return the result.

### `cli.rs`
- `#[derive(Parser)]` struct `Cli` with global flags and `#[command(subcommand)] command: Option<Commands>`.
- `Commands` enum: `Ai { command: Option<AiCommands> }`, `Completion { shell: Shell }`, `Version`.
- `AiCommands` enum (mirrors cosq exactly): `Test { message: Option<String> }`, `Enable`, `Disable`, `Config`, `Status`, `Skill { emit: bool, reference: bool }`.
- `Shell` enum derived `ValueEnum`: `Bash`, `Zsh`, `Fish`, `Powershell`.
- `impl Cli { pub async fn run(self) -> Result<()> }` dispatches to `commands::*` modules. `None` (no subcommand) prints help.

### `commands/ai.rs`
- Identical structure to cosq's: every variant defers to `ailloy::config_tui::*` passing `"pidge"` as the tool name and `&["chat"]` as the capability slice.
- Exposes `pub fn is_ai_active() -> bool` calling `config_tui::is_ai_active("pidge")`. Not used by other commands yet, but ships so future features can branch on AI availability without restructuring.

### `commands/completion.rs`
- Uses `clap_complete::generate` with the shell mapped from the `Shell` enum, printing to stdout.

### `commands/skill.rs`
- Constant `REFERENCE_DOC: &str = include_str!("../../doc/ai-reference.md");`.
- `pub fn run(emit: bool, reference: bool)` matches cosq's logic:
  - No flags → print human-readable setup guide.
  - `--emit` → print Claude Code skill markdown to stdout.
  - `--reference` → print `REFERENCE_DOC`.
- The emitted skill body is a placeholder explicitly stating: *"pidge currently only ships AI configuration commands. Use `pidge ai status` to verify configuration. Feature commands for e-mail and calendar are coming in a future release."* Then a "Quick command reference" listing `pidge ai status`, `pidge ai test`, `pidge ai config`, `pidge completion <shell>`, `pidge version`.
- `doc/ai-reference.md` ships a short stub with the same message plus a pointer to the repo README. The file must exist or compilation fails (`include_str!`).

### `banner.rs`
- `const LOGO` is a 5-letter ASCII block "PIDGE" (same family as cosq's `COSQ` art — block-letter style with `█`/`╔╗╚╝═║` etc.).
- `print_banner()` prints the logo with `colored::Colorize::bold()`.
- `print_banner_with_version()` adds the subtitle line "A fast CLI for e-mail and calendar v{CARGO_PKG_VERSION}" in `dimmed()`.
- Unit tests assert the logo is non-empty and has 6 visible lines (mirrors cosq).

### `update.rs`
- Direct adaptation of cosq's `update.rs`, swapping every `cosq` literal for `pidge`:
  - `CRATE_NAME = "pidge"`
  - Cache path: `dirs::cache_dir()/pidge/update-check.json`
  - User-Agent: `pidge/{CARGO_PKG_VERSION}`
  - Install-method detection: `brew upgrade pidge` (when `homebrew`/`Cellar`/`linuxbrew` in current_exe path), else `cargo binstall pidge` if `which cargo-binstall` succeeds, else `cargo install pidge`.
  - 24h cache TTL, prints "A new version of pidge is available …" to stderr.

## `.claude/skills/release/SKILL.md`

Direct adaptation of cosq's release skill. The differences from cosq's version:
- **Step 4 (Bump version numbers)** has only one bullet — update `version` in `[workspace.package]`. No internal-crate dependency bumps because the workspace has a single member.
- **Step 5 (Update documentation)** lists `CHANGELOG.md`, `README.md`, `CLAUDE.md`, `INSTALL.md` and explicitly notes that the README's installation snippet (brew/cargo) doesn't need version edits — only the changelog gains a dated entry.
- Frontmatter, command name (`/release`), argument-hint (`<major|minor|patch>`), and all other steps are identical to cosq's.

## GitHub workflows

### `.github/workflows/ci.yml`
Direct copy of cosq's `ci.yml`. Four jobs on `push: [main]` and `pull_request: [main]`:
1. `check` — `cargo check --workspace`
2. `test` — `cargo test --workspace`
3. `clippy` — `cargo clippy --workspace -- -D warnings`
4. `format` — `cargo fmt --all -- --check`

All use `actions/checkout@v5`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`.

### `.github/workflows/release.yml`
Direct copy of cosq's `release.yml`, with the following adjustments:
1. Every `cosq` literal → `pidge` (archive names, binary names, formula class name, formula filename in homebrew-tap).
2. The `crates-io` job has a **single** publish step — `cargo publish -p pidge` — with no inter-crate `sleep`s. The `cosq-core` and `cosq-client` pre-steps are removed entirely.
3. The `homebrew` job retains the "create-if-missing" branch (cosq's workflow already has this) so the first `v*` tag automatically creates `homebrew-tap/Formula/pidge.rb`.

Jobs (in order, after the trigger `push: tags: ["v*"]`):
- `ci` — same four checks as `ci.yml`.
- `build` — matrix over `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`. Produces `pidge-v{TAG}-{TARGET}.{tar.gz|zip}` artifacts.
- `github-release` — `softprops/action-gh-release@v2` with `generate_release_notes: true`.
- `homebrew` — needs `github-release`. Downloads the three Unix tarballs, computes SHA256, generates `Formula/pidge.rb`, PUTs it to `mklab-se/homebrew-tap` via `gh api` (using `HOMEBREW_TAP_TOKEN`).
- `crates-io` — needs `ci`. Runs `cargo publish -p pidge` with `CARGO_REGISTRY_TOKEN` from the `crates-io` environment.

### Required repository secrets / environments
- **Secret `HOMEBREW_TAP_TOKEN`** (repo-level, `pidge` repo): PAT with `repo` scope for `mklab-se/homebrew-tap`. Reuse the token already provisioned for cosq/rigg/mdeck.
- **Environment `crates-io`** (repo-level): contains secret `CARGO_REGISTRY_TOKEN` (crates.io API token). Same value as the other tools.

Both items must be configured before the first `v*` tag is pushed. Verifying their presence is part of the implementation handoff but not blocking the initial commit.

## Documentation files

### `README.md`
Replaces the existing 2-line placeholder. Mirrors cosq's README shape:
- Title + one-sentence tagline.
- Install section pointing to brew (`brew install mklab-se/tap/pidge`), `cargo install pidge`, and `cargo binstall pidge`.
- Quick-start showing `pidge ai config` / `pidge ai status` / `pidge --help`.
- Roadmap section explicitly stating that this is a foundation release with no feature commands yet.
- License & contributing pointers.

### `CHANGELOG.md`
Keep-a-Changelog format. At first commit:
```
## [Unreleased]

### Added
- Initial repository scaffold: CLI skeleton, AI integration via ailloy, update checker, shell completions, banner, release pipeline.
```
The `/release` skill renames `[Unreleased]` to `[0.1.0] - YYYY-MM-DD` on first cut.

### `CLAUDE.md`
Short orientation doc for AI agents: project purpose, where commands live, how AI integration works (delegates to ailloy), how release works (tag → workflow), pointers to the release skill and design specs.

### `CONTRIBUTING.md`
Direct copy of cosq's, with name swap.

### `INSTALL.md`
Direct copy of cosq's, with name swap and any Cosmos-specific examples removed.

### `.gitignore`
Extend the existing rust-template `.gitignore` with the editor/OS lines from cosq: `.idea/`, `.vscode/`, `*.swp`, `*.swo`, `*~`, `.DS_Store`, `Thumbs.db`, `*.log`, `coverage/`. Do **not** add a `pidge.yaml` ignore — there is no project config file yet.

## Versioning & release sequencing

1. **Initial commit** lands everything in this spec on `0.1.0`. No tag, no publish.
2. **First release** uses `/release patch` (or `minor`) to:
   - Rename `[Unreleased]` → `[0.1.0] - 2026-05-13` (or current date).
   - Run pre-flight checks (`fmt --check`, `clippy -D warnings`, `test --workspace`).
   - Tag `v0.1.0` and push.
3. The `release.yml` workflow then handles GitHub release, Homebrew tap, and crates.io publish in parallel.

## Out-of-scope for this spec (explicitly deferred)

- **Workspace decomposition**: when a real HTTP client or shared types appear, split into `pidge`, `pidge-core`, `pidge-client` (cosq pattern). Workspace already supports adding members via a single line.
- **MCP server**: add as a `pidge-mcp` feature later, mirroring `rigg`'s `mcp/` subtree and `mcp install` subcommand.
- **AI feature commands**: `ai test/enable/disable/config` ship now; domain commands that *use* AI (like `cosq queries generate`, `rigg explain`) come once feature commands exist.
- **Logo refinement**: the `PIDGE` ASCII art ships as a functional placeholder. A polished logo / banner can land in any future patch release.
