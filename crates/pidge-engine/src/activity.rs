//! The activity log: what the user asked (the intent, as their agent puts
//! it), every call made to answer it, and what was carried out behind the
//! answers, one JSON line each. Every line names the pidge version, so that
//! before and after a release can be told apart. The `Host` decides where
//! the lines go; the engine only builds them.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

/// The program making the calls, as it names itself when it connects
/// (MCP `clientInfo`): `claude-ai 0.1.0`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Client {
    pub name: String,
    pub version: String,
    /// The MCP protocol version of the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// What it says it can do: extensions, and `form` / `url` questions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub can: Vec<String>,
}

/// The pidge version every line carries.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Most characters of an answer kept in a line.
pub const ANSWER_CAP: usize = 4_000;

/// The file a line written at `t` goes to: `2026-11-22.jsonl`.
pub fn day_file(t: DateTime<Utc>) -> String {
    format!("{}.jsonl", t.format("%Y-%m-%d"))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The line that marks a server (re)starting with this version.
pub fn start_line() -> String {
    json!({ "t": now(), "v": VERSION, "kind": "start" }).to_string()
}

/// The line for an instruction the queue has carried out.
pub fn job_line(
    id: &str,
    label: &str,
    ms: u64,
    attempt: u32,
    done: usize,
    failed: usize,
    retrying: usize,
) -> String {
    json!({
        "t": now(),
        "v": VERSION,
        "kind": "job",
        "id": id,
        "label": label,
        "ms": ms,
        "attempt": attempt + 1,
        "done": done,
        "failed": failed,
        "retrying": retrying,
    })
    .to_string()
}

/// The line for a question put to the user themself, and their answer.
pub fn ask_line(client: Option<&Client>, tool: &str, answer: &str) -> String {
    json!({
        "t": now(),
        "v": VERSION,
        "kind": "ask",
        "client": client,
        "tool": tool,
        "answer": answer,
    })
    .to_string()
}

/// The line for questions put to the user's AI.
pub fn ai_line(purpose: &str, questions: usize) -> String {
    json!({ "t": now(), "v": VERSION, "kind": "ai", "purpose": purpose, "n": questions })
        .to_string()
}

/// The line for one call: its answer text, or its error message. `intent`
/// is what the user asked, in the agent's words.
pub fn call_line(
    client: Option<&Client>,
    intent: Option<&str>,
    tool: &str,
    input: &Value,
    ms: u64,
    result: Result<&str, &str>,
) -> String {
    let (ok, error, answer) = match result {
        Ok(text) => (true, None, text),
        Err(e) => (false, Some(e), ""),
    };
    let capped: String = answer.chars().take(ANSWER_CAP).collect();
    json!({
        "t": now(),
        "v": VERSION,
        "kind": "call",
        "client": client,
        "intent": intent,
        "tool": tool,
        "input": input,
        "ms": ms,
        "ok": ok,
        "error": error,
        "bytes": answer.len(),
        "answer": capped,
    })
    .to_string()
}

/// Append one line to the day's file in `dir` (created if missing).
pub fn append_line(dir: &std::path::Path, at: DateTime<Utc>, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(day_file(at)))?;
    writeln!(file, "{line}")
}
