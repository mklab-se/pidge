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

- Update `version` in the root `Cargo.toml` `[workspace.package]` section.
- Update internal-crate `version = "X.Y.Z"` pins in the root `Cargo.toml` `[workspace.dependencies]` section — both `pidge-core` and `pidge-client`. They use the bumped version (no `=` prefix).

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
