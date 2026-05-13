# pidge Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scaffold the `pidge` repository — Cargo workspace, CLI skeleton with `ai` / `completion` / `version` commands wired to `ailloy`, full release pipeline (GitHub Actions → crates.io + mklab-se/homebrew-tap), and the `/release` skill — shipping as `0.1.0` with no feature commands.

**Architecture:** Single-crate Cargo workspace (`crates/pidge`). The CLI delegates AI configuration to `ailloy::config_tui`, runs an async update checker against crates.io on every invocation, and produces static shell completions via `clap_complete`. The release workflow is triggered by `v*` tags and parallelises GitHub binary release, Homebrew formula update (auto-created on first run), and crates.io publish.

**Tech Stack:** Rust 2024 edition (MSRV 1.85), `clap` v4 + `clap_complete`, `tokio`, `reqwest` (rustls), `tracing`, `colored`, `serde`, `semver`, `chrono`, `dirs`, `ailloy = "0.7"` with `config-tui` feature. GitHub Actions for CI/release.

**Reference source:** When a task says "copy from cosq", read the file at `/Users/kristofer/repos/mklab-se/cosq/<path>` and swap every `cosq` literal for `pidge` (preserving case: `Cosq` → `Pidge`, `COSQ` → `PIDGE`).

**Working directory:** `/Users/kristofer/repos/mklab-se/pidge`

---

## File overview

| Path | Purpose |
|---|---|
| `Cargo.toml` | Workspace root: members, workspace.package, workspace.dependencies |
| `crates/pidge/Cargo.toml` | Binary crate metadata, dependencies, binstall config |
| `crates/pidge/src/main.rs` | Entry point: tokio runtime, tracing init, dynamic-completion env shim, background update check, dispatch |
| `crates/pidge/src/cli.rs` | `clap` `Cli` struct, `Commands`/`AiCommands`/`Shell` enums, `Cli::run` dispatcher |
| `crates/pidge/src/banner.rs` | "PIDGE" ASCII art + version subtitle |
| `crates/pidge/src/update.rs` | crates.io polling, 24h cache, brew/binstall/cargo detection |
| `crates/pidge/src/commands/mod.rs` | Module exports |
| `crates/pidge/src/commands/ai.rs` | `pidge ai *` — thin wrapper over `ailloy::config_tui` |
| `crates/pidge/src/commands/completion.rs` | `pidge completion <shell>` — static script generation |
| `crates/pidge/src/commands/skill.rs` | `pidge ai skill [--emit\|--reference]` |
| `crates/pidge/doc/ai-reference.md` | Reference text included by `include_str!` |
| `.gitignore` | Rust + editor/OS ignores |
| `README.md` | Project overview, install, quick-start, roadmap |
| `CHANGELOG.md` | Keep-a-Changelog with `[Unreleased]` only |
| `INSTALL.md` | brew / binary / cargo / binstall / completions |
| `CONTRIBUTING.md` | Contributor guide |
| `CLAUDE.md` | Agent orientation |
| `.github/workflows/ci.yml` | check / test / clippy / format on push+PR |
| `.github/workflows/release.yml` | tag-triggered build → GitHub release → homebrew + crates.io |
| `.claude/skills/release/SKILL.md` | `/release` skill |

---

## Task 1: Workspace bootstrap — root `Cargo.toml`

**Files:**
- Create: `Cargo.toml`

- [ ] **Step 1: Verify the working directory**

Run: `pwd && ls`
Expected output includes `pidge` as the current directory and shows `LICENSE`, `README.md`, `.gitignore`.

- [ ] **Step 2: Write the root workspace `Cargo.toml`**

Create `/Users/kristofer/repos/mklab-se/pidge/Cargo.toml`:

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

- [ ] **Step 3: Defer build verification**

The workspace member `crates/pidge` doesn't exist yet. We'll verify with `cargo build` after Task 2 creates the binary crate. Do **not** run `cargo build` here — it will fail.

- [ ] **Step 4: No commit yet**

We'll commit after Task 2 produces a runnable binary so the first commit is a self-contained "Hello pidge".

---

## Task 2: Crate skeleton — `crates/pidge/Cargo.toml` and hello-world `main.rs`

**Files:**
- Create: `crates/pidge/Cargo.toml`
- Create: `crates/pidge/src/main.rs`

- [ ] **Step 1: Create the crate directory**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge/src`

- [ ] **Step 2: Write `crates/pidge/Cargo.toml`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/Cargo.toml`:

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

- [ ] **Step 3: Write a hello-world `main.rs`**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/main.rs`:

```rust
//! pidge — A fast CLI for e-mail and calendar

fn main() {
    println!("pidge v{}", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build`
Expected: build succeeds, downloads ~50–100 crates on first run, finishes with `Finished \`dev\`` and no warnings.

- [ ] **Step 5: Smoke-test the binary**

Run: `cargo run -q`
Expected output (exact): `pidge v0.1.0`

- [ ] **Step 6: Commit the bootstrap**

```bash
git add Cargo.toml Cargo.lock crates/pidge/Cargo.toml crates/pidge/src/main.rs
git commit -m "Bootstrap Cargo workspace and pidge binary crate"
```

---

## Task 3: Banner module — `crates/pidge/src/banner.rs`

**Files:**
- Create: `crates/pidge/src/banner.rs`
- Modify: `crates/pidge/src/main.rs` (add `mod banner;` only)

- [ ] **Step 1: Write the banner module (with unit tests)**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/banner.rs`:

```rust
//! ASCII art banner for pidge CLI

use colored::Colorize;

const LOGO: &str = r#"
██████╗ ██╗██████╗  ██████╗ ███████╗
██╔══██╗██║██╔══██╗██╔════╝ ██╔════╝
██████╔╝██║██║  ██║██║  ███╗█████╗
██╔═══╝ ██║██║  ██║██║   ██║██╔══╝
██║     ██║██████╔╝╚██████╔╝███████╗
╚═╝     ╚═╝╚═════╝  ╚═════╝ ╚══════╝"#;

/// Print the pidge ASCII art banner.
pub fn print_banner() {
    for line in LOGO.lines() {
        println!("{}", line.bold());
    }
}

/// Print the banner with version and subtitle.
pub fn print_banner_with_version() {
    print_banner();
    println!(
        " {} {}",
        "A fast CLI for e-mail and calendar".dimmed(),
        format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_not_empty() {
        assert!(!LOGO.is_empty());
    }

    #[test]
    fn logo_has_six_visible_lines() {
        let lines: Vec<&str> = LOGO.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 6, "Logo should have 6 lines of block letters");
    }
}
```

- [ ] **Step 2: Declare the module in `main.rs`**

Edit `crates/pidge/src/main.rs` so the file reads exactly:

```rust
//! pidge — A fast CLI for e-mail and calendar

mod banner;

fn main() {
    banner::print_banner_with_version();
}
```

- [ ] **Step 3: Run the unit tests**

Run: `cargo test -p pidge banner`
Expected: 2 passed (`logo_is_not_empty`, `logo_has_six_visible_lines`), 0 failed.

- [ ] **Step 4: Visually verify the banner**

Run: `cargo run -q`
Expected: 6 lines of block letters spelling "PIDGE" in bold, followed by a dimmed subtitle `A fast CLI for e-mail and calendar v0.1.0`.

- [ ] **Step 5: Commit**

```bash
git add crates/pidge/src/banner.rs crates/pidge/src/main.rs
git commit -m "Add pidge ASCII banner with version subtitle"
```

---

## Task 4: Update checker — `crates/pidge/src/update.rs`

**Files:**
- Create: `crates/pidge/src/update.rs`

This is a name-swapped port of `/Users/kristofer/repos/mklab-se/cosq/crates/cosq/src/update.rs`. Read that file first so you can confirm parity if anything below is ambiguous.

- [ ] **Step 1: Write the update checker**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/update.rs`:

```rust
//! Version update checker
//!
//! Queries crates.io for the latest version of pidge, caches results for 24 hours,
//! and prints a notification if a newer version is available.

use std::io::Write;
use std::path::PathBuf;

use colored::Colorize;
use serde::{Deserialize, Serialize};
use tracing::debug;

const CRATE_NAME: &str = "pidge";
const CACHE_DURATION_HOURS: i64 = 24;

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCache {
    latest_version: String,
    checked_at: String,
}

#[derive(Debug, Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    krate: CrateInfo,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    max_stable_version: String,
}

fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("pidge").join("update-check.json"))
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let cache: UpdateCache = serde_json::from_str(&data).ok()?;

    let checked_at = chrono::DateTime::parse_from_rfc3339(&cache.checked_at).ok()?;
    let age = chrono::Utc::now() - checked_at.to_utc();
    if age.num_hours() >= CACHE_DURATION_HOURS {
        debug!("update cache expired");
        return None;
    }

    Some(cache)
}

fn write_cache(latest_version: &str) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = UpdateCache {
        latest_version: latest_version.to_string(),
        checked_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&path, json);
    }
}

async fn fetch_latest_version() -> Option<String> {
    let url = format!("https://crates.io/api/v1/crates/{CRATE_NAME}");
    let client = reqwest::Client::builder()
        .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;

    let resp: CratesIoResponse = client.get(&url).send().await.ok()?.json().await.ok()?;
    Some(resp.krate.max_stable_version)
}

fn detect_install_method() -> &'static str {
    if let Ok(exe) = std::env::current_exe() {
        let exe_str = exe.to_string_lossy();
        if exe_str.contains("homebrew")
            || exe_str.contains("Cellar")
            || exe_str.contains("linuxbrew")
        {
            return "brew upgrade pidge";
        }
    }

    if which_exists("cargo-binstall") {
        return "cargo binstall pidge";
    }

    "cargo install pidge"
}

fn which_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn print_update_notification(current: &semver::Version, latest: &semver::Version) {
    let update_cmd = detect_install_method();
    let _ = writeln!(
        std::io::stderr(),
        "\n{} {} → {} (update with: {})",
        "A new version of pidge is available:".yellow().bold(),
        current.to_string().dimmed(),
        latest.to_string().green().bold(),
        update_cmd.cyan(),
    );
}

/// Check for updates in the background. Returns a future that resolves
/// after checking and optionally printing a notification.
pub async fn check_for_updates() {
    let current_str = env!("CARGO_PKG_VERSION");
    let Ok(current) = semver::Version::parse(current_str) else {
        return;
    };

    let latest_str = if let Some(cache) = read_cache() {
        debug!(version = %cache.latest_version, "using cached version info");
        cache.latest_version
    } else {
        debug!("fetching latest version from crates.io");
        let Some(version) = fetch_latest_version().await else {
            debug!("failed to fetch latest version");
            return;
        };
        write_cache(&version);
        version
    };

    let Ok(latest) = semver::Version::parse(&latest_str) else {
        return;
    };

    if latest > current {
        print_update_notification(&current, &latest);
    } else {
        debug!(current = %current, latest = %latest, "pidge is up to date");
    }
}
```

- [ ] **Step 2: Verify the module compiles in isolation**

The module isn't referenced from `main.rs` yet — but `cargo build` only checks reachable items. Add a temporary compile-only check by running `cargo check -p pidge` with `mod update;` added to `main.rs` (we'll wire `check_for_updates()` properly in Task 11). Instead of that detour, defer the build verification to Task 11.

- [ ] **Step 3: No commit yet**

`update.rs` is dead code until Task 11 wires it. We'll bundle the commit with the CLI scaffolding so the first commit that mentions `update.rs` shows a callsite.

---

## Task 5: CLI definitions — `crates/pidge/src/cli.rs`

**Files:**
- Create: `crates/pidge/src/cli.rs`

- [ ] **Step 1: Write the CLI module**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/cli.rs`:

```rust
//! CLI argument definitions using clap

use anyhow::Result;
use clap::Parser;

/// A fast CLI for e-mail and calendar
#[derive(Parser)]
#[command(name = "pidge")]
#[command(author, version, about)]
#[command(long_about = "A fast CLI for e-mail and calendar.\n\n\
    Foundation release — AI configuration, shell completions, and version info only. \
    E-mail and calendar feature commands ship in future releases.")]
#[command(propagate_version = true)]
pub struct Cli {
    /// Increase output verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(clap::Subcommand)]
pub enum Commands {
    /// Manage AI features (shows status when run without a subcommand)
    Ai {
        #[command(subcommand)]
        command: Option<AiCommands>,
    },

    /// Generate shell completions
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Show version information
    Version,
}

#[derive(clap::Subcommand)]
pub enum AiCommands {
    /// Test AI integration by sending a message
    Test {
        /// Message to send (default: "Say hello in one sentence.")
        message: Option<String>,
    },
    /// Enable AI features for pidge
    Enable,
    /// Disable AI features for pidge
    Disable,
    /// Interactively configure AI provider and model settings
    Config,
    /// Show AI status (same as running `pidge ai` without a subcommand)
    Status,
    /// AI agent skill information — helps set up Claude Code skills for pidge
    Skill {
        /// Output the skill markdown content (ready to save as a skill file)
        #[arg(long)]
        emit: bool,

        /// Output detailed reference documentation for AI agents
        #[arg(long)]
        reference: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Some(Commands::Ai { command }) => crate::commands::ai::run(command).await,
            Some(Commands::Completion { shell }) => {
                crate::commands::completion::generate_completions(shell);
                Ok(())
            }
            Some(Commands::Version) => {
                crate::banner::print_banner_with_version();
                Ok(())
            }
            None => {
                use clap::CommandFactory;
                let mut cmd = Self::command();
                cmd.print_help()?;
                println!();
                Ok(())
            }
        }
    }
}
```

- [ ] **Step 2: Build will fail — that's expected**

`cli.rs` references `crate::commands::ai` and `crate::commands::completion`, which don't exist yet. We won't run `cargo build` until Task 10 wires every module. No action this step.

---

## Task 6: Commands module skeleton — `crates/pidge/src/commands/mod.rs`

**Files:**
- Create: `crates/pidge/src/commands/mod.rs`

- [ ] **Step 1: Create the commands directory and `mod.rs`**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/mod.rs`:

```rust
//! CLI command implementations

pub mod ai;
pub mod completion;
pub mod skill;
```

- [ ] **Step 2: No build verification yet**

`ai`, `completion`, `skill` are missing — build will fail until Tasks 7–9 create them.

---

## Task 7: AI subcommand — `crates/pidge/src/commands/ai.rs`

**Files:**
- Create: `crates/pidge/src/commands/ai.rs`

- [ ] **Step 1: Write the AI command module**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/ai.rs`:

```rust
//! AI feature management
//!
//! `pidge ai`         — show status
//! `pidge ai test`    — test AI connection
//! `pidge ai enable`  — enable AI for pidge
//! `pidge ai disable` — disable AI for pidge
//! `pidge ai config`  — interactive AI node configuration

use anyhow::Result;

use ailloy::config::Config;
use ailloy::config_tui;

use crate::cli::AiCommands;

pub async fn run(cmd: Option<AiCommands>) -> Result<()> {
    match cmd {
        None => config_tui::print_ai_status("pidge", &["chat"]),
        Some(AiCommands::Test { message }) => config_tui::run_test_chat("pidge", message).await,
        Some(AiCommands::Enable) => config_tui::enable_ai("pidge"),
        Some(AiCommands::Disable) => config_tui::disable_ai("pidge"),
        Some(AiCommands::Config) => {
            let mut config = Config::load_global()?;
            config_tui::run_interactive_config(&mut config, &["chat"]).await?;
            Ok(())
        }
        Some(AiCommands::Status) => config_tui::print_ai_status("pidge", &["chat"]),
        Some(AiCommands::Skill { emit, reference }) => {
            crate::commands::skill::run(emit, reference);
            Ok(())
        }
    }
}

/// Check if AI features are active (configured via ailloy + enabled for this tool).
#[allow(dead_code)]
pub fn is_ai_active() -> bool {
    config_tui::is_ai_active("pidge")
}
```

The `#[allow(dead_code)]` on `is_ai_active` is required because no feature command calls it yet; it ships now so future features don't have to add a public helper later.

---

## Task 8: Completion subcommand — `crates/pidge/src/commands/completion.rs`

**Files:**
- Create: `crates/pidge/src/commands/completion.rs`

- [ ] **Step 1: Write the completion module**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/completion.rs`:

```rust
//! Shell completion generation
//!
//! - Static (AOT): `pidge completion <shell>` generates a static completion script
//! - Dynamic: `source <(COMPLETE=<shell> pidge)` enables dynamic completions
//!   (handled in main.rs via clap_complete::CompleteEnv)

use std::io;

use clap::CommandFactory;
use clap_complete::generate;
use colored::Colorize;

use crate::cli::{Cli, Shell};

/// Generate shell completions and write them to stdout.
pub fn generate_completions(shell: Shell) {
    let shell_name = match shell {
        Shell::Bash => "bash",
        Shell::Zsh => "zsh",
        Shell::Fish => "fish",
        Shell::Powershell => "powershell",
    };

    let clap_shell = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
        Shell::Powershell => clap_complete::Shell::PowerShell,
    };

    let mut cmd = Cli::command();
    generate(clap_shell, &mut cmd, "pidge", &mut io::stdout());

    eprintln!();
    eprintln!(
        "{} For dynamic completions, use instead:",
        "Tip:".bold()
    );
    eprintln!(
        "  {}",
        format!("source <(COMPLETE={shell_name} pidge)").cyan()
    );
}
```

---

## Task 9: Skill subcommand and reference doc

**Files:**
- Create: `crates/pidge/doc/ai-reference.md`
- Create: `crates/pidge/src/commands/skill.rs`

- [ ] **Step 1: Create the `doc/` directory and reference markdown**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/crates/pidge/doc`

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/doc/ai-reference.md`:

```markdown
# pidge — AI agent reference

This is a foundation release. pidge currently ships only AI configuration, shell completion, and version commands. There are no e-mail or calendar feature commands yet.

## Today's command surface

- `pidge ai status` — show AI configuration status (from `~/.config/ailloy/config.yaml`)
- `pidge ai test [<message>]` — round-trip test to the configured AI provider
- `pidge ai enable` / `pidge ai disable` — toggle AI usage for pidge
- `pidge ai config` — interactive ailloy configuration TUI
- `pidge ai skill --emit` — print the Claude Code skill body
- `pidge ai skill --reference` — print this reference doc
- `pidge completion <bash|zsh|fish|powershell>` — emit static completion script
- `pidge version` — print banner + version

## Global flags

- `-v` / `-vv` — increase log verbosity (`debug` / `trace`)
- `-q` / `--quiet` — suppress non-essential output and update notifications
- `--no-color` — disable ANSI colors

## Configuration locations

- AI provider/model: `~/.config/ailloy/config.yaml` (managed via `pidge ai config`)
- Update-check cache: `${XDG_CACHE_HOME:-~/.cache}/pidge/update-check.json` (24h TTL)

## Environment variables

- `PIDGE_NO_UPDATE_CHECK` — when set, skip the background crates.io update check

## Roadmap

Feature commands for inbox interaction, message search, calendar events, and AI-driven workflows are planned. Check the repository README for status.
```

- [ ] **Step 2: Write the skill command module**

Create `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/commands/skill.rs`:

```rust
//! AI agent skill information for pidge
//!
//! Provides skill file content and reference documentation to help
//! AI coding agents (like Claude Code) work effectively with pidge.

const REFERENCE_DOC: &str = include_str!("../../doc/ai-reference.md");

/// Run the `pidge ai skill` command.
///
/// - No flags: print a human-readable setup guide.
/// - `--emit`: print a Claude Code skill markdown file to stdout.
/// - `--reference`: print comprehensive reference documentation.
pub fn run(emit: bool, reference: bool) {
    if emit {
        print_skill_file();
    } else if reference {
        print_reference();
    } else {
        print_guide();
    }
}

fn print_guide() {
    println!(
        r#"pidge AI Skill Setup
====================

pidge is a CLI for e-mail and calendar. This is a foundation release —
feature commands ship later. The emitted skill explains today's surface
and points the agent at the live reference doc.

To create the skill file, run:

  pidge ai skill --emit > ~/.claude/skills/pidge.md

Or ask your AI agent:

  "Use `pidge ai skill --emit` to set up a skill for pidge"

The skill instructs the AI agent to run `pidge ai skill --reference` at
runtime to fetch full documentation, so the agent always has up-to-date
command details without bloating the skill file itself.
"#
    );
}

fn print_skill_file() {
    print!(
        r#"---
name: pidge
description: A fast CLI for e-mail and calendar. Foundation release — AI configuration, shell completions, and version commands only.
---

# pidge — E-mail and Calendar CLI

pidge is in foundation phase. No e-mail or calendar feature commands ship yet.

## Before you start

Run this command to get full, up-to-date reference documentation:

```bash
pidge ai skill --reference
```

## Quick command reference

- `pidge ai status` — show AI configuration status
- `pidge ai test` — test the configured AI connection
- `pidge ai config` — interactive AI provider/model setup
- `pidge completion <shell>` — generate static shell completion script
- `pidge version` — print banner and version
"#
    );
}

fn print_reference() {
    print!("{REFERENCE_DOC}");
}
```

---

## Task 10: Final `main.rs` — wire CLI, banner, update, tracing

**Files:**
- Modify: `crates/pidge/src/main.rs`

- [ ] **Step 1: Replace `main.rs` with the production entry point**

Overwrite `/Users/kristofer/repos/mklab-se/pidge/crates/pidge/src/main.rs` so it reads exactly:

```rust
//! pidge — A fast CLI for e-mail and calendar

use anyhow::Result;
use clap::{CommandFactory, Parser};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod banner;
mod cli;
mod commands;
mod update;

use cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    // Handle dynamic shell completions (when invoked via COMPLETE=<shell> pidge)
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose > 0 {
        match cli.verbose {
            1 => "pidge=debug",
            _ => "pidge=trace",
        }
    } else if cli.quiet {
        "error"
    } else {
        "pidge=info"
    };

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).without_time())
        .with(EnvFilter::new(filter))
        .init();

    if cli.no_color {
        colored::control::set_override(false);
    }

    // Spawn background update check (skip in quiet mode or if disabled via env)
    let update_handle = if !cli.quiet && std::env::var("PIDGE_NO_UPDATE_CHECK").is_err() {
        Some(tokio::spawn(update::check_for_updates()))
    } else {
        None
    };

    let result = cli.run().await;

    if let Some(handle) = update_handle {
        let _ = handle.await;
    }

    result
}
```

- [ ] **Step 2: Build the full workspace**

Run: `cargo build`
Expected: clean build, no warnings, no errors. If clippy/dead-code warnings appear on `is_ai_active`, the `#[allow(dead_code)]` from Task 7 should be silencing them — if not, double-check that attribute is present.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --workspace`
Expected: 2 banner tests pass, 0 failed.

- [ ] **Step 4: Smoke-test every subcommand**

Run each command and confirm the expected output:

```bash
cargo run -q -- --help
```
Expected: top-level help with `ai`, `completion`, `version` subcommands listed and the long-about text "A fast CLI for e-mail and calendar.\n\nFoundation release — …".

```bash
cargo run -q -- version
```
Expected: PIDGE banner + subtitle, then a blank line.

```bash
cargo run -q -- --version
```
Expected: `pidge 0.1.0`.

```bash
cargo run -q -- completion bash | head -5
```
Expected: first lines of a bash completion script starting with `_pidge() {`. The tip ("For dynamic completions…") prints to **stderr**, so it shouldn't appear in the head'd stdout. The pipe will not error.

```bash
cargo run -q -- ai
```
Expected: ailloy prints AI status (likely "AI is not enabled for pidge" or similar) — no panic, no error.

```bash
cargo run -q -- ai skill
```
Expected: the human-readable "pidge AI Skill Setup" guide.

```bash
cargo run -q -- ai skill --emit
```
Expected: markdown starting with `---\nname: pidge\n…`.

```bash
cargo run -q -- ai skill --reference
```
Expected: the reference doc starting with `# pidge — AI agent reference`.

If any smoke test fails or produces unexpected output, fix before continuing.

- [ ] **Step 5: Lint and format**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace -- -D warnings`
Expected: both pass; if clippy flags anything, fix it inline.

- [ ] **Step 6: Commit the full CLI scaffold**

```bash
git add Cargo.lock crates/pidge/src crates/pidge/doc
git commit -m "Add pidge CLI skeleton: cli, commands, update checker, ai/completion/skill subcommands"
```

---

## Task 11: Extend `.gitignore`

**Files:**
- Modify: `.gitignore`

- [ ] **Step 1: Read the current `.gitignore`**

Run: `cat .gitignore`

You should see the rust-template ignores already present (target/, *.rs.bk, *.pdb, mutants.out*/).

- [ ] **Step 2: Append editor/OS ignores**

Append the following lines to `/Users/kristofer/repos/mklab-se/pidge/.gitignore` (after the existing content, keeping a blank line separator):

```
.idea/
.vscode/

*.swp
*.swo
*~

.DS_Store
Thumbs.db

*.log
coverage/
```

- [ ] **Step 3: Verify**

Run: `git status`
Expected: `.gitignore` shows as modified, no surprise file unstaging.

- [ ] **Step 4: Commit**

```bash
git add .gitignore
git commit -m "Extend .gitignore with editor and OS ignores"
```

---

## Task 12: README, CHANGELOG, INSTALL, CONTRIBUTING, CLAUDE.md

**Files:**
- Modify: `README.md` (overwrite)
- Create: `CHANGELOG.md`
- Create: `INSTALL.md`
- Create: `CONTRIBUTING.md`
- Create: `CLAUDE.md`

- [ ] **Step 1: Overwrite `README.md`**

Replace `/Users/kristofer/repos/mklab-se/pidge/README.md` with:

```markdown
<h1 align="center">pidge</h1>

<p align="center">
  A fast CLI for e-mail and calendar.
</p>

<p align="center">
  <a href="https://github.com/mklab-se/pidge/actions/workflows/ci.yml"><img src="https://github.com/mklab-se/pidge/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/pidge"><img src="https://img.shields.io/crates/v/pidge.svg" alt="crates.io"></a>
  <a href="https://github.com/mklab-se/pidge/releases/latest"><img src="https://img.shields.io/github/v/release/mklab-se/pidge" alt="GitHub Release"></a>
  <a href="https://github.com/mklab-se/homebrew-tap/blob/main/Formula/pidge.rb"><img src="https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmklab-se%2Fhomebrew-tap%2Fmain%2FFormula%2Fpidge.rb&search=%5Cd%2B%5C.%5Cd%2B%5C.%5Cd%2B&label=homebrew&prefix=v&color=orange" alt="Homebrew"></a>
  <a href="https://github.com/mklab-se/pidge/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/pidge.svg" alt="License"></a>
</p>

## Status

**Foundation release.** pidge currently ships AI configuration, shell completions, and version commands only. E-mail and calendar feature commands are coming in future releases.

## Quick Start

```bash
# Install (macOS / Linux)
brew install mklab-se/tap/pidge

# Or via cargo
cargo install pidge

# Configure your AI provider (uses ailloy)
pidge ai config

# Check status
pidge ai status

# See what's available today
pidge --help
```

See [INSTALL.md](INSTALL.md) for all installation methods and shell completions.

## AI Integration

pidge delegates AI configuration to [ailloy](https://github.com/mklab-se/ailloy), a unified AI provider library shared by the MKLab CLI suite (`rigg`, `mdeck`, `cosq`, `pidge`). Configure your provider once with `pidge ai config` and it's available to every ailloy-based tool.

## Development

```bash
cargo build              # Build
cargo test --workspace   # Run tests
cargo clippy             # Lint
cargo fmt                # Format
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contributor guide.

## License

MIT — see [LICENSE](LICENSE).
```

- [ ] **Step 2: Create `CHANGELOG.md`**

Create `/Users/kristofer/repos/mklab-se/pidge/CHANGELOG.md`:

```markdown
# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

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
```

- [ ] **Step 3: Create `INSTALL.md`**

Create `/Users/kristofer/repos/mklab-se/pidge/INSTALL.md`:

```markdown
# Installing pidge

## Homebrew (macOS / Linux)

```bash
brew install mklab-se/tap/pidge
```

## Pre-built Binaries

Download the latest binary for your platform from [GitHub Releases](https://github.com/mklab-se/pidge/releases/latest):

| Platform | Archive |
|---|---|
| macOS (Apple Silicon) | `pidge-v*-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `pidge-v*-x86_64-apple-darwin.tar.gz` |
| Linux (x86_64) | `pidge-v*-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) | `pidge-v*-x86_64-pc-windows-msvc.zip` |

Extract and move the binary to a directory on your `PATH`:

```bash
# macOS / Linux
tar xzf pidge-v*-*.tar.gz
sudo mv pidge /usr/local/bin/
```

## cargo install

Compile from source via crates.io (requires Rust 1.85+):

```bash
cargo install pidge
```

## Build from Source

```bash
git clone https://github.com/mklab-se/pidge.git
cd pidge
cargo build --release
```

The binary is at `target/release/pidge`. Requires Rust 1.85 or later.

## cargo binstall

If you already have [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) installed, it can download a pre-built binary from GitHub Releases instead of compiling:

```bash
cargo binstall pidge
```

## Shell Completions

### Dynamic Completions (recommended)

Dynamic completions adapt to whatever flags and subcommands the current binary supports. Add to your shell config:

**Bash** — add to `~/.bashrc`:
```bash
source <(COMPLETE=bash pidge)
```

**Zsh** — add to `~/.zshrc`:
```bash
source <(COMPLETE=zsh pidge)
```

**Fish** — add to `~/.config/fish/config.fish`:
```bash
source (COMPLETE=fish pidge | psub)
```

### Static Completions

If you prefer static completions, use `pidge completion <shell>`:

**Bash** — add to `~/.bashrc`:
```bash
source <(pidge completion bash)
```

**Zsh** — add to `~/.zshrc`:
```bash
source <(pidge completion zsh)
```

**Fish** — save to completions directory:
```bash
pidge completion fish > ~/.config/fish/completions/pidge.fish
```

**PowerShell** — add to profile:
```powershell
pidge completion powershell >> $PROFILE
```

## Verify Installation

```bash
pidge --version
```
```

- [ ] **Step 4: Create `CONTRIBUTING.md`**

Create `/Users/kristofer/repos/mklab-se/pidge/CONTRIBUTING.md`:

```markdown
# Contributing to pidge

Thank you for considering contributing to pidge! This guide will help you get started.

## Getting Started

1. Fork the repository and clone your fork
2. Install Rust 1.85+ via [rustup](https://rustup.rs/)
3. Build the project: `cargo build`
4. Run tests: `cargo test`

## Development Workflow

### Project Structure

```
crates/
  pidge/       # CLI binary and command implementations
```

### Running Tests

```bash
cargo test                # All tests
cargo test -p pidge       # Single crate
cargo test test_name      # Single test
cargo clippy              # Lint check
```

### Code Style

- Run `cargo clippy` before submitting -- CI will check this
- Follow existing patterns in the codebase
- Add tests for new functionality

## Making Changes

### Bug Fixes

1. Create a branch: `git checkout -b fix/description`
2. Write a test that reproduces the bug
3. Fix the bug
4. Verify all tests pass: `cargo test`
5. Open a pull request

### New Features

1. Open an issue to discuss the feature first
2. Create a branch: `git checkout -b feature/description`
3. Implement with tests
4. Update the README if the feature is user-facing
5. Open a pull request

## Pull Requests

- Keep PRs focused -- one feature or fix per PR
- Include tests for new code paths
- Write a clear description of what changed and why
- CI must pass (build, test, clippy, fmt)

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
```

- [ ] **Step 5: Create `CLAUDE.md`**

Create `/Users/kristofer/repos/mklab-se/pidge/CLAUDE.md`:

```markdown
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

Rust workspace with a single crate:

```
crates/
  pidge/                # CLI binary (package and binary name: pidge)
    src/
      main.rs           # Entry point, tracing setup, dynamic completions, background update check
      cli.rs            # Clap CLI definitions, command dispatch
      banner.rs         # ASCII art logo + version subtitle
      update.rs         # Version update checker (queries crates.io, caches 24h)
      commands/
        mod.rs          # Command module exports
        ai.rs           # `pidge ai` — delegates to ailloy::config_tui
        completion.rs   # `pidge completion <shell>` — static completion + dynamic tip
        skill.rs        # `pidge ai skill [--emit|--reference]`
    doc/
      ai-reference.md   # Embedded via include_str! in skill.rs
```

The workspace is single-member by design at this stage. Splitting into `pidge`, `pidge-core`, `pidge-client` is planned when HTTP/provider code lands.

- **Workspace root** `Cargo.toml` defines shared dependencies and metadata
- Crate inherits `version`, `edition`, `authors`, `license`, `repository`, `rust-version` from workspace
- Single version bump in root `Cargo.toml` updates everything

## Key Patterns

- CLI built with `clap` derive macros + `clap_complete` for shell completions (static and dynamic)
- Async runtime: `tokio`
- Logging: `tracing` + `tracing-subscriber` with `-v`/`-vv` verbosity levels
- Colored output via `colored` crate (respects `--no-color`)
- Error handling: `anyhow` (CLI), `thiserror` (libraries — none yet)
- AI integration: delegates entirely to `ailloy::config_tui` with tool name `"pidge"` and capability slice `&["chat"]`. Config lives at `~/.config/ailloy/config.yaml`, shared with `rigg`, `mdeck`, `cosq`.
- Update checker: background task, cached at `~/.cache/pidge/`, skip with `PIDGE_NO_UPDATE_CHECK=1`

## Releasing

Use the `/release` skill (see `.claude/skills/release/SKILL.md`):

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

## Design Docs

Specs and implementation plans live under `docs/superpowers/`:

- `docs/superpowers/specs/` — design documents
- `docs/superpowers/plans/` — implementation plans
```

- [ ] **Step 6: Commit the docs**

```bash
git add README.md CHANGELOG.md INSTALL.md CONTRIBUTING.md CLAUDE.md
git commit -m "Add README, CHANGELOG, INSTALL, CONTRIBUTING, and CLAUDE.md"
```

---

## Task 13: CI workflow — `.github/workflows/ci.yml`

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create the workflows directory**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/.github/workflows`

- [ ] **Step 2: Write `ci.yml`**

Create `/Users/kristofer/repos/mklab-se/pidge/.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  check:
    name: Check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo check --workspace

  test:
    name: Test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --workspace

  clippy:
    name: Clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: stable
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace -- -D warnings

  format:
    name: Format
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: stable
          components: rustfmt
      - run: cargo fmt --all -- --check
```

- [ ] **Step 3: No local verification possible**

GitHub Actions YAML can't be smoke-tested locally without a runner. We'll rely on the first push exercising it. Move on.

---

## Task 14: Release workflow — `.github/workflows/release.yml`

**Files:**
- Create: `.github/workflows/release.yml`

This file is a name-swapped port of `/Users/kristofer/repos/mklab-se/cosq/.github/workflows/release.yml` with the `crates-io` job simplified to publish only `pidge`. Refer to the cosq file if anything below is ambiguous.

- [ ] **Step 1: Write `release.yml`**

Create `/Users/kristofer/repos/mklab-se/pidge/.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags:
      - "v*"

env:
  CARGO_TERM_COLOR: always

permissions:
  contents: write

jobs:
  # Run the full CI suite before building release artifacts
  ci:
    name: CI
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace -- -D warnings
      - run: cargo test --workspace

  # Build release binaries for each platform
  build:
    name: Build ${{ matrix.target }}
    needs: ci
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            archive: tar.gz
          - target: x86_64-apple-darwin
            os: macos-latest
            archive: tar.gz
          - target: aarch64-apple-darwin
            os: macos-latest
            archive: tar.gz
          - target: x86_64-pc-windows-msvc
            os: windows-latest
            archive: zip
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}

      - name: Build release binary
        shell: bash
        run: cargo build --release --target "$TARGET"
        env:
          TARGET: ${{ matrix.target }}

      - name: Package (unix)
        if: matrix.archive == 'tar.gz'
        shell: bash
        run: |
          cd "target/${TARGET}/release"
          tar czf "../../../pidge-${RELEASE_TAG}-${TARGET}.tar.gz" pidge
          cd ../../..
        env:
          TARGET: ${{ matrix.target }}
          RELEASE_TAG: ${{ github.ref_name }}

      - name: Package (windows)
        if: matrix.archive == 'zip'
        shell: pwsh
        run: |
          Compress-Archive -Path "target/$env:TARGET/release/pidge.exe" -DestinationPath "pidge-$env:RELEASE_TAG-$env:TARGET.zip"
        env:
          TARGET: ${{ matrix.target }}
          RELEASE_TAG: ${{ github.ref_name }}

      - name: Upload artifact
        uses: actions/upload-artifact@v5
        with:
          name: pidge-${{ matrix.target }}
          path: pidge-${{ github.ref_name }}-${{ matrix.target }}.*

  # Create GitHub Release with all binaries
  github-release:
    name: GitHub Release
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5

      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: artifacts
          merge-multiple: true

      - name: Create release
        uses: softprops/action-gh-release@v2
        with:
          generate_release_notes: true
          files: artifacts/*

  # Update Homebrew tap with new version and SHA256 hashes
  homebrew:
    name: Update Homebrew Tap
    needs: github-release
    runs-on: ubuntu-latest
    steps:
      - name: Update formula
        env:
          GH_TOKEN: ${{ secrets.HOMEBREW_TAP_TOKEN }}
        run: |
          set -e
          VERSION="${GITHUB_REF_NAME#v}"
          TAG="${GITHUB_REF_NAME}"
          BASE_URL="https://github.com/mklab-se/pidge/releases/download/${TAG}"

          # Download release artifacts and compute SHA256
          curl -sL "${BASE_URL}/pidge-${TAG}-aarch64-apple-darwin.tar.gz" -o aarch64-darwin.tar.gz
          curl -sL "${BASE_URL}/pidge-${TAG}-x86_64-apple-darwin.tar.gz" -o x86_64-darwin.tar.gz
          curl -sL "${BASE_URL}/pidge-${TAG}-x86_64-unknown-linux-gnu.tar.gz" -o x86_64-linux.tar.gz

          SHA_AARCH64=$(sha256sum aarch64-darwin.tar.gz | cut -d' ' -f1)
          SHA_X86_64=$(sha256sum x86_64-darwin.tar.gz | cut -d' ' -f1)
          SHA_LINUX=$(sha256sum x86_64-linux.tar.gz | cut -d' ' -f1)

          # Generate formula
          cat > formula.rb <<RUBY
          class Pidge < Formula
            desc "A fast CLI for e-mail and calendar"
            homepage "https://github.com/mklab-se/pidge"
            version "${VERSION}"
            license "MIT"

            on_macos do
              if Hardware::CPU.arm?
                url "https://github.com/mklab-se/pidge/releases/download/v#{version}/pidge-v#{version}-aarch64-apple-darwin.tar.gz"
                sha256 "${SHA_AARCH64}"
              else
                url "https://github.com/mklab-se/pidge/releases/download/v#{version}/pidge-v#{version}-x86_64-apple-darwin.tar.gz"
                sha256 "${SHA_X86_64}"
              end
            end

            on_linux do
              url "https://github.com/mklab-se/pidge/releases/download/v#{version}/pidge-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
              sha256 "${SHA_LINUX}"
            end

            def install
              bin.install "pidge"
            end

            test do
              assert_match version.to_s, shell_output("#{bin}/pidge --version")
            end
          end
          RUBY

          # Push to homebrew-tap repo via GitHub API
          CONTENT=$(base64 -w 0 < formula.rb)
          FILE_SHA=$(gh api repos/mklab-se/homebrew-tap/contents/Formula/pidge.rb --jq '.sha' 2>/dev/null || echo "")

          if [ -n "$FILE_SHA" ]; then
            gh api repos/mklab-se/homebrew-tap/contents/Formula/pidge.rb \
              -X PUT \
              -f message="Update pidge to ${VERSION}" \
              -f content="$CONTENT" \
              -f sha="$FILE_SHA" \
              && echo "Homebrew formula updated to ${VERSION}" \
              || echo "::warning::Failed to update Homebrew tap. Add HOMEBREW_TAP_TOKEN secret with repo scope for mklab-se/homebrew-tap."
          else
            # Create file for the first time
            gh api repos/mklab-se/homebrew-tap/contents/Formula/pidge.rb \
              -X PUT \
              -f message="Add pidge ${VERSION}" \
              -f content="$CONTENT" \
              && echo "Homebrew formula created for ${VERSION}" \
              || echo "::warning::Failed to create Homebrew formula. Add HOMEBREW_TAP_TOKEN secret with repo scope for mklab-se/homebrew-tap."
          fi

  # Publish to crates.io
  crates-io:
    name: Publish to crates.io
    needs: ci
    runs-on: ubuntu-latest
    environment: crates-io
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Publish pidge
        run: cargo publish -p pidge
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

- [ ] **Step 2: Commit both workflows**

```bash
git add .github/workflows/ci.yml .github/workflows/release.yml
git commit -m "Add CI and release GitHub Actions workflows"
```

---

## Task 15: Release skill — `.claude/skills/release/SKILL.md`

**Files:**
- Create: `.claude/skills/release/SKILL.md`

- [ ] **Step 1: Create the skill directory**

Run: `mkdir -p /Users/kristofer/repos/mklab-se/pidge/.claude/skills/release`

- [ ] **Step 2: Write the release skill**

Create `/Users/kristofer/repos/mklab-se/pidge/.claude/skills/release/SKILL.md`:

```markdown
---
name: release
description: "Release a new version: bump version, update docs, commit, push, and tag"
argument-hint: "<major|minor|patch>"
---

Release a new version of pidge.

## Input

$ARGUMENTS must be one of: `major`, `minor`, `patch`. If empty or invalid, stop and ask.

## Steps

### 1. Determine the new version

- Read the current version from the `version` field in the workspace `Cargo.toml`
- Apply the semver bump based on $ARGUMENTS:
  - `patch`: 0.1.0 -> 0.1.1
  - `minor`: 0.1.0 -> 0.2.0
  - `major`: 0.1.0 -> 1.0.0
- Show the user: "Releasing pidge v{OLD} -> v{NEW}"

### 2. Update dependencies

- Run `cargo update` to update all dependencies to their latest compatible versions

### 3. Pre-flight checks

- Run `cargo fmt --all -- --check` — abort if formatting issues
- Run `cargo clippy --workspace -- -D warnings` — abort if warnings
- Run `cargo test --workspace` — abort if any test fails
- Run `git status` — abort if there are uncommitted changes that are NOT documentation or version files

### 4. Bump version numbers

- Update `version` in the root `Cargo.toml` `[workspace.package]` section. The single workspace member inherits via `version.workspace = true`, so no other Cargo files need editing.

### 5. Update documentation

- **CHANGELOG.md**: Rename the `[Unreleased]` section to `[{NEW_VERSION}] - {TODAY}` (YYYY-MM-DD format). If there is no `[Unreleased]` section, create a new dated entry summarizing changes since the last release.
- **README.md**: Review for accuracy — the install snippet uses `brew`/`cargo install` without a pinned version, so no edits are typically required. Update the "Status" section if the release moves pidge beyond foundation phase.
- **INSTALL.md**: Review for accuracy — no version references to update typically.
- **CLAUDE.md**: Review for accuracy — update the Architecture section if the workspace shape changed.

### 6. Verify the build

- Run `cargo build --workspace` to ensure everything compiles with the new version
- Run `cargo test --workspace` once more after version bump

### 7. Commit, push, and tag

- Stage all changed files: `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and any updated docs
- Commit with message: `Release v{NEW_VERSION}`
- Push to main: `git push`
- Create and push tag: `git tag v{NEW_VERSION} && git push origin v{NEW_VERSION}`

### 8. Confirm

- Tell the user the release is tagged and pushed
- Remind them that the GitHub Actions release workflow will now build binaries, publish to crates.io, and update the Homebrew tap
```

- [ ] **Step 3: Commit the release skill**

```bash
git add .claude/skills/release/SKILL.md
git commit -m "Add /release skill for version bump and tag workflow"
```

---

## Task 16: Final verification — full CI parity locally

**Files:** None modified.

- [ ] **Step 1: Run the full CI suite that GitHub will run**

Run, in order:

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo check --workspace
```

Expected: every command exits 0 with no warnings. If anything fails, fix it before continuing.

- [ ] **Step 2: Smoke-test the binary one more time**

```bash
cargo run -q -- --version
cargo run -q -- version
cargo run -q -- ai
cargo run -q -- ai skill --emit | head -5
cargo run -q -- completion bash > /tmp/pidge-completion.bash && wc -l /tmp/pidge-completion.bash
```

Expected: `--version` prints `pidge 0.1.0`. `version` prints the banner. `ai` prints ailloy's status output. `ai skill --emit` starts with `---\nname: pidge`. The bash completion file has at least 20 lines.

- [ ] **Step 3: Confirm no stray files**

Run: `git status`
Expected: working tree clean (everything from Tasks 1–15 has been committed).

- [ ] **Step 4: Inspect the commit log**

Run: `git log --oneline`
Expected: a sequence of focused commits — bootstrap, banner, CLI skeleton, .gitignore, docs, workflows, release skill — on top of the existing `Initial commit`.

---

## Task 17: Hand-off notes (no code changes)

- [ ] **Step 1: List what's intentionally deferred**

Before declaring the foundation complete, write a short status summary in your reply to the user covering:

1. **Repo state**: branch `main`, N new commits on top of the original `Initial commit`, clean working tree, no tag pushed.
2. **Pre-release todos for the user**:
   - Push the branch to `git@github.com:mklab-se/pidge.git` (create the GitHub repo if it doesn't exist).
   - Configure repo secret `HOMEBREW_TAP_TOKEN` (same value as the cosq/rigg/mdeck token).
   - Configure environment `crates-io` with secret `CARGO_REGISTRY_TOKEN`.
   - When ready: run `/release patch` (or `minor`) to cut `0.1.0` and watch the release workflow create the GitHub release, publish to crates.io, and create `homebrew-tap/Formula/pidge.rb`.
3. **What's not in this scaffold**: no `init`/`auth`/feature commands, no provider integration, no MCP server, no workspace split into core/client crates. Roadmap items live in CHANGELOG `[Unreleased]` only after they're built.

- [ ] **Step 2: Done — no further code changes**

---

## Plan self-review

**Spec coverage check** (every section of `docs/superpowers/specs/2026-05-13-pidge-foundation-design.md`):

| Spec section | Covered by |
|---|---|
| Repository layout | Tasks 1, 2, 6, 11, 12, 13, 14, 15 (every listed path is created) |
| Cargo workspace — root `Cargo.toml` | Task 1 |
| Cargo workspace — `crates/pidge/Cargo.toml` | Task 2 |
| CLI surface (foundation) | Task 5 (`cli.rs` defines every listed subcommand and global flag) |
| Module responsibilities — `main.rs` | Task 10 |
| Module responsibilities — `cli.rs` | Task 5 |
| Module responsibilities — `commands/ai.rs` | Task 7 |
| Module responsibilities — `commands/completion.rs` | Task 8 |
| Module responsibilities — `commands/skill.rs` + `doc/ai-reference.md` | Task 9 |
| Module responsibilities — `banner.rs` | Task 3 |
| Module responsibilities — `update.rs` | Task 4 |
| `.claude/skills/release/SKILL.md` | Task 15 |
| `.github/workflows/ci.yml` | Task 13 |
| `.github/workflows/release.yml` | Task 14 |
| Homebrew formula auto-create on first tag | Task 14 (`if [ -n "$FILE_SHA" ] ... else` branch) |
| `README.md`, `CHANGELOG.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `INSTALL.md` | Task 12 |
| `.gitignore` extension | Task 11 |
| Versioning & release sequencing — initial commit on 0.1.0, no tag yet | Tasks 1–16 produce the scaffold; tagging is explicitly deferred to the user (Task 17 hand-off) |
| Out-of-scope items | Not implemented — confirmed in Task 17 hand-off |

**Placeholder scan:** No "TBD", "TODO", or "implement later" remain. Every code block is complete and executable.

**Type/name consistency:** Verified — `Cli`, `Commands`, `AiCommands`, `Shell`, `Cli::run`, `commands::ai::run`, `commands::completion::generate_completions`, `commands::skill::run`, `update::check_for_updates`, `banner::print_banner_with_version` are referenced with the same names everywhere they appear.

**Notable risks / things that could surprise the implementer:**

1. **`ailloy v0.7` API.** The plan assumes `config_tui::print_ai_status`, `run_test_chat`, `enable_ai`, `disable_ai`, `run_interactive_config`, `is_ai_active`, and `Config::load_global` are all present with the signatures used here. These signatures are the ones cosq's `0.9.0` release uses against `ailloy v0.5`. If `ailloy 0.7` has renamed or removed any of these, Task 7 will fail to compile and the engineer needs to inspect `~/.cargo/registry/src/.../ailloy-0.7.*/src/config_tui.rs` (or the ailloy repo at `../ailloy/`) and adjust the call sites.
2. **`dirs v6.0` vs `v5.0`.** `cosq` and `mdeck` use `dirs = "6.0"`, `rigg` uses `dirs = "5.0"`. The plan uses `6.0`. `dirs::cache_dir()` and `dirs::config_dir()` exist in both; no expected breakage.
3. **`colored v2.1`.** Same version cosq/mdeck/rigg use; `Colorize::bold()`, `dimmed()`, `yellow()`, `green()`, `cyan()`, `control::set_override` are all stable.
4. **`reqwest` rustls features.** `rustls-tls-native-roots` may need adjustment if `reqwest 0.12` has renamed the feature; cosq uses this exact spelling against `reqwest 0.12`.
5. **Homebrew formula class name.** Ruby class names must be CamelCase derived from the file name. `pidge.rb` → `class Pidge`. Already correct in Task 14.
