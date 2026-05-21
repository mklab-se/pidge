//! `pidge calendar show <hash>` — display full details of one event.

use anyhow::Result;
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::commands::calendar_fragment;
use crate::commands::calendar_list::short_id_for;
use crate::commands::time::format_when;
use crate::output::resolve_tz;

pub async fn run(fragment: &str, json: bool) -> Result<()> {
    let (_hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let event = graph.get_event(&r.account, &r.event_id).await?;

    if json {
        #[derive(serde::Serialize)]
        struct EventOut<'a> {
            hash: String,
            #[serde(flatten)]
            event: &'a pidge_core::Event,
        }
        let out = EventOut {
            hash: short_id_for(&event),
            event: &event,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let tz = resolve_tz(None);
    println!("{}", event.subject.bold());
    println!("{} {}", "When: ".dimmed(), format_when(event.start.at, &tz));
    println!("{} {}", "Until:".dimmed(), format_when(event.end.at, &tz));
    if let Some(loc) = &event.location {
        println!("{} {}", "Where:".dimmed(), loc);
    }
    println!(
        "{} {} <{}>",
        "Org:  ".dimmed(),
        event.organizer.name,
        event.organizer.address
    );
    if !event.attendees.is_empty() {
        println!("{}", "Attendees:".dimmed());
        for a in &event.attendees {
            let kind = format!("{:?}", a.kind).to_lowercase();
            let resp = format!("{:?}", a.response).to_lowercase();
            println!("  - {} <{}>  ({kind}, {resp})", a.name, a.address);
        }
    }
    if let Some(url) = &event.online_meeting_url {
        println!("{} {}", "Join: ".dimmed(), url);
    }
    if let Some(r) = &event.recurrence {
        let freq = format!("{:?}", r.freq).to_lowercase();
        let range_label = match &r.range {
            pidge_core::RecurrenceRange::NoEnd => "forever".to_string(),
            pidge_core::RecurrenceRange::Count(n) => format!("{n} times"),
            pidge_core::RecurrenceRange::EndDate(d) => format!("until {d}"),
        };
        let days = if r.by_weekday.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = r
                .by_weekday
                .iter()
                .map(|w| format!("{w:?}").to_lowercase())
                .collect();
            format!(" on {}", names.join(","))
        };
        println!(
            "{} {freq} every {} interval{days}, {range_label}",
            "Repeat:".dimmed(),
            r.interval
        );
    }
    if !event.body_content.trim().is_empty() {
        println!();
        println!("{}", event.body_content.trim());
    }
    Ok(())
}
