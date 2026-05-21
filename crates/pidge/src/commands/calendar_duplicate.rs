//! `pidge calendar duplicate <hash>` — use an event as a template.

use anyhow::{Result, anyhow};
use chrono::{Duration, Utc};

use pidge_client::{AuthClient, GraphClient, graph::events::NewEvent};
use pidge_core::Config;

use crate::commands::calendar_fragment;
use crate::commands::time::parse_when;
use crate::output::resolve_tz;

pub async fn run(
    fragment: &str,
    start_s: Option<&str>,
    title: Option<&str>,
    calendar_name_or_id: Option<&str>,
    json: bool,
) -> Result<()> {
    let (_hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let cur = graph.get_event(&r.account, &r.event_id).await?;
    let tz = resolve_tz(Some(&cur.start.tz));
    let dur = cur.end.at - cur.start.at;
    let start = match start_s {
        Some(s) => parse_when(s, &tz, Utc::now(), None)?,
        None => cur.start.at + Duration::days(7),
    };

    let new = NewEvent {
        subject: title.map(|s| s.to_string()).unwrap_or(cur.subject.clone()),
        start,
        end: start + dur,
        tz: cur.start.tz.clone(),
        all_day: cur.all_day,
        location: cur.location.clone(),
        body_text: Some(cur.body_content.clone()),
        required_attendees: cur
            .attendees
            .iter()
            .filter(|a| matches!(a.kind, pidge_core::AttendeeKind::Required))
            .map(|a| a.address.clone())
            .collect(),
        optional_attendees: cur
            .attendees
            .iter()
            .filter(|a| matches!(a.kind, pidge_core::AttendeeKind::Optional))
            .map(|a| a.address.clone())
            .collect(),
        recurrence: None,
        online_meeting: cur.online_meeting_url.is_some(),
    };

    let cal_id = match calendar_name_or_id {
        Some(name) => {
            let cals = graph.list_calendars(&r.account).await?;
            let id = cals
                .iter()
                .find(|c| c.id == name || c.name.eq_ignore_ascii_case(name))
                .map(|c| c.id.clone())
                .ok_or_else(|| anyhow!("Calendar '{name}' not found"))?;
            Some(id)
        }
        None => None,
    };
    let id = graph
        .create_event(&r.account, cal_id.as_deref(), &new)
        .await?;
    if json {
        println!("{}", serde_json::json!({ "id": id, "ok": true }));
    } else {
        println!("Duplicated as {id}.");
    }
    Ok(())
}
