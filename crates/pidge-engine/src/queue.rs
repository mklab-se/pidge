//! The queue: every change the user asks for is an instruction with an id
//! (`q7`). The index shows it at once and the caller has its answer; the
//! provider hears about it when it is due. Until then `undo` simply drops
//! it, afterwards `undo` sends the opposite. How long an instruction waits
//! is an internal detail.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use pidge_core::Event;
use pidge_core::provider::{
    EventPatch, EventSpec, EventWhen, Folder, MailChange, ProviderError, Rsvp,
};
use pidge_store::{HandleKind, QueueRow, UndoEntry};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ctx::{Frontend, User};
use crate::error::{OpError, OpResult};
use crate::registry::Output;
use crate::render;

/// One change to some messages of one mailbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailJob {
    pub account: String,
    pub ids: Vec<String>,
    pub change: MailChange,
}

/// One change to a calendar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum CalChange {
    Cancel {
        message: Option<String>,
    },
    Delete,
    Respond {
        response: Rsvp,
        message: Option<String>,
        notify: bool,
    },
    Update {
        patch: EventPatch,
    },
    Create {
        spec: EventSpec,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalJob {
    pub account: String,
    /// The event (empty for a create).
    #[serde(default)]
    pub id: String,
    pub change: CalChange,
}

/// What an instruction does, and what takes it back.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub mail: Vec<MailJob>,
    pub cal: Vec<CalJob>,
    /// The messages as the index had them before.
    pub before: Vec<UndoEntry>,
    /// The events as the index had them before.
    pub events_before: Vec<Event>,
    /// What undoes a calendar change once it has been made.
    pub undo_cal: Vec<CalJob>,
    /// A draft in the outbox this instruction sends.
    pub draft: Option<String>,
    /// Already carried out when it was recorded.
    pub done: bool,
    /// What is left to try again (`None`: all of it).
    pub todo: Option<Todo>,
    /// Done by one of the user's standing rules, not asked for just now.
    pub by_rule: bool,
    /// The standing rules the user made along with it.
    pub always: Vec<crate::prefs::Always>,
    /// This instruction takes another back.
    pub undoes: Option<String>,
}

/// The part of an instruction that has not been carried out yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub mail: Vec<MailJob>,
    pub cal: Vec<CalJob>,
}

/// What the caller gets back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub id: String,
    /// On the CLI the change has been made by now: how many failed.
    pub failed: usize,
}

const MAX_ATTEMPTS: u32 = 3;
const LOOK_BACK: usize = 50;

fn wait_for(change: &MailChange) -> Duration {
    match change {
        MailChange::Delete => Duration::seconds(60),
        MailChange::Move { .. } => Duration::seconds(30),
        _ => Duration::zero(),
    }
}

/// How long a change waits before the provider hears of it.
fn wait(job: &Job) -> Duration {
    if job.undoes.is_some() {
        return Duration::zero();
    }
    let mail = job.mail.iter().map(|m| wait_for(&m.change)).max();
    let cal = job.cal.iter().map(|c| match &c.change {
        CalChange::Create { .. } => Duration::zero(),
        _ => Duration::seconds(30),
    });
    mail.into_iter().chain(cal).max().unwrap_or_default()
}

/// Take an instruction: the index changes now, the provider when it is due.
pub async fn submit(user: &Arc<User>, label: &str, mut job: Job) -> OpResult<Submitted> {
    let inline = user.frontend() == Frontend::Cli;
    let now = Utc::now();
    let due = if inline { now } else { now + wait(&job) };
    let id = user.store.handle(
        HandleKind::Queue,
        "",
        &format!("{}-{:08x}", now.timestamp_micros(), rand::random::<u32>()),
    )?;
    let json = |job: &Job| serde_json::to_string(job).unwrap_or_default();
    let seq = user.store.enqueue(&id, label, due, &json(&job))?;
    for m in &job.mail {
        let before = user
            .store
            .apply_local(&m.account, &m.ids, &m.change, Some(seq))?;
        for b in before {
            // The first change of an instruction saw the original.
            if !job
                .before
                .iter()
                .any(|x| x.account == b.account && x.id == b.id)
            {
                job.before.push(b);
            }
        }
    }
    for c in job.cal.iter().filter(|_| !job.done) {
        user.store.touch(seq, &c.account, &c.id)?;
        apply_event_local(user, c);
    }
    user.store.set_queued_json(seq, &json(&job))?;
    user.own_instructions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(seq);
    if job.done {
        user.store.finish_queued(seq, "done")?;
    } else {
        user.store.release_queued(seq)?;
    }
    let mut failed = 0;
    if inline {
        failed = run_due(user).await.get(&seq).copied().unwrap_or_default();
    } else if !job.done {
        wake_at(user, due);
    }
    Ok(Submitted { id, failed })
}

/// Show a calendar change in the index at once.
fn apply_event_local(user: &User, c: &CalJob) {
    match &c.change {
        CalChange::Cancel { .. } | CalChange::Delete => {
            let _ = user.store.remove_event(&c.account, &c.id);
        }
        CalChange::Respond { response, .. } => {
            if let Ok(Some(mut e)) = user.store.event(&c.account, &c.id) {
                e.response_status = response_status(*response);
                let _ = user.store.upsert_events(&[e]);
            }
        }
        CalChange::Update { patch } => {
            if let Ok(Some(mut e)) = user.store.event(&c.account, &c.id) {
                patch_event(&mut e, patch);
                let _ = user.store.upsert_events(&[e]);
            }
        }
        CalChange::Create { .. } => {}
    }
}

pub fn response_status(r: Rsvp) -> pidge_core::ResponseStatus {
    match r {
        Rsvp::Accept => pidge_core::ResponseStatus::Accepted,
        Rsvp::Tentative => pidge_core::ResponseStatus::Tentative,
        Rsvp::Decline => pidge_core::ResponseStatus::Declined,
    }
}

/// An event as it will be once a patch is made.
pub fn patch_event(e: &mut Event, p: &EventPatch) {
    if let Some(t) = &p.title {
        e.subject = t.clone();
    }
    if let Some(l) = &p.location {
        e.location = Some(l.clone()).filter(|l| !l.is_empty());
    }
    match &p.when {
        Some(EventWhen::Timed { start, end }) => {
            e.start = start.clone();
            e.end = end.clone();
            e.all_day = false;
        }
        Some(EventWhen::AllDay { first, last }) => {
            let at = |d: &chrono::NaiveDate| d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
            e.start.at = at(first);
            e.end.at = at(last) + Duration::days(1);
            e.all_day = true;
        }
        None => {}
    }
    if let Some(list) = &p.attendees {
        e.attendees = list
            .iter()
            .map(|a| {
                e.attendees
                    .iter()
                    .find(|x| x.address.eq_ignore_ascii_case(&a.address))
                    .cloned()
                    .unwrap_or(pidge_core::Attendee {
                        name: a.name.clone(),
                        address: a.address.clone(),
                        kind: pidge_core::AttendeeKind::Required,
                        response: pidge_core::ResponseStatus::None,
                    })
            })
            .collect();
    }
}

/// Calendar changes still on their way, oldest first.
pub fn pending_calendar(user: &User) -> Vec<CalJob> {
    let mut rows = user.store.queue(LOOK_BACK).unwrap_or_default();
    rows.reverse();
    rows.into_iter()
        .filter(|r| r.status == "queued" || r.status == "running")
        .flat_map(|r| {
            let job = job_of(&r);
            job.todo.map_or(job.cal, |t| t.cal)
        })
        .collect()
}

fn job_of(row: &QueueRow) -> Job {
    serde_json::from_str(&row.json).unwrap_or_default()
}

fn wake_at(user: &Arc<User>, due: DateTime<Utc>) {
    let u = user.clone();
    tokio::spawn(async move {
        let wait = (due - Utc::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait + std::time::Duration::from_millis(20)).await;
        run_due(&u).await;
    });
}

/// Instructions a stopped server left behind in a durable index.
pub async fn resume(user: &Arc<User>) {
    let Ok(Some(due)) = user.store.next_queued() else {
        return;
    };
    match user.frontend() {
        Frontend::Mcp => wake_at(user, due),
        Frontend::Cli => {
            run_due(user).await;
        }
    }
}

/// Carry out everything this process has promised, due or not (it is ending).
pub async fn flush(user: &Arc<User>) {
    let own = user
        .own_instructions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let _ = user.store.hurry_queue(&own);
    run_due(user).await;
}

/// Carry out what is due. Returns the failures per instruction.
pub async fn run_due(user: &Arc<User>) -> HashMap<i64, usize> {
    let _one_at_a_time = user.queue_lock.lock().await;
    let mut failures = HashMap::new();
    while let Ok(Some(row)) = user.store.claim_queued(Utc::now()) {
        let seq = row.seq;
        let failed = run_row(user, row).await;
        failures.insert(seq, failed);
    }
    failures
}

/// How a message's change went.
enum Went {
    Done,
    /// It is not in the mailbox any more.
    Gone,
    Again(String),
    Failed(String),
}

fn went(e: ProviderError) -> Went {
    match e {
        ProviderError::NotFound(_) => Went::Gone,
        ProviderError::Invalid(m) if m.contains("Id is malformed") => Went::Gone,
        ProviderError::Throttled { .. } => Went::Again("the mail service is busy".into()),
        ProviderError::Other(m) => Went::Again(m),
        other => Went::Failed(other.to_string()),
    }
}

/// What one change does to one message.
enum Put {
    Folder(Folder),
    Unread(bool),
    Flagged(bool),
    Categories(Vec<String>),
}

fn put_by(change: &MailChange) -> Put {
    match change {
        MailChange::MarkRead => Put::Unread(false),
        MailChange::MarkUnread => Put::Unread(true),
        MailChange::Flag { .. } => Put::Flagged(true),
        MailChange::Unflag => Put::Flagged(false),
        MailChange::Move { to } => Put::Folder(to.clone()),
        MailChange::Delete => Put::Folder(Folder::Deleted),
        MailChange::SetCategories { categories } => Put::Categories(categories.clone()),
    }
}

/// Take one change that failed out of the index again.
fn put_back(user: &User, seq: i64, b: &UndoEntry, change: &MailChange) {
    let Ok(Some(now)) = user.store.get(&b.account, &b.id) else {
        return;
    };
    let mut back = UndoEntry {
        account: b.account.clone(),
        id: b.id.clone(),
        folder: now.item.folder,
        unread: now.item.unread,
        flagged: now.item.flagged,
        categories: now.item.categories,
    };
    match put_by(change) {
        Put::Folder(_) => back.folder = b.folder.clone(),
        Put::Unread(_) => back.unread = b.unread,
        Put::Flagged(_) => back.flagged = b.flagged,
        Put::Categories(_) => back.categories = b.categories.clone(),
    }
    let _ = user.store.restore_under(&back, seq);
}

async fn run_row(user: &Arc<User>, row: QueueRow) -> usize {
    let started = std::time::Instant::now();
    // A command cannot wait to try again: it says what failed.
    let may_retry = user.frontend() == Frontend::Mcp && row.attempts < MAX_ATTEMPTS;
    let mut job = job_of(&row);
    let work = job.todo.take().unwrap_or_else(|| Todo {
        mail: job.mail.clone(),
        cal: job.cal.clone(),
    });
    let mut again = Todo::default();
    // (what, message, why)
    let mut failed: Vec<(MailJob, String)> = Vec::new();
    let mut done = 0;
    // An id the provider has since replaced: old → new.
    let mut renamed: HashMap<String, String> = HashMap::new();
    for m in work.mail {
        let ids: Vec<String> = m
            .ids
            .iter()
            .map(|id| renamed.get(id).unwrap_or(id).clone())
            .collect();
        let one = |id: &str| MailJob {
            account: m.account.clone(),
            ids: vec![id.to_string()],
            change: m.change.clone(),
        };
        let Ok(p) = user.provider(&m.account) else {
            let why = format!("{} is not connected", m.account);
            failed.extend(ids.iter().map(|id| (one(id), why.clone())));
            continue;
        };
        if let MailChange::SetCategories { categories } = &m.change {
            for c in categories {
                let _ = p.ensure_category(c).await;
            }
        }
        let results = p.apply(&ids, &m.change).await;
        let mut retry = Vec::new();
        for (n, id) in ids.iter().enumerate() {
            let mut id = id.clone();
            let mut outcome = match results.get(n) {
                Some(Ok(())) => Went::Done,
                Some(Err(e)) => went(e.clone()),
                None => Went::Failed("no answer".into()),
            };
            if matches!(outcome, Went::Gone) {
                // Gone, or known under another id by now?
                match find_again(user, &job, &m.account, &id).await {
                    Ok(Some(new)) if new != id => {
                        let _ = user.store.rename_item(&m.account, &id, &new);
                        for b in job.before.iter_mut().filter(|b| b.id == id) {
                            b.id = new.clone();
                        }
                        for j in job.mail.iter_mut() {
                            for x in j.ids.iter_mut().filter(|x| **x == id) {
                                *x = new.clone();
                            }
                        }
                        renamed.insert(id.clone(), new.clone());
                        id = new;
                        outcome = match p.apply(std::slice::from_ref(&id), &m.change).await.pop() {
                            Some(Ok(())) => Went::Done,
                            Some(Err(e)) => match went(e) {
                                Went::Gone => Went::Failed("not found".into()),
                                w => w,
                            },
                            None => Went::Failed("no answer".into()),
                        };
                    }
                    Ok(Some(_)) => outcome = Went::Failed("the target was not found".into()),
                    Ok(None) => {}
                    Err(e) => outcome = went(e),
                }
            }
            match outcome {
                Went::Done => done += 1,
                Went::Gone => {
                    let _ = user
                        .store
                        .remove_items(&m.account, std::slice::from_ref(&id));
                    let out_of_the_way = matches!(
                        m.change,
                        MailChange::Delete
                            | MailChange::MarkRead
                            | MailChange::Move {
                                to: Folder::Archive
                            }
                    );
                    if out_of_the_way {
                        done += 1;
                    } else {
                        failed.push((one(&id), "it is no longer there".into()));
                    }
                }
                Went::Again(_) if may_retry => retry.push(id),
                Went::Again(why) | Went::Failed(why) => failed.push((one(&id), why)),
            }
        }
        if !retry.is_empty() {
            again.mail.push(MailJob { ids: retry, ..m });
        }
    }
    let mut cal_failed: Vec<(CalJob, String)> = Vec::new();
    for c in work.cal {
        // Asked twice, these would tell people twice or make two events: only
        // a clear refusal is tried again.
        let once = !matches!(
            c.change,
            CalChange::Update { .. } | CalChange::Delete | CalChange::Respond { notify: false, .. }
        );
        match run_cal(user, &c).await {
            Ok(()) => done += 1,
            Err(e) => {
                let refused = matches!(e, ProviderError::Throttled { .. });
                match went(e) {
                    // Someone else removed it: cancelled, deleted, declined.
                    Went::Gone | Went::Done => done += 1,
                    Went::Again(_) if may_retry && (refused || !once) => again.cal.push(c),
                    Went::Again(why) | Went::Failed(why) => cal_failed.push((c, why)),
                }
            }
        }
    }
    for (m, why) in &failed {
        let id = m.ids.first().cloned().unwrap_or_default();
        tracing::debug!(account = %m.account, id, why, "a change failed");
        if let Some(b) = job
            .before
            .iter()
            .find(|b| b.account == m.account && b.id == id)
        {
            put_back(user, row.seq, b, &m.change);
        }
        // Undo is not to take back what was never done.
        for j in job
            .mail
            .iter_mut()
            .filter(|j| j.account == m.account && j.change == m.change)
        {
            j.ids.retain(|x| *x != id);
        }
    }
    for (c, _) in &cal_failed {
        if let Some(e) = job.events_before.iter().find(|e| e.id == c.id) {
            let _ = user.store.upsert_events(std::slice::from_ref(e));
        }
        job.cal.retain(|x| x != c);
        job.undo_cal.retain(|u| c.id.is_empty() || u.id != c.id);
    }
    let n_failed = failed.len() + cal_failed.len();
    if n_failed > 0 {
        let why = failed
            .first()
            .map(|f| f.1.clone())
            .or_else(|| cal_failed.first().map(|f| f.1.clone()))
            .unwrap_or_default();
        let _ = user.store.push_note(&format!(
            "{} ({}): {n_failed} failed ({}); left as it was.",
            row.label,
            row.id,
            render::cap_inline(&render::one_line(&why), 120)
        ));
    }
    let retrying: usize = again.mail.iter().map(|m| m.ids.len()).sum::<usize>() + again.cal.len();
    let json = |job: &Job| serde_json::to_string(job).unwrap_or_default();
    if retrying > 0 {
        job.todo = Some(again);
        let wait = [5, 30, 120][(row.attempts as usize).min(2)];
        let due = Utc::now() + Duration::seconds(wait);
        let _ = user.store.requeue(row.seq, due, &json(&job));
        wake_at(user, due);
    } else {
        let _ = user.store.set_queued_json(row.seq, &json(&job));
        let status = if done == 0 && n_failed > 0 && row.attempts == 0 {
            "failed"
        } else {
            "done"
        };
        let _ = user.store.finish_queued(row.seq, status);
    }
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    user.record(crate::activity::job_line(
        &row.id,
        &row.label,
        ms,
        row.attempts,
        done,
        n_failed,
        retrying,
    ))
    .await;
    n_failed
}

/// The id the provider has for a message now, by its Message-ID. Only the
/// copy that is where the message was: the user's sent copy has the same
/// Message-ID, and one that has been moved has been dealt with.
async fn find_again(
    user: &User,
    job: &Job,
    account: &str,
    id: &str,
) -> Result<Option<String>, ProviderError> {
    let Ok(Some(s)) = user.store.get(account, id) else {
        return Ok(None);
    };
    if s.item.message_id.is_empty() {
        return Ok(None);
    }
    let Ok(p) = user.provider(account) else {
        return Ok(None);
    };
    let was = job
        .before
        .iter()
        .find(|b| b.account == account && b.id == id)
        .map_or(s.item.folder, |b| b.folder.clone());
    let found = p.find_by_message_id(&s.item.message_id).await?;
    // The very same id may be listed: then it is not the message that is
    // missing.
    if found.iter().any(|i| i.id == id) {
        return Ok(Some(id.to_string()));
    }
    Ok(found.into_iter().find(|i| i.folder == was).map(|i| i.id))
}

async fn run_cal(user: &User, c: &CalJob) -> Result<(), ProviderError> {
    let p = user
        .provider(&c.account)
        .map_err(|e| ProviderError::Invalid(e.message))?;
    match &c.change {
        CalChange::Cancel { message } => p.cancel_event(&c.id, message.as_deref()).await,
        CalChange::Delete => p.delete_event(&c.id).await,
        CalChange::Respond {
            response,
            message,
            notify,
        } => {
            p.respond(&c.id, *response, message.as_deref(), *notify)
                .await
        }
        CalChange::Update { patch } => {
            let e = p.update_event(&c.id, patch).await?;
            let _ = user.store.upsert_events(&[e]);
            Ok(())
        }
        CalChange::Create { spec } => {
            let e = p.create_event(spec).await?;
            let _ = user.store.upsert_events(&[e]);
            Ok(())
        }
    }
}

/// Take an instruction back: the latest, or the one named. Returns the
/// messages it concerned, for "do this instead".
pub async fn undo(user: &Arc<User>, id: Option<&str>) -> OpResult<(Output, Vec<(String, String)>)> {
    let row = match id {
        Some(id) => user.store.queued(id.trim())?.ok_or_else(|| {
            OpError::not_found(format!("nothing called {id:?} to undo"))
                .with_hint("use the id the answer gave (q7), or none for the latest")
        })?,
        None => {
            // What the user just asked for: not what a rule of theirs did
            // meanwhile, and not an undo.
            let asked = |r: &QueueRow| {
                let job = job_of(r);
                job.undoes.is_none() && !job.by_rule
            };
            let latest = user
                .store
                .queue(LOOK_BACK)?
                .into_iter()
                .find(|r| r.status != "undone" && r.status != "failed" && asked(r));
            match latest {
                Some(r) => r,
                None => return Ok((Output::text("Nothing to undo."), Vec::new())),
            }
        }
    };
    if row.status == "undone" {
        return Ok((
            Output::text(format!("{} is already undone.", row.id)),
            Vec::new(),
        ));
    }
    let job = job_of(&row);
    let targets: Vec<(String, String)> = job
        .before
        .iter()
        .map(|b| (b.account.clone(), b.id.clone()))
        .collect();
    let mut said = format!("Undone: {} ({}).", row.label, row.id);
    let handles: Vec<String> = targets
        .iter()
        .filter_map(|(a, i)| user.store.lookup(HandleKind::Message, a, i).ok().flatten())
        .collect();
    let data = json!({ "undone": row.id, "label": row.label, "ids": handles });

    if let Some(draft) = &job.draft {
        return match user.store.outbox_entry(draft)? {
            Some(e) if e.status == "held" || e.status == "approval" => {
                user.store.remove_outbox(draft)?;
                user.store.finish_queued(row.seq, "undone")?;
                Ok((
                    Output::new(
                        format!(
                            "Stopped: {} was not sent; {draft} stays a draft.",
                            e.summary
                        ),
                        data,
                    ),
                    Vec::new(),
                ))
            }
            _ => Err(OpError::usage(
                "too late: it has been sent, and sent mail cannot be taken back",
            )),
        };
    }
    let cancelled = user.store.cancel_queued(row.seq)?;
    if cancelled && row.attempts == 0 {
        // Never carried out.
        restore(user, &job, row.seq, |_| true);
        for e in &job.events_before {
            let _ = user.store.upsert_events(std::slice::from_ref(e));
        }
    } else {
        // Being carried out right now, done, or done in part: send the
        // opposite of what has been done.
        if row.status == "running" && !cancelled {
            drop(user.queue_lock.lock().await);
        }
        let row = user.store.queued(&row.id)?.unwrap_or(row);
        let job = job_of(&row);
        // It may have been put back in the queue for another try meanwhile.
        let left = if cancelled || user.store.cancel_queued(row.seq)? {
            job.todo.clone().unwrap_or_default()
        } else {
            Todo::default()
        };
        let not_done = |b: &UndoEntry| {
            left.mail
                .iter()
                .any(|m| m.account == b.account && m.ids.contains(&b.id))
        };
        let undo_cal: Vec<CalJob> = job
            .undo_cal
            .iter()
            .filter(|u| u.id.is_empty() || !left.cal.iter().any(|c| c.id == u.id))
            .cloned()
            .collect();
        if !job.cal.is_empty() && undo_cal.is_empty() && left.cal.len() < job.cal.len() {
            return Err(OpError::usage(
                "too late: that cannot be taken back once it is done; make the change you want instead",
            ));
        }
        let mut finished = job.clone();
        finished.before.retain(|b| !not_done(b));
        let back = opposite(user, &finished)?;
        restore(user, &job, row.seq, not_done);
        let changes = !back.is_empty() || !undo_cal.is_empty();
        let inverse = Job {
            mail: back,
            cal: undo_cal,
            undoes: Some(row.id.clone()),
            ..Default::default()
        };
        if changes {
            submit(user, &format!("undo of {}", row.label), inverse).await?;
        } else if left == Todo::default() && job.always.is_empty() {
            said = format!(
                "Nothing to take back: {} ({}) has been changed again since.",
                row.label, row.id
            );
        }
        user.store.finish_queued(row.seq, "undone")?;
    }
    if !job.always.is_empty() {
        user.update_prefs(|p| p.always.retain(|r| !job.always.contains(r)))
            .await?;
        said.push_str(" And not from now on.");
    }
    Ok((Output::new(said, data), targets))
}

/// What an instruction left a message as, field by field.
#[derive(Default)]
struct Left {
    folder: Option<Folder>,
    unread: Option<bool>,
    flagged: Option<bool>,
    categories: Option<Vec<String>>,
}

fn left_as(job: &Job, b: &UndoEntry) -> Left {
    let mut left = Left::default();
    for m in &job.mail {
        if m.account != b.account || !m.ids.contains(&b.id) {
            continue;
        }
        match put_by(&m.change) {
            Put::Folder(f) => left.folder = Some(f),
            Put::Unread(u) => left.unread = Some(u),
            Put::Flagged(f) => left.flagged = Some(f),
            Put::Categories(c) => left.categories = Some(c),
        }
    }
    left
}

/// The fields of a message that are still as an instruction left them. A
/// later change is not undone with it. A message that has left the index
/// (moved to a folder that is not synced, deleted) is taken to be.
fn still(job: &Job, b: &UndoEntry, now: Option<&pidge_store::Stored>) -> Left {
    let mut left = left_as(job, b);
    if let Some(now) = now {
        left.folder = left.folder.filter(|f| now.item.folder == *f);
        left.unread = left.unread.filter(|u| now.item.unread == *u);
        left.flagged = left.flagged.filter(|f| now.item.flagged == *f);
        left.categories = left.categories.filter(|c| now.item.categories == *c);
    }
    left
}

/// The index as it was, for what was never carried out.
fn restore(user: &User, job: &Job, seq: i64, which: impl Fn(&UndoEntry) -> bool) {
    for b in job.before.iter().filter(|b| which(b)) {
        let Ok(Some(now)) = user.store.get(&b.account, &b.id) else {
            continue;
        };
        let left = still(job, b, Some(&now));
        let back = UndoEntry {
            account: b.account.clone(),
            id: b.id.clone(),
            folder: left.folder.map_or(now.item.folder, |_| b.folder.clone()),
            unread: left.unread.map_or(now.item.unread, |_| b.unread),
            flagged: left.flagged.map_or(now.item.flagged, |_| b.flagged),
            categories: left
                .categories
                .map_or(now.item.categories, |_| b.categories.clone()),
        };
        let _ = user.store.restore_under(&back, seq);
    }
}

/// The changes that put everything back.
fn opposite(user: &User, job: &Job) -> OpResult<Vec<MailJob>> {
    let mut moves: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut reads: BTreeMap<(String, bool), Vec<String>> = BTreeMap::new();
    let mut flags: BTreeMap<(String, bool), Vec<String>> = BTreeMap::new();
    let mut out = Vec::new();
    for b in &job.before {
        let now = user.store.get(&b.account, &b.id)?;
        let left = still(job, b, now.as_ref());
        if left.folder.is_some_and(|f| f != b.folder) {
            moves
                .entry((b.account.clone(), b.folder.key()))
                .or_default()
                .push(b.id.clone());
        }
        if left.unread.is_some_and(|u| u != b.unread) {
            reads
                .entry((b.account.clone(), b.unread))
                .or_default()
                .push(b.id.clone());
        }
        if left.flagged.is_some_and(|f| f != b.flagged) {
            flags
                .entry((b.account.clone(), b.flagged))
                .or_default()
                .push(b.id.clone());
        }
        if left.categories.is_some_and(|c| c != b.categories) {
            out.push(MailJob {
                account: b.account.clone(),
                ids: vec![b.id.clone()],
                change: MailChange::SetCategories {
                    categories: b.categories.clone(),
                },
            });
        }
    }
    for ((account, f), ids) in moves {
        out.push(MailJob {
            account,
            ids,
            change: MailChange::Move {
                to: Folder::from_key(&f),
            },
        });
    }
    for ((account, unread), ids) in reads {
        out.push(MailJob {
            account,
            ids,
            change: if unread {
                MailChange::MarkUnread
            } else {
                MailChange::MarkRead
            },
        });
    }
    for ((account, flagged), ids) in flags {
        out.push(MailJob {
            account,
            ids,
            change: if flagged {
                MailChange::Flag { due: None }
            } else {
                MailChange::Unflag
            },
        });
    }
    Ok(out)
}
