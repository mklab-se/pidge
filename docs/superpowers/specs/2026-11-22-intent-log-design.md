# Intent log: what was asked, and the calls it took

Status: proposed, 2026-11-22

## Why

Kristofer wants Pidge as fast and easy for an AI agent as it can be, starting with the MCP server he
hosts for himself (and later Anna). To see where agents take detours, Pidge records what the user
asked for (the intent, as the agent condenses it) together with every tool call made to answer it.
Later Claude reads the log and proposes changes to the tools: merged calls, missing parameters,
answers that make agents ask again.

## Goals

- An `intent` operation: the agent sends one or two sentences on what the user asked and gets an
  id back (`i` plus a short random string, unique across restarts).
- Every other operation accepts an optional `intent` parameter with that id.
- Every call is logged: time, intent id, tool, full parameters, duration, error (if any), the
  answer's size and its text capped at 4,000 characters.
- Calls without an id are logged too; at analysis time they belong to the latest intent before them.
- The MCP instructions (and the skill) tell the agent to start each request with `intent`, pass the
  id on, and that calls are logged so Pidge can be improved.
- Logging never slows an answer and never makes a call fail.
- Everything is kept: no automatic deletion.

## Non-goals

- No dashboard, no search UI. Analysis is Claude reading the files on request.
- No redaction: Kristofer is the only active user and agreed to full logging, contents included.
  Anna has not set Pidge up; the instructions say calls are logged before anyone else uses it.

## Approaches

- **A. JSON lines in files, one per user per day (picked).** Hosted: append blobs in a new
  `activity` container in the existing storage account, `<user hash>/<YYYY-MM-DD>.jsonl`, written
  through the managed identity like the preferences. Local (CLI and `pidge mcp serve`): the same
  format under `${XDG_CACHE_HOME:-~/.cache}/pidge/activity/`. Cheap, simple, and easy to read.
- B. Application Insights: queryable in the portal, but a new resource, truncated parameters, mixed
  with operational logs, and nothing local. Not picked.
- C. The SQLite index: on the hosted server it lives in `/tmp` and is lost on restart. Ruled out.

## Design

- `ops::intent` registers the operation (CLI `pidge intent "..."`). It writes an `intent` line and
  answers with the id and one line on passing it on.
- `registry`: adds an optional `intent` string property to every operation's schema except
  `intent` itself, removes it from the input before the operation parses it, and after the
  operation returns hands a `call` line to the log.
- The log is a new `Host` method (`record(line)`), so frontends decide where lines go: `HostedHost`
  appends to the user's blob; `LocalHost` appends to the local file. Writes happen on the user's
  background worker (inline on the CLI after the answer is printed is fine).
- Line shape:
  `{"t":"2026-11-22T10:15:02Z","kind":"intent","intent":"i4kq2x","summary":"..."}`
  `{"t":"...","kind":"call","intent":"i4kq2x","tool":"mail_find","input":{...},"ms":412,"ok":true,"error":null,"bytes":1830,"answer":"..."}`
- Deploy: `main.bicep` adds the `activity` container; the server's identity already has Storage
  Blob Data Contributor on the account. A new env var, `PIDGE_MCP_ACTIVITY_URL`, points at it,
  documented on `Config::from_env`; unset means no hosted log (dev).
- Analysis: `az storage blob download-batch` from the `activity` container (or the local folder),
  then Claude reads the lines grouped by intent.

## Left to the builder

- Exact wording of the instruction lines and the `intent` description.
- Id alphabet and length; how the append blob is created on first write of the day.
