//! Free-slot computation over a busy list within working hours.
//!
//! Walks each local working day inside a UTC range, subtracts merged busy
//! intervals, and keeps gaps at least as long as the requested duration.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Working hours in local time. `weekdays[0]` is Monday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkingHours {
    pub start_hour: u32,
    pub end_hour: u32,
    pub weekdays: [bool; 7],
}

impl Default for WorkingHours {
    fn default() -> Self {
        Self {
            start_hour: 8,
            end_hour: 18,
            weekdays: [true, true, true, true, true, false, false],
        }
    }
}

/// Compute free slots within `range`, honoring `hours` (interpreted in `tz`)
/// and subtracting merged `busy` intervals. Returns at most `max` slots.
///
/// A working day whose start or end local time does not exist in `tz` (a DST
/// gap) contributes no slots for that day, rather than panicking.
pub fn free_slots(
    busy: &[Busy],
    range: (DateTime<Utc>, DateTime<Utc>),
    duration: Duration,
    hours: &WorkingHours,
    tz: Tz,
    max: usize,
) -> Vec<Slot> {
    let mut merged: Vec<Busy> = busy.to_vec();
    merged.sort_by_key(|b| b.start);
    let mut busy_merged: Vec<Busy> = Vec::new();
    for b in merged {
        match busy_merged.last_mut() {
            Some(last) if b.start <= last.end => last.end = last.end.max(b.end),
            _ => busy_merged.push(b),
        }
    }

    let mut out = Vec::new();
    let mut day = range.0.with_timezone(&tz).date_naive();
    let last_day = range.1.with_timezone(&tz).date_naive();
    while day <= last_day && out.len() < max {
        if hours.weekdays[day.weekday().num_days_from_monday() as usize] {
            let mk = |h: u32| -> Option<DateTime<Utc>> {
                tz.from_local_datetime(&day.and_hms_opt(h, 0, 0)?)
                    .earliest()
                    .map(|d| d.with_timezone(&Utc))
            };
            if let (Some(start), Some(end)) = (mk(hours.start_hour), mk(hours.end_hour)) {
                let mut cursor = start.max(range.0);
                let day_end = end.min(range.1);
                for b in busy_merged.iter().filter(|b| b.start < day_end) {
                    if b.end <= cursor {
                        continue;
                    }
                    if b.start - cursor >= duration {
                        out.push(Slot {
                            start: cursor,
                            end: b.start,
                        });
                    }
                    cursor = cursor.max(b.end);
                }
                if day_end - cursor >= duration {
                    out.push(Slot {
                        start: cursor,
                        end: day_end,
                    });
                }
            }
        }
        day += Duration::days(1);
    }
    out.truncate(max);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn tz() -> Tz {
        chrono_tz::Europe::Stockholm
    }
    fn l(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        tz().with_ymd_and_hms(2026, 9, d, h, m, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn one_day_with_a_meeting_yields_two_gaps() {
        let busy = vec![Busy {
            start: l(23, 10, 0),
            end: l(23, 11, 0),
        }];
        let slots = free_slots(
            &busy,
            (l(23, 0, 0), l(24, 0, 0)),
            Duration::minutes(60),
            &WorkingHours::default(),
            tz(),
            20,
        );
        assert_eq!(
            slots,
            vec![
                Slot {
                    start: l(23, 8, 0),
                    end: l(23, 10, 0)
                },
                Slot {
                    start: l(23, 11, 0),
                    end: l(23, 18, 0)
                },
            ]
        );
    }

    #[test]
    fn weekends_and_short_gaps_are_skipped() {
        // 26/27 Sep 2026 are Saturday/Sunday.
        let busy = vec![Busy {
            start: l(25, 8, 0),
            end: l(25, 17, 30),
        }];
        let slots = free_slots(
            &busy,
            (l(25, 0, 0), l(28, 0, 0)),
            Duration::minutes(60),
            &WorkingHours::default(),
            tz(),
            20,
        );
        assert!(slots.is_empty());
    }

    #[test]
    fn overlapping_busy_intervals_merge() {
        let busy = vec![
            Busy {
                start: l(23, 9, 0),
                end: l(23, 12, 0),
            },
            Busy {
                start: l(23, 11, 0),
                end: l(23, 13, 0),
            },
        ];
        let slots = free_slots(
            &busy,
            (l(23, 0, 0), l(24, 0, 0)),
            Duration::minutes(30),
            &WorkingHours::default(),
            tz(),
            20,
        );
        assert_eq!(
            slots,
            vec![
                Slot {
                    start: l(23, 8, 0),
                    end: l(23, 9, 0)
                },
                Slot {
                    start: l(23, 13, 0),
                    end: l(23, 18, 0)
                },
            ]
        );
    }

    #[test]
    fn dst_gap_day_yields_no_slot_and_does_not_panic() {
        // 2018-11-04 in America/Sao_Paulo: DST started, clocks jumped from
        // midnight to 01:00, so local hour 00:00 does not exist that day.
        let sp = chrono_tz::America::Sao_Paulo;
        let hours = WorkingHours {
            start_hour: 0,
            end_hour: 18,
            weekdays: [true, true, true, true, true, true, true],
        };
        let start = sp
            .with_ymd_and_hms(2018, 11, 3, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let end = sp
            .with_ymd_and_hms(2018, 11, 5, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let slots = free_slots(&[], (start, end), Duration::minutes(60), &hours, sp, 20);
        assert!(
            slots.iter().all(|s| {
                let d = s.start.with_timezone(&sp).date_naive();
                d != chrono::NaiveDate::from_ymd_opt(2018, 11, 4).unwrap()
            }),
            "expected no slot on the DST-gap day, got {slots:?}"
        );
    }
}
