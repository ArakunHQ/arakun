//! Settling after a merge: once an environment's pull request is merged, its
//! review threads are resolved on GitHub, its notifications are marked read,
//! and the environment's sessions stop asking for attention and leave the
//! sessions list until they are used again. On unless turned off in
//! Settings > Branches.
//!
//! The merges already settled are kept in `settled.json` next to the state, so
//! each one is settled once, even after a restart.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Merged longer ago than this, a pull request is left as it is: the first run
/// doesn't go back over every old pull request.
pub const RECENT_SECS: u64 = 7 * 86_400;
/// Pull requests kept in the book: far more than a week of merges.
const BOOK_MAX: usize = 500;

static CHOICE: AtomicU8 = AtomicU8::new(UNSET);
const UNSET: u8 = 0;
const ON: u8 = 1;
const OFF: u8 = 2;

/// Whether merges are settled. On by default.
pub fn on() -> bool {
    match CHOICE.load(Ordering::Relaxed) {
        ON => return true,
        OFF => return false,
        _ => {}
    }
    let on = std::fs::read_to_string(choice_path())
        .ok()
        .and_then(|t| parse_choice(&t))
        .unwrap_or(true);
    CHOICE.store(if on { ON } else { OFF }, Ordering::Relaxed);
    on
}

pub fn set(on: bool) {
    // Kept in memory too, so the switch works for this run even when the file
    // cannot be written.
    CHOICE.store(if on { ON } else { OFF }, Ordering::Relaxed);
    crate::durable::write_soon(
        choice_path(),
        if on { "on\n" } else { "off\n" }.into(),
        "the settle-after-merge setting will not last past a restart",
    );
}

fn choice_path() -> PathBuf {
    crate::platform::app_dir().join("settle-after-merge")
}

fn parse_choice(text: &str) -> Option<bool> {
    match text.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// A pull request of the user's that was merged.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Merged {
    pub number: u64,
    pub url: String,
    #[serde(rename = "mergedAt", default)]
    pub merged_at: String,
    /// The branch it was merged from.
    #[serde(rename = "headRefName", default)]
    pub head: String,
}

impl Merged {
    /// Unix seconds; 0 when GitHub didn't say.
    pub fn merged_secs(&self) -> u64 {
        crate::account::parse_rfc3339(&self.merged_at).unwrap_or(0)
    }

    /// Merged within [`RECENT_SECS`] of `now`.
    pub fn recent(&self, now: u64) -> bool {
        let at = self.merged_secs();
        at > 0 && now.saturating_sub(at) <= RECENT_SECS
    }
}

/// The most merged pull requests read per repository: far more than one
/// person merges in [`RECENT_SECS`].
const MERGED_PER_REPO: &str = "300";
/// A `gh` that hangs must not hold up the next check.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// The user's pull requests merged in the last [`RECENT_SECS`] in the
/// repository at `dir`, from every branch. One `gh` call for the whole
/// repository: asking once per environment ran dozens of `gh` processes in a
/// row for a project with many environments. Empty when there are none or gh
/// can't say.
pub fn recent_merged(dir: &Path, now: u64) -> Vec<Merged> {
    let search = merged_since(now);
    let mut cmd = crate::git::gh(dir);
    cmd.args([
        "pr",
        "list",
        "--state",
        "merged",
        "--author",
        "@me",
        "--search",
        &search,
        "--limit",
        MERGED_PER_REPO,
        "--json",
        "number,url,mergedAt,headRefName",
    ]);
    match crate::headless::sandbox::run_with_timeout(cmd, None, GH_TIMEOUT) {
        Ok(out) if out.status.success() => serde_json::from_slice(&out.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The search for merges in the last [`RECENT_SECS`] before `now`. GitHub's
/// search takes a date, so the day before also counts; [`Merged::recent`]
/// drops what is too old.
fn merged_since(now: u64) -> String {
    let days = now.saturating_sub(RECENT_SECS) / 86_400;
    let (y, m, d) = crate::platform::civil_from_days(days.saturating_sub(1) as i64);
    format!("merged:>={y:04}-{m:02}-{d:02}")
}

/// The merges in `list` from `branch`.
pub fn of_branch<'a>(list: &'a [Merged], branch: &'a str) -> impl Iterator<Item = &'a Merged> {
    list.iter().filter(move |m| m.head == branch)
}

/// The merges already settled, by pull request URL, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    settled: VecDeque<String>,
}

impl Book {
    pub fn load() -> Self {
        crate::durable::load_json(book_path()).unwrap_or_default()
    }

    pub fn store(&self) {
        if let Err(e) = crate::durable::save_json(&book_path(), self) {
            eprintln!("arakun: could not keep the settled merges: {e}");
        }
    }

    pub fn has(&self, url: &str) -> bool {
        self.settled.iter().any(|u| u == url)
    }

    pub fn add(&mut self, url: &str) {
        if self.has(url) {
            return;
        }
        self.settled.push_back(url.to_string());
        while self.settled.len() > BOOK_MAX {
            self.settled.pop_front();
        }
    }
}

/// Next to the state, so a test Arakun (`ARAKUN_STATE`) keeps its own.
fn book_path() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("settled.json"),
        None => crate::state::data_dir().join("settled.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_choice() {
        assert_eq!(parse_choice("on\n"), Some(true));
        assert_eq!(parse_choice("off"), Some(false));
        assert_eq!(parse_choice("maybe"), None);
    }

    #[test]
    fn only_a_recent_merge_counts() {
        let m = |at: &str| Merged {
            number: 1,
            url: "https://github.com/o/r/pull/1".into(),
            merged_at: at.into(),
            head: "b".into(),
        };
        let now = crate::account::parse_rfc3339("2026-11-17T12:00:00Z").unwrap();
        assert!(m("2026-11-17T11:00:00Z").recent(now));
        assert!(m("2026-11-11T12:00:00Z").recent(now));
        assert!(!m("2026-11-01T12:00:00Z").recent(now));
        // GitHub didn't say when: left alone.
        assert!(!m("").recent(now));
    }

    #[test]
    fn the_book_keeps_each_merge_once_and_forgets_the_oldest() {
        let mut b = Book::default();
        b.add("a");
        b.add("a");
        assert_eq!(b.settled.len(), 1);
        for i in 0..BOOK_MAX {
            b.add(&i.to_string());
        }
        assert_eq!(b.settled.len(), BOOK_MAX);
        assert!(!b.has("a"));
        assert!(b.has(&(BOOK_MAX - 1).to_string()));
    }

    #[test]
    fn reads_what_gh_answers() {
        let list: Vec<Merged> = serde_json::from_str(
            r#"[{"number":232,"url":"https://github.com/ArakunHQ/arakun/pull/232","mergedAt":"2026-11-17T18:00:00Z","headRefName":"fix/a"},
                {"number":233,"url":"https://github.com/ArakunHQ/arakun/pull/233","mergedAt":"2026-11-18T18:00:00Z","headRefName":"fix/b"},
                {"number":240,"url":"https://github.com/ArakunHQ/arakun/pull/240","mergedAt":"2026-11-19T18:00:00Z","headRefName":"fix/a"}]"#,
        )
        .unwrap();
        assert_eq!(list[0].number, 232);
        assert!(list[0].merged_secs() > 0);
        // A branch merged twice finds both; another branch's merge isn't its.
        let a: Vec<u64> = of_branch(&list, "fix/a").map(|m| m.number).collect();
        assert_eq!(a, [232, 240]);
        assert_eq!(of_branch(&list, "fix/c").count(), 0);
    }

    #[test]
    fn searches_back_a_week_and_a_day() {
        let now = crate::account::parse_rfc3339("2026-11-21T12:00:00Z").unwrap();
        assert_eq!(merged_since(now), "merged:>=2026-11-13");
        // Early in the day the week still starts on the day before it.
        let early = crate::account::parse_rfc3339("2026-11-21T00:30:00Z").unwrap();
        assert_eq!(merged_since(early), "merged:>=2026-11-13");
    }
}
