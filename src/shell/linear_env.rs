//! An environment on a branch named by someone else (an issue delegated in
//! Linear), made in the background: nothing on screen changes for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};

use super::{PendingEnv, Shell};
use crate::git;
use crate::model::{self, Environment};

/// Called once the environment is there, with its folder, or why it is not.
pub(super) type Waiter =
    Box<dyn FnOnce(&mut Shell, Result<PathBuf, String>, &mut Window, &mut Context<Shell>)>;

const NOT_HERE: &str = "the project is not on this computer";

/// Creations still running, by project root and branch, with who waits on each.
pub(super) struct InFlight<T> {
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
    pub(super) fn join(&mut self, project: &Path, branch: &str, waiter: T) -> bool {
        let list = self
            .waiting
            .entry((project.to_path_buf(), branch.to_string()))
            .or_default();
        list.push(waiter);
        list.len() == 1
    }

    /// Everyone who waited on this key, now that its creation is over.
    pub(super) fn finish(&mut self, project: &Path, branch: &str) -> Vec<T> {
        self.waiting
            .remove(&(project.to_path_buf(), branch.to_string()))
            .unwrap_or_default()
    }
}

impl Shell {
    /// The environment of the project at `project` on `branch`, made when
    /// there is none, then `then` with its folder. For work nobody started
    /// by hand: the view, the tabs and focus stay as they are.
    pub(super) fn env_on_branch(
        &mut self,
        project: &Path,
        branch: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shell, Result<PathBuf, String>, &mut Window, &mut Context<Shell>)
        + 'static,
    ) {
        let Some(p) = self
            .projects
            .iter()
            .find(|p| p.path == project && !p.is_remote())
        else {
            then(self, Err(NOT_HERE.into()), window, cx);
            return;
        };
        // Git decides which worktree is on the branch, even when one is
        // listed here: a `git switch` in a terminal leaves the list behind.
        let listed = p
            .environments
            .iter()
            .any(|e| e.remote.is_none() && e.branch.as_ref() == branch);
        let (repo, name, start) = (p.path.clone(), p.name.clone(), p.base.clone());
        if !self.linear_envs.join(&repo, branch, Box::new(then)) {
            return;
        }
        // A row shows in the sidebar while git works, unless one likely is there.
        if !listed {
            self.pending_envs.push(PendingEnv {
                repo: repo.clone(),
                name: branch.to_string(),
            });
            cx.notify();
        }
        let branch = branch.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let (r, b) = (repo.clone(), branch.clone());
            let made = cx
                // A panic's text could hold anything; Linear gets a fixed reason.
                .background_spawn(crate::guard::task_or(
                    async move { worktree_on(&r, &name, &b, start.as_deref()) },
                    |_| Err(super::linear_run::HIT_AN_ERROR.to_string()),
                ))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending_envs
                    .retain(|p| !(p.repo == repo && p.name == branch));
                let result = made.and_then(|(path, created)| {
                    this.env_made(&repo, &branch, &path, created, window, cx)?;
                    Ok(path)
                });
                for waiter in this.linear_envs.finish(&repo, &branch) {
                    waiter(this, result.clone(), window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Lists the worktree at `path` under its project, as
    /// `environment_created` does, but without selecting it or moving focus.
    fn env_made(
        &mut self,
        repo: &Path,
        branch: &str,
        path: &Path,
        created: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // The project can have moved in the list, or gone, while git worked.
        let pi = self
            .projects
            .iter()
            .position(|p| p.path == repo)
            .ok_or_else(|| "the project was removed while the environment was made".to_string())?;
        let project = &mut self.projects[pi];
        if project.env_index(path).is_none() {
            // Not auto-named: the branch was chosen for it and must stay.
            project.environments.push(Environment::new(
                path.to_path_buf(),
                Some(branch.to_string()),
                false,
            ));
        }
        let setup = project.setup.clone().filter(|_| created);
        if created {
            crate::telemetry::capture(
                crate::telemetry::events::ENVIRONMENT_CREATED,
                &[("auto_name", "false")],
            );
        }
        self.refresh_project(pi, cx);
        // A worktree that was already there has had its setup. The terminal
        // stays in the new environment's dock, unseen until someone opens it.
        if let Some(script) = setup {
            self.run_in_terminal(path, &script, false, window, cx);
        }
        Ok(())
    }
}

/// Off the UI thread: the worktree of `repo` on `branch`, and whether it was
/// made now. A branch only on origin is checked out from there; a new one
/// starts where `quick_create_env_then` would start it. The branch name
/// comes from outside, so it is checked before git sees it.
fn worktree_on(
    repo: &Path,
    project: &str,
    branch: &str,
    start: Option<&str>,
) -> Result<(PathBuf, bool), String> {
    let said = |e: anyhow::Error| {
        let e = e.to_string();
        let line = e.lines().next().unwrap_or("").trim();
        format!("git said \"{line}\"")
    };
    if !git::branch_arg_ok(repo, branch) {
        return Err("the branch name is not one git accepts".into());
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
    let path = model::worktree_path(project, branch);
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
