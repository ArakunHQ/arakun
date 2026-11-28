//! The sidebar's tree of sessions (`tab_layout::SessionsIn::Sidebar`): every
//! session of every project, under its project, each with a mark for its
//! state. It takes the place of the projects and environments list.

pub(super) mod folded;
pub(super) mod keys;

use super::*;
use crate::tab_layout::{LongTitles, Quiet, Sort, StateMark, TabLayout};

/// Height of a session row whose title fits on one line.
const ROW_H: f32 = 30.;

/// How strong the title and mark of a session with no tab open are drawn,
/// over the faint gray: about a quarter of full text, so the sessions in
/// use stand out. The theme has no gray that far back.
const DIMMED: f32 = 0.67;

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

    /// Something the user has to see to: it stays lit and keeps its word
    /// whatever else the row would do.
    fn wants(self) -> bool {
        matches!(self, Self::NeedsYou | Self::Failed | Self::CiFailed)
    }

    /// Whether a row in this state is drawn at full strength even when its
    /// session has no tab open: the state wins over the dimming.
    fn stays_lit(self) -> bool {
        self.wants() || self == Self::Working
    }

    /// What the end of a row says: the state when it wants the user (or
    /// always, when states show as words); else, under the pointer, how
    /// long ago the session moved (`age`); else the pull request.
    fn end(self, style: StateMark, pr: Option<u64>, age: Option<&str>) -> Option<String> {
        if (self.wants() || style == StateMark::Word)
            && let Some(word) = self.word()
        {
            return Some(word.into());
        }
        if let Some(age) = age {
            return Some(age.into());
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

/// Whether a session's row goes under its project's "N done" line: its
/// checks passed, and it is neither pinned nor the one on screen.
fn folds_as_done(state: RowState, pinned: bool, on_screen: bool) -> bool {
    state == RowState::Passed && !pinned && !on_screen
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

/// What a session's row says: the identifier of the Linear issue it was
/// started for, if any, then its title. The first title is
/// `ENG-412: <issue title>`; with the identifier before it, the title drops
/// its own copy so the row doesn't say it twice.
pub(super) fn row_title(title: &str, linear: Option<&str>) -> (Option<String>, String) {
    let Some(issue) = linear.filter(|l| !l.is_empty()) else {
        return (None, title.to_string());
    };
    let rest = title
        .strip_prefix(issue)
        .and_then(|r| r.strip_prefix(": ").or_else(|| r.strip_prefix(' ')))
        .map(str::trim_start)
        .filter(|r| !r.is_empty());
    (Some(issue.to_string()), rest.unwrap_or(title).to_string())
}

/// What a row says to screen readers: its issue and title, then its state.
pub(super) fn row_label(title: &str, linear: Option<&str>, state: RowState) -> String {
    let title = if title.is_empty() {
        "Untitled session"
    } else {
        title
    };
    let named = match row_title(title, linear) {
        (Some(issue), title) => format!("{issue} {title}"),
        (None, title) => title,
    };
    match state.word() {
        Some(word) => format!("{named}, {word}"),
        None => named,
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
    /// The Linear issue it was started for.
    linear: Option<SharedString>,
    agent: crate::agent::AgentKind,
    state: RowState,
    pr: Option<u64>,
    /// Unix seconds of its last activity.
    updated: u64,
    /// Not working, not archived yet and not on a server: the same rule as
    /// the context menu's Archive.
    can_archive: bool,
    /// Archived: it never goes up to Needs you.
    settled: bool,
    /// It has a tab open. Without one the row is drawn dimmed.
    tab: bool,
    /// Its tab is pinned to the title bar: the row goes first in its
    /// project and shows a pin.
    pinned: bool,
    /// The part of the tree it sits in, a rule between one and the next
    /// (`sort_rows` sets it).
    band: Band,
    /// Its place in the order the user dragged its project's sessions into;
    /// `None` for a session that came since.
    manual: Option<usize>,
    /// Finished: it goes under its project's "N done" line
    /// (`tree_listing` sets it).
    done: bool,
}

/// The parts of a project's sessions in the tree, top to bottom, with a rule
/// between one part and the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Band {
    /// Pinned to the title bar, in every order.
    Pinned,
    /// With a tab open, in the orders that set them apart.
    Open,
    #[default]
    Rest,
    /// Finished, folded into the "N done" line at the end of its project.
    Done,
}

/// Where each session sat when the pointer came over the tree: its place in
/// its list, and the part of the tree it was in.
pub(super) type HeldRows = std::collections::HashMap<SharedString, (usize, Band)>;

/// Puts `rows` in the order `sort` asks for. The sort is stable, so rows
/// that tie keep the environments' order. Pinned sessions go first in every
/// order; an order that moves sessions puts the ones with an open tab next,
/// each part in that order. Finished sessions (`Row::done`) go last. With
/// the pointer over the tree (`held`), every row stays where it was
/// instead: a row never leaves from under the pointer. A session that came
/// since goes first.
fn sort_rows(rows: &mut [Row], sort: Sort, held: Option<&HeldRows>) {
    match sort {
        Sort::Recent => rows.sort_by_key(|r| std::cmp::Reverse(r.updated)),
        Sort::Name => rows.sort_by_cached_key(|r| r.title.to_lowercase()),
        // Project by project without project headings; a session that
        // came since goes first, where the user sees it.
        Sort::Manual => rows.sort_by_key(|r| (r.pi, r.manual.map_or(0, |m| m + 1))),
    }
    let split = sort.open_tabs_first();
    for r in rows.iter_mut() {
        r.band = if r.done {
            Band::Done
        } else if r.pinned {
            Band::Pinned
        } else if split && r.tab {
            Band::Open
        } else {
            Band::Rest
        };
    }
    rows.sort_by_key(|r| r.band);
    if let Some(held) = held {
        for r in rows.iter_mut() {
            if let Some((_, band)) = held.get(&r.id) {
                r.band = *band;
            }
        }
        rows.sort_by_key(|r| held.get(&r.id).map_or(0, |(place, _)| place + 1));
    }
}

/// `order` with `moved` put in `target`'s place: the rows between them move
/// up or down one to make room, as when a card is put down on another.
fn move_onto(order: &mut Vec<SharedString>, moved: &SharedString, target: &SharedString) {
    let (Some(from), Some(to)) = (
        order.iter().position(|id| id == moved),
        order.iter().position(|id| id == target),
    ) else {
        return;
    };
    let id = order.remove(from);
    order.insert(to, id);
}

/// The orders after a drag in the tree: the project's own, and the one for
/// every project when that changes. With project headings only the project
/// dragged in goes to Manual; without them there is one order for all.
fn order_after_drag(grouped: bool, all: Sort, own: Option<Sort>) -> (Option<Sort>, Option<Sort>) {
    if !grouped {
        return (own, (all != Sort::Manual).then_some(Sort::Manual));
    }
    match own.unwrap_or(all) {
        Sort::Manual => (own, None),
        _ => (Some(Sort::Manual), None),
    }
}

/// The rule between the sessions with an open tab and the rest.
fn tree_rule(grouped: bool) -> gpui::Div {
    div()
        .flex_none()
        .h(px(1.))
        .my(crate::ui::sp(4.))
        .mr(crate::ui::sp(8.))
        .ml(if grouped {
            px(14.) + crate::ui::sp(8.)
        } else {
            crate::ui::sp(8.)
        })
        .bg(theme::border_subtle())
}

/// The order of the Needs you list, as indices into `rows`, given as
/// `(id, state, last activity)`: what waits on the user, then what failed,
/// then failed checks, the newest first in each. While the pointer is over
/// the sidebar (`held`), the sessions listed when it came stay in their
/// places whatever their state now, and none joins: no row moves from under
/// the pointer.
pub(super) fn needs_order(
    rows: &[(SharedString, RowState, u64)],
    held: Option<&[SharedString]>,
) -> Vec<usize> {
    if let Some(held) = held {
        return held
            .iter()
            .filter_map(|id| rows.iter().position(|r| r.0 == *id))
            .collect();
    }
    let mut ix: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].1.wants()).collect();
    ix.sort_by_key(|&i| (rows[i].1.rank(), std::cmp::Reverse(rows[i].2)));
    ix
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
                    linear: s.linear.clone(),
                    agent: s.agent,
                    state: RowState::of(s.shown_status(), ci),
                    pr,
                    updated: s.updated,
                    can_archive: !s.settled && s.status != Status::Working && env.remote.is_none(),
                    settled: s.settled,
                    tab: self.strip.session_tab(&s.id).is_some(),
                    pinned: self.is_pinned(&s.id),
                    band: Band::Rest,
                    manual: project.session_order.iter().position(|id| *id == s.id),
                    done: false,
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
        let held = self.rail.tree_held.as_ref();
        // Listed in Needs you instead, so each session shows once. The
        // Sessions menu, which searches, has them all.
        let above: Vec<SharedString> = if query.is_none() {
            self.needs_rows(cx).into_iter().map(|r| r.id).collect()
        } else {
            Vec::new()
        };
        // Finished sessions fold into one line per project, but not in a
        // search, nor the one on screen, which would vanish from under the
        // user, nor a pinned one, which says why it is first.
        let fold_done = layout.group && query.is_none();
        let active = self.active_session_id();
        let rows_of = |pi: usize| {
            let mut rows = self.tree_rows(pi, cx);
            rows.retain(|r| !above.contains(&r.id));
            if fold_done {
                for r in &mut rows {
                    r.done = folds_as_done(r.state, r.pinned, active.as_ref() == Some(&r.id));
                }
            }
            if let Some(q) = query {
                // The issue too: the agent may have retitled the session.
                rows.retain(|r| {
                    r.title.to_lowercase().contains(q)
                        || r.linear
                            .as_ref()
                            .is_some_and(|l| l.to_lowercase().contains(q))
                });
            }
            rows
        };
        if !layout.group {
            let mut rows: Vec<Row> = (0..self.projects.len()).flat_map(rows_of).collect();
            sort_rows(&mut rows, layout.sort, held);
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
            sort_rows(&mut rows, project.sort.unwrap_or(layout.sort), held);
            let busy = rows.iter().any(|r| r.state.busy());
            let here = pi == self.active_project;
            let user = self.rail.tree_open.get(&project.path).copied();
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

    /// Where every session sits now, to hold it there while the pointer is
    /// over the tree.
    fn tree_places(&self, cx: &App) -> HeldRows {
        self.tree_listing(None, cx)
            .into_iter()
            .flat_map(|p| {
                p.rows
                    .into_iter()
                    .enumerate()
                    .map(|(place, r)| (r.id, (place, r.band)))
            })
            .collect()
    }

    /// The sessions in Needs you, in its order. None while the sidebar
    /// doesn't show the list: the tree keeps them then.
    fn needs_rows(&self, cx: &App) -> Vec<Row> {
        if !self.needs_list_shown() {
            return Vec::new();
        }
        let mut rows: Vec<Row> = (0..self.projects.len())
            .flat_map(|pi| self.tree_rows(pi, cx))
            .filter(|r| !r.settled)
            .collect();
        let keys: Vec<_> = rows
            .iter()
            .map(|r| (r.id.clone(), r.state, r.updated))
            .collect();
        let order = needs_order(&keys, self.rail.needs_held.as_deref());
        let mut picked: Vec<Option<Row>> = rows.drain(..).map(Some).collect();
        order.into_iter().filter_map(|i| picked[i].take()).collect()
    }

    /// The pointer came over the sidebar (`over`) or left it: hold Needs you
    /// and the tree as they are until it leaves, so no row moves from under
    /// it, or let them go.
    pub(super) fn hold_rail_rows(&mut self, over: bool, cx: &App) {
        if !crate::tab_layout::current().tree() {
            return;
        }
        // Needs you first: what it lists is what the tree leaves out.
        self.rail.needs_held = None;
        self.rail.tree_held = None;
        if over {
            self.rail.needs_held = Some(self.needs_rows(cx).into_iter().map(|r| r.id).collect());
            self.rail.tree_held = Some(self.tree_places(cx));
        }
    }

    /// The projects with an order of their own, ticked in the tree menu's
    /// Select projects list.
    pub(super) fn tree_sort_picked(&self) -> Vec<usize> {
        (0..self.projects.len())
            .filter(|&pi| self.projects[pi].sort.is_some())
            .collect()
    }

    /// The order the tree menu shows picked: the one for every project, or,
    /// on Select projects, the one the ticked projects share. `None` when
    /// they have different ones.
    pub(super) fn tree_menu_sort(&self) -> Option<Sort> {
        let all = crate::tab_layout::current().sort;
        let picked = self.tree_sort_picked();
        if !self.rail.tree_sort_select || picked.is_empty() {
            return Some(all);
        }
        let first = self.projects[picked[0]].sort;
        picked
            .iter()
            .all(|&pi| self.projects[pi].sort == first)
            .then_some(first)
            .flatten()
    }

    /// An order picked in the tree menu: for the ticked projects on Select
    /// projects, else for every project.
    pub(super) fn set_tree_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
        self.drop_pick();
        let picked = self.tree_sort_picked();
        if self.rail.tree_sort_select && !picked.is_empty() {
            for pi in picked {
                self.projects[pi].sort = Some(sort);
            }
            self.save(cx);
        } else {
            let layout = crate::tab_layout::current().with(|l| l.sort = sort);
            self.apply_tab_layout(layout, None, cx);
        }
    }

    /// The tree menu's Applies to. All projects takes every project back to
    /// the one order; what was dragged by hand stays for Manual.
    pub(super) fn set_tree_sort_scope(&mut self, select: bool, cx: &mut Context<Self>) {
        self.rail.tree_sort_select = select;
        if !select && !self.tree_sort_picked().is_empty() {
            for p in &mut self.projects {
                p.sort = None;
            }
            self.save(cx);
        }
        cx.notify();
    }

    /// Ticks project `pi` in the Select projects list, giving it the order
    /// the menu shows, or unticks it back to the order for every project.
    pub(super) fn toggle_tree_sort_project(&mut self, pi: usize, cx: &mut Context<Self>) {
        let shown = self
            .tree_menu_sort()
            .unwrap_or(crate::tab_layout::current().sort);
        if let Some(p) = self.projects.get_mut(pi) {
            p.sort = match p.sort {
                Some(_) => None,
                None => Some(shown),
            };
            self.save(cx);
            cx.notify();
        }
    }

    /// The sessions the tree shows, top to bottom: what Shift+click picks
    /// a range of.
    fn tree_order(&self, cx: &App) -> Vec<SharedString> {
        let needs = self.needs_rows(cx).into_iter().map(|r| r.id);
        let done_open = |pi: Option<usize>| {
            pi.and_then(|pi| self.projects.get(pi))
                .is_some_and(|p| p.done_open)
        };
        let tree = self
            .tree_listing(None, cx)
            .into_iter()
            .filter(|p| p.open)
            .flat_map(|p| {
                let shown = done_open(p.pi);
                p.rows
                    .into_iter()
                    .filter(move |r| shown || r.band != Band::Done)
                    .map(|r| r.id)
            });
        needs.chain(tree).collect()
    }

    /// Session `d` dropped on the tree's row of session `target`, in the
    /// same project: it takes that row's place. That project's order becomes
    /// Manual, starting from what the tree shows, so no other row jumps; the
    /// other projects keep theirs. Without project headings there is one
    /// order for every project, and it becomes Manual.
    fn drop_session_on_tree_row(
        &mut self,
        d: &super::drag::DraggedSession,
        target: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.lifted = None;
        let place = |id: &SharedString| self.session_place(id).map(|(pi, _)| pi);
        let pi = match (place(&d.id), place(&target)) {
            (Some(a), Some(b)) if a == b && d.id != target => a,
            _ => return cx.notify(),
        };
        // What the tree shows now, rows held under the pointer and all.
        let mut order: Vec<SharedString> = self
            .tree_listing(None, cx)
            .into_iter()
            .flat_map(|p| p.rows)
            .filter(|r| r.pi == pi)
            .map(|r| r.id)
            .collect();
        move_onto(&mut order, &d.id, &target);
        // Sessions the tree doesn't show now, archived ones, keep their
        // places after the rest.
        let project = &mut self.projects[pi];
        let rest: Vec<SharedString> = project
            .session_order
            .iter()
            .filter(|id| !order.contains(id))
            .cloned()
            .collect();
        order.extend(rest);
        project.session_order = order;
        let layout = crate::tab_layout::current();
        let (own, all) = order_after_drag(layout.group, layout.sort, project.sort);
        project.sort = own;
        match all {
            Some(sort) => self.apply_tab_layout(layout.with(|l| l.sort = sort), None, cx),
            None => self.save(cx),
        }
        // The pointer is still over the tree: hold the rows where they are now.
        if self.rail.tree_held.take().is_some() {
            self.rail.tree_held = Some(self.tree_places(cx));
        }
        cx.notify();
    }

    /// A tree row clicked with Ctrl (Cmd) or Shift: Ctrl adds the session to
    /// the pick or takes it out, Shift picks the rows from the last one
    /// clicked. Picking here drops a pick made in the projects list.
    fn click_tree_pick(&mut self, id: SharedString, shift: bool, cx: &mut Context<Self>) {
        self.rail.pick.clear();
        self.rail.pick_anchor = None;
        if shift {
            let order = self.tree_order(cx);
            // From the open session when nothing was clicked yet.
            let anchor = self
                .rail
                .tree_pick_anchor
                .clone()
                .or_else(|| self.active_session_id());
            // Without a listed anchor the row is picked alone and anchors
            // the next Shift+click.
            let anchor = anchor.filter(|a| order.contains(a));
            for id in pick_range(&order, anchor.as_ref(), &id) {
                if !self.rail.tree_pick.contains(&id) {
                    self.rail.tree_pick.push(id);
                }
            }
            self.rail.tree_pick_anchor = anchor.or(Some(id));
        } else {
            match self.rail.tree_pick.iter().position(|p| *p == id) {
                Some(i) => {
                    self.rail.tree_pick.remove(i);
                }
                None => self.rail.tree_pick.push(id.clone()),
            }
            self.rail.tree_pick_anchor = Some(id);
        }
        cx.notify();
    }

    /// The picked sessions that still exist: their project, environment and id.
    fn tree_picked(&self) -> Vec<(usize, usize, SharedString)> {
        self.rail
            .tree_pick
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

    /// What the picked sessions allow: archive and close.
    fn tree_pick_allows(&self, picked: &[(usize, usize, SharedString)]) -> (bool, bool) {
        let can_archive = picked.iter().any(|(pi, ei, id)| {
            let env = &self.projects[*pi].environments[*ei];
            env.session_index(id)
                .and_then(|i| env.sessions.get(i))
                .is_some_and(|s| !s.settled && s.status != Status::Working && env.remote.is_none())
        });
        let can_close = picked
            .iter()
            .any(|(_, _, id)| self.strip.session_tab(id).is_some());
        (can_archive, can_close)
    }

    /// A right-click on session `si` of the tree. On a picked session, while
    /// more than one is picked, the menu acts on all of them, as the bar
    /// does. Another session drops the pick.
    fn open_tree_menu(
        &mut self,
        pi: usize,
        ei: usize,
        si: usize,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let on_pick = self
            .projects
            .get(pi)
            .and_then(|p| p.environments.get(ei))
            .and_then(|e| e.sessions.get(si))
            .is_some_and(|s| self.rail.tree_pick.contains(&s.id));
        let target = if on_pick && self.tree_picked().len() > 1 {
            MenuTarget::TreePicked
        } else {
            self.drop_pick();
            MenuTarget::TreeSession(pi, ei, si)
        };
        self.menu = Some(ContextMenu { target, at });
        cx.notify();
    }

    /// The menu of a right-click on one of several picked sessions: the
    /// bar's actions, on all of them.
    pub(super) fn render_tree_pick_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let picked = self.tree_picked();
        let n = picked.len();
        let (can_archive, can_close) = self.tree_pick_allows(&picked);
        let all_pinned = picked.iter().all(|(_, _, id)| self.is_pinned(id));
        ui::menu_panel("tree-pick-menu", 240.)
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(format!("{n} sessions selected")),
            )
            .when(crate::tab_layout::current().pinned_bar(), |d| {
                d.child(
                    ui::menu_item(
                        "tpm-pin",
                        if all_pinned { "pin-off" } else { "pin" },
                        if all_pinned {
                            "Unpin from the title bar"
                        } else {
                            "Pin to the title bar"
                        },
                        false,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        this.pin_tree_picked(cx);
                    })),
                )
            })
            .child(
                ui::menu_item("tpm-archive", "archive02", "Archive", false, can_archive).when(
                    can_archive,
                    |d| {
                        d.on_click(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            this.archive_tree_picked(cx);
                        }))
                    },
                ),
            )
            .child(
                ui::menu_item("tpm-close", "cancel-circle", "Close tabs", false, can_close).when(
                    can_close,
                    |d| {
                        d.on_click(cx.listener(|this, _, window, cx| {
                            this.menu = None;
                            this.close_tree_picked(window, cx);
                        }))
                    },
                ),
            )
            .into_any_element()
    }

    /// "N selected" and what to do with the picked sessions, under the tree.
    fn render_tree_pick_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picked = self.tree_picked();
        if picked.is_empty() {
            return None;
        }
        let n = picked.len();
        let (can_archive, can_close) = self.tree_pick_allows(&picked);
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
            .then(|| self.rail.tree_filter.read(cx).text().trim().to_lowercase())
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
            }))
            // The rows hold their places while the pointer is over the
            // tree, and take the order's again once it leaves. In the
            // sidebar the whole rail holds them (`hold_rail_rows`).
            .when(menu, |d| {
                d.on_hover(cx.listener(|this, over: &bool, _, cx| {
                    this.rail.tree_held = over.then(|| this.tree_places(cx));
                    cx.notify();
                }))
            });
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
                let (done, live): (Vec<Row>, Vec<Row>) =
                    rows.into_iter().partition(|r| r.band == Band::Done);
                let mut band = None;
                for row in live {
                    if band.is_some_and(|b| b != row.band) {
                        list = list.child(tree_rule(layout.group));
                    }
                    band = Some(row.band);
                    let selected = open_id.as_ref() == Some(&row.id);
                    list = list.child(self.render_tree_row(row, selected, false, &layout, cx));
                }
                if let (Some(pi), false) = (pi, done.is_empty()) {
                    let shown = self.projects[pi].done_open;
                    list = list.child(self.render_tree_done(pi, done.len(), shown, cx));
                    if shown {
                        for row in done {
                            let selected = open_id.as_ref() == Some(&row.id);
                            list =
                                list.child(self.render_tree_row(row, selected, false, &layout, cx));
                        }
                    }
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

    /// Needs you: the sessions of every project that wait on the user or
    /// failed, the worst first, with their count. Nothing while none does.
    pub(super) fn render_needs_you(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rows = self.needs_rows(cx);
        if rows.is_empty() {
            return None;
        }
        let layout = crate::tab_layout::current();
        let open_id = self.active_session_id();
        let n = rows.len();
        let mut list = div()
            .id("needs-you")
            .role(gpui::Role::Tree)
            .aria_label(format!(
                "Needs you, {n} session{}",
                if n == 1 { "" } else { "s" }
            ))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            .pb(crate::ui::sp(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .px(crate::ui::sp(8.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(div().flex_1().min_w_0().child("Needs you"))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::needs_you())
                            .child(n.to_string()),
                    ),
            );
        for row in rows {
            let selected = open_id.as_ref() == Some(&row.id);
            list = list.child(self.render_tree_row(row, selected, true, &layout, cx));
        }
        Some(list.into_any_element())
    }

    /// Starts a session in project `pi`, which comes on screen first, and
    /// opens its sessions in the tree so the new one shows.
    fn new_session_in(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.projects.get(pi).map(|p| p.path.clone()) else {
            return;
        };
        self.menu = None;
        self.drop_pick();
        self.rail.tree_open.insert(path, true);
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

    /// The order in use, which the icon alone can't say, and the button
    /// that opens the menu to pick another (`render_tree_sort_menu`).
    pub(super) fn tree_sort_button(&self, id: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        let sort = crate::tab_layout::current().sort;
        let open = matches!(
            self.menu.as_ref().map(|m| m.target),
            Some(MenuTarget::TreeSort)
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(4.))
            .child(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(sort.label()),
            )
            .child(
                super::rail::heading_icon_button(
                    id,
                    "preference-horizontal",
                    format!("Sort sessions: {}", sort.label()),
                )
                .when(open, |d| d.bg(theme::fill_selected()))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    cx.stop_propagation();
                    // The pointer is on the tree's own heading: let go of
                    // the rows, or the order picked would wait for it to leave.
                    this.rail.tree_held = None;
                    // On Select projects while any project has its own order.
                    this.rail.tree_sort_select = !this.tree_sort_picked().is_empty();
                    this.open_composer_menu(MenuTarget::TreeSort, ev.position(), cx)
                })),
            )
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
        let keyed = self.keys_on(pi, &keys::KeyRow::Project);
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
            .aria_selected(keyed)
            .child(ui::hover_fill(
                ui::motion::Fills::hover(theme::hover()),
                crate::ui::rad(8.),
            ))
            .when(keyed, |d| d.child(keys::key_ring()))
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
                this.rail.tree_open.insert(path.clone(), !open);
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

    /// The quiet line after project `pi`'s live sessions: a check and
    /// "N done". It opens the finished sessions under it, or folds them.
    fn render_tree_done(
        &self,
        pi: usize,
        n: usize,
        shown: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = format!(
            "{n} done session{} in {}",
            if n == 1 { "" } else { "s" },
            self.projects[pi].name
        );
        div()
            .id(("tree-done", pi))
            .map(|d| ui::labelled(d, gpui::Role::TreeItem, label))
            .map(ui::focusable)
            .aria_expanded(shown)
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ROW_H))
            .ml(px(14.))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_faint())
            .hover(|s| s.text_color(theme::text_muted()))
            .child(ui::hover_fill(
                ui::motion::Fills::hover(theme::hover()),
                crate::ui::rad(8.),
            ))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .opacity(DIMMED)
                    .child(ci_mark(ci::State::Passed)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("{n} done")),
            )
            .child(
                icon(
                    if shown {
                        "arrow-down01"
                    } else {
                        "arrow-right01"
                    },
                    12.,
                )
                .flex_none(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(p) = this.projects.get_mut(pi) {
                    p.done_open = !p.done_open;
                    this.save(cx);
                    cx.notify();
                }
            }))
    }

    /// A session's row: in the tree, or in Needs you (`needs`), where it
    /// says its project with the project's icon instead of a state mark,
    /// and how long it has waited or why it failed instead of the state.
    fn render_tree_row(
        &self,
        row: Row,
        selected: bool,
        needs: bool,
        layout: &TabLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Row {
            pi,
            ei,
            si,
            id,
            title,
            linear,
            agent,
            state,
            pr,
            updated,
            can_archive,
            settled: _,
            tab,
            pinned,
            band: _,
            manual: _,
            done: _,
        } = row;
        let env_path = self.projects[pi].environments[ei].path.clone();
        let picked = self.rail.tree_pick.contains(&id);
        let dragged =
            super::drag::DraggedSession::row(env_path.clone(), id.clone(), title.clone(), agent);
        let shell = cx.entity();
        // From the shell's state, not `group_hover`: that one only paints, and
        // the title needs the room the end of the row gives up.
        let under_pointer = self.rail.tree_hover.as_ref().is_some_and(|h| h.2 == id);
        // Not on the open session: archiving it there is too easy by accident.
        // Nor while a session is dragged over the row.
        let archive_shown = can_archive && under_pointer && !selected && !cx.has_active_drag();
        let hovered = id.clone();
        let title: SharedString = if title.is_empty() {
            "Untitled session".into()
        } else {
            title
        };
        let mut label = row_label(&title, linear.as_deref(), state);
        let (issue, title) = row_title(&title, linear.as_deref());
        // The age shows only under the pointer, so the title has the row's
        // whole width the rest of the time.
        let age = under_pointer.then(|| model::age(updated));
        let end = if needs {
            match state {
                RowState::NeedsYou => Some(model::age(updated).to_string()),
                _ => state.word().map(str::to_string),
            }
        } else {
            state.end(layout.state, pr, age.as_deref())
        };
        // In Needs you the project's icon leads instead of a mark.
        let mark = if needs {
            None
        } else {
            state.mark(layout.state)
        };
        let project_icon = needs || !layout.group;
        // A session with no tab open is a click away, not in use: its row
        // steps back, unless something runs in it or it wants the user. It
        // comes back under the pointer, to be read before it is opened.
        let dimmed = !tab && !selected && !under_pointer && !state.stays_lit();
        let two_lines = layout.titles == LongTitles::TwoLines;
        let project = &self.projects[pi];
        if project_icon {
            label = format!("{label}, {}", project.name);
        }
        let logo: Hsla = gpui::rgb(project.logo).into();
        let opens = id.clone();
        let picks = id.clone();
        let archives = id.clone();
        let onto = id.clone();
        let lifted = self.lifted_session(cx) == Some(&id);
        // Needs you lists the row again: the ring goes on the project's own.
        let keyed = !needs && self.keys_on(pi, &keys::KeyRow::Session(id.clone()));
        div()
            .id(SharedString::from(if needs {
                format!("needs-session-{id}")
            } else {
                format!("tree-session-{id}")
            }))
            .relative()
            .map(|d| ui::labelled(d, gpui::Role::TreeItem, label))
            .aria_selected(selected || picked || keyed)
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .min_h(px(ROW_H))
            .when(two_lines, |d| d.py(crate::ui::sp(4.)))
            .when(layout.group && !needs, |d| d.ml(px(14.)))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_MD))
            .text_color(if selected || state == RowState::NeedsYou {
                theme::content()
            } else if dimmed {
                theme::text_faint()
            } else {
                theme::text_muted()
            })
            .when(selected, |d| d.bg(theme::selection()))
            .when(!selected, |d| {
                d.hover(|s| s.text_color(theme::content()))
                    .child(ui::hover_fill(
                        ui::motion::Fills::hover(theme::hover()),
                        crate::ui::rad(8.),
                    ))
            })
            .children(mark.map(|mark| {
                div()
                    .flex()
                    .flex_none()
                    .when(dimmed, |d| d.opacity(DIMMED))
                    .child(mark)
            }))
            // Without project headings, each row says its project.
            .when(project_icon, |d| {
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
            // The issue a session from Linear was started for, before its
            // title: the agent may have replaced the title that said so.
            .children(issue.map(|issue| {
                div()
                    .flex_none()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .when(dimmed, |d| d.opacity(DIMMED))
                    .child(issue)
            }))
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
                    .when(dimmed, |d| d.opacity(DIMMED))
                    .child(title),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    // Room for the archive button, so it never covers the
                    // end of the row.
                    .when(archive_shown, |d| d.mr(px(ui::ICON_BUTTON)))
                    .children(end.map(|text| {
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(state.word_color())
                            .child(text)
                    }))
                    // In the full text color so pinned rows stand out, not
                    // amber, which only means the session needs you. It stays
                    // under the pointer too: it says why the row is first.
                    .when(pinned, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("tree-pinned-{id}")))
                                .flex()
                                .map(|d| ui::labelled(d, gpui::Role::Image, "Pinned"))
                                .child(icon("pin", 11.).text_color(theme::text())),
                        )
                    }),
            )
            // On hover, an archive button comes after what the row ends with.
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
            // The archive button comes and goes with the pointer, as the
            // row's hover background does.
            .child(super::hover::probe(
                under_pointer,
                cx.listener(move |this, over: &bool, _, cx| {
                    let row = (pi, ei, hovered.clone());
                    if super::hover::track(&mut this.rail.tree_hover, row, *over, |a, b| a.2 == b.2)
                    {
                        cx.notify();
                    }
                }),
            ))
            .when(picked, |d| d.bg(theme::selection_strong()))
            // Ctrl (Cmd) or Shift picks the session; a plain click opens it.
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                this.menu = None;
                let m = ev.modifiers();
                if m.secondary(cx) || m.shift {
                    this.click_tree_pick(picks.clone(), m.shift, cx);
                    return;
                }
                this.drop_pick();
                this.open_session_at(pi, ei, opens.clone(), window, cx)
            }))
            // Dropped on another row, it takes that row's place; up on a bar
            // of pinned tabs, it pins.
            .on_drag(dragged, move |d, offset, _, cx| {
                shell.update(cx, |this, cx| this.start_session_drag(d, offset, cx))
            })
            // Needs you keeps its own order: nothing is put down in it.
            .when(!needs, |d| {
                d.drag_over::<super::drag::DraggedSession>(|s, _, _, _| s.bg(theme::hover()))
                    .on_drop(
                        cx.listener(move |this, d: &super::drag::DraggedSession, _, cx| {
                            cx.stop_propagation();
                            this.drop_session_on_tree_row(d, onto.clone(), cx)
                        }),
                    )
            })
            .when(lifted, |d| d.opacity(0.5))
            .when(keyed, |d| d.child(keys::key_ring()))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_tree_menu(pi, ei, si, ev.position, cx);
                }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_from_linear_says_its_issue_once() {
        let mark = Some("ENG-412");
        let got = row_title;
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            got("ENG-412: Fix the tab", mark),
            (some("ENG-412"), "Fix the tab".into())
        );
        assert_eq!(
            got("ENG-412 Fix the tab", mark),
            (some("ENG-412"), "Fix the tab".into())
        );
        assert_eq!(
            got("A title the agent chose", mark),
            (some("ENG-412"), "A title the agent chose".into())
        );
        assert_eq!(got("Anything", None), (None, "Anything".into()));
        // One issue's identifier can start another's.
        assert_eq!(
            got("ENG-41: Other issue", Some("ENG-4")),
            (some("ENG-4"), "ENG-41: Other issue".into())
        );
        // Nothing left after the identifier: the title stays as it was.
        assert_eq!(
            got("ENG-412: ", mark),
            (some("ENG-412"), "ENG-412: ".into())
        );
    }

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
            NeedsYou.end(StateMark::Icon, Some(5), None).as_deref(),
            Some("Needs you")
        );
        assert_eq!(
            CiFailed.end(StateMark::Icon, None, None).as_deref(),
            Some("CI failed")
        );
        assert_eq!(
            Working.end(StateMark::Icon, Some(677), None).as_deref(),
            Some("#677")
        );
        assert_eq!(
            Passed.end(StateMark::Dot, Some(682), None).as_deref(),
            Some("#682")
        );
        assert_eq!(Idle.end(StateMark::Icon, None, None), None);
        // As words, every state but idle says itself.
        assert_eq!(
            Working.end(StateMark::Word, Some(677), None).as_deref(),
            Some("Working")
        );
        assert_eq!(
            Idle.end(StateMark::Word, Some(9), None).as_deref(),
            Some("#9")
        );
        assert!(Working.mark(StateMark::Word).is_none());
    }

    #[test]
    fn under_the_pointer_a_row_says_its_age_unless_it_wants_you() {
        use RowState::*;
        // The age takes the pull request's place, or the empty end.
        assert_eq!(
            Working
                .end(StateMark::Icon, Some(677), Some("1h"))
                .as_deref(),
            Some("1h")
        );
        assert_eq!(
            Idle.end(StateMark::Dot, None, Some("3d")).as_deref(),
            Some("3d")
        );
        // What wants the user keeps saying so.
        assert_eq!(
            NeedsYou
                .end(StateMark::Icon, Some(5), Some("1h"))
                .as_deref(),
            Some("Needs you")
        );
        assert_eq!(
            Failed.end(StateMark::Icon, None, Some("1h")).as_deref(),
            Some("Failed")
        );
        assert_eq!(
            CiFailed.end(StateMark::Dot, None, Some("2m")).as_deref(),
            Some("CI failed")
        );
        // As words the state is the row's only mark, so it stays too.
        assert_eq!(
            Working.end(StateMark::Word, None, Some("1h")).as_deref(),
            Some("Working")
        );
        assert_eq!(
            Idle.end(StateMark::Word, None, Some("1h")).as_deref(),
            Some("1h")
        );
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

    fn row(id: &str, title: &str, updated: u64, tab: bool) -> Row {
        Row {
            pi: 0,
            ei: 0,
            si: 0,
            id: id.to_string().into(),
            title: title.to_string().into(),
            linear: None,
            agent: crate::agent::AgentKind::Claude,
            state: RowState::Idle,
            pr: None,
            updated,
            can_archive: true,
            settled: false,
            tab,
            pinned: false,
            band: Band::Rest,
            manual: None,
            done: false,
        }
    }

    /// The ids in order, and the part of the tree each sits in.
    fn placed(rows: &[Row]) -> (Vec<String>, Vec<Band>) {
        (
            rows.iter().map(|r| r.id.to_string()).collect(),
            rows.iter().map(|r| r.band).collect(),
        )
    }

    #[test]
    fn sessions_sort_latest_first_by_title_or_as_listed() {
        // Every session has a tab open, so none is set apart.
        let rows = || {
            vec![
                row("a", "zebra", 10, true),
                row("b", "mango", 30, true),
                row("c", "Apple", 20, true),
                row("d", "kiwi", 30, true),
            ]
        };
        let ids = |sort| {
            let mut r = rows();
            sort_rows(&mut r, sort, None);
            placed(&r).0
        };
        // A tie keeps the listed order.
        assert_eq!(ids(Sort::Recent), ["b", "d", "c", "a"]);
        assert_eq!(ids(Sort::Name), ["c", "d", "b", "a"]);
        assert_eq!(ids(Sort::Manual), ["a", "b", "c", "d"]);
    }

    #[test]
    fn sessions_with_an_open_tab_go_above_the_rest_in_the_same_order() {
        let rows = || {
            vec![
                row("a", "zebra", 10, true),
                row("b", "mango", 30, false),
                row("c", "Apple", 20, true),
                row("d", "kiwi", 30, false),
            ]
        };
        let sorted = |sort| {
            let mut r = rows();
            sort_rows(&mut r, sort, None);
            placed(&r)
        };
        let split = vec![Band::Open, Band::Open, Band::Rest, Band::Rest];
        let (ids, top) = sorted(Sort::Recent);
        assert_eq!(ids, ["c", "a", "b", "d"]);
        assert_eq!(top, split);
        let (ids, top) = sorted(Sort::Name);
        assert_eq!(ids, ["c", "a", "d", "b"]);
        assert_eq!(top, split);
        // Manual keeps every session where it is, and nothing is set apart.
        let (ids, top) = sorted(Sort::Manual);
        assert_eq!(ids, ["a", "b", "c", "d"]);
        assert_eq!(top, vec![Band::Rest; 4]);
    }

    #[test]
    fn needs_you_lists_the_worst_first_and_the_newest_first_within() {
        use RowState::*;
        let rows: Vec<(SharedString, RowState, u64)> = [
            ("idle", Idle, 90),
            ("ci", CiFailed, 80),
            ("old", NeedsYou, 10),
            ("runs", Working, 70),
            ("failed", Failed, 5),
            ("new", NeedsYou, 40),
            ("passed", Passed, 60),
        ]
        .into_iter()
        .map(|(id, state, at)| (SharedString::from(id), state, at))
        .collect();
        let ids = |ix: Vec<usize>| {
            ix.into_iter()
                .map(|i| rows[i].0.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(needs_order(&rows, None)),
            ["new", "old", "failed", "ci"]
        );
        assert!(needs_order(&[], None).is_empty());
    }

    #[test]
    fn needs_you_holds_its_rows_while_the_pointer_is_over_the_sidebar() {
        use RowState::*;
        let held: Vec<SharedString> = ["b", "a"].into_iter().map(SharedString::from).collect();
        // Under the pointer: "a" was answered and works, "c" starts to wait,
        // "b" is gone.
        let rows: Vec<(SharedString, RowState, u64)> = [("a", Working, 9), ("c", NeedsYou, 8)]
            .into_iter()
            .map(|(id, state, at)| (SharedString::from(id), state, at))
            .collect();
        // "a" stays where it was and "c" waits in its project for the
        // pointer to leave.
        assert_eq!(needs_order(&rows, Some(&held)), [0]);
        assert_eq!(needs_order(&rows, None), [1]);
    }

    #[test]
    fn only_passed_sessions_fold_and_never_the_pinned_or_the_one_on_screen() {
        use RowState::*;
        assert!(folds_as_done(Passed, false, false));
        assert!(!folds_as_done(Passed, true, false));
        assert!(!folds_as_done(Passed, false, true));
        for state in [Idle, Working, NeedsYou, Failed, CiFailed] {
            assert!(!folds_as_done(state, false, false), "{state:?}");
        }
    }

    #[test]
    fn done_sessions_go_after_the_live_ones_in_the_chosen_order() {
        let done = |id: &str, title: &str, at: u64, tab: bool| Row {
            done: true,
            ..row(id, title, at, tab)
        };
        let rows = || {
            vec![
                done("a", "zebra", 50, true),
                row("b", "mango", 30, false),
                done("c", "Apple", 40, false),
                row("d", "kiwi", 20, true),
            ]
        };
        let sorted = |sort| {
            let mut r = rows();
            sort_rows(&mut r, sort, None);
            placed(&r)
        };
        let (ids, bands) = sorted(Sort::Recent);
        assert_eq!(ids, ["d", "b", "a", "c"]);
        assert_eq!(bands, [Band::Open, Band::Rest, Band::Done, Band::Done]);
        assert_eq!(sorted(Sort::Name).0, ["d", "b", "c", "a"]);
        // While the pointer is over the tree, a session that passes keeps
        // its place instead of folding away from under it.
        let mut r = vec![row("b", "mango", 30, false), row("d", "kiwi", 20, false)];
        sort_rows(&mut r, Sort::Recent, None);
        let held: HeldRows = r
            .iter()
            .enumerate()
            .map(|(place, r)| (r.id.clone(), (place, r.band)))
            .collect();
        let mut r = vec![done("b", "mango", 30, false), row("d", "kiwi", 20, false)];
        sort_rows(&mut r, Sort::Recent, Some(&held));
        assert_eq!(
            placed(&r),
            (vec!["b".into(), "d".into()], vec![Band::Rest; 2])
        );
    }

    #[test]
    fn rows_hold_their_places_while_the_pointer_is_over_the_tree() {
        let mut rows = vec![
            row("a", "zebra", 10, true),
            row("b", "mango", 30, false),
            row("c", "Apple", 20, true),
            row("d", "kiwi", 30, false),
        ];
        sort_rows(&mut rows, Sort::Recent, None);
        let held: HeldRows = rows
            .iter()
            .enumerate()
            .map(|(place, r)| (r.id.clone(), (place, r.band)))
            .collect();
        // Under the pointer: "a" loses its tab, "d" is opened and moves,
        // and a new session starts.
        let now = || {
            vec![
                row("a", "zebra", 10, false),
                row("b", "mango", 30, false),
                row("c", "Apple", 20, true),
                row("d", "kiwi", 99, true),
                row("e", "new", 50, true),
            ]
        };
        let mut rows = now();
        sort_rows(&mut rows, Sort::Recent, Some(&held));
        let (ids, top) = placed(&rows);
        // The new one comes first; the others stay put, rule and all.
        assert_eq!(ids, ["e", "c", "a", "b", "d"]);
        assert_eq!(
            top,
            [Band::Open, Band::Open, Band::Open, Band::Rest, Band::Rest]
        );
        // Once the pointer leaves, the order is the chosen one again.
        let mut rows = now();
        sort_rows(&mut rows, Sort::Recent, None);
        let (ids, top) = placed(&rows);
        assert_eq!(ids, ["d", "e", "c", "b", "a"]);
        assert_eq!(
            top,
            [Band::Open, Band::Open, Band::Open, Band::Rest, Band::Rest]
        );
    }

    #[test]
    fn manual_follows_the_dragged_order_with_new_sessions_first() {
        let placed_at = |id: &str, pi: usize, manual: Option<usize>, tab: bool| Row {
            pi,
            manual,
            ..row(id, id, 0, tab)
        };
        let mut rows = vec![
            placed_at("a", 0, Some(2), true),
            placed_at("b", 0, Some(0), false),
            placed_at("new", 0, None, false),
            placed_at("c", 0, Some(1), true),
        ];
        sort_rows(&mut rows, Sort::Manual, None);
        let (ids, top) = placed(&rows);
        assert_eq!(ids, ["new", "b", "c", "a"]);
        // No rule: open tabs stay where the user put them.
        assert_eq!(top, vec![Band::Rest; 4]);
        // Without project headings, project by project, each in its order.
        let mut rows = vec![
            placed_at("q", 1, Some(0), true),
            placed_at("a", 0, Some(1), true),
            placed_at("p", 1, Some(1), true),
            placed_at("b", 0, Some(0), true),
        ];
        sort_rows(&mut rows, Sort::Manual, None);
        assert_eq!(placed(&rows).0, ["b", "a", "q", "p"]);
    }

    #[test]
    fn pinned_sessions_go_first_in_every_order() {
        let pinned = |id: &str, updated: u64| Row {
            pinned: true,
            ..row(id, id, updated, true)
        };
        let rows = || {
            vec![
                row("a", "a", 40, true),
                pinned("p", 10),
                row("b", "b", 30, false),
                pinned("q", 20),
            ]
        };
        let sorted = |sort| {
            let mut r = rows();
            sort_rows(&mut r, sort, None);
            placed(&r)
        };
        use Band::*;
        // Recent: the pinned ones, then the open tab, then the rest.
        assert_eq!(
            sorted(Sort::Recent),
            (
                vec!["q".into(), "p".into(), "a".into(), "b".into()],
                vec![Pinned, Pinned, Open, Rest]
            )
        );
        assert_eq!(sorted(Sort::Name).0, ["p", "q", "a", "b"]);
        // Manual keeps its order under the pinned ones, with one rule.
        assert_eq!(
            sorted(Sort::Manual),
            (
                vec!["p".into(), "q".into(), "a".into(), "b".into()],
                vec![Pinned, Pinned, Rest, Rest]
            )
        );
    }

    #[test]
    fn a_drag_switches_only_its_project_to_manual() {
        use Sort::*;
        // With project headings: that project only; the order for all stays.
        assert_eq!(order_after_drag(true, Recent, None), (Some(Manual), None));
        assert_eq!(
            order_after_drag(true, Recent, Some(Name)),
            (Some(Manual), None)
        );
        // Already Manual, by its own order or the one for all: nothing changes.
        assert_eq!(
            order_after_drag(true, Recent, Some(Manual)),
            (Some(Manual), None)
        );
        assert_eq!(order_after_drag(true, Manual, None), (None, None));
        // Without them, one list: the order for every project goes to Manual.
        assert_eq!(order_after_drag(false, Name, None), (None, Some(Manual)));
        assert_eq!(order_after_drag(false, Manual, None), (None, None));
    }

    #[test]
    fn a_dropped_session_takes_the_place_of_the_row_it_lands_on() {
        let ids = |list: &[&str]| -> Vec<SharedString> {
            list.iter()
                .map(|s| SharedString::from(s.to_string()))
                .collect()
        };
        let moved = |list: &[&str], from: &str, onto: &str| {
            let mut order = ids(list);
            move_onto(
                &mut order,
                &from.to_string().into(),
                &onto.to_string().into(),
            );
            order
        };
        // Down: the rows between move up one.
        assert_eq!(
            moved(&["a", "b", "c", "d"], "a", "c"),
            ids(&["b", "c", "a", "d"])
        );
        // Up: they move down one.
        assert_eq!(
            moved(&["a", "b", "c", "d"], "d", "b"),
            ids(&["a", "d", "b", "c"])
        );
        // Onto itself, or onto a session not in the list: nothing moves.
        assert_eq!(moved(&["a", "b"], "a", "a"), ids(&["a", "b"]));
        assert_eq!(moved(&["a", "b"], "a", "gone"), ids(&["a", "b"]));
    }

    #[test]
    fn a_state_that_wants_you_or_runs_stays_lit_without_a_tab() {
        use RowState::*;
        for s in [NeedsYou, Failed, CiFailed, Working] {
            assert!(s.stays_lit(), "{s:?}");
        }
        for s in [Idle, Passed] {
            assert!(!s.stays_lit(), "{s:?}");
        }
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
