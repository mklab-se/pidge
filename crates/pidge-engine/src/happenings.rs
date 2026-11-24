//! Mail about something that happens at a time, and how that time looks in
//! the calendar. Rules find the dates, the user's AI (when there is one)
//! says whether it is something that happens to the user and which of the
//! dates it is, and a local copy of the calendar says whether that time is
//! free. All of it in the background, before anyone asks.

use std::sync::Arc;
use std::time::Duration;

use ailloy::{Question, Questions};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use pidge_core::ResponseStatus;
use pidge_core::provider::{Folder, Provider};
use pidge_store::{Filter, Happening, Stored};
use serde_json::{Value, json};

use crate::ctx::User;
use crate::dates::{self, Found};
use crate::kinds::Kind;

/// Messages looked into per round, at most.
const PER_ROUND: usize = 15;
/// Tries before a message that cannot be read is left alone.
const MAX_TRIES: u32 = 3;
const AI_TIMEOUT: Duration = Duration::from_secs(20);
const SURE: f64 = 0.75;
/// How much of a message is searched, and how much the AI sees.
const BODY_CHARS: usize = 6_000;
const AI_CHARS: usize = 2_500;

/// Words that make it worth reading the whole message for a time.
const EVENT_WORDS: &[&str] = &[
    "möte",
    "tid ",
    "tider",
    "bokning",
    "bokad",
    "boka",
    "kallelse",
    "besök",
    "hämta",
    "inbjud",
    "välkommen",
    "träff",
    "middag",
    "lunch",
    "fika",
    "visning",
    "intervju",
    "konsert",
    "biljett",
    "resa",
    "flyg",
    "incheckning",
    "appointment",
    "booking",
    "reservation",
    "meeting",
    "visit",
    "invitation",
    "interview",
    "dinner",
    "ticket",
    "flight",
    "check-in",
    "pickup",
    "pick up",
    "rsvp",
];

/// Booking systems look like mailing lists; their subjects say what they are.
const BOOKED_WORDS: &[&str] = &[
    "bokning",
    "bokad",
    "kallelse",
    "din tid",
    "påminnelse",
    "appointment",
    "your booking",
    "reservation",
    "reminder",
];

fn candidate(s: &Stored, tz: Tz) -> bool {
    let subject = s.item.subject.to_lowercase();
    let dated = |text: &str| !dates::find(text, s.item.received_at, tz).is_empty();
    let kind = s.class.kind.as_deref().and_then(Kind::parse);
    let text = format!("{}\n{}", s.item.subject, s.item.preview);
    if matches!(
        kind,
        Some(Kind::Newsletter | Kind::Promotion | Kind::Social | Kind::Receipt)
    ) || s.item.list_unsubscribe
    {
        return BOOKED_WORDS.iter().any(|w| subject.contains(w)) && dated(&text);
    }
    if s.item.is_invite
        || kind == Some(Kind::Invite)
        || (kind == Some(Kind::Delivery)
            && crate::delivery::status(&s.item) != Some(crate::delivery::Status::ToCollect))
    {
        return false;
    }
    if dated(&text) {
        return true;
    }
    let lower = text.to_lowercase();
    EVENT_WORDS.iter().any(|w| lower.contains(w))
}

/// Look for times in new mail in the inbox. Each message is looked into once.
pub async fn check(user: &Arc<User>) {
    let tz = user.tz().await;
    let recent = match user.store.query(&Filter {
        folder: Some(Folder::Inbox),
        since: Some(Utc::now() - chrono::Duration::days(14)),
        time_unchecked: true,
        ..Default::default()
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "looking for times: index query failed");
            return;
        }
    };
    let mut looked = 0;
    let mut asked = 0;
    for s in recent {
        if s.classified_at.is_none() {
            continue;
        }
        if !candidate(&s, tz) {
            let _ = user.store.set_happening(&s.item.account, &s.item.id, None);
            continue;
        }
        if looked == PER_ROUND {
            break;
        }
        looked += 1;
        let Ok(p) = user.provider(&s.item.account) else {
            continue;
        };
        let msg = match p.get_message(&s.item.id).await {
            Ok(m) => m,
            Err(_) => {
                // Another try next round, but not for ever.
                let tries: u32 = user
                    .store
                    .get_doc("timetries", &s.handle)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                if tries + 1 >= MAX_TRIES {
                    let _ = user.store.set_happening(&s.item.account, &s.item.id, None);
                }
                let _ = user.store.put_doc(
                    "timetries",
                    &s.handle,
                    &(tries + 1),
                    Some(Utc::now() + chrono::Duration::days(2)),
                );
                continue;
            }
        };
        let body: String = pidge_core::render::strip_quoted_history(
            &crate::ops::mail_read::short_links(&crate::ops::mail_read::body_text(&msg)),
        )
        .chars()
        .take(BODY_CHARS)
        .collect();
        let text = format!("{}\n{body}", s.item.subject);
        let mut found = dates::find(&text, s.item.received_at, tz);
        found.retain(|f| f.when.start > s.item.received_at || f.when.all_day);
        let happening = if found.is_empty() {
            None
        } else {
            match ask(user, &s, &text, &found, tz).await {
                Answer::Is(h) => {
                    asked += 1;
                    Some(h)
                }
                Answer::Not => {
                    asked += 1;
                    None
                }
                Answer::Unknown => without_ai(&s, &found),
            }
        };
        let _ = user
            .store
            .set_happening(&s.item.account, &s.item.id, happening.as_ref());
    }
    if asked > 0 {
        user.record(crate::activity::ai_line("happenings", asked))
            .await;
    }
}

/// Without an AI only the plain case counts: a person, one date, a time.
fn without_ai(s: &Stored, found: &[Found]) -> Option<Happening> {
    let personal = s.class.kind.as_deref() == Some(Kind::Personal.as_str());
    match found {
        [one] if personal && !one.when.all_day => Some(one.when.clone()),
        _ => None,
    }
}

enum Answer {
    Is(Happening),
    Not,
    Unknown,
}

pub(crate) fn questions(found: &[Found], tz: Tz) -> Questions {
    let mut q = Questions::new();
    q.insert(
        "happens".into(),
        Question::yes_no(
            "Is this e-mail (`email`) about something the recipient is to attend, do, visit or \
             collect at one of the times in `times`?",
        )
        .yes(
            "An appointment, booking, meeting, visit, dinner, trip, or a time when something is \
             to be collected, agreed or proposed to the recipient",
        )
        .no(
            "The dates only say when an offer or sale ends, when something was ordered, issued, \
             paid or published, when a bill is due, when a change takes effect, or when anyone \
             may attend a public event that is being advertised",
        ),
    );
    let mut which = Question::choice("Which of the times in `times` is when it happens?");
    for (n, f) in found.iter().enumerate() {
        which = which.option(format!("t{}", n + 1), dates::words(&f.when, tz));
    }
    q.insert(
        "which".into(),
        which.option("none", "None of them, or it cannot be told"),
    );
    q
}

pub(crate) fn state(s: &Stored, text: &str, found: &[Found], tz: Tz) -> Value {
    let times: serde_json::Map<String, Value> = found
        .iter()
        .enumerate()
        .map(|(n, f)| {
            (
                format!("t{}", n + 1),
                json!({ "time": dates::words(&f.when, tz), "in_the_mail": f.words }),
            )
        })
        .collect();
    json!({
        "email": {
            "from": { "name": s.item.from.name, "address": s.item.from.address },
            "received": dates::words(
                &Happening { start: s.item.received_at, end: None, all_day: false }, tz),
            "subject": s.item.subject,
            "text": text.chars().take(AI_CHARS).collect::<String>(),
        },
        "times": times,
    })
}

async fn ask(user: &Arc<User>, s: &Stored, text: &str, found: &[Found], tz: Tz) -> Answer {
    let prefs = user.prefs().await;
    let Some(client) = user.ai().filter(|_| crate::classify::ai_usable(&prefs)) else {
        return Answer::Unknown;
    };
    if user.ai_allowance(1).await == 0 {
        return Answer::Unknown;
    }
    let q = questions(found, tz);
    let resp =
        match tokio::time::timeout(AI_TIMEOUT, client.eval(state(s, text, found, tz), &q)).await {
            Ok(Ok(r)) => r,
            _ => return Answer::Unknown,
        };
    let happens = resp
        .answers
        .get("happens")
        .and_then(|a| a.as_yes_no())
        .unwrap_or(0.0);
    if happens < SURE {
        return Answer::Not;
    }
    match resp.answers.get("which") {
        Some(ailloy::Answer::Choice { choice, .. }) => choice
            .strip_prefix('t')
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| found.get(n.wrapping_sub(1)))
            .map_or(Answer::Not, |f| Answer::Is(f.when.clone())),
        _ => Answer::Unknown,
    }
}

// ---- the calendar, kept nearby ----------------------------------------------

const MIRROR_DAYS: i64 = 90;
const MIRROR_FRESH_MINUTES: i64 = 10;

/// Keep a local copy of the calendars, at most every ten minutes.
pub async fn refresh_calendar(user: &Arc<User>, providers: &[Arc<dyn Provider>]) {
    let from = Utc::now() - chrono::Duration::days(1);
    let to = from + chrono::Duration::days(MIRROR_DAYS);
    let tasks = providers.iter().map(|p| async move {
        let key = p.address().to_string();
        let fresh = user
            .store
            .get_doc::<bool>("calmirror", &key)
            .ok()
            .flatten()
            .is_some();
        if fresh {
            return;
        }
        match p.list_events(from, to).await {
            Ok(mut events) => {
                // A change still on its way is already true here.
                crate::ops::calendar::overlay(user, &mut events);
                let _ = user.store.replace_events(&key, from, to, &events);
                let _ = user.store.put_doc(
                    "calmirror",
                    &key,
                    &true,
                    Some(Utc::now() + chrono::Duration::minutes(MIRROR_FRESH_MINUTES)),
                );
                let _ = user.store.put_doc("calknown", &key, &true, None);
            }
            Err(e) => tracing::debug!(account = %key, error = %e, "calendar not refreshed"),
        }
    });
    futures::future::join_all(tasks).await;
}

/// How a time looks in the calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Free,
    /// Something is there already: its handle and title.
    Clash(String, String),
    /// This very thing is in the calendar.
    Booked(String, String),
}

impl Slot {
    pub fn words(&self) -> String {
        match self {
            Slot::Free => "free".into(),
            Slot::Clash(h, t) => format!("clashes with {h} {}", crate::render::inline(t)),
            Slot::Booked(h, t) => format!("already booked: {h} {}", crate::render::inline(t)),
        }
    }
}

fn significant(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_string)
        .collect()
}

/// The calendar at the time a message is about. `None` when the calendar has
/// not been read yet, or the time has passed.
pub fn slot(user: &User, s: &Stored, h: &Happening, now: DateTime<Utc>) -> Option<Slot> {
    let end = h.end.unwrap_or(
        h.start
            + if h.all_day {
                chrono::Duration::days(1)
            } else {
                chrono::Duration::hours(1)
            },
    );
    if end < now || h.start > now + chrono::Duration::days(MIRROR_DAYS - 2) {
        return None;
    }
    let accounts = user.addresses();
    let known = accounts
        .iter()
        .all(|a| matches!(user.store.get_doc::<bool>("calknown", a), Ok(Some(_))));
    if !known {
        return None;
    }
    let events: Vec<_> = user
        .store
        .events_between(&accounts, h.start, end)
        .ok()?
        .into_iter()
        .filter(|(_, e)| e.response_status != ResponseStatus::Declined)
        .collect();
    let about = significant(&format!(
        "{} {} {}",
        s.item.subject, s.item.from.name, s.item.from.address
    ));
    let same = |title: &str| significant(title).iter().any(|w| about.contains(w));
    if let Some((handle, e)) = events
        .iter()
        .find(|(_, e)| same(&e.subject) && (h.all_day || !e.all_day))
    {
        return Some(Slot::Booked(handle.clone(), e.subject.clone()));
    }
    if h.all_day {
        return None;
    }
    Some(match events.iter().find(|(_, e)| !e.all_day) {
        Some((handle, e)) => Slot::Clash(handle.clone(), e.subject.clone()),
        None => Slot::Free,
    })
}

/// `when: Thu 26 Nov 17:00-19:00, free`, for a list line.
pub fn line(user: &User, s: &Stored, tz: Tz, now: DateTime<Utc>) -> Option<String> {
    let h = s.happening.as_ref()?;
    let end = h.end.unwrap_or(h.start + chrono::Duration::days(1));
    if end < now {
        return None;
    }
    let slot = slot(user, s, h, now)
        .map(|s| format!(", {}", s.words()))
        .unwrap_or_default();
    Some(format!("when: {}{slot}", dates::words(h, tz)))
}
