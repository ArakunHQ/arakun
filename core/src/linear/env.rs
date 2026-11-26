//! Making an environment on a branch named by someone else (an issue
//! delegated in Linear): the reasons it fails, and one creation per branch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git;

pub const NOT_HERE: &str = "the project is not on this computer";
pub const BAD_NAME: &str = "the branch name is not one git accepts";

/// Whether making the environment failed over the branch's name: git does
/// not take it, or a branch the project has is in its way (`a` for `a/b`).
/// Another name can then work where this one cannot.
pub fn name_refused(error: &str) -> bool {
    error == BAD_NAME || error.contains("cannot lock ref")
}

/// The line of git's error that says why. `git worktree add` first prints
/// what it is about to do, so the first line alone can say nothing.
pub fn why(error: &str) -> &str {
    let lines = || error.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .find(|l| l.starts_with("fatal: ") || l.starts_with("error: "))
        .or_else(|| lines().next())
        .unwrap_or_default()
}

/// Creations still running, by project root and branch, with who waits on each.
pub struct InFlight<T> {
    waiting: HashMap<(PathBuf, String), Vec<T>>,
}

impl<T> Default for InFlight<T> {
    fn default() -> Self {
        Self {
            waiting: HashMap::new(),
        }
    }
}

impl<T> InFlight<T> {
    /// Adds `waiter`; true when nothing was running for this key yet, so the
    /// caller starts the creation.
    pub fn join(&mut self, project: &Path, branch: &str, waiter: T) -> bool {
        let list = self
            .waiting
            .entry((project.to_path_buf(), branch.to_string()))
            .or_default();
        list.push(waiter);
        list.len() == 1
    }

    /// Everyone who waited on this key, now that its creation is over.
    pub fn finish(&mut self, project: &Path, branch: &str) -> Vec<T> {
        self.waiting
            .remove(&(project.to_path_buf(), branch.to_string()))
            .unwrap_or_default()
    }
}

/// The worktree of `repo` on `branch`, and whether it was made now. One git
/// already has on the branch is used as it is. A branch only on origin is
/// checked out from there; a new one starts from the project's base.
/// It blocks on git, so it runs off the UI thread.
pub fn worktree_on(
    repo: &Path,
    project: &str,
    branch: &str,
    start: Option<&str>,
) -> Result<(PathBuf, bool), String> {
    let said = |e: anyhow::Error| format!("git said \"{}\"", why(&e.to_string()));
    if !git::branch_arg_ok(repo, branch) {
        return Err(BAD_NAME.into());
    }
    // Listed here or not, the worktree git has on the branch now is used as it is.
    if let Some(tree) = git::worktrees(repo)
        .map_err(said)?
        .into_iter()
        .find(|t| t.branch.as_deref() == Some(branch))
    {
        return Ok((tree.path, false));
    }
    if !git::has_commits(repo).map_err(said)? {
        return Err("the project has no commits yet".into());
    }
    let mut branches = git::branches(repo).map_err(said)?;
    let local = branches.local.iter().any(|b| b == branch);
    // Pushed from another computer and not fetched here yet, it would look
    // new, and a fresh branch made from the base could not be pushed later.
    if !local && git::fetch_branch(repo, branch) {
        branches = git::branches(repo).map_err(said)?;
    }
    let path = crate::model::worktree_path(project, branch);
    let on_origin = branches
        .remote
        .iter()
        .any(|b| b == &format!("origin/{branch}"));
    if !local && on_origin {
        git::add_branch_worktree(repo, &path, branch).map_err(said)?;
    } else {
        let base = git::new_env_base(repo, start);
        git::add_worktree(repo, &path, branch, base.as_deref(), false).map_err(said)?;
    }
    Ok((path, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_git_refuses_is_told_from_other_failures() {
        // What `git worktree add -b a/b` prints when a branch `a` exists.
        let taken = "Preparing worktree (new branch 'a/b')\n\
                     fatal: cannot lock ref 'refs/heads/a/b': 'refs/heads/a' exists; \
                     cannot create 'refs/heads/a/b'";
        assert_eq!(
            why(taken),
            "fatal: cannot lock ref 'refs/heads/a/b': 'refs/heads/a' exists; \
             cannot create 'refs/heads/a/b'"
        );
        assert!(name_refused(&format!("git said \"{}\"", why(taken))));
        assert!(name_refused(BAD_NAME));
        assert!(!name_refused(NOT_HERE));
        assert!(!name_refused("git said \"'/w/x' already exists\""));
        assert!(!name_refused("the project has no commits yet"));
        // Without a line that names the failure, the first one is shown.
        assert_eq!(why("'/w/x' already exists\nmore"), "'/w/x' already exists");
        assert_eq!(why(""), "");
    }

    #[test]
    fn a_second_call_for_the_same_branch_waits_on_the_first() {
        let mut flight = InFlight::default();
        let root = Path::new("/r");
        assert!(flight.join(root, "linear/ara-1", 1));
        assert!(!flight.join(root, "linear/ara-1", 2));
        // Another branch, or the same branch of another project, starts its own.
        assert!(flight.join(root, "linear/ara-2", 3));
        assert!(flight.join(Path::new("/other"), "linear/ara-1", 4));
        assert_eq!(flight.finish(root, "linear/ara-1"), vec![1, 2]);
        assert!(flight.finish(root, "linear/ara-1").is_empty());
        // Once over, the next call starts a new one.
        assert!(flight.join(root, "linear/ara-1", 5));
    }
}
