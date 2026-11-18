//! What GitHub says about the item in the sheet: its properties, the
//! confirm strip, a notification that isn't a pull request, and the body
//! under the tabs. The sheet (`sheet.rs`) puts them together.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::{Confirm, GitHubView, Tab};
use crate::github::pr::{self, CheckState, Pull, WaitingOn};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, PillKind, Type, Typography as _, icon};

/// The label column of the property list.
const KEY_W: f32 = 140.;

/// A pull request's state as an icon and a color.
pub(super) fn pull_look(p: &Pull) -> (&'static str, Hsla, &'static str) {
    if p.merged() {
        ("git-merge", theme::pr_merged().into(), "Merged")
    } else if !p.open() {
        (
            "git-pull-request-closed",
            theme::pr_closed().into(),
            "Closed",
        )
    } else if p.is_draft {
        ("git-pull-request-draft", theme::pr_draft().into(), "Draft")
    } else {
        ("git-pull-request", theme::pr_open().into(), "Open")
    }
}

fn issue_look(i: &pr::Issue) -> (&'static str, Hsla, &'static str) {
    if i.open() {
        ("circle-dot", theme::pr_open().into(), "Open")
    } else {
        ("checkmark-circle02", theme::pr_merged().into(), "Closed")
    }
}

pub(super) fn check_color(s: CheckState) -> Hsla {
    match s {
        CheckState::Passed => theme::success().into(),
        CheckState::Failed => theme::error().into(),
        CheckState::Running => theme::working(),
        CheckState::Skipped => theme::border_strong(),
    }
}

/// A dot before a value.
fn dot(color: Hsla) -> gpui::Div {
    div().size(px(8.)).flex_none().rounded_full().bg(color)
}

/// One property of the item: its icon, its name and its value.
pub(super) struct Prop {
    icon: &'static str,
    label: &'static str,
    value: AnyElement,
    /// Shown when the sheet folds its details to one line.
    key: bool,
}

impl Prop {
    pub(super) fn new(icon: &'static str, label: &'static str, value: impl IntoElement) -> Self {
        Prop {
            icon,
            label,
            value: value.into_any_element(),
            key: false,
        }
    }

    pub(super) fn key(mut self) -> Self {
        self.key = true;
        self
    }
}

/// Key properties side by side on one wrapping line: the sheet's details
/// folded, so a short window keeps its room for the tabs.
pub(super) fn prop_line(props: Vec<Prop>) -> gpui::Div {
    let mut line = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(crate::ui::sp(20.))
        .gap_y(crate::ui::sp(8.));
    for p in props.into_iter().filter(|p| p.key) {
        let first = p.label == "Status";
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .min_w_0()
                .when(!first, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_color(theme::text_subtle())
                            .child(p.label),
                    )
                })
                .child(p.value),
        );
    }
    line
}

/// A property as a row: icon, name, value.
pub(super) fn prop_row(p: Prop) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .min_h(px(24.))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(8.))
                .w(px(KEY_W))
                .h(px(24.))
                .text_color(theme::text_subtle())
                .child(icon(p.icon, 14.).text_color(theme::text_faint()))
                .child(p.label),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h(px(24.))
                .items_center()
                .gap(crate::ui::sp(8.))
                .child(p.value),
        )
}

/// A label in its own color, lightened enough to read on the dark card.
fn label_chip(l: &pr::Label) -> gpui::Div {
    let raw = u32::from_str_radix(l.color.trim_start_matches('#'), 16).unwrap_or(0x8b949e);
    let mut c: Hsla = gpui::rgb(raw).into();
    c.l = c.l.max(0.68);
    c.s = c.s.min(0.75);
    ui::tag(l.name.clone(), c)
}

impl GitHubView {
    /// The strip under the sheet's bar: a write waiting for its second click.
    pub(super) fn render_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let strip = || {
            div()
                .flex()
                .flex_none()
                .flex_wrap()
                .items_center()
                .gap(crate::ui::sp(10.))
                .mx(crate::ui::sp(20.))
                .mb(crate::ui::sp(8.))
                .px(crate::ui::sp(14.))
                .py(crate::ui::sp(10.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
        };
        if let Some(c) = self.confirm {
            let base = match self.github.read(cx).details(&self.item()?) {
                Some(Details::Pull(p)) => p.base_ref_name.clone(),
                _ => String::new(),
            };
            let (question, verb, needs_body) = match c {
                Confirm::Approve => (
                    "Approve this pull request?".to_string(),
                    "Approve".to_string(),
                    true,
                ),
                Confirm::RequestChanges => (
                    "Request changes?".to_string(),
                    "Request changes".to_string(),
                    true,
                ),
                Confirm::Merge(m) => (
                    format!(
                        "{} {} into {base}?",
                        super::merge_label(m),
                        crate::github::pr::short_sha(&self.confirm_head)
                    ),
                    super::merge_label(m).to_string(),
                    false,
                ),
                Confirm::Close => (
                    "Close this issue?".to_string(),
                    "Close issue".to_string(),
                    false,
                ),
                Confirm::Reopen => (
                    "Open this issue again?".to_string(),
                    "Reopen issue".to_string(),
                    false,
                ),
                Confirm::PostDraft => {
                    let (draft, Some(Details::Pull(p))) = (
                        self.draft.as_ref()?,
                        self.github.read(cx).details(&self.item()?),
                    ) else {
                        return None;
                    };
                    let (q, verb) = super::draft_question(
                        draft,
                        pr::draft_is_stale(draft, p),
                        pr::draft_posts_as(draft, p),
                    );
                    (q, verb, false)
                }
            };
            let (question, verb) = self.pilot_question(c).unwrap_or((question, verb));
            let busy = self.busy.is_some();
            return Some(
                strip()
                    .bg(theme::needs_you().opacity(0.06))
                    .child(icon("alert-circle", 14.).text_color(theme::needs_you()))
                    .child(
                        div()
                            .flex_none()
                            .ty(Type::Body)
                            .font_weight(FontWeight::MEDIUM)
                            .child(question),
                    )
                    .child(if needs_body {
                        ui::field()
                            .flex_1()
                            .h(px(30.))
                            .bg(theme::background())
                            .child(self.body_input.clone())
                            .into_any_element()
                    } else {
                        div().flex_1().into_any_element()
                    })
                    .child(
                        ui::key_pill(
                            "gh-confirm-yes",
                            None,
                            if busy { "Sending…".to_string() } else { verb },
                            None,
                            PillKind::Primary,
                            !busy,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.run_confirm(c, cx))),
                    )
                    .child(
                        ui::key_pill("gh-confirm-no", None, "Cancel", None, PillKind::Quiet, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm = None;
                                cx.notify();
                            })),
                    )
                    .children(self.note.as_ref().filter(|n| !n.ok).map(|n| {
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::error())
                            .child(n.text.clone())
                    }))
                    .into_any_element(),
            );
        }
        None
    }

    /// What the last action did, or why it didn't go: over the composer.
    pub(super) fn render_note_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The confirm strip shows its own failure.
        if self.confirm.is_some() {
            return None;
        }
        let strip = || {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(10.))
                .px(crate::ui::sp(6.))
        };
        let note = self.note.as_ref()?;
        let (name, color): (&str, Hsla) = match (note.ok, note.quiet) {
            (true, _) => ("tick02", theme::success().into()),
            (false, true) => ("alert-circle", theme::text_subtle()),
            (false, false) => ("alert-circle", theme::error().into()),
        };
        Some(
            strip()
                .child(icon(name, 13.).text_color(color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .ty(Type::Label)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(if note.ok || note.quiet {
                            theme::text_muted()
                        } else {
                            theme::error().into()
                        })
                        .child(note.text.clone()),
                )
                .child(
                    ui::icon_button("gh-note-close", "cancel01", 12.).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.note = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
        )
    }

    /// A notification about something that is not a pull request or issue.
    pub(super) fn render_notice(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = &self.picked else {
            return div().into_any_element();
        };
        let Some(n) = e.notification.clone() else {
            return div().into_any_element();
        };
        let kind = n.kind();
        let ask = n.clone();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(18.))
            .child(ui::icon_tile(kind.icon(), 44., theme::text_muted()))
            .child(super::mono(
                format!(
                    "{} · {}",
                    kind.noun(),
                    crate::github::reason_label(&n.reason)
                ),
                theme::text_subtle(),
            ))
            .child(div().ty(Type::Title).child(n.subject.title.clone()))
            .child(ui::text(
                Type::Body,
                theme::text_muted(),
                format!(
                    "Updated {} ago in {}.",
                    super::ago(n.updated()),
                    n.repository.full_name
                ),
            ))
            .child(
                div()
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .child(
                        ui::key_pill(
                            "gh-notice-open",
                            Some(("link-square02", None)),
                            "Open on GitHub",
                            Some("O"),
                            PillKind::Primary,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.open_on_github(cx))),
                    )
                    .child(
                        ui::key_pill(
                            "gh-notice-ask",
                            Some(("sparkles", Some(theme::text_subtle()))),
                            "Hand to an agent",
                            None,
                            PillKind::Normal,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ask(
                                crate::github::ask_title(&ask),
                                crate::github::ask_prompt(&ask),
                                cx,
                            );
                        })),
                    )
                    .when(n.unread, |d| {
                        d.child(
                            ui::key_pill(
                                "gh-notice-read",
                                Some(("tick02", None)),
                                "Mark read",
                                None,
                                PillKind::Quiet,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.mark_read(cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The pull request's properties, for one pilot doesn't follow.
    pub(super) fn pull_props(&self, p: &Pull, cx: &mut Context<Self>) -> Vec<Prop> {
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default().to_string();
        let (icon_name, color, status) = pull_look(p);
        let checks = pr::checks_summary(p);
        let blockers = pr::blockers(p);
        let waiting = pr::waiting_on(p, &me);

        let status_v = ui::state_pill(icon_name, status, color);
        let waiting_v = match &waiting {
            WaitingOn::You(why) => {
                let why = if why == "your review is requested" {
                    "review requested"
                } else {
                    why.as_str()
                };
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .min_w_0()
                    .text_color(theme::needs_you())
                    .child(dot(theme::needs_you()))
                    .child(div().truncate().child(format!("You · {why}")))
            }
            WaitingOn::Author => div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .min_w_0()
                .child(self.face(&p.author.login, 20., cx))
                .child(div().truncate().child(p.author.login.clone())),
            other => div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .min_w_0()
                .text_color(theme::text())
                .child(dot(match other {
                    WaitingOn::Nobody => theme::border_strong(),
                    _ => theme::link().into(),
                }))
                .when_some(
                    match other {
                        WaitingOn::Reviewers(who) if who.len() == 1 => Some(who[0].clone()),
                        _ => None,
                    },
                    |d, who| d.child(self.face(&who, 20., cx)),
                )
                .child(div().truncate().child(match other {
                    WaitingOn::Reviewers(who) if who.is_empty() => "A reviewer".into(),
                    WaitingOn::Reviewers(who) => who.join(", "),
                    WaitingOn::Checks => "Checks".into(),
                    _ => "Nobody".into(),
                })),
        };
        let author_v = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .min_w_0()
            .child(self.face(&p.author.login, 20., cx))
            .child(div().truncate().child(p.author.login.clone()));
        let checks_v = if checks.total() == 0 {
            div().text_color(theme::text_subtle()).child("No checks")
        } else {
            let colors: Vec<Hsla> = checks
                .checks
                .iter()
                .take(12)
                .map(|c| check_color(c.state))
                .collect();
            let what = if checks.failed > 0 {
                format!(
                    "{} failing",
                    checks
                        .names(CheckState::Failed)
                        .first()
                        .cloned()
                        .unwrap_or_default()
                )
            } else if checks.running > 0 {
                format!(
                    "{} running",
                    checks
                        .names(CheckState::Running)
                        .first()
                        .cloned()
                        .unwrap_or_default()
                )
            } else {
                String::new()
            };
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .min_w_0()
                .child(ui::mini_squares(&colors, 9.))
                .child(super::mono(
                    format!("{}/{}", checks.passed, checks.total() - checks.skipped),
                    theme::text(),
                ))
                .child(
                    div()
                        .truncate()
                        .text_color(theme::text_subtle())
                        .child(what),
                )
        };
        let review_text = match p.review_decision.as_str() {
            "APPROVED" => "Approved".to_string(),
            "CHANGES_REQUESTED" => "Changes requested".to_string(),
            "REVIEW_REQUIRED" => "Approval required".into(),
            _ => "No review required".into(),
        };
        let review_text = if pr::my_review_dismissed(p, &me) {
            format!("{review_text} · yours was dismissed")
        } else {
            review_text
        };
        let reviewers = pr::reviewers(p);
        let reviewers_v = if reviewers.is_empty() {
            div()
                .text_color(theme::text_subtle())
                .child(review_text.clone())
        } else {
            let mut list = div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .w_full()
                .py(crate::ui::sp(2.));
            for (login, state) in &reviewers {
                let (name, c) = reviewer_look(*state);
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .min_w_0()
                        .child(self.face(login, 20., cx))
                        .child(div().flex_1().min_w_0().truncate().child(login.clone()))
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(crate::ui::sp(4.))
                                .ty(Type::Caption)
                                .text_color(c)
                                .child(icon(name, 12.).text_color(c))
                                .child(reviewer_word(*state)),
                        ),
                );
            }
            list
        };
        let merge_v = if blockers.is_empty() && p.open() {
            div().text_color(theme::success()).child("Ready to merge")
        } else if !p.open() {
            div().text_color(theme::text_muted()).child(status)
        } else {
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme::text_muted())
                .child(format!(
                    "Blocked · {}",
                    lower_first(&pr::blockers_text(&blockers))
                ))
        };
        let changes_v = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(super::mono(
                format!("+{}", p.additions),
                theme::diff_add().into(),
            ))
            .child(super::mono(
                format!("−{}", p.deletions),
                theme::diff_del().into(),
            ))
            .child(super::mono(
                format!("{} files", p.changed_files),
                theme::text_subtle(),
            ));
        let commit_v = match p.commits.last() {
            Some(c) => div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .child(super::mono(c.short_oid().to_string(), theme::text()))
                .child(super::mono(
                    format!("· {} ago", super::ago_iso(&c.committed_date)),
                    theme::text_subtle(),
                )),
            None => div().text_color(theme::text_subtle()).child("—"),
        };
        // A merged or closed one waits on nobody and has nothing left to merge.
        let open = p.open();
        let mut props = vec![Prop::new(icon_name, "Status", status_v).key()];
        if open {
            props.push(Prop::new("clock01", "Waiting on", waiting_v));
        }
        props.push(Prop::new("user", "Author", author_v));
        props.push(Prop::new("activity01", "Checks", checks_v).key());
        if reviewers.is_empty() {
            props.push(Prop::new("tick02", "Review", reviewers_v).key());
        } else {
            props.push(Prop::new("tick02", "Reviewers", reviewers_v));
            props.push(
                Prop::new(
                    "checkmark-circle02",
                    "Review",
                    div().truncate().child(review_text),
                )
                .key(),
            );
        }
        if open {
            props.push(Prop::new("git-merge", "Merge", merge_v).key());
        }
        props.push(Prop::new("file01", "Changes", changes_v));
        props.push(Prop::new("git-commit", "Commit", commit_v));
        if !p.labels.is_empty() {
            props.push(Prop::new(
                "tag01",
                "Labels",
                div()
                    .flex()
                    .flex_wrap()
                    .gap(crate::ui::sp(4.))
                    .children(p.labels.iter().map(label_chip)),
            ));
        }
        props
    }

    pub(super) fn issue_props(&self, i: &pr::Issue, cx: &mut Context<Self>) -> Vec<Prop> {
        let (icon_name, color, status) = issue_look(i);
        let mut assignees = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(8.))
            .min_w_0();
        if i.assignees.is_empty() {
            assignees = assignees.text_color(theme::text_subtle()).child("Nobody");
        }
        for a in i.assignees.iter().take(3) {
            assignees = assignees.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(self.face(&a.login, 20., cx))
                    .child(a.login.clone()),
            );
        }
        let mut props = vec![
            Prop::new(
                icon_name,
                "Status",
                ui::state_pill(icon_name, status, color),
            )
            .key(),
            Prop::new(
                "user",
                "Author",
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .min_w_0()
                    .child(self.face(&i.author.login, 20., cx))
                    .child(div().truncate().child(i.author.login.clone())),
            ),
            Prop::new("user", "Assignees", assignees).key(),
            Prop::new(
                "comment01",
                "Comments",
                super::mono(i.comments.len().to_string(), theme::text()),
            ),
            Prop::new(
                "calendar03",
                "Opened",
                super::mono(
                    format!("{} ago", super::ago_iso(&i.created_at)),
                    theme::text(),
                ),
            ),
        ];
        if !i.labels.is_empty() {
            props.push(Prop::new(
                "tag01",
                "Labels",
                div()
                    .flex()
                    .flex_wrap()
                    .gap(crate::ui::sp(4.))
                    .children(i.labels.iter().map(label_chip)),
            ));
        }
        props
    }

    /// The pull request's own actions: the agent's review and the merge
    /// button, blocked with its reason.
    pub(super) fn pull_actions(&self, p: &Pull, cx: &mut Context<Self>) -> gpui::Div {
        let (_, _, status) = pull_look(p);
        let blockers = pr::blockers(p);
        let methods = self
            .merge_methods
            .get(&self.item().map(|i| i.repo).unwrap_or_default())
            .cloned()
            .flatten();
        let method = self.merge_method();
        let no_rights = methods.as_ref().is_some_and(Vec::is_empty);
        let why_not = if !p.open() {
            Some(status.to_string())
        } else if no_rights {
            Some("You can't merge in this repository".to_string())
        } else if !blockers.is_empty() {
            Some(pr::blockers_text(&blockers))
        } else {
            None
        };
        let can_merge = why_not.is_none() && method.is_some() && self.busy.is_none();
        let label = method.map_or("Merge", super::merge_label);
        let merge_btn = ui::key_pill(
            "gh-merge",
            Some(("git-merge", None)),
            label,
            None,
            PillKind::Normal,
            can_merge,
        )
        .when(can_merge, |b| {
            b.on_click(cx.listener(|this, _, _, cx| this.ask_merge(cx)))
        })
        .when_some(why_not.clone(), |b, why| b.tooltip(ui::tip_text(why)));
        let agent = ui::key_pill(
            "gh-agent",
            Some(("sparkles", Some(theme::text_subtle()))),
            "Review with agent",
            Some("R"),
            PillKind::Normal,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.review_with_agent(cx)));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(agent)
            .when(p.open(), |d| d.child(merge_btn))
    }

    /// An issue's own actions: an agent to work on it, and closing it or
    /// opening it again.
    pub(super) fn issue_actions(&self, i: &pr::Issue, cx: &mut Context<Self>) -> gpui::Div {
        let free = self.busy.is_none();
        let agent = ui::key_pill(
            "gh-issue-agent",
            Some(("sparkles", Some(theme::text_subtle()))),
            "Work on it with agent",
            None,
            PillKind::Normal,
            true,
        )
        .tooltip(ui::tip("Start an agent on this issue in the open project"))
        .on_click(cx.listener(|this, _, _, cx| this.work_on_issue(cx)));
        let (label, glyph, ask) = if i.open() {
            ("Close issue", "checkmark-circle01", Confirm::Close)
        } else {
            ("Reopen issue", "circle-dot", Confirm::Reopen)
        };
        let state = ui::key_pill(
            "gh-issue-state",
            Some((glyph, Some(theme::text_subtle()))),
            label,
            None,
            PillKind::Normal,
            free,
        )
        .when(free, |b| {
            b.on_click(cx.listener(move |this, _, _, cx| this.ask_confirm(ask, cx)))
        });
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(8.))
            .when(i.open(), |d| d.child(agent))
            .child(state)
    }

    /// What the tab picked shows, in the body under the tabs.
    pub(super) fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_timeline(cx);
        self.sync_files();
        let Some(item) = self.item() else {
            return div().into_any_element();
        };
        let board = self.board_pr(cx);
        if self.tab == Tab::Pilot
            && let Some(b) = &board
        {
            return div()
                .id("gh-body")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.body_scroll)
                .child(self.render_pilot_steps(b))
                .into_any_element();
        }
        let g = self.github.read(cx);
        let Some(details) = g.details(&item) else {
            return ui::loading("gh-body-loading", "Reading the pull request…").into_any_element();
        };
        let tab = if matches!(details, Details::Issue(_)) {
            Tab::Timeline
        } else {
            self.tab
        };
        let content = match (tab, details) {
            (Tab::Files, Details::Pull(p)) => self.render_files(p, cx),
            (Tab::Checks, Details::Pull(p)) => self.render_checks(p, cx),
            (Tab::Commits, Details::Pull(p)) => {
                let me = g.me().unwrap_or_default().to_string();
                self.render_commits(p, &me, cx)
            }
            _ => gpui::list(
                self.timeline_list.clone(),
                cx.processor(Self::render_timeline_row),
            )
            .size_full()
            .into_any_element(),
        };
        // The timeline and the files are lists that scroll themselves.
        let scrolls = matches!(
            (tab, details),
            (Tab::Checks | Tab::Commits, Details::Pull(_))
        );
        div()
            .id("gh-body")
            .size_full()
            .when(scrolls, |d| {
                d.overflow_y_scroll().track_scroll(&self.body_scroll)
            })
            .child(content)
            .into_any_element()
    }
}

fn reviewer_look(s: pr::ReviewerState) -> (&'static str, Hsla) {
    match s {
        pr::ReviewerState::Approved => ("tick02", theme::success().into()),
        pr::ReviewerState::ChangesRequested => ("alert-circle", theme::error().into()),
        pr::ReviewerState::Commented => ("comment01", theme::text_muted()),
        pr::ReviewerState::Requested => ("clock01", theme::needs_you()),
    }
}

fn reviewer_word(s: pr::ReviewerState) -> &'static str {
    match s {
        pr::ReviewerState::Approved => "Approved",
        pr::ReviewerState::ChangesRequested => "Changes",
        pr::ReviewerState::Commented => "Commented",
        pr::ReviewerState::Requested => "Pending",
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_first_lowers_only_the_first() {
        assert_eq!(
            lower_first("Needs an approving review"),
            "needs an approving review"
        );
    }
}
