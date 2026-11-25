//! Several rows of the open project's list picked at once, with Ctrl+click
//! (Cmd on macOS) and Shift+click, and what to do with all of them: settle
//! their sessions, close them, or delete the worktrees.
//!
//! Rows are kept by path: deleting or adding a worktree shifts the indices.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, AppContext, Context, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window, div, prelude::FluentBuilder, px,
};

use super::{ContextMenu, MenuTarget, Modal, Shell};
use crate::git_dialogs::{ConfirmDialog, GitDialogEvent};
use crate::model::Status;
use crate::{git, theme, ui};

/// The rows from `anchor` to `to`, both included, in the order `order` lists
/// them. Without the anchor in the list, `to` alone.
pub(super) fn range(order: &[PathBuf], anchor: Option<&Path>, to: &Path) -> Vec<PathBuf> {
    let end = order.iter().position(|p| p == to);
    let start = anchor.and_then(|a| order.iter().position(|p| p == a));
    match (start, end) {
        (Some(a), Some(b)) => order[a.min(b)..=a.max(b)].to_vec(),
        _ => vec![to.to_path_buf()],
    }
}

/// Whether an environment with sessions of `statuses` has a row in the
/// project's list, with the Active list drawn above it or not.
fn listed(is_checkout: bool, statuses: &[Status], active_above: bool) -> bool {
    if statuses.is_empty() {
        return !is_checkout;
    }
    !(active_above && statuses.iter().all(|s| super::active::is_active(*s)))
}

/// What settling turns a session's status into; None leaves it. Running
/// agents keep running.
fn settled(status: Status) -> Option<Status> {
    matches!(status, Status::NeedsInput | Status::Error).then_some(Status::Idle)
}

impl Shell {
    /// The open project's rows as the list shows them, by index.
    pub(super) fn listed_envs(&self, pi: usize) -> Vec<usize> {
        let Some(project) = self.projects.get(pi) else {
            return Vec::new();
        };
        // What waits or runs is in the Active list above; here the rest. A
        // worktree from before one session per worktree stays while any of
        // its sessions is idle. Sessions start in worktrees, so the main
        // checkout shows only while it holds older ones.
        let active_above = self.active_list_shown();
        project
            .environments
            .iter()
            .enumerate()
            .filter(|(ei, env)| {
                let statuses: Vec<Status> = env.sessions.iter().map(|s| s.status).collect();
                listed(env.is_checkout, &statuses, active_above)
                    && (self.show_settled || !self.archive_hides(pi, *ei))
            })
            .map(|(ei, _)| ei)
            .collect()
    }

    /// Whether row `ei` of project `pi` is archived and would leave the list.
    /// The open one stays, so going to it shows where you are.
    pub(super) fn archive_hides(&self, pi: usize, ei: usize) -> bool {
        self.projects.get(pi).is_some_and(|p| {
            p.active_env != ei && p.environments.get(ei).is_some_and(|e| e.archived())
        })
    }

    /// The archived rows of project `pi` that the "archived" row shows or hides.
    pub(super) fn archived_count(&self, pi: usize) -> usize {
        self.projects.get(pi).map_or(0, |p| {
            (0..p.environments.len())
                .filter(|&ei| self.archive_hides(pi, ei))
                .count()
        })
    }

    pub(super) fn is_picked(&self, path: &Path) -> bool {
        self.rail_pick.iter().any(|p| p == path)
    }

    /// A click on row `ei` of project `pi`. Ctrl (Cmd) adds or takes it out,
    /// Shift picks the rows from the last one clicked; a plain click opens it,
    /// on session `opens` when the row is named after one, and drops the pick.
    pub(super) fn click_env(
        &mut self,
        pi: usize,
        ei: usize,
        opens: Option<gpui::SharedString>,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .projects
            .get(pi)
            .and_then(|p| p.environments.get(ei))
            .map(|e| e.path.clone())
        else {
            return;
        };
        if modifiers.shift {
            let order: Vec<PathBuf> = self
                .listed_envs(pi)
                .into_iter()
                .map(|i| self.projects[pi].environments[i].path.clone())
                .collect();
            // From the open row when nothing was clicked yet.
            let anchor = self
                .rail_pick_anchor
                .clone()
                .or_else(|| Some(self.env().path.clone()));
            // An anchor not in the list (the main checkout, a row gone) picks
            // the row alone, which then anchors the next Shift+click.
            let anchor = anchor.filter(|a| order.contains(a));
            self.rail_pick = range(&order, anchor.as_deref(), &path);
            self.rail_pick_anchor = anchor.or(Some(path));
        } else if modifiers.secondary(cx) {
            if self.rail_pick.is_empty() {
                // The open row is part of it, as in a file manager; not the
                // main checkout, which most of these leave alone.
                let open = self.env().path.clone();
                if open != path
                    && !self.env().is_checkout
                    && self
                        .listed_envs(pi)
                        .iter()
                        .any(|&i| self.projects[pi].environments[i].path == open)
                {
                    self.rail_pick.push(open);
                }
            }
            if let Some(i) = self.rail_pick.iter().position(|p| *p == path) {
                self.rail_pick.remove(i);
            } else {
                self.rail_pick.push(path.clone());
            }
            self.rail_pick_anchor = Some(path);
        } else {
            self.drop_pick();
            self.rail_pick_anchor = Some(path);
            match opens {
                Some(id) => self.open_session_at(pi, ei, id, window, cx),
                None => self.select_env(pi, ei, window, cx),
            }
            return;
        }
        cx.notify();
    }

    /// Ctrl or Shift+click on a row of the Active list: it stands for its
    /// session's worktree, which may be listed only there (all its sessions
    /// wait or run). No range: the Active list is in another order.
    pub(super) fn toggle_pick(&mut self, path: &Path, cx: &mut Context<Self>) {
        match self.rail_pick.iter().position(|p| p == path) {
            Some(i) => {
                self.rail_pick.remove(i);
            }
            None => self.rail_pick.push(path.to_path_buf()),
        }
        cx.notify();
    }

    /// Whether the bar goes under the Active list: a picked worktree is not in
    /// the open project's list, so it was picked there. Not while that list
    /// draws no rows, or the bar would show nowhere.
    pub(super) fn pick_bar_under_active(&self) -> bool {
        if !self.active_list_shown() || !self.any_active() {
            return false;
        }
        let pi = self.active_project;
        let listed: Vec<&Path> = self
            .listed_envs(pi)
            .into_iter()
            .map(|ei| self.projects[pi].environments[ei].path.as_path())
            .collect();
        self.rail_pick
            .iter()
            .any(|p| !listed.contains(&p.as_path()))
    }

    /// Returns whether anything was picked.
    pub(super) fn drop_pick(&mut self) -> bool {
        self.rail_pick_anchor = None;
        self.tree_pick_anchor = None;
        let sessions = !std::mem::take(&mut self.tree_pick).is_empty();
        !std::mem::take(&mut self.rail_pick).is_empty() || sessions
    }

    /// The picked rows that still exist, as (project, environment).
    fn picked(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (pi, p) in self.projects.iter().enumerate() {
            for (ei, e) in p.environments.iter().enumerate() {
                if self.is_picked(&e.path) {
                    out.push((pi, ei));
                }
            }
        }
        out
    }

    /// Archives the picked rows: their sessions that are not working leave
    /// the lists, and their pull requests' threads are resolved. On a server,
    /// the server settles them.
    fn archive_picked(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .picked()
            .into_iter()
            .map(|(pi, ei)| self.projects[pi].environments[ei].path.clone())
            .collect();
        for path in paths {
            self.archive_env(&path, cx);
        }
        self.drop_pick();
        cx.notify();
    }

    /// Closes the tabs of the picked rows' sessions and stops their agents,
    /// settled first. The worktrees and transcripts stay; an agent still at
    /// work finishes in the background.
    fn close_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut sessions = Vec::new();
        let mut remote = Vec::new();
        for (pi, ei) in self.picked() {
            let env = &mut self.projects[pi].environments[ei];
            // A server keeps asking until it settles them itself.
            let on_server = env.remote.is_some();
            if on_server {
                remote.push(env.path.clone());
            }
            for s in &mut env.sessions {
                if !on_server && let Some(status) = settled(s.status) {
                    s.status = status;
                }
                sessions.push((env.path.clone(), s.id.clone()));
            }
        }
        for path in remote {
            self.servers_settle_env(&path, cx);
        }
        for (path, id) in sessions {
            while let Some(i) = self.strip.session_tab(&id) {
                self.discard_tab(i, window, cx);
            }
            self.retire_if_hidden(&path, &id, cx);
        }
        self.drop_pick();
        self.save(cx);
        cx.notify();
    }

    /// Asks once, then deletes the picked worktrees: this computer's here,
    /// a server's by asking the server, which answers each on its own. Their
    /// branches stay. The main checkout is left out.
    fn open_delete_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut targets = Vec::new();
        let mut remote = Vec::new();
        let mut skipped = 0;
        for (pi, ei) in self.picked() {
            let project = &self.projects[pi];
            let env = &project.environments[ei];
            if env.is_checkout {
                skipped += 1;
                continue;
            }
            let title = env
                .sessions
                .iter()
                .max_by_key(|s| s.updated)
                .map_or_else(|| env.name.to_string(), |s| s.title.to_string());
            if let Some(r) = &env.remote {
                remote.push((r.clone(), format!("{title} (on {})", r.machine)));
                continue;
            }
            targets.push((
                project.path.clone(),
                env.path.clone(),
                title,
                env.changes.len(),
            ));
        }
        if targets.is_empty() && remote.is_empty() {
            return;
        }
        let n = targets.len() + remote.len();
        let warn: gpui::Hsla = theme::warn().into();
        let muted = theme::mix(0.45);
        let mut details = Vec::new();
        for (_, _, title, dirty) in &targets {
            let text = if *dirty > 0 {
                format!(
                    "{title}: {dirty} uncommitted change{} discarded",
                    plural(*dirty)
                )
            } else {
                title.clone()
            };
            details.push((if *dirty > 0 { warn } else { muted }, text));
        }
        for (_, title) in &remote {
            details.push((muted, title.clone()));
        }
        let mut body = format!(
            "This removes the folder{s} of {n} environment{s}. Their branches and commits stay.",
            s = plural(n)
        );
        if !remote.is_empty() {
            body.push_str(
                " A server deletes its own, with everything in it, uncommitted changes too.",
            );
        }
        if skipped > 0 {
            body.push_str(" The main checkout stays.");
        }
        let work: Vec<(PathBuf, PathBuf)> = targets
            .iter()
            .map(|(r, p, _, _)| (r.clone(), p.clone()))
            .collect();
        let dialog = cx.new(|cx| {
            let mut d = ConfirmDialog::new(
                format!("Delete {n} environment{}?", plural(n)),
                body,
                "Delete",
                true,
                move || {
                    // One that fails does not stop the rest.
                    let mut failed = Vec::new();
                    for (repo, path) in &work {
                        if let Err(e) = git::remove_worktree(repo, path, None) {
                            failed.push(format!("{}: {e}", path.display()));
                        }
                    }
                    if failed.is_empty() {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(failed.join("\n")))
                    }
                },
                window,
                cx,
            );
            for (color, text) in details {
                d = d.detail("folder01", color, text);
            }
            d
        });
        let paths: Vec<PathBuf> = targets.into_iter().map(|(_, p, _, _)| p).collect();
        // What is gone from disk leaves the list once the dialog shows the
        // result: on success it closes, and after a partial failure it stays
        // open on the error, which only redraws it.
        let kept = paths.clone();
        let observed = cx.observe_in(&dialog, window, move |this, _, window, cx| {
            this.forget_gone(&kept, window, cx)
        });
        let events = cx.subscribe_in(
            &dialog,
            window,
            move |this, dialog, ev: &GitDialogEvent, window, cx| {
                this.forget_gone(&paths, window, cx);
                this.close_modal(window, cx);
                // Asked once the ones here are gone. Each server answers on
                // its own; one that cannot is said by the island.
                if matches!(ev, GitDialogEvent::Done) {
                    this.servers_delete_envs(&remote, dialog.entity_id(), cx);
                    this.drop_pick();
                }
            },
        );
        let sub = gpui::Subscription::join(observed, events);
        self.menu = None;
        self.modal = Some((Modal::GitConfirm(dialog), sub));
        cx.notify();
    }

    /// Takes the worktrees of `paths` that are gone from disk off the list.
    fn forget_gone(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let gone: Vec<PathBuf> = paths
            .iter()
            .filter(|p| !p.exists() && self.find_env(p).is_some())
            .cloned()
            .collect();
        self.forget_envs(&gone, window, cx);
    }

    /// What the picked rows allow: archive, close and delete.
    fn pick_allows(&self, picked: &[(usize, usize)]) -> (bool, bool, bool) {
        let envs = || {
            picked
                .iter()
                .map(|&(pi, ei)| &self.projects[pi].environments[ei])
        };
        let can_archive = envs().any(|e| {
            e.sessions
                .iter()
                .any(|s| s.status != Status::Working && (e.remote.is_some() || !s.settled))
        });
        let can_close = envs().any(|e| !e.sessions.is_empty());
        let can_delete = envs().any(|e| !e.is_checkout);
        (can_archive, can_close, can_delete)
    }

    /// A right-click on row `ei` of project `pi`. On a picked row, while more
    /// than one is picked, the menu acts on all of them, as the bar does.
    /// Another row drops the pick, so no action reaches rows it does not show.
    pub(super) fn open_env_menu(
        &mut self,
        pi: usize,
        ei: usize,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let on_pick = self
            .projects
            .get(pi)
            .and_then(|p| p.environments.get(ei))
            .is_some_and(|e| self.is_picked(&e.path));
        let target = if on_pick && self.picked().len() > 1 {
            MenuTarget::Picked
        } else {
            self.drop_pick();
            MenuTarget::Env(pi, ei)
        };
        self.menu = Some(ContextMenu { target, at });
        cx.notify();
    }

    /// The menu of a right-click on one of several picked rows: the bar's
    /// actions, on all of them.
    pub(super) fn render_pick_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let picked = self.picked();
        let n = picked.len();
        let (can_archive, can_close, can_delete) = self.pick_allows(&picked);
        let s = plural(n);
        ui::menu_panel("pick-menu", 240.)
            .child(
                div()
                    .px(ui::sp(8.))
                    .py(ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(format!("{n} environment{s} selected")),
            )
            .child(
                ui::menu_item("pm-archive", "archive02", "Archive", false, can_archive)
                    .tooltip(ui::tip(
                        "Hide their sessions and resolve their pull requests' review threads",
                    ))
                    .when(can_archive, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            this.archive_picked(cx);
                        }))
                    }),
            )
            .child(
                ui::menu_item(
                    "pm-close",
                    "cancel-circle",
                    "Close sessions",
                    false,
                    can_close,
                )
                .when(can_close, |d| {
                    d.on_click(cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        this.close_picked(window, cx);
                    }))
                }),
            )
            .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
            .child(
                ui::menu_item(
                    "pm-delete",
                    "delete02",
                    format!("Delete {n} environment{s}…"),
                    true,
                    can_delete,
                )
                .when(can_delete, |d| {
                    d.on_click(
                        cx.listener(|this, _, window, cx| this.open_delete_picked(window, cx)),
                    )
                }),
            )
            .into_any_element()
    }

    /// The bar under the picked rows: how many, and what to do with them.
    pub(super) fn render_pick_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picked = self.picked();
        if picked.is_empty() {
            return None;
        }
        let n = picked.len();
        let (can_archive, can_close, can_delete) = self.pick_allows(&picked);
        let action = |id: &'static str, icon_name: &'static str, tip: &'static str, on: bool| {
            // gpui has no disabled state to give screen readers: without a
            // click handler the button offers no action, and this says why.
            ui::icon_button(id, icon_name, 14., tip).when(!on, |d| {
                d.opacity(0.4)
                    .cursor_default()
                    .aria_description("Not available for the selected environments")
            })
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(ui::sp(4.))
                .mt(ui::sp(4.))
                .h(px(28.))
                .pl(ui::sp(8.))
                .pr(ui::sp(4.))
                .rounded(ui::rad(8.))
                .bg(theme::selection_strong())
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::content())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("{n} selected")),
                )
                .child(
                    action(
                        "pick-archive",
                        "archive02",
                        "Archive: hide their sessions and resolve their review threads",
                        can_archive,
                    )
                    .when(can_archive, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.archive_picked(cx)))
                    }),
                )
                .child(
                    action(
                        "pick-close",
                        "cancel-circle",
                        "Close their sessions; the environments stay",
                        can_close,
                    )
                    .when(can_close, |d| {
                        d.on_click(cx.listener(|this, _, window, cx| this.close_picked(window, cx)))
                    }),
                )
                .child(
                    action(
                        "pick-delete",
                        "delete02",
                        "Delete the environments…",
                        can_delete,
                    )
                    .when(can_delete, |d| {
                        d.on_click(
                            cx.listener(|this, _, window, cx| this.open_delete_picked(window, cx)),
                        )
                    }),
                )
                .child(
                    ui::icon_button("pick-clear", "cancel01", 14., "Clear the selection (Esc)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.drop_pick();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn shift_picks_from_the_anchor_either_way() {
        let order = paths(&["a", "b", "c", "d"]);
        assert_eq!(
            range(&order, Some(Path::new("b")), Path::new("d")),
            paths(&["b", "c", "d"])
        );
        assert_eq!(
            range(&order, Some(Path::new("c")), Path::new("a")),
            paths(&["a", "b", "c"])
        );
        assert_eq!(
            range(&order, Some(Path::new("c")), Path::new("c")),
            paths(&["c"])
        );
    }

    #[test]
    fn without_a_listed_anchor_shift_picks_one() {
        let order = paths(&["a", "b"]);
        assert_eq!(range(&order, None, Path::new("b")), paths(&["b"]));
        assert_eq!(
            range(&order, Some(Path::new("gone")), Path::new("a")),
            paths(&["a"])
        );
    }

    #[test]
    fn the_main_checkout_is_listed_only_while_it_holds_sessions() {
        use Status::*;
        assert!(!listed(true, &[], false));
        assert!(listed(true, &[Idle], true));
        assert!(listed(false, &[], true));
        // What runs or waits is named in the Active list instead.
        assert!(!listed(true, &[Working], true));
        assert!(!listed(false, &[NeedsInput, Error], true));
        assert!(listed(false, &[Working, Idle], true));
        assert!(listed(false, &[Working], false));
    }

    #[test]
    fn settling_leaves_running_and_idle_sessions() {
        assert_eq!(settled(Status::NeedsInput), Some(Status::Idle));
        assert_eq!(settled(Status::Error), Some(Status::Idle));
        assert_eq!(settled(Status::Working), None);
        assert_eq!(settled(Status::Idle), None);
    }
}
