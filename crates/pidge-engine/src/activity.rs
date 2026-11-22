//! The activity log: what the user asked (the intent, as their agent puts
//! it) and every call made to answer it, one JSON line each. The `Host`
//! decides where the lines go; the engine only builds them.

use chrono::{DateTime, SecondsFormat, Utc};
use rand::{Rng, rng};
use serde_json::{Value, json};

/// Most characters of an answer kept in a line.
pub const ANSWER_CAP: usize = 4_000;

/// A fresh intent id: `i` and six characters of `[a-z0-9]`.
pub fn new_intent_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 6];
    rng().fill_bytes(&mut bytes);
    let tail: String = bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect();
    format!("i{tail}")
}

/// The file a line written at `t` goes to: `2026-11-22.jsonl`.
pub fn day_file(t: DateTime<Utc>) -> String {
    format!("{}.jsonl", t.format("%Y-%m-%d"))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The line for a new intent.
pub fn intent_line(id: &str, summary: &str) -> String {
    json!({ "t": now(), "kind": "intent", "intent": id, "summary": summary }).to_string()
}

/// The line for one call: its answer text, or its error message.
pub fn call_line(
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
        "kind": "call",
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
