//! The sidebar's tree of sessions (`tab_layout::SessionsIn::Sidebar`): every
//! session of every project, under its project, each with a mark for its
//! state. It takes the place of the projects and environments list.

use super::*;
use crate::tab_layout::{LongTitles, Quiet, StateMark, TabLayout};

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

/// One session in the tree.
struct Row {
    ei: usize,
    id: SharedString,
    title: SharedString,
    state: RowState,
    pr: Option<u64>,
}

impl Shell {
    /// The sessions of project `pi` the tree lists, in sidebar order:
    /// archived ones only while archived sessions are shown.
    fn tree_rows(&self, pi: usize, cx: &App) -> Vec<Row> {
        let Some(project) = self.projects.get(pi) else {
            return Vec::new();
        };
        let repo = self.ci.read(cx).repo(&project.path);
        let on_screen: Vec<SharedString> = self
            .sessions_on_screen()
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        let mut rows = Vec::new();
        for (ei, env) in project.environments.iter().enumerate() {
            // The checks of the main checkout's branch are not any one session's.
            let own = (!env.is_checkout).then_some(repo).flatten();
            let ci = own
                .and_then(|r| r.branch_state(&env.branch))
                .map(|(s, _)| s);
            let pr = own.and_then(|r| r.pr_of(&env.branch)).map(|p| p.number);
            for s in &env.sessions {
                // An archived session still on screen keeps its row.
                if s.settled && !self.show_settled && !on_screen.contains(&s.id) {
                    continue;
                }
                rows.push(Row {
                    ei,
                    id: s.id.clone(),
                    title: s.title.clone(),
                    state: RowState::of(s.shown_status(), ci),
                    pr,
                });
            }
        }
        rows
    }

    pub(super) fn render_session_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let layout = crate::tab_layout::current();
        let open_id = self.active_session_id();
        let mut list = div()
            .id("session-tree")
            .role(gpui::Role::Tree)
            .aria_label("Sessions")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.));
        list = list.child(self.render_tree_heading(cx));
        for (pi, project) in self.projects.iter().enumerate() {
            let rows = self.tree_rows(pi, cx);
            let busy = rows.iter().any(|r| r.state.busy());
            let here = pi == self.active_project;
            let user = self.tree_open.get(&project.path).copied();
            let open = if layout.group {
                match project_open(layout.quiet, busy, here, user) {
                    Some(open) => open,
                    None => continue,
                }
            } else {
                true
            };
            if layout.group {
                let urgent = rows
                    .iter()
                    .map(|r| r.state)
                    .filter(|s| *s != RowState::Idle)
                    .min_by_key(|s| s.rank());
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
                    list = list.child(self.render_tree_row(pi, row, selected, &layout, cx));
                }
            }
        }
        // Rows picked with Ctrl or Shift get the bar that acts on them.
        list.children(self.render_pick_bar(cx)).into_any_element()
    }

    /// "Sessions", and the + that starts one in the project on screen.
    fn render_tree_heading(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .h(px(28.))
            .pl(crate::ui::sp(8.))
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(div().flex_1().min_w_0().child("Sessions"))
            .child(
                icon_button("tree-new-session", "add01", 14., "New session (Ctrl+T)")
                    .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx))),
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
        let label = format!(
            "{}, {count} session{}",
            project.name,
            if count == 1 { "" } else { "s" }
        );
        div()
            .id(("tree-project", pi))
            .map(|d| ui::labelled(d, gpui::Role::TreeItem, label))
            .aria_expanded(open)
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
                    .child(count.to_string()),
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
        pi: usize,
        row: Row,
        selected: bool,
        layout: &TabLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Row {
            ei,
            id,
            title,
            state,
            pr,
        } = row;
        let env_path = self.projects[pi].environments[ei].path.clone();
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
        div()
            .id(SharedString::from(format!("tree-session-{id}")))
            .map(|d| ui::labelled(d, gpui::Role::TreeItem, label))
            .aria_selected(selected)
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
                    .child(title),
            )
            .children(end.map(|text| {
                div()
                    .flex_none()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(state.word_color())
                    .child(text)
            }))
            .when(self.is_picked(&env_path), |d| {
                d.bg(theme::selection_strong())
            })
            // Ctrl (Cmd) or Shift picks the session's worktree, as in the
            // projects list; a plain click opens the session.
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                let m = ev.modifiers();
                if m.secondary() || m.shift {
                    if let Some(path) = this
                        .projects
                        .get(pi)
                        .and_then(|p| p.environments.get(ei))
                        .map(|e| e.path.clone())
                    {
                        this.toggle_pick(&path, cx);
                    }
                    return;
                }
                this.drop_pick();
                this.open_session_at(pi, ei, opens.clone(), window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu = Some(ContextMenu {
                        target: MenuTarget::Env(pi, ei),
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
