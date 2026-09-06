//! `pidge calendar list` — list events across accounts in a time window.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::{CachedEventRef, Config, Event, EventCache};

use crate::commands::time::{format_when, parse_when};
use crate::output::resolve_tz;

#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub fn compute_window(
    tz: &Tz,
    now: DateTime<Utc>,
    today: bool,
    tomorrow: bool,
    week: bool,
    month: bool,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Window> {
    let today_date = now.with_timezone(tz).date_naive();
    let (s, e) = if today {
        day_range(today_date, tz)
    } else if tomorrow {
        day_range(today_date + Duration::days(1), tz)
    } else if week {
        range(today_date, today_date + Duration::days(7), tz)
    } else if month {
        range(today_date, today_date + Duration::days(30), tz)
    } else if let (Some(f), Some(t)) = (from, to) {
        (parse_when(f, tz, now, None)?, parse_when(t, tz, now, None)?)
    } else if let Some(f) = from {
        let s = parse_when(f, tz, now, None)?;
        (s, s + Duration::days(7))
    } else {
        range(today_date, today_date + Duration::days(7), tz)
    };
    if e <= s {
        return Err(anyhow!("--to must be after --from"));
    }
    Ok(Window { start: s, end: e })
}

fn day_range(d: NaiveDate, tz: &Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    let s = tz
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap()
        .with_timezone(&Utc);
    (s, s + Duration::days(1))
}
fn range(s: NaiveDate, e: NaiveDate, tz: &Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = tz
        .from_local_datetime(&s.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let end = tz
        .from_local_datetime(&e.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap()
        .with_timezone(&Utc);
    (start, end)
}

pub async fn run_default(json: bool) -> Result<()> {
    run(
        vec![],
        None,
        None,
        false,
        false,
        false,
        false,
        None,
        50,
        false,
        false,
        json,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    accounts_filter: Vec<String>,
    from: Option<String>,
    to: Option<String>,
    today: bool,
    tomorrow: bool,
    week: bool,
    month: bool,
    calendar: Option<String>,
    limit: usize,
    compact: bool,
    table: bool,
    json: bool,
    cursor: Option<String>,
) -> Result<()> {
    // Continuation: pull the next page per non-exhausted account.
    if let Some(token) = cursor {
        let cursor = pidge_client::Cursor::decode(&token, "cal")?;
        let graph = GraphClient::new(AuthClient::from_env()?)?;
        let tz = resolve_tz(None);
        let mut all: Vec<Event> = Vec::new();
        let mut next = pidge_client::Cursor::new("cal");
        for (email, link) in &cursor.per_account {
            let Some(link) = link else { continue };
            let page = graph.list_events_at(email, link).await?;
            next.per_account.insert(email.clone(), page.next_link);
            all.extend(page.events);
        }
        all.sort_by_key(|e| e.start.at);
        refresh_cache(&all)?;
        if json {
            return print_json_with_cursor(&all, (!next.exhausted()).then(|| next.encode()));
        }
        print_compact(&all, &tz);
        return Ok(());
    }

    let config = Config::load()?;
    let accounts = select_accounts(&config, &accounts_filter)?;
    if accounts.is_empty() {
        anyhow::bail!("No signed-in accounts. Run `pidge account add` first.");
    }

    let tz = resolve_tz(None);
    let window = compute_window(
        &tz,
        Utc::now(),
        today,
        tomorrow,
        week,
        month,
        from.as_deref(),
        to.as_deref(),
    )?;

    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;

    let mut all: Vec<Event> = Vec::new();
    let mut next = pidge_client::Cursor::new("cal");
    for acct in &accounts {
        let cal_id = resolve_calendar(&graph, &acct.email, calendar.as_deref()).await?;
        let page = graph
            .list_calendar_view(
                &acct.email,
                cal_id.as_deref(),
                window.start,
                window.end,
                limit,
            )
            .await?;
        next.per_account.insert(acct.email.clone(), page.next_link);
        all.extend(page.events);
    }
    all.sort_by_key(|e| e.start.at);
    refresh_cache(&all)?;

    if json {
        print_json_with_cursor(&all, (!next.exhausted()).then(|| next.encode()))?;
    } else if table {
        print_table(&all, &tz);
    } else if compact {
        print_compact(&all, &tz);
    } else {
        print_cards(&all, &tz);
    }
    Ok(())
}

fn select_accounts(config: &Config, filter: &[String]) -> Result<Vec<pidge_core::Account>> {
    if filter.is_empty() {
        return Ok(config.accounts.clone());
    }
    Ok(config
        .accounts
        .iter()
        .filter(|a| filter.iter().any(|f| f == &a.email))
        .cloned()
        .collect())
}

async fn resolve_calendar(
    graph: &GraphClient,
    account: &str,
    name_or_id: Option<&str>,
) -> Result<Option<String>> {
    let Some(name_or_id) = name_or_id else {
        return Ok(None);
    };
    let cals = graph.list_calendars(account).await?;
    if let Some(c) = cals
        .iter()
        .find(|c| c.id == name_or_id || c.name.eq_ignore_ascii_case(name_or_id))
    {
        Ok(Some(c.id.clone()))
    } else {
        anyhow::bail!(
            "Calendar '{name_or_id}' not found on account {account}. \
             Try `pidge calendar calendars`."
        );
    }
}

fn refresh_cache(events: &[Event]) -> Result<()> {
    let mut cache = EventCache::load()?;
    let refs: Vec<CachedEventRef> = events
        .iter()
        .map(|e| CachedEventRef::new(e.id.clone(), e.calendar_id.clone(), e.account.clone()))
        .collect();
    cache.insert_many(&refs);
    cache.save()?;
    Ok(())
}

fn print_json_with_cursor(events: &[Event], next_cursor: Option<String>) -> Result<()> {
    match next_cursor {
        Some(cursor) => {
            let items: serde_json::Value = serde_json::from_str(&events_to_json(events)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "items": items,
                    "next_cursor": cursor,
                }))?
            );
            Ok(())
        }
        None => print_json(events),
    }
}

fn print_json(events: &[Event]) -> Result<()> {
    println!("{}", events_to_json(events)?);
    Ok(())
}

/// Serialize events to pretty JSON with an added `hash` field per event so
/// agents can take the result of `calendar list --json` and feed it straight
/// into follow-up commands (`show`, `move-time`, …) without a second call to
/// pidge to recover the short hash.
pub(crate) fn events_to_json(events: &[Event]) -> Result<String> {
    #[derive(serde::Serialize)]
    struct EventOut<'a> {
        hash: String,
        #[serde(flatten)]
        event: &'a Event,
    }
    let out: Vec<EventOut<'_>> = events
        .iter()
        .map(|e| EventOut {
            hash: short_id_for(e),
            event: e,
        })
        .collect();
    Ok(serde_json::to_string_pretty(&out)?)
}

fn print_compact(events: &[Event], tz: &Tz) {
    for e in events {
        println!(
            "{}  {}  {}",
            short_id_for(e),
            format_when(e.start.at, tz),
            e.subject
        );
    }
}

fn print_table(events: &[Event], tz: &Tz) {
    use comfy_table::{ContentArrangement, Table};
    let mut t = Table::new();
    t.load_style(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    t.set_content_arrangement(ContentArrangement::Dynamic);
    t.set_header(vec!["ID", "WHEN", "TITLE", "LOCATION"]);
    for e in events {
        t.add_row(vec![
            short_id_for(e),
            format_when(e.start.at, tz),
            e.subject.clone(),
            e.location.clone().unwrap_or_default(),
        ]);
    }
    println!("{t}");
}

fn print_cards(events: &[Event], tz: &Tz) {
    if events.is_empty() {
        println!("{}", "No events in this window.".dimmed());
        return;
    }
    for e in events {
        let id = short_id_for(e);
        let when = format_when(e.start.at, tz);
        println!("{}  {}  {}", id.dimmed(), when.cyan(), e.subject.bold());
        if let Some(loc) = &e.location {
            println!("    {} {}", "@".dimmed(), loc);
        }
        if !e.attendees.is_empty() {
            let names: Vec<String> = e
                .attendees
                .iter()
                .take(4)
                .map(|a| a.address.clone())
                .collect();
            let suffix = if e.attendees.len() > 4 {
                format!(", +{} more", e.attendees.len() - 4)
            } else {
                String::new()
            };
            println!("    {} {}{}", "with".dimmed(), names.join(", "), suffix);
        }
        if let Some(url) = &e.online_meeting_url {
            println!("    {} {}", "join".dimmed(), url);
        }
        println!();
    }
}

pub fn short_id_for(e: &Event) -> String {
    pidge_core::short_hash(&format!("{}|{}", e.account, e.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Stockholm;
    use pidge_core::{Attendee, EventTime};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn sample_event() -> Event {
        Event {
            account: "user@example.com".into(),
            calendar_id: "primary".into(),
            id: "AAMkAGRAW_GRAPH_ID".into(),
            subject: "Sync".into(),
            start: EventTime {
                at: now(),
                tz: "UTC".into(),
            },
            end: EventTime {
                at: now() + Duration::hours(1),
                tz: "UTC".into(),
            },
            all_day: false,
            location: None,
            organizer: Attendee {
                name: "Organizer".into(),
                address: "organizer@example.com".into(),
                kind: Default::default(),
                response: Default::default(),
            },
            attendees: vec![],
            body_preview: String::new(),
            body_content: String::new(),
            body_content_type: Default::default(),
            recurrence: None,
            is_organizer: true,
            response_status: Default::default(),
            online_meeting_url: None,
            series_master_id: None,
        }
    }

    #[test]
    fn json_output_includes_short_hash_per_event() {
        let event = sample_event();
        let expected_hash = short_id_for(&event);
        let json = events_to_json(std::slice::from_ref(&event)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = v.as_array().expect("top level is an array");
        assert_eq!(arr.len(), 1);
        let hash = arr[0]
            .get("hash")
            .and_then(|h| h.as_str())
            .expect("hash field present");
        assert_eq!(hash.len(), 8, "short hash is exactly 8 chars");
        assert_eq!(hash, expected_hash);
        // Existing fields still present (non-breaking).
        assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some(&*event.id));
        assert_eq!(
            arr[0].get("subject").and_then(|v| v.as_str()),
            Some(&*event.subject)
        );
    }

    #[test]
    fn today_window_spans_one_local_day() {
        let w = compute_window(&Stockholm, now(), true, false, false, false, None, None).unwrap();
        assert_eq!(w.start.to_rfc3339(), "2026-05-19T22:00:00+00:00");
        assert_eq!(w.end.to_rfc3339(), "2026-05-20T22:00:00+00:00");
    }

    #[test]
    fn week_window_is_seven_days() {
        let w = compute_window(&Stockholm, now(), false, false, true, false, None, None).unwrap();
        assert_eq!((w.end - w.start).num_days(), 7);
    }

    #[test]
    fn month_window_is_thirty_days() {
        let w = compute_window(&Stockholm, now(), false, false, false, true, None, None).unwrap();
        assert_eq!((w.end - w.start).num_days(), 30);
    }

    #[test]
    fn default_window_is_today_plus_seven() {
        let w = compute_window(&Stockholm, now(), false, false, false, false, None, None).unwrap();
        assert_eq!((w.end - w.start).num_days(), 7);
    }

    #[test]
    fn explicit_from_to_overrides_canned_windows() {
        let w = compute_window(
            &Stockholm,
            now(),
            false,
            false,
            false,
            false,
            Some("2026-06-01"),
            Some("2026-06-30"),
        )
        .unwrap();
        // 2026-06-01 to 2026-06-30 — at least 28 days
        assert!((w.end - w.start).num_days() >= 28);
    }

    #[test]
    fn reversed_from_to_errors() {
        let err = compute_window(
            &Stockholm,
            now(),
            false,
            false,
            false,
            false,
            Some("2026-06-30"),
            Some("2026-06-01"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be after"));
    }
}
