//! Removes old environments by themselves (`crate::cleanup`): a pull request
//! merged or closed, nothing to lose, and unused for the days picked in
//! Settings > Branches. Before the user picked, it suggests the setting once,
//! the first time an environment would have gone.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use super::island::Open;
use crate::cleanup::{self, Choice, Seen};
use crate::island::AlertKind;
use crate::model::{Status, unix_now};

/// Asks git and GitHub about the old environments at most this often.
const EVERY: Duration = Duration::from_secs(3600);

#[derive(Default)]
pub(super) struct Watch {
    last: Option<Instant>,
    /// A check or a removal is running.
    busy: bool,
}

impl Shell {
    /// The environment on screen counts as used now.
    pub(super) fn mark_visible_used(&mut self) {
        if let Some(p) = self.projects.get_mut(self.active_project)
            && let Some(e) = p.environments.get_mut(p.active_env)
        {
            e.used = unix_now();
        }
    }

    /// Called on the scheduler's tick; asks git and GitHub at most every
    /// [`EVERY`].
    pub(super) fn clean_up_envs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mark_visible_used();
        let choice = cleanup::choice();
        let days = match (choice, choice.days()) {
            (_, Some(d)) => d,
            (Choice::Unset, None) => cleanup::SUGGEST_DAYS,
            _ => return,
        };
        if self.cleanup.busy || self.cleanup.last.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.cleanup.last = Some(Instant::now());
        let found = self.old_envs(days);
        if found.is_empty() {
            return;
        }
        self.cleanup.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let removable: Vec<(PathBuf, PathBuf)> = cx
                .background_spawn(async move {
                    found
                        .into_iter()
                        .filter(|(_, path)| cleanup::removable(path))
                        .collect()
                })
                .await;
            let Ok(go) = this.update(cx, |this, cx| this.cleanup_found(removable, cx)) else {
                return;
            };
            let removed: Vec<PathBuf> = cx
                .background_spawn(async move {
                    go.into_iter()
                        .filter_map(|(repo, path)| {
                            match crate::git::remove_worktree(&repo, &path, None) {
                                Ok(()) => Some(path),
                                Err(e) => {
                                    eprintln!(
                                        "arakun: could not remove the old environment {}: {e}",
                                        path.display()
                                    );
                                    None
                                }
                            }
                        })
                        .collect()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.cleanup.busy = false;
                if removed.is_empty() {
                    return;
                }
                let names: Vec<String> =
                    removed.iter().map(|p| crate::model::dir_name(p)).collect();
                for path in &removed {
                    this.env_removed(path, window, cx);
                }
                let title = match removed.len() {
                    1 => "Removed an old environment".to_string(),
                    n => format!("Removed {n} old environments"),
                };
                let body = format!(
                    "{}. Their pull requests are done and their branches are kept.",
                    names.join(", ")
                );
                this.notify(
                    AlertKind::Info,
                    &title,
                    &body,
                    Some(Open::BranchSettings),
                    cx,
                );
            });
        })
        .detach();
    }

    /// (repository, worktree) of each environment unused for `days`, before
    /// git and GitHub are asked.
    fn old_envs(&self, days: u8) -> Vec<(PathBuf, PathBuf)> {
        let now = unix_now();
        self.projects
            .iter()
            .enumerate()
            .flat_map(|(pi, p)| {
                p.environments
                    .iter()
                    .enumerate()
                    .filter(move |(ei, e)| {
                        let seen = self.seen(e, pi == self.active_project && *ei == p.active_env);
                        cleanup::candidate(seen, days, now)
                    })
                    .map(|(_, e)| (p.path.clone(), e.path.clone()))
            })
            .collect()
    }

    fn seen(&self, e: &crate::model::Environment, visible: bool) -> Seen {
        Seen {
            is_checkout: e.is_checkout,
            visible,
            busy: e.git_busy.is_some()
                || e.sessions
                    .iter()
                    .any(|s| matches!(s.status, Status::Working | Status::NeedsInput)),
            used: e.last_used(),
        }
    }

    /// What git and GitHub found removable: suggests the setting when it is
    /// still unpicked, or returns the environments that may go now. Anything
    /// that changed while they were asked (an agent started, the environment
    /// was opened, the setting turned off) keeps an environment.
    fn cleanup_found(
        &mut self,
        found: Vec<(PathBuf, PathBuf)>,
        cx: &mut Context<Self>,
    ) -> Vec<(PathBuf, PathBuf)> {
        let choice = cleanup::choice();
        let Some(days) = choice.days() else {
            if choice == Choice::Unset && !found.is_empty() {
                cleanup::set_suggested();
                let n = found.len();
                let title = "Remove old environments by themselves?";
                let body = format!(
                    "{n} environment{} {} a finished pull request and sat unused for {} days. \
                     Settings > Branches can remove such environments for you.",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "has" } else { "have" },
                    cleanup::SUGGEST_DAYS,
                );
                self.notify(
                    AlertKind::Info,
                    title,
                    &body,
                    Some(Open::BranchSettings),
                    cx,
                );
            }
            return Vec::new();
        };
        let still = self.old_envs(days);
        found.into_iter().filter(|f| still.contains(f)).collect()
    }
}
