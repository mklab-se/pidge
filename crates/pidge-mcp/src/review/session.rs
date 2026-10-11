//! A review in progress: the ordered items, which are handled, the last
//! action (for undo), and a worker that applies actions in the background
//! in the order they were asked for. In memory only: a lost session is
//! rebuilt from the inbox, since handled mail is no longer unread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use pidge_core::FullMessage;
use tokio::sync::{mpsc, oneshot};

use super::categories::Category;
use super::classify::{Candidate, Judgment};
use super::graph::{self, Op};
use crate::state::SharedState;
use crate::users::user_hash;

/// Idle time after which a session is dropped.
pub const SESSION_TTL: Duration = Duration::from_secs(60 * 60);
/// Sessions kept per user; the least recently used goes first.
pub const MAX_SESSIONS: usize = 3;

/// What the user decided about a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Read,
    Archive,
    Delete,
    Flag,
    KeepUnread,
}

impl Act {
    pub fn past(self) -> &'static str {
        match self {
            Act::Read => "Marked read",
            Act::Archive => "Archived",
            Act::Delete => "Deleted (moved to Deleted Items)",
            Act::Flag => "Flagged",
            Act::KeepUnread => "Left unread",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Act::Read => "marking read",
            Act::Archive => "archiving",
            Act::Delete => "deleting",
            Act::Flag => "flagging",
            Act::KeepUnread => "skipping",
        }
    }

    /// The Graph steps, in order.
    pub fn ops(self) -> &'static [Op] {
        match self {
            Act::Read => &[Op::MarkRead],
            Act::Archive => &[Op::MarkRead, Op::Move("archive")],
            Act::Delete => &[Op::MarkRead, Op::Move("deleteditems")],
            Act::Flag => &[Op::Flag],
            Act::KeepUnread => &[],
        }
    }
}

pub struct Item {
    pub cand: Candidate,
    /// Other copies of the same message (a duplicate delivery, the same
    /// mail in another mailbox, the user's own forward of it): every action
    /// on the item applies to them too.
    pub copies: Vec<Candidate>,
    pub judgment: Judgment,
    pub handled: bool,
}

impl Item {
    /// The item's messages: `0` is `cand`, `k` is `copies[k - 1]`.
    fn message(&self, copy: usize) -> Option<&Candidate> {
        match copy {
            0 => Some(&self.cand),
            k => self.copies.get(k - 1),
        }
    }

    fn message_mut(&mut self, copy: usize) -> Option<&mut Candidate> {
        match copy {
            0 => Some(&mut self.cand),
            k => self.copies.get_mut(k - 1),
        }
    }
}

/// The session-scoped short id of item `index`: `r1`, `r2`, …
pub fn short_id(index: usize) -> String {
    format!("r{}", index + 1)
}

/// The item index a short id names, if `id` is one.
pub fn parse_short(id: &str) -> Option<usize> {
    let n: usize = id.trim().strip_prefix(['r', 'R'])?.parse().ok()?;
    n.checked_sub(1)
}

/// One message an action touched, with its id after a move.
#[derive(Debug, Clone)]
struct Touched {
    index: usize,
    copy: usize,
    account: String,
    moved_to: Option<String>,
}

struct UndoRecord {
    act: Act,
    touched: Vec<Touched>,
}

struct Inner {
    items: Vec<Item>,
    current: Option<usize>,
    last_used: Instant,
    undo: Option<UndoRecord>,
    failures: Vec<String>,
    applied: usize,
    tally: HashMap<&'static str, usize>,
    bodies: HashMap<String, FullMessage>,
}

enum Job {
    Apply { act: Act, indexes: Vec<usize> },
    Undo(oneshot::Sender<Result<Vec<usize>, String>>),
}

pub struct Session {
    pub id: String,
    pub user: String,
    pub created_at: DateTime<Utc>,
    pub categories: Arc<Vec<Category>>,
    /// Class-cache prefix, to pick up AI answers that arrived late.
    pub cache_prefix: Option<String>,
    inner: Mutex<Inner>,
    jobs: mpsc::UnboundedSender<Job>,
}

impl Session {
    pub fn new(
        state: &SharedState,
        user: &str,
        items: Vec<Item>,
        categories: Arc<Vec<Category>>,
        cache_prefix: Option<String>,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = Arc::new(Self {
            id: crate::oauth::jwt::random_id(),
            user: user.to_string(),
            created_at: Utc::now(),
            categories,
            cache_prefix,
            inner: Mutex::new(Inner {
                items,
                current: None,
                last_used: Instant::now(),
                undo: None,
                failures: Vec::new(),
                applied: 0,
                tally: HashMap::new(),
                bodies: HashMap::new(),
            }),
            jobs: tx,
        });
        tokio::spawn(worker(state.clone(), Arc::downgrade(&session), rx));
        session
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("review session lock")
    }

    /// The index of the item `id` names: a short id (`r3`), or the full id
    /// of any of the item's copies.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        let inner = self.lock();
        if let Some(i) = parse_short(id) {
            return (i < inner.items.len()).then_some(i);
        }
        inner
            .items
            .iter()
            .position(|i| i.cand.id == id || i.copies.iter().any(|c| c.id == id))
    }

    /// The mailbox and full id of item `index`'s first message.
    pub fn message_ref(&self, index: usize) -> Option<(String, String)> {
        self.lock()
            .items
            .get(index)
            .map(|i| (i.cand.account.clone(), i.cand.id.clone()))
    }

    /// Unhandled items, the current one included.
    pub fn remaining(&self) -> usize {
        self.lock().items.iter().filter(|i| !i.handled).count()
    }

    /// Every message of the session with the category its item is in, for
    /// tagging the mail in the mailboxes.
    pub fn tagging(&self) -> Vec<(Candidate, String)> {
        self.lock()
            .items
            .iter()
            .flat_map(|i| {
                std::iter::once(&i.cand)
                    .chain(&i.copies)
                    .map(|c| (c.clone(), i.judgment.category.clone()))
            })
            .collect()
    }

    pub fn current(&self) -> Option<usize> {
        self.lock().current
    }

    /// Unhandled items of the named categories (case-insensitive).
    pub fn unhandled_in(&self, categories: &[String]) -> Vec<usize> {
        self.lock()
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                !i.handled
                    && categories
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&i.judgment.category))
            })
            .map(|(n, _)| n)
            .collect()
    }

    /// Marks `indexes` handled now and queues `act` on them. Returns how
    /// many were not handled before.
    pub fn apply(&self, act: Act, indexes: Vec<usize>) -> usize {
        let fresh: Vec<usize> = {
            let mut inner = self.lock();
            inner.last_used = Instant::now();
            let fresh: Vec<usize> = indexes
                .into_iter()
                .filter(|&i| inner.items.get(i).is_some_and(|it| !it.handled))
                .collect();
            for &i in &fresh {
                inner.items[i].handled = true;
            }
            if !fresh.is_empty() {
                *inner.tally.entry(act.past()).or_default() += fresh.len();
            }
            fresh
        };
        if !fresh.is_empty() {
            let _ = self.jobs.send(Job::Apply {
                act,
                indexes: fresh.clone(),
            });
        }
        fresh.len()
    }

    /// Reverses the last action once every earlier action has run. Returns
    /// the restored items' indexes.
    pub async fn undo(&self) -> Result<Vec<usize>, String> {
        let (tx, rx) = oneshot::channel();
        self.jobs
            .send(Job::Undo(tx))
            .map_err(|_| "the review has ended".to_string())?;
        match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(r)) => r,
            _ => Err("undo is taking long and is still running in the background".into()),
        }
    }

    /// The next unhandled item, which becomes current; `None` when done.
    pub fn advance(&self) -> Option<usize> {
        let mut inner = self.lock();
        inner.last_used = Instant::now();
        let next = inner.items.iter().position(|i| !i.handled);
        inner.current = next;
        next
    }

    /// The item after the current one that would come next, for prefetching.
    pub fn upcoming(&self) -> Option<usize> {
        let inner = self.lock();
        inner
            .items
            .iter()
            .enumerate()
            .position(|(n, i)| !i.handled && Some(n) != inner.current)
    }

    /// Makes `index` current (after an undo brought it back).
    pub fn set_current(&self, index: usize) {
        self.lock().current = Some(index);
    }

    /// Background failures since the last call, for the next response.
    pub fn take_failures(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().failures)
    }

    /// The item's candidate and judgment, with a late AI answer applied.
    pub fn item(
        &self,
        index: usize,
        cache: &super::classify::ClassCache,
    ) -> Option<(Candidate, Judgment, usize)> {
        let mut inner = self.lock();
        let item = inner.items.get_mut(index)?;
        if item.judgment.source == super::classify::Source::Signal
            && let Some(prefix) = &self.cache_prefix
            && let Some(j) = cache.get(&super::classify::ClassCache::key(prefix, &item.cand.id))
        {
            item.judgment = j;
        }
        Some((item.cand.clone(), item.judgment.clone(), item.copies.len()))
    }

    /// Bodies fetched ahead of the session (while classifying).
    pub fn add_bodies(&self, bodies: impl IntoIterator<Item = FullMessage>) {
        let mut inner = self.lock();
        for m in bodies {
            inner.bodies.insert(m.id.clone(), m);
        }
    }

    /// `9 archived, 2 deleted, …` for the closing line.
    pub fn tally(&self) -> String {
        let inner = self.lock();
        let mut parts: Vec<(&str, usize)> = inner.tally.iter().map(|(k, v)| (*k, *v)).collect();
        parts.sort();
        parts
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// True the first time an action is applied: the moment the review
    /// counts as having happened.
    pub fn first_application(&self) -> bool {
        let mut inner = self.lock();
        inner.applied += 1;
        inner.applied == 1
    }

    pub fn take_body(&self, id: &str) -> Option<FullMessage> {
        self.lock().bodies.remove(id)
    }

    /// Fetches the body of item `index` in the background.
    pub fn prefetch(self: &Arc<Self>, state: &SharedState, index: usize) {
        let Some((account, id)) = self
            .lock()
            .items
            .get(index)
            .map(|i| (i.cand.account.clone(), i.cand.id.clone()))
        else {
            return;
        };
        if self.lock().bodies.contains_key(&id) {
            return;
        }
        let (state, session) = (state.clone(), Arc::downgrade(self));
        tokio::spawn(async move {
            if let Ok(m) = state.graph.get_message(&account, &id).await
                && let Some(s) = session.upgrade()
            {
                s.lock().bodies.insert(id, m);
            }
        });
    }

    fn expired(&self) -> bool {
        self.lock().last_used.elapsed() > SESSION_TTL
    }

    fn last_used(&self) -> Instant {
        self.lock().last_used
    }
}

/// Applies a session's jobs in order until the session is dropped.
async fn worker(state: SharedState, session: Weak<Session>, mut rx: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = rx.recv().await {
        let Some(s) = session.upgrade() else { return };
        match job {
            Job::Apply { act, indexes } => apply(&state, &s, act, indexes).await,
            Job::Undo(reply) => {
                let _ = reply.send(undo(&state, &s).await);
            }
        }
    }
}

/// One message an action is applied to: `(item index, copy, account, id)`.
type Target = (usize, usize, String, String);

async fn apply(state: &SharedState, s: &Session, act: Act, indexes: Vec<usize>) {
    // Every copy of each item: a merged duplicate is handled as one.
    let targets: Vec<Target> = {
        let inner = s.lock();
        indexes
            .iter()
            .filter_map(|&i| inner.items.get(i).map(|it| (i, it)))
            .flat_map(|(i, it)| {
                (0..=it.copies.len()).filter_map(move |k| {
                    it.message(k)
                        .map(|c| (i, k, c.account.clone(), c.id.clone()))
                })
            })
            .collect()
    };
    let mut touched: Vec<Touched> = targets
        .iter()
        .map(|(i, k, a, _)| Touched {
            index: *i,
            copy: *k,
            account: a.clone(),
            moved_to: None,
        })
        .collect();
    let mut failed: HashMap<usize, String> = HashMap::new();
    for op in act.ops() {
        let mut by_account: HashMap<&str, Vec<usize>> = HashMap::new();
        for (n, (_, _, account, _)) in targets.iter().enumerate() {
            if !failed.contains_key(&n) {
                by_account.entry(account.as_str()).or_default().push(n);
            }
        }
        for (account, ns) in by_account {
            let ids: Vec<String> = ns.iter().map(|&n| targets[n].3.clone()).collect();
            let results = graph::run(state, account, &ids, *op).await;
            for (n, r) in ns.into_iter().zip(results) {
                match r {
                    Ok(moved) => {
                        if moved.is_some() {
                            touched[n].moved_to = moved;
                        }
                    }
                    Err(reason) => {
                        failed.insert(n, reason);
                    }
                }
            }
        }
    }
    state.cache.invalidate_user(&s.user);
    let mut inner = s.lock();
    let mut reported = std::collections::HashSet::new();
    for (n, reason) in &failed {
        let index = &targets[*n].0;
        // One line per item, however many of its copies failed.
        if !reported.insert(*index) {
            continue;
        }
        inner.failures.push(format!(
            "earlier: {} message {} failed: {reason}",
            act.noun(),
            short_id(*index)
        ));
        // Not done after all: offer it again.
        if let Some(item) = inner.items.get_mut(*index) {
            item.handled = false;
        }
    }
    if !failed.is_empty() {
        tracing::warn!(user = %user_hash(&s.user), failed = failed.len(), "review: actions failed");
    }
    let done: Vec<Touched> = touched
        .into_iter()
        .enumerate()
        .filter(|(n, _)| !failed.contains_key(n))
        .map(|(_, t)| t)
        .collect();
    // Leaving a message unread changes nothing, so it leaves the undo
    // record alone; an action that failed everywhere clears it, so undo
    // never reaches past the failure to the action before.
    if act != Act::KeepUnread {
        inner.undo = (!done.is_empty()).then_some(UndoRecord { act, touched: done });
    }
}

async fn undo(state: &SharedState, s: &Session) -> Result<Vec<usize>, String> {
    let record = s.lock().undo.take().ok_or("there is nothing to undo")?;
    // Per account: the moved messages back to the inbox in one batch, then
    // everything marked unread (and unflagged) in another.
    let mut by_account: HashMap<String, Vec<&Touched>> = HashMap::new();
    for t in &record.touched {
        by_account.entry(t.account.clone()).or_default().push(t);
    }
    let mut restored = Vec::new();
    let mut errors = 0;
    for (account, touched) in by_account {
        // (index, id the message has now), for those that are back.
        let mut back: Vec<(usize, String)> = Vec::new();
        let moved: Vec<&&Touched> = touched.iter().filter(|t| t.moved_to.is_some()).collect();
        if !moved.is_empty() {
            let ids: Vec<String> = moved.iter().filter_map(|t| t.moved_to.clone()).collect();
            let results = graph::run(state, &account, &ids, Op::Move("inbox")).await;
            for ((t, old), r) in moved.iter().zip(&ids).zip(results) {
                match r {
                    Ok(new_id) => {
                        let new_id = new_id.unwrap_or_else(|| old.clone());
                        // Back in the inbox: record the id whatever happens next.
                        if let Some(item) = s.lock().items.get_mut(t.index) {
                            if let Some(m) = item.message_mut(t.copy) {
                                m.id = new_id.clone();
                            }
                            item.handled = false;
                        }
                        back.push((t.index, new_id));
                    }
                    Err(_) => errors += 1,
                }
            }
        }
        for t in touched.iter().filter(|t| t.moved_to.is_none()) {
            if let Some(m) = s.lock().items.get(t.index).and_then(|i| i.message(t.copy)) {
                back.push((t.index, m.id.clone()));
            }
        }
        let op = match record.act {
            Act::Flag => Some(Op::Unflag),
            Act::KeepUnread => None,
            _ => Some(Op::MarkUnread),
        };
        if let Some(op) = op
            && !back.is_empty()
        {
            let ids: Vec<String> = back.iter().map(|(_, id)| id.clone()).collect();
            let results = graph::run(state, &account, &ids, op).await;
            for ((index, _), r) in back.iter().zip(results) {
                if r.is_err() && record.act == Act::Read {
                    errors += 1;
                    continue;
                }
                restored.push(*index);
            }
        } else {
            restored.extend(back.iter().map(|(i, _)| *i));
        }
    }
    restored.sort_unstable();
    restored.dedup();
    {
        let mut inner = s.lock();
        for &i in &restored {
            if let Some(item) = inner.items.get_mut(i) {
                item.handled = false;
            }
        }
        if let Some(n) = inner.tally.get_mut(record.act.past()) {
            *n = n.saturating_sub(restored.len());
        }
    }
    state.cache.invalidate_user(&s.user);
    if restored.is_empty() && errors > 0 {
        return Err("undo failed at Microsoft; the message is in Archive or Deleted Items".into());
    }
    Ok(restored)
}

/// Every user's live sessions.
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Vec<Arc<Session>>>>,
}

impl Sessions {
    /// The caller's session `id`; another user's id reads as unknown.
    pub fn get(&self, user: &str, id: &str) -> Option<Arc<Session>> {
        let mut map = self.map.lock().expect("sessions lock");
        let list = map.get_mut(user)?;
        list.retain(|s| !s.expired());
        list.iter().find(|s| s.id == id).cloned()
    }

    /// The caller's most recently used session, for a short id given
    /// without its session (e.g. to `mail_categories`).
    pub fn latest(&self, user: &str) -> Option<Arc<Session>> {
        let mut map = self.map.lock().expect("sessions lock");
        let list = map.get_mut(user)?;
        list.retain(|s| !s.expired());
        list.iter().max_by_key(|s| s.last_used()).cloned()
    }

    pub fn insert(&self, session: Arc<Session>) {
        let mut map = self.map.lock().expect("sessions lock");
        map.retain(|_, list| {
            list.retain(|s| !s.expired());
            !list.is_empty()
        });
        let list = map.entry(session.user.clone()).or_default();
        while list.len() >= MAX_SESSIONS {
            let Some(oldest) = list
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.last_used())
                .map(|(i, _)| i)
            else {
                break;
            };
            list.remove(oldest);
        }
        list.push(session);
    }
}
