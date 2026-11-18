//! Split panes and tab shortcuts. The focused pane always shows the active tab
//! (see `model::Split`); the other pane shows one more tab of the same environment.

use gpui::{AnyElement, Context, IntoElement, MouseButton, Window, canvas, div, px, relative};

use super::*;
use crate::model::{Split, SplitDir};

impl Shell {
    /// Registers the pane and tab actions, and the divider drag, on the window root.
    pub(super) fn pane_handlers(
        &self,
        root: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let dir = self
            .has_projects()
            .then(|| self.env().split.map(|s| s.dir))
            .flatten();
        root.when(self.split_drag, |d| {
            d.when(dir == Some(SplitDir::Right), |d| d.cursor_col_resize())
                .when(dir == Some(SplitDir::Down), |d| d.cursor_row_resize())
                .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _, cx| {
                    this.drag_divider(ev.position, cx)
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.split_drag = false;
                        this.save(cx);
                        cx.notify();
                    }),
                )
        })
        .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
        .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(-1, window, cx)))
        .on_action(
            cx.listener(|this, _: &SelectTab1, window, cx| this.select_tab_number(0, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab2, window, cx| this.select_tab_number(1, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab3, window, cx| this.select_tab_number(2, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab4, window, cx| this.select_tab_number(3, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab5, window, cx| this.select_tab_number(4, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab6, window, cx| this.select_tab_number(5, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab7, window, cx| this.select_tab_number(6, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectTab8, window, cx| this.select_tab_number(7, window, cx)),
        )
        .on_action(cx.listener(|this, _: &SelectLastTab, window, cx| {
            this.select_tab_number(usize::MAX, window, cx)
        }))
        .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
            this.toggle_split(SplitDir::Right, window, cx)
        }))
        .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
            this.toggle_split(SplitDir::Down, window, cx)
        }))
        .on_action(cx.listener(|this, _: &FocusOtherPane, window, cx| {
            this.focus_other_pane(true, window, cx)
        }))
        .on_action(cx.listener(|this, _: &SwapPanes, _, cx| this.swap_panes(cx)))
        .on_action(
            cx.listener(|this, _: &ReopenClosedTab, window, cx| this.reopen_closed_tab(window, cx)),
        )
        .on_action(cx.listener(|this, _: &MoveTabLeft, _, cx| this.move_active_tab(-1, cx)))
        .on_action(cx.listener(|this, _: &MoveTabRight, _, cx| this.move_active_tab(1, cx)))
    }

    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let env = self.env();
        let n = env.tabs.len() as isize;
        if n < 2 {
            return;
        }
        let next = (env.active_tab as isize + delta).rem_euclid(n) as usize;
        self.select_tab(next, window, cx);
        self.focus_active_content(window, cx);
    }

    /// Ctrl+1…8 pick that tab; `usize::MAX` (Ctrl+9) picks the last one.
    fn select_tab_number(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let len = self.env().tabs.len();
        let index = if n == usize::MAX { len - 1 } else { n };
        if index < len && index != self.env().active_tab {
            self.select_tab(index, window, cx);
            self.focus_active_content(window, cx);
        }
    }

    /// Puts keyboard focus where typing makes sense for the active tab, unless a
    /// dock terminal has it (switching tabs from a shell keeps typing in the shell).
    pub(super) fn focus_active_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env();
        if env
            .terminals
            .iter()
            .any(|t| t.view.focus_handle(cx).contains_focused(window, cx))
        {
            return;
        }
        let Some(tab) = env.tabs.get(env.active_tab) else {
            return;
        };
        match &tab.kind {
            TabKind::Session { .. } | TabKind::NewSession => window.focus(&self.composer_focus, cx),
            TabKind::Notes => window.focus(&self.notes_input.focus_handle(cx), cx),
            TabKind::Browser { .. } => self.ensure_active_browser(true, window, cx),
            TabKind::File { .. } | TabKind::Diff { .. } => window.focus(&self.focus, cx),
            #[cfg(feature = "coderlabs")]
            TabKind::Tasks => self
                .tasks_view
                .update(cx, |v, cx| v.focus_input(window, cx)),
            TabKind::Runs => self.runs_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Memory => self.memory_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Cloudflare => self.cloudflare_view.update(cx, |v, cx| v.focus(window, cx)),
            #[cfg(feature = "coderlabs")]
            TabKind::Site => self.site_view.update(cx, |v, cx| v.focus(window, cx)),
            #[cfg(feature = "team")]
            TabKind::Team => self.team_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::PostHog => self.posthog_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Branches => self.branches_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::GitHub => self.github_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Phone => self.phone_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Servers => self.servers_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Teams => self.teams_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Usage => self.usage_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Gallery => self.gallery_view.update(cx, |v, cx| v.focus(window, cx)),
            // The builder takes keys through the window; nothing in it needs focus first.
            TabKind::SidebarBuilder => {}
            TabKind::Isometric => self.iso_view.update(cx, |v, cx| v.focus(window, cx)),
            #[cfg(feature = "coderlabs")]
            TabKind::Canvas => self.canvas_view.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Extensions => self.extensions_view.update(cx, |v, cx| v.focus(window, cx)),
        }
    }

    /// Ctrl+D / Ctrl+Shift+D: splits, turns an existing split the other way, or
    /// unsplits when it already goes that way.
    fn toggle_split(&mut self, dir: SplitDir, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let env = self.env_mut();
        let current = env.split_other().and(env.split.as_ref()).map(|s| s.dir);
        match current {
            Some(d) if d == dir => self.close_split(cx),
            Some(_) => {
                if let Some(split) = env.split.as_mut() {
                    split.dir = dir;
                }
                self.save(cx);
                cx.notify();
            }
            None => self.split_with(None, dir, window, cx),
        }
    }

    /// Shows tab `target` (or, without one, another recent tab) in a focused pane
    /// to the right of or below the active tab.
    pub(super) fn split_with(
        &mut self,
        target: Option<usize>,
        dir: SplitDir,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let env = self.env_mut();
        let Some(active_id) = env.tabs.get(env.active_tab).map(|t| t.id) else {
            return;
        };
        let ratio = env.split.map_or(0.5, |s| s.ratio);
        match target.filter(|t| *t < env.tabs.len()) {
            Some(t) if t == env.active_tab => {
                let other = companion(env);
                let other_id = env.tabs[other].id;
                env.split = Some(Split {
                    other: other_id,
                    dir,
                    ratio,
                    focus_first: false,
                });
            }
            target => {
                let next = target.unwrap_or_else(|| companion(env));
                env.split = Some(Split {
                    other: active_id,
                    dir,
                    ratio,
                    focus_first: false,
                });
                env.active_tab = next;
            }
        }
        self.note_active_tab(cx);
        self.ensure_active_live(window, cx);
        self.ensure_active_browser(false, window, cx);
        self.focus_active_content(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Ctrl+Alt+Shift+arrow: the two panes trade places.
    pub(super) fn swap_panes(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() || self.env().split_other().is_none() {
            return;
        }
        if let Some(split) = self.env_mut().split.as_mut() {
            split.swap();
        }
        self.save(cx);
        cx.notify();
    }

    /// Shows the tab `id` in the first (left or top) pane or the second, splitting if needed.
    /// Dropping the other pane's tab, or the focused one, on the opposite side swaps them.
    pub(super) fn place_tab(
        &mut self,
        id: u64,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lifted_tab = None;
        if !self.has_projects() {
            return;
        }
        if !self.env_mut().place_tab(id, first, companion) {
            cx.notify();
            return;
        }
        self.note_active_tab(cx);
        self.ensure_active_live(window, cx);
        self.ensure_active_browser(false, window, cx);
        self.focus_active_content(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// While a tab is dragged: the two halves of the content (or the two panes) to drop it on.
    pub(super) fn render_tab_drop_zones(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.lifted_tab(cx)?;
        let env = self.env();
        let split = env.split_other().and(env.split);
        let row = split.is_none_or(|s| s.dir == SplitDir::Right);
        let ratio = split.map_or(0.5, |s| s.ratio);
        // Each zone owns its share of the content exactly (a margin would add
        // to the share and push the zones past the middle and the bottom);
        // the inset preview, which takes the drop, sits inside its padding.
        let zone = |first: bool, cx: &mut Context<Self>| {
            div().p(crate::ui::sp(6.)).child(
                div()
                    .id(if first {
                        "tab-drop-first"
                    } else {
                        "tab-drop-second"
                    })
                    .size_full()
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_2()
                    .border_color(gpui::transparent_black())
                    .drag_over::<drag::DraggedTab>(|s, _, _, _| {
                        s.bg(theme::active().opacity(0.12))
                            .border_color(theme::active().opacity(0.55))
                    })
                    .on_drop(cx.listener(move |this, d: &drag::DraggedTab, window, cx| {
                        this.place_tab(d.id, first, window, cx)
                    })),
            )
        };
        let first = zone(true, cx).flex_none().map(|d| {
            if row {
                d.h_full().w(relative(ratio))
            } else {
                d.w_full().h(relative(ratio))
            }
        });
        let second = zone(false, cx)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .map(|d| if row { d.h_full() } else { d.w_full() });
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .when(!row, |d| d.flex_col())
                .child(first)
                .child(second)
                .into_any_element(),
        )
    }

    /// While an image is dragged out of a chat: the right half of the content,
    /// where dropping it opens the image in a tab beside the chat.
    pub(super) fn render_image_drop_zone(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        crate::zoom::lifted_image(cx)?;
        let env_path = self.env().path.clone();
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .child(div().flex_1())
                .child(
                    div().w(relative(0.5)).h_full().p(crate::ui::sp(6.)).child(
                        div()
                            .id("image-drop")
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(crate::ui::rad(ui::RADIUS_LG))
                            .border_2()
                            .border_color(theme::active().opacity(0.25))
                            .bg(theme::background().opacity(0.4))
                            .text_size(px(ui::TEXT_MD))
                            .text_color(theme::text_subtle())
                            .child("Drop to open beside")
                            .drag_over::<crate::zoom::DraggedImage>(|s, _, _, _| {
                                s.bg(theme::active().opacity(0.12))
                                    .border_color(theme::active().opacity(0.55))
                            })
                            .on_drop(cx.listener(
                                move |this, d: &crate::zoom::DraggedImage, window, cx| {
                                    crate::zoom::drop_lifted_image();
                                    this.open_file_beside(&env_path, &d.path, None, window, cx);
                                },
                            )),
                    ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn close_split(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        self.env_mut().split = None;
        self.sync_browsers(cx);
        self.save(cx);
        cx.notify();
    }

    /// Makes the other pane the focused one; `keyboard` also moves keyboard focus there.
    fn focus_other_pane(&mut self, keyboard: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let Some(other) = self.env().split_other() else {
            return;
        };
        // Activating the other pane's tab swaps the panes' roles (`settle_split`).
        self.select_tab(other, window, cx);
        // A click lands on whatever it hit, except the New session placeholder,
        // which turns into the composer and should take the typing.
        let env = self.env();
        let fresh = matches!(
            env.tabs.get(env.active_tab).map(|t| &t.kind),
            Some(TabKind::NewSession)
        );
        if keyboard || fresh {
            self.focus_active_content(window, cx);
        }
    }

    fn drag_divider(&mut self, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let b = self.split_bounds.get();
        let Some(split) = self.env_mut().split.as_mut() else {
            return;
        };
        let (pos, origin, len) = match split.dir {
            SplitDir::Right => (at.x, b.origin.x, b.size.width),
            SplitDir::Down => (at.y, b.origin.y, b.size.height),
        };
        if len.as_f32() <= 0. {
            return;
        }
        split.ratio =
            ((pos - origin).as_f32() / len.as_f32()).clamp(Split::MIN_RATIO, Split::MAX_RATIO);
        cx.notify();
    }

    /// Closes the tabs at the indices `close` picks, never `anchor`; they can be
    /// reopened in turn, leftmost first.
    fn close_tabs(
        &mut self,
        anchor: usize,
        close: impl Fn(usize) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() || anchor >= self.env().tabs.len() {
            return;
        }
        let closed = self.env_mut().remove_tabs(anchor, close);
        let path = self.env().path.clone();
        let mut sessions = Vec::new();
        for (index, tab) in closed.into_iter().rev() {
            if let Some(kind) = reopenable(&tab, cx) {
                self.env_mut().remember_closed(index, kind);
            }
            if let Some(b) = &tab.browser {
                b.view.update(cx, |v, cx| v.close(cx));
            }
            if let TabKind::Session { id, .. } = tab.kind {
                sessions.push(id);
            }
        }
        for id in sessions {
            self.retire_if_hidden(&path, &id, cx);
        }
        // A no-op while a Servers tab is left anywhere.
        self.servers_tab_closed(cx);
        self.note_active_tab(cx);
        self.ensure_active_live(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Opens the tab closed last again, where it was. Tabs that can't come back
    /// (a deleted session) are skipped.
    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        while let Some((index, kind)) = self.env_mut().closed_tabs.pop() {
            let before: Vec<u64> = self.env().tabs.iter().map(|t| t.id).collect();
            if !self.reopen_kind(kind, window, cx) {
                continue;
            }
            let env = self.env_mut();
            let active = env.active_tab;
            // Already open elsewhere: the opener only went to it, so it stays put.
            if let Some(tab) = env.tabs.get_mut(active)
                && !before.contains(&tab.id)
            {
                tab.preview = false;
                env.move_tab(active, index);
            }
            self.note_active_tab(cx);
            self.ensure_active_live(window, cx);
            self.ensure_active_browser(false, window, cx);
            self.focus_active_content(window, cx);
            self.save(cx);
            cx.notify();
            return;
        }
    }

    /// Opens a tab of `kind` the way the rest of the app does; false when it can't.
    fn reopen_kind(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match kind {
            TabKind::Session { id, .. } => {
                if self.env().session_index(&id).is_none() {
                    return false;
                }
                self.open_session(id, window, cx);
            }
            TabKind::Browser { url } => self.open_browser_tab(url.to_string(), window, cx),
            kind @ (TabKind::File { .. } | TabKind::Diff { .. }) => {
                self.open_doc_tab(kind, window, cx)
            }
            TabKind::Notes => self.open_notes_tab(window, cx),
            TabKind::Phone => self.open_phone_tab(window, cx),
            TabKind::Servers => self.open_servers_tab(window, cx),
            TabKind::Teams => self.open_teams_tab(window, cx),
            TabKind::Runs => self.open_runs_tab(None, window, cx),
            TabKind::Branches => self.open_branches_tab(window, cx),
            TabKind::GitHub => self.open_github_tab(window, cx),
            TabKind::Memory => self.open_memory_tab(window, cx),
            TabKind::Cloudflare => self.open_cloudflare_tab(window, cx),
            TabKind::PostHog => self.open_posthog_tab(window, cx),
            TabKind::Usage => self.open_usage_tab(window, cx),
            TabKind::Gallery => self.open_gallery_tab(window, cx),
            TabKind::SidebarBuilder => self.open_sidebar_builder(window, cx),
            TabKind::Isometric => self.open_iso_tab(window, cx),
            TabKind::Extensions => self.open_extensions_tab(window, cx),
            _ => return false,
        }
        true
    }

    /// Moves the active tab one place left (`-1`) or right (`1`).
    fn move_active_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let env = self.env_mut();
        let from = env.active_tab;
        let Some(to) = from.checked_add_signed(delta) else {
            return;
        };
        if env.move_tab(from, to) {
            let at = env.active_tab;
            self.tab_scroll.scroll_to_item(at);
            self.save(cx);
            cx.notify();
        }
    }

    /// A tab dragged onto another in the tab bar takes that tab's place.
    pub(super) fn drop_tab_at(&mut self, id: u64, to: usize, cx: &mut Context<Self>) {
        self.lifted_tab = None;
        if !self.has_projects() {
            return;
        }
        let env = self.env_mut();
        if let Some(from) = env.tabs.iter().position(|t| t.id == id)
            && env.move_tab(from, to)
        {
            self.save(cx);
        }
        cx.notify();
    }

    pub(super) fn render_split(
        &self,
        other: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let env = self.env();
        let Some(split) = env.split else {
            return self.render_pane(env.active_tab, true, window, cx);
        };
        let active = env.active_tab;
        let row = split.dir == SplitDir::Right;
        let (first, second) = if split.focus_first {
            (active, other)
        } else {
            (other, active)
        };
        let bounds = self.split_bounds.clone();

        let mut pane = |index: usize, cx: &mut Context<Self>| {
            let focused = index == active;
            div()
                .id(("pane", index))
                .relative()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(self.render_pane(index, focused, window, cx))
                .when(focused, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(2.))
                            .bg(theme::active().opacity(0.6)),
                    )
                })
                .when(!focused, |d| {
                    d.capture_any_mouse_down(
                        cx.listener(|this, _, window, cx| this.focus_other_pane(false, window, cx)),
                    )
                })
        };
        let first_pane = pane(first, cx).flex_none().map(|d| {
            if row {
                d.h_full().w(relative(split.ratio))
            } else {
                d.w_full().h(relative(split.ratio))
            }
        });
        let second_pane = pane(second, cx)
            .flex_1()
            .map(|d| if row { d.h_full() } else { d.w_full() });
        let line_color = if self.split_drag {
            theme::active()
        } else {
            theme::stroke()
        };
        let line = div().flex_none().bg(line_color).map(|d| {
            if row {
                d.w(px(1.)).h_full()
            } else {
                d.h(px(1.)).w_full()
            }
        });
        // Last child, so it sits above both panes and wins the hit test.
        let handle = div()
            .id("split-resize")
            .absolute()
            .map(|d| {
                if row {
                    d.top_0()
                        .bottom_0()
                        .left(relative(split.ratio))
                        .ml(px(-3.))
                        .w(px(7.))
                        .cursor_col_resize()
                } else {
                    d.left_0()
                        .right_0()
                        .top(relative(split.ratio))
                        .mt(px(-3.))
                        .h(px(7.))
                        .cursor_row_resize()
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if ev.click_count >= 2 {
                        if let Some(s) = this.env_mut().split.as_mut() {
                            s.ratio = 0.5;
                        }
                        this.save(cx);
                    } else {
                        this.split_drag = true;
                    }
                    cx.notify();
                }),
            );

        div()
            .id("split")
            .relative()
            .flex()
            .when(!row, |d| d.flex_col())
            .size_full()
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(first_pane)
            .child(line)
            .child(second_pane)
            .child(handle)
            .into_any_element()
    }

    pub(super) fn render_tab_menu(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = ui::menu_panel("tab-menu", 220.)
            .min_h_0()
            .overflow_y_scroll();
        let env = self.env();
        let Some(tab) = env.tabs.get(index) else {
            return panel.into_any_element();
        };
        let split_open = env.split_other().is_some();
        let several = env.tabs.len() > 1;
        let any_right = index + 1 < env.tabs.len();
        let can_reopen = !env.closed_tabs.is_empty();
        panel =
            panel
                .child(
                    ui::menu_item(
                        "tm-right",
                        "layout-two-column",
                        "Open to the right",
                        false,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.split_with(Some(index), SplitDir::Right, window, cx)
                    })),
                )
                .child(
                    ui::menu_item("tm-down", "layout-two-row", "Open below", false, true).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            this.split_with(Some(index), SplitDir::Down, window, cx)
                        }),
                    ),
                )
                .when(split_open, |d| {
                    let icon = match env.split.map(|s| s.dir) {
                        Some(SplitDir::Down) => "arrow-data-transfer-vertical",
                        _ => "arrow-data-transfer-horizontal",
                    };
                    d.child(
                        ui::menu_item("tm-swap", icon, "Swap sides", false, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                this.swap_panes(cx)
                            }),
                        ),
                    )
                    .child(
                        ui::menu_item("tm-unsplit", "square", "Close split", false, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                this.close_split(cx)
                            }),
                        ),
                    )
                })
                .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
                .child(
                    ui::menu_item("tm-close", "cancel01", "Close tab", false, true).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            this.close_tab(index, window, cx)
                        }),
                    ),
                )
                .child(
                    ui::menu_item(
                        "tm-close-others",
                        "cancel01",
                        "Close other tabs",
                        false,
                        several,
                    )
                    .when(several, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            this.close_tabs(index, |_| true, window, cx)
                        }))
                    }),
                )
                .child(
                    ui::menu_item(
                        "tm-close-right",
                        "cancel01",
                        "Close tabs to the right",
                        false,
                        any_right,
                    )
                    .when(any_right, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            this.close_tabs(index, move |i| i > index, window, cx)
                        }))
                    }),
                )
                .child(
                    ui::menu_item("tm-reopen", "undo", "Reopen closed tab", false, can_reopen)
                        .when(can_reopen, |d| {
                            d.on_click(cx.listener(|this, _, window, cx| {
                                this.menu = None;
                                this.reopen_closed_tab(window, cx)
                            }))
                        }),
                );
        if let TabKind::Session { id, .. } = &tab.kind
            && let Some(si) = env.session_index(id)
        {
            panel = panel.child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()));
            panel = self.session_menu_items(si, panel, cx);
        }
        panel.into_any_element()
    }
}

/// The tab to pair with the active one: the most recently visited other tab, else
/// any other tab, else a fresh New session tab.
fn companion(env: &mut Environment) -> usize {
    let active_id = env.tabs.get(env.active_tab).map(|t| t.id);
    let recent = env
        .tab_back
        .iter()
        .rev()
        .filter(|id| Some(**id) != active_id)
        .find_map(|id| env.tabs.iter().position(|t| t.id == *id));
    if let Some(i) = recent.or_else(|| (0..env.tabs.len()).find(|i| *i != env.active_tab)) {
        return i;
    }
    env.tabs.push(Tab::new("New session", TabKind::NewSession));
    env.tabs.len() - 1
}
