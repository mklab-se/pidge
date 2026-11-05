//! Google Calendar v3 events: JSON shapes, mapping onto [`Event`], and the
//! request bodies for create, patch and RSVP.

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use pidge_core::provider::{EventPatch, EventSpec, EventWhen, Rsvp};
use pidge_core::{
    Attendee, AttendeeKind, BodyContentType, Event, EventTime, MessageFrom, RecurrenceFreq,
    RecurrencePattern, RecurrenceRange, ResponseStatus, Weekday,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// The calendar pidge reads and writes.
pub(crate) const PRIMARY: &str = "primary";

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventList {
    #[serde(default)]
    pub items: Vec<GEvent>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// The calendar's own time zone.
    #[serde(default)]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GEvent {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub start: GTime,
    #[serde(default)]
    pub end: GTime,
    #[serde(default)]
    pub organizer: Option<GPerson>,
    #[serde(default)]
    pub attendees: Vec<GPerson>,
    #[serde(default)]
    pub hangout_link: Option<String>,
    #[serde(default)]
    pub conference_data: Option<ConferenceData>,
    #[serde(default)]
    pub recurring_event_id: Option<String>,
    #[serde(default)]
    pub recurrence: Vec<String>,
    #[serde(default)]
    pub reminders: Option<Reminders>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GTime {
    #[serde(default)]
    pub date: Option<NaiveDate>,
    #[serde(default)]
    pub date_time: Option<DateTime<chrono::FixedOffset>>,
    #[serde(default)]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GPerson {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, rename = "self")]
    pub is_self: bool,
    #[serde(default)]
    pub organizer: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub resource: bool,
    #[serde(default)]
    pub response_status: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConferenceData {
    #[serde(default)]
    pub entry_points: Vec<EntryPoint>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntryPoint {
    #[serde(default)]
    pub entry_point_type: String,
    #[serde(default)]
    pub uri: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reminders {
    #[serde(default)]
    pub use_default: bool,
    #[serde(default)]
    pub overrides: Vec<ReminderOverride>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct ReminderOverride {
    #[serde(default)]
    pub minutes: u32,
}

fn parse_response(s: Option<&str>) -> ResponseStatus {
    match s {
        Some("accepted") => ResponseStatus::Accepted,
        Some("tentative") => ResponseStatus::Tentative,
        Some("declined") => ResponseStatus::Declined,
        Some("needsAction") => ResponseStatus::NotResponded,
        _ => ResponseStatus::None,
    }
}

fn person(p: &GPerson) -> Attendee {
    Attendee {
        name: p.display_name.clone().unwrap_or_default(),
        address: p.email.clone(),
        kind: if p.resource {
            AttendeeKind::Resource
        } else if p.optional {
            AttendeeKind::Optional
        } else {
            AttendeeKind::Required
        },
        response: if p.organizer {
            ResponseStatus::Organizer
        } else {
            parse_response(p.response_status.as_deref())
        },
    }
}

/// Midnight of `date` in `tz` (UTC when the zone is unknown).
fn midnight(date: NaiveDate, tz: &str) -> DateTime<Utc> {
    let naive = date.and_time(NaiveTime::MIN);
    tz.parse::<chrono_tz::Tz>()
        .ok()
        .and_then(|z| z.from_local_datetime(&naive).earliest())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
}

fn event_time(t: &GTime, default_tz: &str) -> EventTime {
    let tz = t
        .time_zone
        .clone()
        .filter(|z| !z.is_empty())
        .unwrap_or_else(|| default_tz.to_string());
    let at = match (t.date_time, t.date) {
        (Some(dt), _) => dt.with_timezone(&Utc),
        (None, Some(d)) => midnight(d, &tz),
        (None, None) => DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"),
    };
    EventTime { at, tz }
}

fn weekday(code: &str) -> Option<Weekday> {
    // BYDAY may carry an ordinal (`1MO`, `-1FR`): keep the day.
    let day = code.trim_start_matches(|c: char| c == '-' || c == '+' || c.is_ascii_digit());
    Some(match day {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

/// The `RRULE:` line of a series master as a [`RecurrencePattern`].
pub(crate) fn parse_rrule(lines: &[String]) -> Option<RecurrencePattern> {
    let rule = lines.iter().find_map(|l| l.strip_prefix("RRULE:"))?;
    let mut freq = None;
    let mut interval = 1;
    let mut by_weekday = Vec::new();
    let mut range = RecurrenceRange::NoEnd;
    for part in rule.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k {
            "FREQ" => {
                freq = Some(match v {
                    "DAILY" => RecurrenceFreq::Daily,
                    "WEEKLY" => RecurrenceFreq::Weekly,
                    "MONTHLY" => RecurrenceFreq::Monthly,
                    "YEARLY" => RecurrenceFreq::Yearly,
                    _ => return None,
                })
            }
            "INTERVAL" => interval = v.parse().unwrap_or(1),
            "BYDAY" => by_weekday = v.split(',').filter_map(weekday).collect(),
            "COUNT" => {
                if let Ok(n) = v.parse() {
                    range = RecurrenceRange::Count(n);
                }
            }
            "UNTIL" => {
                if let Some(d) = v
                    .get(..8)
                    .and_then(|d| NaiveDate::parse_from_str(d, "%Y%m%d").ok())
                {
                    range = RecurrenceRange::EndDate(d);
                }
            }
            _ => {}
        }
    }
    Some(RecurrencePattern {
        freq: freq?,
        interval,
        by_weekday,
        range,
    })
}

fn looks_like_html(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    [
        "<br", "<p>", "<p ", "<div", "<a ", "<b>", "<i>", "<ul", "<span", "<html",
    ]
    .iter()
    .any(|t| l.contains(t))
}

/// Google's event as pidge's. `default_tz` is the calendar's zone (used
/// for all-day events and when an event names none).
pub(crate) fn to_event(account: &str, g: &GEvent, default_tz: &str) -> Event {
    let organizer = g.organizer.as_ref().map(person).unwrap_or(Attendee {
        name: String::new(),
        address: String::new(),
        kind: AttendeeKind::Required,
        response: ResponseStatus::Organizer,
    });
    let organizer = Attendee {
        response: ResponseStatus::Organizer,
        ..organizer
    };
    let is_organizer = g.organizer.as_ref().is_some_and(|o| o.is_self);
    let response_status = if is_organizer {
        ResponseStatus::Organizer
    } else {
        g.attendees
            .iter()
            .find(|a| a.is_self)
            .map(|a| parse_response(a.response_status.as_deref()))
            .unwrap_or_default()
    };
    let description = g.description.clone().unwrap_or_default();
    let body_content_type = if looks_like_html(&description) {
        BodyContentType::Html
    } else {
        BodyContentType::Text
    };
    let body_preview: String = description.chars().take(255).collect();
    let online_meeting_url = g.hangout_link.clone().or_else(|| {
        g.conference_data.as_ref().and_then(|c| {
            c.entry_points
                .iter()
                .find(|e| e.entry_point_type == "video")
                .map(|e| e.uri.clone())
        })
    });
    Event {
        account: account.to_string(),
        calendar_id: PRIMARY.to_string(),
        id: g.id.clone(),
        subject: g.summary.clone().unwrap_or_default(),
        start: event_time(&g.start, default_tz),
        end: event_time(&g.end, default_tz),
        all_day: g.start.date.is_some() && g.start.date_time.is_none(),
        location: g.location.clone().filter(|l| !l.trim().is_empty()),
        organizer,
        attendees: g.attendees.iter().map(person).collect(),
        body_preview,
        body_content: description,
        body_content_type,
        recurrence: parse_rrule(&g.recurrence),
        is_organizer,
        response_status,
        online_meeting_url,
        series_master_id: g.recurring_event_id.clone(),
        reminder_minutes: g.reminders.as_ref().and_then(|r| {
            if r.use_default {
                None
            } else {
                r.overrides.iter().map(|o| o.minutes).min()
            }
        }),
    }
}

/// `start`/`end` objects for a time. Timed: the instant in its zone (an
/// unknown zone falls back to UTC). All-day: Google's end date is
/// exclusive, pidge's `last` inclusive.
fn when_json(when: &EventWhen) -> (Value, Value) {
    match when {
        EventWhen::Timed { start, end } => (time_json(start), time_json(end)),
        EventWhen::AllDay { first, last } => {
            let end = last.succ_opt().unwrap_or(*last);
            (
                json!({ "date": first.format("%Y-%m-%d").to_string() }),
                json!({ "date": end.format("%Y-%m-%d").to_string() }),
            )
        }
    }
}

fn time_json(t: &EventTime) -> Value {
    match t.tz.parse::<chrono_tz::Tz>() {
        Ok(z) => json!({
            "dateTime": t.at.with_timezone(&z).to_rfc3339(),
            "timeZone": t.tz,
        }),
        Err(_) => json!({
            "dateTime": t.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "timeZone": "UTC",
        }),
    }
}

fn attendees_json(list: &[MessageFrom]) -> Value {
    Value::Array(
        list.iter()
            .map(|a| {
                let mut o = json!({ "email": a.address });
                if !a.name.is_empty() {
                    o["displayName"] = json!(a.name);
                }
                o
            })
            .collect(),
    )
}

/// A Meet link request; `request_id` must be unique per request.
fn meet_request(request_id: &str) -> Value {
    json!({
        "createRequest": {
            "requestId": request_id,
            "conferenceSolutionKey": { "type": "hangoutsMeet" }
        }
    })
}

pub(crate) fn create_body(spec: &EventSpec, request_id: &str) -> Value {
    let (start, end) = when_json(&spec.when);
    let mut b = json!({
        "summary": spec.title,
        "start": start,
        "end": end,
    });
    if let Some(l) = &spec.location {
        b["location"] = json!(l);
    }
    if let Some(body) = &spec.body {
        b["description"] = json!(body);
    }
    if !spec.attendees.is_empty() {
        b["attendees"] = attendees_json(&spec.attendees);
    }
    if spec.online_meeting {
        b["conferenceData"] = meet_request(request_id);
    }
    b
}

/// The PATCH body for `patch`. `current` attendees keep their response
/// status when they stay on the list. Turning the online meeting off
/// clears the conference.
pub(crate) fn patch_body(patch: &EventPatch, current: &[GPerson], request_id: &str) -> Value {
    let mut b = json!({});
    if let Some(t) = &patch.title {
        b["summary"] = json!(t);
    }
    if let Some(w) = &patch.when {
        let (start, end) = when_json(w);
        b["start"] = start;
        b["end"] = end;
    }
    if let Some(l) = &patch.location {
        b["location"] = json!(l);
    }
    if let Some(body) = &patch.body {
        b["description"] = json!(body);
    }
    if let Some(list) = &patch.attendees {
        b["attendees"] = Value::Array(
            list.iter()
                .map(|a| {
                    let mut o = json!({ "email": a.address });
                    if !a.name.is_empty() {
                        o["displayName"] = json!(a.name);
                    }
                    if let Some(c) = current
                        .iter()
                        .find(|c| c.email.eq_ignore_ascii_case(&a.address))
                    {
                        if let Some(r) = &c.response_status {
                            o["responseStatus"] = json!(r);
                        }
                        if c.optional {
                            o["optional"] = json!(true);
                        }
                    }
                    o
                })
                .collect(),
        );
    }
    match patch.online_meeting {
        Some(true) => b["conferenceData"] = meet_request(request_id),
        Some(false) => b["conferenceData"] = Value::Null,
        None => {}
    }
    b
}

/// Does a patch touch the conference (and so need `conferenceDataVersion=1`)?
pub(crate) fn touches_conference(patch: &EventPatch) -> bool {
    patch.online_meeting.is_some()
}

/// The attendee list with the user's own answer set, or `None` when the
/// user is not an attendee.
pub(crate) fn rsvp_attendees(
    current: &[GPerson],
    response: Rsvp,
    message: Option<&str>,
) -> Option<Value> {
    current.iter().find(|a| a.is_self)?;
    let status = match response {
        Rsvp::Accept => "accepted",
        Rsvp::Tentative => "tentative",
        Rsvp::Decline => "declined",
    };
    Some(Value::Array(
        current
            .iter()
            .map(|a| {
                let mut o = json!({ "email": a.email });
                if let Some(n) = &a.display_name {
                    o["displayName"] = json!(n);
                }
                if a.optional {
                    o["optional"] = json!(true);
                }
                if a.resource {
                    o["resource"] = json!(true);
                }
                if a.is_self {
                    o["responseStatus"] = json!(status);
                    if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
                        o["comment"] = json!(m);
                    }
                } else {
                    if let Some(r) = &a.response_status {
                        o["responseStatus"] = json!(r);
                    }
                    if let Some(c) = &a.comment {
                        o["comment"] = json!(c);
                    }
                }
                o
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct FreeBusyResponse {
    #[serde(default)]
    pub calendars: std::collections::HashMap<String, FreeBusyCalendar>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct FreeBusyCalendar {
    #[serde(default)]
    pub busy: Vec<FreeBusySpan>,
    #[serde(default)]
    pub errors: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FreeBusySpan {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrule_parses_weekly_with_days_and_until() {
        let p = parse_rrule(&[
            "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20261231T225959Z".into(),
        ])
        .unwrap();
        assert_eq!(p.freq, RecurrenceFreq::Weekly);
        assert_eq!(p.interval, 2);
        assert_eq!(p.by_weekday, vec![Weekday::Monday, Weekday::Wednesday]);
        assert_eq!(
            p.range,
            RecurrenceRange::EndDate(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap())
        );
        let c = parse_rrule(&[
            "EXDATE:x".into(),
            "RRULE:FREQ=MONTHLY;COUNT=3;BYDAY=-1FR".into(),
        ])
        .unwrap();
        assert_eq!(c.range, RecurrenceRange::Count(3));
        assert_eq!(c.by_weekday, vec![Weekday::Friday]);
        assert!(parse_rrule(&[]).is_none());
    }

    #[test]
    fn all_day_end_is_exclusive_on_the_wire() {
        let (s, e) = when_json(&EventWhen::AllDay {
            first: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            last: NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
        });
        assert_eq!(s, json!({"date": "2026-11-05"}));
        assert_eq!(e, json!({"date": "2026-11-07"}));
    }

    #[test]
    fn timed_events_are_written_in_their_zone() {
        let at = DateTime::parse_from_rfc3339("2026-11-05T09:00:00Z")
            .unwrap()
            .to_utc();
        let v = time_json(&EventTime {
            at,
            tz: "Europe/Stockholm".into(),
        });
        assert_eq!(v["dateTime"], "2026-11-05T10:00:00+01:00");
        assert_eq!(v["timeZone"], "Europe/Stockholm");
        let u = time_json(&EventTime {
            at,
            tz: "Nowhere/Special".into(),
        });
        assert_eq!(u["dateTime"], "2026-11-05T09:00:00Z");
        assert_eq!(u["timeZone"], "UTC");
    }

    #[test]
    fn rsvp_requires_being_an_attendee() {
        let others = vec![GPerson {
            email: "bob@example.com".into(),
            ..Default::default()
        }];
        assert!(rsvp_attendees(&others, Rsvp::Accept, None).is_none());
    }
}
