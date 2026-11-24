//! The sidebar's tree of sessions (`tab_layout::SessionsIn::Sidebar`): every
//! session of every project, under its project, each with a mark for its
//! state. It takes the place of the projects and environments list.

use super::*;
use crate::tab_layout::{LongTitles, Quiet, Sort, StateMark, TabLayout};

/// Height of a session row whose title fits on one line.
const ROW_H: f32 = 30.;

/// What a row says about its session, the session's own state first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowState {
    Idle,
    Working,
    NeedsYou,
    /// The agent failed.
    Failed,
    /// The checks on its branch failed.
    CiFailed,
    /// The checks on its branch passed.
    Passed,
}

impl RowState {
    /// From the session's state and, for a session in a git worktree of its
    /// own, the checks on that branch.
    pub(super) fn of(status: Status, ci: Option<ci::State>) -> Self {
        match status {
            Status::NeedsInput => Self::NeedsYou,
            Status::Working => Self::Working,
            Status::Error => Self::Failed,
            Status::Idle => match ci {
                Some(ci::State::Failed) => Self::CiFailed,
                Some(ci::State::Passed) => Self::Passed,
                _ => Self::Idle,
            },
        }
    }

    /// Something runs or waits: the project it is in counts as busy.
    pub(super) fn busy(self) -> bool {
        matches!(self, Self::Working | Self::NeedsYou)
    }

    /// The most urgent first, for a folded project's one mark.
    fn rank(self) -> u8 {
        match self {
            Self::NeedsYou => 0,
            Self::Failed => 1,
            Self::CiFailed => 2,
            Self::Working => 3,
            Self::Passed => 4,
            Self::Idle => 5,
        }
    }

    /// The state in a word, where one is worth showing.
    pub(super) fn word(self) -> Option<&'static str> {
        match self {
            Self::NeedsYou => Some("Needs you"),
            Self::Failed => Some("Failed"),
            Self::CiFailed => Some("CI failed"),
            Self::Working => Some("Working"),
            Self::Passed => Some("Passed"),
            Self::Idle => None,
        }
    }

    fn color(self) -> Hsla {
        match self {
            Self::Idle => theme::mix(0.25),
            Self::Working => theme::working(),
            Self::NeedsYou => theme::needs_you(),
            Self::Failed | Self::CiFailed => theme::failed().into(),
            Self::Passed => theme::passed().into(),
        }
    }

    /// The word's color at the end of a row: only what wants the user stands out.
    fn word_color(self) -> Hsla {
        match self {
            Self::NeedsYou => theme::needs_you(),
            Self::Failed | Self::CiFailed => theme::error().into(),
            _ => theme::text_faint(),
        }
    }

    /// The mark before a title, as the setting asks. A word has no mark:
    /// the word at the end says it.
    pub(super) fn mark(self, style: StateMark) -> Option<AnyElement> {
        match style {
            StateMark::Word => None,
            StateMark::Dot => Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(10.))
                    .child(div().size(px(6.)).rounded_full().bg(self.color()))
                    .into_any_element(),
            ),
            StateMark::Icon => Some(match self {
                Self::Idle => status_dot(Status::Idle).into_any_element(),
                Self::Working => status_dot(Status::Working).into_any_element(),
                Self::NeedsYou => status_dot(Status::NeedsInput).into_any_element(),
                Self::Failed => status_dot(Status::Error).into_any_element(),
                Self::CiFailed => ci_mark(ci::State::Failed).into_any_element(),
                Self::Passed => ci_mark(ci::State::Passed).into_any_element(),
            }),
        }
    }

    /// What the end of a row says: the state when it wants the user (or
    /// always, when states show as words), else the pull request.
    fn end(self, style: StateMark, pr: Option<u64>) -> Option<String> {
        let wants = matches!(self, Self::NeedsYou | Self::Failed | Self::CiFailed);
        if (wants || style == StateMark::Word)
            && let Some(word) = self.word()
        {
            return Some(word.into());
        }
        pr.map(|n| format!("#{n}"))
    }
}

/// Whether a project's sessions show under it: `Some(open)`, or `None` when
/// the project is left out. A project with something running, or the one on
/// screen, always shows. `user` is the user's own fold, which wins.
pub(super) fn project_open(
    quiet: Quiet,
    busy: bool,
    here: bool,
    user: Option<bool>,
) -> Option<bool> {
    if busy || here {
        return Some(user.unwrap_or(true));
    }
    match quiet {
        Quiet::Open => Some(user.unwrap_or(true)),
        Quiet::Folded => Some(user.unwrap_or(false)),
        Quiet::Hidden => None,
    }
}

/// A project in the tree, as `Shell::tree_listing` lays it out. Without
/// project headings, one of these with no project holds every row.
struct TreeProject {
    pi: Option<usize>,
    rows: Vec<Row>,
    /// Its rows show under it.
    open: bool,
    urgent: Option<RowState>,
}

/// The ids from `anchor` to `to`, both included, in the order `order` lists
/// them. Without the anchor in the list, `to` alone.
fn pick_range(
    order: &[SharedString],
    anchor: Option<&SharedString>,
    to: &SharedString,
) -> Vec<SharedString> {
    let end = order.iter().position(|p| p == to);
    let start = anchor.and_then(|a| order.iter().position(|p| p == a));
    match (start, end) {
        (Some(a), Some(b)) => order[a.min(b)..=a.max(b)].to_vec(),
        _ => vec![to.clone()],
    }
}

/// One session in the tree.
struct Row {
    pi: usize,
    ei: usize,
    /// Its place in its environment's list.
    si: usize,
    id: SharedString,
    title: SharedString,
    agent: crate::agent::AgentKind,
    state: RowState,
    pr: Option<u64>,
    /// Unix seconds of its last activity.
    updated: u64,
    /// Not working, not archived yet and not on a server: the same rule as
    /// the context menu's Archive.
    can_archive: bool,
}

/// Puts `rows` in the order `sort` asks for. The sort is stable, so rows
/// that tie keep the environments' order.
fn sort_rows(rows: &mut [Row], sort: Sort) {
    match sort {
        Sort::Latest => rows.sort_by_key(|r| std::cmp::Reverse(r.updated)),
        Sort::Title => rows.sort_by_cached_key(|r| r.title.to_lowercase()),
        Sort::Environment => {}
    }
}

impl Shell {
    /// The sessions of project `pi` the tree lists, in the projects list's
    /// order: archived ones only while archived sessions are shown.
    fn tree_rows(&self, pi: usize, cx: &App) -> Vec<Row> {
        let Some(project) = self.projects.get(pi) else {
            return Vec::new();
        };
        let on_screen: Vec<SharedString> = self
            .sessions_on_screen()
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        let mut rows = Vec::new();
        for (ei, env) in project.environments.iter().enumerate() {
            let (ci, pr) = self.env_checks(pi, ei, cx);
            for (si, s) in env.sessions.iter().enumerate() {
                // An archived session still on screen keeps its row.
                if s.settled && !self.show_settled && !on_screen.contains(&s.id) {
                    continue;
                }
                rows.push(Row {
                    pi,
                    ei,
                    si,
                    id: s.id.clone(),
                    title: s.title.clone(),
                    agent: s.agent,
                    state: RowState::of(s.shown_status(), ci),
                    pr,
                    updated: s.updated,
                    can_archive: !s.settled && s.status != Status::Working && env.remote.is_none(),
                });
            }
        }
        rows
    }

    /// What the tree shows, project by project: each project's rows in the
    /// chosen order, whether they show under it, and the most urgent state
    /// among them. Without project headings, every row in one list. `query`
    /// narrows to the titles it matches and opens every project.
    fn tree_listing(&self, query: Option<&str>, cx: &App) -> Vec<TreeProject> {
        let layout = crate::tab_layout::current();
        let rows_of = |pi: usize| {
            let mut rows = self.tree_rows(pi, cx);
            if let Some(q) = query {
                rows.retain(|r| r.title.to_lowercase().contains(q));
            }
            rows
        };
        if !layout.group {
            let mut rows: Vec<Row> = (0..self.projects.len()).flat_map(rows_of).collect();
            sort_rows(&mut rows, layout.sort);
            return vec![TreeProject {
                pi: None,
                rows,
                open: true,
                urgent: None,
            }];
        }
        let mut out = Vec::new();
        for (pi, project) in self.projects.iter().enumerate() {
            let mut rows = rows_of(pi);
            if query.is_some() && rows.is_empty() {
                continue;
            }
            sort_rows(&mut rows, layout.sort);
            let busy = rows.iter().any(|r| r.state.busy());
            let here = pi == self.active_project;
            let user = self.tree_open.get(&project.path).copied();
            let open = if query.is_some() {
                true
            } else {
                match project_open(layout.quiet, busy, here, user) {
                    Some(open) => open,
                    None => continue,
                }
            };
            let urgent = rows
                .iter()
                .map(|r| r.state)
                .filter(|s| *s != RowState::Idle)
                .min_by_key(|s| s.rank());
            out.push(TreeProject {
                pi: Some(pi),
                rows,
                open,
                urgent,
            });
        }
        out
    }

    /// The sessions the tree shows, top to bottom: what Shift+click picks
    /// a range of.
    fn tree_order(&self, cx: &App) -> Vec<SharedString> {
        self.tree_listing(None, cx)
            .into_iter()
            .filter(|p| p.open)
            .flat_map(|p| p.rows.into_iter().map(|r| r.id))
            .collect()
    }

    /// A tree row clicked with Ctrl (Cmd) or Shift: Ctrl adds the session to
    /// the pick or takes it out, Shift picks the rows from the last one
    /// clicked. Picking here drops a pick made in the projects list.
    fn click_tree_pick(&mut self, id: SharedString, shift: bool, cx: &mut Context<Self>) {
        self.rail_pick.clear();
        self.rail_pick_anchor = None;
        if shift {
            let order = self.tree_order(cx);
            // From the open session when nothing was clicked yet.
            let anchor = self
                .tree_pick_anchor
                .clone()
                .or_else(|| self.active_session_id());
            for id in pick_range(&order, anchor.as_ref(), &id) {
                if !self.tree_pick.contains(&id) {
                    self.tree_pick.push(id);
                }
            }
        } else {
            match self.tree_pick.iter().position(|p| *p == id) {
                Some(i) => {
                    self.tree_pick.remove(i);
                }
                None => self.tree_pick.push(id.clone()),
            }
            self.tree_pick_anchor = Some(id);
        }
        cx.notify();
    }

    /// The picked sessions that still exist: their project, environment and id.
    fn tree_picked(&self) -> Vec<(usize, usize, SharedString)> {
        self.tree_pick
            .iter()
            .filter_map(|id| {
                let (pi, ei) = self.session_place(id)?;
                Some((pi, ei, id.clone()))
            })
            .collect()
    }

    /// Pins every picked session to the title bar, or unpins them all when
    /// they all are.
    fn pin_tree_picked(&mut self, cx: &mut Context<Self>) {
        let picked = self.tree_picked();
        let pin = !picked.iter().all(|(_, _, id)| self.is_pinned(id));
        for (pi, ei, id) in picked {
            if self.is_pinned(&id) != pin {
                self.set_pinned(pi, ei, id, pin, None, cx);
            }
        }
        self.drop_pick();
        cx.notify();
    }

    /// Archives the picked sessions that are not working.
    fn archive_tree_picked(&mut self, cx: &mut Context<Self>) {
        for (path, ids) in self.picked_by_env() {
            self.archive_sessions(&path, &ids, cx);
        }
        self.drop_pick();
        cx.notify();
    }

    /// Closes the tabs of the picked sessions; the sessions stay in the tree.
    fn close_tree_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (path, ids) in self.picked_by_env() {
            for id in ids {
                while let Some(i) = self.strip.session_tab(&id) {
                    self.discard_tab(i, window, cx);
                }
                self.retire_if_hidden(&path, &id, cx);
            }
        }
        self.drop_pick();
        self.save(cx);
        cx.notify();
    }

    /// The picked sessions, grouped by the environment that holds them.
    fn picked_by_env(&self) -> Vec<(std::path::PathBuf, Vec<String>)> {
        let mut out: Vec<(std::path::PathBuf, Vec<String>)> = Vec::new();
        for (pi, ei, id) in self.tree_picked() {
            let path = &self.projects[pi].environments[ei].path;
            match out.iter_mut().find(|(p, _)| p == path) {
                Some((_, ids)) => ids.push(id.to_string()),
                None => out.push((path.clone(), vec![id.to_string()])),
            }
        }
        out
    }

    /// "N selected" and what to do with the picked sessions, under the tree.
    fn render_tree_pick_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picked = self.tree_picked();
        if picked.is_empty() {
            return None;
        }
        let n = picked.len();
        let sessions = || {
            picked.iter().filter_map(|(pi, ei, id)| {
                let env = &self.projects[*pi].environments[*ei];
                Some((env, env.sessions.get(env.session_index(id)?)?))
            })
        };
        let can_archive = sessions()
            .any(|(e, s)| !s.settled && s.status != Status::Working && e.remote.is_none());
        let can_close = picked
            .iter()
            .any(|(_, _, id)| self.strip.session_tab(id).is_some());
        let pin_bar = crate::tab_layout::current().pinned_bar();
        let all_pinned = picked.iter().all(|(_, _, id)| self.is_pinned(id));
        let action = |id: &'static str, icon_name: &'static str, tip: &'static str, on: bool| {
            // gpui has no disabled state to give screen readers: without a
            // click handler the button offers no action, and this says why.
            ui::icon_button(id, icon_name, 14., tip).when(!on, |d| {
                d.opacity(0.4)
                    .cursor_default()
                    .aria_description("Not available for the selected sessions")
            })
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .mt(crate::ui::sp(4.))
                .h(px(28.))
                .pl(crate::ui::sp(8.))
                .pr(crate::ui::sp(4.))
                .rounded(crate::ui::rad(8.))
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
                .when(pin_bar, |d| {
                    d.child(
                        action(
                            "tree-pick-pin",
                            if all_pinned { "pin-off" } else { "pin" },
                            if all_pinned {
                                "Unpin from the title bar"
                            } else {
                                "Pin to the title bar"
                            },
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.pin_tree_picked(cx))),
                    )
                })
                .child(
                    action(
                        "tree-pick-archive",
                        "archive02",
                        "Archive: hide them from the lists",
                        can_archive,
                    )
                    .when(can_archive, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.archive_tree_picked(cx)))
                    }),
                )
                .child(
                    action(
                        "tree-pick-close",
                        "cancel-circle",
                        "Close their tabs; the sessions stay",
                        can_close,
                    )
                    .when(can_close, |d| {
                        d.on_click(
                            cx.listener(|this, _, window, cx| this.close_tree_picked(window, cx)),
                        )
                    }),
                )
                .child(
                    ui::icon_button(
                        "tree-pick-clear",
                        "cancel01",
                        14.,
                        "Clear the selection (Esc)",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.drop_pick();
                        cx.notify();
                    })),
                )
                .into_any_element(),
        )
    }

    /// The checks and the pull request of environment `ei`'s branch, for a
    /// git worktree; the main checkout's branch is no one session's.
    fn env_checks(&self, pi: usize, ei: usize, cx: &App) -> (Option<ci::State>, Option<u64>) {
        let Some(project) = self.projects.get(pi) else {
            return (None, None);
        };
        let Some(env) = project.environments.get(ei).filter(|e| !e.is_checkout) else {
            return (None, None);
        };
        let Some(repo) = self.ci.read(cx).repo(&project.path) else {
            return (None, None);
        };
        (
            repo.branch_state(&env.branch).map(|(s, _)| s),
            repo.pr_of(&env.branch).map(|p| p.number),
        )
    }

    /// What session `id` of environment `ei` in project `pi` is doing, as its
    /// row says it.
    pub(super) fn session_state(&self, pi: usize, ei: usize, id: &str, cx: &App) -> RowState {
        let status = self
            .projects
            .get(pi)
            .and_then(|p| p.environments.get(ei))
            .and_then(|e| e.sessions.get(e.session_index(id)?))
            .map_or(Status::Idle, |s| s.shown_status());
        RowState::of(status, self.env_checks(pi, ei, cx).0)
    }

    /// The tree; in the Sessions menu (`menu`), narrowed to the titles its
    /// search matches. `heading` puts "Sessions" and its + above it.
    pub(super) fn render_session_tree(
        &self,
        menu: bool,
        heading: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let layout = crate::tab_layout::current();
        let open_id = self.active_session_id();
        let query = menu
            .then(|| self.tree_filter.read(cx).text().trim().to_lowercase())
            .filter(|q| !q.is_empty());
        let mut list = div()
            .id("session-tree")
            .role(gpui::Role::Tree)
            .aria_label("Sessions")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            // A pinned tab dragged back down unpins.
            .drag_over::<super::drag::DraggedTab>(|s, _, _, _| s.bg(theme::hover()))
            .on_drop(cx.listener(|this, d: &super::drag::DraggedTab, _, cx| {
                this.lifted_tab = None;
                this.unpin_tab(d.id, cx)
            }));
        if heading {
            list = list.child(self.render_tree_heading(cx));
        }
        for TreeProject {
            pi,
            rows,
            open,
            urgent,
        } in self.tree_listing(query.as_deref(), cx)
        {
            if let Some(pi) = pi {
                list = list.child(self.render_tree_project(
                    pi,
                    rows.len(),
                    open,
                    urgent.filter(|_| !open),
                    &layout,
                    cx,
                ));
            }
            if open {
                for row in rows {
                    let selected = open_id.as_ref() == Some(&row.id);
                    list = list.child(self.render_tree_row(row, selected, &layout, cx));
                }
            }
        }
        // Archived sessions are a click away, as in the projects list.
        let archived = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .filter(|s| s.settled)
            .count();
        if !menu && archived > 0 {
            list = list.child(
                super::panel::archived_toggle("tree-archived", archived, "", self.show_settled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_settled = !this.show_settled;
                        this.drop_pick();
                        cx.notify();
                    })),
            );
        }
        // Sessions picked with Ctrl or Shift get the bar that acts on them;
        // not in the Sessions menu, which closes on the first click.
        list.when(!menu, |d| d.children(self.render_tree_pick_bar(cx)))
            .into_any_element()
    }

    /// Starts a session in project `pi`, which comes on screen first, and
    /// opens its sessions in the tree so the new one shows.
    fn new_session_in(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.projects.get(pi).map(|p| p.path.clone()) else {
            return;
        };
        self.menu = None;
        self.drop_pick();
        self.tree_open.insert(path, true);
        if pi != self.active_project {
            self.select_project(pi, window, cx);
        }
        self.new_session(window, cx);
    }

    /// "Sessions", and the + for a new session or project.
    fn render_tree_heading(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .h(px(28.))
            .pl(crate::ui::sp(8.))
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(div().flex_1().min_w_0().child("Sessions"))
            .child(self.tree_sort_button("tree-sort", cx))
            // As the titled segment's +: with the tree in place of the
            // project switcher, this is where a project is added too.
            .child(
                icon_button("tree-new", "add01", 14., "New session or project")
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_dialog(window, cx))),
            )
    }

    /// Switches the tree to the next order. It says the order in use, as
    /// the icon alone can't.
    pub(super) fn tree_sort_button(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let layout = crate::tab_layout::current();
        let next = layout.with(|l| l.sort = l.sort.next());
        super::rail::heading_icon_button(
            id,
            "sorting01",
            format!("Sort sessions: {}", layout.sort.label()),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.drop_pick();
            this.apply_tab_layout(next, None, cx)
        }))
    }

    fn render_tree_project(
        &self,
        pi: usize,
        count: usize,
        open: bool,
        urgent: Option<RowState>,
        layout: &TabLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project = &self.projects[pi];
        let logo: Hsla = gpui::rgb(project.logo).into();
        let path = project.path.clone();
        let label = format!(
            "{}, {count} session{}",
            project.name,
            if count == 1 { "" } else { "s" }
        );
        div()
            .id(("tree-project", pi))
            .group("tree-project")
            .map(|d| ui::labelled(d, gpui::Role::TreeItem, label))
            .aria_expanded(open)
            .relative()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(ROW_H))
            .px(crate::ui::sp(4.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_MD))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .hover(|s| s.bg(theme::hover()))
            .child(
                icon(
                    if open {
                        "arrow-down01"
                    } else {
                        "arrow-right01"
                    },
                    12.,
                )
                .text_color(theme::text_faint()),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .child(ui::project_icon(project.icon, 1.5, logo)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(project.name.clone()),
            )
            .children(urgent.and_then(|s| s.mark(layout.state)))
            .child(
                div()
                    .flex_none()
                    .pr(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .group_hover("tree-project", |s| s.invisible())
                    .child(count.to_string()),
            )
            // On hover, the count gives way to a + for a session in this project.
            .child(
                div()
                    .absolute()
                    .right(crate::ui::sp(2.))
                    .invisible()
                    .group_hover("tree-project", |s| s.visible())
                    .child(
                        icon_button(
                            ("tree-project-new", pi),
                            "add01",
                            14.,
                            format!("New session in {}", project.name),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.new_session_in(pi, window, cx)
                            },
                        )),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tree_open.insert(path.clone(), !open);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu = Some(ContextMenu {
                        target: MenuTarget::Project(pi),
                        at: ev.position,
                    });
                    cx.notify();
                }),
            )
    }

    fn render_tree_row(
        &self,
        row: Row,
        selected: bool,
        layout: &TabLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Row {
            pi,
            ei,
            si,
            id,
            title,
            agent,
            state,
            pr,
            updated: _,
            can_archive,
        } = row;
        let pinned = layout.pinned_bar() && self.is_pinned(&id);
        let env_path = self.projects[pi].environments[ei].path.clone();
        let picked = self.tree_pick.contains(&id);
        let dragged =
            super::drag::DraggedSession::new(env_path.clone(), id.clone(), title.clone(), agent);
        let shell = cx.entity();
        // From the shell's state, not `group_hover`: that one only paints, and
        // the title needs the room the end of the row gives up.
        let archive_shown = can_archive && self.tree_hover.as_ref().is_some_and(|h| h.2 == id);
        let hovered = id.clone();
        let title: SharedString = if title.is_empty() {
            "Untitled session".into()
        } else {
            title
        };
        let end = state.end(layout.state, pr);
        let two_lines = layout.titles == LongTitles::TwoLines;
        let label = match state.word() {
            Some(word) => format!("{title}, {word}"),
            None => title.to_string(),
        };
        let project = &self.projects[pi];
        let logo: Hsla = gpui::rgb(project.logo).into();
        let opens = id.clone();
        let picks = id.clone();
        let archives = id.clone();
        div()
            .id(SharedString::from(format!("tree-session-{id}")))
            .relative()
            .map(|d| ui::labelled(d, gpui::Role::TreeItem, label))
            .aria_selected(selected || picked)
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .min_h(px(ROW_H))
            .when(two_lines, |d| d.py(crate::ui::sp(4.)))
            .when(layout.group, |d| d.ml(px(14.)))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_MD))
            .text_color(if selected || state == RowState::NeedsYou {
                theme::content()
            } else {
                theme::text_muted()
            })
            .when(selected, |d| d.bg(theme::selection()))
            .when(!selected, |d| {
                d.hover(|s| s.bg(theme::hover()).text_color(theme::content()))
            })
            .children(state.mark(layout.state))
            // Without project headings, each row says its project.
            .when(!layout.group, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(14.))
                        .child(ui::project_icon(project.icon, 1.25, logo)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .map(|d| {
                        if two_lines {
                            d.line_clamp(2)
                        } else {
                            d.truncate()
                        }
                    })
                    // Room for the archive button, so it never covers the title.
                    .when(archive_shown, |d| d.mr(px(ui::ICON_BUTTON)))
                    .child(title),
            )
            .children(end.filter(|_| !archive_shown).map(|text| {
                div()
                    .flex_none()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(state.word_color())
                    .child(text)
            }))
            // Small and faint: the tab in the bar already says it.
            .when(pinned && !archive_shown, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("tree-pinned-{id}")))
                        .flex_none()
                        .map(|d| ui::labelled(d, gpui::Role::Image, "Pinned"))
                        .child(icon("pin", 11.).text_color(theme::text_faint())),
                )
            })
            // On hover, what the row ends with gives way to an archive button.
            .when(archive_shown, |d| {
                d.child(
                    div().absolute().right(crate::ui::sp(4.)).child(
                        icon_button(
                            SharedString::from(format!("tree-archive-{id}")),
                            "archive02",
                            14.,
                            "Archive",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.archive_sessions(&env_path, &[archives.to_string()], cx);
                        })),
                    ),
                )
            })
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if *over {
                    this.tree_hover = Some((pi, ei, hovered.clone()));
                } else if this.tree_hover.as_ref().is_some_and(|h| h.2 == hovered) {
                    this.tree_hover = None;
                } else {
                    return;
                }
                // The archive button comes and goes with the pointer.
                cx.notify();
            }))
            .when(picked, |d| d.bg(theme::selection_strong()))
            // Ctrl (Cmd) or Shift picks the session; a plain click opens it.
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                this.menu = None;
                let m = ev.modifiers();
                if m.secondary() || m.shift {
                    this.click_tree_pick(picks.clone(), m.shift, cx);
                    return;
                }
                this.drop_pick();
                this.open_session_at(pi, ei, opens.clone(), window, cx)
            }))
            // Dragged up to the title bar, it pins.
            .when(layout.pinned_bar(), |d| {
                d.on_drag(dragged, move |d, offset, _, cx| {
                    shell.update(cx, |this, cx| this.start_session_drag(d, offset, cx))
                })
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu = Some(ContextMenu {
                        target: MenuTarget::TreeSession(pi, ei, si),
                        at: ev.position,
                    });
                    cx.notify();
                }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_says_its_session_before_its_checks() {
        use RowState::*;
        assert_eq!(
            RowState::of(Status::NeedsInput, Some(ci::State::Failed)),
            NeedsYou
        );
        assert_eq!(
            RowState::of(Status::Working, Some(ci::State::Passed)),
            Working
        );
        assert_eq!(RowState::of(Status::Error, None), Failed);
        assert_eq!(
            RowState::of(Status::Idle, Some(ci::State::Failed)),
            CiFailed
        );
        assert_eq!(RowState::of(Status::Idle, Some(ci::State::Passed)), Passed);
        assert_eq!(RowState::of(Status::Idle, Some(ci::State::Running)), Idle);
        assert_eq!(RowState::of(Status::Idle, None), Idle);
    }

    #[test]
    fn the_end_of_a_row_names_what_wants_you_else_the_pull_request() {
        use RowState::*;
        assert_eq!(
            NeedsYou.end(StateMark::Icon, Some(5)).as_deref(),
            Some("Needs you")
        );
        assert_eq!(
            CiFailed.end(StateMark::Icon, None).as_deref(),
            Some("CI failed")
        );
        assert_eq!(
            Working.end(StateMark::Icon, Some(677)).as_deref(),
            Some("#677")
        );
        assert_eq!(
            Passed.end(StateMark::Dot, Some(682)).as_deref(),
            Some("#682")
        );
        assert_eq!(Idle.end(StateMark::Icon, None), None);
        // As words, every state but idle says itself.
        assert_eq!(
            Working.end(StateMark::Word, Some(677)).as_deref(),
            Some("Working")
        );
        assert_eq!(Idle.end(StateMark::Word, Some(9)).as_deref(), Some("#9"));
        assert!(Working.mark(StateMark::Word).is_none());
    }

    #[test]
    fn shift_picks_the_sessions_between_the_anchor_and_the_row() {
        let ids: Vec<SharedString> = ["a", "b", "c", "d"].map(SharedString::from).to_vec();
        let id = |s: &str| SharedString::from(s.to_string());
        assert_eq!(
            pick_range(&ids, Some(&id("d")), &id("b")),
            ids[1..=3].to_vec()
        );
        assert_eq!(
            pick_range(&ids, Some(&id("a")), &id("c")),
            ids[..=2].to_vec()
        );
        // An anchor the tree no longer shows picks the row alone.
        assert_eq!(pick_range(&ids, Some(&id("gone")), &id("c")), vec![id("c")]);
        assert_eq!(pick_range(&ids, None, &id("b")), vec![id("b")]);
    }

    #[test]
    fn sessions_sort_latest_first_by_title_or_as_listed() {
        let row = |id: &str, title: &str, updated: u64| Row {
            pi: 0,
            ei: 0,
            si: 0,
            id: id.to_string().into(),
            title: title.to_string().into(),
            agent: crate::agent::AgentKind::Claude,
            state: RowState::Idle,
            pr: None,
            updated,
            can_archive: true,
        };
        let rows = || {
            vec![
                row("a", "zebra", 10),
                row("b", "mango", 30),
                row("c", "Apple", 20),
                row("d", "kiwi", 30),
            ]
        };
        let ids = |sort| {
            let mut r = rows();
            sort_rows(&mut r, sort);
            r.into_iter().map(|r| r.id.to_string()).collect::<Vec<_>>()
        };
        // A tie keeps the listed order.
        assert_eq!(ids(Sort::Latest), ["b", "d", "c", "a"]);
        assert_eq!(ids(Sort::Title), ["c", "d", "b", "a"]);
        assert_eq!(ids(Sort::Environment), ["a", "b", "c", "d"]);
    }

    #[test]
    fn quiet_projects_fold_or_hide_unless_busy_or_on_screen() {
        use Quiet::*;
        assert_eq!(project_open(Folded, false, false, None), Some(false));
        assert_eq!(project_open(Folded, true, false, None), Some(true));
        assert_eq!(project_open(Folded, false, true, None), Some(true));
        assert_eq!(project_open(Open, false, false, None), Some(true));
        assert_eq!(project_open(Hidden, false, false, None), None);
        assert_eq!(project_open(Hidden, true, false, None), Some(true));
        // The user's own fold wins wherever the project shows.
        assert_eq!(project_open(Folded, false, false, Some(true)), Some(true));
        assert_eq!(project_open(Open, true, false, Some(false)), Some(false));
        assert_eq!(project_open(Hidden, false, false, Some(true)), None);
    }
}
