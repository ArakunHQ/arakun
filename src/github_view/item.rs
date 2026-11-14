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
const SIDE_W: f32 = 332.;
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

/// The first paragraph of a body, without markdown marks: what the pull
/// request says it does, in a few lines.
pub(super) fn first_paragraph(body: &str) -> String {
    let mut out = String::new();
    let mut in_comment = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("<!--") {
            in_comment = !t.contains("-->");
            continue;
        }
        if in_comment {
            in_comment = !t.contains("-->");
            continue;
        }
        if t.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        // Headings and template boxes say what a section is, not what the change does.
        if t.starts_with('#') || t.starts_with("- [") || t.starts_with("```") {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
    }
    let plain: String = out.replace("**", "").replace('`', "");
    if plain.chars().count() > 280 {
        format!(
            "{}…",
            plain.chars().take(278).collect::<String>().trim_end()
        )
    } else {
        plain
    }
}

/// A dot before a value in the property list.
fn dot(color: Hsla) -> gpui::Div {
    div().size(px(8.)).flex_none().rounded_full().bg(color)
}

fn prop(label: &str, value: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .min_h(px(22.))
        .child(
            div()
                .w(px(KEY_W))
                .flex_none()
                .text_color(theme::text_subtle())
                .child(label.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(8.))
                .overflow_hidden()
                .child(value),
        )
}

impl GitHubView {
    pub(super) fn render_item(&self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
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
                .child(self.render_top(narrow, None, cx))
                .child(self.render_notice(cx))
                .into_any_element();
        };
        let g = self.github.read(cx);
        let details = g.details(&item).cloned();
        let problem = g.detail_problem(&item).cloned();
        let card = card
            .child(self.render_top(narrow, details.as_ref(), cx))
            .children(self.render_strip(cx));
        let Some(details) = details else {
            let body = match problem {
                Some(p) => div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
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
        let side = match &details {
            Details::Pull(p) => self.render_pull_side(p, cx),
            Details::Issue(i) => self.render_issue_side(i, cx),
        };
        let main = self.render_main(&details, cx);
        card.child(
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
        .into_any_element()
    }

    fn render_top(
        &self,
        narrow: bool,
        details: Option<&Details>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        // Overlapping circles, each placed by hand: negative margins don't give the row its width.
        let shown = people.len().min(4);
        let avatars = div()
            .relative()
            .flex_none()
            .h(px(24.))
            .w(px(24. + shown.saturating_sub(1) as f32 * 17.))
            .children(people.iter().take(4).enumerate().map(|(i, login)| {
                ui::avatar(login, 24.)
                    .absolute()
                    .top_0()
                    .left(px(i as f32 * 17.))
                    .border_2()
                    .border_color(theme::background())
            }));
        let tip_people = people.join(", ");

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .py(px(10.))
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
                    .gap(px(9.))
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
            .when(!people.is_empty(), |d| {
                d.child(
                    div()
                        .id("gh-people")
                        .flex()
                        .flex_none()
                        .child(avatars)
                        .tooltip(ui::tip_text(tip_people)),
                )
            })
            .when(item.is_some(), |d| {
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
            .when(pull_open && !mine, |d| {
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
                .child(
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
                .gap(px(10.))
                .px(px(16.))
                .py(px(9.))
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
                    format!("{} into {base}?", super::merge_label(m)),
                    super::merge_label(m).to_string(),
                    false,
                ),
                Confirm::Close => (
                    "Close this issue?".to_string(),
                    "Close issue".to_string(),
                    false,
                ),
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
            .gap(px(12.))
            .p(px(18.))
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
                    .gap(px(8.))
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

    fn render_pull_side(&self, p: &Pull, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default().to_string();
        let (icon_name, color, status) = pull_look(p);
        let checks = pr::checks_summary(p);
        let blockers = pr::blockers(p);
        let waiting = pr::waiting_on(p, &me);
        let desc = first_paragraph(&p.body);

        let intro = div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(18.))
            .pt(px(18.))
            .pb(px(16.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(ui::icon_tile(icon_name, 44., color))
            .child(div().mt(px(14.)).truncate().child(super::mono(
                format!("#{} · {} → {}", p.number, p.head_ref_name, p.base_ref_name),
                theme::text_subtle(),
            )))
            .child(div().mt(px(5.)).ty(Type::Title).child(p.title.clone()))
            .when(!desc.is_empty(), |d| {
                d.child(
                    div()
                        .mt(px(8.))
                        .ty(Type::Body)
                        .line_height(px(19.))
                        .text_color(theme::text_muted())
                        .child(desc),
                )
            });

        let status_v = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(dot(color))
            .child(status);
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
                    .gap(px(8.))
                    .min_w_0()
                    .text_color(theme::accent())
                    .child(dot(theme::accent()))
                    .child(div().truncate().child(format!("You · {why}")))
            }
            other => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .text_color(theme::text())
                .child(dot(theme::link().into()))
                .child(div().truncate().child(match other {
                    WaitingOn::Author => p.author.login.clone(),
                    WaitingOn::Reviewers(who) if who.is_empty() => "A reviewer".into(),
                    WaitingOn::Reviewers(who) => who.join(", "),
                    WaitingOn::Checks => "Checks".into(),
                    _ => "Nobody".into(),
                })),
        };
        let author_v = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(dot(theme::link().into()))
            .child(p.author.login.clone());
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
                .gap(px(8.))
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
            "CHANGES_REQUESTED" => {
                let who: Vec<String> = p
                    .latest_reviews
                    .iter()
                    .filter(|r| r.state == "CHANGES_REQUESTED")
                    .map(|r| r.author.login.clone())
                    .collect();
                if who.is_empty() {
                    "Changes requested".into()
                } else {
                    format!("Changes requested by {}", who.join(", "))
                }
            }
            "REVIEW_REQUIRED" => "Approval required".into(),
            _ => "No review required".into(),
        };
        let review_text = if pr::my_review_dismissed(p, &me) {
            format!("{review_text} · yours was dismissed")
        } else {
            review_text
        };
        let merge_v = if blockers.is_empty() {
            div().text_color(theme::success()).child("Ready to merge")
        } else {
            div()
                .truncate()
                .text_color(theme::text_muted())
                .child(format!(
                    "Blocked · {}",
                    lower_first(&pr::blockers_text(&blockers))
                ))
        };
        let changes_v = div()
            .flex()
            .items_center()
            .gap(px(8.))
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
                .gap(px(6.))
                .child(super::mono(c.short_oid().to_string(), theme::text()))
                .child(super::mono(
                    format!("· {} ago", super::ago_iso(&c.committed_date)),
                    theme::text_subtle(),
                )),
            None => div().text_color(theme::text_subtle()).child("—"),
        };
        let props = div()
            .flex()
            .flex_col()
            .gap(px(11.))
            .px(px(18.))
            .py(px(14.))
            .ty(Type::Body)
            .child(prop("Status", status_v))
            .child(prop("Waiting on", waiting_v))
            .child(prop("Author", author_v))
            .child(prop("Checks", checks_v))
            .child(prop("Review", div().truncate().child(review_text)))
            .child(prop("Merge", merge_v))
            .child(prop("Changes", changes_v))
            .child(prop("Commit", commit_v));

        // The merge button: the repository's method, blocked with its reason.
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
        let footer = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .mt_auto()
            .px(px(18.))
            .pt(px(12.))
            .pb(px(16.))
            .child(
                div().flex().flex_wrap().gap(px(8.)).child(
                    ui::key_pill(
                        "gh-agent",
                        Some(("sparkles", Some(theme::accent()))),
                        "Review with agent",
                        Some("R"),
                        PillKind::Normal,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.review_with_agent(cx))),
                ),
            )
            .when(p.open(), |d| d.child(div().flex().child(merge_btn)))
            .when_some(why_not.filter(|_| p.open()), |d, why| {
                d.child(
                    div()
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .line_clamp(2)
                        .child(why),
                )
            });

        div()
            .id("gh-side")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(intro)
            .child(props)
            .child(footer)
            .into_any_element()
    }

    fn render_issue_side(&self, i: &pr::Issue, cx: &mut Context<Self>) -> AnyElement {
        let _ = cx;
        let (icon_name, color, status) = issue_look(i);
        let desc = first_paragraph(&i.body);
        let assignees: Vec<String> = i.assignees.iter().map(|a| a.login.clone()).collect();
        let labels: Vec<String> = i.labels.iter().map(|l| l.name.clone()).collect();
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
                    .px(px(18.))
                    .pt(px(18.))
                    .pb(px(16.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(ui::icon_tile(icon_name, 44., color))
                    .child(div().mt(px(14.)).child(super::mono(
                        format!("#{} · issue", i.number),
                        theme::text_subtle(),
                    )))
                    .child(div().mt(px(5.)).ty(Type::Title).child(i.title.clone()))
                    .when(!desc.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(px(8.))
                                .ty(Type::Body)
                                .line_height(px(19.))
                                .text_color(theme::text_muted())
                                .child(desc),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(11.))
                    .px(px(18.))
                    .py(px(14.))
                    .ty(Type::Body)
                    .child(prop(
                        "Status",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(dot(color))
                            .child(status),
                    ))
                    .child(prop(
                        "Author",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(dot(theme::link().into()))
                            .child(i.author.login.clone()),
                    ))
                    .child(prop(
                        "Assignees",
                        div()
                            .truncate()
                            .when(assignees.is_empty(), |d| d.text_color(theme::text_subtle()))
                            .child(if assignees.is_empty() {
                                "Nobody".to_string()
                            } else {
                                assignees.join(", ")
                            }),
                    ))
                    .when(!labels.is_empty(), |d| {
                        d.child(prop("Labels", div().truncate().child(labels.join(", "))))
                    })
                    .child(prop(
                        "Comments",
                        super::mono(i.comments.len().to_string(), theme::text()),
                    ))
                    .child(prop(
                        "Opened",
                        super::mono(
                            format!("{} ago", super::ago_iso(&i.created_at)),
                            theme::text(),
                        ),
                    )),
            )
            .into_any_element()
    }

    fn render_main(&self, details: &Details, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default().to_string();
        let mut tabs = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(18.))
            .py(px(12.))
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
        let content = match (tab, details) {
            (Tab::Files, Details::Pull(p)) => self.render_files(p, cx),
            (Tab::Checks, Details::Pull(p)) => self.render_checks(p, cx),
            (Tab::Commits, Details::Pull(p)) => self.render_commits(p, &me, cx),
            _ => self.render_timeline(details, &me, cx),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(tabs)
            .child(
                div()
                    .id("gh-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.body_scroll)
                    .child(content),
            )
            .child(self.render_command_bar(cx))
            .into_any_element()
    }

    fn render_command_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let pull = self.item().is_some_and(|i| i.kind == ItemKind::Pull);
        let sending = self.busy == Some("comment");
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(11.))
            .mx(px(14.))
            .mb(px(14.))
            .mt(px(8.))
            .px(px(14.))
            .h(px(44.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::background())
            .child(if sending {
                ui::spinner("gh-sending", 15., theme::text_faint()).into_any_element()
            } else {
                icon("search01", 15.)
                    .text_color(theme::text_faint())
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ty(Type::Body)
                    .child(self.comment.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(14.))
                    .child(
                        div()
                            .id("gh-hint-u")
                            .child(ui::key_hint("Catch me up", "U"))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.catch_up(cx))),
                    )
                    .child(
                        div()
                            .id("gh-hint-s")
                            .child(ui::key_hint("Summarize", "S"))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.summarize(cx))),
                    )
                    .child(
                        div()
                            .id("gh-hint-j")
                            .child(ui::key_hint("Next", "J"))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
                    ),
            )
            .when(!pull, |d| d)
            .into_any_element()
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
    fn the_description_is_the_first_real_paragraph() {
        let body = "<!-- template\nsays things -->\n## What\n\nSamples CPU and **memory** in every job,\nthen reports each Monday.\n\nMore later.";
        assert_eq!(
            first_paragraph(body),
            "Samples CPU and memory in every job, then reports each Monday."
        );
        assert_eq!(first_paragraph(""), "");
        assert_eq!(first_paragraph("- [ ] tests\n- [x] docs"), "");
        let long = "word ".repeat(100);
        assert!(first_paragraph(&long).ends_with('…'));
        assert_eq!(
            lower_first("Needs an approving review"),
            "needs an approving review"
        );
    }
}
