//! Every notice the shell has shown (on the island or as a desktop
//! notification), kept so the ones that came while the user was away can be
//! read later in the Inbox and with `arakun notices`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::island::AlertKind;

/// Older notices fall off past this many.
pub const CAP: usize = 300;

/// How much of a subtitle (often the start of an agent's reply) is kept: the
/// Inbox shows one line of it, so more would only sit on disk.
const PREVIEW_CHARS: usize = 140;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    /// Unix seconds.
    pub at: u64,
    pub kind: AlertKind,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    /// What "Ver" opens (see `Open` in the shell); empty for nothing.
    #[serde(default)]
    pub key: String,
    /// The request a `Confirm` answers, so it is kept once.
    #[serde(default)]
    pub req: String,
    /// The sender's picture, a file.
    #[serde(default)]
    pub image: String,
    /// The service it comes from (`github`).
    #[serde(default)]
    pub brand: String,
    #[serde(default)]
    pub seen: bool,
}

impl Notice {
    pub fn from_alert(alert: &crate::island::Alert, at: u64) -> Notice {
        Notice {
            at,
            kind: alert.kind,
            title: alert.title.to_string(),
            subtitle: preview(&alert.subtitle),
            key: alert.key.to_string(),
            req: alert.req.to_string(),
            image: alert.image.to_string(),
            brand: alert.brand.to_string(),
            seen: false,
        }
    }
}

fn preview(text: &str) -> String {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_string(),
    }
}

/// Oldest first, as they came.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub notices: Vec<Notice>,
}

impl History {
    /// Keeps `n`, dropping the oldest past `CAP`. `false` when it was a
    /// confirmation already kept: one request is shown again while it waits.
    pub fn push(&mut self, n: Notice) -> bool {
        if n.kind == AlertKind::Confirm
            && !n.req.is_empty()
            && self
                .notices
                .iter()
                .any(|o| o.kind == AlertKind::Confirm && o.req == n.req)
        {
            return false;
        }
        self.notices.push(n);
        let over = self.notices.len().saturating_sub(CAP);
        self.notices.drain(..over);
        true
    }

    pub fn unseen(&self) -> usize {
        self.notices.iter().filter(|n| !n.seen).count()
    }

    /// `false` when nothing changed.
    pub fn mark_seen(&mut self, index: usize) -> bool {
        match self.notices.get_mut(index) {
            Some(n) if !n.seen => {
                n.seen = true;
                true
            }
            _ => false,
        }
    }

    /// `false` when nothing changed.
    pub fn mark_all_seen(&mut self) -> bool {
        let mut changed = false;
        for n in self.notices.iter_mut().filter(|n| !n.seen) {
            n.seen = true;
            changed = true;
        }
        changed
    }

    /// Newest first, under a heading per local day: indices into `notices`.
    pub fn by_day(&self, now: u64) -> Vec<(String, Vec<usize>)> {
        group(&self.notices, now, local_date)
    }
}

/// A calendar day: year, month (0-based, as `struct tm`), day of the month.
type Date = (i32, i32, i32);

fn local_date(unix: u64) -> Option<Date> {
    crate::platform::local_time(unix).map(|t| (t.year, t.month, t.day))
}

fn group(
    notices: &[Notice],
    now: u64,
    date: impl Fn(u64) -> Option<Date>,
) -> Vec<(String, Vec<usize>)> {
    let today = date(now);
    // Newest first by time, not by arrival: a clock set back mustn't split a day.
    let mut order: Vec<usize> = (0..notices.len()).rev().collect();
    order.sort_by_key(|&i| std::cmp::Reverse(notices[i].at));
    let mut out: Vec<(Option<Date>, Vec<usize>)> = Vec::new();
    for i in order {
        let d = date(notices[i].at);
        match out.last_mut() {
            Some((last, items)) if *last == d => items.push(i),
            _ => out.push((d, vec![i])),
        }
    }
    out.into_iter()
        .map(|(d, items)| {
            let label = match (d, today) {
                (Some(d), Some(today)) => day_label(d, today),
                _ => "Earlier".to_string(),
            };
            (label, items)
        })
        .collect()
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days(d: Date) -> i64 {
    let (y, m, day) = (d.0 as i64, d.1 as i64 + 1, d.2 as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "Today", "Yesterday", the weekday within the week, else the date.
fn day_label(d: Date, today: Date) -> String {
    let n = days(d);
    match days(today) - n {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        // 1970-01-01 was a Thursday.
        2..=6 => WEEKDAYS[(n + 4).rem_euclid(7) as usize].into(),
        _ => {
            let month = MONTHS.get(d.1 as usize).copied().unwrap_or("?");
            if d.0 == today.0 {
                format!("{month} {}", d.2)
            } else {
                format!("{month} {}, {}", d.2, d.0)
            }
        }
    }
}

/// Next to the state file, like `github.json`, so a test instance keeps its own.
fn file() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("notices.json"),
        None => crate::state::data_dir().join("notices.json"),
    }
}

pub fn load() -> History {
    load_from(&file(), crate::model::unix_now())
}

/// Never loses what the file held: a file that can't be read in full is set
/// aside as `notices.json.corrupt-<unix time>` before anything overwrites it,
/// and the notices that still parse are kept.
fn load_from(path: &Path, now: u64) -> History {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return History::default(),
        Err(e) => {
            eprintln!("arakun: could not read {}: {e}", path.display());
            set_aside(path, now);
            return History::default();
        }
    };
    match parse(&bytes) {
        Ok((history, 0)) => history,
        Ok((history, bad)) => {
            eprintln!(
                "arakun: {} notices in {} could not be read; keeping the other {}",
                bad,
                path.display(),
                history.notices.len()
            );
            if set_aside(path, now) {
                store_to(path, &history);
            }
            history
        }
        Err(e) => {
            eprintln!("arakun: {} is not valid: {e}", path.display());
            set_aside(path, now);
            History::default()
        }
    }
}

/// The history and how many notices in it could not be read. One bad entry
/// (say, a kind from a newer Arakun) must not cost the rest.
fn parse(bytes: &[u8]) -> serde_json::Result<(History, usize)> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        notices: Vec<serde_json::Value>,
    }
    let raw: Raw = serde_json::from_slice(bytes)?;
    let total = raw.notices.len();
    let notices: Vec<Notice> = raw
        .notices
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    let bad = total - notices.len();
    Ok((History { notices }, bad))
}

/// Renames the file out of the way so the next save can't overwrite it.
/// `false` when it is still there.
fn set_aside(path: &Path, now: u64) -> bool {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".corrupt-{now}"));
    let kept = PathBuf::from(name);
    match std::fs::rename(path, &kept) {
        Ok(()) => {
            eprintln!("arakun: kept the old notices in {}", kept.display());
            true
        }
        Err(e) => {
            eprintln!("arakun: could not set {} aside: {e}", path.display());
            false
        }
    }
}

pub fn store(history: &History) {
    store_to(&file(), history);
}

/// Private: subtitles hold the start of agent replies.
fn store_to(path: &Path, history: &History) {
    if let Err(e) = crate::durable::write_private(
        path,
        &serde_json::to_vec_pretty(history).unwrap_or_default(),
    ) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

/// `arakun notices [--unseen]`: the history, newest first, by day.
pub fn cli(args: &[String]) -> i32 {
    let unseen_only = match args {
        [] => false,
        [flag] if flag == "--unseen" => true,
        _ => {
            eprintln!("usage: arakun notices [--unseen]");
            return 2;
        }
    };
    let history = load();
    let now = crate::model::unix_now();
    let mut any = false;
    for (day, items) in history.by_day(now) {
        let items: Vec<&Notice> = items
            .iter()
            .map(|&i| &history.notices[i])
            .filter(|n| !unseen_only || !n.seen)
            .collect();
        if items.is_empty() {
            continue;
        }
        if any {
            println!();
        }
        any = true;
        println!("{day}");
        for n in items {
            let clock = crate::platform::local_time(n.at)
                .map(|t| format!("{:02}:{:02}", t.hour, t.minute))
                .unwrap_or_default();
            let mark = if n.seen { ' ' } else { '*' };
            match n.subtitle.is_empty() {
                true => println!("  {clock} {mark} {}", n.title),
                false => println!("  {clock} {mark} {} — {}", n.title, n.subtitle),
            }
        }
    }
    if !any {
        println!(
            "{}",
            if unseen_only {
                "No unseen notifications."
            } else {
                "No notifications yet."
            }
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(at: u64, title: &str) -> Notice {
        Notice {
            at,
            kind: AlertKind::Info,
            title: title.into(),
            subtitle: String::new(),
            key: String::new(),
            req: String::new(),
            image: String::new(),
            brand: String::new(),
            seen: false,
        }
    }

    #[test]
    fn push_keeps_the_newest_up_to_the_cap() {
        let mut h = History::default();
        for i in 0..CAP as u64 + 5 {
            assert!(h.push(notice(i, &i.to_string())));
        }
        assert_eq!(h.notices.len(), CAP);
        assert_eq!(h.notices[0].at, 5);
        assert_eq!(h.notices.last().unwrap().at, CAP as u64 + 4);
    }

    #[test]
    fn a_confirmation_is_kept_once() {
        let mut h = History::default();
        let mut c = notice(1, "Bash");
        c.kind = AlertKind::Confirm;
        c.req = "r1".into();
        assert!(h.push(c.clone()));
        assert!(!h.push(c.clone()));
        c.req = "r2".into();
        assert!(h.push(c));
        // Other notices repeat freely: CI can fail twice.
        assert!(h.push(notice(2, "CI failed")));
        assert!(h.push(notice(3, "CI failed")));
        assert_eq!(h.notices.len(), 4);
    }

    #[test]
    fn seen_counts_and_marks() {
        let mut h = History::default();
        h.push(notice(1, "a"));
        h.push(notice(2, "b"));
        h.push(notice(3, "c"));
        assert_eq!(h.unseen(), 3);
        assert!(h.mark_seen(1));
        assert!(!h.mark_seen(1));
        assert!(!h.mark_seen(9));
        assert_eq!(h.unseen(), 2);
        assert!(h.mark_all_seen());
        assert!(!h.mark_all_seen());
        assert_eq!(h.unseen(), 0);
    }

    #[test]
    fn day_labels() {
        let today = (2026, 10, 14); // Saturday, Nov 14 2026
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label((2026, 10, 13), today), "Yesterday");
        assert_eq!(day_label((2026, 10, 12), today), "Thursday");
        assert_eq!(day_label((2026, 10, 8), today), "Sunday");
        assert_eq!(day_label((2026, 10, 7), today), "Nov 7");
        assert_eq!(day_label((2025, 11, 31), (2026, 0, 2)), "Wednesday");
        assert_eq!(day_label((2025, 11, 20), (2026, 0, 2)), "Dec 20, 2025");
        assert_eq!(days((1970, 0, 1)), 0);
    }

    #[test]
    fn grouped_newest_first_by_day() {
        let mut h = History::default();
        for (at, t) in [(10, "a"), (20, "b"), (100, "c"), (110, "d"), (200, "e")] {
            h.push(notice(at, t));
        }
        // A "day" of 100 seconds, so the test doesn't hang on the time zone.
        let date = |at: u64| Some((2026, 10, 14 - (2 - at as i32 / 100)));
        let groups = group(&h.notices, 250, date);
        assert_eq!(
            groups,
            vec![
                ("Today".to_string(), vec![4]),
                ("Yesterday".to_string(), vec![3, 2]),
                ("Thursday".to_string(), vec![1, 0]),
            ]
        );
    }

    #[test]
    fn an_old_file_without_new_fields_loads() {
        let h: History =
            serde_json::from_str(r#"{"notices":[{"at":5,"kind":"failure","title":"CI"}]}"#)
                .unwrap();
        assert_eq!(h.notices[0].kind, AlertKind::Failure);
        assert!(!h.notices[0].seen);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-notices-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_corrupt_file_is_set_aside_not_overwritten() {
        let dir = scratch("corrupt");
        let path = dir.join("notices.json");
        let torn = br#"{"notices":[{"at":5,"kind":"failure","title":"CI"#;
        std::fs::write(&path, torn).unwrap();
        let h = load_from(&path, 42);
        assert!(h.notices.is_empty());
        // The next save starts a new file; the old bytes stay where they were put.
        store_to(&path, &h);
        assert_eq!(
            std::fs::read(dir.join("notices.json.corrupt-42")).unwrap(),
            torn
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_bad_notice_does_not_lose_the_others() {
        let dir = scratch("bad-entry");
        let path = dir.join("notices.json");
        let text = r#"{"notices":[
            {"at":1,"kind":"failure","title":"CI"},
            {"at":2,"kind":"from-the-future","title":"?"},
            {"at":3,"kind":"info","title":"Deploy"}
        ]}"#;
        std::fs::write(&path, text).unwrap();
        let h = load_from(&path, 7);
        let titles: Vec<&str> = h.notices.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["CI", "Deploy"]);
        // The good ones are saved again; the whole old file is kept beside them.
        assert_eq!(load_from(&path, 8), h);
        assert_eq!(
            std::fs::read_to_string(dir.join("notices.json.corrupt-7")).unwrap(),
            text
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let dir = scratch("missing");
        assert_eq!(load_from(&dir.join("notices.json"), 1), History::default());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("private");
        let path = dir.join("notices.json");
        let mut h = History::default();
        h.push(notice(1, "Claude terminó"));
        store_to(&path, &h);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_reply_is_kept_short() {
        let long = "word ".repeat(100);
        let kept = preview(&long);
        assert!(kept.chars().count() <= PREVIEW_CHARS + 1);
        assert!(kept.ends_with('…'));
        assert_eq!(preview("short"), "short");
    }
}
