//! The Files, Checks and Commits tabs of a pull request.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::timeline::code_line;
use super::{DiffState, GitHubView};
use crate::github::pr::{self, CheckState, FileStatus, Pull};
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

/// Lines a file shows before "Show more".
const FILE_LINES: usize = 200;
/// Past this many lines in all, the files after it start folded.
const OPEN_LINES: usize = 1500;

impl GitHubView {
    pub(super) fn render_files(&self, p: &Pull, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.item() else {
            return div().into_any_element();
        };
        let key = crate::github::queue::key_of(&item);
        let pad = |d: gpui::Div| d.px(px(18.)).py(px(14.));
        let (files, cut) = match self.diffs.get(&key) {
            None | Some(DiffState::Loading) => {
                return ui::loading("gh-diff-loading", "Reading the diff…").into_any_element();
            }
            Some(DiffState::Failed(e)) => {
                return pad(div().flex().flex_col().gap(px(8.)))
                    .child(ui::text(Type::Body, theme::error().into(), e.clone()))
                    .child(
                        div().flex().child(
                            ui::button("gh-diff-retry", "Try again", ui::ButtonKind::Outline, true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.diffs.remove(&key);
                                    if let Some(item) = this.item() {
                                        this.load_diff(item, cx);
                                    }
                                })),
                        ),
                    )
                    .into_any_element();
            }
            Some(DiffState::Ready { files, cut }) => (files.clone(), *cut),
        };
        let mut out = pad(div().flex().flex_col().gap(px(10.)));
        out = out.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(super::mono(
                    format!("{} files", files.len()),
                    theme::text_muted(),
                ))
                .child(super::mono(
                    format!("+{}", p.additions),
                    theme::diff_add().into(),
                ))
                .child(super::mono(
                    format!("−{}", p.deletions),
                    theme::diff_del().into(),
                )),
        );
        if cut {
            out = out.child(ui::text(Type::Caption, theme::accent(), "The diff was too large to read whole: the files after the last one here are missing."));
        }
        let mut seen = 0usize;
        for (fi, f) in files.iter().enumerate() {
            let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
            // GitHub's own counts: they hold when the lines themselves are left out.
            let counts = p
                .files
                .iter()
                .find(|c| c.path == f.path)
                .map_or((f.additions, f.deletions), |c| (c.additions, c.deletions));
            let folded_default = seen > OPEN_LINES;
            seen += lines;
            let folded = folded_default != self.folded.contains(&fi);
            let (letter, color) = match f.status {
                FileStatus::Added => ("A", theme::diff_add().into()),
                FileStatus::Deleted => ("D", theme::diff_del().into()),
                FileStatus::Renamed => ("R", theme::link().into()),
                FileStatus::Modified => ("M", theme::text_subtle()),
            };
            let name = match &f.old_path {
                Some(old) if f.status == FileStatus::Renamed => format!("{old} → {}", f.path),
                _ => f.path.clone(),
            };
            let head = div()
                .id(("gh-file", fi))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(7.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_hover()))
                .when(!folded, |d| {
                    d.border_b_1().border_color(theme::border_subtle())
                })
                .child(
                    icon(
                        if folded {
                            "arrow-right01"
                        } else {
                            "arrow-down01"
                        },
                        12.,
                    )
                    .text_color(theme::text_subtle()),
                )
                .child(ui::tag(letter, color).font_family(theme::FONT_MONO))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(super::mono(name, theme::text())),
                )
                .child(super::mono(
                    format!("+{}", counts.0),
                    theme::diff_add().into(),
                ))
                .child(super::mono(
                    format!("−{}", counts.1),
                    theme::diff_del().into(),
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.folded.remove(&fi) {
                        this.folded.insert(fi);
                    }
                    cx.notify();
                }));
            let mut block = div()
                .flex()
                .flex_col()
                .rounded(px(ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border())
                .bg(theme::background())
                .overflow_hidden()
                .child(head);
            if !folded {
                let mut body = div().flex().flex_col().py(px(4.));
                if f.binary {
                    body = body.child(div().px(px(12.)).py(px(6.)).child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        "Binary file: nothing to show.",
                    )));
                } else if f.hunks.is_empty() {
                    let what = if f.status == FileStatus::Renamed && counts.0 + counts.1 == 0 {
                        "Renamed without changes."
                    } else if counts.0 + counts.1 > 0 {
                        "GitHub doesn't show this file's lines: it is too large."
                    } else {
                        "No changed lines."
                    };
                    body = body.child(div().px(px(12.)).py(px(6.)).child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        what,
                    )));
                }
                let cap = if self.unfolded_lines.contains(&fi) {
                    usize::MAX
                } else {
                    FILE_LINES
                };
                let mut shown = 0usize;
                'hunks: for h in &f.hunks {
                    body = body.child(
                        div()
                            .px(px(12.))
                            .py(px(3.))
                            .bg(theme::fill_raised())
                            .truncate()
                            .child(super::mono(h.header.clone(), theme::text_subtle())),
                    );
                    for l in &h.lines {
                        if shown >= cap {
                            break 'hunks;
                        }
                        body = body.child(code_line(l.kind, l.new_no.or(l.old_no), &l.text));
                        shown += 1;
                    }
                }
                if lines > shown {
                    body = body.child(
                        div()
                            .id(("gh-more", fi))
                            .px(px(12.))
                            .py(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::fill_hover()))
                            .child(ui::text(
                                Type::Label,
                                theme::link().into(),
                                format!("Show {} more lines", lines - shown),
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unfolded_lines.insert(fi);
                                cx.notify();
                            })),
                    );
                }
                block = block.child(body);
            }
            out = out.child(block);
        }
        out.into_any_element()
    }

    pub(super) fn render_checks(&self, p: &Pull, cx: &mut Context<Self>) -> AnyElement {
        let sum = pr::checks_summary(p);
        if sum.checks.is_empty() {
            return ui::empty_state(
                "check-list",
                "No checks",
                "Nothing ran on the last commit of this pull request.",
                None,
            )
            .into_any_element();
        }
        let mut list = div().flex().flex_col().px(px(10.)).py(px(10.));
        for (i, c) in sum.checks.iter().enumerate() {
            let (name, color) = match c.state {
                CheckState::Passed => ("tick02", theme::success().into()),
                CheckState::Failed => ("cancel01", theme::error().into()),
                CheckState::Running => ("loading03", theme::accent()),
                CheckState::Skipped => ("minus-sign", theme::text_subtle()),
            };
            let url = c.url.clone();
            list =
                list.child(
                    div()
                        .id(("gh-check", i))
                        .flex()
                        .items_center()
                        .gap(px(11.))
                        .px(px(8.))
                        .py(px(7.))
                        .rounded(px(ui::RADIUS))
                        .when(!url.is_empty(), |d| {
                            d.cursor_pointer().hover(|s| s.bg(theme::fill_hover()))
                        })
                        .child(ui::icon_chip(name, 26., Some(color)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .ty(Type::Body)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(c.name.clone()),
                                )
                                .when(!c.workflow.is_empty(), |d| {
                                    d.child(div().truncate().child(super::mono(
                                        c.workflow.clone(),
                                        theme::text_subtle(),
                                    )))
                                }),
                        )
                        .when(!url.is_empty(), |d| {
                            d.child(icon("link-square02", 13.).text_color(theme::text_faint()))
                        })
                        .when(!url.is_empty(), |d| {
                            d.on_click(
                                cx.listener(move |this, _, _, cx| this.open_url(url.clone(), cx)),
                            )
                        }),
                );
        }
        list.into_any_element()
    }

    pub(super) fn render_commits(&self, p: &Pull, me: &str, _cx: &mut Context<Self>) -> AnyElement {
        let new: Vec<String> = pr::new_since_my_review(p, me)
            .iter()
            .map(|c| c.oid.clone())
            .collect();
        let mut list = div().flex().flex_col().px(px(18.)).py(px(10.));
        for c in p.commits.iter().rev() {
            let fresh = new.contains(&c.oid);
            list =
                list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .py(px(7.))
                        .border_b_1()
                        .border_color(theme::border_subtle())
                        .child(
                            super::mono(c.short_oid().to_string(), theme::text_subtle())
                                .flex_none(),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .ty(Type::Body)
                                .child(c.message_headline.clone()),
                        )
                        .when(fresh, |d| d.child(ui::pill("new", theme::accent())))
                        .child(
                            div()
                                .flex_none()
                                .ty(Type::Label)
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme::text_muted())
                                .child(c.author().to_string()),
                        )
                        .child(div().w(px(56.)).flex_none().flex().justify_end().child(
                            super::mono(super::ago_iso(&c.committed_date), theme::text_subtle()),
                        )),
                );
        }
        list.into_any_element()
    }
}
