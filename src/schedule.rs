//! When automations run: hourly, daily, on weekdays or weekly, in local time.
//! Local time goes through libc (`localtime_r` / `mktime`), which knows the
//! user's zone and daylight saving.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "lowercase")]
pub enum Schedule {
    /// Only when run by hand.
    #[default]
    Manual,
    Hourly { minute: u8 },
    Daily { hour: u8, minute: u8 },
    /// Monday to Friday.
    Weekdays { hour: u8, minute: u8 },
    /// `day` 0 is Sunday.
    Weekly { day: u8, hour: u8, minute: u8 },
}

pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

impl Schedule {
    /// "Weekdays at 09:00", for lists.
    pub fn describe(&self) -> String {
        match *self {
            Self::Manual => "Manual".into(),
            Self::Hourly { minute } => format!("Hourly at :{minute:02}"),
            Self::Daily { hour, minute } => format!("Daily at {hour:02}:{minute:02}"),
            Self::Weekdays { hour, minute } => format!("Weekdays at {hour:02}:{minute:02}"),
            Self::Weekly { day, hour, minute } => {
                format!("{}s at {hour:02}:{minute:02}", WEEKDAYS[day as usize % 7])
            }
        }
    }

    /// The first scheduled time strictly after `after` (unix seconds).
    pub fn next_after(&self, after: u64) -> Option<u64> {
        let now = local(after)?;
        match *self {
            Self::Manual => None,
            Self::Hourly { minute } => {
                let at = make(now.year, now.month, now.day, now.hour, minute.min(59) as i32)?;
                if at > after { Some(at) } else { make(now.year, now.month, now.day, now.hour + 1, minute.min(59) as i32) }
            }
            Self::Daily { hour, minute } => first_day(now, after, hour, minute, |_| true),
            Self::Weekdays { hour, minute } => first_day(now, after, hour, minute, |wday| (1..=5).contains(&wday)),
            Self::Weekly { day, hour, minute } => first_day(now, after, hour, minute, |wday| wday == (day % 7) as i32),
        }
    }
}

fn first_day(now: Local, after: u64, hour: u8, minute: u8, ok: impl Fn(i32) -> bool) -> Option<u64> {
    (0..=8).find_map(|d| {
        let at = make(now.year, now.month, now.day + d, hour.min(23) as i32, minute.min(59) as i32)?;
        (at > after && ok(local(at)?.wday)).then_some(at)
    })
}

pub use crate::platform::LocalTime as Local;

pub fn local(unix: u64) -> Option<Local> {
    crate::platform::local_time(unix)
}

/// Local date and time to unix seconds; out-of-range fields roll over (day 32 is
/// the 1st of the next month), as `mktime` does.
fn make(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Option<u64> {
    crate::platform::from_local(year, month, day, hour, minute)
}

/// "today 09:00", "tomorrow 09:00", "Mon 09:00" or "3 Mar 09:00", relative to `now`.
pub fn when(at: u64, now: u64) -> String {
    let (Some(a), Some(n)) = (local(at), local(now)) else {
        return String::new();
    };
    let time = format!("{:02}:{:02}", a.hour, a.minute);
    let days = day_number(a) - day_number(n);
    match days {
        0 => format!("today {time}"),
        1 => format!("tomorrow {time}"),
        -1 => format!("yesterday {time}"),
        2..=6 => format!("{} {time}", &WEEKDAYS[a.wday as usize][..3]),
        _ => {
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{} {} {time}", a.day, MONTHS[a.month as usize % 12])
        }
    }
}

/// Days since an arbitrary epoch, for telling calendar days apart.
fn day_number(l: Local) -> i64 {
    // Days from civil (Howard Hinnant), with a 1-based month.
    let (y, m) = (l.year as i64 - if l.month < 2 { 1 } else { 0 }, l.month as i64 + 1);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + l.day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe
}

/// "09:30" or "9:30" to (9, 30).
pub fn parse_time(text: &str) -> Option<(u8, u8)> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m): (u8, u8) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_767_600_000; // early January 2026

    #[test]
    fn next_runs_land_on_the_schedule() {
        for sched in [
            Schedule::Hourly { minute: 15 },
            Schedule::Daily { hour: 9, minute: 30 },
            Schedule::Weekdays { hour: 8, minute: 0 },
            Schedule::Weekly { day: 0, hour: 18, minute: 45 },
        ] {
            let mut after = NOW;
            for _ in 0..20 {
                let next = sched.next_after(after).expect("a next run");
                assert!(next > after, "{sched:?}");
                let l = local(next).unwrap();
                match sched {
                    Schedule::Hourly { minute } => {
                        assert_eq!(l.minute, minute as i32);
                        assert!(next - after <= 3600);
                    }
                    Schedule::Daily { hour, minute } => {
                        assert_eq!((l.hour, l.minute), (hour as i32, minute as i32));
                        assert!(next - after <= 25 * 3600);
                    }
                    Schedule::Weekdays { hour, minute } => {
                        assert_eq!((l.hour, l.minute), (hour as i32, minute as i32));
                        assert!((1..=5).contains(&l.wday));
                    }
                    Schedule::Weekly { day, hour, minute } => {
                        assert_eq!((l.hour, l.minute, l.wday), (hour as i32, minute as i32, day as i32));
                        assert!(next - after <= 7 * 86400 + 3600);
                    }
                    Schedule::Manual => unreachable!(),
                }
                after = next;
            }
        }
        assert_eq!(Schedule::Manual.next_after(NOW), None);
    }

    #[test]
    fn times_parse_and_describe() {
        assert_eq!(parse_time("9:05"), Some((9, 5)));
        assert_eq!(parse_time(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("nine"), None);
        assert_eq!(Schedule::Weekly { day: 1, hour: 9, minute: 0 }.describe(), "Mondays at 09:00");
        assert_eq!(when(NOW, NOW).split_once(' ').unwrap().0, "today");
        assert_eq!(when(NOW + 86400, NOW).split_once(' ').unwrap().0, "tomorrow");
    }

    #[test]
    fn schedules_serialize_compactly() {
        let s = Schedule::Weekdays { hour: 9, minute: 0 };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"every":"weekdays","hour":9,"minute":0}"#);
        assert_eq!(serde_json::from_str::<Schedule>(&json).unwrap(), s);
    }
}
