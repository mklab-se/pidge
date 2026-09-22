//! Time-range parsing in the user's local timezone.
//!
//! Mail ranges look backward from "now" (`Direction::Past`); calendar ranges look
//! forward (`Direction::Future`). Named ranges (`today`, `this_week`, ...) and
//! explicit `from`/`to` bounds are always resolved against the caller-supplied
//! IANA timezone, then converted to UTC.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// Whether a bare `Nd` range (e.g. `3d`) looks backward from `now` (mail) or
/// forward from `now` (calendar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Past,
    Future,
}

fn local_midnight(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    tz.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .expect("midnight exists")
        .with_timezone(&Utc)
}

/// Parse a single timestamp: RFC 3339, a naive `YYYY-MM-DDTHH:MM:SS` (interpreted
/// in `tz`), or a bare `YYYY-MM-DD` date (interpreted as local midnight, or the
/// following local midnight when `end_of_day` is set, making the date inclusive).
pub fn parse_point(s: &str, tz: Tz, end_of_day: bool) -> Result<DateTime<Utc>, String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|d| d.with_timezone(&Utc))
            .ok_or_else(|| format!("ambiguous local time {s}"));
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let date = if end_of_day {
            date + Duration::days(1)
        } else {
            date
        };
        return Ok(local_midnight(tz, date));
    }
    Err(format!(
        "cannot parse date {s:?}; use YYYY-MM-DD or RFC 3339"
    ))
}

/// Resolve a named range, an `Nd` range, or explicit `from`/`to` bounds into a
/// `(start, end)` UTC pair.
///
/// - `today` / `tomorrow`: local midnight to the next local midnight.
/// - `this_week` / `next_week`: Monday 00:00 local to the following Monday.
/// - `next`: `now` to `now + 14 days` (callers take the first matching event).
/// - `Nd` (e.g. `3d`): `now - N days` to `now` when `dir` is `Past` (mail), or
///   `now` to `now + N days` when `dir` is `Future` (calendar).
/// - `from`/`to`: ISO 8601 date or datetime, naive values interpreted in `tz`;
///   the end date is inclusive of its whole day.
pub fn parse_range(
    range: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    tz: Tz,
    now: DateTime<Utc>,
    dir: Direction,
) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    if from.is_some() || to.is_some() {
        let a = from
            .map(|s| parse_point(s, tz, false))
            .transpose()?
            .unwrap_or(now);
        let b = to
            .map(|s| parse_point(s, tz, true))
            .transpose()?
            .unwrap_or(a + Duration::days(7));
        return if a < b {
            Ok((a, b))
        } else {
            Err("from must be before to".into())
        };
    }
    let today = now.with_timezone(&tz).date_naive();
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let days = |d: NaiveDate, n: i64| local_midnight(tz, d + Duration::days(n));
    match range.unwrap_or("today") {
        "today" => Ok((days(today, 0), days(today, 1))),
        "tomorrow" => Ok((days(today, 1), days(today, 2))),
        "this_week" => Ok((days(monday, 0), days(monday, 7))),
        "next_week" => Ok((days(monday, 7), days(monday, 14))),
        "next" => Ok((now, now + Duration::days(14))),
        other => {
            let n: i64 = other
                .strip_suffix('d')
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| {
                    format!(
                        "unknown range {other:?}; use today, tomorrow, this_week, next_week, next, Nd, or from/to"
                    )
                })?;
            Ok(match dir {
                Direction::Past => (now - Duration::days(n), now),
                Direction::Future => (now, now + Duration::days(n)),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        chrono_tz::Europe::Stockholm
            .with_ymd_and_hms(2026, 9, 23, 10, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }
    fn tz() -> chrono_tz::Tz {
        chrono_tz::Europe::Stockholm
    }
    fn local(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        tz().with_ymd_and_hms(y, m, d, h, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn today_is_local_midnight_to_midnight() {
        let (a, b) =
            parse_range(Some("today"), None, None, tz(), now(), Direction::Future).unwrap();
        assert_eq!((a, b), (local(2026, 9, 23, 0), local(2026, 9, 24, 0)));
    }
    #[test]
    fn weeks_start_monday() {
        let (a, b) = parse_range(
            Some("this_week"),
            None,
            None,
            tz(),
            now(),
            Direction::Future,
        )
        .unwrap();
        assert_eq!((a, b), (local(2026, 9, 21, 0), local(2026, 9, 28, 0)));
        let (a, b) = parse_range(
            Some("next_week"),
            None,
            None,
            tz(),
            now(),
            Direction::Future,
        )
        .unwrap();
        assert_eq!((a, b), (local(2026, 9, 28, 0), local(2026, 10, 5, 0)));
    }
    #[test]
    fn days_look_back_for_mail_and_forward_for_calendar() {
        let (a, b) = parse_range(Some("3d"), None, None, tz(), now(), Direction::Past).unwrap();
        assert_eq!((a, b), (now() - chrono::Duration::days(3), now()));
        let (a, b) = parse_range(Some("3d"), None, None, tz(), now(), Direction::Future).unwrap();
        assert_eq!((a, b), (now(), now() + chrono::Duration::days(3)));
    }
    #[test]
    fn explicit_dates_are_local_and_inclusive_of_the_end_day() {
        let (a, b) = parse_range(
            None,
            Some("2026-10-01"),
            Some("2026-10-02"),
            tz(),
            now(),
            Direction::Future,
        )
        .unwrap();
        assert_eq!((a, b), (local(2026, 10, 1, 0), local(2026, 10, 3, 0)));
    }
    #[test]
    fn unknown_range_is_an_error() {
        assert!(
            parse_range(
                Some("fortnight"),
                None,
                None,
                tz(),
                now(),
                Direction::Future
            )
            .is_err()
        );
    }
}
