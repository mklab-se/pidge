//! Normalized calendar event representation, provider-agnostic.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::message::BodyContentType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub account: String,
    pub calendar_id: String,
    pub id: String,
    pub subject: String,
    pub start: EventTime,
    pub end: EventTime,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub location: Option<String>,
    pub organizer: Attendee,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    #[serde(default)]
    pub body_preview: String,
    #[serde(default)]
    pub body_content: String,
    #[serde(default)]
    pub body_content_type: BodyContentType,
    #[serde(default)]
    pub recurrence: Option<RecurrencePattern>,
    #[serde(default)]
    pub is_organizer: bool,
    #[serde(default)]
    pub response_status: ResponseStatus,
    #[serde(default)]
    pub online_meeting_url: Option<String>,
    #[serde(default)]
    pub series_master_id: Option<String>,
}

/// A calendar instant. `at` is the canonical UTC time; `tz` is the IANA zone
/// the event was scheduled in (e.g. `"Europe/Stockholm"`). Display layers
/// honour `tz` so a Stockholm-scheduled meeting still says "10:00 Stockholm"
/// when read from a laptop in Sydney.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTime {
    pub at: DateTime<Utc>,
    pub tz: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub kind: AttendeeKind,
    #[serde(default)]
    pub response: ResponseStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttendeeKind {
    #[default]
    Required,
    Optional,
    Resource,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseStatus {
    #[default]
    None,
    Organizer,
    Accepted,
    Tentative,
    Declined,
    NotResponded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecurrencePattern {
    pub freq: RecurrenceFreq,
    /// 1 = every freq, 2 = every other, etc.
    pub interval: u32,
    /// Only meaningful when `freq == Weekly`.
    #[serde(default)]
    pub by_weekday: Vec<Weekday>,
    pub range: RecurrenceRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecurrenceFreq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum RecurrenceRange {
    EndDate(NaiveDate),
    Count(u32),
    NoEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    pub account: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub can_edit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_roundtrips_through_json() {
        let e = Event {
            account: "u@e.com".into(),
            calendar_id: "cal-1".into(),
            id: "evt-1".into(),
            subject: "Team sync".into(),
            start: EventTime {
                at: chrono::DateTime::parse_from_rfc3339("2026-05-22T13:00:00Z")
                    .unwrap()
                    .to_utc(),
                tz: "Europe/Stockholm".into(),
            },
            end: EventTime {
                at: chrono::DateTime::parse_from_rfc3339("2026-05-22T14:00:00Z")
                    .unwrap()
                    .to_utc(),
                tz: "Europe/Stockholm".into(),
            },
            all_day: false,
            location: Some("Office".into()),
            organizer: Attendee {
                name: "Kristofer".into(),
                address: "k@mklab.se".into(),
                kind: AttendeeKind::Required,
                response: ResponseStatus::Organizer,
            },
            attendees: vec![],
            body_preview: "Weekly".into(),
            body_content: "Weekly sync".into(),
            body_content_type: BodyContentType::Text,
            recurrence: Some(RecurrencePattern {
                freq: RecurrenceFreq::Weekly,
                interval: 1,
                by_weekday: vec![Weekday::Monday],
                range: RecurrenceRange::NoEnd,
            }),
            is_organizer: true,
            response_status: ResponseStatus::Organizer,
            online_meeting_url: None,
            series_master_id: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        let e2: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(e, e2);
    }

    #[test]
    fn recurrence_range_roundtrips_tagged() {
        for r in [
            RecurrenceRange::NoEnd,
            RecurrenceRange::Count(10),
            RecurrenceRange::EndDate(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
        ] {
            let j = serde_json::to_string(&r).unwrap();
            let r2: RecurrenceRange = serde_json::from_str(&j).unwrap();
            assert_eq!(r, r2);
        }
    }

    #[test]
    fn response_status_serializes_camel_case() {
        assert_eq!(
            serde_json::to_string(&ResponseStatus::NotResponded).unwrap(),
            "\"notResponded\""
        );
    }
}
