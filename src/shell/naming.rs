//! Renaming an environment's branch: by the namer after the first prompt of an
//! environment with a random name, or by `arakun rename <name>`. The worktree
//! folder stays where it is, since terminals, browser tabs and agent sessions
//! work in it; the rail shows the new branch instead.

use std::path::Path;

use futures::channel::oneshot;
use gpui::{AppContext as _, Context};

use super::Shell;
use crate::control::{Reply, Request};
use crate::env_name;
use crate::git;
use crate::island::AlertKind;

impl Shell {
    /// After the first prompt in an environment whose branch still has the
    /// dialog's random name, asks the namer for a name that says what the work is
    /// and renames the branch to it. Runs once, whatever comes of it.
    pub(super) fn name_after_first_prompt(
        &mut self,
        env_path: &Path,
        prompt: &str,
        cx: &mut Context<Self>,
    ) {
        // A bare command such as `/stage` says nothing about the task; wait for a real prompt.
        let prompt = prompt.trim();
        if prompt.is_empty() || (prompt.starts_with('/') && !prompt.contains(char::is_whitespace)) {
            return;
        }
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        if !env.auto_name {
            return;
        }
        env.auto_name = false;
        let old = env.branch.to_string();
        self.save(cx);
        let namer = env_name::namer();
        // Renamed by hand in the meantime, or naming is off.
        if !env_name::is_random(&old) || namer.agent.is_none() {
            return;
        }
        let (dir, task) = (env_path.to_path_buf(), prompt.to_string());
        cx.spawn(async move |this, cx| {
            let d = dir.clone();
            let o = old.clone();
            let renamed = cx
                .background_spawn(async move {
                    let name = env_name::suggest(&namer, &task)?;
                    git::rename_branch(&d, &o, &name, true)
                })
                .await;
            let _ = this.update(cx, |this, cx| match renamed {
                Ok(name) => this.branch_renamed(&dir, name, cx),
                Err(e) => this.notify(
                    AlertKind::Warning,
                    "Could not name the branch",
                    &format!("{e:#}. It stays {old}; `arakun rename <name>` renames it."),
                    None,
                ),
            });
        })
        .detach();
    }

    /// `arakun rename <name>`: renames the branch of the environment the command
    /// ran in. Answers once git is done.
    pub(super) fn rename_command(
        &mut self,
        req: Request,
        done: oneshot::Sender<Reply>,
        cx: &mut Context<Self>,
    ) {
        let Some(env_path) = self.command_env(&req) else {
            let _ = done.send(Reply::err("no project is open in arakun"));
            return;
        };
        let Some(env) = self.find_env(&env_path) else {
            let _ = done.send(Reply::err("no environment here"));
            return;
        };
        if env.is_checkout {
            let _ = done.send(Reply::err(
                "this is the project's main checkout; arakun rename only renames the branch of an environment",
            ));
            return;
        }
        let new = req.args.first().cloned().unwrap_or_default();
        let dir = env_path.clone();
        cx.spawn(async move |this, cx| {
            let d = dir.clone();
            // `env.branch` only changes on a project refresh; a terminal may have switched since.
            let renamed = cx
                .background_spawn(async move {
                    let old = git::current_branch(&d).ok_or_else(|| {
                        anyhow::anyhow!("HEAD is detached; there is no branch to rename")
                    })?;
                    git::rename_branch(&d, &old, &new, false).map(|name| (old, name))
                })
                .await;
            let reply = match renamed {
                Ok((old, name)) => {
                    let message = format!(
                        "renamed {old} to {name}; the folder stays {}",
                        dir.display()
                    );
                    let _ = this.update(cx, |this, cx| this.branch_renamed(&dir, name, cx));
                    Reply::ok(message)
                }
                Err(e) => Reply::err(format!("{e:#}")),
            };
            let _ = done.send(reply);
        })
        .detach();
    }

    /// Shows the environment at `dir` by its new branch name.
    fn branch_renamed(&mut self, dir: &Path, name: String, cx: &mut Context<Self>) {
        if let Some(env) = self.find_env_mut(dir) {
            env.auto_name = false;
            env.set_branch(Some(name));
            env.follow_branch();
        }
        self.save(cx);
        if let Some(pi) = self
            .projects
            .iter()
            .position(|p| p.env_index(dir).is_some())
        {
            self.refresh_project(pi, cx);
        }
        cx.notify();
    }
}
