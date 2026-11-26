//! The Inbox popover: sessions that wait on the user, then every notice the
//! shell has shown, so the ones that came while he was away aren't lost when
//! the island's card leaves.

use super::*;

use crate::island::AlertKind;
use crate::notices::Notice;
use crate::ui::{Type, Typography as _};

const WIDTH: f32 = 380.;
const MAX_H: f32 = 560.;
const PICTURE: f32 = 28.;

impl Shell {
    /// Sessions that are running, waiting for an answer, or failed, newest first.
    pub(super) fn inbox_items(&self) -> Vec<InboxItem> {
        let mut items = Vec::new();
        for (pi, p) in self.projects.iter().enumerate() {
            for (ei, e) in p.environments.iter().enumerate() {
                for sess in &e.sessions {
                    let status = sess.shown_status();
                    if status != Status::Idle {
                        items.push(InboxItem {
                            project: pi,
                            env: ei,
                            id: sess.id.clone(),
                            status,
                            title: sess.title.clone(),
                            linear: sess.linear.clone(),
                            place: format!("{}/{}", p.name, e.name).into(),
                            updated: sess.updated,
                        });
                    }
                }
            }
        }
        // Waiting first, then errors, then running.
        let rank = |s: Status| match s {
            Status::NeedsInput => 0,
            Status::Error => 1,
            _ => 2,
        };
        items.sort_by(|a, b| {
            rank(a.status)
                .cmp(&rank(b.status))
                .then(b.updated.cmp(&a.updated))
        });
        items
    }

    pub(super) fn unseen_notices(&self) -> usize {
        self.notices.borrow().unseen()
    }

    /// Called every frame: once the Inbox closes, what it showed counts as
    /// read. Not on open, so the unread dots stay while the user looks. Not
    /// a notice that came after its last frame either: it was never shown.
    pub(super) fn sync_inbox_seen(&mut self) {
        let open = matches!(
            self.menu,
            Some(ContextMenu {
                target: MenuTarget::Inbox,
                ..
            })
        );
        if self.inbox_open && !open {
            self.clear_armed = false;
            let mut history = self.notices.borrow_mut();
            if history.mark_seen_until(self.inbox_shown.get()) {
                crate::notices::store(&mut history);
            }
        }
        self.inbox_open = open;
    }

    pub(super) fn render_inbox(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.inbox_items();
        let history = self.notices.borrow();
        self.inbox_shown.set(history.arrived());
        let unseen = history.unseen();
        let mut list = div()
            .id("inbox-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb(crate::ui::sp(4.));
        let waiting = !items.is_empty();
        let any_notice = !history.notices.is_empty();
        if waiting {
            list = list.child(section("Waiting on you", items.len()));
            for (i, item) in items.into_iter().enumerate() {
                list = list.child(self.session_row(i, item, cx));
            }
        }
        if waiting && any_notice {
            list = list.child(ui::menu_divider().my(px(6.)));
        }
        if any_notice {
            list = list.child(section("Notices", history.notices.len()));
        } else if !waiting {
            // A popover this small needs one quiet line, not a page's empty state.
            list = list.child(
                ui::text(Type::Body, theme::text_muted(), "No notices yet.")
                    .px(crate::ui::sp(10.))
                    .pt(crate::ui::sp(12.))
                    .pb(crate::ui::sp(8.)),
            );
        }
        let now = model::unix_now();
        for (day, indices) in history.by_day(now) {
            list = list.child(
                ui::text(Type::Caption, theme::text_faint(), day)
                    .px(crate::ui::sp(10.))
                    .pt(crate::ui::sp(8.))
                    .pb(crate::ui::sp(2.)),
            );
            for i in indices {
                list = list.child(notice_row(&history.notices[i], now, cx));
            }
        }
        ui::menu_panel("inbox-menu", WIDTH)
            .max_h(px(MAX_H))
            .min_h_0()
            .p_0()
            .gap_0()
            .child(self.inbox_header(unseen, any_notice, cx))
            .child(list.px(crate::ui::sp(4.)))
    }

    fn inbox_header(&self, unseen: usize, any: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let armed = self.clear_armed;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .pl(crate::ui::sp(14.))
            .pr(crate::ui::sp(8.))
            .h(px(44.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(ui::text(Type::Heading, theme::text(), "Inbox"))
            .when(unseen > 0, |d| {
                d.child(ui::pill(format!("{unseen} new"), theme::text_muted()))
            })
            .child(div().flex_1())
            .when(unseen > 0, |d| {
                d.child(
                    header_action("inbox-read", "Mark all read", false).on_click(cx.listener(
                        |this, _, _, cx| {
                            let mut history = this.notices.borrow_mut();
                            if history.mark_all_seen() {
                                crate::notices::store(&mut history);
                            }
                            drop(history);
                            cx.notify();
                        },
                    )),
                )
            })
            .when(any, |d| {
                let label = if armed {
                    "Click again to clear"
                } else {
                    "Clear"
                };
                d.child(
                    header_action("inbox-clear", label, armed).on_click(cx.listener(
                        |this, _, _, cx| {
                            if this.clear_armed {
                                this.clear_armed = false;
                                {
                                    let mut history = this.notices.borrow_mut();
                                    history.notices.clear();
                                    crate::notices::store(&mut history);
                                }
                                // Cleared is put away: an operator's notice
                                // left unread would show again at the next
                                // word from the account.
                                let unread: Vec<String> = (this.teams.read(cx).notices().iter())
                                    .map(|n| n.id.clone())
                                    .collect();
                                this.teams.update(cx, |t, cx| {
                                    for id in &unread {
                                        t.read_notice(id, cx);
                                    }
                                });
                            } else {
                                this.clear_armed = true;
                            }
                            cx.notify();
                        },
                    )),
                )
            })
    }

    fn session_row(&self, i: usize, item: InboxItem, cx: &mut Context<Self>) -> impl IntoElement {
        let label = match item.status {
            Status::NeedsInput => "Needs you",
            Status::Error => "Failed",
            _ => "Running",
        };
        let (pi, ei, id) = (item.project, item.env, item.id.clone());
        let (issue, title) = super::session_tree::row_title(&item.title, item.linear.as_deref());
        div()
            .id(("inbox-item", i))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(2.))
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(6.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .hover(|s| s.bg(theme::selection_hover()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(status_dot(item.status))
                    .children(issue.map(|issue| {
                        ui::text(Type::Caption, theme::text_faint(), issue).flex_none()
                    }))
                    .child(
                        ui::text(Type::Body, theme::text(), title)
                            .flex_1()
                            .min_w_0()
                            .truncate(),
                    )
                    .child(ui::text(Type::Caption, theme::text_subtle(), label)),
            )
            .child(
                ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    format!("{} · {}", item.place, model::age(item.updated)),
                )
                .pl(crate::ui::sp(14.)),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.menu = None;
                this.open_session_at(pi, ei, id.clone(), window, cx);
            }))
    }
}

/// An overline with how many are under it.
fn section(title: &str, count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(10.))
        .pt(crate::ui::sp(10.))
        .pb(crate::ui::sp(2.))
        .child(ui::overline(title))
        .when(count > 0, |d| {
            d.child(ui::tag(count.to_string(), theme::text_subtle()))
        })
}

fn header_action(id: &'static str, label: &str, danger: bool) -> gpui::Stateful<gpui::Div> {
    let color: Hsla = if danger {
        theme::error().into()
    } else {
        theme::text_subtle()
    };
    div()
        .id(id)
        .flex_none()
        .px(crate::ui::sp(8.))
        .py(crate::ui::sp(3.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .cursor_pointer()
        .text_color(color)
        .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
        .child(ui::text(Type::Label, color, label.to_string()))
}

/// One past notice: what the island showed, and the same "Ver" on click.
fn notice_row(n: &Notice, now: u64, cx: &mut Context<Shell>) -> impl IntoElement {
    let unseen = !n.seen;
    let opens = !n.key.is_empty();
    let (id, key) = (n.id, n.key.clone());
    let title = div()
        .ty(Type::Body)
        .when(unseen, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
        .text_color(if unseen {
            theme::text()
        } else {
            theme::text_muted()
        })
        .truncate()
        .child(crate::redact::text(&n.title).into_owned());
    let age = crate::github::pulse::span(now.saturating_sub(n.at));
    div()
        .id(("inbox-notice", n.id as usize))
        .flex()
        .items_center()
        .gap(crate::ui::sp(10.))
        .px(crate::ui::sp(10.))
        .py(crate::ui::sp(7.))
        .rounded(crate::ui::rad(8.))
        .when(opens, |d| d.cursor_pointer())
        .hover(|s| s.bg(theme::selection_hover()))
        .child(picture(n))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(title)
                .when(!n.subtitle.is_empty(), |d| {
                    d.child(
                        ui::text(
                            Type::Caption,
                            theme::text_subtle(),
                            crate::redact::text(&n.subtitle).into_owned(),
                        )
                        .truncate(),
                    )
                })
                // What an agent sent, in full: the subtitle is only its start.
                .when(!n.body.is_empty(), |d| {
                    d.child(div().pt(crate::ui::sp(4.)).child(ui::text(
                        Type::Caption,
                        theme::text_muted(),
                        crate::redact::text(&n.body).into_owned(),
                    )))
                }),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(6.))
                .self_start()
                .pt(crate::ui::sp(1.))
                .when(unseen, |d| {
                    d.child(div().size(px(6.)).rounded_full().bg(theme::text()))
                })
                .child(ui::text(Type::Mono, theme::text_faint(), age)),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            {
                let mut history = this.notices.borrow_mut();
                // By id, not position: the list may have moved since it was drawn.
                if history.mark_seen(id) {
                    crate::notices::store(&mut history);
                }
            }
            if !key.is_empty() {
                this.menu = None;
                this.open_from_island(key.clone().into(), window, cx);
            }
            cx.notify();
        }))
}

/// The sender's face, ringed in white when a service (GitHub) sent it, as on
/// the island; else the service's logo, else a badge in the kind's color.
fn picture(n: &Notice) -> AnyElement {
    if !n.image.is_empty() {
        let face = ui::avatar_image("", Some(std::path::Path::new(&n.image)), PICTURE);
        if n.brand.is_empty() {
            return face.into_any_element();
        }
        return div()
            .flex_none()
            .p(crate::ui::sp(2.))
            .rounded_full()
            .bg(gpui::white())
            .child(face)
            .into_any_element();
    }
    if n.brand == "github" {
        return div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(PICTURE + 3.))
            .rounded_full()
            .bg(gpui::white())
            .child(ui::icon("github", 18.).text_color(gpui::black()))
            .into_any_element();
    }
    let (name, color): (&str, Option<Hsla>) = match n.kind {
        AlertKind::Finished | AlertKind::Success => ("tick02", Some(theme::success().into())),
        AlertKind::Input | AlertKind::Confirm => ("help-circle", Some(theme::needs_you())),
        AlertKind::Warning => ("alert-circle", Some(theme::attention().into())),
        AlertKind::Error | AlertKind::Failure => ("cancel-circle", Some(theme::error().into())),
        AlertKind::Info => ("message01", None),
    };
    ui::icon_chip(name, PICTURE + 3., color).into_any_element()
}
