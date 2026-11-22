//! Pinned tabs (`tab_layout::BarShows::Pinned`): the title bar holds the
//! sessions pinned to it, from any project, while the sidebar's tree lists
//! them all. A session is pinned by dragging its row up to the bar, from its
//! row's menu, or with Alt+P; closing its tab or dragging it back unpins it.
//! Tabs skip the island in the middle of the bar, shrink, and the rest go
//! under "N more".

use super::drag::DraggedSession;
use super::session_tree::RowState;
use super::*;

/// A pinned tab's width while the bar has room, and the least it shrinks to.
pub(super) const TAB_W: f32 = 180.;
pub(super) const MIN_TAB_W: f32 = 110.;
/// Room for the "N more" button.
pub(super) const MORE_W: f32 = 84.;
/// The island's width in the middle of the top edge, which tabs keep clear of.
pub(super) const NOTCH_W: f32 = 150.;
/// The space between two tabs.
const GAP: f32 = 2.;

/// How the pinned tabs fit the bar: so many left of the island, so many right
/// of it, all this wide. The rest go under "N more".
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fit {
    pub left: usize,
    pub right: usize,
    pub width: f32,
    /// "N more" goes after the tabs left of the island: there is no island,
    /// or no room for it right of the island.
    pub more_left: bool,
}

impl Fit {
    pub(super) fn shown(&self) -> usize {
        self.left + self.right
    }
}

/// How many tabs `w` wide fit in `room`.
fn fits(room: f32, w: f32) -> usize {
    if room < w {
        0
    } else {
        ((room + GAP) / (w + GAP)).floor() as usize
    }
}

/// Fits `n` tabs into `left` points of bar, and `right` more past the island
/// when the bar has one. Tabs fill the left first, then the right; when both
/// are full they shrink to `MIN_TAB_W`, and then the last half keeps room for
/// "N more".
pub(super) fn fit_tabs(n: usize, left: f32, right: Option<f32>) -> Fit {
    let place = |w: f32, left: f32, right: Option<f32>| {
        let l = fits(left, w).min(n);
        let r = right.map_or(0, |room| fits(room, w)).min(n - l);
        (l, r)
    };
    let mut w = TAB_W;
    while w >= MIN_TAB_W {
        let (l, r) = place(w, left, right);
        if l + r >= n {
            return Fit {
                left: l,
                right: r,
                width: w,
                more_left: right.is_none(),
            };
        }
        w -= 2.;
    }
    // "N more" ends the half it fits in: right of the island when it can.
    let (more_left, (l, r)) = match right {
        Some(room) if room >= MORE_W => (false, place(MIN_TAB_W, left, Some(room - MORE_W))),
        Some(room) => (true, place(MIN_TAB_W, left - MORE_W, Some(room))),
        None => (true, place(MIN_TAB_W, left - MORE_W, None)),
    };
    Fit {
        left: l,
        right: r,
        width: MIN_TAB_W,
        more_left,
    }
}

/// Keeps the tab at `active` among the first `shown` of `order`: past them,
/// it takes the last place shown, and the tab there goes under "N more".
pub(super) fn bring_into_view(order: &mut Vec<usize>, shown: usize, active: usize) {
    if shown == 0 {
        return;
    }
    if let Some(p) = order.iter().position(|&i| i == active)
        && p >= shown
    {
        let tab = order.remove(p);
        order.insert(shown - 1, tab);
    }
}

/// Where the island sits in the bar, as the room left of it and right of it
/// from the bar's start, or `None` when it is not over the bar. `bar` is the
/// bar's start and width in window points, `notch` the island's middle.
pub(super) fn split_room(bar: (f32, f32), notch: Option<f32>) -> (f32, Option<f32>) {
    let (x, w) = bar;
    let Some(mid) = notch else {
        return (w, None);
    };
    let (l, r) = (mid - NOTCH_W / 2. - x, mid + NOTCH_W / 2. - x);
    if r <= 0. || l >= w {
        return (w, None);
    }
    (l.max(0.), Some((w - r).max(0.)))
}

impl Shell {
    /// The island's middle in window points while it sits over the title
    /// bar: on, and the window spread over its screen's top edge. A window
    /// on Wayland can't tell where it is otherwise.
    pub(super) fn bar_notch(&self, window: &Window) -> Option<f32> {
        let over = cfg!(any(target_os = "linux", target_os = "macos"))
            && crate::island::enabled()
            && (window.is_maximized() || window.is_fullscreen());
        over.then(|| f32::from(window.viewport_size().width) / 2.)
    }

    /// Uses `layout` from Settings. Moving to a pinned bar pins every open
    /// session first, so no tab disappears; a preset that needs the tree
    /// widens a sidebar too narrow for it.
    pub(super) fn apply_tab_layout(
        &mut self,
        layout: crate::tab_layout::TabLayout,
        preset: Option<crate::tab_layout::Preset>,
        cx: &mut Context<Self>,
    ) {
        let was = crate::tab_layout::current();
        if layout.pinned_bar() && !was.pinned_bar() {
            for t in &mut self.strip.tabs {
                if t.session_id().is_some() {
                    t.pinned = true;
                }
            }
        }
        crate::tab_layout::set(layout);
        // The tree needs its width: when it comes, or when a preset is picked.
        let min = preset
            .and_then(|p| p.min_sidebar_w())
            .or((layout.tree() && !was.tree()).then_some(crate::tab_layout::SIDEBAR_W));
        if let Some(min) = min
            && self.rail_w < min
        {
            self.rail_w = min;
        }
        self.save(cx);
        cx.notify();
    }

    /// The project and environment that hold session `id`.
    pub(super) fn session_place(&self, id: &str) -> Option<(usize, usize)> {
        self.projects.iter().enumerate().find_map(|(pi, p)| {
            p.environments
                .iter()
                .position(|e| e.session_index(id).is_some())
                .map(|ei| (pi, ei))
        })
    }

    pub(super) fn is_pinned(&self, id: &str) -> bool {
        self.strip
            .session_tab(id)
            .is_some_and(|i| self.strip.tabs[i].pinned)
    }

    /// Pins session `id` of environment `ei` in project `pi` to the bar, its
    /// tab going into the gap before the tab at `gap` (the end when `None`),
    /// or unpins it. An unpinned tab closes unless it is on screen.
    pub(super) fn set_pinned(
        &mut self,
        pi: usize,
        ei: usize,
        id: SharedString,
        pinned: bool,
        gap: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.projects.get(pi).and_then(|p| p.environments.get(ei)) else {
            return;
        };
        let path = env.path.clone();
        if !pinned {
            if let Some(i) = self.strip.session_tab(&id) {
                let tab = self.strip.tabs[i].id;
                self.unpin_tab(tab, cx);
            }
            return;
        }
        let tab = match self.strip.session_tab(&id) {
            Some(i) => self.strip.tabs[i].id,
            None => {
                let Some(s) = env
                    .sessions
                    .get(env.session_index(&id).unwrap_or(usize::MAX))
                else {
                    return;
                };
                let tab = Tab::of(
                    &path,
                    s.title.clone(),
                    TabKind::Session {
                        agent: s.agent,
                        id: id.clone(),
                    },
                );
                let tab_id = tab.id;
                // At the end of the bar; pushing never moves the active tab.
                self.strip.tabs.push(tab);
                tab_id
            }
        };
        if let Some(i) = self.strip.position(tab) {
            self.strip.tabs[i].pinned = true;
        }
        if let Some(gap) = gap {
            self.strip.move_to_gap(tab, gap);
        }
        self.save(cx);
        cx.notify();
    }

    /// Takes tab `tab` off the bar. A session tab not on screen closes; its
    /// session stays in the tree.
    pub(super) fn unpin_tab(&mut self, tab: u64, cx: &mut Context<Self>) {
        let Some(i) = self.strip.position(tab) else {
            return;
        };
        self.strip.tabs[i].pinned = false;
        self.settle_pins(cx);
        self.save(cx);
        cx.notify();
    }

    /// Alt+P: pins or unpins the session under the pointer in the tree, else
    /// the tab on screen.
    pub(super) fn toggle_pin_here(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() || !crate::tab_layout::current().tree() {
            return;
        }
        if let Some((pi, ei, id)) = self.tree_hover.clone() {
            let pinned = self.is_pinned(&id);
            self.set_pinned(pi, ei, id, !pinned, None, cx);
            return;
        }
        let Some(tab) = self.strip.active() else {
            return;
        };
        if tab.session_id().is_none() {
            return;
        }
        let (id, pinned) = (tab.id, tab.pinned);
        if pinned {
            self.unpin_tab(id, cx);
        } else if let Some(i) = self.strip.position(id) {
            self.strip.tabs[i].pinned = true;
            self.save(cx);
            cx.notify();
        }
    }

    /// With pinned tabs, closes the session tabs that show nowhere, and pins
    /// the session on screen when nothing is pinned. Run after the bar may
    /// have changed: render queues it.
    pub(super) fn settle_pins(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() || !crate::tab_layout::current().pinned_bar() {
            return;
        }
        // Only the first session of a fresh window pins itself; after that
        // an unpinned bar is the user's choice.
        let mut changed = !self.pins_seeded && self.strip.pin_lone();
        if self.strip.tabs.iter().any(|t| t.pinned) {
            self.pins_seeded = true;
        }
        let keep = self.split_other().map(|i| self.strip.tabs[i].id);
        let strays = self.strip.strays(keep);
        if !strays.is_empty() {
            let gone = self.strip.remove_where(|t| strays.contains(&t.id));
            for tab in gone {
                if let (Some(env), Some(id)) = (tab.env.clone(), tab.session_id().cloned()) {
                    self.retire_if_hidden(&env, &id, cx);
                }
            }
            changed = true;
        }
        if changed {
            self.save(cx);
            cx.notify();
        }
    }

    /// Whether `settle_pins` has something to do.
    pub(super) fn pins_unsettled(&self) -> bool {
        if !self.has_projects() || !crate::tab_layout::current().pinned_bar() {
            return false;
        }
        let keep = self.split_other().map(|i| self.strip.tabs[i].id);
        let any_pinned = self.strip.tabs.iter().any(|t| t.pinned);
        let lone = self
            .strip
            .active()
            .is_some_and(|t| t.session_id().is_some());
        !self.strip.strays(keep).is_empty() || (!self.pins_seeded && (any_pinned || lone))
    }

    /// A tab's state, for the mark a pinned tab shows.
    pub(super) fn tab_state(&self, tab: &Tab, cx: &App) -> Option<RowState> {
        let id = tab.session_id()?;
        let (pi, ei) = self.session_place(id)?;
        Some(self.session_state(pi, ei, id, cx))
    }

    /// Unpins the pinned sessions that are done: idle, or their checks passed.
    pub(super) fn unpin_done(&mut self, cx: &mut Context<Self>) {
        let active = self.strip.active().map(|t| t.id);
        let done: Vec<u64> = self
            .strip
            .tabs
            .iter()
            .filter(|t| t.pinned && Some(t.id) != active)
            .filter(|t| {
                matches!(
                    self.tab_state(t, cx),
                    Some(RowState::Idle | RowState::Passed)
                )
            })
            .map(|t| t.id)
            .collect();
        for t in &done {
            if let Some(i) = self.strip.position(*t) {
                self.strip.tabs[i].pinned = false;
            }
        }
        self.settle_pins(cx);
        self.save(cx);
        cx.notify();
    }

    /// A session row dropped on the bar: pinned into the gap marked.
    pub(super) fn drop_session_on_bar(
        &mut self,
        d: &DraggedSession,
        gap: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.lifted = None;
        let gap = self.tab_drop.take().or(gap);
        if let Some((pi, ei)) = self.session_place(&d.id) {
            self.set_pinned(pi, ei, d.id.clone(), true, gap, cx);
        }
    }

    /// The pinned tabs that did not fit, with "Unpin the ones that are done".
    pub(super) fn render_more_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let more = self.bar_more.borrow().clone();
        let layout = crate::tab_layout::current();
        let mut panel = ui::menu_panel("more-tabs", 280.)
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(4.))
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(format!("{} MORE PINNED", more.len())),
            );
        for (n, id) in more.into_iter().enumerate() {
            let Some(i) = self.strip.position(id) else {
                continue;
            };
            let tab = &self.strip.tabs[i];
            let mark = self
                .tab_state(tab, cx)
                .and_then(|s| s.mark(layout.state))
                .unwrap_or_else(|| self.tab_icon(tab, cx));
            let title = crate::redact::text(&tab.title).into_owned();
            panel = panel.child(
                div()
                    .id(("more-tab", n))
                    .map(|d| ui::labelled(d, gpui::Role::MenuItem, title.clone()))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(ui::MENU_ROW_H))
                    .px(crate::ui::sp(8.))
                    .rounded(ui::menu_row_radius())
                    .cursor_pointer()
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text_muted())
                    .hover(|s| s.bg(theme::selection_hover()).text_color(theme::content()))
                    .child(mark)
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .children(self.tab_project_icon(tab))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        if let Some(i) = this.strip.position(id) {
                            this.select_tab(i, window, cx);
                        }
                    })),
            );
        }
        panel
            .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
            .child(
                ui::menu_item(
                    "more-unpin-done",
                    "pin-off",
                    "Unpin the ones that are done",
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.menu = None;
                    this.unpin_done(cx)
                })),
            )
            .into_any_element()
    }

    /// A tree row's menu: pin, open, and the session's own actions.
    pub(super) fn render_tree_menu(
        &self,
        pi: usize,
        ei: usize,
        si: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut panel = ui::menu_panel("tree-menu", 260.)
            .min_h_0()
            .overflow_y_scroll();
        let Some(env) = self.projects.get(pi).and_then(|p| p.environments.get(ei)) else {
            return panel.into_any_element();
        };
        let Some(s) = env.sessions.get(si) else {
            return panel.into_any_element();
        };
        let id = s.id.clone();
        let pinned = self.is_pinned(&id);
        let (pin, open) = (id.clone(), id.clone());
        panel = panel
            .child(
                ui::menu_item(
                    "tree-pin",
                    if pinned { "pin-off" } else { "pin" },
                    if pinned {
                        "Unpin from the title bar"
                    } else {
                        "Pin to the title bar"
                    },
                    false,
                    true,
                )
                .child(ui::shortcut(&["Alt", "P"]))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.set_pinned(pi, ei, pin.clone(), !pinned, None, cx)
                })),
            )
            .child(
                ui::menu_item("tree-open", "arrow-turn-forward", "Open", false, true).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.open_session_at(pi, ei, open.clone(), window, cx)
                    }),
                ),
            )
            .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()));
        // The session's own actions work on the environment on screen.
        if self.env().path == env.path {
            return self.session_menu_items(si, panel, cx).into_any_element();
        }
        let path = env.path.clone();
        let can_archive = !s.settled && s.status != Status::Working && env.remote.is_none();
        panel
            .child(
                ui::menu_item("tree-archive", "archive02", "Archive", false, can_archive).when(
                    can_archive,
                    |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.archive_sessions(&path, &[id.to_string()], cx);
                        }))
                    },
                ),
            )
            .into_any_element()
    }

    /// The tree as a menu, with a search, for the Sessions button the bar
    /// shows while the sidebar is hidden.
    pub(super) fn render_sessions_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        ui::menu_panel("sessions-menu", 340.)
            .max_h(px(560.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(8.))
                    .h(px(ui::INPUT_H))
                    .rounded(crate::ui::rad(8.))
                    .bg(theme::fill_raised())
                    .child(icon("search01", 14.).text_color(theme::text_subtle()))
                    .child(div().flex_1().min_w_0().child(self.tree_filter.clone())),
            )
            .child(
                div()
                    .id("sessions-menu-list")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .child(self.render_session_tree(true, false, cx)),
            )
            .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child("Alt+P pins the session under the pointer"),
            )
            .into_any_element()
    }

    /// The Sessions button: the tree as a menu while the sidebar is hidden,
    /// with how many sessions need you.
    pub(super) fn render_sessions_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let waiting = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .filter(|s| !s.settled && s.shown_status() == Status::NeedsInput)
            .count();
        let label = match waiting {
            0 => "Sessions".to_string(),
            n => format!("Sessions, {n} need you"),
        };
        div()
            .id("title-sessions")
            .map(|d| ui::labelled(d, gpui::Role::Button, label))
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(ui::TAB_H))
            .px(crate::ui::sp(8.))
            .mr(crate::ui::sp(2.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_MD))
            .text_color(theme::text_muted())
            .bg(theme::fill_raised())
            .hover(|s| s.bg(theme::hover()).text_color(theme::content()))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(icon("left-to-right-list-bullet", 14.))
            .child("Sessions")
            .when(waiting > 0, |d| {
                d.child(status_dot(Status::NeedsInput)).child(
                    div()
                        .text_color(theme::needs_you())
                        .child(waiting.to_string()),
                )
            })
            .child(icon("arrow-down01", 12.).text_color(theme::text_faint()))
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| {
                let at = ev.position();
                this.menu = Some(ContextMenu {
                    target: MenuTarget::Sessions,
                    at: point(at.x, px(44.)),
                });
                let focus = this.tree_filter.focus_handle(cx);
                window.focus(&focus, cx);
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_keep_their_width_while_the_bar_has_room() {
        assert_eq!(
            fit_tabs(3, 1000., None),
            Fit {
                left: 3,
                right: 0,
                width: TAB_W,
                more_left: true,
            }
        );
        // Left of the island first, then right of it.
        let fit = fit_tabs(5, 400., Some(600.));
        assert_eq!((fit.left, fit.right, fit.width), (2, 3, TAB_W));
    }

    #[test]
    fn tabs_shrink_then_the_rest_go_under_more() {
        // Six tabs need 1090 at full width; 800 holds them a little narrower.
        let fit = fit_tabs(6, 800., None);
        assert_eq!(fit.shown(), 6);
        assert!(fit.width < TAB_W && fit.width >= MIN_TAB_W);
        // Too many even at the least width: those that fit beside "N more".
        let fit = fit_tabs(20, 600., None);
        assert_eq!(fit.width, MIN_TAB_W);
        assert_eq!(fit.shown(), fits(600. - MORE_W, MIN_TAB_W));
        let fit = fit_tabs(20, 400., Some(400.));
        assert_eq!(fit.left, fits(400., MIN_TAB_W));
        assert_eq!(fit.right, fits(400. - MORE_W, MIN_TAB_W));
        assert!(!fit.more_left);
        // No room for it right of the island: it ends the left half instead.
        let fit = fit_tabs(20, 400., Some(60.));
        assert!(fit.more_left);
        assert_eq!(fit.left, fits(400. - MORE_W, MIN_TAB_W));
        assert_eq!(fit.right, 0);
    }

    #[test]
    fn the_tab_on_screen_never_hides_under_more() {
        let mut order = vec![0, 1, 2, 3, 4];
        bring_into_view(&mut order, 3, 4);
        assert_eq!(order, [0, 1, 4, 2, 3]);
        let mut order = vec![0, 1, 2];
        bring_into_view(&mut order, 3, 1);
        assert_eq!(order, [0, 1, 2]);
    }

    #[test]
    fn the_island_splits_the_bar_only_where_it_sits() {
        // Bar from 300 to 1300 points, island in the middle of a 1600 window.
        assert_eq!(split_room((300., 1000.), Some(800.)), (425., Some(425.)));
        assert_eq!(split_room((300., 1000.), None), (1000., None));
        // Island left of the bar, or past its end.
        assert_eq!(split_room((900., 400.), Some(700.)), (400., None));
        assert_eq!(split_room((0., 400.), Some(800.)), (400., None));
        // The bar starts under the island: everything goes right of it.
        assert_eq!(split_room((780., 400.), Some(800.)), (0., Some(305.)));
    }
}
