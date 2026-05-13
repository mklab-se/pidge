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
