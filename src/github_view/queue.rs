//! The left top card: what waits on the user, grouped, and the menu with the
//! watch list.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::GitHubView;
use crate::github::queue::{Entry, Group, Lane, Mark, Placed, short_count};
use crate::github::{GitHub, Problem};
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

/// The color of a lane, in the bar, its legend and its rows.
pub(super) fn lane_color(lane: Lane) -> Hsla {
    match lane {
        Lane::OnYou => theme::accent(),
        Lane::OnOthers => theme::link().into(),
        Lane::Ready => theme::success().into(),
        Lane::Done | Lane::None => theme::border_strong(),
    }
}

/// A row's round icon and its color.
pub(super) fn mark_look(mark: Mark, placed: &Placed) -> (&'static str, Hsla) {
    let color = match placed.group {
        Group::NeedsReview => theme::accent(),
        Group::Mentions => theme::link().into(),
        Group::Ready => theme::success().into(),
        Group::Done => theme::pr_merged().into(),
        Group::Yours if placed.lane == Lane::OnYou => theme::accent(),
        Group::Yours => theme::link().into(),
        Group::Other => theme::text_muted(),
    };
    let name = match mark {
        Mark::Pull => "git-pull-request",
        Mark::Issue => "circle-dot",
        Mark::Mention => "at",
        Mark::Merge => "git-merge",
        Mark::Done => "tick02",
        Mark::Closed => "git-pull-request-closed",
        Mark::Ci => "activity01",
        Mark::Other => "notification-off01",
    };
    (name, color)
}

impl GitHubView {
    pub(super) fn render_queue(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let q = g.queue();
        let place = match g.focus() {
            [] => "All repositories".to_string(),
            [one] => one.clone(),
            many => format!("{} places", many.len()),
        };
        let scope = format!("{place} · {} tracked", q.tracked());
        let focused = !g.focus().is_empty();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(11.))
            .px(crate::ui::sp(16.))
            .pt(crate::ui::sp(14.))
            .pb(crate::ui::sp(12.))
            .child(ui::icon_chip("github", 34., None))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(ui::text(Type::Heading, theme::text(), "Review queue"))
                    .child(
                        div()
                            .id("gh-focus")
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .min_w_0()
                            .cursor_pointer()
                            .ty(Type::Label)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(if focused {
                                theme::text_muted()
                            } else {
                                theme::text_subtle()
                            })
                            .hover(|s| s.text_color(theme::text()))
                            .tooltip(ui::tip("Choose which organizations and repositories show"))
                            .child(div().truncate().child(scope))
                            .child(
                                icon("arrow-down01", 11.)
                                    .text_color(theme::text_subtle())
                                    .flex_none(),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.focus_at = if this.focus_at.is_some() {
                                        None
                                    } else {
                                        Some(ev.position + gpui::point(px(-12.), px(14.)))
                                    };
                                    this.menu_at = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            )
            // A narrow column keeps the room for the scope: the bar says it too.
            .when(q.on_you > 0 && self.layout().left_w >= 380., |d| {
                d.child(ui::pill(
                    format!("{} waiting on you", q.on_you),
                    theme::accent(),
                ))
            })
            .child(if g.busy() {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(ui::ICON_BUTTON))
                    .child(ui::spinner("gh-busy", 13., theme::text_faint()))
                    .into_any_element()
            } else {
                ui::icon_button("gh-menu", "more-horizontal", 15.)
                    .tooltip(ui::tip("Refresh, mark read, alerts"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.menu_at = if this.menu_at.is_some() {
                                None
                            } else {
                                Some(ev.position + gpui::point(px(-240.), px(14.)))
                            };
                            this.confirm_all = None;
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            });

        let lanes = [
            ("On you", q.on_you, Lane::OnYou),
            ("On others", q.on_others, Lane::OnOthers),
            ("Ready", q.ready, Lane::Ready),
            ("Done today", q.done, Lane::Done),
        ];
        let bar = ui::segment_bar(&lanes.map(|(_, n, l)| (n as f32, lane_color(l))), 7.)
            .px(crate::ui::sp(16.))
            .mt(px(2.));
        let legend = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_x(crate::ui::sp(14.))
            .gap_y(crate::ui::sp(4.))
            .px(crate::ui::sp(16.))
            .pt(crate::ui::sp(9.))
            .pb(crate::ui::sp(12.))
            .children(lanes.map(|(label, n, lane)| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_muted())
                    .child(
                        div()
                            .size(px(8.))
                            .rounded(crate::ui::rad(2.5))
                            .bg(lane_color(lane)),
                    )
                    .child(label)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(n.to_string()),
                    )
            }));

        let body = self.render_queue_body(cx);
        ui::surface_card()
            .flex_1()
            .child(header)
            .child(bar)
            .child(legend)
            .child(
                ui::inset_panel()
                    .flex_1()
                    .mx(px(10.))
                    .mb(px(10.))
                    .child(body),
            )
            .into_any_element()
    }

    fn render_queue_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let q = g.queue().clone();
        let problem = g.problem().or(g.search_problem()).cloned();
        let loading = !g.loaded(false) || !g.searched();
        if q.rows.is_empty() {
            return match problem {
                Some(Problem::NoGh) => empty(
                    "github",
                    "GitHub CLI not found",
                    "Arakun reads GitHub with gh. Install it (cli.github.com), then run `gh auth login`.",
                ),
                Some(Problem::SignedOut) => empty(
                    "github",
                    "Not signed in to GitHub",
                    "Run `gh auth login` in a terminal, then refresh.",
                ),
                Some(Problem::Other(e)) => empty("alert-circle", "Couldn't read GitHub", &e),
                None if loading => ui::loading("gh-loading", "Reading your queue…")
                    .flex_1()
                    .justify_center()
                    .into_any_element(),
                None => empty(
                    "checkmark-circle01",
                    "Nothing waits on you",
                    "Review requests, mentions and your open pull requests show up here.",
                ),
            };
        }
        let picked = self.picked.as_ref().map(Entry::key);
        let mut list = div()
            .id("gh-queue")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.queue_scroll)
            .pb(crate::ui::sp(6.));
        if let Some(p) = problem {
            // GitHub's answers run long: the first sentence, the rest on hover.
            let full = p.text();
            let short = full
                .split(". ")
                .next()
                .unwrap_or_default()
                .trim_end_matches('.')
                .to_string();
            list = list.child(
                div()
                    .id("gh-problem")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .mx(px(10.))
                    .mt(px(10.))
                    .px(crate::ui::sp(10.))
                    .py(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .bg(theme::error().opacity(0.08))
                    .child(icon("alert-circle", 14.).text_color(theme::error()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ty(Type::Caption)
                            .text_color(theme::error())
                            .line_clamp(2)
                            .child(format!("{short}.")),
                    )
                    .tooltip(ui::tip_text(full)),
            );
        }
        let mut ix = 0;
        for group in Group::ALL.into_iter().filter(|g| *g != Group::Done) {
            if q.count(group) == 0 {
                continue;
            }
            let rows: Vec<&(Entry, Placed)> = q.group(group).collect();
            if rows.is_empty() {
                continue;
            }
            list = list.child(group_header(group.label(), rows.len()));
            for (e, p) in rows {
                list =
                    list.child(self.render_row(ix, e, p, picked.as_deref() == Some(&e.key()), cx));
                ix += 1;
            }
        }
        let done: Vec<&(Entry, Placed)> = q.group(Group::Done).collect();
        if !done.is_empty() {
            let open = self.done_open;
            let last = done
                .first()
                .map(|(e, _)| e.title.clone())
                .unwrap_or_default();
            let newest = done.iter().map(|(e, _)| e.updated()).max().unwrap_or(0);
            list = list.child(
                div()
                    .id("gh-done")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(11.))
                    .mt(px(4.))
                    .px(crate::ui::sp(14.))
                    .py(crate::ui::sp(10.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .child(ui::icon_chip(
                        "tick02",
                        28.,
                        Some(theme::pr_merged().into()),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .gap(crate::ui::sp(5.))
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(
                                        div()
                                            .font_family(theme::FONT_MONO)
                                            .child(done.len().to_string()),
                                    )
                                    .child("done today"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Label)
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme::text_subtle())
                                    .child(format!("Last: {last}")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .child(super::mono(super::ago(newest), theme::text_muted()))
                            .child(
                                icon(if open { "arrow-up01" } else { "arrow-down01" }, 12.)
                                    .text_color(theme::text_subtle()),
                            ),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.done_open = !this.done_open;
                        cx.notify();
                    })),
            );
            if open {
                for (e, p) in done {
                    list = list.child(self.render_row(
                        ix,
                        e,
                        p,
                        picked.as_deref() == Some(&e.key()),
                        cx,
                    ));
                    ix += 1;
                }
            }
        }
        list.into_any_element()
    }

    fn render_row(
        &self,
        ix: usize,
        e: &Entry,
        p: &Placed,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (icon_name, color) = mark_look(p.mark, p);
        let waited = crate::model::unix_now().saturating_sub(e.updated());
        let late = p.lane == Lane::OnYou && waited > 3600;
        let unread = e.unread();
        let entry = e.clone();
        let g = self.github.read(cx);
        let who = if p.author.is_empty() {
            e.author.as_str()
        } else {
            p.author.as_str()
        };
        // A notification about no one (a CI run) shows its kind instead of a face.
        let lead = if who.is_empty() {
            ui::icon_chip(icon_name, 32., Some(color))
        } else {
            ui::avatar_badge(
                who,
                g.avatar(who),
                32.,
                icon_name,
                color,
                if selected {
                    theme::fill_raised()
                } else {
                    theme::background()
                },
            )
        };
        let repo = e.repo();
        let short_repo = repo.rsplit('/').next().unwrap_or_default().to_string();
        let number = e.item.as_ref().map(|i| format!("#{}", i.number));
        let title = e.title.clone();

        let line = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .min_w_0()
            .when(!short_repo.is_empty(), |d| d.child(repo_tag(short_repo)))
            .when_some(number, |d, n| {
                d.child(super::mono(n, theme::text_subtle()).flex_none())
            })
            .when(!p.sub.is_empty(), |d| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child(p.sub.clone()),
                )
            });
        let right_bottom = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .when_some(p.changes, |d, (add, del)| {
                d.child(super::mono(
                    format!("+{}", short_count(add)),
                    theme::diff_add().into(),
                ))
                .child(super::mono(
                    format!("−{}", short_count(del)),
                    theme::diff_del().into(),
                ))
            })
            .when_some(p.checks, |d, c| {
                d.child(
                    div()
                        .size(px(6.))
                        .ml(px(2.))
                        .rounded_full()
                        .bg(super::item::check_color(c)),
                )
            });

        div()
            .id(("gh-row", ix))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(12.))
            .mx(px(6.))
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::fill_raised()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::fill_hover())))
            .when(selected, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(-6.))
                        .top(px(12.))
                        .bottom(px(12.))
                        .w(px(3.))
                        .rounded_full()
                        .bg(theme::accent()),
                )
            })
            .child(lead)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(
                                div()
                                    .id(("gh-row-title", ix))
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Body)
                                    .font_weight(if unread {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .text_color(if unread || selected {
                                        theme::text()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .tooltip(ui::tip_text(title.clone()))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(crate::ui::sp(6.))
                                    .when(unread, |d| {
                                        d.child(div().size(px(6.)).rounded_full().bg(theme::link()))
                                    })
                                    .child(super::mono(
                                        super::ago(e.updated()),
                                        if late {
                                            theme::accent()
                                        } else {
                                            theme::text_subtle()
                                        },
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(div().flex_1().min_w_0().child(line))
                            .child(right_bottom),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select(entry.clone(), true, cx);
                window.focus(&this.focus, cx);
            }))
            .into_any_element()
    }

    /// The header menu: refresh, mark all read, and the watch list.
    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let g: &GitHub = self.github.read(cx);
        let to_mark: Vec<String> = g.list(false).iter().map(|n| n.id.clone()).collect();
        let can_mark = !to_mark.is_empty();
        let armed = can_mark && self.confirm_all == Some(to_mark.len());
        let mark_label = if armed {
            format!("Mark {} read? GitHub can't undo it", to_mark.len())
        } else {
            format!("Mark all {} read", to_mark.len())
        };
        let watch = g.watch().to_vec();
        let mut chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(6.))
            .px(crate::ui::sp(8.));
        for (i, entry) in watch.into_iter().enumerate() {
            let name = entry.clone();
            chips = chips.child(
                ui::chip(("gh-watch", i), entry, false)
                    .child(icon("cancel01", 10.).text_color(theme::text_subtle()))
                    .tooltip(ui::tip("Stop alerting for this"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.github.update(cx, |g, cx| g.remove_watch(&name, cx));
                    })),
            );
        }
        ui::menu_panel("gh-menu-panel", 300.)
            .child(ui::menu_item("gh-refresh", "refresh01", "Refresh", false, true).on_click(cx.listener(|this, _, _, cx| {
                this.menu_at = None;
                this.refresh(cx);
                cx.notify();
            })))
            .child(
                ui::menu_item("gh-mark-all", "tick-double02", mark_label, armed, can_mark).when(can_mark, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        if this.confirm_all != Some(to_mark.len()) {
                            this.confirm_all = Some(to_mark.len());
                            cx.notify();
                            return;
                        }
                        this.confirm_all = None;
                        this.menu_at = None;
                        let ids = to_mark.clone();
                        this.github.update(cx, |g, cx| g.mark_all_read(ids, cx));
                    }))
                }),
            )
            .child(ui::menu_divider())
            .child(ui::menu_heading("Alerts"))
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .pb(crate::ui::sp(6.))
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child("Review requests, mentions and assignments always alert you. Add an organization or a repository to be alerted for everything there."),
            )
            .child(chips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(8.))
                    .pt(crate::ui::sp(6.))
                    .pb(crate::ui::sp(4.))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .h(px(ui::CONTROL_H))
                            .px(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .border_1()
                            .border_color(theme::border_strong())
                            .ty(Type::Label)
                            .font_weight(FontWeight::NORMAL)
                            .child(self.watch_input.clone()),
                    )
                    .child(
                        ui::button("gh-watch-add", "Add", ui::ButtonKind::Outline, true)
                            .h(px(ui::CONTROL_H))
                            .on_click(cx.listener(|this, _, _, cx| this.add_watch(cx))),
                    ),
            )
            .when_some(self.watch_note, |d, note| d.child(div().px(crate::ui::sp(8.)).pb(crate::ui::sp(4.)).ty(Type::Caption).text_color(theme::error()).child(note)))
            .into_any_element()
    }
}

impl GitHubView {
    /// Which owners and repositories the tab shows. Several can be on; none
    /// shows everything.
    pub(super) fn render_focus_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let g: &GitHub = self.github.read(cx);
        let focus = g.focus().to_vec();
        let (owners, repos) = g.scopes();
        let on = |name: &str| focus.iter().any(|f| f.eq_ignore_ascii_case(name));
        // A focus entry with nothing in the queue right now still shows, so it can be turned off.
        let mut owners = owners;
        let mut repos = repos;
        for f in &focus {
            let list = if f.contains('/') {
                &mut repos
            } else {
                &mut owners
            };
            if !list.iter().any(|(k, _)| k.eq_ignore_ascii_case(f)) {
                list.push((f.clone(), 0));
            }
        }
        let row =
            |id: (&'static str, usize), name: String, count: usize, cx: &mut Context<Self>| {
                let picked = on(&name);
                let key = name.clone();
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(ui::MENU_ROW_H))
                    .px(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::selection_hover()))
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(if picked {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .child(div().flex_none().w(px(14.)).when(picked, |d| {
                        d.child(icon("tick02", 14.).text_color(theme::accent()))
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(super::mono(count.to_string(), theme::text_subtle()).flex_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.github
                            .update(cx, |g, cx| g.toggle_focus(Some(&key), cx));
                        this.after_focus_change(cx);
                    }))
            };
        let mut panel = ui::menu_panel("gh-focus-panel", 300.).child(
            ui::menu_item(
                "gh-focus-all",
                if focus.is_empty() { "tick02" } else { "github" },
                "All repositories",
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.github.update(cx, |g, cx| g.toggle_focus(None, cx));
                this.after_focus_change(cx);
            })),
        );
        if !owners.is_empty() {
            panel = panel
                .child(ui::menu_divider())
                .child(ui::menu_heading("Organizations"));
            for (i, (name, n)) in owners.into_iter().enumerate() {
                panel = panel.child(row(("gh-focus-owner", i), name, n, cx));
            }
        }
        if !repos.is_empty() {
            let mut list = div()
                .id("gh-focus-repos")
                .flex()
                .flex_col()
                .max_h(px(280.))
                .overflow_y_scroll();
            for (i, (name, n)) in repos.into_iter().enumerate() {
                list = list.child(row(("gh-focus-repo", i), name, n, cx));
            }
            panel = panel
                .child(ui::menu_divider())
                .child(ui::menu_heading("Repositories"))
                .child(list);
        }
        panel
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .pt(crate::ui::sp(6.))
                    .pb(crate::ui::sp(4.))
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child("What is left out doesn't show, count or alert you."),
            )
            .into_any_element()
    }
}

/// The repository's name on a quiet chip: `arakun`.
fn repo_tag(name: String) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(18.))
        .px(crate::ui::sp(6.))
        .rounded(crate::ui::rad(ui::RADIUS_SM))
        .bg(theme::fill_selected())
        .ty(Type::Caption)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(name)
}

/// A group's heading in the queue: SMALL CAPS and a count on a chip.
fn group_header(label: &str, n: usize) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(8.))
        .px(crate::ui::sp(16.))
        .pt(crate::ui::sp(16.))
        .pb(crate::ui::sp(6.))
        .child(ui::overline(label))
        .child(
            div()
                .flex()
                .items_center()
                .h(px(16.))
                .px(crate::ui::sp(6.))
                .rounded_full()
                .bg(theme::fill_selected())
                .ty(Type::Caption)
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_muted())
                .child(n.to_string()),
        )
}

fn empty(icon_name: &str, title: &str, text: &str) -> AnyElement {
    ui::empty_state(icon_name, title, text, None)
        .flex_1()
        .justify_center()
        .into_any_element()
}
