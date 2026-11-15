//! The Runs tab (GitHub Actions): opening it, its rail badge and status-bar
//! chips, desktop notices for finished runs, and handing a failure to Droid.

use std::collections::HashMap;
use std::path::Path;

use gpui::{App, Context, SharedString, Window};

use super::island::Open;
use super::*;
use crate::ci::{self, CiEvent, State};
use crate::island::AlertKind;
use crate::runs_view::LinkedSession;
use crate::runs_view::RunsEvent;

impl Shell {
    /// Opens (or focuses) the Runs tab of the visible environment, at a run's page
    /// when `run` is set.
    pub(super) fn open_runs_tab(
        &mut self,
        run: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Runs", TabKind::Runs, |k| matches!(k, TabKind::Runs));
        self.note_active_tab();
        self.sync_runs_view(cx);
        let root = self.project().path.clone();
        self.ci.update(cx, |ci, cx| {
            ci.refresh_if_stale(&root, Duration::from_secs(20), cx)
        });
        self.runs_view.update(cx, |v, cx| {
            if run.is_some() {
                v.select(run, cx);
            }
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Tells the Ci model which projects to follow.
    pub(super) fn sync_ci_roots(&self, cx: &mut Context<Self>) {
        let projects = self
            .projects
            .iter()
            .map(|p| {
                (
                    p.path.clone(),
                    p.environments.iter().map(|e| e.path.clone()).collect(),
                )
            })
            .collect();
        self.ci.update(cx, |ci, cx| ci.watch(projects, cx));
    }

    /// Hands the Runs tab the visible project and the fix sessions of its runs.
    pub(super) fn sync_runs_view(&self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.runs_view.update(cx, |v, _| {
                v.set_project(None, SharedString::default(), Vec::new(), None)
            });
            return;
        }
        let pi = self.active_project;
        let p = self.project();
        let branches = p.environments.iter().map(|e| e.branch.clone()).collect();
        let mut linked: HashMap<u64, Vec<LinkedSession>> = HashMap::new();
        let mut prs: HashMap<String, LinkedSession> = HashMap::new();
        for (ei, e) in p.environments.iter().enumerate() {
            for s in &e.sessions {
                let link = LinkedSession {
                    project: pi,
                    env: ei,
                    id: s.id.clone(),
                    title: s.title.clone(),
                    agent: s.agent,
                    status: s.status,
                    updated: s.updated,
                };
                if let Some(url) = s.meta.as_ref().and_then(|m| m.pr.clone())
                    && prs.get(&url).is_none_or(|o| o.updated < s.updated)
                {
                    prs.insert(url, link.clone());
                }
                if let Some(run) = s.ci_run {
                    linked.entry(run).or_default().push(link);
                }
            }
        }
        for list in linked.values_mut() {
            list.sort_by_key(|s| std::cmp::Reverse(s.updated));
        }
        let (root, name) = (p.path.clone(), p.name.clone());
        let current = Some(self.env().branch.to_string());
        self.runs_view.update(cx, |v, _| {
            v.set_project(Some(root), name, branches, current);
            v.set_sessions(linked, prs);
        });
    }

    /// The CI state of an environment's branch, when GitHub has runs for it.
    pub(super) fn env_ci(&self, pi: usize, ei: usize, cx: &App) -> Option<(State, ci::Run)> {
        let p = self.projects.get(pi)?;
        let env = p.environments.get(ei)?;
        let repo = self.ci.read(cx).repo(&p.path)?;
        repo.branch_state(&env.branch).map(|(s, r)| (s, r.clone()))
    }

    /// The rail badge: environments of the visible project whose CI fails (plus a
    /// failed latest deploy); otherwise how many runs are going.
    pub(super) fn runs_badge(&self, cx: &App) -> Option<(usize, bool)> {
        if !self.has_projects() {
            return None;
        }
        let p = self.project();
        let repo = self.ci.read(cx).repo(&p.path)?;
        let mut branches: Vec<&str> = p.environments.iter().map(|e| e.branch.as_ref()).collect();
        branches.sort_unstable();
        branches.dedup();
        let failing = branches
            .iter()
            .filter(|b| {
                repo.branch_state(b)
                    .is_some_and(|(s, _)| s == State::Failed)
            })
            .count()
            + usize::from(
                repo.latest_deploy()
                    .is_some_and(|d| d.state() == State::Failed),
            );
        if failing > 0 {
            return Some((failing, true));
        }
        let running = repo.runs.iter().filter(|r| !r.state().done()).count();
        (running > 0).then_some((running, false))
    }

    /// The visible environment's CI and the project's last deploy, for the status bar.
    pub(super) fn status_ci(&self, cx: &App) -> (Option<(State, ci::Run)>, Option<ci::Run>) {
        if !self.has_projects() {
            return (None, None);
        }
        let p = self.project();
        let Some(repo) = self.ci.read(cx).repo(&p.path) else {
            return (None, None);
        };
        let env = self.env();
        (
            repo.branch_state(&env.branch).map(|(s, r)| (s, r.clone())),
            repo.latest_deploy().cloned(),
        )
    }

    pub(super) fn on_runs_event(
        &mut self,
        ev: &RunsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            RunsEvent::Fix { root, run } => self.fix_run(root, *run, window, cx),
            RunsEvent::Revert { branch, sha } => self.revert_commit(branch, sha, cx),
            RunsEvent::Push { branch } => self.push_branch(branch, cx),
            RunsEvent::OpenPr { branch, url } => self.open_pr(branch, url.as_deref(), cx),
            RunsEvent::Ship {
                project,
                env,
                id,
                branch,
            } => self.ship_fix(*project, *env, id, branch, window, cx),
            RunsEvent::SendFailure {
                project,
                env,
                id,
                run,
            } => self.send_failure(*project, *env, id, *run, window, cx),
            RunsEvent::OpenSession { project, env, id } => {
                self.open_session_at(*project, *env, id.clone(), window, cx)
            }
            RunsEvent::OpenFile { branch, path } => {
                if !self.has_projects() {
                    return;
                }
                let p = self.project();
                let env = p
                    .environments
                    .iter()
                    .find(|e| e.branch.as_ref() == branch)
                    .unwrap_or_else(|| self.env());
                let env_path = env.path.clone();
                let file = env_path.join(path);
                self.open_doc_in(&env_path, TabKind::File { path: file }, true, cx);
                if env_path != self.env().path
                    && let Some(ei) = self.project().env_index(&env_path)
                {
                    let pi = self.active_project;
                    self.select_env(pi, ei, window, cx);
                }
                cx.notify();
            }
        }
    }

    /// Starts Droid on a failed run, with the failure as its first prompt, in the
    /// environment of the run's branch. A branch with no environment gets one.
    fn fix_run(&mut self, root: &Path, run_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pi) = self.projects.iter().position(|p| p.path == root) else {
            return;
        };
        let ci = self.ci.read(cx);
        let Some(repo) = ci.repo(root) else {
            return;
        };
        let Some(run) = repo.run(run_id).cloned() else {
            return;
        };
        let detail = match repo.detail(run_id) {
            Some(ci::Load::Ready(d)) => Some(d.clone()),
            _ => None,
        };
        let project = &self.projects[pi];
        if let Some(env) = project
            .environments
            .iter()
            .find(|e| e.branch.as_ref() == run.head_branch)
        {
            let path = env.path.clone();
            let prompt = ci::fix_prompt(&run, detail.as_deref(), &run.head_branch);
            self.start_fix(&path, &run, prompt, window, cx);
            return;
        }
        let main = project
            .environments
            .iter()
            .find(|e| e.is_checkout)
            .unwrap_or(&project.environments[0]);
        if run.is_deploy() {
            // Tags have no branch to check out; the main checkout gets the fix.
            let (path, branch) = (main.path.clone(), main.branch.to_string());
            let prompt = ci::fix_prompt(&run, detail.as_deref(), &branch);
            self.start_fix(&path, &run, prompt, window, cx);
            return;
        }
        let wt = model::worktree_path(&project.name, &run.head_branch);
        let (repo_root, branch) = (project.path.clone(), run.head_branch.clone());
        cx.spawn_in(window, async move |this, cx| {
            let (r, p, b) = (repo_root.clone(), wt.clone(), branch.clone());
            let result = cx
                .background_spawn(async move { git::add_branch_worktree(&r, &p, &b) })
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
                    let prompt = ci::fix_prompt(&run, detail.as_deref(), &branch);
                    this.start_fix(&wt, &run, prompt, window, cx);
                }
                Err(e) => {
                    let open = Open::Runs {
                        root: repo_root,
                        run: Some(run.id),
                    };
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

    /// Pushes the environment on `branch`, then follows the CI of the push.
    fn push_branch(&mut self, branch: &str, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let p = self.project();
        let Some(env) = p.environments.iter().find(|e| e.branch.as_ref() == branch) else {
            return;
        };
        let remote = env
            .sync
            .remote
            .clone()
            .unwrap_or_else(|| "origin".to_string());
        let (dir, root, branch) = (env.path.clone(), p.path.clone(), branch.to_string());
        self.runs_view.update(cx, |v, cx| {
            v.set_notice(format!("Pushing {branch}…"), false, cx)
        });
        cx.spawn(async move |this, cx| {
            let (d, r, b) = (dir.clone(), remote.clone(), branch.clone());
            let result = cx
                .background_spawn(async move { git::push(&d, &r, &b) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (text, error) = match result {
                    Ok(()) => {
                        this.ci.update(cx, |ci, cx| ci.pushed(&root, &branch, cx));
                        (format!("Pushed {branch}. Waiting for CI…"), false)
                    }
                    Err(e) => (format!("Couldn't push {branch}: {}", first_line(&e)), true),
                };
                this.runs_view
                    .update(cx, |v, cx| v.set_notice(text, error, cx));
                this.refresh_status(&dir, cx);
            });
        })
        .detach();
    }

    /// Opens the branch's pull request, or GitHub's page to open one.
    fn open_pr(&mut self, branch: &str, url: Option<&str>, cx: &mut Context<Self>) {
        if let Some(url) = url {
            cx.open_url(url);
            return;
        }
        if !self.has_projects() {
            return;
        }
        let p = self.project();
        let dir = p
            .environments
            .iter()
            .find(|e| e.branch.as_ref() == branch)
            .map_or_else(|| p.path.clone(), |e| e.path.clone());
        let branch = branch.to_string();
        cx.spawn(async move |this, cx| {
            let b = branch.clone();
            let result = cx
                .background_spawn(async move { git::pr_create_web(&dir, &b) })
                .await;
            if let Err(e) = result {
                let _ = this.update(cx, |this, cx| {
                    let text = format!(
                        "Couldn't open a pull request for {branch}: {}",
                        first_line(&e)
                    );
                    this.runs_view
                        .update(cx, |v, cx| v.set_notice(text, true, cx));
                });
            }
        })
        .detach();
    }

    /// Has a fix's agent commit and push, and follows the run: a failure goes back
    /// to the same session (a few times), a pass ends the loop.
    fn ship_fix(
        &mut self,
        pi: usize,
        ei: usize,
        id: &SharedString,
        branch: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((root, env_path)) = self
            .projects
            .get(pi)
            .and_then(|p| Some((p.path.clone(), p.environments.get(ei)?.path.clone())))
        else {
            return;
        };
        let branch = branch.to_string();
        self.stage_session(
            &env_path,
            id,
            window,
            cx,
            move |this, staged, env_path, id, window, cx| {
                let staged = staged.unwrap_or_default();
                if !this.send_to_session(
                    &env_path,
                    &id,
                    ci::ship_prompt(&branch, &staged),
                    window,
                    cx,
                ) {
                    return;
                }
                this.ci.update(cx, |ci, _| ci.watch_branch(&root, &branch));
                let key = (root, branch.clone());
                let rounds = this.fix_loops.get(&key).map_or(0, |l| l.rounds);
                this.fix_loops.insert(
                    key,
                    FixLoop {
                        env_path,
                        session: id,
                        rounds,
                    },
                );
                this.runs_view.update(cx, |v, cx| {
                    v.set_notice(
                        format!(
                            "Droid is committing and pushing {branch}; Arakun will follow its CI."
                        ),
                        false,
                        cx,
                    )
                });
            },
        );
    }

    /// Sends a newer failure of the branch to the session that fixed an earlier one.
    fn send_failure(
        &mut self,
        pi: usize,
        ei: usize,
        id: &SharedString,
        run_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((root, env_path)) = self
            .projects
            .get(pi)
            .and_then(|p| Some((p.path.clone(), p.environments.get(ei)?.path.clone())))
        else {
            return;
        };
        let ci = self.ci.read(cx);
        let Some(repo) = ci.repo(&root) else {
            return;
        };
        let Some(run) = repo.run(run_id).cloned() else {
            return;
        };
        let detail = match repo.detail(run_id) {
            Some(ci::Load::Ready(d)) => Some(d.clone()),
            _ => None,
        };
        let text = format!(
            "CI failed again.\n\n{}",
            ci::fix_prompt(&run, detail.as_deref(), &run.head_branch)
        );
        if self.send_to_session(&env_path, id, text, window, cx) {
            if let Some(s) = self.session_mut(&env_path, id) {
                s.ci_run = Some(run_id);
            }
            self.save(cx);
            self.sync_runs_view(cx);
        }
    }

    /// The first result of a fix's push: green ends the loop; red goes back to
    /// the session until it has had `FIX_ROUNDS` tries.
    fn advance_fix_loop(
        &mut self,
        root: &Path,
        run: &ci::Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = (root.to_path_buf(), run.head_branch.clone());
        let Some(lp) = self.fix_loops.get(&key).cloned() else {
            return false;
        };
        let branch = &run.head_branch;
        let open = || {
            Some(Open::Runs {
                root: root.to_path_buf(),
                run: Some(run.id),
            })
        };
        match run.state() {
            State::Passed => {
                self.fix_loops.remove(&key);
                self.notify(
                    AlertKind::Success,
                    &format!("CI passes on {branch} · #{}", run.number),
                    "Droid's fix is green.",
                    open(),
                    cx,
                );
                self.runs_view.update(cx, |v, cx| {
                    v.set_notice(format!("CI passes on {branch}. Fixed."), false, cx)
                });
            }
            State::Failed if lp.rounds < FIX_ROUNDS => {
                let round = lp.rounds + 1;
                let detail = match self.ci.read(cx).repo(root).and_then(|r| r.detail(run.id)) {
                    Some(ci::Load::Ready(d)) => Some(d.clone()),
                    _ => None,
                };
                let text = ci::retry_prompt(run, detail.as_deref(), round, FIX_ROUNDS);
                if self.send_to_session(&lp.env_path, &lp.session, text, window, cx) {
                    if let Some(s) = self.session_mut(&lp.env_path, &lp.session) {
                        s.ci_run = Some(run.id);
                    }
                    self.save(cx);
                    self.sync_runs_view(cx);
                }
                if let Some(l) = self.fix_loops.get_mut(&key) {
                    l.rounds = round;
                }
                self.notify(
                    AlertKind::Warning,
                    &format!("CI still fails on {branch} · #{}", run.number),
                    &format!("Sent back to Droid (try {round} of {FIX_ROUNDS})."),
                    open(),
                    cx,
                );
            }
            State::Failed => {
                self.fix_loops.remove(&key);
                self.notify(
                    AlertKind::Failure,
                    &format!("CI still fails on {branch} · #{}", run.number),
                    &format!("Droid tried {FIX_ROUNDS} times; it needs you now."),
                    open(),
                    cx,
                );
            }
            _ => return false,
        }
        true
    }

    /// Reverts a commit in the environment of its branch, as a new local commit;
    /// pushing it stays with the user.
    fn revert_commit(&mut self, branch: &str, sha: &str, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let p = self.project();
        let Some(env) = p.environments.iter().find(|e| e.branch.as_ref() == branch) else {
            return;
        };
        let (dir, root, sha, branch) = (
            env.path.clone(),
            p.path.clone(),
            sha.to_string(),
            branch.to_string(),
        );
        cx.spawn(async move |this, cx| {
            let (d, s) = (dir.clone(), sha.clone());
            let result = cx
                .background_spawn(async move { git::revert(&d, &s) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let short = sha.get(..7).unwrap_or(&sha);
                let (text, error) = match result {
                    Ok(()) => (
                        format!("Reverted {short} in {branch}. Push to run CI."),
                        false,
                    ),
                    Err(e) => (format!("Couldn't revert {short}: {}", first_line(&e)), true),
                };
                this.runs_view
                    .update(cx, |v, cx| v.set_notice(text, error, cx));
                this.ci.update(cx, |ci, cx| ci.forget_graphs(&root, cx));
                if let Some(pi) = this.projects.iter().position(|p| p.path == root) {
                    this.refresh_project(pi, cx);
                }
            });
        })
        .detach();
    }

    fn start_fix(
        &mut self,
        env_path: &Path,
        run: &ci::Run,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title: SharedString = format!("Fix CI #{}", run.number).into();
        // The tab stays on the run; the fix shows in its timeline and opens from there.
        let Some(id) =
            self.start_session_in(env_path, Agent::Droid, title, prompt, false, window, cx)
        else {
            return;
        };
        if let Some(s) = self.session_mut(env_path, &id) {
            s.ci_run = Some(run.id);
        }
        self.save(cx);
        self.sync_runs_view(cx);
        cx.notify();
    }

    /// A desktop notice for runs that matter: failures on the branch of an open
    /// environment, failed deploys, and branches that went green again.
    pub(super) fn on_ci_event(
        &mut self,
        ev: &CiEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CiEvent::Finished {
            root,
            run,
            prev,
            watched,
        } = ev;
        if *watched && self.advance_fix_loop(root, run, window, cx) {
            return;
        }
        let Some(pi) = self.projects.iter().position(|p| &p.path == root) else {
            return;
        };
        let watching = window.is_window_active()
            && pi == self.active_project
            && matches!(
                self.env().tabs.get(self.env().active_tab).map(|t| &t.kind),
                Some(TabKind::Runs)
            );
        if watching {
            return;
        }
        let project = &self.projects[pi];
        let open_branch = project
            .environments
            .iter()
            .any(|e| e.branch.as_ref() == run.head_branch);
        let state = run.state();
        let (title, kind) = match (state, prev) {
            (State::Failed, _) if run.is_deploy() => (
                format!("Deploy failed · {}", run.head_branch),
                AlertKind::Failure,
            ),
            (State::Failed, _) if open_branch => (
                format!("CI failed on {} · #{}", run.head_branch, run.number),
                AlertKind::Failure,
            ),
            (State::Passed, _) if *watched => (
                format!("CI passed on {} · #{}", run.head_branch, run.number),
                AlertKind::Success,
            ),
            (State::Passed, Some(State::Failed)) if open_branch || run.is_deploy() => (
                format!("CI passes again on {} · #{}", run.head_branch, run.number),
                AlertKind::Success,
            ),
            _ => return,
        };
        let body = format!(
            "{} · {} — {}",
            project.name, run.workflow_name, run.display_title
        );
        self.notify(
            kind,
            &title,
            &body,
            Some(Open::Runs {
                root: root.clone(),
                run: Some(run.id),
            }),
            cx,
        );
    }
}

/// Tries a fix's agent gets when CI keeps failing after its push.
const FIX_ROUNDS: u8 = 3;

#[derive(Clone)]
pub(super) struct FixLoop {
    env_path: std::path::PathBuf,
    session: SharedString,
    rounds: u8,
}

fn first_line(e: &anyhow::Error) -> String {
    e.to_string().lines().next().unwrap_or("").to_string()
}

/// The status-bar chip of the visible environment's CI.
pub(super) fn ci_chip_label(state: State, run: &ci::Run) -> String {
    match state {
        State::Failed => format!("CI failed · #{}", run.number),
        State::Running => format!("CI running · #{}", run.number),
        State::Queued => format!("CI queued · #{}", run.number),
        State::Passed => "CI passed".to_string(),
        State::Cancelled => "CI cancelled".to_string(),
        State::Skipped => "CI skipped".to_string(),
    }
}
