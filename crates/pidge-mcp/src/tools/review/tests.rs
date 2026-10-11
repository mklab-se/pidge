use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, Request, Respond, ResponseTemplate};

use super::*;
use crate::review::ai::{AiProvider, AiSecret};
use crate::review::classify::{Candidate, Judgment};
use crate::tools::tests::{ToolHarness, request_context, text};

const JANE: &str = "jane@example.com";

/// Every `$batch` sub-request: method, url, body.
type Log = Arc<Mutex<Vec<(String, String, Value)>>>;

/// A message in the fake inbox.
#[derive(Clone)]
struct Msg {
    id: &'static str,
    from: &'static str,
    name: &'static str,
    subject: &'static str,
    minutes_ago: i64,
    list: bool,
}

fn msg(id: &'static str, from: &'static str, subject: &'static str, minutes_ago: i64) -> Msg {
    Msg {
        id,
        from,
        name: "",
        subject,
        minutes_ago,
        list: false,
    }
}

/// Answers `$batch` like a mailbox: the unread listing, the inbox count,
/// headers, PATCHes and moves (a move returns `moved-<id>`), and records
/// every sub-request. `fail_moves` answers moves with 404.
#[derive(Clone)]
struct Mailbox {
    msgs: Arc<Vec<Msg>>,
    log: Log,
    fail_moves: bool,
}

impl Mailbox {
    fn new(msgs: Vec<Msg>) -> Self {
        Self {
            msgs: Arc::new(msgs),
            log: Arc::default(),
            fail_moves: false,
        }
    }

    fn row(m: &Msg) -> Value {
        json!({
            "id": m.id,
            "subject": m.subject,
            "from": { "emailAddress": { "name": m.name, "address": m.from } },
            "receivedDateTime": (Utc::now() - ChronoDuration::minutes(m.minutes_ago)).to_rfc3339(),
            "bodyPreview": format!("preview of {}", m.subject),
            "hasAttachments": false,
            "toRecipients": [ { "emailAddress": { "address": JANE } } ],
            "inferenceClassification": "focused"
        })
    }

    fn sub(&self, method: &str, url: &str) -> (u16, Value) {
        if url.starts_with("/me/mailFolders/inbox/messages") {
            let rows: Vec<Value> = self.msgs.iter().map(Self::row).collect();
            return (200, json!({ "value": rows }));
        }
        if url.starts_with("/me/mailFolders/inbox?") {
            return (200, json!({ "unreadItemCount": self.msgs.len() + 2 }));
        }
        if method == "GET" && url.contains("internetMessageHeaders") {
            let id = url
                .trim_start_matches("/me/messages/")
                .split('?')
                .next()
                .unwrap();
            let list = self.msgs.iter().any(|m| m.id == id && m.list);
            let headers = if list {
                json!([{ "name": "List-Unsubscribe", "value": "<https://example.com/u>" }])
            } else {
                json!([])
            };
            return (200, json!({ "internetMessageHeaders": headers }));
        }
        if method == "POST" && url.ends_with("/move") {
            if self.fail_moves {
                return (404, json!({}));
            }
            let id = url
                .trim_start_matches("/me/messages/")
                .trim_end_matches("/move");
            return (201, json!({ "id": format!("moved-{id}") }));
        }
        (200, json!({}))
    }
}

impl Respond for Mailbox {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let responses: Vec<Value> = body["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let (m, u) = (r["method"].as_str().unwrap(), r["url"].as_str().unwrap());
                self.log
                    .lock()
                    .unwrap()
                    .push((m.to_string(), u.to_string(), r["body"].clone()));
                let (status, body) = self.sub(m, u);
                json!({ "id": r["id"], "status": status, "body": body })
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({ "responses": responses }))
    }
}

/// The single-message GET a review uses for bodies.
struct Bodies;

impl Respond for Bodies {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let id = req.url.path().trim_start_matches("/v1.0/me/messages/");
        ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "subject": "s",
            "from": { "emailAddress": { "name": "n", "address": "n@example.com" } },
            "toRecipients": [], "ccRecipients": [], "bccRecipients": [],
            "receivedDateTime": "2026-10-11T06:00:00Z",
            "sentDateTime": "2026-10-11T06:00:00Z",
            "isRead": false, "hasAttachments": false,
            "body": { "contentType": "text", "content": format!("Body of {id}.\n\nOn Mon someone wrote:\n> old quoted text\n\n-- \nSignature line") }
        }))
    }
}

async fn mount(h: &ToolHarness, mailbox: Mailbox) {
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(mailbox)
        .mount(&h.graph)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1\.0/me/messages/[^/]+$"))
        .respond_with(Bodies)
        .mount(&h.graph)
        .await;
}

fn inbox() -> Vec<Msg> {
    let mut news = msg("news1", "news@shop.example", "Autumn sale", 30);
    news.list = true;
    let mut anna = msg("anna1", "anna@example.com", "Weekend", 10);
    anna.name = "Anna";
    vec![
        anna,
        news,
        msg("bot1", "noreply@bank.example", "Your statement", 20),
    ]
}

async fn start(h: &ToolHarness) -> String {
    text(
        &h.mcp
            .mail_review_start(Parameters(StartArgs::default()), h.ctx())
            .await
            .unwrap(),
    )
}

async fn next(
    h: &ToolHarness,
    session: &str,
    action: Option<NextAction>,
    category: Option<&str>,
) -> String {
    text(
        &h.mcp
            .mail_review_next(
                Parameters(NextArgs {
                    session: session.to_string(),
                    action,
                    id: None,
                    category: category.map(str::to_string),
                }),
                h.ctx(),
            )
            .await
            .unwrap(),
    )
}

fn session_id(out: &str) -> String {
    out.split("Review session ")
        .nth(1)
        .unwrap()
        .split(':')
        .next()
        .unwrap()
        .to_string()
}

/// Waits until the background worker has sent `n` sub-requests matching `pred`.
async fn wait_for(
    log: &Log,
    pred: impl Fn(&(String, String, Value)) -> bool,
    n: usize,
) -> Vec<(String, String, Value)> {
    for _ in 0..100 {
        let hits: Vec<_> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|e| pred(e))
            .cloned()
            .collect();
        if hits.len() >= n {
            return hits;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "background requests never arrived: {:?}",
        log.lock().unwrap()
    );
}

#[tokio::test]
async fn start_sorts_by_graph_signals_and_offers_the_bulk_clean_up() {
    let h = ToolHarness::new(&[JANE]).await;
    mount(&h, Mailbox::new(inbox())).await;
    let out = start(&h).await;

    assert!(out.contains("3 unread messages"), "{out}");
    assert!(
        out.contains("By category: Newsletters 1, Notifications 1, Other 1"),
        "{out}"
    );
    assert!(
        out.contains("Suggest first: archive all 1 in Newsletters?"),
        "{out}"
    );
    assert!(out.contains("2 older unread are not included"), "{out}");
    assert!(out.contains("hasn't chosen categories yet"), "{out}");
    assert!(
        !out.contains("accounts_connect_ai"),
        "no hint for a small review: {out}"
    );
    // Other (real people) comes before bulk and automated mail.
    assert!(out.contains("item 1 of 3 · Other · today"), "{out}");
    assert!(out.contains("id anna1"), "{out}");
    let block = out.split("<untrusted-email-content>").nth(1).unwrap();
    assert!(
        block.contains("from: Anna") && block.contains("Body of anna1."),
        "{out}"
    );
    assert!(
        !block.contains("old quoted text") && !block.contains("Signature line"),
        "{out}"
    );
    assert!(out.contains("next: mail_review_next session="), "{out}");
}

#[tokio::test]
async fn next_applies_the_action_in_the_background_and_returns_the_next_message() {
    let h = ToolHarness::new(&[JANE]).await;
    let mailbox = Mailbox::new(inbox());
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let s = session_id(&start(&h).await);

    let out = next(&h, &s, Some(NextAction::Archive), None).await;
    assert!(out.starts_with("Archived."), "{out}");
    assert!(out.contains("item 2 of 3"), "{out}");

    let moves = wait_for(
        &log,
        |(m, u, _)| m == "POST" && u == "/me/messages/anna1/move",
        1,
    )
    .await;
    assert_eq!(moves[0].2, json!({ "destinationId": "archive" }));
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|(m, u, b)| m == "PATCH" && u == "/me/messages/anna1" && b["isRead"] == true)
    );
    // The review now counts as the last one.
    assert!(h.record().await.review.last_review_at.is_some());

    // A plain "next" marks read and keeps the message in the inbox.
    let out = next(&h, &s, None, None).await;
    assert!(out.starts_with("Marked read."), "{out}");
    let out = next(&h, &s, Some(NextAction::KeepUnread), None).await;
    assert!(out.contains("Done: nothing left in this review"), "{out}");
}

#[tokio::test]
async fn bulk_archive_takes_every_bulk_category_and_undo_brings_them_back() {
    let h = ToolHarness::new(&[JANE]).await;
    let mailbox = Mailbox::new(inbox());
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let s = session_id(&start(&h).await);

    let out = next(&h, &s, Some(NextAction::Archive), Some("bulk")).await;
    assert!(
        out.starts_with("Archived: 1 message in Newsletters, Promotions."),
        "{out}"
    );
    // The current message (Anna) is still current.
    assert!(out.contains("id anna1"), "{out}");
    wait_for(
        &log,
        |(m, u, _)| m == "POST" && u == "/me/messages/news1/move",
        1,
    )
    .await;

    let out = next(&h, &s, Some(NextAction::Undo), None).await;
    assert!(
        out.starts_with("Undone: 1 message back in the inbox"),
        "{out}"
    );
    assert!(
        out.contains("id moved-moved-news1"),
        "the restored message carries its new id: {out}"
    );
    let log = log.lock().unwrap();
    assert!(log.iter().any(|(m, u, b)| m == "POST"
        && u == "/me/messages/moved-news1/move"
        && b["destinationId"] == "inbox"));
    assert!(log.iter().any(|(m, u, b)| m == "PATCH"
        && u == "/me/messages/moved-moved-news1"
        && b["isRead"] == false));
}

#[tokio::test]
async fn a_failed_background_action_is_reported_on_the_next_call() {
    let h = ToolHarness::new(&[JANE]).await;
    let mut mailbox = Mailbox::new(inbox());
    mailbox.fail_moves = true;
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let s = session_id(&start(&h).await);
    let first = next(&h, &s, Some(NextAction::Delete), None).await;
    wait_for(&log, |(m, _, _)| m == "POST", 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let second = next(&h, &s, None, None).await;
    // Reported on whichever response follows the failure.
    let out = format!("{first}\n{second}");
    assert!(
        out.contains("earlier: deleting message anna1 failed: not found"),
        "{out}"
    );
}

#[tokio::test]
async fn an_unknown_or_foreign_session_starts_a_new_review_and_touches_nothing_else() {
    let h = ToolHarness::new(&[JANE]).await;
    let mailbox = Mailbox::new(inbox());
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let s = session_id(&start(&h).await);

    // Another signed-in user presenting Jane's session id.
    let other = request_context("anna@example.com");
    let out = text(
        &h.mcp
            .mail_review_next(
                Parameters(NextArgs {
                    session: s.clone(),
                    action: Some(NextAction::Archive),
                    id: None,
                    category: Some("bulk".into()),
                }),
                other,
            )
            .await
            .unwrap(),
    );
    assert!(out.starts_with("(That review session had ended"), "{out}");
    assert!(out.contains("Nothing was changed."), "{out}");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !log.lock().unwrap().iter().any(|(m, _, _)| m == "POST"),
        "no move was made"
    );

    // An expired session with an id still applies the action, then restarts.
    let out = text(
        &h.mcp
            .mail_review_next(
                Parameters(NextArgs {
                    session: "gone".into(),
                    action: Some(NextAction::Archive),
                    id: Some("anna1".into()),
                    category: None,
                }),
                h.ctx(),
            )
            .await
            .unwrap(),
    );
    assert!(out.contains("Archived: anna1."), "{out}");
    assert!(out.contains("Review session "), "{out}");
}

fn typesafe_answer(category: &str, needs_reply: f64) -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "category": { "type": "choice", "choice": category, "probabilities": { category: 0.9 }, "confidence": 0.85 },
            "needs_reply": { "type": "noul", "noul": needs_reply },
            "importance": { "type": "score", "score": 3.0, "probabilities": { "3": 1.0 }, "confidence": 0.9 }
        },
        "usage": { "input_tokens": 900, "output_tokens": 3 }
    })
}

/// Classifies by sender: Anna is Important and needs a reply, the rest Promotions.
struct Jev;

impl Respond for Jev {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let from = body["state"]["email"]["from"]["address"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            body["questions"].as_object().unwrap().len(),
            3,
            "one request carries every question"
        );
        if from == "anna@example.com" {
            ResponseTemplate::new(200).set_body_json(typesafe_answer("Important", 0.9))
        } else {
            ResponseTemplate::new(200).set_body_json(typesafe_answer("Promotions", 0.0))
        }
    }
}

async fn configure_ai(h: &ToolHarness) {
    crate::review::ai::save(
        &h.secrets,
        JANE,
        &AiSecret {
            provider: AiProvider::Typesafe,
            model: "jev-latest".into(),
            api_key: "ts-key".into(),
            updated_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let mut rec = h.record().await;
    rec.ai.set_status(AiStatus::Ok, Utc::now());
    rec.ai.provider = Some("TypeSafe jev-latest".into());
    h.state.users.save(&rec).await.unwrap();
}

#[tokio::test]
async fn with_a_key_jev_sorts_and_flags_what_needs_a_reply() {
    let h = ToolHarness::new(&[JANE]).await;
    mount(&h, Mailbox::new(inbox())).await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(Jev)
        .expect(3)
        .mount(&h.graph)
        .await;
    configure_ai(&h).await;

    let out = start(&h).await;
    assert!(
        out.contains("By category: Important 1, Promotions 2"),
        "{out}"
    );
    assert!(
        out.contains("item 1 of 3 · Important · needs reply"),
        "{out}"
    );
    assert!(!out.contains("note (tell the user once)"), "{out}");
    // Cached: a second review the same day asks nothing again.
    start(&h).await;
}

#[tokio::test]
async fn out_of_credits_is_reported_and_not_retried_for_an_hour() {
    let h = ToolHarness::new(&[JANE]).await;
    mount(&h, Mailbox::new(inbox())).await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(402)
                .set_body_json(json!({"detail":{"message":"Payment required"}})),
        )
        .mount(&h.graph)
        .await;
    configure_ai(&h).await;

    let out = start(&h).await;
    assert!(
        out.contains(
            "AI sorting is set up (TypeSafe jev-latest) but the account is out of credits"
        ),
        "{out}"
    );
    assert!(out.contains("the mailbox's own signals"), "{out}");
    let rec = h.record().await;
    assert_eq!(rec.ai.status, AiStatus::NoCredits);
    assert!(rec.ai.last_attempt_at.is_some());

    let before = jev_calls(&h).await;
    let out = start(&h).await;
    assert_eq!(jev_calls(&h).await, before, "not retried within the hour");
    assert!(out.contains("out of credits"), "still reported: {out}");
}

#[tokio::test]
async fn a_large_unsorted_review_without_ai_suggests_a_key_once() {
    let h = ToolHarness::new(&[JANE]).await;
    let ids: Vec<&'static str> = (0..16)
        .map(|i| &*Box::leak(format!("p{i}").into_boxed_str()))
        .collect();
    let msgs = ids
        .iter()
        .map(|id| msg(id, "person@example.com", "Hello", 5))
        .collect();
    mount(&h, Mailbox::new(msgs)).await;
    let out = start(&h).await;
    assert!(out.contains("accounts_connect_ai sets it up"), "{out}");
    assert!(h.record().await.ai.last_hint_at.is_some());
    let out = start(&h).await;
    assert!(
        !out.contains("accounts_connect_ai sets it up"),
        "only once: {out}"
    );
}

#[tokio::test]
async fn rules_put_a_sender_in_its_category_without_ai() {
    let h = ToolHarness::new(&[JANE]).await;
    mount(&h, Mailbox::new(inbox())).await;
    let mut rec = h.record().await;
    let mut cats = crate::review::categories::defaults();
    cats[1].rules.push("sender:anna@example.com".into());
    rec.review.categories = Some(cats);
    rec.review.confirmed = true;
    h.state.users.save(&rec).await.unwrap();
    let out = start(&h).await;
    assert!(out.contains("item 1 of 3 · Personal"), "{out}");
    assert!(!out.contains("hasn't chosen categories"), "{out}");
}

#[test]
fn ordering_puts_needs_reply_and_importance_first_within_a_category() {
    let cats = crate::review::categories::defaults();
    let item = |id: &'static str, cat: &str, reply: f64, imp: f64| Item {
        cand: Candidate {
            account: JANE.into(),
            id: id.into(),
            from_name: String::new(),
            from_address: String::new(),
            to_me: true,
            subject: String::new(),
            preview: String::new(),
            has_attachments: false,
            received_at: Utc::now(),
            focused: None,
            list: false,
            automated: false,
        },
        judgment: Judgment {
            category: cat.into(),
            needs_reply: reply,
            importance: imp,
            source: Source::Ai,
        },
        handled: false,
    };
    let mut items = vec![
        item("promo", "Promotions", 0.0, 0.1),
        item("other", OTHER, 0.0, 0.5),
        item("imp-low", "Important", 0.0, 0.4),
        item("imp-reply", "Important", 0.9, 0.2),
        item("imp-high", "Important", 0.0, 0.9),
    ];
    order(&mut items, &cats);
    let ids: Vec<&str> = items.iter().map(|i| i.cand.id.as_str()).collect();
    assert_eq!(ids, ["imp-reply", "imp-high", "imp-low", "other", "promo"]);
}

async fn jev_calls(h: &ToolHarness) -> usize {
    h.graph
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/systemone")
        .count()
}
