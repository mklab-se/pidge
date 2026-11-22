# Intent Log Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Log every request's intent and every tool call it took, locally and hosted, then release.

**Architecture:** A new `intent` operation hands out ids; `registry::op` adds an optional `intent`
field to every other operation; `registry::run` times each call and hands one JSON line to the
user's `Host` through the background worker. `LocalHost` appends to a daily file in the cache dir,
`HostedHost` to a daily append blob.

**Tech Stack:** Rust, serde_json, rand 0.10, reqwest, Azure Blob REST (append blobs), Bicep.

**Spec:** `docs/superpowers/specs/2026-11-22-intent-log-design.md`

## Global Constraints

- No em-dashes anywhere (CI enforces).
- Logging never fails or delays a call: errors are dropped with a `tracing::warn!`.
- Everything is kept; no deletion.
- Answer text in a line is capped at 4,000 characters.

## Review Focus

- A call whose `intent` id was never issued: logged as given, no error.
- An operation that fails: logged with `ok:false` and the error message.
- A non-ASCII answer longer than the cap: cut on a character boundary.
- First call of a day on the hosted server: the append blob is created, and a concurrent create (409) is fine.
- `intent` passed to the CLI as `--intent`: accepted like any other field.

---

### Task 1: Engine: `intent` operation, the `intent` field, and the call log

**Files:**
- Create: `crates/pidge-engine/src/activity.rs`, `crates/pidge-engine/src/ops/intent.rs`
- Modify: `crates/pidge-engine/src/registry.rs` (`op`, `run`), `crates/pidge-engine/src/ctx.rs` (`Host::record`), `crates/pidge-engine/src/ops/mod.rs` (`all`), `crates/pidge-engine/src/lib.rs`, `crates/pidge-engine/src/playbooks.rs` (RULES), `crates/pidge-engine/Cargo.toml` (rand)
- Test: `crates/pidge-engine/src/tests.rs`

**Interfaces:**
- Produces: `Host::record(&self, line: String)` (async, default no-op); `activity::new_intent_id() -> String` (`i` + 6 chars of `[a-z0-9]`); `activity::day_file(t: DateTime<Utc>) -> String` (`"2026-11-22.jsonl"`).
- Line shapes: `{"t","kind":"intent","intent","summary"}` and `{"t","kind":"call","intent","tool","input","ms","ok","error","bytes","answer"}`; `input` without the `intent` key.

- [ ] Tests: `every_tool_but_intent_takes_an_intent_id`; `an_intent_and_its_calls_are_logged` (intent line with summary, then a `mail_find` call line with the same id, `ok:true`, `ms` present, `input` lacking `intent`); `a_failed_call_is_logged_with_its_error`; `a_long_answer_is_capped_in_the_log` (4,000 chars, multibyte safe). TestHost gains `lines: Mutex<Vec<String>>`.
- [ ] Run, watch them fail; implement; run the workspace tests and clippy; commit.

### Task 2: Local sink

**Files:** Modify `crates/pidge/src/local.rs`.

- [ ] Test `lines_are_appended_to_the_days_file` for `append_line(dir: &Path, t: DateTime<Utc>, line: &str) -> std::io::Result<()>` (creates `dir`, two lines in order, file named by `day_file`).
- [ ] Implement; `LocalHost::record` calls it with `paths::cache_dir()/activity`; commit.

### Task 3: Hosted sink, config and deploy

**Files:** Modify `crates/pidge-mcp/src/engine.rs` (`BlobActivity`, `HostedHost`), `crates/pidge-mcp/src/config.rs` (`PIDGE_MCP_ACTIVITY_URL`, https, absolute; unset means a local dir under the temp dir), `deploy/azure/main.bicep` (`activity` container, env var), docs (`docs/mcp.md`, CLAUDE.md env list, CHANGELOG).

- [ ] Config test: `activity_url_must_be_https`.
- [ ] `BlobActivity::record`: `PUT <container>/<user_hash_long>/<day_file>` with `x-ms-blob-type: AppendBlob` and `If-None-Match: *` (201 or 409 both fine), then `PUT ...?comp=appendblock` with the line plus `\n`.
- [ ] Commit, then release (`/release minor`), watch the workflow, and confirm a hosted line lands in the container.
