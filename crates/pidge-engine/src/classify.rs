//! pidge's judgement of each message: kind, category and importance, with
//! the source of each. Order: the user's rules, deterministic signals, the
//! user's own AI key (optional), and finally the harness (which records its
//! verdicts through `mail_setup` / `mail_act categorize`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use ailloy::{EvalResponse, Question, Questions};
use pidge_core::provider::{BulkHint, Folder, MailItem, ProviderKind};
use pidge_store::{Classification, Filter, Stored};
use serde_json::{Value, json};

use crate::ctx::User;
use crate::kinds::{Kind, signal_kind};
use crate::prefs::{Category, OTHER, Prefs, matches_any};

/// Facts about the user that classification needs.
pub struct Known {
    pub own: Vec<String>,
    /// Threads the user has written in.
    pub threads_with_sent: HashSet<String>,
    /// Addresses the user has written to (lowercase).
    pub written_to: HashSet<String>,
}

impl Known {
    pub fn load(user: &User) -> Self {
        let own: Vec<String> = user
            .addresses()
            .into_iter()
            .map(|a| a.to_ascii_lowercase())
            .collect();
        let threads_with_sent = user.store.threads_with_sent().unwrap_or_default();
        let written_to = user
            .store
            .people("", &own, 5000)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.sent_to > 0)
            .map(|p| p.address)
            .collect();
        Self {
            own,
            threads_with_sent,
            written_to,
        }
    }
}

const DEADLINE_WORDS: &[&str] = &[
    "deadline",
    "senast",
    "före ",
    "by friday",
    "by monday",
    "by tomorrow",
    "imorgon",
    "i morgon",
    "asap",
    "brådskande",
    "urgent",
    "akut",
    "due ",
    "förfaller",
];

fn to_me(m: &MailItem, own: &[String]) -> bool {
    m.to.iter()
        .any(|r| own.contains(&r.address.to_ascii_lowercase()))
}

fn rule_category(categories: &[Category], address: &str) -> Option<String> {
    categories
        .iter()
        .find(|c| matches_any(&c.rules, address))
        .map(|c| c.name.clone())
}

fn category_for_kind(categories: &[Category], kind: Kind) -> Option<String> {
    categories
        .iter()
        .find(|c| c.kinds.contains(&kind))
        .map(|c| c.name.clone())
}

/// The deterministic part: rules, signals, importance with reasons.
pub fn judge_without_ai(m: &MailItem, prefs: &Prefs, known: &Known) -> Classification {
    let categories = prefs.categories();
    let from = m.from.address.to_ascii_lowercase();
    let from_self = known.own.contains(&from);
    let knows_sender = known.written_to.contains(&from);
    let mut c = Classification::default();

    if let Some(cat) = rule_category(&categories, &from) {
        c.category = Some(cat.clone());
        c.category_source = Some("rule".into());
        if let Some(k) = categories
            .iter()
            .find(|x| x.name == cat)
            .and_then(|x| x.kinds.first())
        {
            c.kind = Some(k.as_str().into());
            c.kind_source = Some("rule".into());
        }
    }
    if c.kind.is_none()
        && let Some(k) = signal_kind(m, knows_sender)
    {
        c.kind = Some(k.as_str().into());
        c.kind_source = Some("signal".into());
    }
    let kind = c.kind.as_deref().and_then(Kind::parse);
    if c.category.is_none()
        && let Some(cat) = kind.and_then(|k| category_for_kind(&categories, k))
    {
        c.category = Some(cat);
        c.category_source = Some("signal".into());
    }

    // Importance, with reasons a person would accept.
    let mut reasons: Vec<String> = Vec::new();
    let bulk = kind.is_some_and(Kind::is_bulk);
    // A machine replying in a thread, or a receipt ending in "?", asks nothing.
    let conversational =
        !bulk && !crate::kinds::automated_sender(&from) && kind != Some(Kind::Receipt);
    if prefs.is_vip(&from) {
        reasons.push("vip".into());
    }
    if !from_self && known.threads_with_sent.contains(&m.thread_id) && conversational {
        reasons.push("reply-to-you".into());
    }
    if m.is_invite {
        reasons.push("invite".into());
    }
    let direct = to_me(m, &known.own);
    if direct && knows_sender && !bulk {
        reasons.push("from-someone-you-write-to".into());
    }
    let text = format!("{} {}", m.subject, m.preview).to_lowercase();
    if direct && conversational {
        if DEADLINE_WORDS.iter().any(|w| text.contains(w)) {
            reasons.push("deadline".into());
        }
        if m.preview.contains('?') && (knows_sender || m.bulk_hint != Some(BulkHint::Other)) {
            reasons.push("question".into());
        }
    }
    if m.flagged {
        reasons.push("flagged".into());
    }
    c.importance = if prefs.is_muted(&from) {
        reasons.clear();
        0
    } else if !reasons.is_empty() {
        2
    } else if bulk {
        0
    } else {
        1
    };
    c.reasons = reasons;
    if c.importance == 2
        && c.category_source.as_deref() != Some("rule")
        && kind != Some(Kind::Receipt)
        && categories.iter().any(|x| x.name == "Important")
    {
        c.category = Some("Important".into());
        c.category_source = Some("signal".into());
    }
    c
}

// ---- the user's own AI ------------------------------------------------------

const AI_CONCURRENCY: usize = 4;
/// AI requests per classification round, at most.
const AI_MAX_PER_RUN: usize = 40;
/// Senders asked about `List-Unsubscribe` per round, at most.
const MAX_SENDER_LOOKUPS: usize = 200;
/// A voice harness cannot wait: on a server, what does not fit is picked up
/// on the next call (answers are remembered, so each round gets further).
const SENDER_LOOKUP_BUDGET: Duration = Duration::from_secs(25);
const SENDER_LOOKUP_BUDGET_MCP: Duration = Duration::from_secs(3);
const AI_TIMEOUT: Duration = Duration::from_secs(20);
/// A category or kind the model is less sure of than this is left open.
const MIN_PROBABILITY: f64 = 0.6;

fn questions(categories: &[Category]) -> Questions {
    let mut choice = Question::choice(
        "Which of the user's categories does this e-mail (`email`) belong to? Use the \
         descriptions and the examples of each category.",
    );
    for c in categories {
        let mut desc = json!({ "description": c.description });
        if !c.examples.is_empty() {
            desc["examples"] = json!(
                c.examples
                    .iter()
                    .map(|e| json!({ "from": e.from, "subject": e.subject }))
                    .collect::<Vec<_>>()
            );
        }
        choice = choice.option(c.name.clone(), desc);
    }
    choice = choice.option(OTHER, "None of the other categories fits.");

    let mut kind = Question::choice("What kind of e-mail is this (`email`)?");
    for k in Kind::ALL {
        let d = match k {
            Kind::Receipt => "A receipt, invoice, bill, or order or payment confirmation",
            Kind::Newsletter => "Editorial content the recipient subscribed to",
            Kind::Promotion => "Marketing, an offer, a sale or an advertisement",
            Kind::Notification => "An automated notice from a service or system",
            Kind::Social => "A notice from a social network",
            Kind::Invite => "A meeting invitation",
            Kind::Personal => "Written by a person to the recipient",
        };
        kind = kind.option(k.as_str(), d);
    }

    let needs_reply = Question::yes_no(
        "Does this e-mail (`email`) ask the recipient personally for a reply, a decision or an action?",
    )
    .yes("A person is waiting for the recipient to answer or act")
    .no("Nothing is asked of the recipient personally");

    let importance =
        Question::score("How much does this e-mail (`email`) matter to the recipient?").levels([
            "Automated or bulk mail that is safe to ignore",
            "Of passing interest: worth a glance, no consequences if skipped",
            "Relevant to the recipient: they would want to know about it today",
            "Personal and time-sensitive: someone the recipient knows needs them soon",
        ]);

    let mut q = Questions::new();
    q.insert("category".into(), choice);
    q.insert("kind".into(), kind);
    q.insert("needs_reply".into(), needs_reply);
    q.insert("importance".into(), importance);
    q
}

fn state(m: &MailItem, own: &[String]) -> Value {
    json!({
        "email": {
            "from": { "name": m.from.name, "address": m.from.address },
            "sent_directly_to_recipient": to_me(m, own),
            "subject": m.subject,
            "preview": m.preview.chars().take(400).collect::<String>(),
            "has_attachments": m.has_attachments,
            "mailing_list": m.list_unsubscribe,
        }
    })
}

fn choice(resp: &EvalResponse, key: &str) -> Option<String> {
    match resp.answers.get(key) {
        Some(ailloy::Answer::Choice {
            choice,
            probabilities,
            ..
        }) if probabilities.get(choice).copied().unwrap_or(0.0) >= MIN_PROBABILITY => {
            Some(choice.clone())
        }
        _ => None,
    }
}

/// Merge the AI's answers into a deterministic judgement: it fills what
/// rules and signals left open, and can raise or lower importance.
fn merge_ai(mut c: Classification, resp: &EvalResponse, categories: &[Category]) -> Classification {
    if c.kind.is_none()
        && let Some(k) = choice(resp, "kind").and_then(|k| Kind::parse(&k))
    {
        c.kind = Some(k.as_str().into());
        c.kind_source = Some("ai".into());
    }
    if c.category_source.as_deref() != Some("rule")
        && let Some(cat) = choice(resp, "category")
            .filter(|n| n != OTHER && categories.iter().any(|x| &x.name == n))
    {
        c.category = Some(cat);
        c.category_source = Some("ai".into());
    }
    let needs_reply = resp
        .answers
        .get("needs_reply")
        .and_then(|a| a.as_yes_no())
        .unwrap_or(0.0);
    let score = resp
        .answers
        .get("importance")
        .and_then(|a| a.normalized_score())
        .unwrap_or(0.5);
    // Bulk is bulk: a model reading "book now" as a request changes nothing.
    let bulk_kind = c
        .kind
        .as_deref()
        .and_then(Kind::parse)
        .is_some_and(Kind::is_bulk);
    let bulk_category = c
        .category
        .as_ref()
        .and_then(|n| categories.iter().find(|x| &x.name == n))
        .is_some_and(|x| x.sweep);
    if bulk_kind || bulk_category {
        if c.reasons.is_empty() {
            c.importance = 0;
        }
        return c;
    }
    if needs_reply >= 0.7 && !c.reasons.iter().any(|r| r == "needs-reply") {
        c.reasons.push("needs-reply".into());
    }
    if c.importance > 0 {
        if score >= 0.75 || needs_reply >= 0.7 {
            c.importance = 2;
        } else if score <= 0.2 && c.reasons.is_empty() {
            c.importance = 0;
        }
    }
    c
}

/// Providers whose lists do not carry `List-Unsubscribe` (Graph) are asked
/// once per sender, from one message, and the answer is remembered: it tells
/// mailing lists from people better than anything else in a list row.
async fn learn_list_senders(user: &Arc<User>, pending: &[Stored], known: &Known) {
    let mut seen = HashSet::new();
    let mut ask = Vec::new();
    for s in pending {
        let from = s.item.from.address.to_ascii_lowercase();
        if from.is_empty() || s.item.folder != Folder::Inbox || known.own.contains(&from) {
            continue;
        }
        let Ok(p) = user.provider(&s.item.account) else {
            continue;
        };
        if p.kind() != ProviderKind::Microsoft || ask.len() >= MAX_SENDER_LOOKUPS {
            continue;
        }
        // Someone the user writes to can also send newsletters from the same
        // address: judge each message outside the shared conversations.
        if known.written_to.contains(&from) {
            if !known.threads_with_sent.contains(&s.item.thread_id) {
                ask.push((p, s.item.account.clone(), s.item.id.clone(), None));
            }
            continue;
        }
        if !seen.insert(from.clone()) {
            continue;
        }
        match user.store.get_doc::<bool>("listsender", &from) {
            Ok(Some(true)) => {
                let _ = user.store.mark_list_sender(&from);
            }
            Ok(Some(false)) => {}
            _ => ask.push((p, s.item.account.clone(), s.item.id.clone(), Some(from))),
        }
    }
    let gate = Arc::new(tokio::sync::Semaphore::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for (p, account, id, sender) in ask {
        let gate = gate.clone();
        tasks.spawn(async move {
            let _permit = gate.acquire_owned().await;
            let headers = p.unsubscribe_headers(&id).await;
            (account, id, sender, headers)
        });
    }
    let until = chrono::Utc::now() + chrono::Duration::days(60);
    let collect = async {
        while let Some(Ok((account, id, sender, headers))) = tasks.join_next().await {
            let Ok(h) = headers else { continue };
            let list = h.list_unsubscribe.is_some();
            match sender {
                Some(from) => {
                    let _ = user.store.put_doc("listsender", &from, &list, Some(until));
                    if list {
                        let _ = user.store.mark_list_sender(&from);
                    }
                }
                None if list => {
                    let _ = user.store.mark_list_message(&account, &id);
                }
                None => {}
            }
        }
    };
    let budget = match user.frontend() {
        crate::ctx::Frontend::Cli => SENDER_LOOKUP_BUDGET,
        crate::ctx::Frontend::Mcp => SENDER_LOOKUP_BUDGET_MCP,
    };
    let _ = tokio::time::timeout(budget, collect).await;
}

/// Classify every unclassified message in the index: rules and signals at
/// once, the AI (when configured) for what they left open, within `budget`.
/// Requests still running when the budget ends keep going.
pub async fn classify_pending(user: &Arc<User>, budget: Duration) {
    let prefs = user.prefs().await;
    let known = Known::load(user);
    let pending = match user.store.query(&Filter {
        unclassified: true,
        ..Default::default()
    }) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "classify: index query failed");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    learn_list_senders(user, &pending, &known).await;
    // Read again: what was just learned is on the rows now.
    let pending = user
        .store
        .query(&Filter {
            unclassified: true,
            ..Default::default()
        })
        .unwrap_or(pending);
    let ai = user.ai().filter(|_| {
        !matches!(
            prefs.ai.status.as_str(),
            "no_credits" | "key_rejected" | "failing"
        )
    });
    let mut ask: Vec<(Stored, Classification)> = Vec::new();
    for s in pending {
        let c = judge_without_ai(&s.item, &prefs, &known);
        let open = c.kind.is_none() || c.category.is_none();
        let worth_asking = s.item.folder == Folder::Inbox
            && (chrono::Utc::now() - s.item.received_at).num_days() <= 14;
        // Newest first, and bounded: the AI is an accelerator, not a cost.
        if ai.is_some() && open && worth_asking && ask.len() < AI_MAX_PER_RUN {
            ask.push((s, c));
        } else {
            let _ = user
                .store
                .set_classification(&s.item.account, &s.item.id, &c);
        }
    }
    let Some(client) = ai else { return };
    if ask.is_empty() {
        return;
    }
    let categories = Arc::new(prefs.categories());
    let questions = Arc::new(questions(&categories));
    let gate = Arc::new(tokio::sync::Semaphore::new(AI_CONCURRENCY));
    let mut tasks = Vec::new();
    let status: Arc<std::sync::Mutex<Option<ailloy::ErrorKind>>> = Arc::default();
    for (s, c) in ask {
        let (client, questions, gate, user, categories, own, status) = (
            client.clone(),
            questions.clone(),
            gate.clone(),
            user.clone(),
            categories.clone(),
            known.own.clone(),
            status.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let _permit = gate.acquire().await;
            let st = state(&s.item, &own);
            let result = tokio::time::timeout(AI_TIMEOUT, client.eval(st, &questions)).await;
            let c = match result {
                Ok(Ok(resp)) => merge_ai(c, &resp, &categories),
                Ok(Err(e)) => {
                    *status.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some(ailloy::error_kind(&e));
                    c
                }
                Err(_) => c,
            };
            let _ = user
                .store
                .set_classification(&s.item.account, &s.item.id, &c);
        }));
    }
    let all = futures::future::join_all(tasks);
    let _ = tokio::time::timeout(budget, all).await;
    let failure = *status.lock().unwrap_or_else(|p| p.into_inner());
    let new_status = match failure {
        None => "ok",
        Some(ailloy::ErrorKind::QuotaExhausted) => "no_credits",
        Some(ailloy::ErrorKind::Auth) => "key_rejected",
        Some(
            ailloy::ErrorKind::RateLimited
            | ailloy::ErrorKind::Unavailable
            | ailloy::ErrorKind::Network,
        ) => "unavailable",
        Some(_) => "failing",
    };
    if prefs.ai.status != new_status {
        let _ = user
            .update_prefs(|p| {
                p.ai.status = new_status.into();
                p.ai.since = Some(chrono::Utc::now());
            })
            .await;
    }
}

/// Record a verdict from the harness or the user for one message.
pub fn record_verdict(
    user: &User,
    s: &Stored,
    kind: Option<Kind>,
    category: Option<&str>,
) -> pidge_store::StoreResult<()> {
    let mut c = s.class.clone();
    if let Some(k) = kind {
        c.kind = Some(k.as_str().into());
        c.kind_source = Some("harness".into());
        if k.is_bulk() {
            c.importance = 0;
        }
    }
    if let Some(cat) = category {
        c.category = Some(cat.to_string());
        c.category_source = Some("harness".into());
    }
    user.store
        .set_classification(&s.item.account, &s.item.id, &c)
}

/// Group counts of a set, for maps and plans.
pub fn count_by<'a>(
    items: impl Iterator<Item = &'a Stored>,
    key: impl Fn(&Stored) -> Option<String>,
) -> Vec<(String, usize)> {
    let mut map: HashMap<String, usize> = HashMap::new();
    for s in items {
        if let Some(k) = key(s) {
            *map.entry(k).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pidge_core::MessageFrom;
    use pidge_core::provider::Folder;

    fn m(from: &str, to_me: bool, subject: &str, preview: &str) -> MailItem {
        MailItem {
            account: "me@x.se".into(),
            id: "1".into(),
            thread_id: "t1".into(),
            folder: Folder::Inbox,
            from: MessageFrom {
                name: "N".into(),
                address: from.into(),
            },
            to: if to_me {
                vec![MessageFrom {
                    name: String::new(),
                    address: "me@x.se".into(),
                }]
            } else {
                vec![]
            },
            cc: vec![],
            subject: subject.into(),
            received_at: Utc::now(),
            preview: preview.into(),
            unread: true,
            flagged: false,
            has_attachments: false,
            categories: vec![],
            bulk_hint: Some(BulkHint::Focused),
            list_unsubscribe: false,
            is_invite: false,
        }
    }

    fn known(written_to: &[&str], threads: &[&str]) -> Known {
        Known {
            own: vec!["me@x.se".into()],
            threads_with_sent: threads.iter().map(|s| s.to_string()).collect(),
            written_to: written_to.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_question_from_someone_you_write_to_is_important() {
        let c = judge_without_ai(
            &m(
                "anna@x.se",
                true,
                "Avtal",
                "Kan du skriva under före fredag?",
            ),
            &Prefs::default(),
            &known(&["anna@x.se"], &[]),
        );
        assert_eq!(c.importance, 2);
        assert_eq!(c.kind.as_deref(), Some("personal"));
        assert_eq!(c.category.as_deref(), Some("Important"));
        assert!(c.reasons.contains(&"question".to_string()));
    }

    #[test]
    fn rules_beat_signals_and_promotions_are_low() {
        let mut prefs = Prefs::default();
        let mut cats = crate::prefs::default_categories();
        cats.push(Category {
            rules: vec!["domain:klatterklubben.se".into()],
            ..Category::new("Leisure", "Climbing", &[], false)
        });
        prefs.categories = Some(cats);
        let c = judge_without_ai(
            &m("info@klatterklubben.se", false, "Kurs i helgen", ""),
            &prefs,
            &known(&[], &[]),
        );
        assert_eq!(c.category.as_deref(), Some("Leisure"));
        assert_eq!(c.category_source.as_deref(), Some("rule"));

        let mut promo = m("news@shop.se", false, "REA på allt", "");
        promo.list_unsubscribe = true;
        promo.bulk_hint = Some(BulkHint::Other);
        let c = judge_without_ai(&promo, &Prefs::default(), &known(&[], &[]));
        assert_eq!(c.kind.as_deref(), Some("promotion"));
        assert_eq!(c.category.as_deref(), Some("Promotions"));
        assert_eq!(c.importance, 0);
    }

    #[test]
    fn replies_in_your_threads_and_vips_matter() {
        let c = judge_without_ai(
            &m("bob@y.se", false, "Re: offert", "Låter bra"),
            &Prefs::default(),
            &known(&[], &["t1"]),
        );
        assert!(c.reasons.contains(&"reply-to-you".to_string()));
        let prefs = Prefs {
            vips: vec!["domain:y.se".into()],
            ..Default::default()
        };
        let c = judge_without_ai(&m("bob@y.se", false, "Hej", ""), &prefs, &known(&[], &[]));
        assert_eq!(c.importance, 2);
    }

    #[test]
    fn machines_and_receipts_ask_nothing() {
        let c = judge_without_ai(
            &m(
                "noreply@github.com",
                true,
                "[GitHub] Payment Receipt",
                "Questions?",
            ),
            &Prefs::default(),
            &known(&[], &["t1"]),
        );
        assert_eq!(c.kind.as_deref(), Some("receipt"));
        assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    }

    #[test]
    fn muted_senders_never_matter() {
        let prefs = Prefs {
            muted: vec!["bob@y.se".into()],
            vips: vec!["bob@y.se".into()],
            ..Default::default()
        };
        let c = judge_without_ai(&m("bob@y.se", true, "Hej?", "?"), &prefs, &known(&[], &[]));
        assert_eq!(c.importance, 0);
    }
}
