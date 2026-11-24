//! Finding dates and times in mail, in Swedish and English: "torsdag 26
//! november kl 17-19", "Nov 26, 2026 at 5 pm", "i morgon 15.00". Rules only:
//! what is found is in the text, nothing is guessed.

use std::sync::LazyLock;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use pidge_store::Happening;
use regex::{Captures, Regex};

/// A time found in a text, and the words around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub when: Happening,
    pub words: String,
}

const MONTHS: &[(&str, u32)] = &[
    ("januari", 1),
    ("january", 1),
    ("jan", 1),
    ("februari", 2),
    ("february", 2),
    ("feb", 2),
    ("mars", 3),
    ("march", 3),
    ("mar", 3),
    ("april", 4),
    ("apr", 4),
    ("maj", 5),
    ("may", 5),
    ("juni", 6),
    ("june", 6),
    ("jun", 6),
    ("juli", 7),
    ("july", 7),
    ("jul", 7),
    ("augusti", 8),
    ("august", 8),
    ("aug", 8),
    ("september", 9),
    ("sept", 9),
    ("sep", 9),
    ("oktober", 10),
    ("october", 10),
    ("okt", 10),
    ("oct", 10),
    ("november", 11),
    ("nov", 11),
    ("december", 12),
    ("dec", 12),
];

const DAYS: &[(&str, Weekday)] = &[
    ("måndag", Weekday::Mon),
    ("monday", Weekday::Mon),
    ("tisdag", Weekday::Tue),
    ("tuesday", Weekday::Tue),
    ("onsdag", Weekday::Wed),
    ("wednesday", Weekday::Wed),
    ("torsdag", Weekday::Thu),
    ("thursday", Weekday::Thu),
    ("fredag", Weekday::Fri),
    ("friday", Weekday::Fri),
    ("lördag", Weekday::Sat),
    ("saturday", Weekday::Sat),
    ("söndag", Weekday::Sun),
    ("sunday", Weekday::Sun),
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

fn month_words() -> String {
    MONTHS.iter().map(|(m, _)| *m).collect::<Vec<_>>().join("|")
}

static ISO: LazyLock<Regex> = LazyLock::new(|| re(r"\b(20\d\d)-(\d{1,2})-(\d{1,2})\b"));
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b(\d{{1,2}})(?:\.|:e|e|st|nd|rd|th)?\s+(?:of\s+)?({})\b\.?(?:,?\s+(20\d\d)\b)?",
        month_words()
    ))
});
static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b({})\b\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s+(20\d\d)\b)?",
        month_words()
    ))
});
/// `26/11` says a date only next to a time or a weekday.
static SLASH: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})/(\d{1,2})(?:[/-](20\d\d))?\b"));
static DAY_WORD: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b({}|idag|i dag|ikväll|i kväll|today|tonight|imorgon|i morgon|tomorrow)\b",
        DAYS.iter().map(|(d, _)| *d).collect::<Vec<_>>().join("|")
    ))
});

const DASH: &str = "(?:-|\u{2013}|\u{2014}|till|to|och|and|until|til)";

/// `17-19`, `17.00–19.00`, `5 pm to 7 pm`, with a word or minutes that make
/// it a time.
static TIME_RANGE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)(kl\.?|klockan|at|@|från|from|mellan|between)?\s*\b(\d{{1,2}})(?:[:.](\d{{2}}))?\s*(am|pm)?\s*{DASH}\s*(?:kl\.?\s*)?(\d{{1,2}})(?:[:.](\d{{2}}))?\s*(am|pm)?\b"
    ))
});
static TIME: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(kl\.?|klockan|at|@)?\s*\b(\d{1,2})(?:[:.](\d{2}))?\s*(am|pm)?\b"));

fn month(word: &str) -> Option<u32> {
    let w = word.to_lowercase();
    MONTHS.iter().find(|(m, _)| *m == w).map(|(_, n)| *n)
}

fn clock(hour: &str, minute: Option<&str>, half: Option<&str>) -> Option<NaiveTime> {
    let mut h: u32 = hour.parse().ok()?;
    let m: u32 = minute.map_or(Some(0), |m| m.parse().ok())?;
    match half.map(str::to_lowercase).as_deref() {
        Some("pm") if h < 12 => h += 12,
        Some("am") if h == 12 => h = 0,
        _ => {}
    }
    NaiveTime::from_hms_opt(h, m, 0)
}

type Times = (NaiveTime, Option<NaiveTime>);

/// `12-17` right after a date is a time though nothing says so.
static BARE_RANGE: LazyLock<Regex> =
    LazyLock::new(|| re("^[\\s,:]{0,4}(\\d{1,2})\\s*[-\u{2013}]\\s*(\\d{1,2})\\b(?:[^\\d:.-]|$)"));

fn bare_range(after: &str) -> Option<Times> {
    let c = BARE_RANGE.captures(after)?;
    let (a, b) = (clock(&c[1], None, None)?, clock(&c[2], None, None)?);
    (b > a).then_some((a, Some(b)))
}

/// A time range or a time in a short stretch of text.
fn times(text: &str) -> Option<Times> {
    for c in TIME_RANGE.captures_iter(text) {
        let g = |n: usize| c.get(n).map(|m| m.as_str());
        let spoken = g(1).is_some() || g(3).is_some() || g(6).is_some();
        let half = g(4).or(g(7));
        if !spoken && half.is_none() {
            continue;
        }
        let (Some(a), Some(b)) = (clock(g(2)?, g(3), half), clock(g(5)?, g(6), g(7).or(g(4))))
        else {
            continue;
        };
        if b > a {
            return Some((a, Some(b)));
        }
    }
    for c in TIME.captures_iter(text) {
        let g = |n: usize| c.get(n).map(|m| m.as_str());
        if g(1).is_none() && g(3).is_none() && g(4).is_none() {
            continue;
        }
        if let Some(t) = clock(g(2)?, g(3), g(4)) {
            return Some((t, None));
        }
    }
    None
}

/// The text around `[from, to)`, cut on character boundaries.
fn around(text: &str, from: usize, to: usize, before: usize, after: usize) -> (&str, &str) {
    let start = text[..from]
        .char_indices()
        .rev()
        .nth(before.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    let end = text[to..]
        .char_indices()
        .nth(after)
        .map_or(text.len(), |(i, _)| to + i);
    (&text[start..from], &text[to..end])
}

/// The end of `before` that belongs to the same sentence. A dot between
/// digits is part of a time.
fn same_sentence(before: &str) -> &str {
    let bytes = before.as_bytes();
    let cut = before
        .char_indices()
        .rev()
        .find(|(i, c)| match c {
            '\n' | '!' | '?' => true,
            '.' => !bytes.get(i + 1).is_some_and(u8::is_ascii_digit),
            _ => false,
        })
        .map_or(0, |(i, _)| i + 1);
    &before[cut..]
}

/// The day a date without a year means: the first one not before the mail.
fn upcoming(month: u32, day: u32, received: NaiveDate) -> Option<NaiveDate> {
    let this = NaiveDate::from_ymd_opt(received.year(), month, day)?;
    if this >= received {
        return Some(this);
    }
    // "3 January" in a November mail is next year; "3 November" is past.
    let next = NaiveDate::from_ymd_opt(received.year() + 1, month, day)?;
    ((next - received).num_days() < 240).then_some(next)
}

fn day_word(word: &str, received: NaiveDate) -> Option<NaiveDate> {
    let w = word.to_lowercase();
    match w.as_str() {
        "idag" | "i dag" | "today" | "ikväll" | "i kväll" | "tonight" => Some(received),
        "imorgon" | "i morgon" | "tomorrow" => Some(received + Duration::days(1)),
        _ => {
            let wd = DAYS.iter().find(|(d, _)| *d == w)?.1;
            let mut d = received + Duration::days(1);
            while d.weekday() != wd {
                d += Duration::days(1);
            }
            Some(d)
        }
    }
}

/// The dates and times in `text` from the day it was received and on, in the
/// order they appear, at most six.
pub fn find(text: &str, received: DateTime<Utc>, tz: Tz) -> Vec<Found> {
    let day = received.with_timezone(&tz).date_naive();
    // (position, end, date, needs a time to count)
    let mut dates: Vec<(usize, usize, NaiveDate, bool)> = Vec::new();
    let mut add = |c: &Captures<'_>, date: Option<NaiveDate>, needs_time: bool| {
        if let (Some(m), Some(d)) = (c.get(0), date)
            && d >= day
            && (d - day).num_days() <= 400
        {
            dates.push((m.start(), m.end(), d, needs_time));
        }
    };
    let num = |c: &Captures<'_>, n: usize| -> Option<u32> { c.get(n)?.as_str().parse().ok() };
    for c in ISO.captures_iter(text) {
        let d = (|| {
            NaiveDate::from_ymd_opt(i32::try_from(num(&c, 1)?).ok()?, num(&c, 2)?, num(&c, 3)?)
        })();
        add(&c, d, false);
    }
    let of = |m: Option<u32>, d: Option<u32>, y: Option<u32>| -> Option<NaiveDate> {
        match y {
            Some(y) => NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m?, d?),
            None => upcoming(m?, d?, day),
        }
    };
    for c in DAY_MONTH.captures_iter(text) {
        let m = c.get(2).and_then(|m| month(m.as_str()));
        let d = of(m, num(&c, 1), num(&c, 3));
        add(&c, d, false);
    }
    for c in MONTH_DAY.captures_iter(text) {
        let m = c.get(1).and_then(|m| month(m.as_str()));
        let d = of(m, num(&c, 2), num(&c, 3));
        add(&c, d, false);
    }
    for c in SLASH.captures_iter(text) {
        let d = of(num(&c, 2), num(&c, 1), num(&c, 3));
        add(&c, d, true);
    }
    for c in DAY_WORD.captures_iter(text) {
        let d = c.get(1).and_then(|m| day_word(m.as_str(), day));
        add(&c, d, true);
    }
    dates.sort_by_key(|d| (d.0, std::cmp::Reverse(d.1)));
    let mut out: Vec<Found> = Vec::new();
    let mut last_end = 0;
    for (n, (from, to, date, needs_time)) in dates.iter().copied().enumerate() {
        if from < last_end {
            continue;
        }
        // A weekday right before a date is that date.
        if needs_time
            && dates
                .get(n + 1)
                .is_some_and(|next| !next.3 && next.0 >= to && text[to..next.0].trim().len() <= 4)
        {
            continue;
        }
        let (before, after) = around(text, from, to, 25, 80);
        // What follows belongs to this date only up to the next one.
        let after_limit = dates
            .iter()
            .map(|d| d.0)
            .filter(|s| *s >= to)
            .min()
            .map_or(after, |s| {
                if s < to + after.len() {
                    &text[to..s]
                } else {
                    after
                }
            });
        let t = bare_range(after_limit)
            .or_else(|| times(after_limit))
            .or_else(|| times(same_sentence(before)));
        if needs_time && t.is_none() {
            continue;
        }
        let at = |t: NaiveTime| {
            tz.from_local_datetime(&date.and_time(t))
                .earliest()
                .map(|d| d.with_timezone(&Utc))
        };
        let when = match t {
            Some((a, b)) => match at(a) {
                Some(start) => Happening {
                    start,
                    end: b.and_then(at),
                    all_day: false,
                },
                None => continue,
            },
            None => match at(NaiveTime::MIN) {
                Some(start) => Happening {
                    start,
                    end: None,
                    all_day: true,
                },
                None => continue,
            },
        };
        last_end = to;
        if out.iter().any(|f| f.when.start == when.start) {
            continue;
        }
        out.push(Found {
            when,
            words: crate::render::one_line(&format!("{before}{}{after}", &text[from..to])),
        });
        if out.len() == 6 {
            break;
        }
    }
    out
}

/// How a time is shown: `Thu 26 Nov 17:00-19:00`.
pub fn words(h: &Happening, tz: Tz) -> String {
    let s = h.start.with_timezone(&tz);
    if h.all_day {
        return s.format("%a %-d %b").to_string();
    }
    match h.end {
        Some(e) => format!(
            "{}-{}",
            s.format("%a %-d %b %H:%M"),
            e.with_timezone(&tz).format("%H:%M")
        ),
        None => s.format("%a %-d %b %H:%M").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = chrono_tz::Europe::Stockholm;

    /// Tuesday 24 November 2026.
    fn received() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-24T09:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn found(text: &str) -> Vec<String> {
        find(text, received(), TZ)
            .iter()
            .map(|f| words(&f.when, TZ))
            .collect()
    }

    #[test]
    fn swedish_dates_with_times() {
        assert_eq!(
            found("Ringen kan hämtas torsdag 26 november kl 17-19 i butiken."),
            ["Thu 26 Nov 17:00-19:00"]
        );
        assert_eq!(
            found("Välkommen den 4:e december klockan 15.30!"),
            ["Fri 4 Dec 15:30"]
        );
        assert_eq!(found("Vi ses i morgon kl. 9.15"), ["Wed 25 Nov 09:15"]);
        assert_eq!(found("Funkar det på fredag 14:00?"), ["Fri 27 Nov 14:00"]);
        assert_eq!(found("tors 26/11 kl 17.00"), ["Thu 26 Nov 17:00"]);
        assert_eq!(
            found("Mellan 17.00 och 19.00 den 26 november"),
            ["Thu 26 Nov 17:00-19:00"]
        );
    }

    #[test]
    fn english_dates_with_times() {
        assert_eq!(
            found("Your appointment is on Thursday, November 26, 2026 at 5 pm"),
            ["Thu 26 Nov 17:00"]
        );
        assert_eq!(
            found("Dinner 3rd of December, 6:30 pm - 9 pm"),
            ["Thu 3 Dec 18:30-21:00"]
        );
        assert_eq!(found("tomorrow at 3pm"), ["Wed 25 Nov 15:00"]);
        assert_eq!(found("Start: 2026-12-01 08:00"), ["Tue 1 Dec 08:00"]);
    }

    #[test]
    fn a_date_without_a_time_is_a_whole_day() {
        assert_eq!(found("Bytet gäller från 1 december."), ["Tue 1 Dec"]);
    }

    #[test]
    fn past_dates_and_bare_numbers_are_not_times() {
        assert!(found("Er order från 3 november").is_empty());
        assert!(found("Faktura 2026-10-01").is_empty());
        assert!(found("Plats 154, 3 av 4, se sida 12").is_empty());
        assert!(found("Vi hörs på måndag").is_empty());
        assert!(found("1/2 liter grädde").is_empty());
        assert!(found("").is_empty());
    }

    #[test]
    fn a_date_early_next_year_is_next_year() {
        let f = find("7 januari kl 10", received(), TZ);
        assert_eq!(f[0].when.start.year(), 2027);
    }

    #[test]
    fn confirmations_with_the_time_on_its_own_line() {
        assert_eq!(
            found(
                "Datum: fredag 27 november 2026\nNamn: Jane Doe ,\nTid: 17:45 - 19:45\nAntal gäster: 2"
            ),
            ["Fri 27 Nov 17:45-19:45"]
        );
        assert_eq!(
            found("Saturday, November 28\n12-17\nWe can't wait!"),
            ["Sat 28 Nov 12:00-17:00"]
        );
        assert_eq!(
            found("When\nWednesday, November 25, 2026 17:30 CET (30 minutes)"),
            ["Wed 25 Nov 17:30"]
        );
        assert_eq!(
            found("Tid: den 10 december 14:20 Resurs: Jane Doe"),
            ["Thu 10 Dec 14:20"]
        );
        assert_eq!(
            found("måndag och tisdag den 21\u{2013}22 december."),
            ["Tue 22 Dec"]
        );
        assert_eq!(
            found(
                "från 2026-12-01 blir 154 ledigt, svara senast torsdag 2026-11-26. Vxl 08-617 75 00"
            ),
            ["Tue 1 Dec", "Thu 26 Nov"]
        );
    }

    #[test]
    fn several_dates_keep_their_own_times() {
        assert_eq!(
            found("Tisdag 1 december kl 10.00 eller onsdag 2 december kl 14.00"),
            ["Tue 1 Dec 10:00", "Wed 2 Dec 14:00"]
        );
    }

    #[test]
    fn text_of_any_kind_is_safe() {
        for t in [
            "åäö 🎂 26 november 🎂🎂🎂🎂🎂🎂🎂🎂🎂🎂🎂🎂",
            "99/99 99:99 kl 77",
            "31 februari kl 10",
            "kl",
        ] {
            let _ = find(t, received(), TZ);
        }
    }
}
