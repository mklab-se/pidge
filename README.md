<p align="center">
  <img src="https://raw.githubusercontent.com/mklab-se/pidge/main/media/pidge.png" alt="pidge" width="600">
</p>

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
