//! The Files, Checks and Commits tabs of a pull request.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use std::rc::Rc;

use super::timeline::code_line;
use super::{Diff, DiffState, GitHubView};
use crate::github::pr::{self, CheckState, FileDiff, FileStatus, Pull};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

/// Lines a file shows before "Show more".
const FILE_LINES: usize = 200;
/// Past this many lines in all, the files after it start folded.
const OPEN_LINES: usize = 1500;

/// The Files list of one diff: the diff, and which of its files start folded
/// (those past `OPEN_LINES` lines in all).
pub(super) struct FilesRows {
    key: String,
    files: Rc<Vec<FileDiff>>,
    folded_default: Vec<bool>,
}

impl GitHubView {
    /// Works the rows of the Files list out again when the diff changed. The
    /// list builds and lays out only the files on screen: a diff shows up to
    /// 1500 lines open, all of them laid out again on every step of the wheel.
    pub(super) fn sync_files(&mut self) {
        let Some(item) = self.item() else {
            return;
        };
        let key = crate::github::queue::key_of(&item);
        let Some(Diff {
            state: DiffState::Ready { files, .. },
            ..
        }) = self.diffs.get(&key)
        else {
            return;
        };
        if self
            .files_rows
            .as_ref()
            .is_some_and(|r| r.key == key && Rc::ptr_eq(&r.files, files))
        {
            return;
        }
        let mut seen = 0usize;
        let folded_default = files
            .iter()
            .map(|f| {
                let folded = seen > OPEN_LINES;
                seen += f.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
                folded
            })
            .collect();
        let rows = files.len() + 1;
        let same_item = self.files_rows.as_ref().filter(|r| r.key == key);
        if let Some(old) = same_item {
            // A new diff of the same item: keep the place, measure again.
            let old_rows = old.files.len() + 1;
            let keep = old_rows.min(rows);
            self.files_list.splice(keep..old_rows, rows - keep);
            self.files_list.remeasure();
        } else {
            self.files_list.reset(rows);
        }
        self.files_rows = Some(FilesRows {
            key,
            files: files.clone(),
            folded_default,
        });
    }

    pub(super) fn render_files(&self, p: &Pull, cx: &Context<Self>) -> AnyElement {
        let Some(item) = self.item() else {
            return div().into_any_element();
        };
        let key = crate::github::queue::key_of(&item);
        // Files of an older head or base never show beside this head's checks.
        let diff = self
            .diffs
            .get(&key)
            .filter(|d| super::diff_is_current(&d.tip, &p.head_ref_oid, &p.base_ref_name))
            .map(|d| &d.state);
        match diff {
            None | Some(DiffState::Loading) => {
                ui::loading("gh-diff-loading", "Reading the diff…").into_any_element()
            }
            Some(DiffState::Failed(e)) => div()
                .px(crate::ui::sp(18.))
                .py(crate::ui::sp(14.))
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
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
                .into_any_element(),
            Some(DiffState::Ready { .. }) => {
                gpui::list(self.files_list.clone(), cx.processor(Self::render_file_row))
                    .size_full()
                    .into_any_element()
            }
        }
    }

    /// One row of the Files list: the counts first, then a file each.
    fn render_file_row(
        &mut self,
        ix: usize,
        _: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(rows), Some(item)) = (&self.files_rows, self.item()) else {
            return div().into_any_element();
        };
        let Some(Diff {
            state: DiffState::Ready { cut, .. },
            ..
        }) = self.diffs.get(&rows.key)
        else {
            return div().into_any_element();
        };
        let Some(Details::Pull(p)) = self.github.read(cx).details(&item) else {
            return div().into_any_element();
        };
        let files = &rows.files;
        // The rows stand as the files did in one column: 14 above and below,
        // 10 between.
        let row = div()
            .px(crate::ui::sp(18.))
            .pt(crate::ui::sp(if ix == 0 { 14. } else { 10. }))
            .when(ix == files.len(), |d| d.pb(crate::ui::sp(14.)));
        if ix == 0 {
            return row
                .flex()
                .flex_col()
                .gap(crate::ui::sp(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
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
                )
                .when(*cut, |d| {
                    d.child(ui::text(Type::Caption, theme::accent(), "The diff was too large to read whole: the files after the last one here are missing."))
                })
                .into_any_element();
        }
        let fi = ix - 1;
        let (Some(f), Some(&folded_default)) = (files.get(fi), rows.folded_default.get(fi)) else {
            return div().into_any_element();
        };
        row.child(self.render_file(p, f, fi, folded_default, cx))
            .into_any_element()
    }

    fn render_file(
        &self,
        p: &Pull,
        f: &FileDiff,
        fi: usize,
        folded_default: bool,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
        // GitHub's own counts: they hold when the lines themselves are left out.
        let counts = p
            .files
            .iter()
            .find(|c| c.path == f.path)
            .map_or((f.additions, f.deletions), |c| (c.additions, c.deletions));
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
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(7.))
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
                this.files_list.remeasure_items(fi + 1..fi + 2);
                cx.notify();
            }));
        let mut block = div()
            .flex()
            .flex_col()
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::background())
            .overflow_hidden()
            .child(head);
        if !folded {
            let mut body = div().flex().flex_col().py(crate::ui::sp(4.));
            if f.binary {
                body = body.child(div().px(crate::ui::sp(12.)).py(crate::ui::sp(6.)).child(
                    ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        "Binary file: nothing to show.",
                    ),
                ));
            } else if f.hunks.is_empty() {
                let what = if f.status == FileStatus::Renamed && counts.0 + counts.1 == 0 {
                    "Renamed without changes."
                } else if counts.0 + counts.1 > 0 {
                    "GitHub doesn't show this file's lines: it is too large."
                } else {
                    "No changed lines."
                };
                body = body.child(
                    div()
                        .px(crate::ui::sp(12.))
                        .py(crate::ui::sp(6.))
                        .child(ui::text(Type::Caption, theme::text_subtle(), what)),
                );
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
                        .px(crate::ui::sp(12.))
                        .py(crate::ui::sp(3.))
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
                        .px(crate::ui::sp(12.))
                        .py(crate::ui::sp(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::fill_hover()))
                        .child(ui::text(
                            Type::Label,
                            theme::link().into(),
                            format!("Show {} more lines", lines - shown),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.unfolded_lines.insert(fi);
                            this.files_list.remeasure_items(fi + 1..fi + 2);
                            cx.notify();
                        })),
                );
            }
            block = block.child(body);
        }
        block
    }

    pub(super) fn render_checks(&self, p: &Pull, cx: &Context<Self>) -> AnyElement {
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
        let mut list = div()
            .flex()
            .flex_col()
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(10.));
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
                        .gap(crate::ui::sp(11.))
                        .px(crate::ui::sp(8.))
                        .py(crate::ui::sp(7.))
                        .rounded(crate::ui::rad(ui::RADIUS))
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

    pub(super) fn render_commits(&self, p: &Pull, me: &str, cx: &Context<Self>) -> AnyElement {
        let new: Vec<String> = pr::new_since_my_review(p, me)
            .iter()
            .map(|c| c.oid.clone())
            .collect();
        let mut list = div()
            .flex()
            .flex_col()
            .px(crate::ui::sp(16.))
            .py(crate::ui::sp(12.));
        for c in p.commits.iter().rev() {
            let fresh = new.contains(&c.oid);
            let who = c.author().to_string();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(8.))
                    .h(px(44.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .hover(|s| s.bg(theme::fill_hover()))
                    .child(self.face(&who, 24., cx))
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
                                    .child(c.message_headline.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(format!(
                                        "{who} committed {} ago",
                                        super::ago_iso(&c.committed_date)
                                    )),
                            ),
                    )
                    .when(fresh, |d| d.child(ui::tag("new", theme::accent())))
                    .child(
                        super::mono(c.short_oid().to_string(), theme::text_subtle()).flex_none(),
                    ),
            );
        }
        list.into_any_element()
    }
}
