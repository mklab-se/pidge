//! Which unread messages a review covers. With no request from the user
//! pidge picks: since the last review, narrowed to today when that is a
//! lot, widened a few days back when it is very little. Pure, so the
//! thresholds are tested with fixed clocks.

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// More than this many since the last review: start with today's.
pub const NARROW_ABOVE: usize = 30;
/// Fewer than this: widen the window a few days back.
pub const WIDEN_BELOW: usize = 15;
/// `auto` never reaches further back than this many days.
pub const MAX_AUTO_DAYS: i64 = 7;
/// An explicit `days` or `since_last_review` reaches at most this far back.
pub const MAX_DAYS: i64 = 30;
/// The widening steps, in days back from the start of today.
const WIDEN_STEPS: [i64; 3] = [1, 3, 7];

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    /// pidge decides (the default).
    #[default]
    Auto,
    Today,
    SinceLastReview,
    /// The last `days` days, today included.
    Days,
    /// Every unread message in the inbox, however old.
    AllUnread,
}

/// The resolved window: messages received at or after `start` (all of
/// them when `None`), with a phrase for the user and, when pidge adjusted
/// the window, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub start: Option<DateTime<Utc>>,
    pub label: String,
    pub reason: Option<String>,
}

/// Midnight `days_back` days before today, in the user's timezone.
pub fn start_of_day(now: DateTime<Utc>, tz: Tz, days_back: i64) -> DateTime<Utc> {
    let day = now.with_timezone(&tz).date_naive() - Duration::days(days_back);
    tz.from_local_datetime(&day.and_time(NaiveTime::MIN))
        .earliest()
        .map_or(now - Duration::days(days_back), |t| t.with_timezone(&Utc))
}

/// How far back to list from Graph so [`resolve`] has what it needs.
pub fn fetch_since(
    kind: ScopeKind,
    days: Option<u32>,
    now: DateTime<Utc>,
    tz: Tz,
    last_review_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let floor = start_of_day(now, tz, MAX_DAYS - 1);
    match kind {
        ScopeKind::Auto => Some(start_of_day(now, tz, MAX_AUTO_DAYS)),
        ScopeKind::Today => Some(start_of_day(now, tz, 0)),
        ScopeKind::SinceLastReview => Some(
            last_review_at
                .unwrap_or_else(|| start_of_day(now, tz, 0))
                .max(floor),
        ),
        ScopeKind::Days => Some(days_start(days, now, tz)),
        ScopeKind::AllUnread => None,
    }
}

/// Picks the window for `kind` given the receive times of the unread
/// candidates (any order) listed from [`fetch_since`].
pub fn resolve(
    kind: ScopeKind,
    days: Option<u32>,
    received: &[DateTime<Utc>],
    now: DateTime<Utc>,
    tz: Tz,
    last_review_at: Option<DateTime<Utc>>,
) -> Window {
    let count = |start: DateTime<Utc>| received.iter().filter(|t| **t >= start).count();
    let today = start_of_day(now, tz, 0);
    let since_phrase =
        |t: DateTime<Utc>| format!("since your last review ({})", phrase_time(t, now, tz));
    match kind {
        ScopeKind::Today => window(Some(today), "today", None),
        ScopeKind::AllUnread => window(None, "all unread in the inbox", None),
        ScopeKind::Days => {
            let n = days.unwrap_or(1).clamp(1, MAX_DAYS as u32);
            let label = if n == 1 {
                "today".to_string()
            } else {
                format!("the last {n} days")
            };
            window(Some(days_start(days, now, tz)), &label, None)
        }
        ScopeKind::SinceLastReview => match last_review_at {
            Some(t) => {
                let floor = start_of_day(now, tz, MAX_DAYS - 1);
                if t < floor {
                    window(Some(floor), &format!("the last {MAX_DAYS} days"), None)
                } else {
                    window(Some(t), &since_phrase(t), None)
                }
            }
            None => window(Some(today), "today (no earlier review)", None),
        },
        ScopeKind::Auto => {
            let auto_floor = start_of_day(now, tz, MAX_AUTO_DAYS);
            let (base, base_label) = match last_review_at {
                Some(t) if t >= auto_floor => (t, since_phrase(t)),
                Some(_) => (auto_floor, format!("the last {MAX_AUTO_DAYS} days")),
                None => (today, "today".to_string()),
            };
            let n = count(base);
            if n > NARROW_ABOVE && base < today && count(today) > 0 {
                let m = count(today);
                return window(
                    Some(today),
                    "today",
                    Some(format!(
                        "{n} unread {base_label}, so starting with today's {m}"
                    )),
                );
            }
            if n < WIDEN_BELOW {
                let mut chosen = None;
                for d in WIDEN_STEPS {
                    let start = start_of_day(now, tz, d);
                    if start >= base {
                        continue;
                    }
                    chosen = Some((d, start));
                    if count(start) >= WIDEN_BELOW {
                        break;
                    }
                }
                if let Some((d, start)) = chosen
                    && count(start) > n
                {
                    let label = if d == 1 {
                        "since yesterday".to_string()
                    } else {
                        format!("the last {d} days")
                    };
                    return window(
                        Some(start),
                        &label,
                        Some(format!(
                            "only {n} unread {base_label}, so including {label}"
                        )),
                    );
                }
            }
            window(Some(base), &base_label, None)
        }
    }
}

fn days_start(days: Option<u32>, now: DateTime<Utc>, tz: Tz) -> DateTime<Utc> {
    let n = days.unwrap_or(1).clamp(1, MAX_DAYS as u32) as i64;
    start_of_day(now, tz, n - 1)
}

fn window(start: Option<DateTime<Utc>>, label: &str, reason: Option<String>) -> Window {
    Window {
        start,
        label: label.to_string(),
        reason,
    }
}

/// `today 08:14`, `yesterday 17:02`, `Thursday 09:30`, or `3 Oct 09:30`.
pub fn phrase_time(t: DateTime<Utc>, now: DateTime<Utc>, tz: Tz) -> String {
    let local = t.with_timezone(&tz);
    let days = (now.with_timezone(&tz).date_naive() - local.date_naive()).num_days();
    let hm = local.format("%H:%M");
    match days {
        0 => format!("today {hm}"),
        1 => format!("yesterday {hm}"),
        2..=6 => format!("{} {hm}", local.format("%A")),
        _ => format!("{} {hm}", local.format("%-d %b")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = chrono_tz::Europe::Stockholm;

    /// Sunday 2026-10-11 15:00 in Stockholm.
    fn now() -> DateTime<Utc> {
        TZ.with_ymd_and_hms(2026, 10, 11, 15, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ago(hours: i64) -> DateTime<Utc> {
        now() - Duration::hours(hours)
    }

    fn times(n: usize, hours_ago: i64) -> Vec<DateTime<Utc>> {
        (0..n).map(|_| ago(hours_ago)).collect()
    }

    #[test]
    fn auto_without_history_is_today_when_today_is_busy_enough() {
        let mut r = times(20, 2);
        r.extend(times(5, 30));
        let w = resolve(ScopeKind::Auto, None, &r, now(), TZ, None);
        assert_eq!(w.start, Some(start_of_day(now(), TZ, 0)));
        assert_eq!(w.label, "today");
        assert_eq!(w.reason, None);
    }

    #[test]
    fn auto_narrows_a_long_backlog_to_today() {
        let mut r = times(10, 2);
        r.extend(times(40, 50));
        let last = Some(ago(24 * 5));
        let w = resolve(ScopeKind::Auto, None, &r, now(), TZ, last);
        assert_eq!(w.label, "today");
        assert!(w.reason.unwrap().contains("so starting with today's 10"));
    }

    #[test]
    fn auto_widens_a_quiet_day_until_there_is_enough() {
        // 4 today, 3 yesterday, 10 three days ago.
        let mut r = times(4, 2);
        r.extend(times(3, 20));
        r.extend(times(10, 60));
        let w = resolve(ScopeKind::Auto, None, &r, now(), TZ, None);
        assert_eq!(w.start, Some(start_of_day(now(), TZ, 3)));
        assert_eq!(w.label, "the last 3 days");
        assert!(w.reason.unwrap().starts_with("only 4 unread today"));
    }

    #[test]
    fn auto_stops_widening_at_a_week() {
        let mut r = times(2, 2);
        r.extend(times(2, 24 * 6));
        let w = resolve(ScopeKind::Auto, None, &r, now(), TZ, None);
        assert_eq!(w.start, Some(start_of_day(now(), TZ, 7)));
    }

    #[test]
    fn auto_keeps_the_window_when_widening_adds_nothing() {
        let r = times(3, 2);
        let w = resolve(ScopeKind::Auto, None, &r, now(), TZ, None);
        assert_eq!(w.label, "today");
        assert_eq!(w.reason, None);
    }

    #[test]
    fn auto_uses_the_last_review_when_the_count_is_right() {
        let r = times(20, 30);
        let last = ago(40);
        let w = resolve(ScopeKind::Auto, None, &r, now(), TZ, Some(last));
        assert_eq!(w.start, Some(last));
        assert!(
            w.label.starts_with("since your last review (Friday"),
            "{}",
            w.label
        );
    }

    #[test]
    fn explicit_scopes() {
        let w = resolve(ScopeKind::Days, Some(3), &[], now(), TZ, None);
        assert_eq!(w.start, Some(start_of_day(now(), TZ, 2)));
        assert_eq!(w.label, "the last 3 days");
        let w = resolve(ScopeKind::AllUnread, None, &[], now(), TZ, None);
        assert_eq!(w.start, None);
        let old = ago(24 * 60);
        let w = resolve(ScopeKind::SinceLastReview, None, &[], now(), TZ, Some(old));
        assert_eq!(w.start, Some(start_of_day(now(), TZ, MAX_DAYS - 1)));
        assert_eq!(
            fetch_since(ScopeKind::Auto, None, now(), TZ, None),
            Some(start_of_day(now(), TZ, MAX_AUTO_DAYS))
        );
    }

    #[test]
    fn phrases_are_relative_to_today() {
        assert_eq!(phrase_time(ago(1), now(), TZ), "today 14:00");
        assert_eq!(phrase_time(ago(24), now(), TZ), "yesterday 15:00");
        assert_eq!(phrase_time(ago(72), now(), TZ), "Thursday 15:00");
        assert_eq!(phrase_time(ago(24 * 10), now(), TZ), "1 Oct 15:00");
    }
}
