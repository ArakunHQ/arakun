//! Removing old environments by themselves: once an environment's pull request
//! is merged or closed and nobody used it for a while, its worktree goes. The
//! branch stays. Off until the user picks a number of days in Settings >
//! Branches or during the setup; the shell (`shell::cleanup`) watches and
//! removes.
//!
//! An environment is only removed when nothing in it can be lost: no
//! uncommitted or untracked files, and every commit is in the pull request.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use serde::Deserialize;

/// The choices Settings and the setup offer, in days; 0 is off.
pub const CHOICES: [u8; 4] = [0, 7, 14, 30];
/// What the one-time suggestion counts with, before the user picked.
pub const SUGGEST_DAYS: u8 = 14;

/// The user's choice, as it is in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Never picked, nor suggested yet: off.
    Unset,
    /// Off, after the suggestion was shown and left alone.
    Suggested,
    Off,
    Days(u8),
}

impl Choice {
    /// Days an environment must sit unused; None when off.
    pub fn days(self) -> Option<u8> {
        match self {
            Choice::Days(d) if d > 0 => Some(d),
            _ => None,
        }
    }

    fn parse(text: &str) -> Choice {
        match text.trim() {
            "off" => Choice::Off,
            "suggested" => Choice::Suggested,
            n => match n.parse::<u8>() {
                Ok(0) => Choice::Off,
                Ok(d) => Choice::Days(d),
                Err(_) => Choice::Unset,
            },
        }
    }

    fn text(self) -> String {
        match self {
            Choice::Unset => String::new(),
            Choice::Suggested => "suggested\n".into(),
            Choice::Off | Choice::Days(0) => "off\n".into(),
            Choice::Days(d) => format!("{d}\n"),
        }
    }

    fn code(self) -> u8 {
        match self {
            Choice::Unset => LOADED_UNSET,
            Choice::Suggested => LOADED_SUGGESTED,
            Choice::Off | Choice::Days(0) => LOADED_OFF,
            Choice::Days(d) => d,
        }
    }

    fn from_code(code: u8) -> Option<Choice> {
        match code {
            NOT_LOADED => None,
            LOADED_UNSET => Some(Choice::Unset),
            LOADED_SUGGESTED => Some(Choice::Suggested),
            LOADED_OFF => Some(Choice::Off),
            d => Some(Choice::Days(d)),
        }
    }
}

/// The choice in memory, so the setting works for this run even when the file
/// cannot be written. Days are stored as themselves; the rest at the top.
static CHOICE: AtomicU8 = AtomicU8::new(NOT_LOADED);
const NOT_LOADED: u8 = 255;
const LOADED_UNSET: u8 = 254;
const LOADED_SUGGESTED: u8 = 253;
const LOADED_OFF: u8 = 252;

pub fn choice() -> Choice {
    if let Some(c) = Choice::from_code(CHOICE.load(Ordering::Relaxed)) {
        return c;
    }
    let c = std::fs::read_to_string(choice_path())
        .map(|t| Choice::parse(&t))
        .unwrap_or(Choice::Unset);
    CHOICE.store(c.code(), Ordering::Relaxed);
    c
}

/// Days an environment must sit unused before it goes; None when off.
pub fn days() -> Option<u8> {
    choice().days()
}

/// Turns the cleanup on after `days`, or off with 0.
pub fn set_days(days: u8) {
    set(if days == 0 {
        Choice::Off
    } else {
        Choice::Days(days.min(LOADED_OFF - 1))
    });
}

/// Notes that the suggestion was shown, so it is shown once.
pub fn set_suggested() {
    if choice() == Choice::Unset {
        set(Choice::Suggested);
    }
}

fn set(c: Choice) {
    CHOICE.store(c.code(), Ordering::Relaxed);
    let path = choice_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, c.text()) {
        eprintln!("arakun: the environment cleanup setting will not last past a restart: {e}");
    }
}

fn choice_path() -> std::path::PathBuf {
    crate::platform::app_dir().join("env-cleanup")
}

/// "Off", "7 days"…
pub fn label(days: u8) -> String {
    match days {
        0 => "Off".into(),
        1 => "1 day".into(),
        d => format!("{d} days"),
    }
}

// ---------------------------------------------------------------------------
// What may go

/// What the shell knows about an environment without asking git or GitHub.
#[derive(Clone, Copy, Debug)]
pub struct Seen {
    pub is_checkout: bool,
    /// On screen right now.
    pub visible: bool,
    /// An agent is working or waiting for an answer.
    pub busy: bool,
    /// Unix seconds it was last opened or one of its sessions last moved.
    pub used: u64,
}

/// Whether the environment is worth asking git and GitHub about.
pub fn candidate(s: Seen, days: u8, now: u64) -> bool {
    !s.is_checkout
        && !s.visible
        && !s.busy
        && days > 0
        && now.saturating_sub(s.used) >= u64::from(days) * 86_400
}

/// An environment's pull request, as `gh pr view` tells it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Pr {
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    #[serde(rename = "headRefOid", default)]
    pub head: String,
}

impl Pr {
    pub fn done(&self) -> bool {
        matches!(self.state.as_str(), "MERGED" | "CLOSED")
    }
}

/// A `gh` that hangs must not hold up the next check.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// The pull request of the branch checked out in `dir`; None when there is
/// none or gh can't say.
fn pr_of(dir: &Path) -> Option<Pr> {
    let mut cmd = crate::git::gh(dir);
    cmd.args(["pr", "view", "--json", "state,headRefOid"]);
    match crate::headless::sandbox::run_with_timeout(cmd, None, GH_TIMEOUT) {
        Ok(out) if out.status.success() => serde_json::from_slice(&out.stdout).ok(),
        _ => None,
    }
}

/// Every commit checked out in `dir` is in the pull request's last commit.
fn head_in(dir: &Path, pr_head: &str) -> bool {
    let Ok(head) = crate::git::read(dir, &["rev-parse", "HEAD"]) else {
        return false;
    };
    let head = head.trim();
    if head.is_empty() || pr_head.is_empty() {
        return false;
    }
    head == pr_head
        // Someone pushed more to the pull request after the last pull. A
        // commit this clone doesn't have fails the check, which is safe.
        || crate::git::read(dir, &["merge-base", "--is-ancestor", head, pr_head]).is_ok()
}

/// Asks git and GitHub whether the environment at `dir` can go: its pull
/// request is merged or closed, nothing is uncommitted, and every commit is in
/// the pull request. Slow: runs git and gh.
pub fn removable(dir: &Path) -> bool {
    if !crate::git::status(dir).is_ok_and(|c| c.is_empty()) {
        return false;
    }
    pr_of(dir).is_some_and(|pr| pr.done() && head_in(dir, &pr.head))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn seen(used: u64) -> Seen {
        Seen {
            is_checkout: false,
            visible: false,
            busy: false,
            used,
        }
    }

    #[test]
    fn the_choice_reads_back_what_it_wrote() {
        for c in [
            Choice::Suggested,
            Choice::Off,
            Choice::Days(7),
            Choice::Days(30),
        ] {
            assert_eq!(Choice::parse(&c.text()), c);
            assert_eq!(Choice::from_code(c.code()), Some(c));
        }
        assert_eq!(Choice::parse(""), Choice::Unset);
        assert_eq!(Choice::parse("soon"), Choice::Unset);
        assert_eq!(Choice::parse("0"), Choice::Off);
    }

    #[test]
    fn only_a_number_of_days_turns_it_on() {
        assert_eq!(Choice::Unset.days(), None);
        assert_eq!(Choice::Suggested.days(), None);
        assert_eq!(Choice::Off.days(), None);
        assert_eq!(Choice::Days(14).days(), Some(14));
    }

    #[test]
    fn an_environment_must_sit_unused_for_the_days() {
        let now = 100 * DAY;
        assert!(candidate(seen(now - 14 * DAY), 14, now));
        assert!(!candidate(seen(now - 14 * DAY + 1), 14, now));
        assert!(!candidate(seen(now - 40 * DAY), 0, now));
        // A time in the future (a clock moved back) is not old.
        assert!(!candidate(seen(now + DAY), 7, now));
    }

    #[test]
    fn the_checkout_and_what_is_in_use_stay() {
        let now = 100 * DAY;
        let old = seen(0);
        assert!(candidate(old, 7, now));
        for s in [
            Seen {
                is_checkout: true,
                ..old
            },
            Seen {
                visible: true,
                ..old
            },
            Seen { busy: true, ..old },
        ] {
            assert!(!candidate(s, 7, now));
        }
    }

    #[test]
    fn a_merged_or_closed_pull_request_is_done() {
        let pr = |state: &str| Pr {
            state: state.into(),
            head: String::new(),
        };
        assert!(pr("MERGED").done());
        assert!(pr("CLOSED").done());
        assert!(!pr("OPEN").done());
    }

    #[test]
    fn head_in_wants_every_commit_in_the_pull_request() {
        let dir = std::env::temp_dir().join(format!("arakun-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = crate::git::git(&dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let first = git(&["rev-parse", "HEAD"]);
        assert!(head_in(&dir, &first));
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        let second = git(&["rev-parse", "HEAD"]);
        // A commit the pull request doesn't have.
        assert!(!head_in(&dir, &first));
        git(&["reset", "-q", "--hard", &first]);
        // The pull request got more after this clone pulled.
        assert!(head_in(&dir, &second));
        assert!(!head_in(&dir, "0123456789012345678901234567890123456789"));
        assert!(!head_in(&dir, ""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
