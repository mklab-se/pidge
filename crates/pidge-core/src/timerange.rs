//! Time-range parsing in the user's local timezone.
//!
//! Mail ranges look backward from "now" (`Direction::Past`); calendar ranges look
//! forward (`Direction::Future`). Named ranges (`today`, `this_week`, ...) and
//! explicit `from`/`to` bounds are always resolved against the caller-supplied
//! IANA timezone, then converted to UTC.

use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// Whether a bare `Nd` range (e.g. `3d`) looks backward from `now` (mail) or
/// forward from `now` (calendar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Past,
    Future,
}

/// Longest `Nd` range accepted: ten years of days.
pub const MAX_RANGE_DAYS: u32 = 3650;
/// Longest explicit `from`/`to` span accepted: ten years (with leap days).
pub const MAX_SPAN_DAYS: i64 = 3653;

fn out_of_range(s: &str) -> String {
    format!("date {s:?} is out of range")
}

/// `t` moved by `days` (negative: back), or a usage error instead of the
/// overflow panic `DateTime ± Duration` would raise.
fn shift(t: DateTime<Utc>, days: i64) -> Result<DateTime<Utc>, String> {
    Duration::try_days(days)
        .and_then(|d| t.checked_add_signed(d))
        .ok_or_else(|| "that range reaches past the supported dates".to_string())
}

fn local_midnight(tz: Tz, date: NaiveDate) -> Result<DateTime<Utc>, String> {
    tz.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .ok_or_else(|| {
            format!(
                "local midnight on {date} does not exist in {tz} (DST gap); use an explicit RFC 3339 time"
            )
        })
}

/// Parse a single timestamp: RFC 3339, a naive `YYYY-MM-DDTHH:MM:SS` or
/// `YYYY-MM-DDTHH:MM` (interpreted in `tz`), or a bare `YYYY-MM-DD` date (interpreted as local midnight, or the
/// following local midnight when `end_of_day` is set, making the date inclusive).
pub fn parse_point(s: &str, tz: Tz, end_of_day: bool) -> Result<DateTime<Utc>, String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"));
    if let Ok(naive) = naive {
        return tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|d| d.with_timezone(&Utc))
            .ok_or_else(|| {
                format!(
                    "local time {s} does not exist in {tz} (DST gap); use an explicit RFC 3339 time"
                )
            });
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let date = if end_of_day {
            date.succ_opt().ok_or_else(|| out_of_range(s))?
        } else {
            date
        };
        return local_midnight(tz, date);
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
/// - `Nd` (e.g. `3d`, N at most [`MAX_RANGE_DAYS`]): `now - N days` to
///   `now` when `dir` is `Past` (mail), or `now` to `now + N days` when
///   `dir` is `Future` (calendar).
/// - `from`/`to`: ISO 8601 date or datetime, naive values interpreted in `tz`;
///   the end date is inclusive of its whole day. At most
///   [`MAX_SPAN_DAYS`] apart.
///
/// Out-of-range input (huge `N`, dates at the edge of the calendar) is a
/// usage error, never a panic.
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
        let b = match to {
            Some(s) => parse_point(s, tz, true)?,
            None => shift(a, 7)?,
        };
        if a >= b {
            return Err("from must be before to".into());
        }
        if b - a > Duration::days(MAX_SPAN_DAYS) {
            return Err("from and to may be at most 10 years apart".into());
        }
        return Ok((a, b));
    }
    let today = now.with_timezone(&tz).date_naive();
    let monday = today
        .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
        .ok_or_else(|| out_of_range(&today.to_string()))?;
    let days = |d: NaiveDate, n: u64| {
        d.checked_add_days(Days::new(n))
            .ok_or_else(|| out_of_range(&d.to_string()))
            .and_then(|d| local_midnight(tz, d))
    };
    match range.unwrap_or("today") {
        "today" => Ok((days(today, 0)?, days(today, 1)?)),
        "tomorrow" => Ok((days(today, 1)?, days(today, 2)?)),
        "this_week" => Ok((days(monday, 0)?, days(monday, 7)?)),
        "next_week" => Ok((days(monday, 7)?, days(monday, 14)?)),
        "next" => Ok((now, shift(now, 14)?)),
        other => {
            let n: u32 = other
                .strip_suffix('d')
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| {
                    format!(
                        "unknown range {other:?}; use today, tomorrow, this_week, next_week, next, Nd, or from/to"
                    )
                })?;
            if n > MAX_RANGE_DAYS {
                return Err(format!(
                    "range {other:?} is too long; use at most {MAX_RANGE_DAYS}d"
                ));
            }
            Ok(match dir {
                Direction::Past => (shift(now, -i64::from(n))?, now),
                Direction::Future => (now, shift(now, n.into())?),
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
    fn day_ranges_are_capped_at_ten_years_without_panicking() {
        for dir in [Direction::Past, Direction::Future] {
            assert!(parse_range(Some("3650d"), None, None, tz(), now(), dir).is_ok());
            let err = parse_range(Some("3651d"), None, None, tz(), now(), dir).unwrap_err();
            assert!(err.contains("at most 3650d"), "{err}");
            for huge in [
                "4294967295d",
                "99999999999999999999d",
                "9223372036854775807d",
                "-3d",
            ] {
                assert!(
                    parse_range(Some(huge), None, None, tz(), now(), dir).is_err(),
                    "{huge}"
                );
            }
        }
    }

    #[test]
    fn explicit_ranges_are_capped_and_calendar_edges_do_not_panic() {
        let span = |from: Option<&str>, to: Option<&str>| {
            parse_range(None, from, to, tz(), now(), Direction::Future)
        };
        assert!(span(Some("2026-01-01"), Some("2035-12-31")).is_ok());
        let err = span(Some("2020-01-01"), Some("2031-01-01")).unwrap_err();
        assert!(err.contains("10 years"), "{err}");
        // The last date chrono knows has no following midnight; a `from`
        // there has no "a week later". Both are errors, not panics.
        assert!(span(Some("2026-01-01"), Some("+262143-12-31")).is_err());
        assert!(span(Some("+262143-12-31T12:00:00Z"), None).is_err());
        assert!(span(Some("-262144-01-01"), Some("2026-01-01")).is_err());
        assert!(parse_point("+262143-12-31", tz(), true).is_err());
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
    #[test]
    fn today_errors_instead_of_panicking_across_a_dst_gap() {
        // America/Sao_Paulo sprang forward at 2018-11-04T00:00:00, so local
        // midnight on that date does not exist. 15:00 local that same day is a
        // valid, unambiguous moment we can use as `now`.
        let dst_gap_day_tz = chrono_tz::America::Sao_Paulo;
        let now_on_gap_day = dst_gap_day_tz
            .with_ymd_and_hms(2018, 11, 4, 15, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let result = parse_range(
            Some("today"),
            None,
            None,
            dst_gap_day_tz,
            now_on_gap_day,
            Direction::Future,
        );
        assert!(result.is_err());
    }
    #[test]
    fn parse_point_accepts_a_local_time_without_seconds() {
        let tz = chrono_tz::Europe::Stockholm;
        let expected = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
        assert_eq!(
            parse_point("2026-09-24T14:00", tz, false).unwrap(),
            expected
        );
        assert_eq!(
            parse_point("2026-09-24T14:00", tz, false),
            parse_point("2026-09-24T14:00:00", tz, false)
        );
    }

    #[test]
    fn parse_point_errors_on_a_nonexistent_local_time() {
        let err =
            parse_point("2018-11-04T00:30:00", chrono_tz::America::Sao_Paulo, false).unwrap_err();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist' in error, got: {err}"
        );
    }
}
