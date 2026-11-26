//! Annotations and review comments waiting above the composer. They go out
//! with the next message; "Submit review" sends them right away.

use std::path::Path;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight,
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
        self.composer
            .update(cx, |c, _| c.draft_mut(env).notes.push(note));
        self.sync_note_marks(env, cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Tells each open transcript of `env` which of its passages have notes.
    pub(super) fn sync_note_marks(&mut self, env: &Path, cx: &mut Context<Self>) {
        let notes = self
            .composer
            .read(cx)
            .draft(env)
            .map(|d| d.notes.clone())
            .unwrap_or_default();
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
            self.composer
                .update(cx, |c, _| c.draft_mut(&env).notes.push(n));
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
                .gap(crate::ui::sp(8.))
                .p(crate::ui::sp(10.))
                .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .font_family(theme::FONT_SANS)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
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
                        .pl(crate::ui::sp(8.))
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
                        .px(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(8.))
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
                        .gap(crate::ui::sp(8.))
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
                                .px(crate::ui::sp(10.))
                                .py(crate::ui::sp(4.))
                                .rounded(crate::ui::rad(8.))
                                .cursor_pointer()
                                .bg(theme::content())
                                .text_color(theme::background())
                                .text_size(px(crate::ui::TEXT_SM))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Add comment")
                                .on_click(cx.listener(|this, _, window, cx| this.finish_review(true, window, cx))),
                        ),
                )
                .map(|d| crate::ui::motion::rise(d, "review-box-in", 6., crate::ui::motion::FAST))
                .into_any_element(),
        )
    }

    /// Review comments waiting for `path` in the visible environment, as line ranges.
    pub(super) fn commented_lines(&self, path: &str, cx: &App) -> Vec<(usize, usize)> {
        self.composer
            .read(cx)
            .draft(&self.env().path)
            .into_iter()
            .flat_map(|d| &d.notes)
            .filter(|n| n.kind == NoteKind::Review && n.path.as_deref() == Some(path))
            .filter_map(|n| n.lines)
            .collect()
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
