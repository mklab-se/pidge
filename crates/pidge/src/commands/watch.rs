//! `pidge watch` — a JSONL event stream for long-running agents.
//!
//! Polls mail/calendar delta streams on an interval and prints one JSON line
//! per event to stdout. Errors are emitted as events and the loop continues;
//! Ctrl-C exits cleanly. `--state-file` persists cursors so a restarted
//! watch resumes instead of re-bootstrapping.

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use pidge_client::graph::delta::{CalendarDeltaEvent, MailDeltaEvent};
use pidge_client::{AuthClient, ClientError, Cursor, GraphClient};
use pidge_core::Config;
use serde_json::json;

pub struct WatchArgs {
    pub mail: bool,
    pub calendar: bool,
    pub interval: u64,
    pub account: Vec<String>,
    pub folder: String,
    pub state_file: Option<std::path::PathBuf>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct WatchState {
    mail_cursor: Option<String>,
    calendar_cursor: Option<String>,
}

fn emit(value: serde_json::Value) {
    // One event per line — agents parse JSONL. Explicit flush: watch runs
    // under pipes where stdout is block-buffered and events must not lag.
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

pub async fn run(args: WatchArgs) -> Result<()> {
    let config = Config::load()?;
    let accounts: Vec<String> = if args.account.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        args.account.clone()
    };
    if accounts.is_empty() {
        return Err(anyhow!("No accounts signed in."));
    }
    // No flags = both streams.
    let (watch_mail, watch_calendar) = match (args.mail, args.calendar) {
        (false, false) => (true, true),
        pair => pair,
    };

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let mut state: WatchState = args
        .state_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    // Bootstrap missing cursors.
    if watch_mail && state.mail_cursor.is_none() {
        let mut cursor = Cursor::new("mail-delta");
        for email in &accounts {
            let (_msgs, link) = graph.mail_delta_bootstrap(email, &args.folder).await?;
            cursor.per_account.insert(email.clone(), Some(link));
        }
        state.mail_cursor = Some(cursor.encode());
        emit(json!({"stream": "watch", "type": "ready", "watching": "mail", "at": now()}));
    }
    if watch_calendar && state.calendar_cursor.is_none() {
        let start = Utc::now();
        let end = start + ChronoDuration::days(14);
        let mut cursor = Cursor::new("cal-delta");
        for email in &accounts {
            let (_events, link) = graph.calendar_delta_bootstrap(email, start, end).await?;
            cursor.per_account.insert(email.clone(), Some(link));
        }
        state.calendar_cursor = Some(cursor.encode());
        emit(json!({"stream": "watch", "type": "ready", "watching": "calendar", "at": now()}));
    }
    persist(&args.state_file, &state);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                emit(json!({"stream": "watch", "type": "stopped", "at": now()}));
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(args.interval)) => {}
        }

        if watch_mail {
            match poll_mail(&graph, state.mail_cursor.as_deref()).await {
                Ok(Some(next)) => state.mail_cursor = Some(next),
                Ok(None) => {}
                Err(e) => handle_poll_error("mail", &e, &mut state.mail_cursor),
            }
        }
        if watch_calendar {
            match poll_calendar(&graph, state.calendar_cursor.as_deref()).await {
                Ok(Some(next)) => state.calendar_cursor = Some(next),
                Ok(None) => {}
                Err(e) => handle_poll_error("calendar", &e, &mut state.calendar_cursor),
            }
        }
        persist(&args.state_file, &state);
    }
}

fn handle_poll_error(stream: &str, err: &anyhow::Error, cursor: &mut Option<String>) {
    // An expired delta stream resets itself: drop the cursor so the next
    // iteration re-bootstraps. Everything else is reported and retried.
    let expired = err.chain().any(|c| {
        matches!(
            c.downcast_ref::<ClientError>(),
            Some(ClientError::DeltaExpired)
        )
    });
    if expired {
        *cursor = None;
        emit(json!({"stream": stream, "type": "reset", "reason": "delta_expired", "at": now()}));
    } else {
        emit(
            json!({"stream": stream, "type": "error", "message": format!("{err:#}"), "at": now()}),
        );
    }
}

async fn poll_mail(graph: &GraphClient, cursor: Option<&str>) -> Result<Option<String>> {
    let Some(token) = cursor else { return Ok(None) };
    let cursor = Cursor::decode(token, "mail-delta")?;
    let mut next = Cursor::new("mail-delta");
    for (email, link) in &cursor.per_account {
        let Some(link) = link else { continue };
        let (changes, new_link) = graph.mail_delta(email, link).await?;
        next.per_account.insert(email.clone(), Some(new_link));
        for change in changes {
            match change {
                MailDeltaEvent::Changed(m) => emit(json!({
                    "stream": "mail", "type": "changed", "at": now(),
                    "message": {
                        "id": pidge_core::short_hash(&m.graph_id),
                        "account": email,
                        "from": m.from,
                        "subject": m.subject,
                        "received_at": m.received_at,
                        "is_read": m.is_read,
                        "preview": m.preview,
                    }
                })),
                MailDeltaEvent::Deleted { graph_id } => emit(json!({
                    "stream": "mail", "type": "deleted", "at": now(),
                    "message": {"id": pidge_core::short_hash(&graph_id), "account": email}
                })),
            }
        }
    }
    Ok(Some(next.encode()))
}

async fn poll_calendar(graph: &GraphClient, cursor: Option<&str>) -> Result<Option<String>> {
    let Some(token) = cursor else { return Ok(None) };
    let cursor = Cursor::decode(token, "cal-delta")?;
    let mut next = Cursor::new("cal-delta");
    for (email, link) in &cursor.per_account {
        let Some(link) = link else { continue };
        let (changes, new_link) = graph.calendar_delta(email, link).await?;
        next.per_account.insert(email.clone(), Some(new_link));
        for change in changes {
            match change {
                CalendarDeltaEvent::CreatedOrUpdated(e) => emit(json!({
                    "stream": "calendar", "type": "changed", "at": now(),
                    "event": {
                        "id": pidge_core::short_hash(&e.id),
                        "account": e.account,
                        "subject": e.subject,
                        "start": e.start.at,
                        "end": e.end.at,
                    }
                })),
                CalendarDeltaEvent::Deleted { graph_id } => emit(json!({
                    "stream": "calendar", "type": "deleted", "at": now(),
                    "event": {"id": pidge_core::short_hash(&graph_id), "account": email}
                })),
            }
        }
    }
    Ok(Some(next.encode()))
}

fn persist(path: &Option<std::path::PathBuf>, state: &WatchState) {
    if let Some(path) = path {
        if let Ok(text) = serde_json::to_string_pretty(state) {
            let _ = std::fs::write(path, text);
        }
    }
}
