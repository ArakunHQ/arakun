//! Removing what is no longer needed. Old environments by themselves: once an environment's pull request
//! is merged or closed and nobody used it for a while, its worktree goes. The
//! branch stays. Off until the user picks a number of days in Settings >
//! Branches or during the setup; the shell (`shell::cleanup`) watches and
//! removes.
//!
//! An environment is only removed when nothing in it can be lost: no
//! uncommitted, untracked or git-ignored files (build output aside), and every
//! commit is in the pull request.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// The choices Settings and the setup offer, in days; 0 is off.
pub const CHOICES: [u8; 4] = [0, 7, 14, 30];
/// What the one-time suggestion counts with, before the user picked.
pub const SUGGEST_DAYS: u8 = 14;

pub use crate::settings::Cleanup as Choice;

pub fn choice() -> Choice {
    crate::settings::read(|s| s.branches.remove_old_environments)
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
        Choice::Days(days)
    });
}

/// Notes that the suggestion was shown, so it is shown once.
pub fn set_suggested() {
    if choice() == Choice::Unset {
        set(Choice::Suggested);
    }
}

fn set(c: Choice) {
    crate::settings::change(|s| s.branches.remove_old_environments = c);
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
    /// An automation that is on runs here. Without the environment it would
    /// run in the main checkout instead.
    pub scheduled: bool,
    /// Unix seconds it was last opened or one of its sessions last moved.
    pub used: u64,
}

/// Whether the environment is worth asking git and GitHub about.
pub fn candidate(s: Seen, days: u8, now: u64) -> bool {
    !s.is_checkout
        && !s.visible
        && !s.busy
        && !s.scheduled
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

/// Ignored folders a build or package manager makes again, so losing them is
/// fine. Anything else ignored, like a `.env`, keeps the environment.
const REBUILDABLE: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    ".next",
    ".turbo",
    ".cache",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
];

/// Files the system leaves behind that nobody misses.
const JUNK: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// Whether one line of `git ls-files --others --ignored --directory` is safe
/// to lose: a whole rebuildable folder, or system junk.
fn expendable(entry: &str) -> bool {
    let (path, is_dir) = match entry.strip_suffix('/') {
        Some(dir) => (dir, true),
        None => (entry, false),
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    if is_dir {
        REBUILDABLE.contains(&name)
    } else {
        JUNK.contains(&name) || name.ends_with(".pyc")
    }
}

/// `git worktree remove` deletes ignored files without a word, and
/// `git status` never lists them, so ask for them on their own. `--directory`
/// lists an ignored folder once instead of every file in it.
fn ignored_files_kept(dir: &Path) -> bool {
    let args = [
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
        "-z",
    ];
    match crate::git::read(dir, &args) {
        Ok(out) => out.split('\0').any(|e| !e.is_empty() && !expendable(e)),
        // Can't tell: keep it.
        Err(_) => true,
    }
}

/// Asks git and GitHub whether the environment at `dir` can go: its pull
/// request is merged or closed, nothing is uncommitted, no ignored file other
/// than build output would be lost, and every commit is in the pull request.
/// Slow: runs git and gh.
pub fn removable(dir: &Path) -> bool {
    if !crate::git::status(dir).is_ok_and(|c| c.is_empty()) {
        return false;
    }
    if ignored_files_kept(dir) {
        return false;
    }
    pr_of(dir).is_some_and(|pr| pr.done() && head_in(dir, &pr.head))
}

/// Removes the environment at `dir` once `removable` said it can go. The pull
/// request lookup can be slow, and `git worktree remove` deletes an ignored
/// file created since without a word, so look for one again first.
pub fn remove_clean_worktree(repo: &Path, dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(!ignored_files_kept(dir), "the environment has local files");
    crate::git::remove_clean_worktree(repo, dir)
}

// ---------------------------------------------------------------------------
// Files left behind

/// How long a copy of a server's image stays in the temp folder. It is
/// fetched again when shown after a restart, so the age only spares a copy
/// another Arakun on this machine shows right now.
const SERVER_IMAGE_AGE: Duration = Duration::from_secs(7 * 86_400);

/// Removes files earlier runs left behind: voice clips a quit cut off, old
/// copies of servers' images in `images`, and the temp files of writes that
/// died. The server cleans its own folder (`server::store::Housekeeping`).
/// Slow on a big folder: call it off the UI thread.
pub fn sweep_leftovers(images: &Path) {
    use crate::housekeep::{DAY, remove_old, remove_write_leftovers};
    // Secret files and handles agents never used, now and every hour.
    crate::agent::secret::start_sweeper();
    let mut removed = remove_old(&std::env::temp_dir(), DAY, is_voice_clip);
    // The folder is in the shared temp folder: another user could put a link
    // there that points at the user's own files.
    if std::fs::symlink_metadata(images).is_ok_and(|m| m.is_dir()) {
        removed += remove_old(images, SERVER_IMAGE_AGE, is_server_image);
    }
    for dir in [
        crate::platform::app_dir(),
        crate::state::data_dir(),
        crate::paths::store(crate::paths::SESSIONS),
        crate::paths::store(crate::paths::LINKED),
        crate::paths::shared(crate::paths::CACHE),
    ] {
        removed += remove_write_leftovers(&dir);
    }
    if removed > 0 {
        eprintln!("arakun: removed {removed} files earlier runs left behind");
    }
}

/// `voice::temp_path`: `arakun-voice-<pid>-<n>.<ext>`.
fn is_voice_clip(name: &str) -> bool {
    name.strip_prefix("arakun-voice-")
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(stamp, ext)| {
            matches!(ext, "wav" | "pcm")
                && stamp
                    .split_once('-')
                    .is_some_and(|(pid, n)| is_number(pid) && is_number(n))
        })
}

/// `servers::keep_image`: 32 hex digits and an image extension.
fn is_server_image(name: &str) -> bool {
    name.split_once('.').is_some_and(|(hash, ext)| {
        hash.len() == 32
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
            && matches!(ext, "png" | "jpg" | "gif" | "webp")
    })
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
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
            scheduled: false,
            used,
        }
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
            Seen {
                scheduled: true,
                ..old
            },
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

    #[test]
    fn only_build_output_and_junk_may_be_lost() {
        for e in [
            "target/",
            "web/node_modules/",
            "a/b/__pycache__/",
            ".DS_Store",
            "src/.DS_Store",
            "x/y.pyc",
        ] {
            assert!(expendable(e), "{e}");
        }
        // A file named like a build folder is not one.
        for e in [
            ".env",
            "notes.md",
            "config/local.toml",
            "target",
            "secrets/",
        ] {
            assert!(!expendable(e), "{e}");
        }
    }

    #[test]
    fn an_ignored_file_keeps_the_environment() {
        let dir = std::env::temp_dir().join(format!("arakun-cleanup-ign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = crate::git::git(&dir)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        assert!(out.status.success());
        std::fs::write(dir.join(".gitignore"), "target/\n.env\n").unwrap();
        assert!(!ignored_files_kept(&dir));
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/app"), "bin").unwrap();
        std::fs::write(dir.join(".DS_Store"), "").unwrap();
        assert!(!ignored_files_kept(&dir));
        std::fs::write(dir.join(".env"), "TOKEN=1").unwrap();
        assert!(ignored_files_kept(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_ignored_file_made_late_keeps_the_worktree() {
        let root = std::env::temp_dir().join(format!("arakun-cleanup-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, env) = (root.join("repo"), root.join("env"));
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = crate::git::git(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        git(&["add", ".gitignore"]);
        git(&["commit", "-q", "-m", "one"]);
        git(&["worktree", "add", "-q", "-b", "env", env.to_str().unwrap()]);
        // Written after the environment was found removable.
        std::fs::write(env.join(".env"), "TOKEN=1").unwrap();
        assert!(remove_clean_worktree(&repo, &env).is_err());
        assert!(env.join(".env").exists());
        std::fs::remove_file(env.join(".env")).unwrap();
        remove_clean_worktree(&repo, &env).unwrap();
        assert!(!env.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_voice_clips_are_taken_from_the_temp_folder() {
        assert!(is_voice_clip("arakun-voice-123-0.wav"));
        assert!(is_voice_clip("arakun-voice-1-42.pcm"));
        assert!(!is_voice_clip("arakun-voice-123-0.mp3"));
        assert!(!is_voice_clip("arakun-voice-x-0.wav"));
        assert!(!is_voice_clip("arakun-voice-123.wav"));
        assert!(!is_voice_clip("arakun-cleanup-123"));
    }

    #[test]
    fn only_server_image_copies_are_taken() {
        assert!(is_server_image("0123456789abcdef0123456789abcdef.png"));
        assert!(!is_server_image("0123456789abcdef0123456789abcdef.txt"));
        assert!(!is_server_image("notes.png"));
        assert!(!is_server_image(
            ".0123456789abcdef0123456789abcdef.png.1-0.tmp"
        ));
    }
}
