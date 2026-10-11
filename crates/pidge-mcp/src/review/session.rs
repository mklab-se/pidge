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
    pub judgment: Judgment,
    pub handled: bool,
}

/// One message an action touched, with its id after a move.
#[derive(Debug, Clone)]
struct Touched {
    index: usize,
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

    /// The index of the message `id`, if this session holds it.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.lock().items.iter().position(|i| i.cand.id == id)
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
            _ => Err("undo took too long; check the Archive or Deleted Items folder".into()),
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
    ) -> Option<(Candidate, Judgment)> {
        let mut inner = self.lock();
        let item = inner.items.get_mut(index)?;
        if item.judgment.source == super::classify::Source::Signal
            && let Some(prefix) = &self.cache_prefix
            && let Some(j) = cache.get(&super::classify::ClassCache::key(prefix, &item.cand.id))
        {
            item.judgment = j;
        }
        Some((item.cand.clone(), item.judgment.clone()))
    }

    pub fn position_counts(&self) -> (usize, usize) {
        let inner = self.lock();
        let handled = inner.items.iter().filter(|i| i.handled).count();
        (handled, inner.items.len())
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

async fn apply(state: &SharedState, s: &Session, act: Act, indexes: Vec<usize>) {
    let targets: Vec<(usize, String, String)> = {
        let inner = s.lock();
        indexes
            .iter()
            .filter_map(|&i| {
                inner
                    .items
                    .get(i)
                    .map(|it| (i, it.cand.account.clone(), it.cand.id.clone()))
            })
            .collect()
    };
    let mut touched: Vec<Touched> = targets
        .iter()
        .map(|(i, a, _)| Touched {
            index: *i,
            account: a.clone(),
            moved_to: None,
        })
        .collect();
    let mut failed: HashMap<usize, String> = HashMap::new();
    for op in act.ops() {
        let mut by_account: HashMap<&str, Vec<usize>> = HashMap::new();
        for (n, (_, account, _)) in targets.iter().enumerate() {
            if !failed.contains_key(&n) {
                by_account.entry(account.as_str()).or_default().push(n);
            }
        }
        for (account, ns) in by_account {
            let ids: Vec<String> = ns.iter().map(|&n| targets[n].2.clone()).collect();
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
    for (n, reason) in &failed {
        let (index, _, id) = &targets[*n];
        inner.failures.push(format!(
            "earlier: {} message {id} failed: {reason}",
            act.noun()
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
    if !done.is_empty() {
        inner.undo = Some(UndoRecord { act, touched: done });
    }
}

async fn undo(state: &SharedState, s: &Session) -> Result<Vec<usize>, String> {
    let record = s.lock().undo.take().ok_or("there is nothing to undo")?;
    let mut restored = Vec::new();
    let mut errors = 0;
    for t in &record.touched {
        let id = s.lock().items.get(t.index).map(|i| i.cand.id.clone());
        let Some(id) = id else { continue };
        let result = match (&t.moved_to, record.act) {
            (Some(moved), _) => {
                match graph::run(
                    state,
                    &t.account,
                    std::slice::from_ref(moved),
                    Op::Move("inbox"),
                )
                .await
                .remove(0)
                {
                    Ok(back) => {
                        let back = back.unwrap_or_else(|| moved.clone());
                        let r = graph::run(
                            state,
                            &t.account,
                            std::slice::from_ref(&back),
                            Op::MarkUnread,
                        )
                        .await
                        .remove(0);
                        r.map(|_| Some(back))
                    }
                    Err(e) => Err(e),
                }
            }
            (None, Act::Flag) => graph::run(state, &t.account, &[id], Op::Unflag)
                .await
                .remove(0)
                .map(|_| None),
            (None, Act::Read) => graph::run(state, &t.account, &[id], Op::MarkUnread)
                .await
                .remove(0)
                .map(|_| None),
            (None, _) => Ok(None),
        };
        match result {
            Ok(new_id) => {
                let mut inner = s.lock();
                if let Some(item) = inner.items.get_mut(t.index) {
                    if let Some(new_id) = new_id {
                        item.cand.id = new_id;
                    }
                    item.handled = false;
                }
                if let Some(n) = inner.tally.get_mut(record.act.past()) {
                    *n = n.saturating_sub(1);
                }
                restored.push(t.index);
            }
            Err(_) => errors += 1,
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
