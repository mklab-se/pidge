---
name: release
description: "Release a new version: bump version, update docs, commit, push, tag, and verify the release workflow"
argument-hint: "<major|minor|patch>"
---

Release a new version of pidge.

## Input

$ARGUMENTS must be one of: `major`, `minor`, `patch`. If empty or invalid, stop and ask.

## Steps

### 1. Determine the new version

- Read the current version from the `version` field in the workspace `Cargo.toml` (`[workspace.package]`)
- Apply the semver bump based on $ARGUMENTS:
  - `patch`: 0.1.0 -> 0.1.1
  - `minor`: 0.1.0 -> 0.2.0
  - `major`: 0.1.0 -> 1.0.0
- Show the user: "Releasing pidge v{OLD} -> v{NEW}"

### 2. Update toolchain and dependencies

- Run `rustup update stable` — CI runs the LATEST stable Rust, and newer clippy versions ship new
  lints. Running the pre-flight checks on an older local toolchain lets warnings through that then
  fail the release workflow. After updating, confirm with `rustc --version`
- Run `cargo update` to update all dependencies to their latest compatible versions

### 3. Pre-flight checks

- Run `cargo fmt --all -- --check` — abort if formatting issues. If you fix formatting with
  `cargo fmt --all`, re-run clippy afterwards: reformatting can change what clippy flags
- Run `cargo clippy --workspace --all-targets -- -D warnings` — abort if warnings
  (`--all-targets` matches CI: it also lints tests and benches)
- Run `cargo test --workspace` — abort if any test fails
- Run `git status` — abort if there are uncommitted changes that are NOT documentation, version,
  or dependency files

### 4. Bump version numbers

- Update `version` in the root `Cargo.toml` `[workspace.package]` section
- Update internal crate dependency versions (`pidge-core`, `pidge-client`) in the root
  `Cargo.toml` `[workspace.dependencies]` section — they use `version = "X.Y.Z"` format

### 5. Update documentation

- **CHANGELOG.md**: Rename the `[Unreleased]` section to `[{NEW_VERSION}] - {TODAY}` (YYYY-MM-DD format). If there is no `[Unreleased]` section, create a new dated entry summarizing changes since the last release
- **README.md**: Review for accuracy — update any version references if present
- **CLAUDE.md**: Review for accuracy — update the Architecture section if the workspace shape changed
- **INSTALL.md**: Review for accuracy — no version references to update typically

### 6. Verify the build

- Run `cargo build --workspace` to ensure everything compiles with the new version
- Run `cargo test --workspace` once more after version bump

### 7. Commit, push, and tag

- Stage all changed files: `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and any updated docs
- Commit with message: `Release v{NEW_VERSION}`
- Push to main: `git push` (the `main` ruleset only forbids force-pushes and deletion; direct
  pushes are fine)
- Create and push tag: `git tag v{NEW_VERSION} && git push origin v{NEW_VERSION}`. The tag is the
  one trigger for everything that ships: CLI binaries, crates.io, Homebrew, the MCP image AND the
  pidge-mcp deployment to Azure all run from `release.yml` on this tag, behind its CI job

### 8. Watch and verify

- The tag push triggers the Release workflow. Do NOT declare success yet — watch it:
  `gh run list --repo mklab-se/pidge --workflow release.yml --limit 1`, then
  `gh run watch <id> --repo mklab-se/pidge --exit-status` until it completes
- If a job fails on a GitHub flake (artifact download, runner outage), `gh run rerun <id> --failed`
  re-runs just the failed jobs. If the fix needs a code change, commit it to `main`, then move the
  tag: `git tag -f v{NEW_VERSION} && git push -f origin v{NEW_VERSION}` re-runs the whole workflow;
  crate versions already on crates.io are skipped, everything else is redone
- When it is green, confirm the outputs:
  - `gh release view v{NEW_VERSION} --repo mklab-se/pidge` lists 4 archives
    (3 × `.tar.gz`, 1 × `.zip`) plus 4 matching `.cdx.json` SBOMs
  - `cargo search pidge --limit 1` shows the new version on crates.io
  - `Formula/pidge.rb` in `mklab-se/homebrew-tap` carries the new version
  - The `Deploy MCP` job passed its smoke test, and `curl -s https://pidge.mklab.se/healthz`
    returns `ok`

### 9. Confirm

- Tell the user the release is tagged, pushed, and the workflow is green — auditable binaries and
  SBOMs are attached to the GitHub Release, crates.io is published (`pidge-core` → `pidge-client` →
  `pidge`), the Homebrew tap is updated, and pidge-mcp is deployed
- The publish jobs require the `CARGO_REGISTRY_TOKEN` (in the `crates-io` environment) and
  `HOMEBREW_TAP_TOKEN` (repo secret) to be configured — see README.md
