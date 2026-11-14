//! Annotations and review comments waiting above the composer. They go out
//! with the next message; "Submit review" sends them right away.

use std::path::Path;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use super::Shell;
use crate::agent::notes::{ChatNote, NoteKind};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::icon;

/// A review comment being written on lines of a file or diff tab.
pub(super) struct ReviewDraft {
    pub path: String,
    /// Document line indices, first and last.
    pub rows: (usize, usize),
    pub lines: (usize, usize),
    pub quote: String,
    pub input: Entity<TextInput>,
    _sub: Subscription,
}

/// The line each row of a unified diff stands for in the new file. Removed
/// lines get the line they were removed before; headers get none.
pub(super) fn diff_line_numbers(lines: &[SharedString]) -> Vec<Option<usize>> {
    let mut next: Option<usize> = None;
    lines
        .iter()
        .map(|l| {
            if l.starts_with("@@") {
                next = l
                    .split_whitespace()
                    .find(|p| p.starts_with('+'))
                    .and_then(|p| p[1..].split(',').next()?.parse().ok());
                return None;
            }
            let n = next?;
            if l.starts_with("+++") || l.starts_with("---") || l.starts_with('\\') {
                None
            } else if l.starts_with('-') {
                Some(n)
            } else {
                next = Some(n + 1);
                Some(n)
            }
        })
        .collect()
}

impl Shell {
    pub(super) fn add_note(
        &mut self,
        env: &Path,
        note: ChatNote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notes.entry(env.to_path_buf()).or_default().push(note);
        self.sync_note_marks(env, cx);
        window.focus(&self.composer_focus, cx);
        cx.notify();
    }

    fn remove_note(&mut self, ix: usize, cx: &mut Context<Self>) {
        let env = self.env().path.clone();
        if let Some(v) = self.notes.get_mut(&env) {
            if ix < v.len() {
                v.remove(ix);
            }
            if v.is_empty() {
                self.notes.remove(&env);
            }
        }
        self.sync_note_marks(&env, cx);
        cx.notify();
    }

    /// Tells each open transcript of `env` which of its passages have notes.
    pub(super) fn sync_note_marks(&mut self, env: &Path, cx: &mut Context<Self>) {
        let notes = self.notes.get(env).cloned().unwrap_or_default();
        let Some(e) = self.find_env(env) else {
            return;
        };
        let views: Vec<_> = e
            .sessions
            .iter()
            .filter_map(|s| Some((s.id.to_string(), s.live.as_ref()?.view.clone())))
            .collect();
        for (id, view) in views {
            let marks = notes
                .iter()
                .filter_map(|n| n.mark.as_ref())
                .filter(|(sid, _, _)| *sid == id)
                .map(|(_, surface, range)| (*surface, range.clone()))
                .collect();
            view.update(cx, |v, cx| v.set_marks(marks, cx));
        }
    }

    /// Starts a comment on document row `row` of `path`, or with Shift
    /// stretches the open one to it. `numbers` maps rows to file lines.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_review(
        &mut self,
        path: String,
        rows: &[SharedString],
        numbers: &[Option<usize>],
        row: usize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (a, b) = match &self.review_draft {
            Some(d) if extend && d.path == path => (d.rows.0.min(row), d.rows.1.max(row)),
            _ => (row, row),
        };
        let lines_in: Vec<usize> = (a..=b)
            .filter_map(|i| numbers.get(i).copied().flatten())
            .collect();
        let (Some(&first), Some(&last)) = (lines_in.iter().min(), lines_in.iter().max()) else {
            return;
        };
        let quote = rows[a..=b.min(rows.len() - 1)].join("\n");
        let input = match self.review_draft.take() {
            Some(d) if d.path == path => d.input,
            _ => cx.new(|cx| TextInput::new("Comment, Enter adds it to the review", cx)),
        };
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submit | InputEvent::SubmitAlt => this.finish_review(true, window, cx),
                InputEvent::Cancel => this.finish_review(false, window, cx),
                _ => {}
            },
        );
        window.focus(&input.focus_handle(cx), cx);
        self.review_draft = Some(ReviewDraft {
            path,
            rows: (a, b),
            lines: (first, last),
            quote,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    pub(super) fn finish_review(&mut self, add: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.review_draft.take() else {
            return;
        };
        let note = d.input.read(cx).text().trim().to_string();
        if add && !note.is_empty() {
            let env = self.env().path.clone();
            let n = ChatNote {
                kind: NoteKind::Review,
                quote: d.quote,
                note,
                path: Some(d.path),
                lines: Some(d.lines),
                mark: None,
            };
            self.notes.entry(env).or_default().push(n);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The comment box over the bottom of a file or diff tab.
    pub(super) fn render_review_box(
        &self,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let d = self.review_draft.as_ref().filter(|d| d.path == path)?;
        let what = if d.lines.0 == d.lines.1 {
            format!("Comment on line {}", d.lines.0)
        } else {
            format!("Comment on lines {}-{}", d.lines.0, d.lines.1)
        };
        Some(
            div()
                .id("review-box")
                .occlude()
                .absolute()
                .left(px(16.))
                .right(px(16.))
                .bottom(px(16.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(10.))
                .rounded(px(crate::ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .font_family(theme::FONT_SANS)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .child(icon("comment-add01", 13.).text_color(crate::agent::mark_color()))
                        .child(div().text_color(theme::text_muted()).child(what))
                        .child(
                            div()
                                .text_color(theme::text_faint())
                                .child("· Shift+click another line to widen"),
                        ),
                )
                .child(
                    div()
                        .max_h(px(58.))
                        .overflow_hidden()
                        .pl(px(8.))
                        .border_l_2()
                        .border_color(crate::agent::mark_color().opacity(0.6))
                        .font_family(theme::FONT_MONO)
                        .text_size(px(crate::ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(d.quote.lines().take(3).collect::<Vec<_>>().join("\n")),
                )
                .child(
                    div()
                        .h(px(crate::ui::INPUT_H))
                        .flex()
                        .items_center()
                        .px(px(8.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::fill_raised())
                        .text_size(px(crate::ui::TEXT_MD))
                        .child(d.input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(crate::ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .child("Comments wait above the composer until you submit the review · Esc cancels"),
                        )
                        .child(
                            div()
                                .id("review-add")
                                .px(px(10.))
                                .py(px(4.))
                                .rounded(px(8.))
                                .cursor_pointer()
                                .bg(theme::content())
                                .text_color(theme::background())
                                .text_size(px(crate::ui::TEXT_SM))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Add comment")
                                .on_click(cx.listener(|this, _, window, cx| this.finish_review(true, window, cx))),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Review comments waiting for `path` in the visible environment, as line ranges.
    pub(super) fn commented_lines(&self, path: &str) -> Vec<(usize, usize)> {
        self.notes
            .get(&self.env().path)
            .into_iter()
            .flatten()
            .filter(|n| n.kind == NoteKind::Review && n.path.as_deref() == Some(path))
            .filter_map(|n| n.lines)
            .collect()
    }

    pub(super) fn render_note_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notes = self.notes.get(&self.env().path).filter(|v| !v.is_empty())?;
        let annotations = notes
            .iter()
            .filter(|n| n.kind == NoteKind::Annotation)
            .count();
        let reviews = notes.len() - annotations;
        let mut summary = Vec::new();
        if annotations > 0 {
            summary.push(format!(
                "{annotations} annotation{}",
                if annotations == 1 { "" } else { "s" }
            ));
        }
        if reviews > 0 {
            summary.push(format!(
                "{reviews} review comment{}",
                if reviews == 1 { "" } else { "s" }
            ));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(12.))
                .pt(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .h(px(20.))
                                .px(px(8.))
                                .rounded_full()
                                .bg(crate::agent::mark_color().opacity(0.14))
                                .text_size(px(crate::ui::TEXT_XS))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(crate::agent::mark_color())
                                .child(
                                    icon("comment01", 11.).text_color(crate::agent::mark_color()),
                                )
                                .child(summary.join(" · ")),
                        )
                        .child(div().flex_1())
                        .when(reviews > 0, |d| {
                            d.child(
                                div()
                                    .id("submit-review")
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .h(px(24.))
                                    .px(px(8.))
                                    .rounded(px(8.))
                                    .cursor_pointer()
                                    .bg(theme::content())
                                    .text_color(theme::background())
                                    .text_size(px(crate::ui::TEXT_XS))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Submit review")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.submit(window, cx);
                                    })),
                            )
                        })
                        .child(
                            div()
                                .id("notes-clear")
                                .text_size(px(crate::ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text_muted()))
                                .child("Clear")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    let env = this.env().path.clone();
                                    this.notes.remove(&env);
                                    this.sync_note_marks(&env, cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .children(notes.iter().enumerate().map(|(i, n)| {
                            let review = n.kind == NoteKind::Review;
                            div()
                                .id(("note-chip", i))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(26.))
                                .max_w(px(320.))
                                .pl(px(8.))
                                .pr(px(4.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::fill_hover())
                                .text_size(px(crate::ui::TEXT_SM))
                                .child(
                                    icon(if review { "comment-add01" } else { "note01" }, 12.)
                                        .text_color(theme::text_subtle()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme::text_muted())
                                        .child(n.label()),
                                )
                                .child(
                                    div()
                                        .id(("note-remove", i))
                                        .flex_none()
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::fill_selected()))
                                        .child(
                                            icon("cancel01", 11.).text_color(theme::text_subtle()),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.remove_note(i, cx);
                                        })),
                                )
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_diff_rows_by_new_file_line() {
        let lines: Vec<SharedString> = [
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
            "@@ -3,3 +3,4 @@",
            " keep",
            "-old",
            "+new",
            "+more",
            " tail",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        assert_eq!(
            diff_line_numbers(&lines),
            [
                None,
                None,
                None,
                None,
                Some(3),
                Some(4),
                Some(4),
                Some(5),
                Some(6)
            ]
        );
    }
}
