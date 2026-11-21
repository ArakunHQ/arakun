//! The git workflow of the Changes panel: commit, push, pull, pull requests, merging
//! a worktree into the main checkout, and per-file stage / unstage / discard.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};

use super::{ContextMenu, MenuTarget, Modal, PanelTab, Shell, panel_empty, plural};
use crate::agent::AgentKind;
use crate::git::{self, ChangeKind};
use crate::git_dialogs::{
    CommitDialog, ConfirmDialog, GitDialogEvent, PullRequestDialog, PullRequestEvent,
};
use crate::model::{GitOp, TabKind, pretty_path};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

/// `gh pr view` goes over the network; look again at most this often.
const PR_RECHECK: Duration = Duration::from_secs(60);

/// Background git reads of one environment that the window repeats on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum GitRead {
    Status,
    PullRequest,
}

/// The [`GitRead`]s running, so none of them overlaps itself. Polls ask for them every
/// few seconds; while one is slow, starting more would only take more of the few
/// background threads, which everything else (saves, diffs, file lists) waits on.
#[derive(Debug, Default)]
pub(super) struct GitReads {
    running: HashSet<(PathBuf, GitRead)>,
    /// Environments whose status was asked for while one was running. That one may
    /// have read the files before the change that asked, so status runs once more.
    status_again: HashSet<PathBuf>,
    /// Environments whose last status ran past its limit, and when it timed out. A
    /// status that always takes longer would otherwise be killed and start again at
    /// once, keeping a core busy for nothing.
    status_timed_out: HashMap<PathBuf, Instant>,
}

/// How long status rests after it ran past its limit.
pub(super) const STATUS_PAUSE: Duration = Duration::from_secs(5 * 60);

impl GitReads {
    /// Marks `read` of `path` as running; false when it already is.
    pub(super) fn start(&mut self, path: &Path, read: GitRead) -> bool {
        if read == GitRead::Status
            && self
                .status_timed_out
                .get(path)
                .is_some_and(|at| at.elapsed() < STATUS_PAUSE)
        {
            return false;
        }
        let started = self.running.insert((path.to_path_buf(), read));
        if !started && read == GitRead::Status {
            self.status_again.insert(path.to_path_buf());
        }
        started
    }

    /// Marks `read` of `path` as ended; true when status must run again.
    pub(super) fn end(&mut self, path: &Path, read: GitRead) -> bool {
        self.running.remove(&(path.to_path_buf(), read));
        read == GitRead::Status && self.status_again.remove(path)
    }

    /// Notes how the status of `path` that just ended went: one that timed out does not
    /// run again for [`STATUS_PAUSE`]. True when it worked after one that timed out.
    pub(super) fn status_ended(&mut self, path: &Path, timed_out: bool) -> bool {
        if timed_out {
            self.status_again.remove(path);
            self.status_timed_out
                .insert(path.to_path_buf(), Instant::now());
            false
        } else {
            self.status_timed_out.remove(path).is_some()
        }
    }
}

impl Shell {
    /// Looks up the pull request of the visible environment's branch while the
    /// Changes tab is open.
    pub(super) fn maybe_lookup_pr(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.has_projects() || self.panel_tab != PanelTab::Changes || self.env().path != path {
            return;
        }
        let env = self.env_mut();
        let branch = env
            .sync
            .branch
            .clone()
            .filter(|_| env.sync.github && env.sync.gh);
        let Some(branch) = branch else {
            if env.pr.take().is_some() {
                cx.notify();
            }
            return;
        };
        match &env.pr_checked {
            Some((b, at)) if b.as_ref() == branch && at.elapsed() < PR_RECHECK => return,
            Some((b, _)) if b.as_ref() != branch => env.pr = None,
            _ => {}
        }
        if !self.git_reads.start(path, GitRead::PullRequest) {
            return;
        }
        self.env_mut().pr_checked = Some((branch.clone().into(), Instant::now()));
        let path = path.to_path_buf();
        let at = self.git_at(&path);
        cx.spawn(async move |this, cx| {
            let pr = cx
                .background_spawn(crate::guard::task(async move { at.pr_view() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.git_reads.end(&path, GitRead::PullRequest);
                let Some(env) = this.find_env_mut(&path) else {
                    return;
                };
                // A server that could not be asked says nothing about the
                // pull request: the one known stays, and the next look asks.
                let Ok(pr) = pr else {
                    env.pr_checked = None;
                    return;
                };
                if env.sync.branch.as_deref() == Some(branch.as_str()) && env.pr != pr {
                    env.pr = pr;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Runs a push, pull or PR job for one environment, one at a time, and keeps
    /// its error on the panel.
    fn run_git_op(
        &mut self,
        path: PathBuf,
        op: GitOp,
        job: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.find_env_mut(&path) else {
            return;
        };
        if env.git_busy.is_some() {
            return;
        }
        env.git_busy = Some(op);
        env.git_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move { job() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(env) = this.find_env_mut(&path) {
                    env.git_busy = None;
                    env.pr_checked = None;
                    if let Err(e) = result {
                        env.git_error = Some(e.to_string().into());
                    }
                }
                this.refresh_status(&path, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn git_push(&mut self, cx: &mut Context<Self>) {
        let env = self.env();
        let (Some(remote), Some(branch)) = (env.sync.remote.clone(), env.sync.branch.clone())
        else {
            return;
        };
        let path = env.path.clone();
        let at = self.git_at(&path);
        self.run_git_op(path, GitOp::Push, move || at.push(&remote, &branch), cx);
    }

    fn git_pull(&mut self, cx: &mut Context<Self>) {
        if self.env().sync.upstream.is_none() {
            return;
        }
        let path = self.env().path.clone();
        let at = self.git_at(&path);
        self.run_git_op(path, GitOp::Pull, move || at.pull(), cx);
    }

    /// Opens the branch's pull request, or the dialog that creates one. What
    /// the dialog starts with is read from the clone first.
    fn git_pull_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env();
        if let Some(pr) = env.pr.as_ref() {
            let url = pr.url.clone();
            self.open_url(url, window, cx);
            return;
        }
        let s = env.sync.clone();
        let (Some(remote), Some(branch), true) = (s.remote, s.branch, s.gh) else {
            return;
        };
        if env.git_busy.is_some() || self.modal.is_some() {
            return;
        }
        let needs_push = !s.published || s.ahead > 0;
        let path = env.path.clone();
        let agent = env.agent;
        // Busy while the clone is read, so a second click opens nothing.
        if let Some(env) = self.find_env_mut(&path) {
            env.git_busy = Some(GitOp::PullRequest);
        }
        cx.notify();
        let at = self.git_at(&path);
        cx.spawn_in(window, async move |this, cx| {
            let (r, b) = (remote.clone(), branch.clone());
            let plan = cx
                .background_spawn(crate::guard::task(async move {
                    // Finding an agent's CLI reads the disk too.
                    let writer = crate::pr_writer::writer(agent, crate::env_name::installed);
                    at.pr_plan(&r, &b).map(|plan| (plan, writer))
                }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| match plan {
                Ok(plan) => {
                    this.show_pull_request(path, remote, branch, needs_push, plan, window, cx)
                }
                // A server that could not be asked: a dialog would start
                // from a plan it never read.
                Err(e) => {
                    if let Some(env) = this.find_env_mut(&path) {
                        env.git_busy = None;
                        env.git_error = Some(e.to_string().into());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn show_pull_request(
        &mut self,
        path: PathBuf,
        remote: String,
        branch: String,
        needs_push: bool,
        (plan, writer): (git::PrPlan, Option<AgentKind>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(env) = self.find_env_mut(&path) {
            env.git_busy = None;
        }
        cx.notify();
        if self.modal.is_some() {
            return;
        }
        let at = self.git_at(&path);
        let dialog = cx.new(|cx| {
            PullRequestDialog::new(at, remote, branch, needs_push, plan, writer, window, cx)
        });
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &PullRequestEvent, window, cx| {
                // Either way the branch may have been pushed.
                if let Some(env) = this.find_env_mut(&path) {
                    env.pr_checked = None;
                    env.git_error = None;
                }
                this.refresh_status(&path, cx);
                this.close_modal(window, cx);
                let PullRequestEvent::Created(url) = ev else {
                    return;
                };
                // The new pull request shows in the GitHub tab; one on a host
                // that tab doesn't read opens its page.
                match crate::github::pr::ItemRef::from_url(url) {
                    Some(item) => {
                        this.open_github_tab(window, cx);
                        this.github_view.update(cx, |v, cx| v.reveal(&item, cx));
                    }
                    None if !url.is_empty() => cx.open_url(url),
                    None => {}
                }
            },
        );
        self.menu = None;
        self.modal = Some((Modal::PullRequest(dialog), sub));
        cx.notify();
    }

    pub(super) fn toggle_stage(&mut self, change: git::Change, cx: &mut Context<Self>) {
        let path = self.env().path.clone();
        let at = self.git_at(&path);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    if change.fully_staged() {
                        at.unstage(&change.path)
                    } else {
                        at.stage(&change.path)
                    }
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(env) = this.find_env_mut(&path) {
                    env.git_error = result.err().map(|e| e.to_string().into());
                }
                this.refresh_status(&path, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env();
        if env.changes.is_empty() {
            return;
        }
        let path = env.path.clone();
        let branch = env.branch.clone();
        let files = env.changes.len();
        let staged = env.changes.iter().filter(|c| c.staged).count();
        let agent = env.agent;
        let at = self.git_at(&path);
        let dialog = cx.new(|cx| CommitDialog::new(at, branch, files, staged, agent, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &GitDialogEvent, window, cx| {
                if let GitDialogEvent::Done = ev {
                    if let Some(env) = this.find_env_mut(&path) {
                        env.git_error = None;
                    }
                    this.refresh_status(&path, cx);
                }
                this.close_modal(window, cx);
            },
        );
        self.menu = None;
        self.modal = Some((Modal::Commit(dialog), sub));
        cx.notify();
    }

    pub(super) fn confirm_discard(
        &mut self,
        change: git::Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.env().path.clone();
        let body = match change.kind {
            ChangeKind::Untracked => {
                "This deletes the file. It was never committed, so it can't be recovered."
            }
            ChangeKind::Added => {
                "This removes the new file from the index and deletes it. It can't be undone."
            }
            _ => {
                "This throws away every uncommitted change to the file, staged or not. It can't be undone."
            }
        };
        let at = self.git_at(&path);
        let file = change.path.clone();
        let dialog = cx.new(|cx| {
            ConfirmDialog::new(
                "Discard changes?",
                body,
                "Discard",
                true,
                move || at.discard(&change),
                window,
                cx,
            )
            .detail("file01", theme::mix(0.45), file)
        });
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, _: &GitDialogEvent, window, cx| {
                this.refresh_status(&path, cx);
                this.close_modal(window, cx);
            },
        );
        self.menu = None;
        self.modal = Some((Modal::GitConfirm(dialog), sub));
        cx.notify();
    }

    /// Merging a worktree's branch into the main checkout; the plan is read first so
    /// the dialog can say what happens, or why it can't.
    fn open_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pi = self.active_project;
        let env = self.env();
        // The main checkout is on the server: the server has no merge yet.
        if env.is_checkout || env.remote.is_some() {
            return;
        }
        let Some(branch) = env.sync.branch.clone() else {
            return;
        };
        let Some(main) = self
            .project()
            .environments
            .iter()
            .find(|e| e.is_checkout)
            .map(|e| e.path.clone())
        else {
            return;
        };
        let dirty_here = env.changes.len();
        cx.spawn_in(window, async move |this, cx| {
            let (m, b) = (main.clone(), branch.clone());
            let plan = cx
                .background_spawn(crate::guard::task(async move { git::merge_plan(&m, &b) }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.show_merge(pi, main, branch, plan, dirty_here, window, cx)
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn show_merge(
        &mut self,
        pi: usize,
        main: PathBuf,
        branch: String,
        plan: anyhow::Result<git::MergePlan>,
        dirty_here: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal.is_some() {
            return;
        }
        let (blocked, base, commits) = match &plan {
            Ok(p) => (p.blocker(&branch), p.base.clone(), p.commits),
            Err(e) => (Some(e.to_string()), None, 0),
        };
        let base = base.unwrap_or_else(|| "the main checkout".into());
        let warn: Hsla = theme::warn().into();
        let muted = theme::mix(0.45);
        let body =
            format!("Merges {branch} into {base} in the main checkout, with a merge commit.");
        let dialog = cx.new(|cx| {
            let (m, b) = (main.clone(), branch.clone());
            let mut d = ConfirmDialog::new(
                format!("Merge into {base}?"),
                body,
                "Merge",
                false,
                move || git::merge_into_main(&m, &b),
                window,
                cx,
            );
            if commits > 0 {
                d = d.detail(
                    "git-commit",
                    muted,
                    format!("{} to merge", plural(commits as usize, "commit")),
                );
            }
            d = d.detail("folder01", muted, pretty_path(&main));
            if dirty_here > 0 {
                d = d.detail(
                    "file-diff",
                    warn,
                    format!(
                        "{} in this environment {} not included; commit {} first.",
                        plural(dirty_here, "uncommitted change"),
                        if dirty_here == 1 { "is" } else { "are" },
                        if dirty_here == 1 { "it" } else { "them" },
                    ),
                );
            }
            d.blocked(blocked)
        });
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &GitDialogEvent, window, cx| {
                if let GitDialogEvent::Done = ev {
                    this.refresh_project(pi, cx);
                }
                this.close_modal(window, cx);
            },
        );
        self.menu = None;
        self.modal = Some((Modal::GitConfirm(dialog), sub));
        cx.notify();
    }

    // -----------------------------------------------------------------------

    pub(super) fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let env = self.env();
        let s = &env.sync;
        let busy = env.git_busy;
        let idle = busy.is_none();

        let mut chips = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(2.));
        if s.upstream.is_some() {
            let label = (s.behind > 0).then(|| format!("{}", s.behind));
            chips = chips.child(
                sync_chip(
                    "git-pull",
                    "circle-arrow-down01",
                    None,
                    label,
                    busy == Some(GitOp::Pull),
                    idle,
                )
                .on_click(cx.listener(|this, _, _, cx| this.git_pull(cx))),
            );
        }
        if s.remote.is_some() && s.branch.is_some() {
            let label = if !s.published {
                Some("Publish".to_string())
            } else {
                (s.ahead > 0).then(|| format!("{}", s.ahead))
            };
            let enabled = idle && (!s.published || s.ahead > 0);
            chips = chips.child(
                sync_chip(
                    "git-push",
                    "cloud-upload",
                    None,
                    label,
                    busy == Some(GitOp::Push),
                    enabled,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if enabled {
                        this.git_push(cx)
                    }
                })),
            );
        }
        if s.github && s.branch.is_some() {
            let pr = env.pr.as_ref();
            let (icon_name, tint) = match pr {
                Some(p) if p.state == "MERGED" => ("git-merge", Some(theme::pr_merged())),
                Some(p) if p.state == "CLOSED" => {
                    ("git-pull-request-closed", Some(theme::pr_closed()))
                }
                Some(p) if p.is_draft => ("git-pull-request-draft", Some(theme::pr_draft())),
                Some(_) => ("git-pull-request", Some(theme::pr_open())),
                None => ("git-pull-request", None),
            };
            let label = pr.map_or("PR".to_string(), |p| format!("#{}", p.number));
            let enabled = pr.is_some() || (idle && s.gh);
            chips = chips.child(
                sync_chip(
                    "git-pr",
                    icon_name,
                    tint,
                    Some(label),
                    busy == Some(GitOp::PullRequest),
                    enabled,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if enabled {
                        this.git_pull_request(window, cx)
                    }
                })),
            );
        }

        let branch_row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(36.))
            .pl(crate::ui::sp(14.))
            .pr(crate::ui::sp(8.))
            .border_b_1()
            .border_color(theme::stroke())
            .child(icon("git-branch", 13.).text_color(theme::text_subtle()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_SM))
                    .font_family(theme::FONT_MONO)
                    .text_color(theme::text_muted())
                    .child(env.branch.clone()),
            )
            .child(chips);

        let error = env.git_error.clone().map(|e| {
            div()
                .flex()
                .flex_none()
                .items_start()
                .gap(crate::ui::sp(6.))
                .mx(px(8.))
                .mt(px(8.))
                .p(crate::ui::sp(8.))
                .rounded(crate::ui::rad(8.))
                .bg(gpui::rgba(0xef44441a))
                .child(
                    div()
                        .id("git-error")
                        .flex_1()
                        .min_w_0()
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .text_size(px(ui::TEXT_XS))
                        .line_height(px(16.))
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::error())
                        .child(e),
                )
                .child(
                    div()
                        .id("git-error-close")
                        .flex_none()
                        .cursor_pointer()
                        .child(icon("cancel01", 12.).text_color(theme::error()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.env_mut().git_error = None;
                            cx.notify();
                        })),
                )
        });

        let can_merge = !env.is_checkout && env.remote.is_none() && s.branch.is_some();
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .p(crate::ui::sp(8.))
            .border_t_1()
            .border_color(theme::stroke())
            .child(
                ui::button(
                    "git-commit",
                    if env.changes.is_empty() {
                        "Commit".to_string()
                    } else {
                        format!("Commit {}…", plural(env.changes.len(), "file"))
                    },
                    ButtonKind::Primary,
                    !env.changes.is_empty(),
                )
                .flex_1()
                .justify_center()
                .on_click(cx.listener(|this, _, window, cx| this.open_commit(window, cx))),
            )
            .when(can_merge, |d| {
                d.child(
                    ui::button("git-merge", "Merge", ButtonKind::Ghost, true)
                        .border_1()
                        .border_color(theme::border())
                        .on_click(cx.listener(|this, _, window, cx| this.open_merge(window, cx))),
                )
            });

        let body: AnyElement = match &env.status_error {
            Some(e) if env.changes.is_empty() => div()
                .flex_1()
                .min_h_0()
                .child(panel_empty(&format!("Couldn't read the changes: {e}")))
                .into_any_element(),
            _ if env.changes.is_empty() => div()
                .flex_1()
                .min_h_0()
                .child(panel_empty("No uncommitted changes in this environment."))
                .into_any_element(),
            Some(e) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(panel_empty(&format!(
                    "These may be out of date. Couldn't read the changes again: {e}"
                )))
                .child(self.render_change_list(cx))
                .into_any_element(),
            None => self.render_change_list(cx).into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(branch_row)
            .children(error)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    fn render_change_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let env = self.env();
        let (added, removed) = env.diff_totals();
        let staged = env.changes.iter().filter(|c| c.staged).count();
        let summary = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .px(crate::ui::sp(14.))
            .pt(crate::ui::sp(10.))
            .pb(crate::ui::sp(4.))
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_subtle())
            .child(div().flex_1().child(if staged > 0 {
                format!("{} · {staged} staged", plural(env.changes.len(), "file"))
            } else {
                plural(env.changes.len(), "file")
            }))
            .child(
                div()
                    .text_color(theme::diff_add())
                    .child(format!("+{added}")),
            )
            .child(
                div()
                    .text_color(theme::diff_del())
                    .child(format!("-{removed}")),
            );
        div()
            .id("changes")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(summary)
            .child(div().flex().flex_col().p(crate::ui::sp(6.)).children(
                env.changes.iter().enumerate().map(|(i, c)| {
                    let (dir, file) = c.path.rsplit_once('/').unwrap_or(("", &c.path));
                    let (letter, color): (&str, Hsla) = match c.kind {
                        ChangeKind::Modified => ("M", theme::warn().into()),
                        ChangeKind::Added => ("A", theme::diff_add().into()),
                        ChangeKind::Untracked => ("U", theme::diff_add().into()),
                        ChangeKind::Deleted => ("D", theme::diff_del().into()),
                        ChangeKind::Renamed => ("R", theme::accent()),
                    };
                    let (stage_icon, stage_color): (&str, Hsla) = if c.fully_staged() {
                        ("checkmark-circle02", theme::diff_add().into())
                    } else if c.staged {
                        ("circle-dot", theme::accent())
                    } else {
                        // Unstaged files show what they are, as in the Explorer.
                        super::documents::file_icon(file)
                    };
                    let group: SharedString = format!("change-row-{i}").into();
                    let path = c.path.clone();
                    let untracked = c.kind == ChangeKind::Untracked;
                    let (toggle, toggle2, discard) = (c.clone(), c.clone(), c.clone());
                    let action = |id: (&'static str, usize), name: &'static str| {
                        div()
                            .id(id)
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .rounded(crate::ui::rad(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::fill_selected()))
                            .child(icon(name, 13.).text_color(theme::text_subtle()))
                    };
                    div()
                        .id(("change", i))
                        .group(group.clone())
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(26.))
                        .px(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(4.))
                        .text_size(px(ui::TEXT_MD))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .child(
                            div()
                                .id(("change-stage", i))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(18.))
                                .ml(px(-2.))
                                .rounded(crate::ui::rad(4.))
                                .hover(|s| s.bg(theme::fill_selected()))
                                .child(icon(stage_icon, 14.).text_color(stage_color))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_stage(toggle.clone(), cx);
                                })),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(150.))
                                .truncate()
                                .when(c.kind == ChangeKind::Deleted, |d| d.line_through())
                                .child(file.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .child(dir.to_string()),
                        )
                        .child(
                            // Hover swaps counts for actions by visibility only: changing
                            // `display` on hover shifts layout between prepaint and paint.
                            div()
                                .relative()
                                .flex()
                                .flex_none()
                                .justify_end()
                                .items_center()
                                .h(px(20.))
                                .min_w(px(44.))
                                .child(
                                    div()
                                        .flex()
                                        .gap(crate::ui::sp(4.))
                                        .text_size(px(ui::TEXT_XS))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .group_hover(group.clone(), |s| s.invisible())
                                        .when(c.added > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(theme::diff_add())
                                                    .child(format!("+{}", c.added)),
                                            )
                                        })
                                        .when(c.removed > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(theme::diff_del())
                                                    .child(format!("-{}", c.removed)),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .flex()
                                        .invisible()
                                        .group_hover(group, |s| s.visible())
                                        .items_center()
                                        .gap(crate::ui::sp(2.))
                                        .child(
                                            action(
                                                ("change-toggle", i),
                                                if c.fully_staged() {
                                                    "minus-sign"
                                                } else {
                                                    "add01"
                                                },
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_stage(toggle2.clone(), cx);
                                                }),
                                            ),
                                        )
                                        .child(action(("change-discard", i), "undo").on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                cx.stop_propagation();
                                                this.confirm_discard(discard.clone(), window, cx);
                                            }),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .w(px(12.))
                                .text_size(px(ui::TEXT_XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color)
                                .child(letter),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.menu = Some(ContextMenu {
                                    target: MenuTarget::Change(i),
                                    at: ev.position,
                                });
                                cx.notify();
                            }),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_doc_tab(
                                TabKind::Diff {
                                    path: path.clone(),
                                    untracked,
                                },
                                window,
                                cx,
                            )
                        }))
                }),
            ))
    }
}

/// Pull / push / PR control of the branch row: an icon with an optional count or label.
fn sync_chip(
    id: &'static str,
    icon_name: &'static str,
    tint: Option<gpui::Rgba>,
    label: Option<String>,
    busy: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(4.))
        .h(px(ui::ICON_BUTTON))
        .px(crate::ui::sp(6.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .text_size(px(ui::TEXT_SM))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .when(enabled && !busy, |d| {
            d.cursor_pointer().hover(|s| s.bg(theme::fill_selected()))
        })
        .when(!enabled && !busy, |d| d.opacity(0.4))
        .child(if busy {
            ui::spinner(13., theme::mix(0.65)).into_any_element()
        } else {
            icon(icon_name, 13.)
                .text_color(tint.map_or(theme::mix(0.6), Hsla::from))
                .into_any_element()
        })
        .when_some(label, |d, l| d.child(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_read_does_not_start_while_it_runs() {
        let mut reads = GitReads::default();
        let (a, b) = (Path::new("/a"), Path::new("/b"));
        assert!(reads.start(a, GitRead::PullRequest));
        assert!(!reads.start(a, GitRead::PullRequest));
        assert!(reads.start(b, GitRead::PullRequest));
        assert!(reads.start(a, GitRead::Status));
        assert!(!reads.end(a, GitRead::PullRequest));
        assert!(reads.start(a, GitRead::PullRequest));
    }

    #[test]
    fn status_asked_for_while_it_runs_runs_once_more() {
        let mut reads = GitReads::default();
        let a = Path::new("/a");
        assert!(reads.start(a, GitRead::Status));
        assert!(!reads.end(a, GitRead::Status));
        assert!(reads.start(a, GitRead::Status));
        assert!(!reads.start(a, GitRead::Status));
        assert!(!reads.start(a, GitRead::Status));
        assert!(reads.end(a, GitRead::Status));
        assert!(reads.start(a, GitRead::Status));
        assert!(!reads.end(a, GitRead::Status));
    }

    #[test]
    fn status_that_timed_out_rests_before_it_runs_again() {
        let mut reads = GitReads::default();
        let (a, b) = (Path::new("/a"), Path::new("/b"));
        assert!(reads.start(a, GitRead::Status));
        // Asked for again while it ran, then it timed out: no new run at once.
        assert!(!reads.start(a, GitRead::Status));
        assert!(!reads.status_ended(a, true));
        assert!(!reads.end(a, GitRead::Status));
        assert!(!reads.start(a, GitRead::Status));
        assert!(reads.start(b, GitRead::Status));
        assert!(reads.start(a, GitRead::PullRequest));
        // Once the pause is over it runs, and a status that works ends it.
        reads
            .status_timed_out
            .insert(a.to_path_buf(), Instant::now() - STATUS_PAUSE);
        assert!(reads.start(a, GitRead::Status));
        assert!(reads.status_ended(a, false));
        assert!(!reads.status_ended(a, false));
    }
}
