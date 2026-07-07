//! `pidge mail delta` / `pidge calendar delta` — change feeds for agents.
//!
//! First call (no cursor) establishes state and returns a cursor; later calls
//! return only what changed plus a fresh cursor. Output is always JSON —
//! these commands exist for agents.

use anyhow::{Result, anyhow};
use chrono::{Duration, Utc};
use pidge_client::graph::delta::{CalendarDeltaEvent, MailDeltaEvent};
use pidge_client::{AuthClient, Cursor, GraphClient};
use pidge_core::Config;
use serde_json::{Value, json};

pub struct MailDeltaArgs {
    pub folder: String,
    pub cursor: Option<String>,
    pub account: Vec<String>,
    pub full: bool,
}

pub struct CalendarDeltaArgs {
    pub cursor: Option<String>,
    pub account: Vec<String>,
    pub days: i64,
}

fn target_accounts(config: &Config, filter: &[String]) -> Result<Vec<String>> {
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }
    if filter.is_empty() {
        return Ok(config.accounts.iter().map(|a| a.email.clone()).collect());
    }
    for f in filter {
        if config.find(f).is_none() {
            return Err(anyhow!("not signed in to {f}"));
        }
    }
    Ok(filter.to_vec())
}

fn message_json(m: &pidge_core::Message) -> Value {
    json!({
        "id": pidge_core::short_hash(&m.id),
        "graph_id": m.id,
        "account": m.account,
        "from": m.from,
        "subject": m.subject,
        "received_at": m.received_at,
        "is_read": m.is_read,
        "preview": m.preview,
        "has_attachments": m.has_attachments,
    })
}

fn event_json(e: &pidge_core::Event) -> Value {
    json!({
        "id": pidge_core::short_hash(&e.id),
        "graph_id": e.id,
        "account": e.account,
        "subject": e.subject,
        "start": e.start.at,
        "end": e.end.at,
        "location": e.location,
    })
}

pub async fn mail(args: MailDeltaArgs) -> Result<()> {
    let config = Config::load()?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    match args.cursor {
        None => {
            // Bootstrap: establish per-account delta state.
            let accounts = target_accounts(&config, &args.account)?;
            let mut cursor = Cursor::new("mail-delta");
            let mut events: Vec<Value> = Vec::new();
            for email in &accounts {
                let (messages, delta_link) =
                    graph.mail_delta_bootstrap(email, &args.folder).await?;
                cursor.per_account.insert(email.clone(), Some(delta_link));
                if args.full {
                    events.extend(
                        messages
                            .iter()
                            .map(|m| json!({"type": "created", "message": message_json(m)})),
                    );
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "events": events,
                    "next_cursor": cursor.encode(),
                }))?
            );
        }
        Some(token) => {
            let cursor = Cursor::decode(&token, "mail-delta")?;
            let mut next = Cursor::new("mail-delta");
            let mut events: Vec<Value> = Vec::new();
            for (email, link) in &cursor.per_account {
                let Some(link) = link else { continue };
                let (changes, new_link) = graph.mail_delta(email, link).await?;
                next.per_account.insert(email.clone(), Some(new_link));
                for change in changes {
                    events.push(match change {
                        MailDeltaEvent::Changed(m) => {
                            let mut obj = json!({
                                "id": pidge_core::short_hash(&m.graph_id),
                                "graph_id": m.graph_id,
                                "account": email,
                            });
                            let fields = obj.as_object_mut().expect("object");
                            if let Some(v) = &m.subject {
                                fields.insert("subject".into(), json!(v));
                            }
                            if let Some(v) = m.is_read {
                                fields.insert("is_read".into(), json!(v));
                            }
                            if let Some(v) = &m.received_at {
                                fields.insert("received_at".into(), json!(v));
                            }
                            if let Some(v) = &m.preview {
                                fields.insert("preview".into(), json!(v));
                            }
                            if let Some(v) = &m.from {
                                fields.insert("from".into(), v.clone());
                            }
                            if let Some(v) = &m.conversation_id {
                                fields.insert("conversation_id".into(), json!(v));
                            }
                            json!({"type": "changed", "message": obj})
                        }
                        MailDeltaEvent::Deleted { graph_id } => json!({
                            "type": "deleted",
                            "message": {
                                "id": pidge_core::short_hash(&graph_id),
                                "graph_id": graph_id,
                                "account": email,
                            }
                        }),
                    });
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "events": events,
                    "next_cursor": next.encode(),
                }))?
            );
        }
    }
    Ok(())
}

pub async fn calendar(args: CalendarDeltaArgs) -> Result<()> {
    let config = Config::load()?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    match args.cursor {
        None => {
            let accounts = target_accounts(&config, &args.account)?;
            let start = Utc::now();
            let end = start + Duration::days(args.days);
            let mut cursor = Cursor::new("cal-delta");
            for email in &accounts {
                let (_events, delta_link) =
                    graph.calendar_delta_bootstrap(email, start, end).await?;
                cursor.per_account.insert(email.clone(), Some(delta_link));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "events": [],
                    "next_cursor": cursor.encode(),
                }))?
            );
        }
        Some(token) => {
            let cursor = Cursor::decode(&token, "cal-delta")?;
            let mut next = Cursor::new("cal-delta");
            let mut events: Vec<Value> = Vec::new();
            for (email, link) in &cursor.per_account {
                let Some(link) = link else { continue };
                let (changes, new_link) = graph.calendar_delta(email, link).await?;
                next.per_account.insert(email.clone(), Some(new_link));
                for change in changes {
                    events.push(match change {
                        CalendarDeltaEvent::CreatedOrUpdated(e) => {
                            json!({"type": "changed", "event": event_json(&e)})
                        }
                        CalendarDeltaEvent::Deleted { graph_id } => json!({
                            "type": "deleted",
                            "event": {
                                "id": pidge_core::short_hash(&graph_id),
                                "graph_id": graph_id,
                                "account": email,
                            }
                        }),
                    });
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "events": events,
                    "next_cursor": next.encode(),
                }))?
            );
        }
    }
    Ok(())
}
