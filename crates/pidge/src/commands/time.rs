//! Human time-string parsing for calendar commands.
//!
//! Accepted forms (all interpreted in `tz` unless the input itself carries
//! an explicit offset):
//!
//! - ISO with offset: `2026-05-22T15:00+02:00`, `2026-05-22T13:00Z`
//! - Local ISO:       `2026-05-22T15:00`
//! - Date only:       `2026-05-22` (00:00 local)
//! - Time only:       `15:00` (today, local; if past, tomorrow)
//! - Weekday:         `mon 09:00`, `next tue 14:00`, `tomorrow 15:00`
//! - Relative offset: `+2h`, `+30m`, `+1d` (only when `relative_to` is set)

use anyhow::{Result, anyhow};
use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;

pub fn parse_when(
    input: &str,
    tz: &Tz,
    now: DateTime<Utc>,
    relative_to: Option<DateTime<Utc>>,
) -> Result<DateTime<Utc>> {
    let s = input.trim();
    if s.is_empty() {
        return Err(anyhow!("empty time string"));
    }

    if let Some(stripped) = s.strip_prefix('+') {
        let base = relative_to.ok_or_else(|| {
            anyhow!("relative offset '{s}' is only valid for --end (relative to --start)")
        })?;
        return parse_offset(stripped, base);
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.to_utc());
    }

    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return local_to_utc(ndt, tz, s);
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M") {
        return local_to_utc(ndt, tz, s);
    }

    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d.and_hms_opt(0, 0, 0).unwrap();
        return local_to_utc(ndt, tz, s);
    }

    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        return Ok(at_time_today_or_tomorrow(t, tz, now));
    }

    if let Some(dt) = try_parse_phrase(s, tz, now)? {
        return Ok(dt);
    }

    Err(anyhow!(
        "could not parse '{s}' as a time. Accepted forms: ISO \
         (2026-05-22T15:00 or 2026-05-22T15:00+02:00), date (2026-05-22), \
         time (15:00), 'tomorrow 15:00', 'next mon 09:00', or '+2h' (only on --end)"
    ))
}

fn local_to_utc(ndt: NaiveDateTime, tz: &Tz, original: &str) -> Result<DateTime<Utc>> {
    tz.from_local_datetime(&ndt)
        .single()
        .map(|d| d.with_timezone(&Utc))
        .ok_or_else(|| anyhow!("ambiguous or invalid local time {original} in {tz}"))
}

fn parse_offset(spec: &str, base: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = spec.trim();
    if s.is_empty() {
        return Err(anyhow!("empty offset"));
    }
    let last = s.chars().last().unwrap();
    let num_part = &s[..s.len() - last.len_utf8()];
    let n: i64 = num_part
        .parse()
        .map_err(|_| anyhow!("bad offset number in '+{spec}'"))?;
    let dur = match last {
        'm' => Duration::minutes(n),
        'h' => Duration::hours(n),
        'd' => Duration::days(n),
        _ => return Err(anyhow!("offset unit must be m, h, or d (got '{last}')")),
    };
    Ok(base + dur)
}

fn at_time_today_or_tomorrow(t: NaiveTime, tz: &Tz, now: DateTime<Utc>) -> DateTime<Utc> {
    let local_now = now.with_timezone(tz);
    let candidate = local_now.date_naive().and_time(t);
    let today_utc = tz
        .from_local_datetime(&candidate)
        .single()
        .map(|d| d.with_timezone(&Utc));
    match today_utc {
        Some(dt) if dt > now => dt,
        _ => {
            let tomorrow = candidate + Duration::days(1);
            tz.from_local_datetime(&tomorrow)
                .single()
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or(now + Duration::days(1))
        }
    }
}

fn try_parse_phrase(s: &str, tz: &Tz, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    let lower = s.to_lowercase();
    let Some((day_part, time_part)) = lower.rsplit_once(' ') else {
        return Ok(None);
    };
    let Some(t) = parse_loose_time(time_part.trim()) else {
        return Ok(None);
    };

    let local_now = now.with_timezone(tz);
    let target_date = match day_part.trim() {
        "today" => local_now.date_naive(),
        "tomorrow" => local_now.date_naive() + Duration::days(1),
        other => {
            let Some((weekday, is_next)) = parse_weekday_phrase(other) else {
                return Ok(None);
            };
            advance_to_weekday(local_now.date_naive(), weekday, is_next)
        }
    };
    let ndt = target_date.and_time(t);
    Ok(tz
        .from_local_datetime(&ndt)
        .single()
        .map(|d| d.with_timezone(&Utc)))
}

fn parse_loose_time(s: &str) -> Option<NaiveTime> {
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        return Some(t);
    }
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M:%S") {
        return Some(t);
    }
    if let Ok(t) = NaiveTime::parse_from_str(s, "%I:%M%p") {
        return Some(t);
    }
    if let Ok(t) = NaiveTime::parse_from_str(s, "%I%p") {
        return Some(t);
    }
    None
}

fn parse_weekday_phrase(s: &str) -> Option<(Weekday, bool)> {
    let (rest, is_next) = match s.strip_prefix("next ") {
        Some(r) => (r, true),
        None => (s, false),
    };
    let w = match rest.trim() {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tuesday" => Weekday::Tue,
        "wed" | "wednesday" => Weekday::Wed,
        "thu" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    };
    Some((w, is_next))
}

fn advance_to_weekday(from: NaiveDate, target: Weekday, force_next: bool) -> NaiveDate {
    let cur = from.weekday();
    let mut days =
        (7 + target.num_days_from_monday() as i64 - cur.num_days_from_monday() as i64) % 7;
    if days == 0 {
        days = 7;
    } else if force_next {
        days += 7;
    }
    from + Duration::days(days)
}

/// Format a UTC instant in the given IANA TZ for human display.
/// Returns "Fri 22 May 15:00 Europe/Stockholm" style.
pub fn format_when(dt: DateTime<Utc>, tz: &Tz) -> String {
    let local = dt.with_timezone(tz);
    format!("{} {}", local.format("%a %d %b %H:%M"), tz.name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Stockholm;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .to_utc()
    }

    #[test]
    fn parses_rfc3339_with_offset() {
        let dt = parse_when("2026-05-22T15:00:00+02:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-22T13:00:00+00:00");
    }

    #[test]
    fn parses_zulu_iso() {
        let dt = parse_when("2026-05-22T13:00:00Z", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-22T13:00:00+00:00");
    }

    #[test]
    fn parses_local_iso_using_tz() {
        // Stockholm is +02:00 in May (CEST)
        let dt = parse_when("2026-05-22T15:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-22T13:00:00+00:00");
    }

    #[test]
    fn parses_date_only_as_midnight_local() {
        let dt = parse_when("2026-05-22", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-21T22:00:00+00:00");
    }

    #[test]
    fn time_only_today_when_future() {
        // now = 12:00Z = 14:00 Stockholm; ask for 18:00 → today
        let dt = parse_when("18:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-20T16:00:00+00:00");
    }

    #[test]
    fn time_only_tomorrow_when_past() {
        // 10:00 local = 08:00Z, already past 12:00Z so → tomorrow
        let dt = parse_when("10:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-21T08:00:00+00:00");
    }

    #[test]
    fn tomorrow_phrase() {
        let dt = parse_when("tomorrow 15:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-21T13:00:00+00:00");
    }

    #[test]
    fn weekday_phrase_advances_to_next_occurrence() {
        // 2026-05-20 is a Wednesday; ask for "fri 09:00"
        let dt = parse_when("fri 09:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-22T07:00:00+00:00");
    }

    #[test]
    fn next_weekday_skips_current_week() {
        let dt = parse_when("next wed 10:00", &Stockholm, now(), None).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-27T08:00:00+00:00");
    }

    #[test]
    fn offset_only_with_relative_to() {
        let start = DateTime::parse_from_rfc3339("2026-05-22T13:00:00Z")
            .unwrap()
            .to_utc();
        let dt = parse_when("+90m", &Stockholm, now(), Some(start)).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-22T14:30:00+00:00");
    }

    #[test]
    fn offset_hour_unit() {
        let start = DateTime::parse_from_rfc3339("2026-05-22T13:00:00Z")
            .unwrap()
            .to_utc();
        let dt = parse_when("+2h", &Stockholm, now(), Some(start)).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-22T15:00:00+00:00");
    }

    #[test]
    fn offset_day_unit() {
        let start = DateTime::parse_from_rfc3339("2026-05-22T13:00:00Z")
            .unwrap()
            .to_utc();
        let dt = parse_when("+1d", &Stockholm, now(), Some(start)).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-23T13:00:00+00:00");
    }

    #[test]
    fn offset_without_relative_to_errors() {
        assert!(parse_when("+2h", &Stockholm, now(), None).is_err());
    }

    #[test]
    fn unrecognized_input_errors() {
        let err = parse_when("definitely not a time", &Stockholm, now(), None).unwrap_err();
        assert!(err.to_string().contains("could not parse"));
    }

    #[test]
    fn format_when_includes_tz_label() {
        let dt = DateTime::parse_from_rfc3339("2026-05-22T13:00:00Z")
            .unwrap()
            .to_utc();
        let s = format_when(dt, &Stockholm);
        assert!(s.contains("15:00"));
        assert!(s.contains("Stockholm"));
    }
}
