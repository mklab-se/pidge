//! Microsoft Graph calendar event endpoints.
//!
//! All requests send `Prefer: outlook.timezone="UTC"` so Graph normalizes
//! timestamps regardless of the mailbox's preferred TZ; pidge owns display
//! formatting via the IANA name carried alongside each `EventTime`.

use chrono::{DateTime, Datelike, Utc};
use pidge_core::{
    Attendee, AttendeeKind, BodyContentType, Event, EventTime, RecurrenceFreq, RecurrencePattern,
    RecurrenceRange, ResponseStatus, Weekday,
};
use serde::Deserialize;

use crate::error::ClientError;

const PREFER_UTC: &str = "outlook.timezone=\"UTC\"";

#[derive(Deserialize)]
struct GraphEventList {
    value: Vec<GraphEvent>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

#[derive(Deserialize)]
struct GraphEvent {
    id: String,
    subject: Option<String>,
    #[serde(rename = "bodyPreview", default)]
    body_preview: Option<String>,
    #[serde(default)]
    body: Option<GraphBody>,
    start: GraphDateTime,
    end: GraphDateTime,
    #[serde(rename = "isAllDay", default)]
    is_all_day: Option<bool>,
    #[serde(default)]
    location: Option<GraphLocation>,
    #[serde(default)]
    organizer: Option<GraphAttendee>,
    #[serde(default)]
    attendees: Vec<GraphAttendee>,
    #[serde(default)]
    recurrence: Option<GraphRecurrence>,
    #[serde(rename = "isOrganizer", default)]
    is_organizer: Option<bool>,
    #[serde(rename = "responseStatus", default)]
    response_status: Option<GraphResponseStatus>,
    #[serde(rename = "onlineMeeting", default)]
    online_meeting: Option<GraphOnlineMeeting>,
    #[serde(rename = "seriesMasterId", default)]
    series_master_id: Option<String>,
    #[serde(rename = "calendarId", default)]
    calendar_id: Option<String>,
}

#[derive(Deserialize)]
struct GraphBody {
    #[serde(rename = "contentType")]
    content_type: String,
    content: String,
}

#[derive(Deserialize)]
struct GraphDateTime {
    #[serde(rename = "dateTime")]
    date_time: String,
    #[serde(rename = "timeZone")]
    time_zone: String,
}

#[derive(Deserialize)]
struct GraphLocation {
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct GraphAttendee {
    #[serde(rename = "emailAddress")]
    email_address: GraphEmailAddress,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<GraphAttendeeStatus>,
}

#[derive(Deserialize)]
struct GraphEmailAddress {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    address: Option<String>,
}

#[derive(Deserialize)]
struct GraphAttendeeStatus {
    #[serde(default)]
    response: Option<String>,
}

#[derive(Deserialize)]
struct GraphResponseStatus {
    #[serde(default)]
    response: Option<String>,
}

#[derive(Deserialize)]
struct GraphOnlineMeeting {
    #[serde(rename = "joinUrl", default)]
    join_url: Option<String>,
}

#[derive(Deserialize)]
struct GraphRecurrence {
    pattern: GraphRecurrencePattern,
    range: GraphRecurrenceRange,
}

#[derive(Deserialize)]
struct GraphRecurrencePattern {
    #[serde(rename = "type")]
    kind: String,
    interval: u32,
    #[serde(rename = "daysOfWeek", default)]
    days_of_week: Vec<String>,
}

#[derive(Deserialize)]
struct GraphRecurrenceRange {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "endDate", default)]
    end_date: Option<String>,
    #[serde(rename = "numberOfOccurrences", default)]
    number_of_occurrences: Option<u32>,
}

pub struct EventsPage {
    pub events: Vec<Event>,
    pub has_more: bool,
    /// Graph continuation URL for the next page (`@odata.nextLink`).
    pub next_link: Option<String>,
}

/// What the caller is sending — pre-Graph-serialization shape for create/update.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub subject: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tz: String,
    pub all_day: bool,
    pub location: Option<String>,
    pub body_text: Option<String>,
    pub required_attendees: Vec<String>,
    pub optional_attendees: Vec<String>,
    pub recurrence: Option<RecurrencePattern>,
    pub online_meeting: bool,
}

impl NewEvent {
    pub(crate) fn to_graph_json(&self) -> serde_json::Value {
        // `format_graph_dt` emits the *UTC* clock for `start`/`end`, so the
        // accompanying `timeZone` MUST be "UTC". Labelling a UTC clock with a
        // local zone (e.g. self.tz = "Europe/Stockholm") makes Graph
        // re-interpret it as local time and shift it again — a double
        // conversion that stored events hours off. `self.tz` records the
        // zone the event was scheduled in but must not relabel the clock.
        let mut v = serde_json::json!({
            "subject": self.subject,
            "isAllDay": self.all_day,
            "start": {
                "dateTime": format_graph_dt(self.start),
                "timeZone": "UTC",
            },
            "end": {
                "dateTime": format_graph_dt(self.end),
                "timeZone": "UTC",
            },
        });
        if let Some(loc) = &self.location {
            v["location"] = serde_json::json!({ "displayName": loc });
        }
        if let Some(b) = &self.body_text {
            v["body"] = serde_json::json!({ "contentType": "text", "content": b });
        }
        let mut atts: Vec<serde_json::Value> = self
            .required_attendees
            .iter()
            .map(|a| attendee_json(a, "required"))
            .collect();
        atts.extend(
            self.optional_attendees
                .iter()
                .map(|a| attendee_json(a, "optional")),
        );
        if !atts.is_empty() {
            v["attendees"] = serde_json::Value::Array(atts);
        }
        if let Some(r) = &self.recurrence {
            v["recurrence"] = recurrence_to_json(r, self.start);
        }
        if self.online_meeting {
            v["isOnlineMeeting"] = serde_json::Value::Bool(true);
            v["onlineMeetingProvider"] = serde_json::Value::String("teamsForBusiness".into());
        }
        v
    }
}

fn format_graph_dt(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn attendee_json(addr: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "emailAddress": { "address": addr },
        "type": kind,
    })
}

fn recurrence_to_json(r: &RecurrencePattern, start: DateTime<Utc>) -> serde_json::Value {
    let pattern_type = match r.freq {
        RecurrenceFreq::Daily => "daily",
        RecurrenceFreq::Weekly => "weekly",
        RecurrenceFreq::Monthly => "absoluteMonthly",
        RecurrenceFreq::Yearly => "absoluteYearly",
    };
    let mut pattern = serde_json::json!({
        "type": pattern_type,
        "interval": r.interval,
    });
    if r.freq == RecurrenceFreq::Weekly && !r.by_weekday.is_empty() {
        pattern["daysOfWeek"] = serde_json::Value::Array(
            r.by_weekday
                .iter()
                .map(|w| serde_json::Value::String(weekday_name(*w).into()))
                .collect(),
        );
    }
    if matches!(r.freq, RecurrenceFreq::Monthly | RecurrenceFreq::Yearly) {
        pattern["dayOfMonth"] = serde_json::Value::Number(start.day().into());
    }
    if r.freq == RecurrenceFreq::Yearly {
        pattern["month"] = serde_json::Value::Number(start.month().into());
    }

    let start_date = start.format("%Y-%m-%d").to_string();
    let range = match &r.range {
        RecurrenceRange::NoEnd => serde_json::json!({ "type": "noEnd", "startDate": start_date }),
        RecurrenceRange::Count(n) => serde_json::json!({
            "type": "numbered", "startDate": start_date, "numberOfOccurrences": n
        }),
        RecurrenceRange::EndDate(d) => serde_json::json!({
            "type": "endDate", "startDate": start_date, "endDate": d.format("%Y-%m-%d").to_string()
        }),
    };

    serde_json::json!({ "pattern": pattern, "range": range })
}

fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "monday",
        Weekday::Tuesday => "tuesday",
        Weekday::Wednesday => "wednesday",
        Weekday::Thursday => "thursday",
        Weekday::Friday => "friday",
        Weekday::Saturday => "saturday",
        Weekday::Sunday => "sunday",
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RsvpKind {
    Accept,
    Tentative,
    Decline,
}

/// GET /me/calendarView?startDateTime=..&endDateTime=..
///
/// Expands recurrence instances within the window. Pass `calendar_id` to
/// scope to a non-default calendar.
#[allow(clippy::too_many_arguments)]
pub async fn list_calendar_view(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    calendar_id: Option<&str>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    limit: usize,
) -> Result<EventsPage, ClientError> {
    let url = match calendar_id {
        Some(id) => format!("{base_url}/me/calendars/{id}/calendarView"),
        None => format!("{base_url}/me/calendarView"),
    };
    let resp = super::send_with_retry(
        http.get(&url)
            .bearer_auth(access_token)
            .header("Prefer", PREFER_UTC)
            .query(&[
                ("startDateTime", start.to_rfc3339()),
                ("endDateTime", end.to_rfc3339()),
                ("$orderby", "start/dateTime".into()),
                ("$top", limit.to_string()),
            ]),
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let list: GraphEventList = resp.json().await?;
    Ok(EventsPage {
        has_more: list.next_link.is_some(),
        next_link: list.next_link,
        events: list
            .value
            .into_iter()
            .map(|g| to_event(g, account, calendar_id))
            .collect(),
    })
}

/// Fetch a page of calendar-view events at an absolute Graph URL (an
/// `@odata.nextLink` carried in a pidge cursor).
pub async fn list_events_at(
    http: &reqwest::Client,
    access_token: &str,
    account: &str,
    url: &str,
) -> Result<EventsPage, ClientError> {
    let resp = super::send_with_retry(
        http.get(url)
            .bearer_auth(access_token)
            .header("Prefer", PREFER_UTC),
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let list: GraphEventList = resp.json().await?;
    Ok(EventsPage {
        has_more: list.next_link.is_some(),
        next_link: list.next_link,
        events: list
            .value
            .into_iter()
            .map(|g| to_event(g, account, None))
            .collect(),
    })
}

/// GET /me/events/{id}
pub async fn get_event(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
    event_id: &str,
) -> Result<Event, ClientError> {
    let url = format!("{base_url}/me/events/{event_id}");
    let resp = super::send_with_retry(
        http.get(&url)
            .bearer_auth(access_token)
            .header("Prefer", PREFER_UTC),
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let g: GraphEvent = resp.json().await?;
    Ok(to_event(g, account, None))
}

/// POST /me/calendar/events (or /me/calendars/{id}/events). Returns the
/// new event ID. Attendees in the payload trigger Graph to send invitations.
pub async fn create_event(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    calendar_id: Option<&str>,
    event: &NewEvent,
) -> Result<String, ClientError> {
    let url = match calendar_id {
        Some(id) => format!("{base_url}/me/calendars/{id}/events"),
        None => format!("{base_url}/me/calendar/events"),
    };
    let resp = super::send_with_retry(
        http.post(&url)
            .bearer_auth(access_token)
            .json(&event.to_graph_json()),
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let v: serde_json::Value = resp.json().await?;
    v["id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| ClientError::Graph {
            status: 200,
            message: "create event response missing 'id'".to_string(),
        })
}

/// PATCH /me/events/{id} — overwrite editable fields.
pub async fn update_event(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    event_id: &str,
    event: &NewEvent,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/events/{event_id}");
    let resp = super::send_with_retry(
        http.patch(&url)
            .bearer_auth(access_token)
            .json(&event.to_graph_json()),
    )
    .await?;
    bubble_no_body(resp).await
}

/// PATCH /me/events/{id} — change only start + end.
pub async fn move_time(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    event_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    tz: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/events/{event_id}");
    let body = serde_json::json!({
        "start": { "dateTime": format_graph_dt(start), "timeZone": tz },
        "end":   { "dateTime": format_graph_dt(end),   "timeZone": tz },
    });
    let resp =
        super::send_with_retry(http.patch(&url).bearer_auth(access_token).json(&body)).await?;
    bubble_no_body(resp).await
}

/// DELETE /me/events/{id} — silent removal (no attendee notification).
pub async fn delete_event(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    event_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/events/{event_id}");
    let resp = super::send_with_retry(http.delete(&url).bearer_auth(access_token)).await?;
    bubble_no_body(resp).await
}

/// POST /me/events/{id}/cancel — organizer-only; sends cancellation notices.
pub async fn cancel_event(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    event_id: &str,
    comment: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/events/{event_id}/cancel");
    let body = serde_json::json!({ "Comment": comment });
    let resp =
        super::send_with_retry(http.post(&url).bearer_auth(access_token).json(&body)).await?;
    bubble_no_body(resp).await
}

/// POST /me/events/{id}/accept | /tentativelyAccept | /decline
pub async fn rsvp_event(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    event_id: &str,
    kind: RsvpKind,
    comment: &str,
    send_response: bool,
) -> Result<(), ClientError> {
    let verb = match kind {
        RsvpKind::Accept => "accept",
        RsvpKind::Tentative => "tentativelyAccept",
        RsvpKind::Decline => "decline",
    };
    let url = format!("{base_url}/me/events/{event_id}/{verb}");
    let body = serde_json::json!({ "Comment": comment, "SendResponse": send_response });
    let resp =
        super::send_with_retry(http.post(&url).bearer_auth(access_token).json(&body)).await?;
    bubble_no_body(resp).await
}

/// PATCH /me/events/{id} — move event to a different calendar by binding
/// the navigation property to the destination calendar's URL.
pub async fn move_event_to_calendar(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    event_id: &str,
    destination_calendar_id: &str,
) -> Result<(), ClientError> {
    let url = format!("{base_url}/me/events/{event_id}");
    let bind = format!("{base_url}/me/calendars/{destination_calendar_id}");
    let body = serde_json::json!({ "calendar@odata.bind": bind });
    let resp =
        super::send_with_retry(http.patch(&url).bearer_auth(access_token).json(&body)).await?;
    bubble_no_body(resp).await
}

async fn bubble_no_body(resp: reqwest::Response) -> Result<(), ClientError> {
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    Ok(())
}

fn to_event(g: GraphEvent, account: &str, calendar_hint: Option<&str>) -> Event {
    let (body_content, body_content_type) = match g.body {
        Some(b) => {
            let kind = if b.content_type.eq_ignore_ascii_case("html") {
                BodyContentType::Html
            } else {
                BodyContentType::Text
            };
            (b.content, kind)
        }
        None => (String::new(), BodyContentType::Text),
    };
    let organizer = g
        .organizer
        .map(graph_attendee_to_attendee)
        .unwrap_or_else(|| Attendee {
            name: String::new(),
            address: String::new(),
            kind: AttendeeKind::Required,
            response: ResponseStatus::Organizer,
        });

    Event {
        account: account.to_string(),
        calendar_id: g
            .calendar_id
            .unwrap_or_else(|| calendar_hint.unwrap_or("").to_string()),
        id: g.id,
        subject: g.subject.unwrap_or_default(),
        start: parse_event_time(g.start),
        end: parse_event_time(g.end),
        all_day: g.is_all_day.unwrap_or(false),
        location: g
            .location
            .and_then(|l| l.display_name)
            .filter(|s| !s.is_empty()),
        organizer,
        attendees: g
            .attendees
            .into_iter()
            .map(graph_attendee_to_attendee)
            .collect(),
        body_preview: g.body_preview.unwrap_or_default(),
        body_content,
        body_content_type,
        recurrence: g.recurrence.map(parse_recurrence),
        is_organizer: g.is_organizer.unwrap_or(false),
        response_status: parse_response(g.response_status.and_then(|r| r.response).as_deref()),
        online_meeting_url: g.online_meeting.and_then(|m| m.join_url),
        series_master_id: g.series_master_id,
    }
}

fn parse_event_time(g: GraphDateTime) -> EventTime {
    // With Prefer: outlook.timezone="UTC", Graph returns the dateTime
    // already in UTC; timeZone is the IANA zone the event was scheduled
    // in (e.g. "Europe/Stockholm").
    let at = chrono::NaiveDateTime::parse_from_str(&g.date_time, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(&g.date_time, "%Y-%m-%dT%H:%M:%S"))
        .map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
        .unwrap_or_else(|_| Utc::now());
    EventTime {
        at,
        tz: g.time_zone,
    }
}

fn graph_attendee_to_attendee(g: GraphAttendee) -> Attendee {
    Attendee {
        name: g.email_address.name.unwrap_or_default(),
        address: g.email_address.address.unwrap_or_default(),
        kind: match g.kind.as_deref() {
            Some("optional") => AttendeeKind::Optional,
            Some("resource") => AttendeeKind::Resource,
            _ => AttendeeKind::Required,
        },
        response: parse_response(g.status.and_then(|s| s.response).as_deref()),
    }
}

fn parse_response(s: Option<&str>) -> ResponseStatus {
    match s {
        Some("organizer") => ResponseStatus::Organizer,
        Some("accepted") => ResponseStatus::Accepted,
        Some("tentativelyAccepted") => ResponseStatus::Tentative,
        Some("declined") => ResponseStatus::Declined,
        Some("notResponded") => ResponseStatus::NotResponded,
        _ => ResponseStatus::None,
    }
}

fn parse_recurrence(g: GraphRecurrence) -> RecurrencePattern {
    let freq = match g.pattern.kind.as_str() {
        "daily" => RecurrenceFreq::Daily,
        "weekly" => RecurrenceFreq::Weekly,
        "absoluteMonthly" | "relativeMonthly" => RecurrenceFreq::Monthly,
        "absoluteYearly" | "relativeYearly" => RecurrenceFreq::Yearly,
        _ => RecurrenceFreq::Daily,
    };
    let by_weekday = g
        .pattern
        .days_of_week
        .into_iter()
        .filter_map(|d| match d.as_str() {
            "monday" => Some(Weekday::Monday),
            "tuesday" => Some(Weekday::Tuesday),
            "wednesday" => Some(Weekday::Wednesday),
            "thursday" => Some(Weekday::Thursday),
            "friday" => Some(Weekday::Friday),
            "saturday" => Some(Weekday::Saturday),
            "sunday" => Some(Weekday::Sunday),
            _ => None,
        })
        .collect();
    let range = match g.range.kind.as_str() {
        "noEnd" => RecurrenceRange::NoEnd,
        "numbered" => RecurrenceRange::Count(g.range.number_of_occurrences.unwrap_or(1)),
        _ => g
            .range
            .end_date
            .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .map(RecurrenceRange::EndDate)
            .unwrap_or(RecurrenceRange::NoEnd),
    };
    RecurrencePattern {
        freq,
        interval: g.pattern.interval,
        by_weekday,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_event(tz: &str) -> NewEvent {
        NewEvent {
            subject: "Course".into(),
            start: DateTime::parse_from_rfc3339("2026-07-01T08:00:00Z")
                .unwrap()
                .to_utc(),
            end: DateTime::parse_from_rfc3339("2026-07-01T09:15:00Z")
                .unwrap()
                .to_utc(),
            tz: tz.into(),
            all_day: false,
            location: None,
            body_text: None,
            required_attendees: vec![],
            optional_attendees: vec![],
            recurrence: None,
            online_meeting: false,
        }
    }

    #[test]
    fn to_graph_json_labels_utc_clock_as_utc_not_local_zone() {
        // Regression: a 10:00 Europe/Stockholm booking parses to 08:00Z.
        // `format_graph_dt` emits the UTC clock ("08:00:00"), so the payload
        // MUST label it "UTC". Labelling it "Europe/Stockholm" makes Graph
        // re-interpret 08:00 as local time = 06:00Z — a double conversion
        // that stored the Fotografiska course two hours early.
        let v = sample_event("Europe/Stockholm").to_graph_json();
        assert_eq!(v["start"]["dateTime"], "2026-07-01T08:00:00");
        assert_eq!(v["start"]["timeZone"], "UTC");
        assert_eq!(v["end"]["dateTime"], "2026-07-01T09:15:00");
        assert_eq!(v["end"]["timeZone"], "UTC");
    }

    #[tokio::test]
    async fn list_calendar_view_passes_window_and_orderby() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/calendarView"))
            .and(header("authorization", "Bearer AT"))
            .and(header("prefer", "outlook.timezone=\"UTC\""))
            .and(query_param("$orderby", "start/dateTime"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "evt-1",
                        "subject": "Sync",
                        "bodyPreview": "weekly",
                        "start": { "dateTime": "2026-05-22T13:00:00.000", "timeZone": "UTC" },
                        "end":   { "dateTime": "2026-05-22T14:00:00.000", "timeZone": "UTC" },
                        "isAllDay": false,
                        "organizer": { "emailAddress": { "name": "K", "address": "k@x.com" } },
                        "attendees": [
                            {
                                "emailAddress": { "name": "A", "address": "a@x.com" },
                                "type": "required",
                                "status": { "response": "accepted" }
                            }
                        ],
                        "isOrganizer": true,
                        "responseStatus": { "response": "organizer" }
                    }
                ]
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let start = DateTime::parse_from_rfc3339("2026-05-20T00:00:00Z")
            .unwrap()
            .to_utc();
        let end = DateTime::parse_from_rfc3339("2026-05-27T00:00:00Z")
            .unwrap()
            .to_utc();
        let page = list_calendar_view(&http, &server.uri(), "AT", "u@e.com", None, start, end, 50)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].subject, "Sync");
        assert!(page.events[0].is_organizer);
        assert_eq!(page.events[0].attendees.len(), 1);
        assert_eq!(page.events[0].attendees[0].address, "a@x.com");
    }

    #[tokio::test]
    async fn list_calendar_view_supports_calendar_scope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/calendars/CAL-2/calendarView"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": []})),
            )
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let s = DateTime::parse_from_rfc3339("2026-05-20T00:00:00Z")
            .unwrap()
            .to_utc();
        let e = DateTime::parse_from_rfc3339("2026-05-21T00:00:00Z")
            .unwrap()
            .to_utc();
        let p = list_calendar_view(
            &http,
            &server.uri(),
            "AT",
            "u@e.com",
            Some("CAL-2"),
            s,
            e,
            50,
        )
        .await
        .unwrap();
        assert!(p.events.is_empty());
    }

    #[tokio::test]
    async fn get_event_parses_recurrence() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/events/EVT-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "EVT-1",
                "subject": "Daily standup",
                "start": { "dateTime": "2026-05-22T07:00:00.000", "timeZone": "UTC" },
                "end":   { "dateTime": "2026-05-22T07:15:00.000", "timeZone": "UTC" },
                "isAllDay": false,
                "organizer": { "emailAddress": { "name": "K", "address": "k@x.com" } },
                "recurrence": {
                    "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["monday","wednesday","friday"] },
                    "range": { "type": "noEnd", "startDate": "2026-05-22" }
                }
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let e = get_event(&http, &server.uri(), "AT", "u@e.com", "EVT-1")
            .await
            .unwrap();
        let r = e.recurrence.unwrap();
        assert_eq!(r.freq, RecurrenceFreq::Weekly);
        assert_eq!(r.by_weekday.len(), 3);
        assert!(matches!(r.range, RecurrenceRange::NoEnd));
    }

    #[tokio::test]
    async fn create_event_serializes_attendees_recurrence_online_meeting() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/me/calendar/events"))
            .and(body_partial_json(serde_json::json!({
                "subject": "Weekly sync",
                "isAllDay": false,
                "isOnlineMeeting": true,
                "onlineMeetingProvider": "teamsForBusiness",
                "attendees": [
                    { "emailAddress": { "address": "a@x.com" }, "type": "required" },
                    { "emailAddress": { "address": "b@x.com" }, "type": "optional" }
                ],
                "recurrence": {
                    "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["monday"] }
                }
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": "NEW"})),
            )
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let new = NewEvent {
            subject: "Weekly sync".into(),
            start: DateTime::parse_from_rfc3339("2026-05-25T13:00:00Z")
                .unwrap()
                .to_utc(),
            end: DateTime::parse_from_rfc3339("2026-05-25T14:00:00Z")
                .unwrap()
                .to_utc(),
            tz: "Europe/Stockholm".into(),
            all_day: false,
            location: None,
            body_text: None,
            required_attendees: vec!["a@x.com".into()],
            optional_attendees: vec!["b@x.com".into()],
            recurrence: Some(RecurrencePattern {
                freq: RecurrenceFreq::Weekly,
                interval: 1,
                by_weekday: vec![Weekday::Monday],
                range: RecurrenceRange::NoEnd,
            }),
            online_meeting: true,
        };
        let id = create_event(&http, &server.uri(), "AT", None, &new)
            .await
            .unwrap();
        assert_eq!(id, "NEW");
    }

    #[tokio::test]
    async fn move_time_patches_start_and_end() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/me/events/[A-Za-z0-9]+"))
            .and(body_partial_json(serde_json::json!({
                "start": { "dateTime": "2026-05-22T15:00:00", "timeZone": "Europe/Stockholm" },
                "end":   { "dateTime": "2026-05-22T16:00:00", "timeZone": "Europe/Stockholm" }
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let s = DateTime::parse_from_rfc3339("2026-05-22T15:00:00Z")
            .unwrap()
            .to_utc();
        let e = DateTime::parse_from_rfc3339("2026-05-22T16:00:00Z")
            .unwrap()
            .to_utc();
        move_time(&http, &server.uri(), "AT", "EVT", s, e, "Europe/Stockholm")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_event_calls_delete() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path_regex("/me/events/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        delete_event(&http, &server.uri(), "AT", "EVT")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancel_event_posts_comment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/me/events/[A-Za-z0-9]+/cancel"))
            .and(body_partial_json(
                serde_json::json!({ "Comment": "Sorry, can't make it" }),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        cancel_event(&http, &server.uri(), "AT", "EVT", "Sorry, can't make it")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rsvp_event_posts_to_correct_verb() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/me/events/[A-Za-z0-9]+/tentativelyAccept"))
            .and(body_partial_json(
                serde_json::json!({ "SendResponse": false }),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        rsvp_event(
            &http,
            &server.uri(),
            "AT",
            "EVT",
            RsvpKind::Tentative,
            "",
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn move_event_patches_calendar_bind() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/me/events/[A-Za-z0-9]+"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        move_event_to_calendar(&http, &server.uri(), "AT", "EVT-1", "CAL-2")
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        let expected = format!("{}/me/calendars/CAL-2", server.uri());
        assert_eq!(body["calendar@odata.bind"], expected);
    }
}
