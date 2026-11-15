//! The Timeline tab: what happened on the item, oldest first, on a line of
//! dots, with the agent's draft review as the last event.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::{GitHubView, body_style};
use crate::github::pr::{self, Event, EventKind, LineKind, Thread, Verdict};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, PillKind, Type, Typography as _, icon};

/// How many lines of the diff a thread shows above its comments.
const HUNK_TAIL: usize = 4;
/// The avatars on the rail.
const RAIL: f32 = 28.;
/// Lines a long body shows before "Show more".
const FOLD_LINES: usize = 10;
/// The line height of bodies in the timeline.
const BODY_LINE_H: f32 = 22.;

/// About how many lines `text` takes at `cols` characters a line: each line
/// wrapped, blank lines as half a line (they are the gap between paragraphs).
fn estimated_lines(text: &str, cols: usize) -> usize {
    let cols = cols.max(20);
    let mut halves = 0;
    for line in text.lines() {
        let n = line.trim_end().chars().count();
        halves += if n == 0 { 1 } else { 2 * n.div_ceil(cols) };
    }
    halves.div_ceil(2)
}

/// Whether a body is long enough to fold to its first lines. A body only a
/// little past the fold shows whole: hiding three lines saves nothing.
fn folds(text: &str, cols: usize) -> bool {
    estimated_lines(text, cols) > FOLD_LINES + 3
}

/// Text without its inline HTML tags: `a<br/><sub>b</sub>` → `a b`.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first line of a body with words in it, for a folded bot comment.
fn first_words(text: &str) -> String {
    let plain =
        strip_tags(&super::without_html(text).replace('\n', "\u{1}")).replace('\u{1}', "\n");
    plain
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '>', '-', '*', ' '])
                .replace("**", "")
                .replace('`', "")
        })
        .find(|l| !l.is_empty() && !l.starts_with("```") && !l.starts_with('|'))
        .unwrap_or_default()
}

/// One event: its avatar on the rail, and the line down to the next.
fn event(lead: AnyElement, last: bool, body: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .flex()
        .gap(crate::ui::sp(12.))
        .w_full()
        .min_w_0()
        // The line runs in the event itself: the rail has no height of its own.
        .when(!last, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(RAIL / 2. - 0.5))
                    .top(px(RAIL + 6.))
                    .bottom(px(6.))
                    .w(px(1.))
                    .bg(theme::border_subtle()),
            )
        })
        .child(div().flex_none().w(px(RAIL)).child(lead))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .pb(crate::ui::sp(24.))
                .child(body),
        )
}

/// "rik approved · 2h ago": who in semibold, what they did, and when. The
/// pieces in `extra` go between the action and the time.
fn header(who: &str, what: &str, at: u64, extra: Vec<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(crate::ui::sp(6.))
        .min_h(px(RAIL))
        .ty(Type::Label)
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(who.to_string()),
        )
        .when(!what.is_empty(), |d| {
            d.child(
                div()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_muted())
                    .child(what.to_string()),
            )
        })
        .children(extra)
        .when(at > 0, |d| {
            d.child(super::mono(
                format!("{} ago", super::ago(at)),
                theme::text_faint(),
            ))
        })
}

/// A comment's body in its card.
fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .mt(px(6.))
        .px(crate::ui::sp(16.))
        .py(crate::ui::sp(12.))
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::background())
}

/// A review's verdict as a pill.
fn verdict_pill(state: &str) -> Option<AnyElement> {
    let (name, label, color): (&str, &str, Hsla) = match state {
        "APPROVED" => ("tick02", "Approved", theme::success().into()),
        "CHANGES_REQUESTED" => ("alert-circle", "Changes requested", theme::error().into()),
        "DISMISSED" => ("cancel01", "Dismissed", theme::text_subtle()),
        "COMMENTED" => ("comment01", "Commented", theme::text_muted()),
        _ => return None,
    };
    Some(ui::state_pill(name, label, color).into_any_element())
}

/// A piece of the diff in a bordered block: the file, then numbered lines.
pub(super) fn code_block(path: &str, line: Option<u32>, lines: Vec<AnyElement>) -> gpui::Div {
    div()
        .mt(px(9.))
        .flex()
        .flex_col()
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::background())
        .overflow_hidden()
        .child(
            div()
                .flex()
                .justify_between()
                .gap(crate::ui::sp(12.))
                .px(crate::ui::sp(12.))
                .py(crate::ui::sp(7.))
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(super::mono(path.to_string(), theme::text_muted())),
                )
                .when_some(line, |d, l| {
                    d.child(super::mono(format!(":{l}"), theme::text_subtle()))
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .py(crate::ui::sp(6.))
                .children(lines),
        )
}

/// One line of a diff: its number, a colored edge and tint when it changed.
pub(super) fn code_line(kind: LineKind, number: Option<u32>, text: &str) -> AnyElement {
    let (edge, tint, color): (Option<Hsla>, Option<Hsla>, Hsla) = match kind {
        LineKind::Add => (
            Some(theme::diff_add_bar().into()),
            Some(theme::diff_add_bg()),
            theme::diff_add().into(),
        ),
        LineKind::Del => (
            Some(theme::diff_del_bar().into()),
            Some(theme::diff_del_bg()),
            theme::diff_del().into(),
        ),
        LineKind::Context => (None, None, theme::text_muted()),
    };
    div()
        .relative()
        .flex()
        .items_center()
        .h(px(20.))
        .pr(crate::ui::sp(12.))
        .ty(Type::Mono)
        .when_some(tint, |d, t| d.bg(t))
        .when_some(edge, |d, e| {
            d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(e))
        })
        .child(
            div()
                .w(px(42.))
                .flex_none()
                .pr(crate::ui::sp(12.))
                .text_right()
                .text_color(theme::text_faint())
                .child(number.map(|n| n.to_string()).unwrap_or_default()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(color)
                .child(text.replace('\t', "    ")),
        )
        .into_any_element()
}

/// The last lines of the diff a thread was written on.
fn hunk_lines(hunk: &str) -> Vec<AnyElement> {
    let fake = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{hunk}\n");
    let files = pr::split_diff(&fake);
    let lines: Vec<&pr::Line> = files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .flat_map(|h| h.lines.iter())
        .collect();
    let from = lines.len().saturating_sub(HUNK_TAIL);
    lines[from..]
        .iter()
        .map(|l| code_line(l.kind, l.new_no.or(l.old_no), &l.text))
        .collect()
}

fn verdict_word(v: Verdict) -> &'static str {
    match v {
        Verdict::Approve => "approval",
        Verdict::RequestChanges => "change request",
        Verdict::Comment => "comment",
    }
}

impl GitHubView {
    fn render_thread(&self, key: &str, t: &Thread, cx: &App) -> AnyElement {
        let mut comments = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .pt(crate::ui::sp(12.));
        for (i, c) in t.comments.iter().enumerate() {
            comments = comments.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(self.face(&c.user.login, 20., cx))
                            .child(header(
                                &c.user.login,
                                "",
                                crate::ci::parse_time(&c.created_at).unwrap_or(0),
                                Vec::new(),
                            )),
                    )
                    .child(div().pl(crate::ui::sp(28.)).child(self.markdown(
                        gpui::SharedString::from(format!("{key}-c{i}")),
                        &c.body,
                        body_style(),
                    ))),
            );
        }
        let state = match (t.resolved, t.outdated) {
            (Some(true), _) => Some("Resolved"),
            (_, Some(true)) => Some("Outdated"),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .child(code_block(&t.path, t.line, hunk_lines(&t.diff_hunk)))
            .when_some(state, |d, s| {
                d.child(
                    div()
                        .flex()
                        .mt(px(6.))
                        .child(ui::tag(s, theme::text_subtle())),
                )
            })
            .child(comments)
            .into_any_element()
    }

    /// A body in its card, folded to its first lines when long.
    fn body_card(&self, key: &str, text: &str, cols: usize, cx: &mut Context<Self>) -> gpui::Div {
        let long = folds(text, cols);
        let open = self.expanded.contains(key);
        let content = self.markdown(
            gpui::SharedString::from(key.to_string()),
            text,
            body_style(),
        );
        let k = key.to_string();
        card()
            .child(if long && !open {
                // The last lines fade out, so a cut line reads as "more below".
                div()
                    .relative()
                    .max_h(px(FOLD_LINES as f32 * BODY_LINE_H))
                    .overflow_hidden()
                    .child(content)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(3. * BODY_LINE_H))
                            .bg(gpui::linear_gradient(
                                180.,
                                gpui::linear_color_stop(theme::background().opacity(0.), 0.),
                                gpui::linear_color_stop(theme::background(), 1.),
                            )),
                    )
                    .into_any_element()
            } else {
                content
            })
            .when(long, |d| {
                d.child(
                    div()
                        .id(gpui::SharedString::from(format!("{key}-fold")))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .mt(px(8.))
                        .cursor_pointer()
                        .ty(Type::Label)
                        .text_color(theme::link())
                        .hover(|s| s.text_color(theme::text()))
                        .child(if open { "Show less" } else { "Show more" })
                        .child(
                            icon(if open { "arrow-up01" } else { "arrow-down01" }, 12.)
                                .text_color(theme::link()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.expanded.remove(&k) {
                                this.expanded.insert(k.clone());
                            }
                            cx.notify();
                        })),
                )
            })
    }

    /// A bot's comment: one line until opened.
    fn bot_line(
        &self,
        key: &str,
        who: &str,
        at: u64,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let k = key.to_string();
        let preview = first_words(text);
        div()
            .id(gpui::SharedString::from(format!("{key}-bot")))
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .min_w_0()
            .h(px(RAIL))
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(header(who, "commented", 0, Vec::new()))
                    .child(ui::tag("bot", theme::text_subtle())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_subtle())
                    .hover(|s| s.text_color(theme::text_muted()))
                    .child(preview),
            )
            .when(at > 0, |d| {
                d.child(
                    super::mono(format!("{} ago", super::ago(at)), theme::text_faint()).flex_none(),
                )
            })
            .child(icon("arrow-down01", 12.).text_color(theme::text_subtle()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.expanded.insert(k.clone());
                cx.notify();
            }))
            .into_any_element()
    }

    /// About how many characters a line of the timeline holds now.
    fn timeline_cols(&self) -> usize {
        let lay = self.layout();
        let mut main = lay.item_w;
        if lay.side_props {
            main -= super::item::SIDE_W;
        }
        // The rail, the gaps and the card's own padding.
        ((main - 130.) / 7.4).max(30.) as usize
    }

    pub(super) fn render_timeline(
        &self,
        details: &Details,
        me: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let you = |who: &str| {
            if !who.is_empty() && who.eq_ignore_ascii_case(me) {
                "You".to_string()
            } else {
                who.to_string()
            }
        };
        let cols = self.timeline_cols();
        let mut rows: Vec<(AnyElement, AnyElement)> = Vec::new();
        let face =
            |this: &Self, login: &str, cx: &App| this.face(login, RAIL, cx).into_any_element();

        let (author, created, body, events, new_oids, noun) = match details {
            Details::Pull(p) => {
                let new: Vec<String> = pr::new_since_my_review(p, me)
                    .iter()
                    .map(|c| c.oid.clone())
                    .collect();
                (
                    p.author.login.clone(),
                    p.created_at.clone(),
                    p.body.clone(),
                    pr::timeline(p),
                    new,
                    "opened this pull request",
                )
            }
            Details::Issue(i) => (
                i.author.login.clone(),
                i.created_at.clone(),
                i.body.clone(),
                pr::issue_timeline(i),
                Vec::new(),
                "opened this issue",
            ),
        };
        let opened_at = crate::ci::parse_time(&created).unwrap_or(0);
        rows.push((
            face(self, &author, cx),
            div()
                .flex()
                .flex_col()
                .child(header(&you(&author), noun, opened_at, Vec::new()))
                .child(if body.trim().is_empty() {
                    card()
                        .child(ui::text(Type::Body, theme::text_faint(), "No description."))
                        .into_any_element()
                } else {
                    self.body_card("gh-ev-open", &body, cols, cx)
                        .into_any_element()
                })
                .into_any_element(),
        ));

        for range in pr::group_pushes(&events) {
            let i = range.start;
            let e: &Event = &events[i];
            let key = format!("gh-ev-{i}");
            let lead = face(self, &e.author, cx);
            match &e.kind {
                EventKind::Commit { .. } => {
                    let run = &events[range.clone()];
                    let fresh = run.iter().any(|c| matches!(&c.kind, EventKind::Commit { oid, .. } if new_oids.contains(oid)));
                    let what = if run.len() == 1 {
                        "pushed a commit".to_string()
                    } else {
                        format!("pushed {} commits", run.len())
                    };
                    let extra = if fresh {
                        vec![ui::tag("new since your review", theme::accent()).into_any_element()]
                    } else {
                        Vec::new()
                    };
                    let last = run.last().map_or(0, |c| c.at);
                    let mut list = div()
                        .flex()
                        .flex_col()
                        .mt(px(6.))
                        .py(crate::ui::sp(4.))
                        .rounded(crate::ui::rad(ui::RADIUS_LG))
                        .border_1()
                        .border_color(theme::border_subtle())
                        .bg(theme::background());
                    for c in run.iter().take(8) {
                        if let EventKind::Commit { oid, headline } = &c.kind {
                            let new = new_oids.contains(oid);
                            list = list.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(10.))
                                    .min_w_0()
                                    .px(crate::ui::sp(14.))
                                    .h(px(30.))
                                    .child(icon("git-commit", 14.).text_color(if new {
                                        theme::accent()
                                    } else {
                                        theme::text_faint()
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .ty(Type::Body)
                                            .text_color(theme::text())
                                            .child(headline.clone()),
                                    )
                                    .child(
                                        super::mono(
                                            oid.chars().take(7).collect::<String>(),
                                            theme::text_subtle(),
                                        )
                                        .flex_none(),
                                    ),
                            );
                        }
                    }
                    if run.len() > 8 {
                        list = list.child(
                            div()
                                .px(crate::ui::sp(14.))
                                .h(px(26.))
                                .flex()
                                .items_center()
                                .child(super::mono(
                                    format!("+{} more", run.len() - 8),
                                    theme::text_faint(),
                                )),
                        );
                    }
                    rows.push((
                        lead,
                        div()
                            .flex()
                            .flex_col()
                            .child(header(&you(&e.author), &what, last, extra))
                            .child(list)
                            .into_any_element(),
                    ));
                }
                EventKind::Comment { .. }
                    if pr::is_bot(&e.author) && !self.expanded.contains(&key) =>
                {
                    let line = self.bot_line(&key, &e.author, e.at, &e.body, cx);
                    rows.push((lead, line));
                }
                EventKind::Comment { .. } => {
                    rows.push((
                        lead,
                        div()
                            .flex()
                            .flex_col()
                            .child(header(&you(&e.author), "commented", e.at, Vec::new()))
                            .child(self.body_card(&key, &e.body, cols, cx))
                            .into_any_element(),
                    ));
                }
                EventKind::Review { state, threads } => {
                    let what = match state.as_str() {
                        "APPROVED" => "approved these changes",
                        "CHANGES_REQUESTED" => "requested changes",
                        "DISMISSED" => "reviewed",
                        _ => "reviewed",
                    };
                    let mut body = div().flex().flex_col().child(header(
                        &you(&e.author),
                        what,
                        e.at,
                        verdict_pill(state).into_iter().collect(),
                    ));
                    if !e.body.trim().is_empty() {
                        body = body.child(self.body_card(&key, &e.body, cols, cx));
                    }
                    for (n, t) in threads.iter().enumerate() {
                        body = body.child(div().mt(px(8.)).child(self.render_thread(
                            &format!("{key}-t{n}"),
                            t,
                            cx,
                        )));
                    }
                    rows.push((lead, body.into_any_element()));
                }
                EventKind::Thread(t) => {
                    rows.push((
                        lead,
                        div()
                            .flex()
                            .flex_col()
                            .child(header(
                                &you(&e.author),
                                "commented on the diff",
                                e.at,
                                Vec::new(),
                            ))
                            .child(div().mt(px(6.)).child(self.render_thread(&key, t, cx)))
                            .into_any_element(),
                    ));
                }
            }
        }

        if let (Some(draft), Details::Pull(p)) = (&self.draft, details) {
            let stale = pr::draft_is_stale(draft, p);
            let who = if draft.author.is_empty() {
                "an agent".to_string()
            } else {
                draft.author.clone()
            };
            let post = format!("Post as {}", verdict_word(draft.verdict));
            let sha: String = draft.head_sha.chars().take(7).collect();
            let busy = self.busy.is_some();
            let editing = self.editing_draft;
            let mut actions = div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .mt(px(10.));
            if editing {
                actions = actions
                    .child(
                        ui::key_pill("gh-draft-save", None, "Save", None, PillKind::Primary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.save_draft_edit(cx))),
                    )
                    .child(
                        ui::key_pill(
                            "gh-draft-cancel",
                            None,
                            "Cancel",
                            None,
                            PillKind::Quiet,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.editing_draft = false;
                            cx.notify();
                        })),
                    );
            } else {
                actions = actions
                    .child(
                        ui::key_pill(
                            "gh-draft-post",
                            None,
                            if self.busy == Some("post-draft") {
                                "Posting…".to_string()
                            } else {
                                post
                            },
                            None,
                            PillKind::Primary,
                            !busy,
                        )
                        .when(!busy, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.post_draft(cx)))
                        }),
                    )
                    .child(
                        ui::key_pill("gh-draft-edit", None, "Edit", None, PillKind::Normal, true)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.edit_draft(window, cx)),
                            ),
                    )
                    .child(
                        ui::key_pill(
                            "gh-draft-discard",
                            None,
                            "Discard",
                            None,
                            PillKind::Quiet,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.discard_draft(cx))),
                    );
            }
            if !sha.is_empty() {
                let (text, color) = if stale {
                    (
                        format!("written for {sha} · new commits since"),
                        theme::accent(),
                    )
                } else {
                    (format!("written for {sha}"), theme::text_faint())
                };
                actions = actions.child(div().ml_auto().child(super::mono(text, color)));
            }
            let content = if editing {
                ui::field()
                    .w_full()
                    .child(self.body_input.clone())
                    .into_any_element()
            } else {
                self.markdown("gh-draft-body", &draft.body, body_style())
            };
            let accent = theme::accent();
            rows.push((
                ui::icon_chip("sparkles", RAIL, Some(accent)).into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(crate::ui::sp(6.))
                            .min_h(px(RAIL))
                            .ty(Type::Label)
                            .text_color(accent)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("Draft review by {who}")),
                            )
                            .child(ui::tag("not posted", accent))
                            .when(draft.created > 0, |d| {
                                d.child(super::mono(
                                    format!("{} ago", super::ago(draft.created)),
                                    theme::text_faint(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .px(crate::ui::sp(16.))
                            .py(crate::ui::sp(12.))
                            .rounded(crate::ui::rad(ui::RADIUS_LG))
                            .border_1()
                            .border_color(accent.opacity(0.25))
                            .bg(accent.opacity(0.04))
                            .child(content)
                            .child(actions),
                    )
                    .into_any_element(),
            ));
        }

        let n = rows.len();
        div()
            .flex()
            .flex_col()
            .px(crate::ui::sp(24.))
            .pt(crate::ui::sp(20.))
            .pb(crate::ui::sp(4.))
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(i, (lead, body))| event(lead, i + 1 == n, body)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_bodies_fold() {
        assert_eq!(estimated_lines("one\ntwo", 80), 2);
        // Blank lines count half; a long line wraps.
        assert_eq!(estimated_lines("a\n\nb", 80), 3);
        assert_eq!(estimated_lines(&"x".repeat(200), 80), 3);
        assert!(!folds("A short note.", 80));
        assert!(!folds(&"line\n".repeat(FOLD_LINES + 2), 80));
        assert!(folds(&"line\n".repeat(30), 80));
        assert!(folds(&"word ".repeat(400), 80));
        // Narrow columns wrap more.
        assert!(folds(&"word ".repeat(150), 30));
        assert!(!folds(&"word ".repeat(150), 120));
    }

    #[test]
    fn a_folded_bot_comment_shows_its_first_words() {
        let body = "<!-- cla -->\n## CLA Assistant\n\n**Thank you** for your `submission`.";
        assert_eq!(first_words(body), "CLA Assistant");
        assert_eq!(first_words("\n\n- [x] signed"), "[x] signed");
        assert_eq!(first_words(""), "");
        assert_eq!(
            first_words("Signed ✅<br/><sub>Posted by the bot.</sub>"),
            "Signed ✅ Posted by the bot."
        );
    }
}
