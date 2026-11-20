//! The Branches tab: opening it, telling it the visible project's environments
//! and sessions, and what its buttons do (PR tabs, environments, sessions).

use std::path::PathBuf;

use gpui::{Context, SharedString, Window};

use super::island::Open;
use super::*;
use crate::branches_view::{BranchesEvent, EnvInfo, SessionInfo};
use crate::island::AlertKind;

impl Shell {
    /// Opens (or focuses) the Branches tab of the visible environment.
    pub(super) fn open_branches_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            self.pick_project_for("The Branches tab", Self::open_branches_tab, window, cx);
            return;
        }
        self.show_view_tab("Branches", TabKind::Branches, |k| {
            matches!(k, TabKind::Branches)
        });
        self.note_active_tab(cx);
        self.sync_branches_view(cx);
        self.branches_view.update(cx, |v, cx| {
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Branches of the visible project with conflicts or failing checks; reads
    /// the map in the background now and then so the count stays true.
    pub(super) fn branches_badge(&self, cx: &mut Context<Self>) -> usize {
        if !self.has_projects() {
            return 0;
        }
        let root = self.project().path.clone();
        self.branches_view.update(cx, |v, cx| v.watch(root, cx));
        self.branches_view.read(cx).alarms()
    }

    /// Opens the Branches tab with `branch` selected.
    pub(super) fn reveal_branch(
        &mut self,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_branches_tab(window, cx);
        self.pin_preview(cx);
        self.branches_view.update(cx, |v, cx| v.reveal(branch, cx));
    }

    /// Hands the Branches tab the visible project and what runs in each environment.
    pub(super) fn sync_branches_view(&self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.branches_view
                .update(cx, |v, cx| v.set_project(None, Vec::new(), cx));
            return;
        }
        let pi = self.active_project;
        let project = self.project();
        let envs = project
            .environments
            .iter()
            .enumerate()
            .map(|(ei, e)| EnvInfo {
                project: pi,
                env: ei,
                path: e.path.clone(),
                name: e.name.clone(),
                branch: e.branch.to_string(),
                sessions: e
                    .sessions
                    .iter()
                    .map(|s| SessionInfo {
                        id: s.id.clone(),
                        title: s.title.clone(),
                        agent: s.agent,
                        status: s.status,
                    })
                    .collect(),
            })
            .collect();
        let root = project.path.clone();
        self.branches_view.update(cx, |v, cx| {
            v.set_project(Some(root), envs, cx);
            v.refresh_if_stale(cx);
        });
    }

    pub(super) fn on_branches_event(
        &mut self,
        ev: &BranchesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        match ev {
            BranchesEvent::Open(url) => {
                let env_path = self.env().path.clone();
                self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
            }
            BranchesEvent::OpenEnv { project, env } => self.select_env(*project, *env, window, cx),
            BranchesEvent::OpenSession { project, env, id } => {
                self.open_session_at(*project, *env, id.clone(), window, cx)
            }
            BranchesEvent::Ask {
                path,
                title,
                prompt,
            } => {
                let pi = self.active_project;
                if let Some(ei) = self.projects[pi].env_index(path) {
                    self.select_env(pi, ei, window, cx);
                }
                let agent = self.env().agent;
                self.start_session_in(
                    path,
                    agent,
                    SharedString::from(title.clone()),
                    prompt.clone(),
                    true,
                    window,
                    cx,
                );
            }
            BranchesEvent::CreateEnv(branch) => self.check_out_branch(branch.clone(), window, cx),
            BranchesEvent::ChecksDone {
                title,
                body,
                failed,
            } => {
                let watching = window.is_window_active()
                    && matches!(
                        self.strip.active().map(|t| &t.kind),
                        Some(TabKind::Branches)
                    );
                if !watching {
                    let kind = if *failed {
                        AlertKind::Failure
                    } else {
                        AlertKind::Success
                    };
                    self.notify(
                        kind,
                        title,
                        body,
                        Some(Open::Branches {
                            root: self.project().path.clone(),
                        }),
                        cx,
                    );
                }
            }
        }
    }

    /// A new environment (worktree) on `branch`, then shows it.
    fn check_out_branch(&mut self, branch: String, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project();
        let wt: PathBuf = model::worktree_path(&project.name, &branch);
        let repo_root = project.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (r, p, b) = (repo_root.clone(), wt.clone(), branch.clone());
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    git::add_branch_worktree(&r, &p, &b)
                }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    let Some(pi) = this.projects.iter().position(|p| p.path == repo_root) else {
                        return;
                    };
                    if this.projects[pi].env_index(&wt).is_none() {
                        this.projects[pi].environments.push(Environment::new(
                            wt.clone(),
                            Some(branch.clone()),
                            false,
                        ));
                    }
                    this.refresh_project(pi, cx);
                    if let Some(ei) = this.projects[pi].env_index(&wt) {
                        this.select_env(pi, ei, window, cx);
                    }
                }
                Err(e) => {
                    let open = Open::Branches { root: repo_root };
                    this.notify(
                        AlertKind::Failure,
                        &format!("Couldn't check out {branch}"),
                        &format!("{e}"),
                        Some(open),
                        cx,
                    );
                }
            });
        })
        .detach();
    }
}
