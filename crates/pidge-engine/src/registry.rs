//! The operation registry: every operation pidge offers, defined once.
//!
//! Each [`OpDef`] carries its name (the MCP tool name), its CLI path
//! (`["mail", "find"]` is `pidge mail find`), its description, the JSON
//! Schema of its input, and the function that runs it. The CLI builds its
//! subcommands from this, both MCP servers build their tool lists from it,
//! and the skill is generated from it, so the three can never drift apart.

use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ctx::User;
use crate::error::{OpError, OpResult};

/// What an operation returns: text for people and models, data for scripts.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub text: String,
    pub data: Value,
}

impl Output {
    pub fn new(text: impl Into<String>, data: Value) -> Self {
        Self {
            text: text.into(),
            data,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            data: serde_json::json!({ "message": text }),
            text,
        }
    }
}

type Runner = Box<dyn Fn(Arc<User>, Value) -> BoxFuture<'static, OpResult<Output>> + Send + Sync>;

/// One operation.
pub struct OpDef {
    pub name: &'static str,
    pub cli: &'static [&'static str],
    /// One line for `--help` lists.
    pub summary: &'static str,
    /// The full description (MCP tool description, CLI long help).
    pub description: &'static str,
    /// Input fields the CLI takes positionally, in order.
    pub positional: &'static [&'static str],
    /// Reads only (MCP `readOnlyHint`).
    pub read_only: bool,
    pub schema: Value,
    run: Runner,
}

impl OpDef {
    /// Run with JSON input (validated against the input type).
    pub fn run(&self, user: Arc<User>, input: Value) -> BoxFuture<'static, OpResult<Output>> {
        (self.run)(user, input)
    }

    /// The `properties` of the input schema.
    pub fn properties(&self) -> serde_json::Map<String, Value> {
        self.schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    pub fn required(&self) -> Vec<String> {
        self.schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Static facts of an operation.
pub struct Meta {
    pub name: &'static str,
    pub cli: &'static [&'static str],
    pub summary: &'static str,
    pub description: &'static str,
    pub positional: &'static [&'static str],
    pub read_only: bool,
}

/// Define an operation from a typed handler.
pub fn op<I, F, Fut>(meta: Meta, f: F) -> OpDef
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    F: Fn(Arc<User>, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = OpResult<Output>> + Send + 'static,
{
    let mut settings = schemars::generate::SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<I>();
    let schema = serde_json::to_value(schema).unwrap_or(Value::Null);
    let schema_for_coerce = schema.clone();
    let f = Arc::new(f);
    OpDef {
        name: meta.name,
        cli: meta.cli,
        summary: meta.summary,
        description: meta.description,
        positional: meta.positional,
        read_only: meta.read_only,
        schema,
        run: Box::new(move |user, input| {
            let f = f.clone();
            let input = coerce(&schema_for_coerce, input);
            Box::pin(async move {
                let input: I = serde_json::from_value(input)
                    .map_err(|e| OpError::usage(format!("invalid input: {e}")))?;
                f(user, input).await
            })
        }),
    }
}

/// The JSON types a schema property allows (`["string", "null"]` → both).
fn types_of(prop: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match prop.get("type") {
        Some(Value::String(t)) => out.push(t.clone()),
        Some(Value::Array(a)) => out.extend(a.iter().filter_map(Value::as_str).map(str::to_string)),
        _ => {}
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(alts)) = prop.get(key) {
            for alt in alts {
                out.extend(types_of(alt));
            }
        }
    }
    out
}

/// Harnesses sometimes send scalars as strings (`"true"`, `"5"`) and lists
/// as a single string. Accept those for top-level properties, and drop
/// `null`s so defaults apply.
pub fn coerce(schema: &Value, input: Value) -> Value {
    let Value::Object(mut map) = (match input {
        Value::Null => Value::Object(Default::default()),
        other => other,
    }) else {
        return Value::Null;
    };
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    map.retain(|_, v| !v.is_null());
    for (key, value) in map.iter_mut() {
        let Some(prop) = props.get(key) else { continue };
        let types = types_of(prop);
        if let Value::String(s) = value.clone() {
            if types.iter().any(|t| t == "string") {
                continue;
            }
            if types.iter().any(|t| t == "boolean") {
                match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "yes" | "1" => *value = Value::Bool(true),
                    "false" | "no" | "0" => *value = Value::Bool(false),
                    _ => {}
                }
            } else if types.iter().any(|t| t == "integer" || t == "number") {
                if let Ok(n) = s.trim().parse::<i64>() {
                    *value = Value::from(n);
                }
            } else if types.iter().any(|t| t == "array") {
                let parsed: Option<Vec<Value>> = serde_json::from_str(&s).ok();
                *value = Value::Array(parsed.unwrap_or_else(|| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(|p| Value::String(p.to_string()))
                        .collect()
                }));
            } else if types.iter().any(|t| t == "object")
                && let Ok(v) = serde_json::from_str::<Value>(&s)
            {
                *value = v;
            }
        }
    }
    Value::Object(map)
}

/// Every operation, in the order they are listed.
pub fn all() -> Vec<OpDef> {
    crate::ops::all()
}

/// Find one by MCP name.
pub fn find(name: &str) -> Option<OpDef> {
    all().into_iter().find(|o| o.name == name)
}

/// Run an operation for a user: due outbox entries go first, and notes
/// waiting for the user (earlier background failures, sends that went out)
/// are put on top of the answer.
pub async fn run(user: &Arc<User>, op: &OpDef, input: Value) -> OpResult<Output> {
    crate::outbox::tick(user).await;
    let result = op.run(user.clone(), input).await;
    let notes = user.store.take_notes().unwrap_or_default();
    match result {
        Ok(mut out) => {
            if !notes.is_empty() {
                let mut text = String::new();
                for n in &notes {
                    text.push_str(&format!("Note: {n}\n"));
                }
                text.push_str(&out.text);
                out.text = text;
                if let Value::Object(map) = &mut out.data {
                    map.insert("notes".into(), serde_json::json!(notes));
                }
            }
            Ok(out)
        }
        Err(mut e) => {
            if !notes.is_empty() {
                e.message = format!("{} (also: {})", e.message, notes.join("; "));
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coerce_accepts_stringly_scalars_and_lists() {
        let schema = json!({ "properties": {
            "unread": { "type": ["boolean", "null"] },
            "limit": { "type": ["integer", "null"] },
            "ids": { "type": ["array", "null"] },
            "text": { "type": ["string", "null"] },
        }});
        let out = coerce(
            &schema,
            json!({ "unread": "true", "limit": "5", "ids": "m1, m2", "text": "5", "x": null }),
        );
        assert_eq!(
            out,
            json!({ "unread": true, "limit": 5, "ids": ["m1", "m2"], "text": "5" })
        );
        assert_eq!(coerce(&schema, Value::Null), json!({}));
    }

    #[test]
    fn every_op_has_an_object_schema_and_a_unique_name_and_path() {
        let ops = all();
        let mut names = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for o in &ops {
            assert!(names.insert(o.name), "duplicate {}", o.name);
            assert!(paths.insert(o.cli.join(" ")), "duplicate path {:?}", o.cli);
            assert_eq!(o.schema.get("type"), Some(&json!("object")), "{}", o.name);
            for p in o.positional {
                assert!(o.properties().contains_key(*p), "{}: {p}", o.name);
            }
            assert!(!o.description.contains('\u{2014}'));
        }
    }
}
