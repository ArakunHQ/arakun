//! Settling after a merge: once an environment's pull request is merged, its
//! review threads are resolved on GitHub, its notifications are marked read,
//! and the environment's sessions stop asking for attention. On unless turned
//! off in Settings > Branches.
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
    let text: &[u8] = if on { b"on\n" } else { b"off\n" };
    if let Err(e) = crate::durable::write(&choice_path(), text) {
        eprintln!("arakun: the settle-after-merge setting will not last past a restart: {e}");
    }
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

/// The pull request merged from an environment's branch.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Merged {
    pub number: u64,
    pub url: String,
    #[serde(rename = "mergedAt", default)]
    pub merged_at: String,
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

/// The most merged pull requests read per branch: a branch can be merged
/// more than once.
const MERGED_PER_BRANCH: &str = "5";
/// A `gh` that hangs must not hold up the next check.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// The user's newest merged pull requests from `branch`, asked in `dir`.
/// Empty when there are none or gh can't say.
pub fn merged_prs(dir: &Path, branch: &str) -> Vec<Merged> {
    let mut cmd = crate::git::gh(dir);
    cmd.args([
        "pr",
        "list",
        "--state",
        "merged",
        "--author",
        "@me",
        "--head",
        branch,
        "--limit",
        MERGED_PER_BRANCH,
        "--json",
        "number,url,mergedAt",
    ]);
    match crate::headless::sandbox::run_with_timeout(cmd, None, GH_TIMEOUT) {
        Ok(out) if out.status.success() => serde_json::from_slice(&out.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The merges already settled, by pull request URL, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    settled: VecDeque<String>,
}

impl Book {
    pub fn load() -> Self {
        std::fs::read(book_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn store(&self) {
        let path = book_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(self)
            && let Err(e) = crate::durable::write(&path, &bytes)
        {
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
            r#"[{"number":232,"url":"https://github.com/ArakunHQ/arakun/pull/232","mergedAt":"2026-11-17T18:00:00Z"}]"#,
        )
        .unwrap();
        assert_eq!(list[0].number, 232);
        assert!(list[0].merged_secs() > 0);
    }
}
