//! Review comments on the diff: answering and resolving a thread, and the
//! comments written on lines of the Files tab, kept until they go out
//! together as one review.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};

use super::{DiffState, GitHubView, NEWLINE, Note, body_style};
use crate::github::pr::{self, LineComment, Thread};
use crate::theme;
use crate::ui::{self, Type, Typography as _};

/// What the thread input is writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Writing {
    /// An answer to the thread with this id (`Thread::id`).
    Reply(u64),
    /// A new comment on a line of the diff.
    Line { path: String, line: u32, left: bool },
}

/// The comments written on lines of one pull request, not sent yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Pending {
    /// The commit whose diff they were written on: a review names one commit.
    pub(super) head: String,
    pub(super) comments: Vec<LineComment>,
}

/// Keeps `comment` with the pending ones of `key`, written on the diff of
/// `head`. Comments on another commit's diff can't join the same review, so
/// after a push the ones there go out (or away) first.
pub(super) fn add_pending(
    pending: &mut HashMap<String, Pending>,
    key: &str,
    head: &str,
    comment: LineComment,
) -> Result<(), &'static str> {
    if head.is_empty() {
        return Err("The diff is still being read: try again in a moment.");
    }
    if comment.body.trim().is_empty() {
        return Err("Write a comment first.");
    }
    let list = pending.entry(key.to_string()).or_default();
    if list.comments.is_empty() {
        list.head = head.to_string();
    } else if list.head != head {
        return Err(
            "New commits arrived after your pending comments. Submit or discard those first.",
        );
    }
    list.comments.push(comment);
    Ok(())
}

/// Whether a line comment started on the diff of `started` can go to the
/// diff shown `now`. After a push the same path and line may be other code:
/// the first Enter says so and takes the new diff, the next one adds it.
pub(super) fn same_diff(started: &mut String, now: &str) -> Result<(), &'static str> {
    if started.is_empty() || started == now {
        return Ok(());
    }
    *started = now.to_string();
    Err(
        "New commits arrived while you wrote. Check this is still the line you mean, then press Enter again.",
    )
}

/// Takes the comments that went out as `sent` from the pending ones of
/// `key`. Comments written while it was sent stay, and so does a whole new
/// batch on another commit, even one with the same words on the same line.
pub(super) fn forget_sent(pending: &mut HashMap<String, Pending>, key: &str, sent: &Pending) {
    let Some(now) = pending.get_mut(key) else {
        return;
    };
    if now.head != sent.head {
        return;
    }
    now.comments.retain(|c| !sent.comments.contains(c));
    if now.comments.is_empty() {
        pending.remove(key);
    }
}

/// "1 pending comment", "3 pending comments".
pub(super) fn pending_label(n: usize) -> String {
    if n == 1 {
        "1 pending comment".into()
    } else {
        format!("{n} pending comments")
    }
}

/// A quiet word that acts on click: "Reply", "Resolve".
fn word(id: impl Into<ElementId>, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .ty(Type::Label)
        .text_color(theme::text_subtle())
        .cursor_pointer()
        .hover(|s| s.text_color(theme::text()))
        .child(label)
}

fn name(key: &str, part: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("{key}-{part}")))
}

impl GitHubView {
    /// The lists measured again: a box opened or closed under one of their rows.
    pub(super) fn relayout(&mut self) {
        self.timeline_list.remeasure();
        self.files_list.remeasure();
    }

    /// The key the pending comments of the item shown are kept under.
    fn pending_key(&self) -> Option<String> {
        self.item().map(|i| crate::github::queue::key_of(&i))
    }

    /// The head of the diff the Files tab shows; empty until it is read.
    fn shown_head(&self) -> String {
        match self.pending_key().and_then(|key| self.diffs.get(&key)) {
            Some(d) if matches!(d.state, DiffState::Ready { .. }) => d.tip.head.clone(),
            _ => String::new(),
        }
    }

    /// The pending comments, when they were written on the diff shown now.
    fn pending_on_shown(&self) -> Option<&Pending> {
        self.pending_here().filter(|p| p.head == self.shown_head())
    }

    pub(super) fn pending_here(&self) -> Option<&Pending> {
        self.pending
            .get(&self.pending_key()?)
            .filter(|p| !p.comments.is_empty())
    }

    /// Opens the thread input for `what` and gives it the keyboard.
    pub(super) fn start_writing(
        &mut self,
        what: Writing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.writing.as_ref() != Some(&what) {
            let placeholder = match what {
                Writing::Reply(_) => "Reply to the thread",
                Writing::Line { .. } => "Comment on this line",
            };
            // From one line to another the words come along: after a push
            // the line they were started on may be gone from the diff, and
            // picking another one is how to place them.
            let moved = matches!(
                (&self.writing, &what),
                (Some(Writing::Line { .. }), Writing::Line { .. })
            );
            self.thread_input.update(cx, |i, cx| {
                if !moved {
                    i.set_text("", cx);
                }
                i.set_placeholder(placeholder, cx);
            });
            self.writing_box += 1;
            if matches!(what, Writing::Line { .. }) {
                self.writing_head = self.shown_head();
            }
            self.writing = Some(what);
            self.relayout();
        }
        window.focus(&self.thread_input.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn stop_writing(&mut self, cx: &mut Context<Self>) {
        if self.writing.take().is_some() {
            self.thread_input.update(cx, |i, cx| i.set_text("", cx));
            self.relayout();
            cx.notify();
        }
    }

    /// Enter in the thread input: the answer goes out, or the line's comment
    /// joins the pending ones.
    pub(super) fn submit_writing(&mut self, cx: &mut Context<Self>) {
        let Some(what) = self.writing.clone() else {
            return;
        };
        let body = self.thread_input.read(cx).text().replace(NEWLINE, "\n");
        if body.trim().is_empty() {
            self.note = Some(Note::quiet("Write a comment first."));
            cx.notify();
            return;
        }
        match what {
            Writing::Reply(id) => {
                if let Some(busy) = self.busy {
                    self.note = Some(Note::quiet(super::busy_note(busy)));
                    cx.notify();
                    return;
                }
                let sent = self.thread_input.read(cx).text().to_string();
                let sent_from = self.writing_box;
                self.write(
                    "reply",
                    "Reply posted.".into(),
                    move |item| pr::reply(item, id, &body).map(|_| ()),
                    move |this, cx| {
                        // Another box may be open by now: only this one
                        // closes, and only with the text that went out. The
                        // box stays open while it is sent, so more may have
                        // been typed.
                        if this.writing == Some(Writing::Reply(id))
                            && this.writing_box == sent_from
                            && this.thread_input.read(cx).text() == sent
                        {
                            this.stop_writing(cx);
                        }
                    },
                    cx,
                );
            }
            Writing::Line { path, line, left } => {
                let Some(key) = self.pending_key() else {
                    return;
                };
                let head = self.shown_head();
                if let Err(why) = same_diff(&mut self.writing_head, &head) {
                    self.note = Some(Note::failed(why));
                    cx.notify();
                    return;
                }
                let comment = LineComment {
                    path,
                    line,
                    left,
                    body,
                };
                match add_pending(&mut self.pending, &key, &head, comment) {
                    Ok(()) => {
                        self.note = None;
                        self.stop_writing(cx);
                    }
                    Err(why) => self.note = Some(Note::failed(why)),
                }
                cx.notify();
            }
        }
    }

    fn set_resolved(&mut self, node: String, resolve: bool, cx: &mut Context<Self>) {
        let done = if resolve {
            "Thread resolved."
        } else {
            "Thread opened again."
        };
        self.write(
            "resolve",
            done.into(),
            move |_| pr::set_thread_resolved(&node, resolve),
            |this, _| this.relayout(),
            cx,
        );
    }

    /// The pending comments of the item shown go out as one review.
    fn submit_pending(&mut self, cx: &mut Context<Self>) {
        let (Some(key), Some(pending)) = (self.pending_key(), self.pending_here().cloned()) else {
            return;
        };
        let sent = pending.clone();
        self.write(
            "line-review",
            match pending.comments.len() {
                1 => "Comment posted.".into(),
                n => format!("{n} comments posted."),
            },
            move |item| pr::review_lines(item, &pending.head, &pending.comments),
            move |this, _| {
                forget_sent(&mut this.pending, &key, &sent);
                this.relayout();
            },
            cx,
        );
    }

    fn discard_pending(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.pending_key() {
            self.pending.remove(&key);
            self.relayout();
            cx.notify();
        }
    }

    fn remove_pending(&mut self, comment: &LineComment, cx: &mut Context<Self>) {
        let Some(key) = self.pending_key() else {
            return;
        };
        if let Some(p) = self.pending.get_mut(&key) {
            p.comments.retain(|c| c != comment);
            if p.comments.is_empty() {
                self.pending.remove(&key);
            }
        }
        self.relayout();
        cx.notify();
    }

    /// The thread input in its box, with what Enter does under it.
    fn render_writing_box(&self, hint: &'static str) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(6.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(theme::raised())
            .child(div().ty(Type::Body).child(self.thread_input.clone()))
            .child(
                div()
                    .ty(Type::Caption)
                    .text_color(theme::text_faint())
                    .child(hint),
            )
    }

    /// The comments of a thread, one under the other.
    pub(super) fn render_thread_comments(
        &self,
        key: &str,
        t: &Thread,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let mut comments = div().flex().flex_col().gap(crate::ui::sp(12.));
        let newest_first = self.github.read(cx).newest_first();
        // Reversed here, not in the thread: its first comment is the one to reply to.
        let mut ordered: Vec<_> = t.comments.iter().enumerate().collect();
        if newest_first {
            ordered.reverse();
        }
        for (i, c) in ordered {
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
                            .child(super::timeline::header(
                                &c.user.login,
                                "",
                                crate::ci::parse_time(&c.created_at).unwrap_or(0),
                                Vec::new(),
                            )),
                    )
                    .child(div().pl(crate::ui::sp(28.)).child(self.markdown(
                        SharedString::from(format!("{key}-c{i}")),
                        &c.body,
                        body_style(),
                    ))),
            );
        }
        comments
    }

    /// Under a thread: "Reply" and "Resolve", or the box the answer is
    /// written in.
    pub(super) fn render_thread_foot(
        &self,
        key: &str,
        t: &Thread,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let id = t.id();
        if self.writing == Some(Writing::Reply(id)) {
            return self.render_writing_box(if self.busy == Some("reply") {
                "Sending…"
            } else {
                "Enter replies · Esc cancels"
            });
        }
        let resolved = t.resolved == Some(true);
        let resolve = t.node.clone().map(|node| {
            word(
                name(key, "resolve"),
                if resolved { "Reopen" } else { "Resolve" },
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.set_resolved(node.clone(), !resolved, cx)),
            )
        });
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(14.))
            .pl(crate::ui::sp(28.))
            .child(word(name(key, "reply"), "Reply").on_click(cx.listener(
                move |this, _, window, cx| this.start_writing(Writing::Reply(id), window, cx),
            )))
            .children(resolve)
    }

    /// What sits under one line of the Files tab: the open threads on it,
    /// the pending comments, and the box when a comment is being written
    /// there. None for a line with nothing under it, which is most of them.
    pub(super) fn render_under_line(
        &self,
        key: &str,
        threads: &[Thread],
        path: &str,
        place: (u32, bool),
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let here: Vec<&Thread> = threads.iter().filter(|t| t.place == Some(place)).collect();
        // Comments written before a push are listed over the files instead:
        // the same line of the new diff may be other code.
        let pending: Vec<&LineComment> = self
            .pending_on_shown()
            .into_iter()
            .flat_map(|p| &p.comments)
            .filter(|c| c.path == path && (c.line, c.left) == place)
            .collect();
        let writing = self.writing
            == Some(Writing::Line {
                path: path.to_string(),
                line: place.0,
                left: place.1,
            });
        if here.is_empty() && pending.is_empty() && !writing {
            return None;
        }
        let card = || {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(10.))
                .px(crate::ui::sp(12.))
                .py(crate::ui::sp(10.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .border_1()
                .border_color(theme::border_subtle())
                .bg(theme::fill_raised())
        };
        let mut under = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(crate::ui::sp(8.))
            .pr(crate::ui::sp(12.))
            .py(crate::ui::sp(8.));
        for t in here {
            let key = format!("{key}-t{}", t.id());
            under = under.child(
                card()
                    .child(self.render_thread_comments(&key, t, cx))
                    .child(self.render_thread_foot(&key, t, cx)),
            );
        }
        for (n, c) in pending.into_iter().enumerate() {
            let gone = c.clone();
            under = under.child(
                card()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(ui::tag("Pending", theme::needs_you()))
                            .child(div().flex_1())
                            .child(
                                word(name(key, &format!("p{}-{n}", place.0)), "Remove").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.remove_pending(&gone, cx)
                                    }),
                                ),
                            ),
                    )
                    .child(self.markdown(
                        SharedString::from(format!("{key}-p{}-{n}-md", place.0)),
                        &c.body,
                        body_style(),
                    )),
            );
        }
        if writing {
            under =
                under.child(self.render_writing_box("Enter adds it to your review · Esc cancels"));
        }
        Some(
            div()
                .flex()
                // Past the line numbers' column, with the code.
                .child(div().flex_none().w(px(super::timeline::LINE_NO_W)))
                .child(under)
                .into_any_element(),
        )
    }

    /// Over the files: how to comment, or the pending comments and what to
    /// do with them.
    pub(super) fn render_pending_bar(&self, cx: &Context<Self>) -> gpui::Div {
        let Some(pending) = self.pending_here() else {
            return div().child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "Click a line to comment on it.",
            ));
        };
        let sending = self.busy == Some("line-review");
        let free = self.busy.is_none();
        let old = self.pending_on_shown().is_none();
        let bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .ty(Type::Label)
                    .text_color(theme::needs_you())
                    .child(pending_label(pending.comments.len())),
            )
            .child(
                ui::button("gh-pending-discard", "Discard", ui::ButtonKind::Ghost, free)
                    .when(free, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.discard_pending(cx)))
                    }),
            )
            .child(
                ui::button(
                    "gh-pending-submit",
                    if sending {
                        "Sending…"
                    } else {
                        "Submit review"
                    },
                    ui::ButtonKind::Primary,
                    free,
                )
                .tooltip(ui::tip("Post them as one review"))
                .when(free, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.submit_pending(cx)))
                }),
            );
        if !old {
            return bar;
        }
        // Written on an earlier commit: shown here with where they were
        // written, since the lines of the diff below are the new commit's.
        let mut list = div().flex().flex_col().gap(crate::ui::sp(6.)).child(bar).child(
            ui::text(
                Type::Caption,
                theme::text_muted(),
                "Written before new commits arrived. They go out on the commit they were written on.",
            ),
        );
        for (n, c) in pending.comments.iter().enumerate() {
            let gone = c.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(super::mono(
                        format!("{}:{}", c.path, c.line),
                        theme::text_muted(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Label)
                            .child(c.body.replace('\n', " ")),
                    )
                    .child(
                        word(name("gh-pending-old", &n.to_string()), "Remove").on_click(
                            cx.listener(move |this, _, _, cx| this.remove_pending(&gone, cx)),
                        ),
                    ),
            );
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(line: u32, body: &str) -> LineComment {
        LineComment {
            path: "src/a.rs".into(),
            line,
            left: false,
            body: body.into(),
        }
    }

    #[test]
    fn pending_comments_are_of_one_commit() {
        let mut pending = HashMap::new();
        // Nothing is kept for a diff that isn't read, or without words.
        assert!(add_pending(&mut pending, "o/r#1", "", on(1, "hi")).is_err());
        assert!(add_pending(&mut pending, "o/r#1", "aaa", on(1, " ")).is_err());
        assert!(pending.get("o/r#1").is_none_or(|p| p.comments.is_empty()));

        assert_eq!(
            add_pending(&mut pending, "o/r#1", "aaa", on(1, "one")),
            Ok(())
        );
        assert_eq!(
            add_pending(&mut pending, "o/r#1", "aaa", on(2, "two")),
            Ok(())
        );
        // After a push the lines are another diff's: the review is sent first.
        let late = add_pending(&mut pending, "o/r#1", "bbb", on(3, "three"));
        assert!(late.unwrap_err().contains("New commits"));
        assert_eq!(pending["o/r#1"].head, "aaa");
        assert_eq!(pending["o/r#1"].comments, [on(1, "one"), on(2, "two")]);
        // Another pull request keeps its own.
        assert_eq!(
            add_pending(&mut pending, "o/r#2", "bbb", on(3, "x")),
            Ok(())
        );
        assert_eq!(pending["o/r#2"].head, "bbb");

        // Once they went out (or were discarded) the next ones take the new head.
        pending.get_mut("o/r#1").unwrap().comments.clear();
        assert_eq!(
            add_pending(&mut pending, "o/r#1", "bbb", on(3, "x")),
            Ok(())
        );
        assert_eq!(pending["o/r#1"].head, "bbb");
    }

    #[test]
    fn a_push_while_a_line_comment_is_written_asks_once() {
        // Started on commit aaa; the diff shown is still that one.
        let mut started = "aaa".to_string();
        assert_eq!(same_diff(&mut started, "aaa"), Ok(()));
        // A push reloaded the diff: the first Enter is turned down.
        assert!(
            same_diff(&mut started, "bbb")
                .unwrap_err()
                .contains("New commits")
        );
        // The second goes to the new diff, and to no pending comment of the old.
        assert_eq!(same_diff(&mut started, "bbb"), Ok(()));
        let mut pending = HashMap::new();
        assert_eq!(
            add_pending(&mut pending, "o/r#1", &started, on(12, "x")),
            Ok(())
        );
        assert_eq!(pending["o/r#1"].head, "bbb");
    }

    #[test]
    fn only_what_was_sent_leaves_the_pending_comments() {
        let batch = |head: &str, comments: &[LineComment]| Pending {
            head: head.into(),
            comments: comments.to_vec(),
        };
        let sent = batch("aaa", &[on(1, "x")]);
        // Nothing changed while it was sent: the list is gone.
        let mut pending = HashMap::from([("k".to_string(), sent.clone())]);
        forget_sent(&mut pending, "k", &sent);
        assert!(pending.is_empty());
        // One more was written meanwhile: it stays.
        let mut pending =
            HashMap::from([("k".to_string(), batch("aaa", &[on(1, "x"), on(2, "y")]))]);
        forget_sent(&mut pending, "k", &sent);
        assert_eq!(pending["k"].comments, [on(2, "y")]);
        // Removed, then written again on the next commit with the same words:
        // that one is new and was not sent.
        let mut pending = HashMap::from([("k".to_string(), batch("bbb", &[on(1, "x")]))]);
        forget_sent(&mut pending, "k", &sent);
        assert_eq!(pending["k"], batch("bbb", &[on(1, "x")]));
    }

    #[test]
    fn pending_comments_are_counted_in_words() {
        assert_eq!(pending_label(1), "1 pending comment");
        assert_eq!(pending_label(4), "4 pending comments");
    }
}
