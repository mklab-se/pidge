//! Sorting review candidates into the user's categories: exact rules
//! first, then the user's AI (Jev or another Ailloy eval backend), then
//! Graph's own signals. The AI sees the message only as `state`; its answer
//! is a label from the user's own list, so a hostile e-mail can at most
//! mislabel itself.
//!
//! One AI request per message carries every question (TypeSafe reads the
//! state once and answers them in parallel); requests run concurrently.
//! Results land in a per-message cache, so a later review the same day
//! asks nothing again and a request that outlives the review's latency
//! budget still pays off.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ailloy::{ErrorKind, EvalResponse, Question, Questions, error_kind};
use chrono::{DateTime, Utc};
use lru::LruCache;
use serde_json::{Value, json};

use super::categories::{Category, OTHER, Signal, by_rule};

/// Concurrent AI requests per review.
const CONCURRENCY: usize = 16;
/// Per-request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Below this probability for the chosen category, the message goes to Other.
const MIN_CHOICE_PROBABILITY: f64 = 0.35;
/// How long a classification is reused.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_CAPACITY: usize = 20_000;

/// A review candidate: the fields classification and ordering use.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub account: String,
    pub id: String,
    pub from_name: String,
    pub from_address: String,
    /// One of the user's addresses is in To (not only Cc or Bcc).
    pub to_me: bool,
    pub subject: String,
    pub preview: String,
    pub has_attachments: bool,
    pub received_at: DateTime<Utc>,
    /// The mailbox's Focused (true) / Other (false) sorting, when known.
    pub focused: Option<bool>,
    /// Mailing-list headers (`List-Unsubscribe`, `List-Id`, `Precedence: bulk`).
    pub list: bool,
    /// Machine-sent (`noreply` sender, `Auto-Submitted`).
    pub automated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Rule,
    Ai,
    Signal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Judgment {
    pub category: String,
    /// Probability the message asks the user for a reply or decision.
    pub needs_reply: f64,
    /// 0 (ignorable) to 1 (personal and urgent).
    pub importance: f64,
    pub source: Source,
}

/// Per-message classifications, keyed by user, message, categories and model.
pub struct ClassCache {
    entries: Mutex<LruCache<String, (Instant, Judgment)>>,
}

impl Default for ClassCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(LruCache::new(
                NonZeroUsize::new(CACHE_CAPACITY).expect("nonzero"),
            )),
        }
    }
}

impl ClassCache {
    pub fn key(prefix: &str, id: &str) -> String {
        format!("{prefix}|{id}")
    }

    pub fn get(&self, key: &str) -> Option<Judgment> {
        let mut map = self.entries.lock().expect("class cache lock");
        match map.get(key) {
            Some((at, j)) if at.elapsed() < CACHE_TTL => Some(j.clone()),
            Some(_) => {
                map.pop(key);
                None
            }
            None => None,
        }
    }

    pub fn put(&self, key: String, judgment: Judgment) {
        self.entries
            .lock()
            .expect("class cache lock")
            .put(key, (Instant::now(), judgment));
    }
}

/// The cache-key prefix for one user's classifications under these
/// categories and this model.
pub fn cache_prefix(user: &str, fingerprint: &str, model_label: &str) -> String {
    format!(
        "{}|{fingerprint}|{model_label}",
        crate::users::user_hash_long(user)
    )
}

/// What the Graph signals alone say.
pub fn fallback(c: &Candidate, categories: &[Category]) -> Judgment {
    let with_signal = |s: Signal| categories.iter().find(|cat| cat.signal == Some(s));
    let category = if c.list {
        with_signal(Signal::Bulk)
    } else if c.automated {
        with_signal(Signal::Automated)
    } else {
        None
    }
    .map_or_else(|| OTHER.to_string(), |cat| cat.name.clone());
    let importance = match (c.focused, c.to_me) {
        _ if c.list || c.automated => 0.1,
        (Some(true), true) => 0.6,
        (Some(false), _) => 0.2,
        (_, true) => 0.5,
        _ => 0.3,
    };
    Judgment {
        category,
        needs_reply: 0.0,
        importance,
        source: Source::Signal,
    }
}

/// The rule-assigned judgment, when one of the user's rules matches.
pub fn by_rules(c: &Candidate, categories: &[Category]) -> Option<Judgment> {
    by_rule(categories, &c.from_address).map(|cat| Judgment {
        category: cat.name.clone(),
        needs_reply: 0.0,
        importance: 0.5,
        source: Source::Rule,
    })
}

/// The questions asked about every message.
pub fn questions(categories: &[Category]) -> Questions {
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

    let needs_reply = Question::yes_no(
        "Does this e-mail (`email`) ask the recipient personally for a reply, a decision or an action?",
    )
    .yes("A person is waiting for the recipient to answer or act")
    .no("Nothing is asked of the recipient personally: information, marketing or an automated notice");

    let importance =
        Question::score("How much does this e-mail (`email`) matter to the recipient?").levels([
            "Automated or bulk mail that is safe to ignore",
            "Of passing interest: worth a glance, no consequences if skipped",
            "Relevant to the recipient: they would want to know about it today",
            "Personal and time-sensitive: someone the recipient knows needs them soon",
        ]);

    let mut q = Questions::new();
    q.insert("category".into(), choice);
    q.insert("needs_reply".into(), needs_reply);
    q.insert("importance".into(), importance);
    q
}

/// The message as the classifier sees it.
pub fn state(c: &Candidate) -> Value {
    json!({
        "email": {
            "from": { "name": c.from_name, "address": c.from_address },
            "sent_directly_to_recipient": c.to_me,
            "subject": c.subject,
            "preview": c.preview,
            "has_attachments": c.has_attachments,
            "mailing_list": c.list,
            "automated_sender": c.automated,
        }
    })
}

/// Reads the answers; an unknown or uncertain category becomes Other.
pub fn judge(resp: &EvalResponse, categories: &[Category]) -> Judgment {
    let category = match resp.answers.get("category") {
        Some(ailloy::Answer::Choice {
            choice,
            probabilities,
            ..
        }) => {
            let p = probabilities.get(choice).copied().unwrap_or(0.0);
            categories
                .iter()
                .find(|c| &c.name == choice)
                .filter(|_| p >= MIN_CHOICE_PROBABILITY)
                .map_or_else(|| OTHER.to_string(), |c| c.name.clone())
        }
        _ => OTHER.to_string(),
    };
    let needs_reply = resp
        .answers
        .get("needs_reply")
        .and_then(|a| a.as_yes_no())
        .unwrap_or(0.0);
    let importance = resp
        .answers
        .get("importance")
        .and_then(|a| a.normalized_score())
        .unwrap_or(0.5);
    Judgment {
        category,
        needs_reply,
        importance,
        source: Source::Ai,
    }
}

/// The result of classifying a review's candidates.
pub struct Outcome {
    pub judgments: HashMap<String, Judgment>,
    /// AI requests that succeeded in this round (cache hits not counted).
    pub ai_ok: usize,
    /// The first AI failure, if any request failed.
    pub ai_error: Option<ErrorKind>,
    /// Messages whose AI answer had not arrived within the budget.
    pub pending: usize,
}

/// Classifies `candidates`: rules, cache, then the AI within `budget`, with
/// the Graph signals for anything still unanswered. Requests still running
/// when the budget ends keep going and fill the cache.
pub async fn classify(
    cache: Arc<ClassCache>,
    client: Option<Arc<ailloy::Client>>,
    prefix: &str,
    candidates: &[Candidate],
    categories: Arc<Vec<Category>>,
    budget: Duration,
) -> Outcome {
    let mut judgments = HashMap::new();
    let mut ask = Vec::new();
    for c in candidates {
        if let Some(j) = by_rules(c, &categories) {
            judgments.insert(c.id.clone(), j);
        } else if let Some(j) = client
            .as_ref()
            .and_then(|_| cache.get(&ClassCache::key(prefix, &c.id)))
        {
            judgments.insert(c.id.clone(), j);
        } else {
            ask.push(c.clone());
        }
    }

    let mut ai_ok = 0;
    let mut ai_error = None;
    let mut pending = 0;
    if let Some(client) = client
        && !ask.is_empty()
    {
        let questions = Arc::new(questions(&categories));
        let gate = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
        // Set on a failure the user must fix: no point sending the rest.
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for c in &ask {
            let (client, questions, gate, stop, tx, cache, categories) = (
                client.clone(),
                questions.clone(),
                gate.clone(),
                stop.clone(),
                tx.clone(),
                cache.clone(),
                categories.clone(),
            );
            let key = ClassCache::key(prefix, &c.id);
            let state = state(c);
            tokio::spawn(async move {
                let _permit = gate.acquire().await;
                if stop.load(Ordering::Relaxed) {
                    let _ = tx.send(Err(None));
                    return;
                }
                let result =
                    tokio::time::timeout(REQUEST_TIMEOUT, client.eval(state, &questions)).await;
                let sent = match result {
                    Ok(Ok(resp)) => {
                        cache.put(key, judge(&resp, &categories));
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        let kind = error_kind(&e);
                        if super::ai::AiStatus::from_kind(kind).needs_user() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        Err(Some(kind))
                    }
                    Err(_) => Err(Some(ErrorKind::Network)),
                };
                let _ = tx.send(sent);
            });
        }
        drop(tx);
        let deadline = tokio::time::Instant::now() + budget;
        let mut done = 0;
        while done < ask.len() {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(r)) => {
                    done += 1;
                    match r {
                        Ok(()) => ai_ok += 1,
                        Err(Some(kind)) => {
                            ai_error.get_or_insert(kind);
                        }
                        Err(None) => {}
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        pending = ask.len() - done;
    }

    for c in &ask {
        let j = cache
            .get(&ClassCache::key(prefix, &c.id))
            .unwrap_or_else(|| fallback(c, &categories));
        judgments.insert(c.id.clone(), j);
    }
    Outcome {
        judgments,
        ai_ok,
        ai_error,
        pending,
    }
}

/// Whether a sender address is a machine's.
pub fn is_noreply(address: &str) -> bool {
    let local = address
        .split('@')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(['-', '_', '.'], "");
    [
        "noreply",
        "donotreply",
        "notifications",
        "notification",
        "mailerdaemon",
        "postmaster",
    ]
    .iter()
    .any(|p| local == *p || local.starts_with("noreply") || local.starts_with("donotreply"))
}

/// `(list, automated)` from a message's internet headers.
pub fn header_signals(headers: &[(String, String)]) -> (bool, bool) {
    let mut list = false;
    let mut automated = false;
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        match name.as_str() {
            "list-unsubscribe" | "list-id" => list = true,
            "precedence" if matches!(value.as_str(), "bulk" | "list" | "junk") => list = true,
            "auto-submitted" if value != "no" => automated = true,
            _ => {}
        }
    }
    (list, automated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::categories::defaults;

    fn cand(address: &str) -> Candidate {
        Candidate {
            account: "jane@example.com".into(),
            id: "m1".into(),
            from_name: "Sender".into(),
            from_address: address.into(),
            to_me: true,
            subject: "Hello".into(),
            preview: "Hi".into(),
            has_attachments: false,
            received_at: Utc::now(),
            focused: None,
            list: false,
            automated: false,
        }
    }

    #[test]
    fn fallback_uses_the_signal_categories() {
        let cats = defaults();
        let mut c = cand("news@shop.example");
        c.list = true;
        assert_eq!(fallback(&c, &cats).category, "Newsletters");
        let mut c = cand("noreply@bank.example");
        c.automated = true;
        assert_eq!(fallback(&c, &cats).category, "Notifications");
        assert_eq!(fallback(&cand("anna@example.com"), &cats).category, OTHER);
    }

    #[test]
    fn rules_win_and_name_the_category() {
        let mut cats = defaults();
        cats[1].rules.push("sender:anna@example.com".into());
        let j = by_rules(&cand("Anna@example.com"), &cats).unwrap();
        assert_eq!((j.category.as_str(), j.source), ("Personal", Source::Rule));
    }

    #[test]
    fn questions_offer_every_category_plus_other() {
        let q = questions(&defaults());
        match &q["category"] {
            Question::Choice { options, .. } => {
                assert_eq!(options.len(), 7);
                assert!(options.contains_key(OTHER));
                assert!(options.contains_key("Bills and receipts"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn noreply_and_headers_are_recognised() {
        assert!(is_noreply("no-reply@x.example"));
        assert!(is_noreply("DoNotReply@x.example"));
        assert!(is_noreply("noreply.orders@x.example"));
        assert!(!is_noreply("anna@x.example"));
        let h = |n: &str, v: &str| vec![(n.to_string(), v.to_string())];
        assert_eq!(
            header_signals(&h("List-Unsubscribe", "<https://x>")),
            (true, false)
        );
        assert_eq!(header_signals(&h("Precedence", "bulk")), (true, false));
        assert_eq!(
            header_signals(&h("Auto-Submitted", "auto-generated")),
            (false, true)
        );
        assert_eq!(header_signals(&h("Auto-Submitted", "no")), (false, false));
    }
}
