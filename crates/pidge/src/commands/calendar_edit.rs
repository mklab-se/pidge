//! `pidge calendar edit <hash>` — update an event.

use anyhow::Result;
use chrono::Utc;

use pidge_client::{AuthClient, GraphClient, graph::events::NewEvent};
use pidge_core::Config;

use crate::cli::CalendarEditArgs;
use crate::commands::calendar_fragment;
use crate::commands::time::parse_when;
use crate::output::resolve_tz;

pub async fn run(fragment: &str, args: CalendarEditArgs, json: bool) -> Result<()> {
    let (_hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let cur = graph.get_event(&r.account, &r.event_id).await?;

    let mut event_id = r.event_id.clone();
    if args.series {
        if let Some(master) = &cur.series_master_id {
            event_id = master.clone();
        }
    }

    let tz_name = args.tz.clone().unwrap_or_else(|| cur.start.tz.clone());
    let tz = resolve_tz(Some(&tz_name));

    let start = match &args.start {
        Some(s) => parse_when(s, &tz, Utc::now(), None)?,
        None => cur.start.at,
    };
    let end = match &args.end {
        Some(s) => parse_when(s, &tz, Utc::now(), Some(start))?,
        None => cur.end.at,
    };
    if end <= start {
        anyhow::bail!("--end must be after --start");
    }

    let body_text = match (args.body.clone(), args.body_file.clone()) {
        (Some(_), Some(_)) => anyhow::bail!("--body and --body-file are mutually exclusive"),
        (Some(b), _) => Some(b),
        (_, Some(p)) if p == "-" => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Some(s)
        }
        (_, Some(p)) => Some(std::fs::read_to_string(p)?),
        _ => Some(cur.body_content.clone()),
    };

    let required_attendees: Vec<String> = if args.invite.is_empty() {
        cur.attendees
            .iter()
            .filter(|a| matches!(a.kind, pidge_core::AttendeeKind::Required))
            .map(|a| a.address.clone())
            .collect()
    } else {
        args.invite.clone()
    };
    let optional_attendees: Vec<String> = if args.invite_optional.is_empty() {
        cur.attendees
            .iter()
            .filter(|a| matches!(a.kind, pidge_core::AttendeeKind::Optional))
            .map(|a| a.address.clone())
            .collect()
    } else {
        args.invite_optional.clone()
    };

    let new = NewEvent {
        subject: args.title.clone().unwrap_or_else(|| cur.subject.clone()),
        start,
        end,
        tz: tz_name,
        all_day: cur.all_day,
        location: args.location.clone().or_else(|| cur.location.clone()),
        body_text,
        required_attendees,
        optional_attendees,
        recurrence: cur.recurrence.clone(),
        online_meeting: cur.online_meeting_url.is_some(),
    };

    graph.update_event(&r.account, &event_id, &new).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "id": event_id, "account": r.account })
        );
    } else {
        println!("Updated event {} on {}.", short(&event_id), r.account);
    }
    // Graph sends update notices automatically when attendees change; the
    // --notify / --no-notify flags are surfaced but we have nothing extra to
    // do for them today.
    let _ = (args.notify, args.no_notify);
    Ok(())
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}
