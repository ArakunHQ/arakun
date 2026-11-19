//! The Files, Checks and Commits tabs of a pull request.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};

use std::rc::Rc;

use super::threads::Writing;
use super::timeline::code_line;
use super::{CheckLog, Diff, DiffState, GitHubView};
use crate::ci::{LineKind as LogKind, LogLine};
use crate::github::pr::{self, CheckState, FileDiff, FileStatus, ItemRef, Pull};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

/// Lines a file shows before "Show more".
const FILE_LINES: usize = 200;
/// Room for a file's lines in the ids of the Files list: more than a diff
/// read whole can hold.
const LINE_IDS: usize = 1 << 22;
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
            // The row stays a block, so what is in it gets the list's whole
            // width: the pending bar puts its buttons at the far end.
            let head = div()
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
                .child(self.render_pending_bar(cx))
                .when(*cut, |d| {
                    d.child(ui::text(Type::Caption, theme::text_muted(), "The diff was too large to read whole: the files after the last one here are missing."))
                });
            return row.child(head).into_any_element();
        }
        let fi = ix - 1;
        let (Some(f), Some(&folded_default)) = (files.get(fi), rows.folded_default.get(fi)) else {
            return div().into_any_element();
        };
        // The open threads still on a line of today's diff show under it.
        let threads: Vec<pr::Thread> = if p.inline.iter().any(|c| c.path == f.path) {
            pr::threads(&p.inline)
                .into_iter()
                .filter(|t| t.path == f.path && t.place.is_some() && t.resolved != Some(true))
                .collect()
        } else {
            Vec::new()
        };
        row.child(self.render_file(p, f, fi, folded_default, &threads, cx))
            .into_any_element()
    }

    fn render_file(
        &self,
        p: &Pull,
        f: &FileDiff,
        fi: usize,
        folded_default: bool,
        threads: &[pr::Thread],
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
                    // A removed line is commented on the old side.
                    let place = match l.kind {
                        pr::LineKind::Del => l.old_no.map(|n| (n, true)),
                        _ => l.new_no.map(|n| (n, false)),
                    };
                    let line = code_line(l.kind, l.new_no.or(l.old_no), &l.text);
                    let Some(place) = place else {
                        body = body.child(line);
                        shown += 1;
                        continue;
                    };
                    let path = f.path.clone();
                    body = body.child(
                        div()
                            .id(("gh-line", fi * LINE_IDS + shown))
                            .cursor_pointer()
                            .child(line)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let what = Writing::Line {
                                    path: path.clone(),
                                    line: place.0,
                                    left: place.1,
                                };
                                this.start_writing(what, window, cx)
                            })),
                    );
                    let key = format!("gh-f{fi}-{}{}", if place.1 { "l" } else { "r" }, place.0);
                    body = body.children(self.render_under_line(&key, threads, &f.path, place, cx));
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

    /// Shows or hides the end of a failed check's log, reading it the first
    /// time (and again after a read that failed).
    fn toggle_check_log(&mut self, job: u64, cx: &mut Context<Self>) {
        if !self.checks_open.remove(&job) {
            self.checks_open.insert(job);
            let read = matches!(
                self.check_logs.get(&job),
                Some(CheckLog::Loading | CheckLog::Ready(_))
            );
            if !read && let Some(item) = self.item() {
                self.load_check_log(item, job, cx);
            }
        }
        cx.notify();
    }

    fn load_check_log(&mut self, item: ItemRef, job: u64, cx: &mut Context<Self>) {
        self.check_logs.insert(job, CheckLog::Loading);
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move { pr::check_log(&item, job) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let log = match read {
                    Ok(lines) => CheckLog::Ready(Rc::new(lines)),
                    Err(p) => CheckLog::Failed(p.text()),
                };
                this.check_logs.insert(job, log);
                cx.notify();
            });
        })
        .detach();
    }

    /// "Re-run failed": the failed jobs of every finished run start again.
    fn rerun_failed_checks(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item() else { return };
        let runs = match self.github.read(cx).details(&item) {
            Some(Details::Pull(p)) => {
                pr::rerunnable(&pr::checks_summary(p), &item.repo, &pr::gh_host())
            }
            _ => return,
        };
        let started_on = item.clone();
        self.write(
            "rerun",
            "Failed jobs started again.".into(),
            move |item| {
                pr::rerun_failed(item, &runs)?;
                // GitHub takes a moment to show the new attempt; the read
                // that follows would still see the failed one.
                std::thread::sleep(std::time::Duration::from_secs(3));
                Ok(())
            },
            // The jobs that start now are new ones: no log of theirs is open.
            // Another pull request selected since keeps the logs opened there.
            move |this, _| {
                if this.item().as_ref() == Some(&started_on) {
                    this.checks_open.clear();
                }
            },
            cx,
        );
    }

    /// "Fix with agent": the failed checks, with the logs read so far.
    fn fix_checks(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item() else { return };
        let title = self.title(cx);
        let host = pr::gh_host();
        let prompt = {
            let Some(Details::Pull(p)) = self.github.read(cx).details(&item) else {
                return;
            };
            let sum = pr::checks_summary(p);
            let failed: Vec<(&pr::CheckRun, Option<&[LogLine]>)> = sum
                .checks
                .iter()
                .filter(|c| c.state == CheckState::Failed)
                .map(|c| {
                    let log = match c
                        .failed_job(&item.repo, &host)
                        .and_then(|j| self.check_logs.get(&j))
                    {
                        Some(CheckLog::Ready(lines)) => Some(&lines[..]),
                        _ => None,
                    };
                    (c, log)
                })
                .collect();
            if failed.is_empty() {
                return;
            }
            pr::fix_checks_prompt(&item, &title, p, &failed)
        };
        self.ask(format!("Fix checks: {title}"), prompt, cx);
    }

    /// The line over the checks when some failed: how many, and what to do
    /// about them.
    fn render_checks_head(
        &self,
        p: &Pull,
        sum: &pr::ChecksSummary,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let repo = self.item().map(|i| i.repo).unwrap_or_default();
        let host = pr::gh_host();
        let runs = pr::rerunnable(sum, &repo, &host);
        let (can, why) = if !p.open() {
            (false, "The pull request is not open")
        } else if runs.is_empty() && sum.running > 0 {
            (false, "Wait for the run to finish")
        } else if runs.is_empty() {
            (
                false,
                "These checks aren't GitHub Actions jobs of this repository",
            )
        } else if self.busy.is_some() {
            (false, "Still sending the last action")
        } else {
            (true, "Start the failed jobs again")
        };
        let rerun = ui::button_with(
            "gh-checks-rerun",
            Some("refresh01"),
            if self.busy == Some("rerun") {
                "Starting…"
            } else {
                "Re-run failed"
            },
            ui::ButtonKind::Outline,
            can,
        )
        .tooltip(ui::tip(why))
        .when(can, |b| {
            b.on_click(cx.listener(|this, _, _, cx| this.rerun_failed_checks(cx)))
        });
        let fix = ui::button_with(
            "gh-checks-fix",
            Some("sparkles"),
            "Fix with agent",
            ui::ButtonKind::Outline,
            true,
        )
        .tooltip(ui::tip("Start an agent on the failed checks"))
        .on_click(cx.listener(|this, _, _, cx| this.fix_checks(cx)));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(8.))
            .pb(crate::ui::sp(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .ty(Type::Label)
                    .text_color(theme::error())
                    .child(format!("{} failed", sum.failed)),
            )
            .child(fix)
            .child(rerun)
    }

    /// The end of a failed check's log, under its row.
    fn render_check_log(&self, job: u64, cx: &Context<Self>) -> gpui::Div {
        let block = div()
            .flex()
            .flex_col()
            .mx(crate::ui::sp(8.))
            .mb(crate::ui::sp(6.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::background());
        match self.check_logs.get(&job) {
            None | Some(CheckLog::Loading) => {
                block.child(ui::loading(("gh-check-log", job), "Reading the log…"))
            }
            Some(CheckLog::Failed(e)) => block
                .gap(crate::ui::sp(8.))
                .child(ui::text(Type::Caption, theme::error().into(), e.clone()))
                .child(
                    div().flex().child(
                        ui::button(
                            ("gh-check-log-retry", job),
                            "Try again",
                            ui::ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(item) = this.item() {
                                this.load_check_log(item, job, cx);
                            }
                        })),
                    ),
                ),
            Some(CheckLog::Ready(lines)) if lines.is_empty() => block.child(ui::text(
                Type::Caption,
                theme::text_subtle(),
                "The log is empty.",
            )),
            Some(CheckLog::Ready(lines)) => block.children(lines.iter().map(log_line)),
        }
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
        let repo = self.item().map(|i| i.repo).unwrap_or_default();
        let host = pr::gh_host();
        let mut list = div()
            .flex()
            .flex_col()
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(10.));
        if sum.failed > 0 {
            list = list.child(self.render_checks_head(p, &sum, cx));
        }
        for (i, c) in sum.checks.iter().enumerate() {
            let (name, color) = match c.state {
                CheckState::Passed => ("tick02", theme::success().into()),
                CheckState::Failed => ("cancel01", theme::error().into()),
                CheckState::Running => ("loading03", theme::working()),
                CheckState::Skipped => ("minus-sign", theme::text_subtle()),
            };
            let url = c.url.clone();
            // A failed Actions job opens its log here; its page is one more
            // click, on the link at the end of the row.
            let job = c.failed_job(&repo, &host);
            let open = job.is_some_and(|j| self.checks_open.contains(&j));
            let link = (!url.is_empty()).then(|| {
                let url = url.clone();
                match job {
                    Some(_) => ui::icon_button(("gh-check-link", i), "link-square02", 13.)
                        .tooltip(ui::tip("Open on GitHub"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_url(url.clone(), cx)
                        }))
                        .into_any_element(),
                    None => icon("link-square02", 13.)
                        .text_color(theme::text_faint())
                        .into_any_element(),
                }
            });
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
                        .when(job.is_some() || !url.is_empty(), |d| {
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
                        .when(job.is_some(), |d| {
                            d.child(
                                icon(
                                    if open {
                                        "arrow-down01"
                                    } else {
                                        "arrow-right01"
                                    },
                                    12.,
                                )
                                .text_color(theme::text_subtle()),
                            )
                        })
                        .children(link)
                        .map(|d| match job {
                            Some(job) => d.on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_check_log(job, cx)),
                            ),
                            None if !url.is_empty() => d.on_click(
                                cx.listener(move |this, _, _, cx| this.open_url(url.clone(), cx)),
                            ),
                            None => d,
                        }),
                );
            if let Some(job) = job.filter(|_| open) {
                list = list.child(self.render_check_log(job, cx));
            }
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
                    .when(fresh, |d| d.child(ui::tag("new", theme::text_muted())))
                    .child(
                        super::mono(c.short_oid().to_string(), theme::text_subtle()).flex_none(),
                    ),
            );
        }
        list.into_any_element()
    }
}

/// One line of a job's log, colored by what the runner said it is.
fn log_line(l: &LogLine) -> gpui::Div {
    let (color, text): (Hsla, String) = match l.kind {
        LogKind::Group => (theme::text(), format!("▸ {}", l.text)),
        LogKind::Command => (theme::text_muted(), format!("$ {}", l.text)),
        LogKind::Error => (theme::error().into(), l.text.clone()),
        LogKind::Warning => (theme::warn().into(), l.text.clone()),
        LogKind::Notice => (theme::accent(), l.text.clone()),
        LogKind::Setup => (theme::text_subtle(), l.text.clone()),
        LogKind::Plain => (theme::text_muted(), l.text.clone()),
    };
    div()
        .min_w_0()
        .ty(Type::Mono)
        .line_height(px(18.))
        .text_color(color)
        .child(text.replace('\t', "    "))
}
