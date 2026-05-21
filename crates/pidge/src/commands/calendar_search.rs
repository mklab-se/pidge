//! `pidge calendar search <query>` — substring search across calendar events.
//!
//! Microsoft Graph's `$search` parameter returns 501 on the `/me/events`
//! resource, so we fetch the `calendarView` over a date window and filter
//! client-side. Default window: 1 year back, 1 year forward — override with
//! `--from` / `--to` for narrower or wider searches.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::{CachedEventRef, Config, Event, EventCache};

use crate::commands::calendar_list::events_to_json;
use crate::commands::time::{format_when, parse_when};
use crate::output::resolve_tz;

/// Per-account fetch ceiling fed to `calendarView`. Searches operate over
/// the resulting list client-side, so this caps how much history/lookahead
/// any single account contributes — 500 events per ±1y window covers most
/// personal calendars without ballooning latency.
const PER_ACCOUNT_FETCH_CAP: usize = 500;

pub async fn run(
    query: String,
    accounts_filter: Vec<String>,
    _calendar: Option<String>,
    limit: usize,
    from: Option<String>,
    to: Option<String>,
    json: bool,
) -> Result<()> {
    let config = Config::load()?;
    let accounts: Vec<_> = if accounts_filter.is_empty() {
        config.accounts.clone()
    } else {
        config
            .accounts
            .iter()
            .filter(|a| accounts_filter.iter().any(|f| f == &a.email))
            .cloned()
            .collect()
    };
    if accounts.is_empty() {
        anyhow::bail!("No signed-in accounts.");
    }
    let tz = resolve_tz(None);
    let now = Utc::now();
    let (window_start, window_end) = resolve_window(from.as_deref(), to.as_deref(), now, &tz)?;

    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let mut matches: Vec<Event> = Vec::new();
    for a in &accounts {
        let page = graph
            .list_calendar_view(
                &a.email,
                None,
                window_start,
                window_end,
                PER_ACCOUNT_FETCH_CAP,
            )
            .await?;
        for ev in page.events {
            if matches_query(&ev, &query) {
                matches.push(ev);
                if matches.len() >= limit {
                    break;
                }
            }
        }
        if matches.len() >= limit {
            break;
        }
    }

    let mut cache = EventCache::load()?;
    let refs: Vec<CachedEventRef> = matches
        .iter()
        .map(|e| CachedEventRef::new(e.id.clone(), e.calendar_id.clone(), e.account.clone()))
        .collect();
    cache.insert_many(&refs);
    cache.save()?;

    if json {
        println!("{}", events_to_json(&matches)?);
        return Ok(());
    }
    if matches.is_empty() {
        println!("{}", "No matching events.".dimmed());
        return Ok(());
    }
    for e in &matches {
        let id = pidge_core::short_hash(&format!("{}|{}", e.account, e.id));
        println!(
            "{}  {}  {}",
            id.dimmed(),
            format_when(e.start.at, &tz).cyan(),
            e.subject
        );
    }
    Ok(())
}

fn resolve_window(
    from: Option<&str>,
    to: Option<&str>,
    now: DateTime<Utc>,
    tz: &chrono_tz::Tz,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let start = match from {
        Some(s) => parse_when(s, tz, now, None)?,
        None => now - Duration::days(365),
    };
    let end = match to {
        Some(s) => parse_when(s, tz, now, None)?,
        None => now + Duration::days(365),
    };
    if end <= start {
        anyhow::bail!("--to must be after --from");
    }
    Ok((start, end))
}

/// Case-insensitive substring search across an event's user-visible fields.
/// Empty / whitespace-only queries match every event.
fn matches_query(event: &Event, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let n = needle.as_str();
    let contains = |s: &str| s.to_lowercase().contains(n);
    if contains(&event.subject) {
        return true;
    }
    if contains(&event.body_preview) {
        return true;
    }
    if let Some(loc) = &event.location
        && contains(loc)
    {
        return true;
    }
    if contains(&event.organizer.name) || contains(&event.organizer.address) {
        return true;
    }
    event
        .attendees
        .iter()
        .any(|a| contains(&a.name) || contains(&a.address))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use pidge_core::{Attendee, AttendeeKind, EventTime, ResponseStatus};

    fn ev() -> Event {
        Event {
            account: "u@example.com".into(),
            calendar_id: "primary".into(),
            id: "GRAPH_ID".into(),
            subject: "Quarterly Budget Review".into(),
            start: EventTime {
                at: Utc.with_ymd_and_hms(2026, 5, 21, 14, 0, 0).unwrap(),
                tz: "UTC".into(),
            },
            end: EventTime {
                at: Utc.with_ymd_and_hms(2026, 5, 21, 15, 0, 0).unwrap(),
                tz: "UTC".into(),
            },
            all_day: false,
            location: Some("Conference Room Atlas".into()),
            organizer: Attendee {
                name: "Alice Anders".into(),
                address: "alice@example.com".into(),
                kind: AttendeeKind::Required,
                response: ResponseStatus::Organizer,
            },
            attendees: vec![Attendee {
                name: "Bob Brown".into(),
                address: "bob@partner.io".into(),
                kind: AttendeeKind::Required,
                response: ResponseStatus::None,
            }],
            body_preview: "Agenda: Q2 numbers and CapEx asks.".into(),
            body_content: String::new(),
            body_content_type: Default::default(),
            recurrence: None,
            is_organizer: false,
            response_status: ResponseStatus::None,
            online_meeting_url: None,
            series_master_id: None,
        }
    }

    #[test]
    fn matches_subject_case_insensitively() {
        assert!(matches_query(&ev(), "budget"));
        assert!(matches_query(&ev(), "BUDGET"));
        assert!(matches_query(&ev(), "Quarterly"));
    }

    #[test]
    fn matches_body_preview() {
        assert!(matches_query(&ev(), "capex"));
    }

    #[test]
    fn matches_location() {
        assert!(matches_query(&ev(), "atlas"));
    }

    #[test]
    fn matches_organizer_name_or_address() {
        assert!(matches_query(&ev(), "alice"));
        assert!(matches_query(&ev(), "@example.com"));
    }

    #[test]
    fn matches_attendee_name_or_address() {
        assert!(matches_query(&ev(), "bob"));
        assert!(matches_query(&ev(), "partner.io"));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches_query(&ev(), ""));
        assert!(matches_query(&ev(), "   "));
    }

    #[test]
    fn no_match_returns_false() {
        assert!(!matches_query(&ev(), "tunafish"));
    }
}
