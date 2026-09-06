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

## Software bill of materials (SBOM)

Every release asset above has a matching CycloneDX 1.5 SBOM listing the exact crate versions
compiled into that platform's binary:

```
pidge-vX.Y.Z-<target>.cdx.json
```

The binaries are also built with [`cargo auditable`](https://github.com/rust-secure-code/cargo-auditable),
so the dependency list travels inside the executable itself. Check a downloaded binary against the
RustSec advisory database with:

```sh
cargo install cargo-audit --features=fix
cargo audit bin ./pidge
```

`syft` and `trivy` also understand this format.

## cargo install

Compile from source via crates.io (requires Rust 1.88+):

```bash
cargo install pidge
```

## Build from Source

```bash
git clone https://github.com/mklab-se/pidge.git
cd pidge
cargo build --release
```

The binary is at `target/release/pidge`. Requires Rust 1.88 or later.

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
