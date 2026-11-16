//! The right card: the pull request or issue picked in the queue. Its top bar
//! with the actions, the left column with what it is and where it stands, and
//! the tabs (timeline, files, checks, commits) over the command bar.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Focusable as _, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::{Confirm, GitHubView, Tab};
use crate::github::pr::{self, CheckState, ItemKind, Pull, WaitingOn};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, PillKind, Type, Typography as _, icon};

/// The left column's width inside the card.
pub(super) const SIDE_W: f32 = 332.;
/// The label column of the property list.
const KEY_W: f32 = 96.;

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
        CheckState::Running => theme::accent(),
        CheckState::Skipped => theme::border_strong(),
    }
}

/// A dot before a value.
fn dot(color: Hsla) -> gpui::Div {
    div().size(px(8.)).flex_none().rounded_full().bg(color)
}

/// One property of the item: its icon, its name and its value.
struct Prop {
    icon: &'static str,
    label: &'static str,
    value: AnyElement,
    /// Takes two cells of the grid: a sentence, not a word.
    wide: bool,
    /// One of the few the folded head shows.
    key: bool,
}

impl Prop {
    fn new(icon: &'static str, label: &'static str, value: impl IntoElement) -> Self {
        Prop {
            icon,
            label,
            value: value.into_any_element(),
            wide: false,
            key: false,
        }
    }

    fn key(mut self) -> Self {
        self.key = true;
        self
    }

    fn wide(mut self) -> Self {
        self.wide = true;
        self
    }
}

/// A grid cell's width, when the properties wrap above the tabs.
const CELL_W: f32 = 188.;
const CELL_GAP: f32 = 20.;

/// A property as a row of the side column: icon, name, value.
fn prop_row(p: Prop) -> gpui::Div {
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

/// A property as a cell of the grid above the tabs: its name small over it.
fn prop_cell(p: Prop) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(4.))
        .flex_none()
        .w(px(if p.wide {
            2. * CELL_W + CELL_GAP
        } else {
            CELL_W
        }))
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child(icon(p.icon, 12.).text_color(theme::text_faint()))
                .child(p.label),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .min_w_0()
                .min_h(px(22.))
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
    pub(super) fn render_item(&self, lay: super::Layout, cx: &mut Context<Self>) -> AnyElement {
        let card = ui::surface_card().flex_1().min_w_0();
        let Some(entry) = self.picked.clone() else {
            return card
                .child(
                    ui::empty_state(
                        "git-pull-request",
                        "Nothing picked",
                        "Pick a pull request or an issue in the queue. J and K move through it.",
                        None,
                    )
                    .flex_1()
                    .justify_center(),
                )
                .into_any_element();
        };
        let Some(item) = entry.item.clone() else {
            return card
                .child(self.render_top(lay, None, cx))
                .child(self.render_notice(cx))
                .into_any_element();
        };
        let g = self.github.read(cx);
        let details = g.details(&item).cloned();
        let problem = g.detail_problem(&item).cloned();
        let card = card
            .child(self.render_top(lay, details.as_ref(), cx))
            .children(self.render_strip(cx));
        let Some(details) = details else {
            let body = match problem {
                Some(p) => div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(crate::ui::sp(10.))
                    .child(ui::empty_state(
                        "alert-circle",
                        &format!("Couldn't read {}", item.label()),
                        &p.text(),
                        None,
                    ))
                    .child(
                        ui::button("gh-retry", "Try again", ui::ButtonKind::Outline, true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let item = item.clone();
                                this.github.update(cx, |g, cx| g.fetch_item(item, cx));
                            })),
                    )
                    .into_any_element(),
                None => ui::loading("gh-item-loading", format!("Reading {}…", item.label()))
                    .flex_1()
                    .justify_center()
                    .into_any_element(),
            };
            return card.child(body).into_any_element();
        };
        if lay.side_props {
            let side = self.render_side(&details, cx);
            let main = self.render_main(&details, None, cx);
            return card
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w(px(SIDE_W))
                                .min_h_0()
                                .border_r_1()
                                .border_color(theme::border_subtle())
                                .child(side),
                        )
                        .child(main),
                )
                .into_any_element();
        }
        let head = self.render_head(&details, cx);
        card.child(self.render_main(&details, Some(head), cx))
            .into_any_element()
    }

    fn render_top(
        &self,
        lay: super::Layout,
        details: Option<&Details>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let narrow = lay.narrow;
        // Short bars drop what has another way in: the faces, then the
        // Comment button (the bar below and C), then Request changes (/changes).
        let roomy = lay.item_w >= 860.;
        let tight = lay.item_w < 600.;
        let entry = self.picked.clone();
        let item = entry.as_ref().and_then(|e| e.item.clone());
        let (icon_name, color) = match details {
            Some(Details::Pull(p)) => {
                let (i, c, _) = pull_look(p);
                (i, c)
            }
            Some(Details::Issue(i)) => {
                let (n, c, _) = issue_look(i);
                (n, c)
            }
            None => match &item {
                Some(i) if i.kind == ItemKind::Issue => ("circle-dot", theme::text_muted()),
                Some(_) => ("git-pull-request", theme::text_muted()),
                None => ("github", theme::text_muted()),
            },
        };
        let repo = entry.as_ref().map(|e| e.repo()).unwrap_or_default();
        let number = item.as_ref().map(|i| format!("#{}", i.number));
        let mine = self.is_mine(cx);
        let busy = self.busy;
        let reading = item
            .as_ref()
            .is_some_and(|i| self.github.read(cx).fetching(i));
        let pull_open = matches!(details, Some(Details::Pull(p)) if p.open());
        let issue_open = matches!(details, Some(Details::Issue(i)) if i.open());

        // Who took part: the author, then reviewers and commenters, each once.
        let mut people: Vec<String> = Vec::new();
        let mut add = |login: &str| {
            if !login.is_empty() && !people.iter().any(|p| p.eq_ignore_ascii_case(login)) {
                people.push(login.to_string());
            }
        };
        match details {
            Some(Details::Pull(p)) => {
                add(&p.author.login);
                p.reviews.iter().for_each(|r| add(&r.author.login));
                p.comments.iter().for_each(|c| add(&c.author.login));
                p.review_requests.iter().for_each(|r| add(&r.login));
            }
            Some(Details::Issue(i)) => {
                add(&i.author.login);
                i.comments.iter().for_each(|c| add(&c.author.login));
            }
            None => {}
        }
        let g = self.github.read(cx);
        let faces: Vec<(String, Option<std::path::PathBuf>)> = people
            .iter()
            .take(4)
            .map(|l| (l.clone(), g.avatar(l).map(|p| p.to_path_buf())))
            .collect();
        let avatars = ui::avatar_stack(&faces, 26., theme::fill_raised());
        let tip_people = people.join(", ");

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(10.))
            .px(crate::ui::sp(16.))
            .py(crate::ui::sp(10.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .when(narrow, |d| {
                d.child(
                    ui::icon_button("gh-back", "arrow-left01", 14.)
                        .tooltip(ui::tip("Back to the queue"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.narrow_open = false;
                            cx.notify();
                        })),
                )
            })
            .child(ui::icon_tile(icon_name, 26., color))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(crate::ui::sp(9.))
                    .ty(Type::Body)
                    .text_color(theme::text_muted())
                    .child(div().truncate().child(repo))
                    .when_some(number, |d, n| {
                        d.child(div().text_color(theme::text_faint()).child("›"))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(theme::FONT_MONO)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(n),
                            )
                    }),
            )
            .when(!people.is_empty() && roomy, |d| {
                d.child(
                    div()
                        .id("gh-people")
                        .flex()
                        .flex_none()
                        .child(avatars)
                        .tooltip(ui::tip_text(tip_people)),
                )
            })
            .when(item.is_some() && roomy, |d| {
                d.child(
                    ui::key_pill(
                        "gh-comment",
                        None,
                        "Comment",
                        Some("C"),
                        PillKind::Normal,
                        true,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.comment.focus_handle(cx), cx);
                    })),
                )
            })
            .when(pull_open && !mine && !tight, |d| {
                d.child(
                    ui::key_pill(
                        "gh-changes",
                        None,
                        "Request changes",
                        None,
                        PillKind::Normal,
                        busy.is_none(),
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.ask_confirm(Confirm::RequestChanges, cx)),
                    ),
                )
            })
            .when(pull_open && !mine, |d| {
                d.child(
                    ui::key_pill(
                        "gh-approve",
                        Some(("tick02", None)),
                        "Approve",
                        Some("A"),
                        PillKind::Primary,
                        busy.is_none(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.ask_confirm(Confirm::Approve, cx))),
                )
            })
            .when(issue_open, |d| {
                d.child(
                    ui::key_pill(
                        "gh-close",
                        None,
                        "Close",
                        None,
                        PillKind::Normal,
                        busy.is_none(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.ask_confirm(Confirm::Close, cx))),
                )
            })
            .when(reading, |d| {
                d.child(ui::spinner("gh-reading", 13., theme::text_faint()))
            })
            .child(
                ui::icon_button("gh-open-web", "link-square02", 14.)
                    .tooltip(ui::tip("Open on GitHub (O)"))
                    .on_click(cx.listener(|this, _, _, cx| this.open_on_github(cx))),
            )
            .into_any_element()
    }

    /// The strip under the top bar: a write waiting for its second click, or
    /// what the last one did.
    fn render_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let strip = || {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(10.))
                .px(crate::ui::sp(16.))
                .py(crate::ui::sp(9.))
                .border_b_1()
                .border_color(theme::border_subtle())
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
                        super::short_sha(&self.confirm_head)
                    ),
                    super::merge_label(m).to_string(),
                    false,
                ),
                Confirm::Close => (
                    "Close this issue?".to_string(),
                    "Close issue".to_string(),
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
            let busy = self.busy.is_some();
            return Some(
                strip()
                    .bg(theme::accent().opacity(0.06))
                    .child(icon("alert-circle", 14.).text_color(theme::accent()))
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
    fn render_notice(&self, cx: &mut Context<Self>) -> AnyElement {
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
                            Some(("sparkles", Some(theme::accent()))),
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

    /// The pull request's properties. `grid` shapes values for the cells
    /// above the tabs instead of the side column's rows.
    fn pull_props(&self, p: &Pull, grid: bool, cx: &mut Context<Self>) -> Vec<Prop> {
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
                    .text_color(theme::accent())
                    .child(dot(theme::accent()))
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
                .take(if grid { 6 } else { 12 })
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
        } else if grid && reviewers.len() > 2 {
            // Many: faces with how each stands on a badge.
            let mut row = div().flex().items_center().gap(crate::ui::sp(6.));
            for (login, state) in reviewers.iter().take(6) {
                let (name, c) = reviewer_look(*state);
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("gh-rev-{login}")))
                        .child(ui::avatar_badge(
                            login,
                            g.avatar(login),
                            22.,
                            name,
                            c,
                            theme::fill_raised(),
                        ))
                        .tooltip(ui::tip_text(format!("{login} · {}", reviewer_word(*state)))),
                );
            }
            row
        } else if grid {
            let mut row = div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(16.))
                .min_w_0();
            for (login, state) in &reviewers {
                let (name, c) = reviewer_look(*state);
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("gh-rev-{login}")))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .min_w_0()
                        .child(self.face(login, 20., cx))
                        .child(div().truncate().child(login.clone()))
                        .child(icon(name, 13.).text_color(c))
                        .tooltip(ui::tip_text(format!("{login} · {}", reviewer_word(*state)))),
                );
            }
            row
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
            props.push(Prop::new("clock01", "Waiting on", waiting_v).key());
        }
        props.push(Prop::new("user", "Author", author_v));
        props.push(Prop::new("activity01", "Checks", checks_v).key());
        if reviewers.is_empty() {
            props.push(Prop::new("tick02", "Review", reviewers_v).key());
        } else {
            let many = grid && reviewers.len() == 2;
            let prop = Prop::new("tick02", "Reviewers", reviewers_v).key();
            props.push(if many { prop.wide() } else { prop });
            if !grid {
                props.push(Prop::new(
                    "checkmark-circle02",
                    "Review",
                    div().truncate().child(review_text),
                ));
            }
        }
        if open {
            props.push(Prop::new("git-merge", "Merge", merge_v).wide().key());
        }
        props.push(Prop::new("file01", "Changes", changes_v).key());
        props.push(Prop::new("git-commit", "Commit", commit_v));
        if !p.labels.is_empty() {
            props.push(
                Prop::new(
                    "tag01",
                    "Labels",
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(crate::ui::sp(4.))
                        .children(p.labels.iter().map(label_chip)),
                )
                .wide(),
            );
        }
        props
    }

    fn issue_props(&self, i: &pr::Issue, cx: &mut Context<Self>) -> Vec<Prop> {
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
            )
            .key(),
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
            props.push(
                Prop::new(
                    "tag01",
                    "Labels",
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(crate::ui::sp(4.))
                        .children(i.labels.iter().map(label_chip)),
                )
                .wide(),
            );
        }
        props
    }

    /// The pull request's own actions: the agent's review and the merge
    /// button, blocked with its reason. `row` lays them side by side.
    fn pull_actions(&self, p: &Pull, row: bool, cx: &mut Context<Self>) -> gpui::Div {
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
            Some(("sparkles", Some(theme::accent()))),
            "Review with agent",
            Some("R"),
            PillKind::Normal,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.review_with_agent(cx)));
        if row {
            return div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(8.))
                .child(agent)
                .when(p.open(), |d| d.child(merge_btn));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(div().flex().flex_wrap().gap(crate::ui::sp(8.)).child(agent))
            .when(p.open(), |d| d.child(div().flex().child(merge_btn)))
    }

    /// The item's eyebrow and title.
    fn title_block(&self, details: &Details, big: bool) -> gpui::Div {
        let (eyebrow, title) = match details {
            Details::Pull(p) => (
                format!("#{} · {} → {}", p.number, p.head_ref_name, p.base_ref_name),
                p.title.clone(),
            ),
            Details::Issue(i) => (format!("#{} · issue", i.number), i.title.clone()),
        };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap(crate::ui::sp(4.))
            .child(
                div()
                    .truncate()
                    .child(super::mono(eyebrow, theme::text_subtle())),
            )
            .child(
                div()
                    .ty(if big { Type::Title } else { Type::Subtitle })
                    .text_color(theme::text())
                    .child(title),
            )
    }

    /// The side column beside the tabs: title, properties, actions.
    fn render_side(&self, details: &Details, cx: &mut Context<Self>) -> AnyElement {
        let (icon_name, color) = match details {
            Details::Pull(p) => {
                let (i, c, _) = pull_look(p);
                (i, c)
            }
            Details::Issue(i) => {
                let (n, c, _) = issue_look(i);
                (n, c)
            }
        };
        let props = match details {
            Details::Pull(p) => self.pull_props(p, false, cx),
            Details::Issue(i) => self.issue_props(i, cx),
        };
        let mut list = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .px(crate::ui::sp(20.))
            .py(crate::ui::sp(16.))
            .ty(Type::Body);
        for p in props {
            list = list.child(prop_row(p));
        }
        div()
            .id("gh-side")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(16.))
                    .px(crate::ui::sp(20.))
                    .pt(crate::ui::sp(20.))
                    .pb(crate::ui::sp(16.))
                    .child(ui::icon_tile(icon_name, 40., color))
                    .child(self.title_block(details, true)),
            )
            .child(div().mx(px(20.)).h(px(1.)).bg(theme::border_subtle()))
            .child(list)
            .when_some(
                match details {
                    Details::Pull(p) => Some(self.pull_actions(p, false, cx)),
                    _ => None,
                },
                |d, actions| {
                    d.child(
                        div()
                            .mt_auto()
                            .px(crate::ui::sp(20.))
                            .pt(crate::ui::sp(12.))
                            .pb(crate::ui::sp(20.))
                            .child(actions),
                    )
                },
            )
            .into_any_element()
    }

    /// The item's head when the properties wrap above the tabs: title and
    /// actions, then the properties as a grid.
    fn render_head(&self, details: &Details, cx: &mut Context<Self>) -> AnyElement {
        let props = match details {
            Details::Pull(p) => self.pull_props(p, true, cx),
            Details::Issue(i) => self.issue_props(i, cx),
        };
        // A short window folds the head to its key properties on one line.
        let folded = !self.head_open && self.size.get().1 < super::TALL;
        let mut grid = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(crate::ui::sp(if folded { 20. } else { CELL_GAP }))
            .gap_y(crate::ui::sp(if folded { 8. } else { 14. }))
            .ty(Type::Body);
        for p in props {
            if !folded {
                grid = grid.child(prop_cell(p));
            } else if p.key {
                let first = p.label == "Status";
                grid = grid.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .min_w_0()
                        .when(!first, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(p.label),
                            )
                        })
                        .child(p.value),
                );
            }
        }
        let tall = self.size.get().1 >= super::TALL;
        let grid = div()
            .flex()
            .items_start()
            .gap(crate::ui::sp(12.))
            .child(grid.flex_1().min_w_0())
            .when(!tall, |d| {
                let open = self.head_open;
                d.child(
                    ui::icon_button(
                        "gh-head-fold",
                        if open { "arrow-up01" } else { "arrow-down01" },
                        13.,
                    )
                    .tooltip(ui::tip(if open { "Fewer details" } else { "All details" }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.head_open = !this.head_open;
                        cx.notify();
                    })),
                )
            });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(crate::ui::sp(14.))
            .px(crate::ui::sp(24.))
            .pt(crate::ui::sp(16.))
            .pb(crate::ui::sp(16.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    // A narrow card puts the actions under the title, not beside it.
                    .when(self.layout().item_w < 760., |d| d.flex_col())
                    .items_start()
                    .gap(crate::ui::sp(12.))
                    .child(self.title_block(details, false).flex_1().w_full())
                    .when_some(
                        match details {
                            Details::Pull(p) => Some(self.pull_actions(p, true, cx)),
                            _ => None,
                        },
                        |d, a| d.child(a),
                    ),
            )
            .child(grid)
            .into_any_element()
    }

    fn render_main(
        &self,
        details: &Details,
        head: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default().to_string();
        let mut tabs = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .px(crate::ui::sp(18.))
            .py(crate::ui::sp(12.))
            .border_b_1()
            .border_color(theme::border_subtle());
        let mut new_since = 0;
        let mut list: Vec<(Tab, &str, Option<String>)> = vec![(Tab::Timeline, "Timeline", None)];
        if let Details::Pull(p) = details {
            let checks = pr::checks_summary(p);
            new_since = pr::new_since_my_review(p, &me).len();
            list.push((Tab::Files, "Files", Some(p.changed_files.to_string())));
            list.push((
                Tab::Checks,
                "Checks",
                Some(format!(
                    "{}/{}",
                    checks.passed,
                    checks.total() - checks.skipped
                )),
            ));
            list.push((Tab::Commits, "Commits", Some(p.commits.len().to_string())));
        }
        let tab = if matches!(details, Details::Issue(_)) {
            Tab::Timeline
        } else {
            self.tab
        };
        for (t, label, count) in list {
            tabs = tabs.child(
                ui::outline_tab(("gh-tab", t as usize), label, count, t == tab)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(t, cx))),
            );
        }
        if new_since > 0 {
            tabs = tabs.child(div().ml_auto().child(super::mono(
                if new_since == 1 {
                    "1 new since your review".to_string()
                } else {
                    format!("{new_since} new since your review")
                },
                theme::text_subtle(),
            )));
        }
        let lay = self.layout();
        let main_w = lay.item_w - if lay.side_props { SIDE_W } else { 0. };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .children(head)
            // An issue has only its timeline: a row of one tab says nothing.
            .when(matches!(details, Details::Pull(_)), |d| d.child(tabs))
            // A pane of its own: scrolling it builds only what it shows.
            .child(super::Pane::cached(
                &self.body_pane,
                gpui::StyleRefinement::default().flex_1().min_h_0(),
            ))
            .child(self.render_command_bar(main_w, cx))
            .into_any_element()
    }

    /// What the tab picked shows, in the body under the tabs.
    pub(super) fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_timeline(cx);
        self.sync_files();
        let Some(item) = self.item() else {
            return div().into_any_element();
        };
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default();
        let Some(details) = g.details(&item) else {
            return div().into_any_element();
        };
        let tab = if matches!(details, Details::Issue(_)) {
            Tab::Timeline
        } else {
            self.tab
        };
        let content = match (tab, details) {
            (Tab::Files, Details::Pull(p)) => self.render_files(p, cx),
            (Tab::Checks, Details::Pull(p)) => self.render_checks(p, cx),
            (Tab::Commits, Details::Pull(p)) => self.render_commits(p, me, cx),
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

    fn render_command_bar(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        // The hints go first when the bar is short: typing needs the room.
        let hints = width >= 720.;
        let next_only = !hints && width >= 520.;
        let pull = self.item().is_some_and(|i| i.kind == ItemKind::Pull);
        let sending = self.busy == Some("comment");
        let me = self.github.read(cx).me().unwrap_or_default().to_string();
        let bar = div()
            .flex()
            .flex_1()
            .items_center()
            .gap(crate::ui::sp(12.))
            .pl(crate::ui::sp(10.))
            .pr(crate::ui::sp(14.))
            .h(px(44.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::background())
            .child(if sending {
                ui::spinner("gh-sending", 15., theme::text_faint()).into_any_element()
            } else if me.is_empty() {
                icon("comment01", 15.)
                    .text_color(theme::text_faint())
                    .into_any_element()
            } else {
                self.face(&me, 24., cx).into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ty(Type::Body)
                    .child(self.comment.clone()),
            )
            .when(hints || next_only, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(14.))
                        .when(hints, |d| {
                            d.child(
                                div()
                                    .id("gh-hint-u")
                                    .child(ui::key_hint("Catch me up", "U"))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.catch_up(cx))),
                            )
                        })
                        .when(hints, |d| {
                            d.child(
                                div()
                                    .id("gh-hint-s")
                                    .child(ui::key_hint("Summarize", "S"))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.summarize(cx))),
                            )
                        })
                        .child(
                            div()
                                .id("gh-hint-j")
                                .child(ui::key_hint("Next", "J"))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
                        ),
                )
            })
            .when(!pull, |d| d);
        div()
            .flex()
            .flex_none()
            .px(crate::ui::sp(16.))
            .pt(crate::ui::sp(12.))
            .pb(crate::ui::sp(16.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(bar)
            .into_any_element()
    }
}

fn reviewer_look(s: pr::ReviewerState) -> (&'static str, Hsla) {
    match s {
        pr::ReviewerState::Approved => ("tick02", theme::success().into()),
        pr::ReviewerState::ChangesRequested => ("alert-circle", theme::error().into()),
        pr::ReviewerState::Commented => ("comment01", theme::text_muted()),
        pr::ReviewerState::Requested => ("clock01", theme::accent()),
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
