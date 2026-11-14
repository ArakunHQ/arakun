//! The Timeline tab: what happened on the item, oldest first, on a line of
//! dots, with the agent's draft review as the last event.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::{GitHubView, body_style};
use crate::github::pr::{self, Event, EventKind, LineKind, Thread, Verdict};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, PillKind, Type, Typography as _};

/// How many lines of the diff a thread shows above its comments.
const HUNK_TAIL: usize = 4;

/// One event's place on the line: its dot and the line down to the next.
fn event(dot: Hsla, ring: bool, last: bool, body: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .flex()
        .w_full()
        .min_w_0()
        // The line runs in the event itself: the gutter has no height of its own.
        .when(!last, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(4.))
                    .top(px(14.))
                    .bottom(px(-5.))
                    .w(px(1.))
                    .bg(theme::border_strong()),
            )
        })
        .child(
            div().relative().flex_none().w(px(24.)).child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(5.))
                    .size(px(9.))
                    .rounded_full()
                    .bg(dot)
                    .when(ring, |d| {
                        d.border_2()
                            .border_color(dot.opacity(0.25))
                            .size(px(13.))
                            .left(px(-2.))
                            .top(px(3.))
                    }),
            ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .pb(px(16.))
                .child(body),
        )
}

/// "rik approved · 2h ago": who did what, and when in mono.
fn label(who: &str, what: &str, at: u64, extra: &[String], color: Hsla) -> gpui::Div {
    let mut meta = vec![if at == 0 {
        String::new()
    } else {
        format!("{} ago", super::ago(at))
    }];
    meta.extend(extra.iter().cloned());
    let meta: Vec<String> = meta.into_iter().filter(|m| !m.is_empty()).collect();
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap(px(6.))
        .ty(Type::Label)
        .font_weight(FontWeight::NORMAL)
        .text_color(color)
        .child(format!("{who} {what}").trim().to_string())
        .children(meta.into_iter().map(|m| {
            div()
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_faint())
                .child(format!("· {m}"))
        }))
}

/// A piece of the diff in a bordered block: the file, then numbered lines.
pub(super) fn code_block(path: &str, line: Option<u32>, lines: Vec<AnyElement>) -> gpui::Div {
    div()
        .mt(px(9.))
        .flex()
        .flex_col()
        .rounded(px(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::background())
        .overflow_hidden()
        .child(
            div()
                .flex()
                .justify_between()
                .gap(px(12.))
                .px(px(12.))
                .py(px(7.))
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
        .child(div().flex().flex_col().py(px(6.)).children(lines))
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
        .pr(px(12.))
        .ty(Type::Mono)
        .when_some(tint, |d, t| d.bg(t))
        .when_some(edge, |d, e| {
            d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(e))
        })
        .child(
            div()
                .w(px(42.))
                .flex_none()
                .pr(px(12.))
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
    fn render_thread(&self, key: &str, t: &Thread) -> AnyElement {
        let mut comments = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .mt(px(8.))
            .pl(px(12.))
            .border_l_2()
            .border_color(theme::border_subtle());
        for (i, c) in t.comments.iter().enumerate() {
            comments = comments.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label(
                        &c.user.login,
                        "",
                        crate::ci::parse_time(&c.created_at).unwrap_or(0),
                        &[],
                        theme::text_muted(),
                    ))
                    .child(self.markdown(
                        gpui::SharedString::from(format!("{key}-c{i}")),
                        &c.body,
                        body_style(),
                    )),
            );
        }
        let state = match (t.resolved, t.outdated) {
            (Some(true), _) => Some("resolved"),
            (_, Some(true)) => Some("outdated"),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .child(code_block(&t.path, t.line, hunk_lines(&t.diff_hunk)))
            .when_some(state, |d, s| {
                d.child(div().mt(px(4.)).child(super::mono(s, theme::text_faint())))
            })
            .child(comments)
            .into_any_element()
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
        let mut rows: Vec<(Hsla, bool, AnyElement)> = Vec::new();
        let gray = theme::border_strong();
        let blue: Hsla = theme::link().into();

        let (author, created, body, events, new_oids) = match details {
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
                )
            }
            Details::Issue(i) => (
                i.author.login.clone(),
                i.created_at.clone(),
                i.body.clone(),
                pr::issue_timeline(i),
                Vec::new(),
            ),
        };
        let opened_at = crate::ci::parse_time(&created).unwrap_or(0);
        rows.push((
            gray,
            false,
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(label(
                    &you(&author),
                    "opened",
                    opened_at,
                    &[],
                    theme::text_muted(),
                ))
                .child(if body.trim().is_empty() {
                    ui::text(Type::Body, theme::text_faint(), "No description.").into_any_element()
                } else {
                    self.markdown("gh-ev-open", &body, body_style())
                })
                .into_any_element(),
        ));

        // Commits pushed one after another read as one push.
        let mut i = 0;
        while i < events.len() {
            let e: &Event = &events[i];
            match &e.kind {
                EventKind::Commit { .. } => {
                    let mut run = vec![e];
                    while let Some(next) = events.get(i + run.len()).filter(|n| {
                        matches!(n.kind, EventKind::Commit { .. }) && n.author == e.author
                    }) {
                        run.push(next);
                    }
                    i += run.len();
                    let fresh = run.iter().any(|c| matches!(&c.kind, EventKind::Commit { oid, .. } if new_oids.contains(oid)));
                    let what = if run.len() == 1 {
                        "pushed a commit".to_string()
                    } else {
                        format!("pushed {} commits", run.len())
                    };
                    let extra = if fresh {
                        vec!["new since your review".to_string()]
                    } else {
                        Vec::new()
                    };
                    let last = run.last().map_or(0, |c| c.at);
                    let mut list = div().flex().flex_col().gap(px(2.)).mt(px(3.));
                    for c in run.iter().take(6) {
                        if let EventKind::Commit { oid, headline } = &c.kind {
                            list = list.child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .min_w_0()
                                    .child(
                                        super::mono(
                                            oid.chars().take(7).collect::<String>(),
                                            theme::text_faint(),
                                        )
                                        .flex_none(),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .ty(Type::Body)
                                            .text_color(theme::text())
                                            .child(headline.clone()),
                                    ),
                            );
                        }
                    }
                    if run.len() > 6 {
                        list = list.child(super::mono(
                            format!("+{} more", run.len() - 6),
                            theme::text_faint(),
                        ));
                    }
                    rows.push((
                        gray,
                        false,
                        div()
                            .flex()
                            .flex_col()
                            .child(label(
                                &you(&e.author),
                                &what,
                                last,
                                &extra,
                                theme::text_muted(),
                            ))
                            .child(list)
                            .into_any_element(),
                    ));
                    continue;
                }
                EventKind::Comment { .. } => {
                    let mine = e.author.eq_ignore_ascii_case(me);
                    rows.push((
                        if mine { blue } else { gray },
                        false,
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(label(
                                &you(&e.author),
                                "commented",
                                e.at,
                                &[],
                                theme::text_muted(),
                            ))
                            .child(self.markdown(
                                gpui::SharedString::from(format!("gh-ev-{i}")),
                                &e.body,
                                body_style(),
                            ))
                            .into_any_element(),
                    ));
                }
                EventKind::Review { state, threads } => {
                    let (what, color): (&str, Hsla) = match state.as_str() {
                        "APPROVED" => ("approved", theme::success().into()),
                        "CHANGES_REQUESTED" => ("requested changes", theme::error().into()),
                        "DISMISSED" => ("reviewed (dismissed)", theme::text_faint()),
                        _ => (
                            "reviewed",
                            if e.author.eq_ignore_ascii_case(me) {
                                blue
                            } else {
                                gray
                            },
                        ),
                    };
                    let key = format!("gh-ev-{i}");
                    let mut body = div().flex().flex_col().gap(px(3.)).child(label(
                        &you(&e.author),
                        what,
                        e.at,
                        &[],
                        theme::text_muted(),
                    ));
                    if !e.body.trim().is_empty() {
                        body = body.child(self.markdown(
                            gpui::SharedString::from(key.clone()),
                            &e.body,
                            body_style(),
                        ));
                    }
                    for (n, t) in threads.iter().enumerate() {
                        body = body.child(self.render_thread(&format!("{key}-t{n}"), t));
                    }
                    rows.push((color, false, body.into_any_element()));
                }
                EventKind::Thread(t) => {
                    let key = format!("gh-ev-{i}");
                    rows.push((
                        gray,
                        false,
                        div()
                            .flex()
                            .flex_col()
                            .child(label(
                                &you(&e.author),
                                "commented on the diff",
                                e.at,
                                &[],
                                theme::text_muted(),
                            ))
                            .child(self.render_thread(&key, t))
                            .into_any_element(),
                    ));
                }
            }
            i += 1;
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
            let mut actions = div().flex().items_center().gap(px(8.)).mt(px(10.));
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
                accent,
                true,
                div()
                    .flex()
                    .flex_col()
                    .child(label(
                        &format!("Draft review by {who}"),
                        "",
                        draft.created,
                        &["not posted".into()],
                        accent,
                    ))
                    .child(
                        div()
                            .mt(px(8.))
                            .px(px(13.))
                            .py(px(11.))
                            .rounded(px(ui::RADIUS_LG))
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
            .pl(px(20.))
            .pr(px(22.))
            .pt(px(16.))
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(i, (dot, ring, body))| event(dot, ring, i + 1 == n, body)),
            )
            .into_any_element()
    }
}
