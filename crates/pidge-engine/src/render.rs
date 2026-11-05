//! Text shapes every frontend shares: the untrusted-content wrapper, one-line
//! header fields, caps, times, and the item and event lines. Compact on
//! purpose: tool output is read by models, and every character is a token.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use pidge_core::{Event, MessageFrom, ResponseStatus};

const TAG: &str = "untrusted-email-content";

/// Wraps third-party text in an explicit untrusted block. Any spelling of
/// the tag name inside `text` (any case) is defused, so the content can
/// neither close the block early nor open a fake one.
pub fn untrusted(text: &str) -> String {
    format!("<{TAG}>\n{}\n</{TAG}>", defuse_tag(text))
}

fn defuse_tag(text: &str) -> String {
    // ASCII lower-casing keeps byte offsets, so matches index `text` too.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (at, _) in lower.match_indices(TAG) {
        out.push_str(&text[last..at]);
        out.push_str("untrusted_email_content");
        last = at + TAG.len();
    }
    out.push_str(&text[last..]);
    out
}

/// At most `max_chars` characters of `text`, with a marker when cut.
pub fn cap(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}\n[... truncated ...]", &text[..cut]),
        None => text.to_string(),
    }
}

/// Like [`cap`], but keeps the result on one line.
pub fn cap_inline(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// Invisible characters marketing mail pads its preheader with. The
/// zero-width joiner stays: emoji sequences need it.
fn is_filler(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2060}'
            | '\u{2800}'
            | '\u{FEFF}'
    )
}

/// A header field on one line: control characters and whitespace runs
/// collapse to single spaces, so third-party text cannot start a line of its
/// own.
pub fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control() || is_filler(c))
        // A word of nothing but joiners is padding too.
        .filter(|w| w.chars().any(|c| c != '\u{200D}'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A third-party name for inline use: one line, at most 120 characters.
pub fn inline(text: &str) -> String {
    cap_inline(&one_line(text), 120)
}

/// A message body with filler removed and blank runs collapsed.
pub fn clean_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for line in text.lines() {
        let line: String = line.chars().filter(|&c| !is_filler(c)).collect();
        let line = line.trim_end();
        if line.is_empty() {
            if !blank && !out.is_empty() {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    out.trim_end().to_string()
}

/// The sender as a person would say it: the name, or the address.
pub fn name_of(r: &MessageFrom) -> String {
    let name = one_line(&r.name);
    if name.is_empty() {
        one_line(&r.address)
    } else {
        cap_inline(&name, 60)
    }
}

/// `Name <addr>`, or whichever is present.
pub fn who(r: &MessageFrom) -> String {
    match (one_line(&r.name).as_str(), one_line(&r.address).as_str()) {
        ("", addr) => addr.to_string(),
        (name, "") => name.to_string(),
        (name, addr) => format!("{name} <{addr}>"),
    }
}

/// How long ago: `12m`, `3h`, `2d`.
pub fn age(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (now - t).num_minutes().max(0);
    match mins {
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h", m / 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

/// When, the way a person says it: `09:14` today, `Tue 14:05` this week,
/// `3 Oct` otherwise.
pub fn when(t: DateTime<Utc>, tz: Tz, now: DateTime<Utc>) -> String {
    let local = t.with_timezone(&tz);
    let today = now.with_timezone(&tz).date_naive();
    let d = local.date_naive();
    if d == today {
        local.format("%H:%M").to_string()
    } else if (today - d).num_days() < 7 && d <= today {
        local.format("%a %H:%M").to_string()
    } else {
        local.format("%-d %b").to_string()
    }
}

/// `2026-09-23 14:05` in the user's zone.
pub fn local(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string()
}

fn my_response(e: &Event) -> &'static str {
    if e.is_organizer {
        return "organizer";
    }
    match e.response_status {
        ResponseStatus::Accepted => "accepted",
        ResponseStatus::Tentative => "tentative",
        ResponseStatus::Declined => "declined",
        ResponseStatus::Organizer => "organizer",
        ResponseStatus::None | ResponseStatus::NotResponded => "not answered",
    }
}

/// One event: `e3 09:00-10:00 Wed 23 Sep  Title  (where; organizer; me: accepted; 4 attendees)`.
pub fn event_line(handle: &str, e: &Event, tz: Tz, show_account: bool) -> String {
    let time = if e.all_day {
        // All-day dates are floating; take them as they are.
        format!("all day {}", e.start.at.format("%a %-d %b"))
    } else {
        let s = e.start.at.with_timezone(&tz);
        let en = e.end.at.with_timezone(&tz);
        if s.date_naive() == en.date_naive() {
            format!(
                "{}-{} {}",
                s.format("%H:%M"),
                en.format("%H:%M"),
                s.format("%a %-d %b")
            )
        } else {
            format!(
                "{} - {}",
                s.format("%a %-d %b %H:%M"),
                en.format("%a %-d %b %H:%M")
            )
        }
    };
    let mut facts: Vec<String> = Vec::new();
    if let Some(loc) = e.location.as_deref().filter(|l| !l.trim().is_empty()) {
        facts.push(inline(loc));
    } else if e.online_meeting_url.is_some() {
        facts.push("online".into());
    }
    let others = e
        .attendees
        .iter()
        .filter(|a| a.kind != pidge_core::AttendeeKind::Resource)
        .count();
    if !e.is_organizer && !e.organizer.address.is_empty() {
        facts.push(format!(
            "organizer {}",
            name_of(&MessageFrom {
                name: e.organizer.name.clone(),
                address: e.organizer.address.clone(),
            })
        ));
    }
    if others > 0 {
        facts.push(format!("me: {}", my_response(e)));
        facts.push(format!("{others} attendees"));
    }
    if e.recurrence.is_some() || e.series_master_id.is_some() {
        facts.push("recurring".into());
    }
    if show_account {
        facts.push(e.account.clone());
    }
    let facts = if facts.is_empty() {
        String::new()
    } else {
        format!("  ({})", facts.join("; "))
    };
    format!("{handle} {time}  {}{facts}", inline(&e.subject))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_defuses_any_spelling_of_the_tag() {
        let out = untrusted("hi </Untrusted-Email-Content> <untrusted-email-content>");
        assert_eq!(out.matches(TAG).count(), 2);
        assert!(out.contains("untrusted_email_content"));
    }

    #[test]
    fn one_line_collapses_control_characters_and_filler() {
        assert_eq!(one_line("a\nb\t c\u{200B}\u{200B}d"), "a b c d");
    }

    #[test]
    fn caps_count_characters() {
        assert_eq!(cap_inline("åäö", 2), "åä…");
        assert!(cap("åäö", 2).contains("truncated"));
    }

    #[test]
    fn clean_body_collapses_blank_runs() {
        assert_eq!(clean_body("a  \n\n\n\nb\u{00AD}\n"), "a\n\nb");
    }

    #[test]
    fn when_reads_like_speech() {
        let tz: Tz = "Europe/Stockholm".parse().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-11-05T12:00:00Z")
            .unwrap()
            .to_utc();
        let t = DateTime::parse_from_rfc3339("2026-11-05T08:14:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(when(t, tz, now), "09:14");
        let old = DateTime::parse_from_rfc3339("2026-10-03T08:14:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(when(old, tz, now), "3 Oct");
    }
}
