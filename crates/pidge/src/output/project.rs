//! Token-thrift helpers for agent JSON output: top-level field projection
//! (`--fields id,subject,from`) and body truncation (`--max-chars N`).

use serde_json::Value;

static FIELDS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
static MAX_CHARS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Called once from main with the global --fields/--max-chars values.
pub fn configure(fields: Vec<String>, max_chars: Option<usize>) {
    if !fields.is_empty() {
        let _ = FIELDS.set(fields);
    }
    if let Some(max) = max_chars {
        let _ = MAX_CHARS.set(max);
    }
}

/// Apply the configured projection/truncation to a JSON value about to be
/// printed, then pretty-print it. The single exit point for agent JSON.
pub fn emit_json(mut value: Value) -> anyhow::Result<()> {
    if let Some(max) = MAX_CHARS.get() {
        truncate_bodies(&mut value, *max);
    }
    if let Some(fields) = FIELDS.get() {
        project_fields(&mut value, fields)?;
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("unknown field '{field}' — valid fields: {valid}")]
pub struct UnknownField {
    pub field: String,
    pub valid: String,
}

/// Keep only `fields` (top-level keys) in every object of `value` (an array
/// of objects or an object with an `items` array). Errors listing the valid
/// names when a requested field doesn't exist on the first object.
pub fn project_fields(value: &mut Value, fields: &[String]) -> Result<(), UnknownField> {
    let items: &mut Vec<Value> = match value {
        Value::Array(items) => items,
        Value::Object(map) => match map.get_mut("items") {
            Some(Value::Array(items)) => items,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    if let Some(first) = items.first() {
        if let Some(obj) = first.as_object() {
            for field in fields {
                if !obj.contains_key(field) {
                    return Err(UnknownField {
                        field: field.clone(),
                        valid: obj.keys().cloned().collect::<Vec<_>>().join(", "),
                    });
                }
            }
        }
    }
    for item in items.iter_mut() {
        if let Some(obj) = item.as_object_mut() {
            obj.retain(|k, _| fields.iter().any(|f| f == k));
        }
    }
    Ok(())
}

/// Truncate long string fields (body_text, preview) to `max` chars, appending
/// a marker with the cut size so agents know content was elided.
pub fn truncate_bodies(value: &mut Value, max: usize) {
    fn truncate(s: &mut String, max: usize) {
        let count = s.chars().count();
        if count > max {
            let kept: String = s.chars().take(max).collect();
            *s = format!("{kept}…[truncated {} chars]", count - max);
        }
    }
    fn walk(v: &mut Value, max: usize) {
        match v {
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, max)),
            Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    if (key == "body_text" || key == "preview") && val.is_string() {
                        if let Value::String(s) = val {
                            truncate(s, max);
                        }
                    } else {
                        walk(val, max);
                    }
                }
            }
            _ => {}
        }
    }
    walk(value, max);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projection_keeps_requested_and_errors_on_unknown() {
        let mut v = json!([{"id": "1", "subject": "s", "body_text": "b"}]);
        project_fields(&mut v, &["id".into(), "subject".into()]).unwrap();
        assert_eq!(v, json!([{"id": "1", "subject": "s"}]));

        let mut v = json!([{"id": "1"}]);
        let err = project_fields(&mut v, &["nope".into()]).unwrap_err();
        assert!(err.to_string().contains("valid fields: id"));
    }

    #[test]
    fn projection_reaches_into_items_envelope() {
        let mut v = json!({"items": [{"id": "1", "x": 2}], "next_cursor": "c"});
        project_fields(&mut v, &["id".into()]).unwrap();
        assert_eq!(v["items"], json!([{"id": "1"}]));
        assert_eq!(v["next_cursor"], "c");
    }

    #[test]
    fn truncation_marks_the_cut() {
        let mut v = json!([{"body_text": "abcdefghij", "preview": "xy"}]);
        truncate_bodies(&mut v, 4);
        assert_eq!(v[0]["body_text"], "abcd…[truncated 6 chars]");
        assert_eq!(v[0]["preview"], "xy");
    }
}
