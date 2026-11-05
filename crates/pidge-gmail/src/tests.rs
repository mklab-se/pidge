//! Wiremock tests of [`crate::GmailProvider`] against Gmail's and Calendar's
//! wire shapes.

use pidge_core::provider::{
    BulkHint, DraftKind, DraftSpec, Folder, MailChange, Provider, ProviderError, Rsvp,
};
use pidge_core::{MessageFrom, ResponseStatus};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::mime::{decode_b64url, encode_b64url};
use crate::test_support::{ME, provider};

fn ok(v: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(v)
}

async fn mount_labels(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/gmail/labels"))
        .respond_with(ok(json!({"labels": [
            {"id": "INBOX", "name": "INBOX", "type": "system"},
            {"id": "SENT", "name": "SENT", "type": "system"},
            {"id": "TRASH", "name": "TRASH", "type": "system"},
            {"id": "CATEGORY_PROMOTIONS", "name": "CATEGORY_PROMOTIONS", "type": "system"},
            {"id": "Label_1", "name": "Clients/Acme", "type": "user"},
            {"id": "Label_2", "name": "Receipts", "type": "user"}
        ]})))
        .mount(server)
        .await;
}

fn metadata(id: &str, labels: &[&str], ms: i64, extra: &[(&str, &str)]) -> Value {
    let mut headers = vec![
        json!({"name": "From", "value": "\"Bob Smith\" <bob@example.com>"}),
        json!({"name": "To", "value": "Jane <jane@example.com>"}),
        json!({"name": "Subject", "value": format!("Subject {id}")}),
    ];
    headers.extend(extra.iter().map(|(n, v)| json!({"name": n, "value": v})));
    json!({
        "id": id,
        "threadId": format!("t-{id}"),
        "labelIds": labels,
        "snippet": "Fish &amp; chips",
        "internalDate": ms.to_string(),
        "payload": {"mimeType": "multipart/alternative", "headers": headers}
    })
}

async fn mount_metadata(server: &MockServer, id: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/gmail/messages/{id}")))
        .and(query_param("format", "metadata"))
        .respond_with(ok(body))
        .mount(server)
        .await;
}

fn since() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
        .unwrap()
        .to_utc()
}

/// 2026-11-03T12:00:00Z in milliseconds.
const NOV3: i64 = 1_793_707_200_000;

#[tokio::test]
async fn bootstrap_pages_through_the_window_and_maps_labels() {
    let server = MockServer::start().await;
    mount_labels(&server).await;
    Mock::given(method("GET"))
        .and(path("/gmail/profile"))
        .and(header("authorization", "Bearer AT"))
        .respond_with(ok(json!({"emailAddress": ME, "historyId": "500"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages"))
        .and(query_param(
            "q",
            "after:1793491200 -in:spam -in:trash -in:drafts",
        ))
        .and(query_param("pageToken", "P2"))
        .respond_with(ok(json!({"messages": [{"id": "m3", "threadId": "t"}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages"))
        .and(query_param(
            "q",
            "after:1793491200 -in:spam -in:trash -in:drafts",
        ))
        .respond_with(ok(json!({
            "messages": [{"id": "m1", "threadId": "t"}, {"id": "m2", "threadId": "t"}],
            "nextPageToken": "P2"
        })))
        .mount(&server)
        .await;
    let mut m1 = metadata(
        "m1",
        &[
            "INBOX",
            "UNREAD",
            "STARRED",
            "CATEGORY_PROMOTIONS",
            "Label_1",
        ],
        NOV3,
        &[("List-Unsubscribe", "<mailto:u@example.com>")],
    );
    m1["payload"]["mimeType"] = json!("multipart/mixed");
    mount_metadata(&server, "m1", m1).await;
    mount_metadata(&server, "m2", metadata("m2", &["SENT"], NOV3 + 1000, &[])).await;
    mount_metadata(
        &server,
        "m3",
        metadata("m3", &["Label_2"], NOV3 + 2000, &[]),
    )
    .await;

    let p = provider(&server);
    let first = p.sync_mail(since(), None).await.unwrap();
    assert!(first.more);
    assert_eq!(first.upserts.len(), 2);
    let a = &first.upserts[0];
    assert_eq!(a.account, ME);
    assert_eq!(a.id, "m1");
    assert_eq!(a.thread_id, "t-m1");
    assert_eq!(a.folder, Folder::Inbox);
    assert!(a.unread && a.flagged && a.has_attachments && a.list_unsubscribe);
    assert_eq!(a.bulk_hint, Some(BulkHint::Promotions));
    assert_eq!(a.categories, vec!["Clients/Acme".to_string()]);
    assert_eq!(a.from.name, "Bob Smith");
    assert_eq!(a.from.address, "bob@example.com");
    assert_eq!(a.to[0].address, "jane@example.com");
    assert_eq!(a.preview, "Fish & chips");
    assert_eq!(a.subject, "Subject m1");
    assert_eq!(a.received_at.timestamp_millis(), NOV3);
    let b = &first.upserts[1];
    assert_eq!(b.folder, Folder::Sent);
    assert!(!b.unread && !b.flagged && !b.has_attachments && !b.list_unsubscribe);
    assert_eq!(b.bulk_hint, None);

    let second = p.sync_mail(since(), Some(&first.cursor)).await.unwrap();
    assert!(!second.more);
    assert_eq!(second.upserts.len(), 1);
    assert_eq!(second.upserts[0].folder, Folder::Archive);
    assert_eq!(second.upserts[0].categories, vec!["Receipts".to_string()]);
    let c: Value = serde_json::from_str(&second.cursor).unwrap();
    assert_eq!(c, json!({"v": 1, "mode": "history", "history_id": "500"}));
}

#[tokio::test]
async fn history_round_upserts_changes_and_removes_what_left() {
    let server = MockServer::start().await;
    mount_labels(&server).await;
    Mock::given(method("GET"))
        .and(path("/gmail/history"))
        .and(query_param("startHistoryId", "500"))
        .respond_with(ok(json!({
            "history": [
                {"id": "501", "messagesAdded": [{"message": {"id": "m3", "threadId": "t"}}]},
                {"id": "502", "labelsRemoved": [
                    {"message": {"id": "m1", "threadId": "t"}, "labelIds": ["INBOX"]}
                ]},
                {"id": "503", "messagesDeleted": [{"message": {"id": "m2", "threadId": "t"}}]},
                {"id": "504", "labelsAdded": [
                    {"message": {"id": "m4", "threadId": "t"}, "labelIds": ["TRASH"]},
                    {"message": {"id": "m2", "threadId": "t"}, "labelIds": ["STARRED"]}
                ]},
                {"id": "505", "messagesAdded": [{"message": {"id": "old", "threadId": "t"}}]},
                {"id": "506", "messagesAdded": [{"message": {"id": "gone", "threadId": "t"}}]}
            ],
            "historyId": "510"
        })))
        .mount(&server)
        .await;
    mount_metadata(&server, "m3", metadata("m3", &["INBOX"], NOV3, &[])).await;
    mount_metadata(&server, "m1", metadata("m1", &["Label_1"], NOV3, &[])).await;
    mount_metadata(&server, "m4", metadata("m4", &["TRASH"], NOV3, &[])).await;
    mount_metadata(
        &server,
        "old",
        metadata("old", &["INBOX"], NOV3 - 30 * 86_400_000, &[]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let p = provider(&server);
    let cursor = r#"{"v":1,"mode":"history","history_id":"500"}"#;
    let r = p.sync_mail(since(), Some(cursor)).await.unwrap();
    assert!(!r.more);
    let ups: Vec<(&str, &Folder)> = r
        .upserts
        .iter()
        .map(|i| (i.id.as_str(), &i.folder))
        .collect();
    assert_eq!(ups, vec![("m3", &Folder::Inbox), ("m1", &Folder::Archive)]);
    assert_eq!(r.removed, vec!["m2", "m4", "old", "gone"]);
    let c: Value = serde_json::from_str(&r.cursor).unwrap();
    assert_eq!(c["history_id"], "510");
}

#[tokio::test]
async fn expired_history_and_foreign_cursors_ask_for_a_bootstrap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/history"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": 404, "message": "Requested entity was not found."}
        })))
        .mount(&server)
        .await;
    let p = provider(&server);
    let cursor = r#"{"v":1,"mode":"history","history_id":"1"}"#;
    assert_eq!(
        p.sync_mail(since(), Some(cursor)).await.unwrap_err(),
        ProviderError::CursorExpired
    );
    for bad in [
        "",
        "garbage",
        r#"{"v":99,"mode":"history","history_id":"1"}"#,
    ] {
        assert_eq!(
            p.sync_mail(since(), Some(bad)).await.unwrap_err(),
            ProviderError::CursorExpired
        );
    }
}

#[tokio::test]
async fn archive_is_one_batch_modify_and_bad_ids_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/gmail/messages/batchModify"))
        .and(body_json(json!({
            "ids": ["a1", "a2"],
            "addLabelIds": [],
            "removeLabelIds": ["INBOX", "SPAM", "TRASH"]
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let p = provider(&server);
    let ids = vec!["a1".to_string(), "../x".to_string(), "a2".to_string()];
    let r = p
        .apply(
            &ids,
            &MailChange::Move {
                to: Folder::Archive,
            },
        )
        .await;
    assert_eq!(r.len(), 3);
    assert!(r[0].is_ok() && r[2].is_ok());
    assert!(matches!(r[1], Err(ProviderError::Invalid(_))));
}

#[tokio::test]
async fn delete_trashes_each_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/gmail/messages/d1/trash"))
        .respond_with(ok(json!({"id": "d1", "labelIds": ["TRASH"]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/gmail/messages/d2/trash"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let p = provider(&server);
    let r = p
        .apply(&["d1".into(), "d2".into()], &MailChange::Delete)
        .await;
    assert!(r[0].is_ok());
    assert!(matches!(r[1], Err(ProviderError::NotFound(_))));
}

#[tokio::test]
async fn set_categories_adds_and_removes_user_labels_creating_missing_ones() {
    let server = MockServer::start().await;
    mount_labels(&server).await;
    Mock::given(method("POST"))
        .and(path("/gmail/labels"))
        .and(body_partial_json(json!({"name": "Travel"})))
        .respond_with(ok(
            json!({"id": "Label_9", "name": "Travel", "type": "user"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/c1"))
        .and(query_param("format", "minimal"))
        .respond_with(ok(
            json!({"id": "c1", "labelIds": ["INBOX", "Label_1", "Label_2"]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/gmail/messages/c1/modify"))
        .and(body_json(
            json!({"addLabelIds": ["Label_9"], "removeLabelIds": ["Label_1"]}),
        ))
        .respond_with(ok(json!({"id": "c1"})))
        .expect(1)
        .mount(&server)
        .await;
    let p = provider(&server);
    let r = p
        .apply(
            &["c1".into()],
            &MailChange::SetCategories {
                categories: vec!["receipts".into(), "Travel".into()],
            },
        )
        .await;
    assert_eq!(r, vec![Ok(())]);
}

fn original_raw() -> String {
    let raw = "From: Bob Smith <bob@example.com>\r\n\
               To: Jane <jane@example.com>, carl@example.com\r\n\
               Cc: dana@example.com\r\n\
               Subject: Quarterly plan\r\n\
               Date: Tue, 03 Nov 2026 12:00:00 +0000\r\n\
               Message-ID: <orig-1@mail.example.com>\r\n\
               References: <root@mail.example.com>\r\n\
               Content-Type: text/plain; charset=utf-8\r\n\
               \r\n\
               Can you review?\r\n";
    encode_b64url(raw.as_bytes())
}

#[tokio::test]
async fn reply_all_draft_is_a_threaded_rfc5322_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/orig1"))
        .and(query_param("format", "raw"))
        .respond_with(ok(json!({
            "id": "orig1", "threadId": "thr1", "labelIds": ["INBOX"],
            "internalDate": NOV3.to_string(), "raw": original_raw()
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/gmail/drafts"))
        .and(body_partial_json(json!({"message": {"threadId": "thr1"}})))
        .respond_with(ok(json!({
            "id": "r-42",
            "message": {"id": "dm1", "threadId": "thr1", "labelIds": ["DRAFT"]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let p = provider(&server);
    let d = p
        .create_draft(&DraftSpec {
            kind: DraftKind::ReplyAll {
                to_id: "orig1".into(),
            },
            to: vec![],
            cc: vec![MessageFrom {
                name: "Eve".into(),
                address: "eve@example.com".into(),
            }],
            bcc: vec![],
            subject: None,
            body: "Looks good.".into(),
        })
        .await
        .unwrap();
    assert_eq!(d.id, "r-42");
    assert_eq!(d.account, ME);
    assert_eq!(d.thread_id.as_deref(), Some("thr1"));
    assert_eq!(d.subject, "Re: Quarterly plan");
    assert!(d.body_text.starts_with("Looks good.\n\nOn "));
    assert!(d.body_text.contains("> Can you review?"));

    let reqs = server.received_requests().await.unwrap();
    let post = reqs.iter().find(|r| r.method.as_str() == "POST").unwrap();
    let body: Value = serde_json::from_slice(&post.body).unwrap();
    let raw = decode_b64url(body["message"]["raw"].as_str().unwrap()).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let msg = mail_parser::MessageParser::default()
        .parse(raw.as_bytes())
        .unwrap();
    assert_eq!(msg.subject(), Some("Re: Quarterly plan"));
    assert_eq!(msg.in_reply_to().as_text(), Some("orig-1@mail.example.com"));
    let refs: Vec<String> = match msg.references() {
        mail_parser::HeaderValue::TextList(l) => l.iter().map(|s| s.to_string()).collect(),
        mail_parser::HeaderValue::Text(t) => vec![t.to_string()],
        _ => vec![],
    };
    assert_eq!(refs, ["root@mail.example.com", "orig-1@mail.example.com"]);
    let addrs = |a: Option<&mail_parser::Address<'_>>| -> Vec<String> {
        a.map(|a| {
            a.iter()
                .filter_map(|x| x.address().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
    };
    assert_eq!(addrs(msg.from()), [ME]);
    assert_eq!(addrs(msg.to()), ["bob@example.com", "carl@example.com"]);
    assert_eq!(addrs(msg.cc()), ["dana@example.com", "eve@example.com"]);
}

#[tokio::test]
async fn send_draft_posts_the_draft_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/gmail/drafts/send"))
        .and(body_json(json!({"id": "r-42"})))
        .respond_with(ok(json!({"id": "sent1", "labelIds": ["SENT"]})))
        .expect(1)
        .mount(&server)
        .await;
    provider(&server).send_draft("r-42").await.unwrap();
}

#[tokio::test]
async fn get_message_parses_the_raw_form() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/orig1"))
        .and(query_param("format", "raw"))
        .respond_with(ok(json!({
            "id": "orig1", "threadId": "thr1", "labelIds": ["INBOX", "UNREAD", "STARRED"],
            "internalDate": NOV3.to_string(), "raw": original_raw()
        })))
        .mount(&server)
        .await;
    let m = provider(&server).get_message("orig1").await.unwrap();
    assert_eq!(m.conversation_id, "thr1");
    assert_eq!(m.from.address, "bob@example.com");
    assert_eq!(m.to.len(), 2);
    assert_eq!(m.cc[0].address, "dana@example.com");
    assert_eq!(m.subject, "Quarterly plan");
    assert!(!m.is_read && !m.is_draft);
    assert_eq!(m.flag_status, pidge_core::FlagStatus::Flagged);
    assert_eq!(m.body_content_type, pidge_core::BodyContentType::Text);
    assert!(m.body_content.contains("Can you review?"));
    assert_eq!(m.received_at.timestamp_millis(), NOV3);
}

#[tokio::test]
async fn calendar_list_maps_timed_all_day_and_own_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/calendars/primary/events"))
        .and(query_param("singleEvents", "true"))
        .and(query_param("orderBy", "startTime"))
        .and(query_param("timeMin", "2026-11-01T00:00:00Z"))
        .respond_with(ok(json!({
            "timeZone": "Europe/Stockholm",
            "items": [
                {
                    "id": "ev1_20261105T090000Z",
                    "status": "confirmed",
                    "summary": "Design review",
                    "location": "Room 4",
                    "description": "Agenda",
                    "start": {"dateTime": "2026-11-05T10:00:00+01:00", "timeZone": "Europe/Stockholm"},
                    "end": {"dateTime": "2026-11-05T11:00:00+01:00", "timeZone": "Europe/Stockholm"},
                    "organizer": {"email": "bob@example.com", "displayName": "Bob"},
                    "attendees": [
                        {"email": "bob@example.com", "organizer": true, "responseStatus": "accepted"},
                        {"email": ME, "self": true, "responseStatus": "tentative"},
                        {"email": "room@resource.example.com", "resource": true, "responseStatus": "accepted"}
                    ],
                    "hangoutLink": "https://meet.google.com/abc-defg-hij",
                    "recurringEventId": "ev1",
                    "reminders": {"useDefault": false, "overrides": [{"method": "popup", "minutes": 10}]}
                },
                {
                    "id": "ev2",
                    "status": "confirmed",
                    "summary": "Offsite",
                    "start": {"date": "2026-11-06"},
                    "end": {"date": "2026-11-08"},
                    "organizer": {"email": ME, "self": true}
                },
                {"id": "ev3", "status": "cancelled", "start": {"date": "2026-11-06"}, "end": {"date": "2026-11-07"}}
            ]
        })))
        .mount(&server)
        .await;
    let p = provider(&server);
    let from = since();
    let to = from + chrono::Duration::days(14);
    let evs = p.list_events(from, to).await.unwrap();
    assert_eq!(evs.len(), 2);
    let a = &evs[0];
    assert_eq!(a.account, ME);
    assert_eq!(a.subject, "Design review");
    assert!(!a.all_day);
    assert_eq!(a.start.at.to_rfc3339(), "2026-11-05T09:00:00+00:00");
    assert_eq!(a.start.tz, "Europe/Stockholm");
    assert!(!a.is_organizer);
    assert_eq!(a.response_status, ResponseStatus::Tentative);
    assert_eq!(a.organizer.address, "bob@example.com");
    assert_eq!(a.organizer.response, ResponseStatus::Organizer);
    assert_eq!(a.attendees.len(), 3);
    assert_eq!(a.attendees[2].kind, pidge_core::AttendeeKind::Resource);
    assert_eq!(
        a.online_meeting_url.as_deref(),
        Some("https://meet.google.com/abc-defg-hij")
    );
    assert_eq!(a.series_master_id.as_deref(), Some("ev1"));
    assert_eq!(a.reminder_minutes, Some(10));
    assert_eq!(a.location.as_deref(), Some("Room 4"));
    let b = &evs[1];
    assert!(b.all_day && b.is_organizer);
    assert_eq!(b.response_status, ResponseStatus::Organizer);
    // Midnight in the calendar's zone.
    assert_eq!(b.start.at.to_rfc3339(), "2026-11-05T23:00:00+00:00");
    assert_eq!(b.end.at.to_rfc3339(), "2026-11-07T23:00:00+00:00");
}

#[tokio::test]
async fn respond_patches_only_the_users_own_answer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/calendars/primary/events/ev1"))
        .respond_with(ok(json!({
            "id": "ev1",
            "start": {"dateTime": "2026-11-05T10:00:00+01:00"},
            "end": {"dateTime": "2026-11-05T11:00:00+01:00"},
            "attendees": [
                {"email": "bob@example.com", "organizer": true, "responseStatus": "accepted"},
                {"email": ME, "self": true, "responseStatus": "needsAction"}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/calendar/calendars/primary/events/ev1"))
        .and(query_param("sendUpdates", "none"))
        .and(body_json(json!({"attendees": [
            {"email": "bob@example.com", "responseStatus": "accepted"},
            {"email": ME, "responseStatus": "declined", "comment": "Travelling"}
        ]})))
        .respond_with(ok(json!({"id": "ev1"})))
        .expect(1)
        .mount(&server)
        .await;
    provider(&server)
        .respond("ev1", Rsvp::Decline, Some("Travelling"), false)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_event_asks_for_meet_and_notifies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/calendar/calendars/primary/events"))
        .and(query_param("sendUpdates", "all"))
        .and(query_param("conferenceDataVersion", "1"))
        .and(body_partial_json(json!({
            "summary": "Sync",
            "start": {"date": "2026-11-05"},
            "end": {"date": "2026-11-06"},
            "attendees": [{"email": "bob@example.com"}],
            "conferenceData": {"createRequest": {"conferenceSolutionKey": {"type": "hangoutsMeet"}}}
        })))
        .respond_with(ok(json!({
            "id": "new1", "summary": "Sync",
            "start": {"date": "2026-11-05"}, "end": {"date": "2026-11-06"},
            "organizer": {"email": ME, "self": true},
            "hangoutLink": "https://meet.google.com/x"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/calendars/primary/events"))
        .and(query_param("maxResults", "1"))
        .respond_with(ok(json!({"timeZone": "UTC", "items": []})))
        .mount(&server)
        .await;
    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
    let e = provider(&server)
        .create_event(&pidge_core::provider::EventSpec {
            title: "Sync".into(),
            when: pidge_core::provider::EventWhen::AllDay {
                first: day,
                last: day,
            },
            location: None,
            body: None,
            attendees: vec![MessageFrom {
                name: String::new(),
                address: "bob@example.com".into(),
            }],
            online_meeting: true,
        })
        .await
        .unwrap();
    assert_eq!(e.id, "new1");
    assert!(e.is_organizer && e.all_day);
    assert_eq!(
        e.online_meeting_url.as_deref(),
        Some("https://meet.google.com/x")
    );
}

#[tokio::test]
async fn free_busy_reports_errors_as_empty_and_all_errors_as_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/calendar/freeBusy"))
        .respond_with(ok(json!({"calendars": {
            "bob@example.com": {"busy": [
                {"start": "2026-11-05T09:00:00Z", "end": "2026-11-05T10:00:00Z"}
            ]},
            "x@other.example.com": {"errors": [{"domain": "global", "reason": "notFound"}], "busy": []}
        }})))
        .mount(&server)
        .await;
    let p = provider(&server);
    let from = since();
    let to = from + chrono::Duration::days(7);
    let r = p
        .free_busy(
            &["bob@example.com".into(), "x@other.example.com".into()],
            from,
            to,
        )
        .await
        .unwrap();
    assert_eq!(r[0].1.len(), 1);
    assert!(r[1].1.is_empty());
    assert!(matches!(
        p.free_busy(&["x@other.example.com".into()], from, to).await,
        Err(ProviderError::Unsupported(_))
    ));
}

#[tokio::test]
async fn ids_outside_the_alphabet_never_reach_google() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let p = provider(&server);
    for bad in ["../labels", "a/b", "a?x=1", "a%2e", ""] {
        assert!(matches!(
            p.get_message(bad).await,
            Err(ProviderError::Invalid(_))
        ));
        assert!(matches!(
            p.get_event(bad).await,
            Err(ProviderError::Invalid(_))
        ));
        assert!(matches!(
            p.attachment_bytes("ok1", bad).await,
            Err(ProviderError::Invalid(_))
        ));
        assert!(matches!(
            p.send_draft(bad).await,
            Err(ProviderError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn persistent_throttling_becomes_throttled_and_401_is_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/slow"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(4)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/denied"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let p = provider(&server);
    assert_eq!(
        p.get_message("slow").await.unwrap_err(),
        ProviderError::Throttled {
            retry_after: Some(0)
        }
    );
    match p.get_message("denied").await.unwrap_err() {
        ProviderError::Auth { account, .. } => assert_eq!(account, ME),
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn attachments_are_listed_from_the_full_payload_and_decoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/m1"))
        .and(query_param("format", "full"))
        .respond_with(ok(json!({
            "id": "m1",
            "payload": {"mimeType": "multipart/mixed", "parts": [
                {"mimeType": "text/plain", "filename": "", "body": {"size": 5}},
                {"mimeType": "application/pdf", "filename": "plan.pdf",
                 "headers": [{"name": "Content-Disposition", "value": "attachment; filename=plan.pdf"}],
                 "body": {"attachmentId": "ANGjdJ_1", "size": 3}},
                {"mimeType": "image/png", "filename": "logo.png",
                 "headers": [{"name": "Content-Disposition", "value": "inline"},
                             {"name": "Content-ID", "value": "<logo@x>"}],
                 "body": {"attachmentId": "ANGjdJ_2", "size": 9}}
            ]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/messages/m1/attachments/ANGjdJ_1"))
        .respond_with(ok(json!({"size": 3, "data": encode_b64url(b"PDF")})))
        .mount(&server)
        .await;
    let p = provider(&server);
    let atts = p.list_attachments("m1").await.unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0].name, "plan.pdf");
    assert!(!atts[0].is_inline);
    assert!(atts[1].is_inline);
    assert_eq!(atts[1].content_id.as_deref(), Some("logo@x"));
    assert_eq!(p.attachment_bytes("m1", "ANGjdJ_1").await.unwrap(), b"PDF");
}
