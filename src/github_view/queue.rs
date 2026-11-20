//! The Bandeja's menus: refresh, mark read and the watch list, and which
//! organizations and repositories show.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::GitHubView;
use crate::github::GitHub;
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};
use std::collections::BTreeSet;

impl GitHubView {
    /// The header menu: refresh, mark all read, and the watch list.
    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let g: &GitHub = self.github.read(cx);
        // Only what the focus shows: hidden threads would be marked unseen.
        let to_mark = g.unread_in_focus();
        let can_mark = !to_mark.is_empty();
        // The same threads, not just as many: one that left and one that
        // arrived would keep the count.
        let ids: BTreeSet<String> = to_mark.iter().cloned().collect();
        let armed = can_mark && self.confirm_all.as_ref() == Some(&ids);
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
                    .tooltip(ui::tip("Stop notices for this"))
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
                        if this.confirm_all.as_ref() != Some(&ids) {
                            this.confirm_all = Some(ids.clone());
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
            .child(ui::menu_heading("Notices"))
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .pb(crate::ui::sp(6.))
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child("Review requests, mentions and assignments always send you a notice. Add an organization or a repository to get notices for everything there."),
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
                            .rounded(ui::menu_row_radius())
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
                    .rounded(ui::menu_row_radius())
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
                    .child("What is left out doesn't show, count or send a notice."),
            )
            .into_any_element()
    }
}

/// A group's heading in the queue: SMALL CAPS and a count on a chip.
pub(super) fn group_header(label: &str, n: usize) -> gpui::Div {
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
