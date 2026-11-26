//! An environment on a branch named by someone else (an issue delegated in
//! Linear), made in the background: nothing on screen changes for it.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};

use super::{PendingEnv, Shell};
use crate::git;
use crate::model::{self, Environment};

/// Called once the environment is there, with its folder, or why it is not.
pub(super) type Waiter =
    Box<dyn FnOnce(&mut Shell, Result<PathBuf, String>, &mut Window, &mut Context<Shell>)>;

pub(super) use crate::linear::env::*;

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
