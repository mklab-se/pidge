//! `pidge calendar new` — create a calendar event.

use anyhow::{Result, anyhow};
use chrono::{Duration, Utc};
use chrono_tz::Tz;

use pidge_client::{AuthClient, GraphClient, graph::events::NewEvent};
use pidge_core::{Config, RecurrenceFreq, RecurrencePattern, RecurrenceRange, Weekday};

use crate::cli::CalendarNewArgs;
use crate::commands::time::parse_when;
use crate::output::resolve_tz;

pub async fn run(args: CalendarNewArgs, json: bool) -> Result<()> {
    let config = Config::load()?;
    let account = pick_account(&config, args.from.clone())?;
    let tz = resolve_tz(args.tz.as_deref());

    let title = args
        .title
        .clone()
        .ok_or_else(|| anyhow!("--title is required"))?;
    let start_s = args
        .start
        .clone()
        .ok_or_else(|| anyhow!("--start is required"))?;
    let start = parse_when(&start_s, &tz, Utc::now(), None)?;
    let end = match &args.end {
        Some(s) => parse_when(s, &tz, Utc::now(), Some(start))?,
        None => start + Duration::hours(1),
    };
    if end <= start {
        anyhow::bail!("--end must be after --start");
    }

    let body_text = read_body(args.body.clone(), args.body_file.clone())?;

    let recurrence = build_recurrence(&args)?;

    let new = NewEvent {
        subject: title,
        start,
        end,
        tz: tz.name().to_string(),
        all_day: args.all_day,
        location: args.location.clone(),
        body_text,
        required_attendees: args.invite.clone(),
        optional_attendees: args.invite_optional.clone(),
        recurrence,
        online_meeting: args.online,
    };

    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let cal_id = resolve_calendar(&graph, &account, args.calendar.as_deref(), &tz).await?;
    let id = graph
        .create_event(&account, cal_id.as_deref(), &new)
        .await?;

    if json {
        println!(
            "{}",
            serde_json::json!({ "id": id, "account": account, "ok": true })
        );
    } else {
        println!("Created event {id} on {account}.");
    }
    Ok(())
}

fn pick_account(config: &Config, override_from: Option<String>) -> Result<String> {
    override_from
        .or_else(|| config.defaults.calendar.clone())
        .or_else(|| config.defaults.send.clone())
        .or_else(|| config.accounts.first().map(|a| a.email.clone()))
        .ok_or_else(|| {
            anyhow!(
                "No account to send from. Pass --from or set a default with `pidge account default`."
            )
        })
}

fn read_body(body: Option<String>, body_file: Option<String>) -> Result<Option<String>> {
    match (body, body_file) {
        (Some(_), Some(_)) => anyhow::bail!("--body and --body-file are mutually exclusive"),
        (Some(b), _) => Ok(Some(b)),
        (_, Some(p)) if p == "-" => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(Some(s))
        }
        (_, Some(p)) => Ok(Some(std::fs::read_to_string(p)?)),
        _ => Ok(None),
    }
}

pub(crate) fn build_recurrence(args: &CalendarNewArgs) -> Result<Option<RecurrencePattern>> {
    let Some(repeat) = args.repeat.as_deref() else {
        return Ok(None);
    };
    let freq = match repeat {
        "daily" => RecurrenceFreq::Daily,
        "weekly" => RecurrenceFreq::Weekly,
        "monthly" => RecurrenceFreq::Monthly,
        "yearly" => RecurrenceFreq::Yearly,
        other => anyhow::bail!("--repeat must be daily|weekly|monthly|yearly (got '{other}')"),
    };
    let by_weekday: Vec<Weekday> = args
        .on
        .iter()
        .filter_map(|d| match d.to_lowercase().as_str() {
            "mon" | "monday" => Some(Weekday::Monday),
            "tue" | "tuesday" => Some(Weekday::Tuesday),
            "wed" | "wednesday" => Some(Weekday::Wednesday),
            "thu" | "thursday" => Some(Weekday::Thursday),
            "fri" | "friday" => Some(Weekday::Friday),
            "sat" | "saturday" => Some(Weekday::Saturday),
            "sun" | "sunday" => Some(Weekday::Sunday),
            _ => None,
        })
        .collect();
    if !args.on.is_empty() && by_weekday.is_empty() {
        anyhow::bail!("--on values must be mon|tue|wed|thu|fri|sat|sun");
    }
    let range = match (&args.until, args.count) {
        (Some(_), Some(_)) => anyhow::bail!("--until and --count are mutually exclusive"),
        (Some(s), _) => {
            let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| anyhow!("--until must be YYYY-MM-DD (got '{s}')"))?;
            RecurrenceRange::EndDate(d)
        }
        (_, Some(n)) => RecurrenceRange::Count(n),
        _ => RecurrenceRange::NoEnd,
    };
    Ok(Some(RecurrencePattern {
        freq,
        interval: args.interval,
        by_weekday,
        range,
    }))
}

async fn resolve_calendar(
    graph: &GraphClient,
    account: &str,
    name_or_id: Option<&str>,
    _tz: &Tz,
) -> Result<Option<String>> {
    let Some(name_or_id) = name_or_id else {
        return Ok(None);
    };
    let cals = graph.list_calendars(account).await?;
    cals.iter()
        .find(|c| c.id == name_or_id || c.name.eq_ignore_ascii_case(name_or_id))
        .map(|c| Some(c.id.clone()))
        .ok_or_else(|| anyhow!("Calendar '{name_or_id}' not found on {account}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CalendarNewArgs;

    fn args(repeat: Option<&str>) -> CalendarNewArgs {
        CalendarNewArgs {
            repeat: repeat.map(|s| s.into()),
            interval: 1,
            ..Default::default()
        }
    }

    #[test]
    fn no_repeat_returns_none() {
        assert!(build_recurrence(&args(None)).unwrap().is_none());
    }

    #[test]
    fn weekly_on_mwf_until_date() {
        let mut a = args(Some("weekly"));
        a.on = vec!["mon".into(), "wed".into(), "fri".into()];
        a.until = Some("2026-12-31".into());
        let r = build_recurrence(&a).unwrap().unwrap();
        assert_eq!(r.freq, RecurrenceFreq::Weekly);
        assert_eq!(r.by_weekday.len(), 3);
        assert!(matches!(r.range, RecurrenceRange::EndDate(_)));
    }

    #[test]
    fn count_range() {
        let mut a = args(Some("daily"));
        a.count = Some(10);
        let r = build_recurrence(&a).unwrap().unwrap();
        assert!(matches!(r.range, RecurrenceRange::Count(10)));
    }

    #[test]
    fn no_end_when_until_and_count_absent() {
        let a = args(Some("monthly"));
        let r = build_recurrence(&a).unwrap().unwrap();
        assert!(matches!(r.range, RecurrenceRange::NoEnd));
    }

    #[test]
    fn until_and_count_conflict() {
        let mut a = args(Some("daily"));
        a.until = Some("2026-12-31".into());
        a.count = Some(10);
        assert!(build_recurrence(&a).is_err());
    }

    #[test]
    fn unknown_repeat_errors() {
        let a = args(Some("hourly"));
        assert!(build_recurrence(&a).is_err());
    }

    #[test]
    fn bad_until_format_errors() {
        let mut a = args(Some("weekly"));
        a.until = Some("not-a-date".into());
        assert!(build_recurrence(&a).is_err());
    }

    #[test]
    fn bad_on_values_error() {
        let mut a = args(Some("weekly"));
        a.on = vec!["funday".into()];
        assert!(build_recurrence(&a).is_err());
    }
}
