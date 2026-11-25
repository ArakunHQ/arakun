//! The Linear tab's list: the assigned issues by group, the search box over
//! them, and the notes under them.

use gpui::Role;

use super::*;
use crate::linear::issues::{self, Assigned, Group, Issue};

/// One line of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Heading(Group),
    /// An index into the issues.
    Issue(usize),
}

/// The issues that match `needle`, under the heading of their group.
pub(super) fn rows(issues: &[Issue], needle: &str) -> Vec<Row> {
    let mut out = Vec::new();
    for g in Group::ALL {
        let mut found = issues
            .iter()
            .enumerate()
            .filter(|(_, i)| i.group == g && issues::matches(i, needle))
            .map(|(n, _)| Row::Issue(n))
            .peekable();
        if found.peek().is_some() {
            out.push(Row::Heading(g));
            out.extend(found);
        }
    }
    out
}

/// What the list says under its rows, if anything.
pub(super) fn note(a: &Assigned, needle: &str) -> Option<&'static str> {
    if a.issues.is_empty() {
        return Some("No open issues are assigned to you.");
    }
    if !a.issues.iter().any(|i| issues::matches(i, needle)) {
        return Some("No issue matches.");
    }
    // 250 is `issues::LIMIT`.
    a.more
        .then_some("Showing the first 250. Search in Linear for the rest.")
}

/// The issue `by` rows on from `picked` among those `shown`, stopping at
/// either end. Nothing picked, or picked but filtered out, starts at the top.
pub(super) fn step(
    issues: &[Issue],
    shown: &[Row],
    picked: Option<&str>,
    by: isize,
) -> Option<String> {
    let ids: Vec<&str> = shown
        .iter()
        .filter_map(|r| match r {
            Row::Issue(i) => issues.get(*i).map(|x| x.id.as_str()),
            Row::Heading(_) => None,
        })
        .collect();
    let last = ids.len().checked_sub(1)?;
    let next = match picked.and_then(|p| ids.iter().position(|id| *id == p)) {
        Some(at) => at.saturating_add_signed(by).min(last),
        None => 0,
    };
    Some(ids[next].to_string())
}

pub(super) fn group_icon(g: Group) -> &'static str {
    match g {
        Group::Started => "cs-progress",
        Group::Todo => "cs-todo",
        Group::Backlog => "cs-backlog",
    }
}

/// Linear's priority numbers: 0 none, 1 urgent, then high to low.
fn priority_name(p: u8) -> &'static str {
    match p {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "No priority",
    }
}

/// Linear's own marks: a square for urgent, three bars filled by
/// priority, dashes for none.
fn priority_mark(p: u8) -> AnyElement {
    let mark = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(14.));
    match p {
        1 => mark
            .child(icon("cs-urgent", 14.).text_color(theme::attention()))
            .into_any_element(),
        2..=4 => {
            let lit = 5 - p as usize;
            mark.child(div().flex().items_end().gap(crate::ui::sp(1.5)).children(
                [5., 8., 11.].iter().enumerate().map(|(i, h)| {
                    div()
                        .w(px(3.))
                        .h(px(*h))
                        .rounded(crate::ui::rad(1.))
                        .bg(if i < lit {
                            theme::text_muted()
                        } else {
                            theme::border_strong()
                        })
                }),
            ))
            .into_any_element()
        }
        _ => mark
            .child(icon("cs-no-priority", 14.).text_color(theme::text_faint()))
            .into_any_element(),
    }
}

impl LinearView {
    pub(super) fn needle(&self, cx: &gpui::App) -> String {
        self.search.read(cx).text().trim().to_string()
    }

    /// Moves the pick through the issues shown and keeps it in sight.
    pub(super) fn step_pick(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(a) = self.assigned.data.as_ref() else {
            return;
        };
        let shown = rows(&a.issues, &self.needle(cx));
        self.picked = step(&a.issues, &shown, self.picked.as_deref(), by);
        let at = shown.iter().position(|r| match r {
            Row::Issue(i) => a.issues.get(*i).map(|x| &x.id) == self.picked.as_ref(),
            Row::Heading(_) => false,
        });
        if let Some(at) = at {
            self.list_scroll.scroll_to_item(at);
        }
        cx.notify();
    }

    /// What went wrong with the last read, over the list it leaves showing.
    fn render_error(&self, e: &ApiError, cx: &mut Context<Self>) -> AnyElement {
        // A revoked key is fixed under Accounts, which the header has.
        let action = match e {
            ApiError::Failed(_) => Some(
                ui::button("lin-err-retry", "Try again", ButtonKind::Outline, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.asked_at = 0;
                        this.read(true, cx);
                    }),
                ),
            ),
            ApiError::SignedOut | ApiError::Limited { .. } => None,
        };
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .child(
                ui::text(
                    Type::Body,
                    theme::error().into(),
                    crate::redact::text(&error_line(e)),
                )
                .flex_1()
                .min_w_0(),
            )
            .children(action)
            .into_any_element()
    }

    fn render_row(&self, n: usize, issue: &Issue, cx: &mut Context<Self>) -> AnyElement {
        let picked = self.picked.as_deref() == Some(issue.id.as_str());
        let label = format!(
            "{} {}, {}, {}",
            issue.identifier,
            issue.title,
            issue.team,
            priority_name(issue.priority)
        );
        let id = issue.id.clone();
        ui::labelled(div().id(("lin-row", n)), Role::Button, label)
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .h(px(ROW_H))
            .px(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .cursor_pointer()
            .when(picked, |d| d.bg(theme::fill_selected()))
            .hover(|d| d.bg(theme::fill_hover()))
            .child(priority_mark(issue.priority))
            .child(
                ui::text(Type::Mono, theme::text_subtle(), issue.identifier.clone())
                    .flex_none()
                    .w(px(76.))
                    .truncate(),
            )
            .child(
                ui::text(Type::Body, theme::text(), issue.title.clone())
                    .flex_1()
                    .min_w_0()
                    .truncate(),
            )
            .child(
                ui::text(Type::Caption, theme::text_subtle(), issue.team.clone())
                    .flex_none()
                    .max_w(px(160.))
                    .truncate(),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_issue(id.clone(), window, cx)),
            )
            .into_any_element()
    }

    fn render_heading(&self, g: Group, count: usize) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(10.))
            .pt(crate::ui::sp(16.))
            .pb(crate::ui::sp(6.))
            .child(icon(group_icon(g), 14.).text_color(theme::text_muted()))
            .child(ui::text(Type::Label, theme::text(), g.label()))
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                count.to_string(),
            ))
            .into_any_element()
    }

    /// The main page: the search box, the error if the last read failed,
    /// and the list.
    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let error = self
            .assigned
            .error
            .as_ref()
            .map(|e| self.render_error(e, cx));
        let page = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .px(crate::ui::sp(ui::PAGE_PAD));
        let Some(a) = self.assigned.data.as_ref() else {
            // Nothing read yet in this workspace.
            let waiting = (error.is_none() || self.assigned.loading())
                .then(|| ui::loading("Reading your issues"));
            return page.children(error).children(waiting).into_any_element();
        };
        let needle = self.needle(cx);
        let shown = rows(&a.issues, &needle);
        let mut list: Vec<AnyElement> = Vec::with_capacity(shown.len() + 1);
        for r in &shown {
            list.push(match *r {
                Row::Heading(g) => {
                    let count = shown
                        .iter()
                        .filter(|r| matches!(r, Row::Issue(i) if a.issues[*i].group == g))
                        .count();
                    self.render_heading(g, count)
                }
                Row::Issue(i) => self.render_row(i, &a.issues[i], cx),
            });
        }
        if let Some(n) = note(a, &needle) {
            list.push(
                ui::text(Type::Body, theme::text_subtle(), n)
                    .px(crate::ui::sp(10.))
                    .py(crate::ui::sp(16.))
                    .into_any_element(),
            );
        }
        let search = ui::field()
            .w_full()
            .max_w(px(420.))
            .child(icon("search01", 14.).text_color(theme::text_subtle()))
            .child(self.search.clone());
        page.child(div().flex_none().pb(crate::ui::sp(4.)).child(search))
            .children(error)
            .child(
                div()
                    .id("lin-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(px(1320.))
                    .overflow_y_scroll()
                    .track_scroll(&self.list_scroll)
                    .pb(crate::ui::sp(32.))
                    .children(list),
            )
            .into_any_element()
    }
}

/// A row's height: room for a 13 px title and its padding.
const ROW_H: f32 = 36.;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::issues::{Assigned, Group, Issue};

    fn issue_titled(id: &str, group: Group, title: &str) -> Issue {
        Issue {
            id: id.into(),
            identifier: format!("ENG-{id}"),
            title: title.into(),
            url: format!("https://linear.app/acme/issue/ENG-{id}"),
            priority: 2,
            state: "S".into(),
            group,
            team: "Eng".into(),
            team_id: "t1".into(),
            project: None,
            project_id: None,
            branch: String::new(),
            updated_at: 1,
        }
    }

    fn issue(id: &str, group: Group) -> Issue {
        issue_titled(id, group, "T")
    }

    fn assigned_of(n: usize, more: bool) -> Assigned {
        Assigned {
            issues: (0..n).map(|i| issue(&i.to_string(), Group::Todo)).collect(),
            more,
        }
    }

    #[test]
    fn rows_are_grouped_with_headings_and_empty_groups_are_left_out() {
        let rows = rows(
            &[issue("1", Group::Started), issue("2", Group::Backlog)],
            "",
        );
        assert_eq!(
            rows,
            [
                Row::Heading(Group::Started),
                Row::Issue(0),
                Row::Heading(Group::Backlog),
                Row::Issue(1)
            ]
        );
    }

    #[test]
    fn search_filters_rows_and_drops_their_headings() {
        let rows = rows(
            &[
                issue_titled("1", Group::Started, "Tab name"),
                issue_titled("2", Group::Backlog, "Pricing"),
            ],
            "pric",
        );
        assert_eq!(rows, [Row::Heading(Group::Backlog), Row::Issue(1)]);
    }

    #[test]
    fn the_notes_under_the_list() {
        assert_eq!(
            note(
                &Assigned {
                    issues: vec![],
                    more: false
                },
                ""
            ),
            Some("No open issues are assigned to you.")
        );
        assert_eq!(
            note(&assigned_of(3, true), ""),
            Some("Showing the first 250. Search in Linear for the rest.")
        );
        assert_eq!(
            note(&assigned_of(3, false), "zzz"),
            Some("No issue matches.")
        );
        assert_eq!(note(&assigned_of(3, false), ""), None);
    }

    #[test]
    fn the_arrow_keys_step_through_the_issues_shown() {
        let shown = rows(
            &[
                issue("a", Group::Started),
                issue("b", Group::Todo),
                issue("c", Group::Backlog),
            ],
            "",
        );
        let issues = [
            issue("a", Group::Started),
            issue("b", Group::Todo),
            issue("c", Group::Backlog),
        ];
        assert_eq!(step(&issues, &shown, None, 1), Some("a".to_string()));
        assert_eq!(step(&issues, &shown, Some("a"), 1), Some("b".to_string()));
        assert_eq!(step(&issues, &shown, Some("c"), 1), Some("c".to_string())); // stays on the last
        assert_eq!(step(&issues, &shown, Some("a"), -1), Some("a".to_string()));
        assert_eq!(
            step(&issues, &shown, Some("gone"), 1),
            Some("a".to_string())
        );
        assert_eq!(step(&issues, &[], Some("a"), 1), None);
    }
}
