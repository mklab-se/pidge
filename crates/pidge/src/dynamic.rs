//! `pidge mail ...`, `pidge calendar ...` and `pidge accounts`, built at
//! runtime from the engine's operations: every input field becomes a flag
//! (`set_kind` → `--set-kind`), booleans are switches, lists repeat or take
//! commas, and the operation's `positional` fields come first. So the CLI
//! takes exactly what the MCP tools take, with the same names.

use clap::{Arg, ArgAction, ArgMatches, Command};
use pidge_engine::OpDef;
use serde_json::{Map, Value};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Bool,
    Int,
    List,
    Text,
}

fn shape(prop: &Value) -> Shape {
    let t = types_of(prop);
    if t.iter().any(|t| t == "boolean") {
        Shape::Bool
    } else if t.iter().any(|t| t == "integer") {
        Shape::Int
    } else if t.iter().any(|t| t == "array") {
        Shape::List
    } else {
        Shape::Text
    }
}

fn first_line(s: &str) -> String {
    s.split("\n\n").next().unwrap_or(s).replace('\n', " ")
}

/// The clap command for one operation.
pub fn command_for(op: &OpDef) -> Command {
    let verb = op.cli.last().copied().unwrap_or(op.name);
    let mut cmd = Command::new(verb)
        .about(op.summary)
        .long_about(op.description);
    let props = op.properties();
    for (i, name) in op.positional.iter().enumerate() {
        let Some(prop) = props.get(*name) else {
            continue;
        };
        let help = prop
            .get("description")
            .and_then(Value::as_str)
            .map(first_line)
            .unwrap_or_default();
        let mut arg = Arg::new(name.to_string())
            .index(i + 1)
            .help(help)
            .value_name(name.to_uppercase());
        if shape(prop) == Shape::List {
            arg = arg.num_args(1..).action(ArgAction::Append);
        }
        cmd = cmd.arg(arg);
    }
    for (name, prop) in &props {
        if op.positional.contains(&name.as_str()) {
            continue;
        }
        let help = prop
            .get("description")
            .and_then(Value::as_str)
            .map(first_line)
            .unwrap_or_default();
        let long = name.replace('_', "-");
        let arg = Arg::new(name.clone()).long(long).help(help);
        let arg = match shape(prop) {
            // `--unread` is true; `--mark-seen=false` turns a default-on switch off.
            Shape::Bool => arg
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("true")
                .value_parser(clap::value_parser!(bool)),
            Shape::Int => arg.value_parser(clap::value_parser!(i64)).value_name("N"),
            Shape::List => arg.action(ArgAction::Append).value_name("VALUE"),
            Shape::Text => arg.value_name("VALUE"),
        };
        cmd = cmd.arg(arg);
    }
    cmd
}

/// The groups (`mail`, `calendar`) and top-level operations (`accounts`).
pub fn commands(ops: &[OpDef]) -> Vec<Command> {
    let mut groups: Vec<(String, Command)> = Vec::new();
    let mut top = Vec::new();
    for op in ops {
        match op.cli {
            [single] => top.push(command_for(op).name(*single)),
            [group, ..] => {
                let idx = match groups.iter().position(|(g, _)| g == group) {
                    Some(i) => i,
                    None => {
                        let about = match *group {
                            "mail" => {
                                "E-mail: what's new, find, read, go through, act, write, send"
                            }
                            "calendar" => "Calendar: agenda, events, invitations, free time",
                            other => other,
                        };
                        groups.push((
                            group.to_string(),
                            Command::new(group.to_string()).about(about),
                        ));
                        groups.len() - 1
                    }
                };
                let (name, g) = groups.remove(idx);
                groups.insert(idx, (name, g.subcommand(command_for(op))));
            }
            [] => {}
        }
    }
    groups.into_iter().map(|(_, c)| c).chain(top).collect()
}

/// The operation a parsed command line names, and its JSON input.
pub fn resolve<'a>(ops: &'a [OpDef], matches: &ArgMatches) -> Option<(&'a OpDef, Value)> {
    let (first, sub) = matches.subcommand()?;
    let (op, m) = match ops.iter().find(|o| o.cli == [first]) {
        Some(op) => (op, sub),
        None => {
            let (verb, m) = sub.subcommand()?;
            (
                ops.iter()
                    .find(|o| o.cli.len() == 2 && o.cli[0] == first && o.cli[1] == verb)?,
                m,
            )
        }
    };
    Some((op, input_from(op, m)))
}

/// The default operation for a bare group.
pub fn default_for(group: &str) -> Option<&'static str> {
    match group {
        "mail" => Some("mail_brief"),
        "calendar" => Some("calendar_agenda"),
        _ => None,
    }
}

fn input_from(op: &OpDef, m: &ArgMatches) -> Value {
    let mut out = Map::new();
    for (name, prop) in op.properties() {
        let id = name.as_str();
        let present = m.try_contains_id(id).unwrap_or(false);
        if !present {
            continue;
        }
        match shape(&prop) {
            Shape::Bool => {
                if let Some(b) = m.get_one::<bool>(id) {
                    out.insert(name, Value::Bool(*b));
                }
            }
            Shape::Int => {
                if let Some(n) = m.get_one::<i64>(id) {
                    out.insert(name, Value::from(*n));
                }
            }
            Shape::List => {
                if let Some(vals) = m.get_many::<String>(id) {
                    let list: Vec<Value> = vals
                        .flat_map(|v| {
                            v.split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(|s| Value::String(s.to_string()))
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    if !list.is_empty() {
                        out.insert(name, Value::Array(list));
                    }
                }
            }
            Shape::Text => {
                if let Some(v) = m.get_one::<String>(id) {
                    // `-` reads the value from stdin (long bodies).
                    let v = if v == "-" {
                        let mut s = String::new();
                        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s);
                        s
                    } else {
                        v.clone()
                    };
                    out.insert(name, Value::String(v));
                }
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> (String, Value) {
        let ops = pidge_engine::registry::all();
        let mut cmd = Command::new("pidge");
        for c in commands(&ops) {
            cmd = cmd.subcommand(c);
        }
        let m = cmd.try_get_matches_from(args).unwrap();
        let (op, input) = resolve(&ops, &m).unwrap();
        (op.name.to_string(), input)
    }

    #[test]
    fn flags_and_positionals_become_the_tool_input() {
        let (op, input) = parse(&[
            "pidge", "mail", "act", "archive", "m1", "m2", "--kind", "receipt",
        ]);
        assert_eq!(op, "mail_act");
        assert_eq!(
            input,
            serde_json::json!({ "action": "archive", "ids": ["m1", "m2"], "kind": "receipt" })
        );
        let (op, input) = parse(&[
            "pidge", "mail", "find", "avtal", "--from", "anna", "--unread", "--limit", "5",
        ]);
        assert_eq!(op, "mail_find");
        assert_eq!(
            input,
            serde_json::json!({ "text": "avtal", "from": "anna", "unread": true, "limit": 5 })
        );
        let (op, input) = parse(&[
            "pidge",
            "calendar",
            "event",
            "update",
            "--id",
            "e3",
            "--add-attendees",
            "anna,bob",
        ]);
        assert_eq!(op, "calendar_event");
        assert_eq!(input["add_attendees"], serde_json::json!(["anna", "bob"]));
        let (_, input) = parse(&["pidge", "mail", "brief", "--mark-seen=false"]);
        assert_eq!(input, serde_json::json!({ "mark_seen": false }));
        let (op, _) = parse(&["pidge", "accounts"]);
        assert_eq!(op, "accounts");
    }

    #[test]
    fn every_operation_builds_a_valid_command() {
        for op in pidge_engine::registry::all() {
            command_for(&op).debug_assert();
        }
    }
}
