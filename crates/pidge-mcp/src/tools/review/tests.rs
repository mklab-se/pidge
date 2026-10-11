use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, Request, Respond, ResponseTemplate};

use super::*;
use crate::review::ai::{AiProvider, AiSecret};
use crate::review::classify::{Candidate, Judgment};
use crate::tools::tests::{ToolHarness, access_token, request_context, text};

const JANE: &str = "jane@example.com";
const WORK: &str = "jane@work.example";

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
    categories: Vec<&'static str>,
}

fn msg(id: &'static str, from: &'static str, subject: &'static str, minutes_ago: i64) -> Msg {
    Msg {
        id,
        from,
        name: "",
        subject,
        minutes_ago,
        list: false,
        categories: Vec::new(),
    }
}

/// Answers `$batch` like a mailbox: the inbox listing, the inbox count,
/// headers, PATCHes, moves (a move returns `moved-<id>`) and the mailbox's
/// categories (one list per mailbox, told apart by the access token), and
/// records every sub-request. `fail_moves` answers moves with 404;
/// `deny_masters` answers the categories with 403.
#[derive(Clone)]
struct Mailbox {
    msgs: Arc<Vec<Msg>>,
    log: Log,
    /// `(mailbox token, method, url)` of every sub-request.
    calls: Arc<Mutex<Vec<(String, String, String)>>>,
    masters: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    fail_moves: bool,
    deny_masters: bool,
}

impl Mailbox {
    fn new(msgs: Vec<Msg>) -> Self {
        Self {
            msgs: Arc::new(msgs),
            log: Arc::default(),
            calls: Arc::default(),
            masters: Arc::default(),
            fail_moves: false,
            deny_masters: false,
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
            "inferenceClassification": "focused",
            "categories": m.categories,
        })
    }

    fn set_masters(&self, mailbox: &str, masters: Value) {
        self.masters
            .lock()
            .unwrap()
            .insert(access_token(mailbox), masters.as_array().unwrap().clone());
    }

    fn masters_of(&self, mailbox: &str) -> Vec<Value> {
        self.masters
            .lock()
            .unwrap()
            .get(&access_token(mailbox))
            .cloned()
            .unwrap_or_default()
    }

    fn sub(&self, token: &str, method: &str, url: &str, body: &Value) -> (u16, Value) {
        if url.starts_with("/me/outlook/masterCategories") {
            if self.deny_masters {
                return (403, json!({}));
            }
            let mut all = self.masters.lock().unwrap();
            let list = all.entry(token.to_string()).or_default();
            let id = url.trim_start_matches("/me/outlook/masterCategories/");
            return match method {
                "GET" => (200, json!({ "value": list.clone() })),
                "POST" => {
                    let mut m = body.clone();
                    m["id"] = json!(format!("mc-{}", list.len() + 1));
                    list.push(m.clone());
                    (201, m)
                }
                "PATCH" => {
                    for m in list.iter_mut().filter(|m| m["id"] == id) {
                        m["color"] = body["color"].clone();
                    }
                    (200, json!({}))
                }
                _ => {
                    list.retain(|m| m["id"] != id);
                    (204, json!({}))
                }
            };
        }
        if url.starts_with("/me/mailFolders/inbox/messages") {
            let rows: Vec<Value> = self.msgs.iter().map(Self::row).collect();
            return (200, json!({ "value": rows }));
        }
        if url.starts_with("/me/messages?") {
            // Messages by category: those carrying it.
            let rows: Vec<Value> = self
                .msgs
                .iter()
                .filter(|m| !m.categories.is_empty())
                .map(Self::row)
                .collect();
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

    /// The `(method, url, body)` sub-requests sent to `mailbox`'s categories.
    fn master_calls(&self, mailbox: &str) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _, u)| *t == access_token(mailbox) && u.contains("masterCategories"))
            .map(|(_, m, u)| (m.clone(), u.clone()))
            .collect()
    }
}

impl Respond for Mailbox {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let token = req
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .trim_start_matches("Bearer ")
            .to_string();
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
                self.calls
                    .lock()
                    .unwrap()
                    .push((token.clone(), m.to_string(), u.to_string()));
                let (status, body) = self.sub(&token, m, u, &r["body"]);
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
            "body": { "contentType": "html", "content": format!("<p>Body of {id}. <a href=\"https://example.com/track?u=1\">Read more</a></p><p>---</p><p>On Mon someone wrote:</p><blockquote>&gt; old quoted text</blockquote><p>-- <br>Signature line</p>") }
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

async fn start_with(h: &ToolHarness, args: StartArgs) -> String {
    text(
        &h.mcp
            .mail_review_start(Parameters(args), h.ctx())
            .await
            .unwrap(),
    )
}

async fn start(h: &ToolHarness) -> String {
    start_with(h, StartArgs::default()).await
}

async fn next_id(
    h: &ToolHarness,
    session: &str,
    action: Option<NextAction>,
    id: Option<&str>,
    category: Option<&str>,
) -> String {
    text(
        &h.mcp
            .mail_review_next(
                Parameters(NextArgs {
                    session: session.to_string(),
                    action,
                    id: id.map(str::to_string),
                    category: category.map(str::to_string),
                }),
                h.ctx(),
            )
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
    next_id(h, session, action, None, category).await
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

const STEER: &str = "Read this to the user. When they answer, call mail_review_next (session=";

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
    // Other (real people) comes before bulk and automated mail; the short
    // id and how many are left lead the item.
    assert!(out.contains("r1 · Other · today"), "{out}");
    assert!(out.contains("· 3 left"), "{out}");
    assert!(!out.contains("id anna1"), "the full id stays out: {out}");
    let block = out.split("<untrusted-email-content>").nth(1).unwrap();
    assert!(
        block.contains("from: Anna") && block.contains("Body of anna1."),
        "{out}"
    );
    // Link text, not the link target.
    assert!(
        block.contains("Read more") && !block.contains("example.com/track"),
        "{out}"
    );
    assert!(
        !block.contains("old quoted text") && !block.contains("Signature line"),
        "{out}"
    );
    // The steering line closes the response.
    let last = out.lines().last().unwrap();
    assert!(last.starts_with(STEER), "{out}");
    assert!(last.contains("id=r1"), "{out}");
    assert!(
        last.ends_with("During a review don't use mail_overview, mail_read or mail_act."),
        "{out}"
    );
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
    assert!(out.contains("r2 · Newsletters"), "{out}");
    assert!(out.contains("· 2 left"), "{out}");
    assert!(out.lines().last().unwrap().starts_with(STEER), "{out}");

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
    assert!(out.contains("· 1 left"), "{out}");
    let out = next(&h, &s, Some(NextAction::KeepUnread), None).await;
    assert!(out.contains("Done: nothing left in this review"), "{out}");
    assert!(
        out.ends_with("Read this to the user. The review is over."),
        "{out}"
    );
}

#[tokio::test]
async fn short_ids_and_full_ids_both_name_an_item() {
    let h = ToolHarness::new(&[JANE]).await;
    let mailbox = Mailbox::new(inbox());
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let s = session_id(&start(&h).await);

    // r3 is the bank statement, not the current item.
    next_id(&h, &s, Some(NextAction::Delete), Some("r3"), None).await;
    let moves = wait_for(&log, |(m, u, _)| m == "POST" && u.ends_with("/move"), 1).await;
    assert_eq!(moves[0].1, "/me/messages/bot1/move");
    // The full Graph id still works.
    next_id(&h, &s, Some(NextAction::Flag), Some("news1"), None).await;
    wait_for(
        &log,
        |(m, u, b)| m == "PATCH" && u == "/me/messages/news1" && b["flag"].is_object(),
        1,
    )
    .await;
    // Still validated, and an id outside the review is refused.
    let bad = h
        .mcp
        .mail_review_next(
            Parameters(NextArgs {
                session: s.clone(),
                action: Some(NextAction::Archive),
                id: Some("../x".into()),
                category: None,
            }),
            h.ctx(),
        )
        .await;
    assert!(bad.is_err());
    let unknown = h
        .mcp
        .mail_review_next(
            Parameters(NextArgs {
                session: s.clone(),
                action: Some(NextAction::Archive),
                id: Some("r9".into()),
                category: None,
            }),
            h.ctx(),
        )
        .await;
    assert!(unknown.is_err());
}

#[tokio::test]
async fn bulk_archive_answers_with_a_receipt_and_undo_brings_them_back() {
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
    // The current message (Anna) is still current: a reminder, not the body.
    assert!(
        out.contains("Still on: r1, Anna: Weekend (2 left)."),
        "{out}"
    );
    assert!(!out.contains("untrusted-email-content"), "{out}");
    assert!(out.lines().last().unwrap().starts_with(STEER), "{out}");
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
    assert!(out.contains("r2 · Newsletters"), "{out}");
    let log = log.lock().unwrap();
    assert!(log.iter().any(|(m, u, b)| m == "POST"
        && u == "/me/messages/moved-news1/move"
        && b["destinationId"] == "inbox"));
    // The restored message carries its new id.
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
        out.contains("earlier: deleting message r1 failed: not found"),
        "{out}"
    );
}

#[tokio::test]
async fn copies_of_one_message_are_one_item_and_acted_on_together() {
    // Both mailboxes hold the same three messages.
    let h = ToolHarness::new(&[JANE, WORK]).await;
    let mailbox = Mailbox::new(inbox());
    let calls = mailbox.calls.clone();
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let out = start(&h).await;
    assert!(out.contains("3 unread messages"), "{out}");
    assert!(out.contains("r1 · Other · today"), "{out}");
    assert!(out.contains("(2 copies)"), "{out}");
    let s = session_id(&out);

    next(&h, &s, Some(NextAction::Archive), None).await;
    wait_for(
        &log,
        |(m, u, _)| m == "POST" && u == "/me/messages/anna1/move",
        2,
    )
    .await;
    let tokens: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, m, u)| m == "POST" && u == "/me/messages/anna1/move")
        .map(|(t, _, _)| t.clone())
        .collect();
    assert!(tokens.contains(&access_token(JANE)) && tokens.contains(&access_token(WORK)));
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

    // A short id means nothing once its session is gone.
    let out = next_id(&h, "gone", Some(NextAction::Archive), Some("r1"), None).await;
    assert!(
        out.contains("Nothing was changed: r1 belonged to the review that ended."),
        "{out}"
    );

    // An expired session with a full id still applies the action, then restarts.
    let out = next_id(&h, "gone", Some(NextAction::Archive), Some("anna1"), None).await;
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

/// Classifies by sender: Anna is Important and needs a reply, the rest
/// Promotions (also "needing a reply", which bulk mail never shows).
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
            ResponseTemplate::new(200).set_body_json(typesafe_answer("Promotions", 0.9))
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
    assert!(out.contains("r1 · Important · needs reply"), "{out}");
    assert!(!out.contains("note (tell the user once)"), "{out}");
    let s = session_id(&out);
    let out = next(&h, &s, None, None).await;
    assert!(out.contains("· Promotions ·"), "{out}");
    assert!(
        !out.contains("needs reply"),
        "bulk mail never needs a reply: {out}"
    );
    // Cached: a second review the same day asks nothing again.
    start(&h).await;
}

#[test]
fn needs_reply_takes_a_sure_ai_and_a_person() {
    let cats = crate::review::categories::defaults();
    let c = Candidate::default();
    let j = |category: &str, p: f64, source: Source| Judgment {
        category: category.into(),
        needs_reply: p,
        importance: 0.5,
        source,
    };
    assert!(needs_reply(&c, &j("Important", 0.7, Source::Ai), &cats));
    assert!(!needs_reply(&c, &j("Important", 0.69, Source::Ai), &cats));
    assert!(!needs_reply(&c, &j("Important", 0.9, Source::Rule), &cats));
    assert!(!needs_reply(&c, &j("Newsletters", 0.9, Source::Ai), &cats));
    let bot = Candidate {
        automated: true,
        ..Default::default()
    };
    assert!(!needs_reply(&bot, &j("Important", 0.9, Source::Ai), &cats));
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
    let subjects: Vec<&'static str> = (0..16)
        .map(|i| &*Box::leak(format!("Hello {i}").into_boxed_str()))
        .collect();
    let msgs = ids
        .iter()
        .zip(&subjects)
        .map(|(id, s)| msg(id, "person@example.com", s, 5))
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
    assert!(out.contains("r1 · Personal"), "{out}");
    assert!(!out.contains("hasn't chosen categories"), "{out}");
}

#[test]
fn ordering_puts_needs_reply_and_importance_first_within_a_category() {
    let cats = crate::review::categories::defaults();
    let item = |id: &'static str, cat: &str, reply: f64, imp: f64| Item {
        cand: Candidate {
            account: JANE.into(),
            id: id.into(),
            to_me: true,
            received_at: Utc::now(),
            ..Default::default()
        },
        copies: Vec::new(),
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

// --- mailbox categories ------------------------------------------------

/// Records that `mailbox`'s stored sign-in was granted the categories scope.
async fn grant_scope(h: &ToolHarness, mailbox: &str) {
    let mut rec = h.state.users.load_mailbox(mailbox).await.unwrap().unwrap();
    rec.tokens.scope =
        Some("Mail.ReadWrite https://graph.microsoft.com/MailboxSettings.ReadWrite".into());
    h.state.users.save_mailbox(&rec, mailbox).await.unwrap();
}

async fn categories(h: &ToolHarness, args: crate::tools::categories::CategoriesArgs) -> String {
    text(
        &h.mcp
            .mail_categories(Parameters(args), h.ctx())
            .await
            .unwrap(),
    )
}

fn sync_args(enabled: Option<bool>) -> crate::tools::categories::CategoriesArgs {
    crate::tools::categories::CategoriesArgs {
        action: Some(crate::tools::categories::CategoriesAction::MailboxSync),
        enabled,
        ..Default::default()
    }
}

fn master(id: &str, name: &str, color: &str) -> Value {
    json!({ "id": id, "displayName": name, "color": color })
}

#[tokio::test]
async fn sync_creates_links_and_follows_the_users_changes_in_the_mailbox() {
    let h = ToolHarness::new(&[JANE, WORK]).await;
    grant_scope(&h, JANE).await;
    grant_scope(&h, WORK).await;
    let mailbox = Mailbox::new(inbox());
    mailbox.set_masters(
        JANE,
        json!([
            master("red", "Red category", "preset0"),
            master("blå", "Blå kategori", "preset7"),
            master("own-bills", "bills and receipts", "preset5"),
            master("hobby", "Hobby", "preset9"),
        ]),
    );
    mount(&h, mailbox.clone()).await;

    let out = categories(&h, sync_args(Some(true))).await;
    assert!(out.starts_with("Mailbox categories are on."), "{out}");
    // Jane: five created, the user's own "bills and receipts" linked by
    // name; Work: all six created.
    assert!(
        out.contains("11 created, 1 of the user's own linked"),
        "{out}"
    );
    assert!(out.contains("Hobby (jane@example.com)"), "{out}");
    assert!(
        !out.contains("Red category") && !out.contains("Blå kategori"),
        "{out}"
    );
    let jane = mailbox.masters_of(JANE);
    assert_eq!(jane.len(), 9);
    let bills = jane.iter().find(|m| m["id"] == "own-bills").unwrap();
    assert_eq!(
        bills["color"], "preset5",
        "a linked category keeps its colour"
    );
    // New categories avoid the colours already used.
    let important = jane
        .iter()
        .find(|m| m["displayName"] == "Important")
        .unwrap();
    assert_eq!(important["color"], "preset1");
    // The colour-only defaults were never written to.
    assert!(
        !mailbox
            .master_calls(JANE)
            .iter()
            .any(|(m, u)| m != "GET" && (u.ends_with("/red") || u.contains("bl"))),
        "{:?}",
        mailbox.master_calls(JANE)
    );

    // In Jane's mailbox the user recolours Important, deletes Personal and
    // renames Newsletters.
    {
        let mut all = mailbox.masters.lock().unwrap();
        let list = all.get_mut(&access_token(JANE)).unwrap();
        for m in list.iter_mut() {
            if m["displayName"] == "Important" {
                m["color"] = json!("preset20");
            }
            if m["displayName"] == "Newsletters" {
                m["displayName"] = json!("Nyhetsbrev");
            }
        }
        list.retain(|m| m["displayName"] != "Personal");
    }
    let out = categories(&h, sync_args(Some(true))).await;
    assert!(
        out.contains("the category Personal was removed in jane@example.com; should it come back there (mail_categories action=mailbox_sync restore=Personal account=jane@example.com)"),
        "{out}"
    );
    assert!(
        out.contains("the category Newsletters was renamed to Nyhetsbrev in jane@example.com"),
        "{out}"
    );
    // Not recreated in Jane's mailbox; the colour went to Work's.
    assert!(
        !mailbox
            .masters_of(JANE)
            .iter()
            .any(|m| m["displayName"] == "Personal")
    );
    let work_important = mailbox
        .masters_of(WORK)
        .into_iter()
        .find(|m| m["displayName"] == "Important")
        .unwrap();
    assert_eq!(work_important["color"], "preset20");
    let rec = h.record().await;
    let ms = &rec.review.mailbox_sync;
    assert_eq!(ms.colors["Important"], "preset20");
    assert_eq!(ms.mailboxes[JANE].removed, vec!["Personal".to_string()]);
    assert!(ms.notes.is_empty(), "shown once, then cleared");

    // The user wants Personal back in Jane's mailbox.
    let mut restore = sync_args(None);
    restore.restore = Some("personal".into());
    restore.account = Some(JANE.into());
    let out = categories(&h, restore).await;
    assert!(
        out.starts_with("Personal will be written to jane@example.com again."),
        "{out}"
    );
    assert!(
        mailbox
            .masters_of(JANE)
            .iter()
            .any(|m| m["displayName"] == "Personal")
    );
}

#[tokio::test]
async fn a_mailbox_without_the_scope_is_tagged_and_told_to_reconnect_once() {
    let h = ToolHarness::new(&[JANE, WORK]).await;
    // Jane signed in after the scope was added; Work before it.
    grant_scope(&h, JANE).await;
    let mut mailbox = Mailbox::new(inbox());
    mailbox.deny_masters = true;
    let log = mailbox.log.clone();
    mount(&h, mailbox.clone()).await;

    let out = categories(&h, sync_args(Some(true))).await;
    for mb in [JANE, WORK] {
        assert!(
            out.contains(&format!("{mb} needs accounts_connect once")),
            "{out}"
        );
    }
    // Work's categories were never even asked for.
    assert!(mailbox.master_calls(WORK).is_empty());
    let accounts = text(&h.mcp.accounts_list(h.ctx()).await.unwrap());
    assert!(
        accounts.contains("categories: needs accounts_connect once"),
        "{accounts}"
    );

    // A review still tags the mail.
    start(&h).await;
    wait_for(
        &log,
        |(m, u, b)| {
            m == "PATCH" && u == "/me/messages/news1" && b["categories"] == json!(["Newsletters"])
        },
        2,
    )
    .await;
}

#[tokio::test]
async fn a_review_tags_its_mail_in_the_background_keeping_the_users_own_categories() {
    let h = ToolHarness::new(&[JANE]).await;
    grant_scope(&h, JANE).await;
    let mut msgs = inbox();
    // Already in another pidge category, plus one of the user's own.
    msgs[1].categories = vec!["Promotions", "Hobby"];
    let mailbox = Mailbox::new(msgs);
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    categories(&h, sync_args(Some(true))).await;

    let out = start(&h).await;
    assert!(out.lines().last().unwrap().starts_with(STEER), "{out}");
    let tags = wait_for(
        &log,
        |(m, u, b)| m == "PATCH" && u.starts_with("/me/messages/") && b.get("categories").is_some(),
        2,
    )
    .await;
    let of = |id: &str| {
        tags.iter()
            .find(|(_, u, _)| u.ends_with(id))
            .map(|(_, _, b)| b["categories"].clone())
    };
    assert_eq!(of("news1"), Some(json!(["Hobby", "Newsletters"])));
    assert_eq!(of("bot1"), Some(json!(["Notifications"])));
    assert_eq!(of("anna1"), None, "Other is not written");
}

#[tokio::test]
async fn sort_mail_needs_sync_and_reports_counts_per_category() {
    let h = ToolHarness::new(&[JANE]).await;
    grant_scope(&h, JANE).await;
    let mailbox = Mailbox::new(inbox());
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let sort = || crate::tools::categories::CategoriesArgs {
        action: Some(crate::tools::categories::CategoriesAction::SortMail),
        days: Some(1),
        ..Default::default()
    };

    let out = categories(&h, sort()).await;
    assert!(out.contains("Ask the user whether to turn them on (mail_categories action=mailbox_sync enabled=true)"), "{out}");
    assert!(!log.lock().unwrap().iter().any(|(m, _, _)| m == "PATCH"));

    categories(&h, sync_args(Some(true))).await;
    let out = categories(&h, sort()).await;
    assert!(
        out.starts_with("3 sorted (today): Newsletters 1, Notifications 1, Other 1."),
        "{out}"
    );
    assert!(out.contains("AI sorting is off"), "{out}");
    assert!(
        h.state.review.sessions.latest(JANE).is_none(),
        "no review was opened"
    );
    // Read and unread alike.
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|(_, u, _)| u.starts_with("/me/mailFolders/inbox/messages")
                && !u.contains("isRead"))
    );
    wait_for(
        &log,
        |(m, u, b)| {
            m == "PATCH" && u == "/me/messages/bot1" && b["categories"] == json!(["Notifications"])
        },
        1,
    )
    .await;

    let mut bad = sort();
    bad.days = Some(32);
    assert!(
        h.mcp
            .mail_categories(Parameters(bad), h.ctx())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_review_by_category_reads_the_tag_when_syncing_and_filters_otherwise() {
    let h = ToolHarness::new(&[JANE]).await;
    grant_scope(&h, JANE).await;
    let mut msgs = inbox();
    msgs[1].categories = vec!["Newsletters"];
    let mailbox = Mailbox::new(msgs);
    let log = mailbox.log.clone();
    mount(&h, mailbox).await;
    let by = |c: &str| StartArgs {
        category: Some(Names::One(c.into())),
        ..Default::default()
    };

    // Sync off: the usual window, sorted, then only that category.
    let out = start_with(&h, by("newsletters")).await;
    assert!(
        out.contains("1 unread message in Newsletters, today."),
        "{out}"
    );
    assert!(out.contains("r1 · Newsletters"), "{out}");

    // Sync on: read straight from the mailbox by its tag, read or unread.
    categories(&h, sync_args(Some(true))).await;
    let out = start_with(
        &h,
        StartArgs {
            category: Some(Names::Many(vec!["Newsletters".into(), "Promotions".into()])),
            ..Default::default()
        },
    )
    .await;
    assert!(
        out.contains("1 message in Newsletters, Promotions."),
        "{out}"
    );
    let listed = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, u, _)| u.starts_with("/me/mailFolders/inbox/messages"))
        .map(|(_, u, _)| u.clone())
        .next_back()
        .unwrap();
    assert!(
        listed.contains("categories%2Fany%28c:c%20eq%20%27Newsletters%27%29%20or%20categories%2Fany%28c:c%20eq%20%27Promotions%27%29"),
        "{listed}"
    );
    assert!(!listed.contains("isRead"), "{listed}");

    assert!(
        h.mcp
            .mail_review_start(Parameters(by("Nope")), h.ctx())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn add_takes_rules_examples_by_short_id_and_a_position_at_once() {
    let h = ToolHarness::new(&[JANE]).await;
    mount(&h, Mailbox::new(inbox())).await;
    start(&h).await;
    let out = categories(
        &h,
        crate::tools::categories::CategoriesArgs {
            action: Some(crate::tools::categories::CategoriesAction::Add),
            name: Some("Bank".into()),
            rules: Some(vec!["domain:bank.example".into()]),
            example_ids: Some(vec!["r3".into()]),
            position: Some(1),
            ..Default::default()
        },
    )
    .await;
    assert!(
        out.starts_with("Added Bank with 1 rule and 1 example."),
        "{out}"
    );
    assert!(out.contains("1. Bank"), "{out}");
    assert!(
        out.contains("Bank example 1: n <n@example.com> / s"),
        "{out}"
    );
    // Sync is off: offered once.
    assert!(
        out.contains("pidge can also put these categories on the mail"),
        "{out}"
    );
    let cats = h.record().await.review.categories.unwrap();
    assert_eq!(cats[0].rules, vec!["domain:bank.example".to_string()]);
    let out = categories(
        &h,
        crate::tools::categories::CategoriesArgs {
            action: Some(crate::tools::categories::CategoriesAction::Confirm),
            ..Default::default()
        },
    )
    .await;
    assert!(!out.contains("pidge can also put"), "only once: {out}");
}

#[tokio::test]
async fn renaming_and_removing_follow_into_the_mailboxes() {
    let h = ToolHarness::new(&[JANE]).await;
    grant_scope(&h, JANE).await;
    let mut msgs = inbox();
    msgs[1].categories = vec!["Newsletters", "Hobby"];
    let mailbox = Mailbox::new(msgs);
    let log = mailbox.log.clone();
    mount(&h, mailbox.clone()).await;
    categories(&h, sync_args(Some(true))).await;
    let id_of = |name: &str| {
        mailbox
            .masters_of(JANE)
            .into_iter()
            .find(|m| m["displayName"] == name)
            .map(|m| m["id"].as_str().unwrap().to_string())
    };
    let newsletters = id_of("Newsletters").unwrap();

    let out = categories(
        &h,
        crate::tools::categories::CategoriesArgs {
            action: Some(crate::tools::categories::CategoriesAction::Update),
            name: Some("newsletters".into()),
            new_name: Some("Nyhetsbrev".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(
        out.contains("The mailboxes are being updated in the background."),
        "{out}"
    );
    wait_for(
        &log,
        |(m, u, _)| m == "DELETE" && u.ends_with(&format!("/{newsletters}")),
        1,
    )
    .await;
    assert!(id_of("Nyhetsbrev").is_some());
    assert!(id_of("Newsletters").is_none());
    let retag = wait_for(
        &log,
        |(m, u, b)| m == "PATCH" && u == "/me/messages/news1" && b.get("categories").is_some(),
        1,
    )
    .await;
    assert_eq!(retag[0].2["categories"], json!(["Hobby", "Nyhetsbrev"]));
    // The lookup escaped the name into the filter.
    assert!(log.lock().unwrap().iter().any(|(_, u, _)| {
        u.starts_with("/me/messages?$filter=categories%2Fany%28c:c%20eq%20%27Newsletters%27%29")
    }));

    // Removing asks first whether to take it off the mail.
    let remove = |from_mail: Option<bool>| crate::tools::categories::CategoriesArgs {
        action: Some(crate::tools::categories::CategoriesAction::Remove),
        name: Some("Promotions".into()),
        remove_from_mail: from_mail,
        ..Default::default()
    };
    let out = categories(&h, remove(None)).await;
    assert!(
        out.starts_with("Nothing changed yet. Ask the user whether Promotions"),
        "{out}"
    );
    assert!(
        h.record()
            .await
            .review
            .categories()
            .iter()
            .any(|c| c.name == "Promotions")
    );
    let promotions = id_of("Promotions").unwrap();
    categories(&h, remove(Some(true))).await;
    wait_for(
        &log,
        |(m, u, _)| m == "DELETE" && u.ends_with(&format!("/{promotions}")),
        1,
    )
    .await;
}

#[tokio::test]
async fn prune_lists_first_then_leaves_only_pidges_categories_in_one_colour() {
    let h = ToolHarness::new(&[JANE, WORK]).await;
    grant_scope(&h, JANE).await;
    grant_scope(&h, WORK).await;
    let mut tagged = msg("t1", "club@example.com", "LAN", 30);
    tagged.categories = vec!["Hobby"];
    let mailbox = Mailbox::new(vec![tagged]);
    mailbox.set_masters(
        JANE,
        json!([
            master("red", "Red category", "preset0"),
            master("hobby", "Hobby", "preset9"),
        ]),
    );
    mount(&h, mailbox.clone()).await;
    categories(&h, sync_args(Some(true))).await;
    // The user recolours one of pidge's categories in Work only.
    {
        let mut all = mailbox.masters.lock().unwrap();
        let list = all.get_mut(&access_token(WORK)).unwrap();
        for m in list.iter_mut().filter(|m| m["displayName"] == "Important") {
            m["color"] = json!("preset24");
        }
    }
    let prune =
        |apply: Option<bool>, from_mail: Option<bool>| crate::tools::categories::CategoriesArgs {
            action: Some(crate::tools::categories::CategoriesAction::PruneMailboxes),
            apply,
            remove_from_mail: from_mail,
            ..Default::default()
        };

    let out = categories(&h, prune(None, None)).await;
    assert!(out.contains("would be removed"), "{out}");
    assert!(
        out.contains("Red category (1 messages)") || out.contains("Red category ("),
        "{out}"
    );
    assert!(out.contains("Hobby (1 messages)"), "{out}");
    assert!(out.contains("Nothing changed yet"), "{out}");
    assert!(
        mailbox.masters_of(JANE).iter().any(|m| m["id"] == "hobby"),
        "a preview deletes nothing"
    );

    let out = categories(&h, prune(Some(true), Some(true))).await;
    assert!(out.starts_with("Removed from the mailboxes:"), "{out}");
    let pidge = crate::review::categories::defaults();
    for mb in [JANE, WORK] {
        let names: Vec<String> = mailbox
            .masters_of(mb)
            .iter()
            .map(|m| m["displayName"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), pidge.len(), "{mb}: {names:?}");
        assert!(
            pidge.iter().all(|c| names.contains(&c.name)),
            "{mb}: {names:?}"
        );
    }
    // Same colour for each category in both mailboxes.
    for c in &pidge {
        let colour = |mb: &str| {
            mailbox
                .masters_of(mb)
                .iter()
                .find(|m| m["displayName"] == c.name.as_str())
                .map(|m| m["color"].clone())
        };
        assert_eq!(colour(JANE), colour(WORK), "{}", c.name);
    }
    // The name came off the mail that carried it.
    let log = mailbox.log.lock().unwrap();
    assert!(
        log.iter().any(|(m, u, b)| m == "PATCH"
            && u == "/me/messages/t1"
            && b["categories"]
                .as_array()
                .is_some_and(|c| !c.contains(&json!("Hobby")))),
        "{log:?}"
    );
}
