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
