//! The Active list in the rail: every project's sessions that wait on the
//! user, then those that run, whichever project is selected. The project
//! list below it holds the rest, so nothing shows twice.

use super::rail::status_dot;
use super::*;

const ROW_H: f32 = 28.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Group {
    /// Waiting for an answer, or failed.
    NeedsYou,
    Running,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::NeedsYou => "Needs you",
            Group::Running => "Running",
        }
    }
}

/// Whether a session shows in the Active list, and so not in the project's.
pub(super) fn is_active(status: Status) -> bool {
    status != Status::Idle
}

/// Sorts sessions, given as `(status, last activity)`, into the groups that
/// have any, each newest first. Waiting comes before failed in Needs you.
/// Idle ones are left out. Returns indices into `rows`.
pub(super) fn arrange(rows: &[(Status, u64)]) -> Vec<(Group, Vec<usize>)> {
    let pick = |want: &[Status]| {
        let mut ix: Vec<usize> = (0..rows.len())
            .filter(|&i| want.contains(&rows[i].0))
            .collect();
        let rank = |s: Status| want.iter().position(|w| *w == s);
        ix.sort_by(|&a, &b| {
            rank(rows[a].0)
                .cmp(&rank(rows[b].0))
                .then(rows[b].1.cmp(&rows[a].1))
        });
        ix
    };
    let mut out = vec![
        (Group::NeedsYou, pick(&[Status::NeedsInput, Status::Error])),
        (Group::Running, pick(&[Status::Working])),
    ];
    out.retain(|(_, ix)| !ix.is_empty());
    out
}

struct Row<'a> {
    project: usize,
    env: usize,
    session: &'a model::Session,
}

impl Shell {
    /// Whether a session waits or runs, so the Active list has rows.
    pub(super) fn any_active(&self) -> bool {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .any(|s| is_active(s.shown_status()))
    }

    /// Nothing while no session waits or runs.
    pub(super) fn render_active_block(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mut rows = Vec::new();
        for (pi, p) in self.projects.iter().enumerate() {
            for (ei, e) in p.environments.iter().enumerate() {
                for session in &e.sessions {
                    rows.push(Row {
                        project: pi,
                        env: ei,
                        session,
                    });
                }
            }
        }
        let keys: Vec<_> = rows
            .iter()
            .map(|r| (r.session.shown_status(), r.session.updated))
            .collect();
        let groups = arrange(&keys);
        if groups.is_empty() {
            return None;
        }
        let open = self.active_session_id();
        let open_env = self
            .projects
            .get(self.active_project)
            .map(|p| (self.active_project, p.active_env));
        let mut list = div()
            .id("rail-active")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(2.));
        for (group, ix) in groups {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(8.))
                    .pt(crate::ui::sp(6.))
                    .pb(crate::ui::sp(2.))
                    .text_size(px(ui::TEXT_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if group == Group::NeedsYou {
                        theme::needs_you()
                    } else {
                        theme::text_faint()
                    })
                    .child(group.title().to_uppercase())
                    .child(ix.len().to_string()),
            );
            for i in ix {
                let row = &rows[i];
                let current = open_env == Some((row.project, row.env))
                    && open.as_ref() == Some(&row.session.id);
                list = list.child(self.render_active_row(row, current, cx));
            }
        }
        if self.pick_bar_under_active() {
            list = list.children(self.render_pick_bar(cx));
        }
        Some(list.into_any_element())
    }

    fn render_active_row(&self, row: &Row, current: bool, cx: &mut Context<Self>) -> AnyElement {
        let project = &self.projects[row.project];
        let env = &project.environments[row.env];
        let s = row.session;
        let needs_you = s.shown_status() == Status::NeedsInput;
        let title: SharedString = if s.title.is_empty() {
            "New session".into()
        } else {
            s.title.clone()
        };
        let (pi, ei, id) = (row.project, row.env, s.id.clone());
        let picked = self.is_picked(&env.path);
        let path = env.path.clone();
        div()
            .id(SharedString::from(format!("active-{pi}-{ei}-{}", s.id)))
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ROW_H))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_MD))
            .text_color(if current || needs_you {
                theme::content()
            } else {
                theme::text_muted()
            })
            .when(current && !picked, |d| d.bg(theme::selection()))
            .when(picked, |d| {
                d.bg(theme::selection_strong()).text_color(theme::content())
            })
            .when(!current && !picked, |d| {
                d.hover(|st| st.bg(theme::hover()).text_color(theme::content()))
            })
            .tooltip(ui::tip_text(format!(
                "{}/{} · {}",
                project.name,
                env.name,
                model::age(s.updated)
            )))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(14.))
                    .when(picked, |d| {
                        d.child(icon("tick02", 14.).text_color(theme::content()))
                    })
                    .when(!picked, |d| d.child(status_dot(s.shown_status()))),
            )
            .child(div().flex_1().min_w_0().truncate().child(title))
            .child(
                div()
                    .flex_none()
                    .max_w(px(88.))
                    .truncate()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(project.name.clone()),
            )
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                // Picks its worktree for the bar's actions, as in the list below.
                let m = ev.modifiers();
                if m.secondary() || m.shift {
                    this.toggle_pick(&path, cx);
                } else {
                    this.drop_pick();
                    this.open_session_at(pi, ei, id.clone(), window, cx)
                }
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_waits_comes_first_then_what_runs() {
        use Status::*;
        let rows = [
            (Working, 10),
            (Idle, 50),
            (Error, 90),
            (NeedsInput, 20),
            (Working, 30),
            (NeedsInput, 40),
            (Idle, 70),
        ];
        assert_eq!(
            arrange(&rows),
            vec![
                // Waiting before failed, each newest first.
                (Group::NeedsYou, vec![5, 3, 2]),
                (Group::Running, vec![4, 0]),
            ]
        );
    }

    #[test]
    fn idle_sessions_are_left_to_the_project_list() {
        use Status::*;
        assert!(arrange(&[]).is_empty());
        assert!(arrange(&[(Idle, 1), (Idle, 2)]).is_empty());
        assert_eq!(
            arrange(&[(Idle, 1), (Working, 2)]),
            vec![(Group::Running, vec![1])]
        );
        assert!(!is_active(Idle));
        assert!([Working, NeedsInput, Error].into_iter().all(is_active));
    }
}
