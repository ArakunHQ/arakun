//! Alt+1 to Alt+9: a project's sessions from the keyboard. Folded, the
//! sidebar opens that project's flyout; open, the keys go to the project in
//! the tree. Then the arrows move through its rows, Enter opens one and Esc
//! lets go.
//!
//! The rows have no focus of their own: a cursor in the rail marks one, and
//! the shell reads the arrows, Enter and Esc for it while it is set (see
//! `rail_key`). So the composer keeps the focus, and any other key goes on
//! to it. Screen readers hear each row as the cursor comes to it.

use super::*;

/// A row the keys can be on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::shell) enum KeyRow {
    /// The project's heading in the tree.
    Project,
    Session(SharedString),
    /// The flyout's New session.
    New,
}

/// Where the keys are: on `row` of project `pi`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::shell) struct KeyCursor {
    pub pi: usize,
    pub row: KeyRow,
}

/// The row after `at` (`down`) or before it, staying at the ends as a tree
/// does. The first row when `at` is not among them.
fn step(rows: &[KeyRow], at: &KeyRow, down: bool) -> Option<KeyRow> {
    let Some(i) = rows.iter().position(|r| r == at) else {
        return rows.first().cloned();
    };
    let j = if down {
        (i + 1).min(rows.len() - 1)
    } else {
        i.saturating_sub(1)
    };
    rows.get(j).cloned()
}

/// "2 of 5": where a row sits, for screen readers.
fn place(rows: &[KeyRow], at: &KeyRow) -> String {
    match rows.iter().position(|r| r == at) {
        Some(i) => format!("{} of {}", i + 1, rows.len()),
        None => String::new(),
    }
}

fn sessions(n: usize) -> String {
    format!("{n} session{}", if n == 1 { "" } else { "s" })
}

impl Shell {
    /// The actions of Alt+1 to Alt+9 on the shell's root.
    pub(in crate::shell) fn project_key_handlers(
        &self,
        root: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        macro_rules! on {
            ($root:expr, $action:ident, $i:expr) => {
                $root.on_action(
                    cx.listener(|this, _: &$action, window, cx| this.project_keys($i, window, cx)),
                )
            };
        }
        let root = on!(root, ProjectSessions1, 0);
        let root = on!(root, ProjectSessions2, 1);
        let root = on!(root, ProjectSessions3, 2);
        let root = on!(root, ProjectSessions4, 3);
        let root = on!(root, ProjectSessions5, 4);
        let root = on!(root, ProjectSessions6, 5);
        let root = on!(root, ProjectSessions7, 6);
        let root = on!(root, ProjectSessions8, 7);
        let root = on!(root, ProjectSessions9, 8);
        // A click anywhere takes over from the keys.
        root.capture_any_mouse_down(cx.listener(|this, _, _, cx| {
            if this.rail.keys.take().is_some() {
                cx.notify();
            }
        }))
    }

    /// Alt+N: project `pi`'s sessions, in its flyout when the sidebar is
    /// folded and in the tree when it is open. A hidden sidebar opens first.
    /// Without the tree, the keys switch to the project.
    fn project_keys(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.drop_pick();
        let Some(name) = self.projects.get(pi).map(|p| p.name.clone()) else {
            self.announce(format!("There is no project {}", pi + 1), cx);
            return;
        };
        if self.rail_narrow() {
            self.rail.flyout.open_by_keys(pi);
            let rows = self.key_rows(pi, cx);
            let count = rows
                .iter()
                .filter(|r| matches!(r, KeyRow::Session(_)))
                .count();
            let row = rows.first().cloned().unwrap_or(KeyRow::New);
            let first = self.key_row_spoken(pi, &row, &rows, cx);
            self.rail.keys = Some(KeyCursor { pi, row });
            self.announce(
                format!("Sessions in {name}, {}. {first}", sessions(count)),
                cx,
            );
            cx.notify();
            return;
        }
        if !self.rail_shown() {
            self.rail.open = true;
            self.save(cx);
        }
        if !crate::tab_layout::current().tree() {
            self.rail.keys = None;
            if pi != self.active_project {
                self.select_project(pi, window, cx);
            }
            self.announce(name.to_string(), cx);
            cx.notify();
            return;
        }
        // A project the tree leaves out shows once it is the one on screen.
        if !self.tree_lists(pi, cx) {
            self.select_project(pi, window, cx);
        }
        let rows = self.key_rows(pi, cx);
        let Some(row) = rows.first().cloned() else {
            self.rail.keys = None;
            self.announce(format!("{name}, no sessions"), cx);
            cx.notify();
            return;
        };
        let spoken = self.key_row_spoken(pi, &row, &rows, cx);
        self.rail.keys = Some(KeyCursor { pi, row });
        self.announce(spoken, cx);
        cx.notify();
    }

    /// The tree has project `pi` on it, under its heading or, without
    /// headings, by its rows.
    fn tree_lists(&self, pi: usize, cx: &App) -> bool {
        let layout = crate::tab_layout::current();
        self.tree_listing(None, cx).iter().any(|p| {
            if layout.group {
                p.pi == Some(pi)
            } else {
                p.rows.iter().any(|r| r.pi == pi)
            }
        })
    }

    /// The rows the keys move through in project `pi`, in the order they
    /// are drawn: the flyout's sessions and New session, or in the tree the
    /// project's heading and the sessions that show under it.
    fn key_rows(&self, pi: usize, cx: &App) -> Vec<KeyRow> {
        if self.rail_narrow() {
            return self
                .flyout_rows(pi, cx)
                .into_iter()
                .map(|r| KeyRow::Session(r.id))
                .chain([KeyRow::New])
                .collect();
        }
        let layout = crate::tab_layout::current();
        let mut out = Vec::new();
        for p in self.tree_listing(None, cx) {
            if layout.group && p.pi != Some(pi) {
                continue;
            }
            if layout.group {
                out.push(KeyRow::Project);
            }
            if p.open {
                out.extend(
                    p.rows
                        .into_iter()
                        .filter(|r| r.pi == pi)
                        .map(|r| KeyRow::Session(r.id)),
                );
            }
        }
        out
    }

    /// What screen readers say for `row`: what the row's own label says,
    /// then where it sits.
    fn key_row_spoken(&self, pi: usize, row: &KeyRow, rows: &[KeyRow], cx: &App) -> String {
        let Some(project) = self.projects.get(pi) else {
            return String::new();
        };
        let what = match row {
            KeyRow::Project => {
                let count = rows
                    .iter()
                    .filter(|r| matches!(r, KeyRow::Session(_)))
                    .count();
                let open = self.tree_project_open(pi, cx);
                format!(
                    "{}, {}, {}",
                    project.name,
                    sessions(count),
                    if open { "open" } else { "folded" }
                )
            }
            KeyRow::New => format!("New session in {}", project.name),
            KeyRow::Session(id) => match self.tree_rows(pi, cx).into_iter().find(|r| r.id == *id) {
                Some(r) => row_label(&r.title, r.linear.as_deref(), r.state),
                None => return String::new(),
            },
        };
        format!("{what}, {}", place(rows, row))
    }

    /// The tree shows project `pi`'s sessions under its heading.
    fn tree_project_open(&self, pi: usize, cx: &App) -> bool {
        self.tree_listing(None, cx)
            .iter()
            .any(|p| p.pi == Some(pi) && p.open)
    }

    /// Opens (`open`) or folds project `pi` in the tree.
    fn fold_tree_project(&mut self, pi: usize, open: bool) {
        if let Some(path) = self.projects.get(pi).map(|p| p.path.clone()) {
            self.rail.tree_open.insert(path, open);
        }
    }

    /// The keys are on `row` of project `pi`, to draw it so.
    pub(in crate::shell) fn keys_on(&self, pi: usize, row: &KeyRow) -> bool {
        self.rail
            .keys
            .as_ref()
            .is_some_and(|k| k.pi == pi && k.row == *row)
    }

    /// A key while the keys are on a row of the rail: the arrows move,
    /// Enter opens, Esc lets go. True when it was taken. Any other key lets
    /// go of the row and goes on to where the focus is.
    pub(in crate::shell) fn rail_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(KeyCursor { pi, row }) = self.rail.keys.clone() else {
            return false;
        };
        // The flyout went with the pointer, or the sidebar changed.
        let narrow = self.rail_narrow();
        let gone = if narrow {
            !self.rail.flyout.is_open(pi)
        } else {
            !self.rail_shown() || !crate::tab_layout::current().tree()
        };
        if gone || pi >= self.projects.len() {
            self.rail.keys = None;
            cx.notify();
            return false;
        }
        let rows = self.key_rows(pi, cx);
        let moved = match key {
            "down" | "up" => {
                // Down from a folded heading opens it: its sessions are all
                // there is to go to.
                if key == "down" && row == KeyRow::Project && !self.tree_project_open(pi, cx) {
                    self.fold_tree_project(pi, true);
                    let rows = self.key_rows(pi, cx);
                    step(&rows, &row, true)
                } else {
                    step(&rows, &row, key == "down")
                }
            }
            "right" if row == KeyRow::Project => {
                if self.tree_project_open(pi, cx) {
                    step(&rows, &row, true)
                } else {
                    self.fold_tree_project(pi, true);
                    Some(row.clone())
                }
            }
            "left" if row == KeyRow::Project => {
                self.fold_tree_project(pi, false);
                Some(row.clone())
            }
            "left" if !narrow && rows.contains(&KeyRow::Project) => Some(KeyRow::Project),
            "enter" => {
                self.rail.keys = None;
                match row {
                    KeyRow::Project => {
                        let open = self.tree_project_open(pi, cx);
                        self.fold_tree_project(pi, !open);
                        self.rail.keys = Some(KeyCursor { pi, row });
                    }
                    KeyRow::New => {
                        self.rail.flyout.close();
                        self.new_session_in(pi, window, cx);
                    }
                    KeyRow::Session(id) => {
                        self.rail.flyout.close();
                        let ei = self
                            .tree_rows(pi, cx)
                            .into_iter()
                            .find(|r| r.id == id)
                            .map(|r| r.ei);
                        if let Some(ei) = ei {
                            self.open_session_at(pi, ei, id, window, cx);
                        }
                    }
                }
                cx.notify();
                return true;
            }
            "escape" => {
                self.rail.keys = None;
                self.rail.flyout.close();
                cx.notify();
                return true;
            }
            _ => {
                self.rail.keys = None;
                cx.notify();
                return false;
            }
        };
        if let Some(to) = moved {
            let rows = self.key_rows(pi, cx);
            let spoken = self.key_row_spoken(pi, &to, &rows, cx);
            self.rail.keys = Some(KeyCursor { pi, row: to });
            self.announce(spoken, cx);
        }
        cx.notify();
        true
    }
}

/// The ring on the row the keys are on. It lies over the row, so the row
/// keeps its size.
pub(in crate::shell) fn key_ring() -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .rounded(crate::ui::rad(8.))
        .border_1()
        .border_color(theme::focus_ring())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> KeyRow {
        KeyRow::Session(id.to_string().into())
    }

    #[test]
    fn the_arrows_stop_at_the_ends() {
        let rows = [KeyRow::Project, s("a"), s("b")];
        assert_eq!(step(&rows, &KeyRow::Project, true), Some(s("a")));
        assert_eq!(step(&rows, &s("b"), true), Some(s("b")));
        assert_eq!(step(&rows, &s("a"), false), Some(KeyRow::Project));
        assert_eq!(step(&rows, &KeyRow::Project, false), Some(KeyRow::Project));
    }

    #[test]
    fn a_row_that_went_away_starts_again_at_the_top() {
        let rows = [s("a"), KeyRow::New];
        assert_eq!(step(&rows, &s("gone"), true), Some(s("a")));
        assert_eq!(step(&[], &s("gone"), true), None);
    }

    #[test]
    fn a_row_says_where_it_sits() {
        let rows = [s("a"), s("b"), KeyRow::New];
        assert_eq!(place(&rows, &s("b")), "2 of 3");
        assert_eq!(place(&rows, &KeyRow::New), "3 of 3");
        assert_eq!(sessions(1), "1 session");
        assert_eq!(sessions(0), "0 sessions");
    }
}
