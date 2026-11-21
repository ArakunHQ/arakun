//! What the Changes panel asks git in an environment's folder, as one request
//! ([`Call`]). The app runs it in its own folders, and a paired server runs it
//! in its folders when the app asks (`ClientMsg::Git`): both go through
//! [`Call::run`], so a server's environment shows and does what a local one does.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::git::{self, Change, PrCommit};

/// One thing to do or read with git in a folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Call {
    /// The changes and where the branch stands: a [`Status`].
    Status,
    /// The diff of one changed file: a [`DiffText`]. Past `limit` bytes it
    /// says how large it is instead, so a huge one does not travel.
    Diff {
        path: String,
        untracked: bool,
        #[serde(default)]
        limit: Option<u64>,
    },
    Stage {
        path: String,
    },
    Unstage {
        path: String,
    },
    Discard {
        change: Change,
    },
    Commit {
        message: String,
        all: bool,
    },
    Push {
        remote: String,
        branch: String,
    },
    Pull,
    Fetch,
    /// The checked-out branch's pull request, when it has one.
    PrView,
    /// What a new pull request starts with: a `git::PrPlan`. With `onto`,
    /// for that base, and nothing when the remote has no such branch.
    PrPlan {
        remote: String,
        branch: String,
        #[serde(default)]
        onto: Option<String>,
    },
    /// Pushes when it has to and opens the pull request: its address.
    PrCreate(NewPr),
    /// What a model writes a pull request's description from: a [`PrSource`].
    PrSource {
        remote: String,
        base: String,
        branch: String,
        max_diff: usize,
    },
    /// What a model writes a commit message from: a [`CommitSource`].
    CommitSource {
        all: bool,
        max_diff: usize,
    },
}

/// The changes in a folder and where its branch stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub changes: Vec<Change>,
    pub sync: git::Sync,
}

/// A file's diff, or its size when that is past what a tab shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffText {
    Text(String),
    TooLarge(u64),
}

/// A pull request to open from the Create pull request dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPr {
    pub remote: String,
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// What a model reads to write a pull request (`pr_writer`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSource {
    pub commits: Vec<PrCommit>,
    /// `git diff --stat`: every changed file with its counts.
    pub stat: String,
    /// The diff, cut at `max_diff` characters.
    pub diff: String,
    pub cut: bool,
    pub template: Option<String>,
}

/// What a model reads to write a commit message (`pr_writer::commit`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSource {
    /// `git diff --stat` of what the commit takes, new files listed too.
    pub stat: String,
    /// The diff, cut at `max_diff` characters.
    pub diff: String,
    pub cut: bool,
    /// The last commits' subjects, newest first: the style to follow.
    pub recent: Vec<String>,
}

impl Call {
    /// Runs it in `dir`, blocking. What it found, as JSON: the same on both
    /// sides of a server's connection.
    pub fn run(self, dir: &Path) -> Result<Value> {
        Ok(match self {
            Call::Status => json!(Status {
                changes: git::status(dir)?,
                sync: git::sync(dir)?,
            }),
            Call::Diff {
                path,
                untracked,
                limit,
            } => {
                inside(&path)?;
                json!(diff(dir, &path, untracked, limit)?)
            }
            Call::Stage { path } => {
                inside(&path)?;
                json!(git::stage(dir, &path)?)
            }
            Call::Unstage { path } => {
                inside(&path)?;
                json!(git::unstage(dir, &path)?)
            }
            Call::Discard { change } => {
                inside(&change.path)?;
                json!(git::discard(dir, &change)?)
            }
            Call::Commit { message, all } => json!(git::commit(dir, &message, all)?),
            Call::Push { remote, branch } => {
                // A read-only Arakun sends nothing, the branch included.
                if crate::github::read_only() {
                    bail!("{}", crate::github::pr::READ_ONLY);
                }
                name(&remote)?;
                name(&branch)?;
                json!(git::push(dir, &remote, &branch)?)
            }
            Call::Pull => json!(git::pull(dir)?),
            Call::Fetch => json!(git::fetch(dir)?),
            Call::PrView => json!(git::pr_view(dir)),
            Call::PrPlan {
                remote,
                branch,
                onto: None,
            } => json!(git::pr_plan(dir, &remote, &branch)),
            Call::PrPlan {
                remote,
                branch,
                onto: Some(onto),
            } => json!(git::pr_plan_onto(dir, &remote, &branch, &onto)),
            Call::PrCreate(pr) => json!(create_pr(dir, &pr, false)?),
            Call::PrSource {
                remote,
                base,
                branch,
                max_diff,
            } => json!(pr_source(dir, &remote, &base, &branch, max_diff)),
            Call::CommitSource { all, max_diff } => json!(commit_source(dir, all, max_diff)?),
        })
    }

    /// The longest it may take on the other side. A commit runs its hooks,
    /// and a push or pull goes as fast as the network.
    pub fn limit(&self) -> Duration {
        match self {
            Call::Commit { .. } | Call::Push { .. } | Call::Pull | Call::PrCreate(_) => {
                crate::platform::GIT_LONG
            }
            // Several reads, each with its own limit.
            Call::PrPlan { .. } | Call::PrSource { .. } | Call::CommitSource { .. } => {
                crate::platform::GIT_REMOTE * 3
            }
            _ => crate::platform::GIT_REMOTE,
        }
    }
}

/// Refuses a path that leaves the folder: a server takes these from a device.
/// Rooted and drive paths count on every platform, since the server may run
/// on another one than the app.
fn inside(path: &str) -> Result<()> {
    if path.is_empty()
        || Path::new(path).is_absolute()
        || path.starts_with(['/', '\\'])
        || path.as_bytes().get(1) == Some(&b':')
        || path.split(['/', '\\']).any(|c| c == "..")
    {
        bail!("refusing to touch {path}");
    }
    Ok(())
}

/// Refuses a remote or branch git would read as a flag: a server takes
/// these from a device too.
fn name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('-') {
        bail!("refusing to push to {name:?}");
    }
    Ok(())
}

/// The diff of `path`, or its size once it is past `limit`.
pub fn diff(dir: &Path, path: &str, untracked: bool, limit: Option<u64>) -> Result<DiffText> {
    // A new file that large makes a diff at least as large: git need not
    // read it. A tracked one's diff may be a line of it.
    if untracked
        && let Some(limit) = limit
        && let Ok(meta) = std::fs::metadata(dir.join(path))
        && meta.len() > limit
    {
        return Ok(DiffText::TooLarge(meta.len()));
    }
    let text = git::diff(dir, path, untracked)?;
    Ok(match limit {
        Some(limit) if text.len() as u64 > limit => DiffText::TooLarge(text.len() as u64),
        _ => DiffText::Text(text),
    })
}

/// Opens the pull request `pr` from the branch checked out in `dir`, pushing
/// it first when the remote lacks commits. `web` opens GitHub's form in this
/// machine's browser instead: then there is no address to give back.
pub fn create_pr(dir: &Path, pr: &NewPr, web: bool) -> Result<Option<String>> {
    let NewPr {
        remote,
        branch,
        base,
        title,
        body,
        draft,
    } = pr;
    // The push state below is the checkout's: of another branch it would
    // say nothing about this one.
    let on = git::current_branch(dir);
    if on.as_deref() != Some(branch.as_str()) {
        bail!(
            "The folder is on {} now. Check out {branch} to create its pull request.",
            on.as_deref().unwrap_or("no branch")
        );
    }
    // Read again: a commit made while the dialog was open would stay behind,
    // and gh pushes nothing with --head. Not knowing is no reason to skip
    // the push.
    let Some(needs_push) = git::needs_push(dir, remote) else {
        bail!("Couldn't read the branch's state, so nothing was pushed or created.");
    };
    if needs_push {
        // A read-only Arakun sends nothing, the branch included.
        if crate::github::read_only() {
            bail!("{}", crate::github::pr::READ_ONLY);
        }
        git::push(dir, remote, branch)?;
    }
    // After the push: on a fork the head names its owner.
    let head = git::pr_head(dir, remote, branch);
    if web {
        git::pr_create_web(dir, &head, base, title).map(|()| None)
    } else {
        git::pr_create(dir, &head, base, title, body, *draft).map(Some)
    }
}

/// Reads what a model writes a pull request for `branch` into `base` from.
pub fn pr_source(dir: &Path, remote: &str, base: &str, branch: &str, max_diff: usize) -> PrSource {
    // The branch the dialog names, not whatever is checked out by now, and
    // one commit for every read, also when the branch moves meanwhile.
    let tip = git::branch_tip(dir, branch).unwrap_or_else(|| "HEAD".into());
    let (stat, diff, cut) = git::pr_diff(dir, remote, base, &tip, max_diff);
    PrSource {
        commits: git::pr_commits_at(dir, remote, base, &tip),
        stat,
        diff,
        cut,
        template: git::pr_template(dir, remote),
    }
}

/// Reads what a model writes the message of a commit from: all changes, or
/// only the staged ones.
pub fn commit_source(dir: &Path, all: bool, max_diff: usize) -> Result<CommitSource> {
    let (stat, diff, cut) = git::commit_diff(dir, all, max_diff)?;
    Ok(CommitSource {
        stat,
        diff,
        cut,
        recent: git::recent_subjects(dir, RECENT),
    })
}

/// How many earlier subjects show a model the repository's style.
const RECENT: usize = 12;

/// The server's answer to request `reference`. A git that ran out of time
/// says so, so the app waits before it asks again, as it does with its own.
pub fn reply(reference: u64, result: Result<Value>) -> Value {
    match result {
        Ok(value) => json!({ "type": "git", "ref": reference, "ok": value }),
        Err(error) if git::timed_out(&error) => json!({
            "type": "git", "ref": reference, "error": error.to_string(), "timed_out": true,
        }),
        Err(error) => json!({ "type": "git", "ref": reference, "error": error.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_reads_back_as_it_was_written() {
        let calls = [
            Call::Status,
            Call::Diff {
                path: "src/a.rs".into(),
                untracked: true,
                limit: Some(8),
            },
            Call::Commit {
                message: "fix: it".into(),
                all: false,
            },
            Call::PrCreate(NewPr {
                remote: "origin".into(),
                branch: "fix".into(),
                base: "main".into(),
                title: "fix: it".into(),
                body: "why".into(),
                draft: true,
            }),
        ];
        for call in calls {
            let text = serde_json::to_string(&call).unwrap();
            assert_eq!(serde_json::from_str::<Call>(&text).unwrap(), call);
        }
        assert_eq!(
            serde_json::to_value(Call::Pull).unwrap(),
            json!({ "op": "pull" })
        );
    }

    #[test]
    fn paths_that_leave_the_folder_are_refused() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a\\..\\x",
            "\\Windows\\x",
            "C:\\x",
            "C:x",
        ] {
            assert!(inside(bad).is_err(), "{bad}");
        }
        for good in ["a.rs", "src/a.rs", "a..b/c"] {
            assert!(inside(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn a_diff_past_its_limit_says_its_size() {
        let dir = std::env::temp_dir().join(format!("arakun-git-call-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::init(&dir).unwrap();
        std::fs::write(dir.join("big.txt"), "x".repeat(100)).unwrap();
        assert_eq!(
            diff(&dir, "big.txt", true, Some(10)).unwrap(),
            DiffText::TooLarge(100)
        );
        // The file is under the limit and its diff, with git's lines, over it.
        let DiffText::TooLarge(size) = diff(&dir, "big.txt", true, Some(100)).unwrap() else {
            panic!("shown whole");
        };
        assert!(size > 100);
        let DiffText::Text(text) = diff(&dir, "big.txt", true, None).unwrap() else {
            panic!("no diff");
        };
        assert!(text.contains("+xxx"));
        // A large tracked file with a small change shows that change.
        git::stage(&dir, "big.txt").unwrap();
        let committed = std::process::Command::new("git")
            .current_dir(&dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "big",
            ])
            .status()
            .unwrap();
        assert!(committed.success());
        std::fs::write(dir.join("big.txt"), "x".repeat(99) + "y").unwrap();
        let DiffText::Text(text) = diff(&dir, "big.txt", false, Some(1000)).unwrap() else {
            panic!("a small diff said too large");
        };
        assert!(text.contains("+xxx"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_git_would_read_as_flags_are_refused() {
        let push = |remote: &str, branch: &str| Call::Push {
            remote: remote.into(),
            branch: branch.into(),
        };
        let dir = Path::new("/nonexistent");
        for (remote, branch) in [("--receive-pack=x", "main"), ("origin", "-f"), ("", "main")] {
            let err = push(remote, branch).run(dir).unwrap_err().to_string();
            assert!(err.contains("refusing"), "{err}");
        }
        let discard = Call::Discard {
            change: Change {
                path: "C:\\x".into(),
                kind: git::ChangeKind::Modified,
                added: 0,
                removed: 0,
                staged: false,
                unstaged: true,
            },
        };
        assert!(discard.run(dir).is_err());
    }

    #[test]
    fn a_reply_carries_its_request() {
        assert_eq!(
            reply(3, Ok(json!(null))),
            json!({ "type": "git", "ref": 3, "ok": null })
        );
        assert_eq!(
            reply(4, Err(anyhow::anyhow!("no"))),
            json!({ "type": "git", "ref": 4, "error": "no" })
        );
        let slow = std::io::Error::new(std::io::ErrorKind::TimedOut, "git status took too long");
        assert_eq!(
            reply(5, Err(slow.into())),
            json!({ "type": "git", "ref": 5, "error": "git status took too long", "timed_out": true })
        );
    }
}
