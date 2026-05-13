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
