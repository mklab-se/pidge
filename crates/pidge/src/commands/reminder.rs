//! `--reminder` parsing and display for calendar commands.

use anyhow::{Result, anyhow};
use pidge_client::graph::events::Reminder;

/// Parse a `--reminder` value. `None` (flag absent) is [`Reminder::Default`].
///
/// Accepted: `off` / `none` / `no`; a bare number of minutes (`30`); or a
/// duration with `w`/`d`/`h`/`m` units, which may be chained (`1d2h`,
/// `90m`, `1w`). Whitespace inside the spec is ignored.
pub fn parse_reminder(spec: Option<&str>) -> Result<Reminder> {
    let Some(spec) = spec else {
        return Ok(Reminder::Default);
    };
    let s: String = spec.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = s.to_ascii_lowercase();
    if matches!(lower.as_str(), "off" | "none" | "no" | "0") {
        return Ok(Reminder::Off);
    }
    if lower.is_empty() {
        return Err(anyhow!("--reminder needs a value like 15m, 2h, 1d or off"));
    }
    if let Ok(n) = lower.parse::<u32>() {
        return Ok(Reminder::MinutesBefore(n));
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for c in lower.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num
            .parse()
            .map_err(|_| anyhow!("--reminder '{spec}': expected a number before '{c}'"))?;
        num.clear();
        let mult = match c {
            'm' => 1,
            'h' => 60,
            'd' => 60 * 24,
            'w' => 60 * 24 * 7,
            other => {
                return Err(anyhow!(
                    "--reminder '{spec}': unknown unit '{other}' (use m, h, d or w)"
                ));
            }
        };
        total += n * mult;
        saw_unit = true;
    }
    if !num.is_empty() || !saw_unit {
        return Err(anyhow!(
            "--reminder '{spec}': use minutes (30), a duration (15m, 2h, 1d, 1d2h) or off"
        ));
    }
    let minutes =
        u32::try_from(total).map_err(|_| anyhow!("--reminder '{spec}': that is too far ahead"))?;
    Ok(Reminder::MinutesBefore(minutes))
}

/// Human label for an event's stored reminder, e.g. `1 day before`.
pub fn describe(minutes: Option<u32>) -> String {
    let Some(m) = minutes else {
        return "off".to_string();
    };
    if m == 0 {
        return "at start".to_string();
    }
    let (weeks, rest) = (m / (60 * 24 * 7), m % (60 * 24 * 7));
    let (days, rest) = (rest / (60 * 24), rest % (60 * 24));
    let (hours, mins) = (rest / 60, rest % 60);
    let mut parts = Vec::new();
    for (n, unit) in [
        (weeks, "week"),
        (days, "day"),
        (hours, "hour"),
        (mins, "min"),
    ] {
        if n == 0 {
            continue;
        }
        let plural = if n == 1 || unit == "min" { "" } else { "s" };
        parts.push(format!("{n} {unit}{plural}"));
    }
    format!("{} before", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_flag_is_default() {
        assert_eq!(parse_reminder(None).unwrap(), Reminder::Default);
    }

    #[test]
    fn off_words_switch_reminder_off() {
        for w in ["off", "none", "no", "OFF", "0"] {
            assert_eq!(parse_reminder(Some(w)).unwrap(), Reminder::Off, "{w}");
        }
    }

    #[test]
    fn bare_number_is_minutes() {
        assert_eq!(
            parse_reminder(Some("30")).unwrap(),
            Reminder::MinutesBefore(30)
        );
    }

    #[test]
    fn units_and_chains() {
        assert_eq!(
            parse_reminder(Some("15m")).unwrap(),
            Reminder::MinutesBefore(15)
        );
        assert_eq!(
            parse_reminder(Some("2h")).unwrap(),
            Reminder::MinutesBefore(120)
        );
        assert_eq!(
            parse_reminder(Some("1d")).unwrap(),
            Reminder::MinutesBefore(1440)
        );
        assert_eq!(
            parse_reminder(Some("1w")).unwrap(),
            Reminder::MinutesBefore(10080)
        );
        assert_eq!(
            parse_reminder(Some("1d 2h")).unwrap(),
            Reminder::MinutesBefore(1560)
        );
        assert_eq!(
            parse_reminder(Some("1D2H30M")).unwrap(),
            Reminder::MinutesBefore(1590)
        );
    }

    #[test]
    fn garbage_errors() {
        for bad in ["", "soon", "1x", "d", "1d2", "1h-"] {
            assert!(parse_reminder(Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn describe_labels() {
        assert_eq!(describe(None), "off");
        assert_eq!(describe(Some(0)), "at start");
        assert_eq!(describe(Some(15)), "15 min before");
        assert_eq!(describe(Some(60)), "1 hour before");
        assert_eq!(describe(Some(1440)), "1 day before");
        assert_eq!(describe(Some(1590)), "1 day 2 hours 30 min before");
        assert_eq!(describe(Some(20160)), "2 weeks before");
    }
}
